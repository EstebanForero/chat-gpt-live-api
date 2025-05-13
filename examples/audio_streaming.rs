use cpal::{
    SampleFormat, SampleRate, StreamConfig, SupportedStreamConfig,
    traits::{DeviceTrait, HostTrait, StreamTrait},
};
use crossbeam_channel::{Receiver, Sender, bounded};
use gemini_live_api::{
    GeminiLiveClientBuilder,
    client::{ServerContentContext, UsageMetadataContext},
    types::*,
};
use std::{
    env,
    sync::{Arc, Mutex as StdMutex},
    time::Duration,
};
use tokio::sync::Notify;
use tracing::{debug, error, info, trace, warn};

#[derive(Clone, Debug)]
struct AudioAppState {
    full_response_text: Arc<StdMutex<String>>,
    interaction_complete_signal: Arc<Notify>,
    playback_sender: Arc<Sender<Vec<i16>>>,
    capturing_audio: Arc<StdMutex<bool>>,
}

impl Default for AudioAppState {
    fn default() -> Self {
        let (pb_sender, _) = bounded(100);
        AudioAppState {
            full_response_text: Arc::new(StdMutex::new(String::new())),
            interaction_complete_signal: Arc::new(Notify::new()),
            playback_sender: Arc::new(pb_sender),
            capturing_audio: Arc::new(StdMutex::new(false)),
        }
    }
}

const INPUT_SAMPLE_RATE_HZ: u32 = 16000;
const INPUT_CHANNELS_COUNT: u16 = 1;
const OUTPUT_SAMPLE_RATE_HZ: u32 = 24000;
const OUTPUT_CHANNELS_COUNT: u16 = 1;

async fn handle_on_content(ctx: ServerContentContext, app_state: Arc<AudioAppState>) {
    if let Some(text) = ctx.text {
        let mut full_res = app_state.full_response_text.lock().unwrap();
        *full_res += &text;
        *full_res += " ";
        info!("[Handler] OpenAI Text: {}", text.trim());
    }
    if let Some(audio_samples) = ctx.audio {
        if !audio_samples.is_empty() {
            debug!("[Handler] OpenAI Audio: {} samples", audio_samples.len());
            if let Err(e) = app_state.playback_sender.send(audio_samples) {
                error!("[Handler] Failed to send audio for playback: {}", e);
            }
        }
    }
    if ctx.is_done {
        info!("[Handler] OpenAI content segment processing complete.");
        app_state.interaction_complete_signal.notify_one();
    }
}

async fn handle_usage_metadata(_ctx: UsageMetadataContext, _app_state: Arc<AudioAppState>) {
    info!("[Handler] OpenAI Usage Metadata: {:?}", _ctx.metadata);
}

fn find_supported_config_generic<F, I>(
    mut configs_iterator_fn: F,
    target_sample_rate: u32,
    target_channels: u16,
) -> Result<SupportedStreamConfig, anyhow::Error>
where
    F: FnMut() -> Result<I, cpal::SupportedStreamConfigsError>,
    I: Iterator<Item = cpal::SupportedStreamConfigRange>,
{
    let mut best_config: Option<SupportedStreamConfig> = None;
    let mut min_rate_diff = u32::MAX;

    for config_range in configs_iterator_fn()? {
        if config_range.channels() != target_channels {
            continue;
        }
        if config_range.sample_format() != SampleFormat::I16 {
            continue;
        }

        let current_min_rate = config_range.min_sample_rate().0;
        let current_max_rate = config_range.max_sample_rate().0;
        let rate_to_check =
            if target_sample_rate >= current_min_rate && target_sample_rate <= current_max_rate {
                target_sample_rate
            } else if target_sample_rate < current_min_rate {
                current_min_rate
            } else {
                current_max_rate
            };
        let rate_diff = (rate_to_check as i32 - target_sample_rate as i32).abs() as u32;
        if best_config.is_none() || rate_diff < min_rate_diff {
            min_rate_diff = rate_diff;
            best_config = Some(config_range.with_sample_rate(SampleRate(rate_to_check)));
        }
        if rate_diff == 0 {
            break;
        }
    }
    best_config.ok_or_else(|| {
        anyhow::anyhow!(
            "No i16 config for ~{}Hz {}ch",
            target_sample_rate,
            target_channels
        )
    })
}

fn setup_audio_input(
    /* ... (unchanged) ... */
    audio_chunk_sender: tokio::sync::mpsc::Sender<Vec<i16>>,
    app_state: Arc<AudioAppState>,
) -> Result<cpal::Stream, anyhow::Error> {
    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .ok_or_else(|| anyhow::anyhow!("No input device"))?;
    info!("[AudioInput] Using input: {}", device.name()?);
    let supported_config = find_supported_config_generic(
        || device.supported_input_configs(),
        INPUT_SAMPLE_RATE_HZ,
        INPUT_CHANNELS_COUNT,
    )?;
    let config: StreamConfig = supported_config.config();
    info!("[AudioInput] Building stream with config: {:?}", config);

    let stream = device.build_input_stream(
        &config,
        move |data: &[i16], _: &cpal::InputCallbackInfo| {
            if !*app_state.capturing_audio.lock().unwrap() {
                return;
            }
            if data.is_empty() {
                return;
            }
            match audio_chunk_sender.try_send(data.to_vec()) {
                Ok(_) => {}
                Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                    warn!("[AudioInput] Chunk channel full.")
                }
                Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                    error!("[AudioInput] Chunk channel closed.")
                }
            }
        },
        |err| error!("[AudioInput] CPAL Error: {}", err),
        None,
    )?;
    stream.play()?;
    info!("[AudioInput] Mic stream started with config: {:?}", config);
    Ok(stream)
}
fn setup_audio_output(
    playback_receiver: Receiver<Vec<i16>>,
) -> Result<cpal::Stream, anyhow::Error> {
    /* ... (unchanged) ... */
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or_else(|| anyhow::anyhow!("No output device"))?;
    info!("[AudioOutput] Using output: {}", device.name()?);
    let supported_config = find_supported_config_generic(
        || device.supported_output_configs(),
        OUTPUT_SAMPLE_RATE_HZ,
        OUTPUT_CHANNELS_COUNT,
    )?;
    let config: StreamConfig = supported_config.config();
    info!("[AudioOutput] Building stream with config: {:?}", config);
    let mut samples_buffer: Vec<i16> = Vec::new();

    let stream = device.build_output_stream(
        &config,
        move |data: &mut [i16], _: &cpal::OutputCallbackInfo| {
            let mut written = 0;
            while written < data.len() {
                if samples_buffer.is_empty() {
                    match playback_receiver.try_recv() {
                        Ok(new_samples) => samples_buffer.extend(new_samples),
                        Err(_) => break,
                    }
                }
                if samples_buffer.is_empty() {
                    break;
                }
                let to_write_now = std::cmp::min(data.len() - written, samples_buffer.len());
                for i in 0..to_write_now {
                    data[written + i] = samples_buffer.remove(0);
                }
                written += to_write_now;
            }
            for sample in data.iter_mut().skip(written) {
                *sample = 0;
            }
            if written > 0 {
                trace!("[AudioOutput] Played {} samples.", written);
            }
        },
        |err| error!("[AudioOutput] CPAL Error: {}", err),
        None,
    )?;
    stream.play()?;
    info!(
        "[AudioOutput] Playback stream started with config: {:?}",
        config
    );
    Ok(stream)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    dotenv::dotenv().ok();

    let api_key =
        env::var("API_KEY").map_err(|_| "OPENAI_API_KEY not set (using API_KEY for now)")?;
    let model_name = env::var("OPENAI_MODEL").unwrap_or_else(|_| "gpt-4o".to_string());
    info!("Using OpenAI Model: {}", model_name);

    let (pb_sender, pb_receiver) = bounded::<Vec<i16>>(100);
    let (audio_input_chunk_tx, mut audio_input_chunk_rx) =
        tokio::sync::mpsc::channel::<Vec<i16>>(20);

    let app_state_value = AudioAppState::default();
    let app_state_for_callbacks = Arc::new(app_state_value.clone());

    info!("Configuring Client for OpenAI Audio...");
    let mut builder = GeminiLiveClientBuilder::<AudioAppState>::new_with_model_and_state(
        api_key,
        model_name,
        app_state_value,
    );

    builder = builder.generation_config(GenerationConfig {
        response_modalities: Some(vec![ResponseModality::Audio, ResponseModality::Text]), // OpenAI can do both
        temperature: Some(0.7),
        // speech_config is Gemini specific for output, OpenAI uses .voice()
        ..Default::default()
    });

    builder = builder.voice("alloy".to_string()); // OpenAI specific voice
    builder = builder.input_audio_format("pcm16".to_string());
    builder = builder.output_audio_format("pcm16".to_string());
    // No RealtimeInputConfig for OpenAI, VAD is handled differently

    builder = builder.output_audio_transcription(AudioTranscriptionConfig {});
    builder = builder.system_instruction(Content {
        parts: vec![Part {
            text: Some("You are an OpenAI voice assistant. Respond to my audio.".to_string()),
            ..Default::default()
        }],
        role: Some(Role::System),
        ..Default::default()
    });
    builder = builder.on_server_content(handle_on_content);
    builder = builder.on_usage_metadata(handle_usage_metadata);

    info!("Connecting OpenAI client...");
    let client = builder.connect_openai().await?; // Use connect_openai
    let client_arc = Arc::new(tokio::sync::Mutex::new(client));

    let _input_stream = setup_audio_input(
        audio_input_chunk_tx.clone(),
        app_state_for_callbacks.clone(),
    )?;
    let _output_stream = setup_audio_output(pb_receiver)?;

    info!(
        "OpenAI Client connected. Will send audio for ~3 seconds, then send audio_stream_end and request response."
    );
    *app_state_for_callbacks.capturing_audio.lock().unwrap() = true;
    info!("Microphone capturing enabled. Sending audio for ~3 seconds...");

    let audio_duration = Duration::from_secs(3);
    let start_time = tokio::time::Instant::now();
    while start_time.elapsed() < audio_duration {
        if let Some(samples) = audio_input_chunk_rx.recv().await {
            let mut client_guard = client_arc.lock().await;
            if let Err(e) = client_guard
                .send_audio_chunk(&samples, INPUT_SAMPLE_RATE_HZ)
                .await
            {
                error!("Failed to send audio chunk: {:?}", e);
                if matches!(
                    e,
                    gemini_live_api::error::GeminiError::SendError
                        | gemini_live_api::error::GeminiError::ConnectionClosed
                ) {
                    break;
                }
            }
        } else {
            info!("Audio input channel closed.");
            break;
        }
    }

    info!("Stopping microphone capture.");
    *app_state_for_callbacks.capturing_audio.lock().unwrap() = false;
    drop(audio_input_chunk_tx);

    {
        let mut client_guard = client_arc.lock().await;
        info!("Sending audioStreamEnd to OpenAI...");
        if let Err(e) = client_guard.send_audio_stream_end().await {
            error!("Failed to send audioStreamEnd: {:?}", e);
        }
        info!("Requesting final response from OpenAI...");
        if let Err(e) = client_guard.request_response().await {
            error!("Failed to request final response: {:?}", e);
        }
    }

    tokio::time::sleep(Duration::from_millis(500)).await;

    let interaction_complete_notification =
        app_state_for_callbacks.interaction_complete_signal.clone();
    let main_loop_duration = Duration::from_secs(30); // Increased timeout

    info!("Waiting for OpenAI server response or timeout...");
    tokio::select! {
        _ = interaction_complete_notification.notified() => {
            let final_text = app_state_for_callbacks.full_response_text.lock().unwrap().trim().to_string();
            info!("\n--- OPENAI INTERACTION COMPLETE. Final Text: {} ---", final_text);
        }
        _ = tokio::signal::ctrl_c() => { info!("Ctrl+C received, initiating shutdown."); }
        res = tokio::time::timeout(main_loop_duration, async { loop { tokio::task::yield_now().await; }}) => {
            if res.is_err() { warn!("Interaction timed out after {} seconds.", main_loop_duration.as_secs()); }
        }
    }

    info!("Shutting down OpenAI client...");
    client_arc.lock().await.close().await?;
    info!("OpenAI Client closed.");
    Ok(())
}

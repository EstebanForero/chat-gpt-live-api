use base64::Engine as _;
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
    turn_complete_signal: Arc<Notify>,
    playback_sender: Arc<Sender<Vec<i16>>>, // crossbeam sender for audio playback
    capturing_audio: Arc<StdMutex<bool>>,
    // No direct client sender here; audio input will use a dedicated channel to a Tokio task
}

const INPUT_SAMPLE_RATE_HZ: u32 = 16000;
const INPUT_CHANNELS_COUNT: u16 = 1;
const OUTPUT_SAMPLE_RATE_HZ: u32 = 24000;
const OUTPUT_CHANNELS_COUNT: u16 = 1;
const AUDIO_INPUT_BUFFER_SIZE_MS: u64 = 50; // How much audio to buffer before sending (e.g., 50ms)

async fn handle_on_content(ctx: ServerContentContext, app_state: Arc<AudioAppState>) {
    if let Some(model_turn) = &ctx.content.model_turn {
        for part in &model_turn.parts {
            if let Some(text) = &part.text {
                let mut full_res = app_state.full_response_text.lock().unwrap();
                *full_res += text;
                *full_res += " ";
                info!("[Handler] Received text: {}", text.trim());
            }
            if let Some(blob) = &part.inline_data {
                if blob.mime_type.starts_with("audio/") {
                    debug!(
                        "[Handler] Received audio blob. Mime: {}, Size: {} bytes (encoded)",
                        blob.mime_type,
                        blob.data.len()
                    );
                    match base64::engine::general_purpose::STANDARD.decode(&blob.data) {
                        Ok(decoded_bytes) => {
                            if decoded_bytes.len() % 2 != 0 {
                                warn!(
                                    "[Handler] Decoded audio data has odd number of bytes, skipping."
                                );
                                continue;
                            }
                            let mut samples_i16 = Vec::with_capacity(decoded_bytes.len() / 2);
                            for chunk in decoded_bytes.chunks_exact(2) {
                                samples_i16.push(i16::from_le_bytes([chunk[0], chunk[1]]));
                            }
                            trace!(
                                "[Handler] Decoded {} audio samples for playback.",
                                samples_i16.len()
                            );
                            if let Err(e) = app_state.playback_sender.send(samples_i16) {
                                error!("[Handler] Failed to send audio for playback: {}", e);
                            }
                        }
                        Err(e) => {
                            error!("[Handler] Failed to decode base64 audio data: {}", e);
                        }
                    }
                }
            }
        }
    }

    if let Some(transcription) = &ctx.content.output_transcription {
        info!("[Handler] Output Transcription: {}", transcription.text);
    }

    if ctx.content.turn_complete {
        info!("[Handler] Turn complete message received.");
        app_state.turn_complete_signal.notify_one();
    }
    if ctx.content.generation_complete {
        info!("[Handler] Generation complete message received.");
    }
}

async fn handle_usage_metadata(_ctx: UsageMetadataContext, _app_state: Arc<AudioAppState>) {
    info!("[Handler] Received Usage Metadata: {:?}", _ctx.metadata);
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
            "No suitable i16 stream config found for sample rate ~{} Hz and {} channels.",
            target_sample_rate,
            target_channels
        )
    })
}

fn setup_audio_input(
    // This sender will send Vec<i16> to a dedicated Tokio task
    audio_chunk_sender: tokio::sync::mpsc::Sender<Vec<i16>>,
    app_state: Arc<AudioAppState>,
) -> Result<cpal::Stream, anyhow::Error> {
    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .ok_or_else(|| anyhow::anyhow!("No input device available"))?;
    info!(
        "[AudioInput] Using default input device: {}",
        device.name()?
    );

    let supported_config = find_supported_config_generic(
        || device.supported_input_configs(),
        INPUT_SAMPLE_RATE_HZ,
        INPUT_CHANNELS_COUNT,
    )?;
    info!(
        "[AudioInput] Found supported input config: {:?}",
        supported_config
    );
    let mut config: StreamConfig = supported_config.into();

    // Audio buffer
    //let frame_count = (INPUT_SAMPLE_RATE_HZ as u64 * AUDIO_INPUT_BUFFER_SIZE_MS / 1000) as u32;
    //config.buffer_size = cpal::BufferSize::Fixed(frame_count);

    let stream = device.build_input_stream(
        &config,
        move |data: &[i16], _: &cpal::InputCallbackInfo| {
            if !*app_state.capturing_audio.lock().unwrap() {
                return;
            }
            let samples_vec = data.to_vec();
            if samples_vec.is_empty() {
                return;
            }

            // Use try_send to avoid blocking the cpal audio thread.
            // If the channel is full, it means the Tokio task is not keeping up.
            match audio_chunk_sender.try_send(samples_vec) {
                Ok(_) => { /* trace!("[AudioInputCallback] Sent {} samples to processing task.", data.len()); */ }
                Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                    warn!("[AudioInputCallback] Audio chunk channel full. Dropping audio frame.");
                }
                Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                    error!("[AudioInputCallback] Audio chunk channel closed. Input will stop.");
                }
            }
        },
        |err| error!("[AudioInput] CPAL error: {:?}", err),
        None,
    )?;
    stream.play()?;
    info!(
        "[AudioInput] Microphone stream started with config: {:?}",
        config
    );
    Ok(stream)
}

fn setup_audio_output(
    playback_receiver: Receiver<Vec<i16>>,
) -> Result<cpal::Stream, anyhow::Error> {
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or_else(|| anyhow::anyhow!("No output device available"))?;
    info!(
        "[AudioOutput] Using default output device: {}",
        device.name()?
    );
    let supported_config = find_supported_config_generic(
        || device.supported_output_configs(),
        OUTPUT_SAMPLE_RATE_HZ,
        OUTPUT_CHANNELS_COUNT,
    )?;
    info!(
        "[AudioOutput] Found supported output config: {:?}",
        supported_config
    );
    let config: StreamConfig = supported_config.into();
    let mut samples_buffer: Vec<i16> = Vec::new();
    let stream = device.build_output_stream(
        &config,
        move |data: &mut [i16], _: &cpal::OutputCallbackInfo| {
            while samples_buffer.len() < data.len() {
                match playback_receiver.try_recv() {
                    Ok(new_samples) => samples_buffer.extend(new_samples),
                    Err(_) => break,
                }
            }
            let len_to_write = std::cmp::min(data.len(), samples_buffer.len());
            for i in 0..len_to_write {
                data[i] = samples_buffer.remove(0);
            }
            for sample in data.iter_mut().skip(len_to_write) {
                *sample = 0;
            }
            if len_to_write > 0 {
                trace!("[AudioOutput] Played {} samples.", len_to_write);
            }
        },
        |err| error!("[AudioOutput] CPAL error: {:?}", err),
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

    let api_key = env::var("GEMINI_API_KEY").map_err(|_| "GEMINI_API_KEY not set")?;
    let model_name = "models/gemini-2.0-flash-exp".to_string();

    let (pb_sender, pb_receiver) = bounded::<Vec<i16>>(100);

    let (audio_input_chunk_tx, mut audio_input_chunk_rx) =
        tokio::sync::mpsc::channel::<Vec<i16>>(20);

    let initial_app_state_struct = AudioAppState {
        full_response_text: Arc::new(StdMutex::new(String::new())),
        turn_complete_signal: Arc::new(Notify::new()),
        playback_sender: Arc::new(pb_sender.clone()),
        capturing_audio: Arc::new(StdMutex::new(false)),
    };

    info!("Configuring Gemini Live Client for Audio...");
    let mut builder = GeminiLiveClientBuilder::<AudioAppState>::new_with_state(
        api_key,
        model_name,
        initial_app_state_struct,
    );

    builder = builder.generation_config(GenerationConfig {
        response_modalities: Some(vec![ResponseModality::Audio]),
        temperature: Some(0.7),
        speech_config: Some(SpeechConfig {
            language_code: Some(SpeechLanguageCode::EnglishUS),
        }),
        ..Default::default()
    });

    builder = builder.realtime_input_config(RealtimeInputConfig {
        automatic_activity_detection: Some(AutomaticActivityDetection {
            disabled: Some(false),
            start_of_speech_sensitivity: Some(StartSensitivity::StartSensitivityHigh),
            end_of_speech_sensitivity: Some(EndSensitivity::EndSensitivityHigh),
            silence_duration_ms: Some(1500),
            prefix_padding_ms: Some(100),
            ..Default::default()
        }),
        activity_handling: Some(ActivityHandling::StartOfActivityInterrupts),
        turn_coverage: Some(TurnCoverage::TurnIncludesOnlyActivity),
        ..Default::default()
    });
    builder = builder.output_audio_transcription(AudioTranscriptionConfig {});
    builder = builder.system_instruction(Content {
        parts: vec![Part {
            text: Some("You are a voice assistant. Respond to my audio.".to_string()),
            ..Default::default()
        }],
        role: Some(Role::System),
        ..Default::default()
    });
    builder = builder.on_server_content(handle_on_content);
    builder = builder.on_usage_metadata(handle_usage_metadata);

    info!("Connecting client...");
    let mut client = builder.connect().await?;

    let client_outgoing_mpsc_sender_for_task = client
        .get_outgoing_mpsc_sender_clone()
        .ok_or_else(|| anyhow::anyhow!("Client's outgoing MPSC sender is not available"))?;

    let client_outgoing_mpsc_sender_main = client_outgoing_mpsc_sender_for_task.clone();

    tokio::spawn(async move {
        info!("[AudioProcessingTask] Started.");
        while let Some(samples_vec) = audio_input_chunk_rx.recv().await {
            if samples_vec.is_empty() {
                continue;
            }
            trace!(
                "[AudioProcessingTask] Received {} samples for sending.",
                samples_vec.len()
            );

            let mut byte_data = Vec::with_capacity(samples_vec.len() * 2);
            for sample in &samples_vec {
                byte_data.extend_from_slice(&sample.to_le_bytes());
            }
            let encoded_data = base64::engine::general_purpose::STANDARD.encode(&byte_data);

            let mime_type = format!("audio/pcm;rate={}", INPUT_SAMPLE_RATE_HZ);

            info!("[AudioProcessingTask] Using MIME type: {}", mime_type);

            let audio_blob = Blob {
                mime_type,
                data: encoded_data,
            };
            let realtime_input_payload = BidiGenerateContentRealtimeInput {
                audio: Some(audio_blob),
                ..Default::default()
            };
            let client_message = ClientMessagePayload::RealtimeInput(realtime_input_payload);

            if let Err(e) = client_outgoing_mpsc_sender_for_task
                .send(client_message)
                .await
            {
                error!(
                    "[AudioProcessingTask] Failed to send audio payload to client: {:?}",
                    e
                );
                break;
            }
        }
        info!("[AudioProcessingTask] Stopped.");
    });

    let app_state_for_audio_setup = client.state();

    let _input_stream = setup_audio_input(audio_input_chunk_tx, app_state_for_audio_setup.clone())?;
    let _output_stream = setup_audio_output(pb_receiver)?;

    info!(
        "Client connected. Will send a few seconds of audio, then audioStreamEnd, then turnComplete."
    );

    *app_state_for_audio_setup.capturing_audio.lock().unwrap() = true;
    info!("Microphone capturing enabled. Sending audio for ~3 seconds...");
    tokio::time::sleep(Duration::from_secs(3)).await;

    info!("Stopping microphone capture for now.");
    *app_state_for_audio_setup.capturing_audio.lock().unwrap() = false;

    info!("Sending audioStreamEnd=true");
    let audio_stream_end_payload = BidiGenerateContentRealtimeInput {
        audio_stream_end: Some(true),
        ..Default::default()
    };
    if let Err(e) = client_outgoing_mpsc_sender_main
        .send(ClientMessagePayload::RealtimeInput(
            audio_stream_end_payload,
        ))
        .await
    {
        error!("Failed to send audioStreamEnd: {:?}", e);
    }

    tokio::time::sleep(Duration::from_millis(200)).await;

    let turn_complete_notification = app_state_for_audio_setup.turn_complete_signal.clone();
    let main_loop_duration = Duration::from_secs(20);

    info!("Waiting for server response or timeout...");

    tokio::select! {
        _ = turn_complete_notification.notified() => {
            let final_text = app_state_for_audio_setup.full_response_text.lock().unwrap().trim().to_string();
            info!("\n--- GEMINI TURN COMPLETE (Notification). Last Text: {} ---", final_text);
        }
        _ = tokio::signal::ctrl_c() => {
            info!("Ctrl+C received, initiating shutdown.");
        }
        res = tokio::time::timeout(main_loop_duration, async { loop { tokio::task::yield_now().await; }}) => {
            if res.is_err() {
                 warn!("Interaction timed out after {} seconds waiting for server response.", main_loop_duration.as_secs());
            }
        }
    }

    info!("Shutting down...");
    client.close().await?;
    info!("Client closed.");

    Ok(())
}

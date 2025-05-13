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
use tracing::{debug, error, info, warn};

#[derive(Clone, Debug)]
struct ContinuousAudioAppState {
    full_response_text: Arc<StdMutex<String>>,
    model_turn_complete_signal: Arc<Notify>,
    playback_sender: Arc<Sender<Vec<i16>>>,
    // This will be true for the duration of the active chat
    is_microphone_active: Arc<StdMutex<bool>>,
}

const INPUT_SAMPLE_RATE_HZ: u32 = 16000;
const INPUT_CHANNELS_COUNT: u16 = 1;
const OUTPUT_SAMPLE_RATE_HZ: u32 = 24000;
const OUTPUT_CHANNELS_COUNT: u16 = 1;

async fn handle_on_content(ctx: ServerContentContext, app_state: Arc<ContinuousAudioAppState>) {
    if let Some(model_turn) = &ctx.content.model_turn {
        for part in &model_turn.parts {
            if let Some(text) = &part.text {
                let mut full_res = app_state.full_response_text.lock().unwrap();
                *full_res += text;
                *full_res += " ";
                info!("[Handler] Model Text: {}", text.trim());
            }
            if let Some(blob) = &part.inline_data {
                if blob.mime_type.starts_with("audio/") {
                    debug!(
                        "[Handler] Received audio blob. Mime: {}, Size: {} bytes",
                        blob.mime_type,
                        blob.data.len()
                    );
                    match base64::engine::general_purpose::STANDARD.decode(&blob.data) {
                        Ok(decoded_bytes) => {
                            if decoded_bytes.len() % 2 != 0 {
                                warn!("[Handler] Decoded audio data has odd number of bytes.");
                                continue;
                            }
                            let samples_i16 = decoded_bytes
                                .chunks_exact(2)
                                .map(|chunk| i16::from_le_bytes([chunk[0], chunk[1]]))
                                .collect::<Vec<i16>>();
                            if let Err(e) = app_state.playback_sender.send(samples_i16) {
                                error!("[Handler] Failed to send audio for playback: {}", e);
                            }
                        }
                        Err(e) => error!("[Handler] Failed to decode base64 audio: {}", e),
                    }
                }
            }
        }
    }
    if let Some(transcription) = &ctx.content.output_transcription {
        // Model's own speech transcribed
        info!(
            "[Handler] Model Output Transcription: {}",
            transcription.text
        );
    }
    if ctx.content.turn_complete {
        // Model indicates its turn is fully complete
        info!("[Handler] Model turn_complete message received.");
        app_state.model_turn_complete_signal.notify_one();
    }
    if ctx.content.generation_complete {
        // Model indicates its generation for the current response is complete
        info!("[Handler] Model generation_complete message received.");
    }
    if ctx.content.interrupted {
        info!("[Handler] Model generation was interrupted (likely by barge-in).");
    }
}

async fn handle_usage_metadata(
    _ctx: UsageMetadataContext,
    _app_state: Arc<ContinuousAudioAppState>,
) {
    info!("[Handler] Usage Metadata: {:?}", _ctx.metadata);
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
    audio_chunk_sender: tokio::sync::mpsc::Sender<Vec<i16>>,
    app_state: Arc<ContinuousAudioAppState>,
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
    let config: StreamConfig = supported_config.into();
    let stream = device.build_input_stream(
        &config,
        move |data: &[i16], _: &cpal::InputCallbackInfo| {
            if !*app_state.is_microphone_active.lock().unwrap() {
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
    let config: StreamConfig = supported_config.into();
    let mut samples_buffer: Vec<i16> = Vec::new();
    let stream = device.build_output_stream(
        &config,
        move |data: &mut [i16], _: &cpal::OutputCallbackInfo| {
            while samples_buffer.len() < data.len() {
                if let Ok(new_samples) = playback_receiver.try_recv() {
                    samples_buffer.extend(new_samples);
                } else {
                    break;
                }
            }
            let len_to_write = std::cmp::min(data.len(), samples_buffer.len());
            for i in 0..len_to_write {
                data[i] = samples_buffer.remove(0);
            }
            for sample in data.iter_mut().skip(len_to_write) {
                *sample = 0;
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
    let api_key = env::var("GEMINI_API_KEY").map_err(|_| "GEMINI_API_KEY not set")?;
    let model_name =
        env::var("GEMINI_MODEL").unwrap_or_else(|_| "models/gemini-2.0-flash-exp".to_string());

    let (playback_tx, playback_rx) = bounded::<Vec<i16>>(100);
    let (audio_input_chunk_tx, mut audio_input_chunk_rx) =
        tokio::sync::mpsc::channel::<Vec<i16>>(20);

    let app_state_instance = Arc::new(ContinuousAudioAppState {
        full_response_text: Arc::new(StdMutex::new(String::new())),
        model_turn_complete_signal: Arc::new(Notify::new()),
        playback_sender: Arc::new(playback_tx),
        is_microphone_active: Arc::new(StdMutex::new(false)),
    });

    let mut builder = GeminiLiveClientBuilder::<ContinuousAudioAppState>::new_with_state(
        api_key,
        model_name.clone(),
        (*app_state_instance).clone(),
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
            disabled: Some(false), // Crucial: VAD is ON
            start_of_speech_sensitivity: Some(StartSensitivity::StartSensitivityHigh),
            prefix_padding_ms: Some(100),
            end_of_speech_sensitivity: Some(EndSensitivity::EndSensitivityHigh),
            silence_duration_ms: Some(1200), // Shorter silence to detect end of user speech sooner
        }),
        activity_handling: Some(ActivityHandling::StartOfActivityInterrupts), // Crucial: Barge-in ON
        turn_coverage: Some(TurnCoverage::TurnIncludesOnlyActivity),
    });

    builder = builder.output_audio_transcription(AudioTranscriptionConfig {});
    builder = builder.system_instruction(Content {
        parts: vec![Part {
            text: Some("You are a helpful voice assistant.".to_string()),
            ..Default::default()
        }],
        role: Some(Role::System),
        ..Default::default()
    });

    builder = builder.on_server_content(handle_on_content);
    builder = builder.on_usage_metadata(handle_usage_metadata);

    info!("Connecting to Gemini model: {}", model_name);
    let mut client = builder.connect().await?;

    let client_outgoing_mpsc_sender = client
        .get_outgoing_mpsc_sender_clone()
        .ok_or_else(|| anyhow::anyhow!("Client MPSC sender unavailable"))?;

    tokio::spawn(async move {
        info!("[AudioProcessingTask] Started.");
        while let Some(samples_vec) = audio_input_chunk_rx.recv().await {
            if samples_vec.is_empty() {
                continue;
            }
            let mut byte_data = Vec::with_capacity(samples_vec.len() * 2);
            for sample in &samples_vec {
                byte_data.extend_from_slice(&sample.to_le_bytes());
            }
            let encoded_data = base64::engine::general_purpose::STANDARD.encode(&byte_data);
            let mime_type = format!("audio/pcm;rate={}", INPUT_SAMPLE_RATE_HZ);
            let audio_blob = Blob {
                mime_type,
                data: encoded_data,
            };
            let client_message =
                ClientMessagePayload::RealtimeInput(BidiGenerateContentRealtimeInput {
                    audio: Some(audio_blob),
                    ..Default::default()
                });
            if let Err(e) = client_outgoing_mpsc_sender.send(client_message).await {
                error!("[AudioProcessingTask] Failed to send audio: {:?}", e);
                break;
            }
        }
        info!("[AudioProcessingTask] Stopped.");
    });

    let _input_stream = setup_audio_input(audio_input_chunk_tx, app_state_instance.clone())?;
    let _output_stream = setup_audio_output(playback_rx)?;

    *app_state_instance.is_microphone_active.lock().unwrap() = true;
    info!("Microphone is active. Continuous chat started. Press Ctrl+C to exit.");
    info!("Speak to Gemini. It should respond, and you should be able to interrupt (barge-in).");

    loop {
        tokio::select! {
            _ = app_state_instance.model_turn_complete_signal.notified() => {
                info!("[MainLoop] Model completed its turn.");
            }
            _ = tokio::signal::ctrl_c() => {
                info!("Ctrl+C received. Shutting down...");
                break;
            }
        }
    }

    *app_state_instance.is_microphone_active.lock().unwrap() = false;
    info!("Sending audioStreamEnd before closing...");
    let audio_stream_end_payload = BidiGenerateContentRealtimeInput {
        audio_stream_end: Some(true),
        ..Default::default()
    };
    if let Some(sender) = client.get_outgoing_mpsc_sender_clone() {
        if sender
            .send(ClientMessagePayload::RealtimeInput(
                audio_stream_end_payload,
            ))
            .await
            .is_err()
        {
            warn!("Failed to send final audioStreamEnd, client might be already closing.");
        }
    } else {
        warn!("Could not get client sender for final audioStreamEnd.");
    }
    tokio::time::sleep(Duration::from_millis(100)).await;

    client.close().await?;
    info!("Client closed. Exiting.");
    Ok(())
}

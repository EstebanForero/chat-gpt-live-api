use base64::Engine as _;
use cpal::{
    SampleFormat, SampleRate, StreamConfig, SupportedStreamConfig,
    traits::{DeviceTrait, HostTrait, StreamTrait},
};
use crossbeam_channel::{Receiver, Sender, bounded};
use gemini_live_api::{
    GeminiLiveClientBuilder,
    client::{ServerContentContext, UsageMetadataContext},
    tool_function,
    types::*,
};
use std::{env, sync::Arc, time::Duration};
use tokio::sync::{Mutex as TokioMutex, Notify};
use tracing::{error, info, warn};

#[derive(Clone, Debug)]
struct AudioToolAppState {
    model_response_text: Arc<TokioMutex<String>>,
    model_turn_complete_signal: Arc<Notify>,
    playback_sender: Arc<Sender<Vec<i16>>>,
    is_microphone_active: Arc<TokioMutex<bool>>,
    tool_call_count: Arc<TokioMutex<u32>>,
    is_tool_running: Arc<TokioMutex<bool>>,
}

impl AudioToolAppState {
    fn new(playback_sender: Sender<Vec<i16>>) -> Self {
        Self {
            model_response_text: Arc::new(TokioMutex::new(String::new())),
            model_turn_complete_signal: Arc::new(Notify::new()),
            playback_sender: Arc::new(playback_sender),
            is_microphone_active: Arc::new(TokioMutex::new(false)),
            tool_call_count: Arc::new(TokioMutex::new(0)),
            is_tool_running: Arc::new(TokioMutex::new(false)),
        }
    }
}

const INPUT_SAMPLE_RATE_HZ: u32 = 16000;
const INPUT_CHANNELS_COUNT: u16 = 1;
const OUTPUT_SAMPLE_RATE_HZ: u32 = 24000;
const OUTPUT_CHANNELS_COUNT: u16 = 1;

#[tool_function("Calculates the sum of two numbers and increments a counter")]
async fn sum_tool(state: Arc<AudioToolAppState>, a: f64, b: f64) -> f64 {
    *state.is_tool_running.lock().await = true;
    let mut count_guard = state.tool_call_count.lock().await;
    *count_guard += 1;
    info!(
        "[Tool] sum_tool called (count {}). Args: a={}, b={}",
        *count_guard, a, b
    );
    drop(count_guard);

    tokio::time::sleep(Duration::from_secs(2)).await;
    let result = a + b;
    info!("[Tool] sum_tool result: {}", result);
    *state.is_tool_running.lock().await = false;
    result
}

#[tool_function("Provides the current time")]
async fn get_current_time_tool(state: Arc<AudioToolAppState>) -> String {
    *state.is_tool_running.lock().await = true;
    let mut count_guard = state.tool_call_count.lock().await;
    *count_guard += 1;
    info!(
        "[Tool] get_current_time_tool called (count {})",
        *count_guard
    );
    drop(count_guard);

    tokio::time::sleep(Duration::from_secs(1)).await;
    let now = chrono::Local::now();
    let time_str = now.format("%Y-%m-%d %H:%M:%S").to_string();
    info!("[Tool] get_current_time_tool result: {}", time_str);
    *state.is_tool_running.lock().await = false;
    time_str
}

async fn handle_on_content(ctx: ServerContentContext, app_state: Arc<AudioToolAppState>) {
    if let Some(model_turn) = &ctx.content.model_turn {
        let mut model_text_guard = app_state.model_response_text.lock().await;

        let is_new_model_thought = model_turn.parts.iter().any(|p| p.text.is_some())
            && !ctx.content.interrupted
            && model_turn.parts.iter().all(|p| p.function_call.is_none());

        if is_new_model_thought && !model_text_guard.is_empty() {
            if !ctx.content.turn_complete {
                info!("[Handler] Clearing previous model text for new utterance.");
                model_text_guard.clear();
            }
        }

        for part in &model_turn.parts {
            if let Some(text) = &part.text {
                *model_text_guard += text;
                *model_text_guard += " ";
                info!("[Handler] Model Text: {}", text.trim());
            }
            if let Some(blob) = &part.inline_data {
                if blob.mime_type.starts_with("audio/") {
                    match base64::engine::general_purpose::STANDARD.decode(&blob.data) {
                        Ok(decoded_bytes) => {
                            if decoded_bytes.len() % 2 != 0 {
                                warn!("[Handler] Odd audio bytes");
                                continue;
                            }
                            let samples = decoded_bytes
                                .chunks_exact(2)
                                .map(|c| i16::from_le_bytes([c[0], c[1]]))
                                .collect();
                            if app_state.playback_sender.send(samples).is_err() {
                                error!("Error sending samples");
                            }
                        }
                        Err(e) => error!("[Handler] Base64 decode error: {}", e),
                    }
                }
            }
        }
    }
    if let Some(transcription) = &ctx.content.output_transcription {
        info!(
            "[Handler] Model Output Transcription: {}",
            transcription.text
        );
    }
    if ctx.content.turn_complete {
        info!("[Handler] Model turn_complete message received.");
        app_state.model_response_text.lock().await.clear();
        app_state.model_turn_complete_signal.notify_one();
    }
}

async fn handle_usage_metadata(_ctx: UsageMetadataContext, app_state: Arc<AudioToolAppState>) {
    let tool_calls = *app_state.tool_call_count.lock().await;
    info!(
        "[Handler] Usage Metadata: {:?}, Tool Calls: {}",
        _ctx.metadata, tool_calls
    );
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
    app_state: Arc<AudioToolAppState>,
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
            // IMPORTANT: CPAL callbacks are NOT async. We CANNOT .await here.
            // We must use blocking lock or try_lock if the lock might be contended by an async task.
            // For flags like these, `try_lock` is safer if there's any chance an async task holds them long.
            // However, these specific flags are mostly set by the main async task, so blocking lock might be okay
            // if a tool function (async) holds it, this cpal thread would block.
            // A non-blocking approach: use `AtomicBool` for simple flags if possible, or send state updates via channels.
            // For this example, let's keep blocking lock for simplicity and assume tools are quick or interaction is clear.
            // If `is_tool_running` is set by an async tool function, this sync cpal thread *will* block on `.lock().unwrap()`.
            // This is generally okay if the tool "work" (sleep) is the main thing holding the lock.

            let is_mic_active = app_state.is_microphone_active.blocking_lock();
            let tool_is_running = app_state.is_tool_running.blocking_lock();

            if !*is_mic_active || *tool_is_running {
                return;
            }

            drop(is_mic_active);
            drop(tool_is_running);

            if data.is_empty() {
                return;
            }
            match audio_chunk_sender.try_send(data.to_vec()) {
                Ok(_) => {}
                Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                    warn!("[AudioInput] Chunk channel full.")
                }
                Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                    info!("[AudioInput] Chunk channel closed.")
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
    let model_name = env::var("GEMINI_MODEL_TOOLS")
        .unwrap_or_else(|_| "models/gemini-2.0-flash-exp".to_string());

    let (playback_tx, playback_rx) = bounded::<Vec<i16>>(100);
    let (audio_input_chunk_tx, mut audio_input_chunk_rx) =
        tokio::sync::mpsc::channel::<Vec<i16>>(20);

    let app_state_instance = Arc::new(AudioToolAppState::new(playback_tx));

    let mut builder = GeminiLiveClientBuilder::<AudioToolAppState>::new_with_state(
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
            disabled: Some(false),
            silence_duration_ms: Some(1500),
            prefix_padding_ms: Some(100),
            start_of_speech_sensitivity: Some(StartSensitivity::StartSensitivityHigh),
            end_of_speech_sensitivity: Some(EndSensitivity::EndSensitivityHigh),
            ..Default::default()
        }),
        activity_handling: Some(ActivityHandling::StartOfActivityInterrupts),
        turn_coverage: Some(TurnCoverage::TurnIncludesOnlyActivity),
        ..Default::default()
    });
    builder = builder.output_audio_transcription(AudioTranscriptionConfig {});
    builder = builder.system_instruction(Content {
        parts: vec![Part { text: Some(
            "You are a voice assistant that can perform calculations and tell the current time. \
            Use your tools when asked for calculations or the time. \
            Respond verbally and also provide a short text confirmation of tool results if appropriate."
        .to_string()), ..Default::default()}],
        role: Some(Role::System), ..Default::default()
    });

    builder = sum_tool_register_tool(builder);
    builder = get_current_time_tool_register_tool(builder);

    builder = builder.on_server_content(handle_on_content);
    builder = builder.on_usage_metadata(handle_usage_metadata);

    info!(
        "Connecting to Gemini model for audio + tools: {}",
        model_name
    );
    let mut client = builder.connect().await?;

    let client_managed_app_state = client.state();

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

    let _input_stream = setup_audio_input(audio_input_chunk_tx, client_managed_app_state.clone())?;
    let _output_stream = setup_audio_output(playback_rx)?;

    *client_managed_app_state.is_microphone_active.lock().await = true;
    info!("Microphone active. Continuous audio chat with tools started.");
    info!("Press Ctrl+C to exit.");

    loop {
        tokio::select! {
            _ = client_managed_app_state.model_turn_complete_signal.notified() => {
                let model_response_text_guard = client_managed_app_state.model_response_text.lock().await;
                let final_text = model_response_text_guard.trim().to_string();
                drop(model_response_text_guard);

                if !final_text.is_empty() {
                    info!("[MainLoop] Model turn complete. Final text: '{}'", final_text);
                } else {
                    info!("[MainLoop] Model turn complete (no text accumulated).");
                }
            }
            _ = tokio::signal::ctrl_c() => {
                info!("Ctrl+C received. Shutting down...");
                break;
            }
        }
    }

    *client_managed_app_state.is_microphone_active.lock().await = false;
    info!("Sending final audioStreamEnd...");
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
            warn!("Failed to send final audioStreamEnd.");
        }
    }
    tokio::time::sleep(Duration::from_millis(100)).await;

    client.close().await?;
    info!("Client closed. Exiting.");
    Ok(())
}

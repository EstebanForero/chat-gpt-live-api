// examples/continuous_audio_with_tools.rs
use cpal::{
    BufferSize, 
    SampleFormat, SampleRate, StreamConfig, SupportedStreamConfig,
    traits::{DeviceTrait, HostTrait, StreamTrait},
};
use crossbeam_channel::{Receiver, Sender, bounded};
use gemini_live_api::{
    AiClientBuilder, 
    client::{ServerContentContext, UsageMetadataContext}, 
    tool_function, // This is the macro we are using
    types::*,
};
use std::{
    collections::VecDeque, 
    env,
    sync::Arc,
    time::Duration,
};
use tokio::sync::{Mutex as TokioMutex, Notify};
use tracing::{error, info, trace, warn}; // Removed 'debug'

#[derive(Clone, Debug)]
struct AudioToolAppState {
    model_response_text: Arc<TokioMutex<String>>,
    model_turn_complete_signal: Arc<Notify>,
    playback_sender: Arc<Sender<Vec<i16>>>,
    is_microphone_active: Arc<TokioMutex<bool>>,
    tool_call_count: Arc<TokioMutex<u32>>,
    is_tool_running: Arc<TokioMutex<bool>>,
    user_speech_ended_signal: Arc<Notify>, 
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
            user_speech_ended_signal: Arc::new(Notify::new()),
        }
    }
}

const INPUT_SAMPLE_RATE_HZ: u32 = 16000;
const INPUT_CHANNELS_COUNT: u16 = 1;
const OUTPUT_SAMPLE_RATE_HZ: u32 = 24000;
const OUTPUT_CHANNELS_COUNT: u16 = 1;

/// This tool calculates the sum of two numbers.
#[tool_function] // Can also be #[tool_function("Calculates the sum of two numbers")]
async fn sum_tool(state: Arc<AudioToolAppState>, a: f64, b: f64) -> String {
    *state.is_tool_running.lock().await = true;
    let mut count_guard = state.tool_call_count.lock().await;
    *count_guard += 1;
    info!(
        "[Tool] sum_tool called (count {}). Args: a={}, b={}",
        *count_guard, a, b
    );
    drop(count_guard);
    tokio::time::sleep(Duration::from_secs(1)).await;
    let result = a + b;
    info!("[Tool] sum_tool result: {}", result);
    *state.is_tool_running.lock().await = false;
    format!("The sum is: {result}")
}

/// This tool gets the current time.
#[tool_function] // Can also be #[tool_function("Provides the current time")]
async fn get_current_time_tool(state: Arc<AudioToolAppState>) -> String {
    *state.is_tool_running.lock().await = true;
    let mut count_guard = state.tool_call_count.lock().await;
    *count_guard += 1;
    info!(
        "[Tool] get_current_time_tool called (count {})",
        *count_guard
    );
    drop(count_guard);
    tokio::time::sleep(Duration::from_millis(500)).await;
    let now = chrono::Local::now();
    let time_str = now.format("%Y-%m-%d %H:%M:%S").to_string();
    info!("[Tool] get_current_time_tool result: {}", time_str);
    *state.is_tool_running.lock().await = false;
    time_str
}

async fn handle_on_content(ctx: ServerContentContext, app_state: Arc<AudioToolAppState>) {
    if let Some(text) = ctx.text {
        let mut model_text_guard = app_state.model_response_text.lock().await;
        *model_text_guard += &text;
        *model_text_guard += " ";
        info!("[Handler] OpenAI Model Text: {}", text.trim());
    }
    if let Some(audio_samples) = ctx.audio {
        if !audio_samples.is_empty() {
            match app_state.playback_sender.try_send(audio_samples) {
                Ok(_) => { /* Sent */ }
                Err(crossbeam_channel::TrySendError::Full(s)) => {
                    warn!(
                        "[Handler] Playback channel full, dropping {} audio samples.",
                        s.len()
                    );
                }
                Err(crossbeam_channel::TrySendError::Disconnected(_)) => {
                    error!("[Handler] Playback channel DISCONNECTED when trying to send audio for playback.");
                }
            }
        }
    }
    if ctx.is_done {
        info!("[Handler] OpenAI Content segment done (is_done=true).");
        app_state.model_turn_complete_signal.notify_one();
    }
}
async fn handle_usage_metadata(_ctx: UsageMetadataContext, app_state: Arc<AudioToolAppState>) {
    let tool_calls = *app_state.tool_call_count.lock().await;
    info!(
        "[Handler] OpenAI Usage Metadata: {:?}, Tool Calls: {}",
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
    let config: StreamConfig = supported_config.config();
    info!("[AudioInput] Building stream with config: {:?}", config);
    
    let stream = device.build_input_stream(
        &config,
        move |data: &[i16], _: &cpal::InputCallbackInfo| {
            let mic_active = app_state.is_microphone_active.blocking_lock();
            if !*mic_active { 
                return;
            }
            drop(mic_active); 
            
            let tool_running = app_state.is_tool_running.blocking_lock();
            if *tool_running { 
                return;
            }
            drop(tool_running);

            if data.is_empty() {
                return;
            }
            
            match audio_chunk_sender.try_send(data.to_vec()) {
                Ok(_) => {}
                Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                    warn!("[AudioInput] Chunk channel full.")
                }
                Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                    info!("[AudioInput] Chunk channel closed (expected on shutdown).")
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
    info!("[AudioOutput] Using output device: {}", device.name()?);

    let supported_config_range = find_supported_config_generic(
        || device.supported_output_configs(),
        OUTPUT_SAMPLE_RATE_HZ,
        OUTPUT_CHANNELS_COUNT,
    )?;

    let mut config: StreamConfig = supported_config_range.config();
    let latency_ms = 100.0; 
    let frames = (latency_ms / 1_000.0) * config.sample_rate.0 as f32;
    config.buffer_size = BufferSize::Fixed(frames.round() as u32);

    info!(
        "[AudioOutput] Final attempt to build stream with config: {:?}",
        config
    );

    let mut internal_samples_buffer: VecDeque<i16> = VecDeque::new();
    let mut callback_invocation_count: u64 = 0;

    let stream = device.build_output_stream(
        &config,
        move |data: &mut [i16], _cb_info: &cpal::OutputCallbackInfo| {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                callback_invocation_count += 1;
                let mut cpal_buffer_filled_count = 0;
                let cpal_buffer_len = data.len();

                while cpal_buffer_filled_count < cpal_buffer_len {
                    if internal_samples_buffer.is_empty() {
                        match playback_receiver.try_recv() {
                            Ok(new_api_chunk) => {
                                internal_samples_buffer.extend(new_api_chunk);
                            }
                            Err(crossbeam_channel::TryRecvError::Empty) => {
                                break; 
                            }
                            Err(crossbeam_channel::TryRecvError::Disconnected) => {
                                for sample_ref in data.iter_mut().skip(cpal_buffer_filled_count) {
                                    *sample_ref = 0;
                                }
                                trace!("[AudioOutputCallback] Playback channel disconnected. Filling with silence.");
                                return; 
                            }
                        }
                    }
                    if internal_samples_buffer.is_empty() {
                        break;
                    }

                    let available_in_internal_buffer = internal_samples_buffer.len();
                    let needed_for_cpal_data_slice = cpal_buffer_len - cpal_buffer_filled_count;
                    let num_to_copy_now =
                        std::cmp::min(available_in_internal_buffer, needed_for_cpal_data_slice);

                    if num_to_copy_now > 0 {
                        for i in 0..num_to_copy_now {
                            if let Some(sample) = internal_samples_buffer.pop_front() {
                                data[cpal_buffer_filled_count + i] = sample;
                            } else {
                                data[cpal_buffer_filled_count + i] = 0;
                                error!("[AudioOutputCallback] Unexpected empty internal_samples_buffer during copy!");
                            }
                        }
                        cpal_buffer_filled_count += num_to_copy_now;
                    } else {
                        break; 
                    }
                }
                if cpal_buffer_filled_count < cpal_buffer_len {
                    for sample_ref in data.iter_mut().skip(cpal_buffer_filled_count) {
                        *sample_ref = 0;
                    }
                }
            }));

            if let Err(panic_payload) = result {
                let msg = if let Some(s) = panic_payload.downcast_ref::<&'static str>() {
                    *s
                } else if let Some(s) = panic_payload.downcast_ref::<String>() {
                    s.as_str()
                } else {
                    "Unknown panic payload"
                };
                error!(
                    "[AudioOutputCallback] !!! PANIC CAUGHT IN AUDIO CALLBACK !!! Payload: {}",
                    msg
                );
                for sample_ref in data.iter_mut() {
                    *sample_ref = 0;
                }
            }
        },
        move |err| {
            error!("[AudioOutput] CPAL STREAM ERROR REPORTED: {:?}", err);
        },
        None,
    )?;

    stream.play()?;
    info!("[AudioOutput] Playback stream successfully started (with VecDeque and catch_unwind).");
    Ok(stream)
}


#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    dotenv::dotenv().ok();

    let api_key = env::var("API_KEY").map_err(|_| "API_KEY for OpenAI not set")?;
    let model_name =
        env::var("OPENAI_MODEL").unwrap_or_else(|_| "gpt-4o-mini-realtime-preview".to_string());
    info!("Using OpenAI Model for Tools: {}", model_name);

    let (playback_tx, playback_rx) = bounded::<Vec<i16>>(200); 
    let (audio_input_chunk_tx, mut audio_input_chunk_rx) =
        tokio::sync::mpsc::channel::<Vec<i16>>(50);

    let app_state_value = AudioToolAppState::new(playback_tx.clone());
    let app_state_for_main = Arc::new(app_state_value.clone()); 

    let mut builder = AiClientBuilder::<AudioToolAppState>::new_with_model_and_state( 
        api_key,
        model_name.clone(),
        app_state_value, 
    );

    builder = builder.generation_config(GenerationConfig {
        response_modalities: Some(vec![ResponseModality::Audio, ResponseModality::Text]),
        temperature: Some(0.7),
        ..Default::default()
    });
    builder = builder.voice("alloy".to_string());
    builder = builder.input_audio_format("pcm16".to_string());
    builder = builder.output_audio_format("pcm16".to_string());

    builder = builder.output_audio_transcription(AudioTranscriptionConfig {
        model: Some("whisper-1".to_string()),
        language: Some("en".to_string()),
        prompt: None
    });
    builder = builder.system_instruction(Content {
        parts: vec![Part {
            text: Some(
                "You are a voice assistant that can use tools. Respond verbally.".to_string(),
            ),
            ..Default::default()
        }],
        role: Some(Role::System),
        ..Default::default()
    });

    // These functions are now pub and correctly generated by the macro
    builder = sum_tool_register_tool(builder);
    builder = get_current_time_tool_register_tool(builder);
    
    builder = builder.on_server_content(move |ctx, state_arc| handle_on_content(ctx, state_arc));
    builder = builder.on_usage_metadata(move |ctx, state_arc| handle_usage_metadata(ctx, state_arc));


    info!("Connecting OpenAI client for audio + tools: {}", model_name);
    let client = builder.connect_openai().await?;
    let client_arc = Arc::new(tokio::sync::Mutex::new(client));

    let audio_task_client_clone = client_arc.clone();
    let audio_task_app_state_clone = app_state_for_main.clone(); 

    tokio::spawn(async move {
        info!("[AudioProcessingTask] Started for OpenAI.");
        loop {
            let is_mic_active = *audio_task_app_state_clone.is_microphone_active.lock().await;
            let is_tool_running = *audio_task_app_state_clone.is_tool_running.lock().await;

            tokio::select! {
                biased; // Prioritize explicit signals over continuous audio processing
                _ = audio_task_app_state_clone.user_speech_ended_signal.notified(), if is_mic_active && !is_tool_running => {
                    info!("[AudioProcessingTask] User speech ended signal. Sending audioStreamEnd.");
                    *audio_task_app_state_clone.is_microphone_active.lock().await = false;
                    info!("[AudioProcessingTask] Microphone paused due to VAD speech end.");

                    let mut client_guard = audio_task_client_clone.lock().await;
                    if let Err(e) = client_guard.send_audio_stream_end().await { // This now calls request_response for OpenAI
                        error!("[AudioProcessingTask] Failed to send audioStreamEnd: {:?}", e);
                    }
                }
                maybe_samples = audio_input_chunk_rx.recv(), if is_mic_active && !is_tool_running => {
                    if let Some(samples_vec) = maybe_samples {
                        if samples_vec.is_empty() { continue; }
                        let mut client_guard = audio_task_client_clone.lock().await;
                        if let Err(e) = client_guard.send_audio_chunk(&samples_vec, INPUT_SAMPLE_RATE_HZ).await {
                            error!("[AudioProcessingTask] Failed to send audio: {:?}", e);
                            if matches!(e, gemini_live_api::error::GeminiError::SendError | gemini_live_api::error::GeminiError::ConnectionClosed) {
                                error!("[AudioProcessingTask] Connection error. Stopping task.");
                                break;
                            }
                        }
                    } else {
                        info!("[AudioProcessingTask] Audio input channel closed. Stopping task.");
                        break;
                    }
                }
                _ = tokio::time::sleep(Duration::from_millis(10)), if !is_mic_active || is_tool_running => {
                    // Keep loop alive to re-evaluate conditions
                }
            }
        }
        info!("[AudioProcessingTask] Stopped.");
    });

    let _input_stream = setup_audio_input(audio_input_chunk_tx.clone(), app_state_for_main.clone())?;
    let _output_stream = setup_audio_output(playback_rx)?;

    *app_state_for_main.is_microphone_active.lock().await = true;
    info!("Microphone active. Continuous OpenAI audio chat with tools. Press Ctrl+C to exit.");

    loop {
        tokio::select! {
            _ = app_state_for_main.model_turn_complete_signal.notified() => {
                info!("[MainLoop] OpenAI model turn complete signal.");
                let tool_was_running = *app_state_for_main.is_tool_running.lock().await; // Check before clearing text
                app_state_for_main.model_response_text.lock().await.clear();

                if !tool_was_running { // Only reactivate mic if a tool wasn't the cause of this completion
                    info!("[MainLoop] Model speech turn finished. Re-activating microphone.");
                    *app_state_for_main.is_microphone_active.lock().await = true;
                } else {
                     // If a tool was running, it should set is_tool_running to false itself.
                     // Then, if the model *also* speaks after the tool, this signal might fire again.
                     // The logic in the tool (setting is_tool_running = false) AND this check here
                     // combined with potentially re-activating mic if model speaks *after* tool, can get complex.
                     // For now, we assume tools reset their flag, and if model speaks after, this handler will re-enable mic.
                     // A more robust state machine might be needed for complex interactions.
                    if !*app_state_for_main.is_tool_running.lock().await { // Check again, tool might have just finished
                        info!("[MainLoop] Model turn completed (possibly after a tool). Re-activating microphone.");
                        *app_state_for_main.is_microphone_active.lock().await = true;
                    } else {
                        info!("[MainLoop] Model turn completed, but a tool is still marked as running. Microphone remains paused by tool logic.");
                    }
                }
            }
            _ = tokio::signal::ctrl_c() => {
                info!("Ctrl+C received. Shutting down...");
                *app_state_for_main.is_microphone_active.lock().await = false; // Immediately stop mic
                app_state_for_main.user_speech_ended_signal.notify_one(); 
                tokio::time::sleep(Duration::from_millis(300)).await; 
                break;
            }
        }
    }

    *app_state_for_main.is_microphone_active.lock().await = false;
    info!("Microphone deactivated by main loop exit.");
    drop(audio_input_chunk_tx); 

    {
        let mut client_guard = client_arc.lock().await;
        if !client_guard.is_closed() { 
            info!("Sending final audioStreamEnd to OpenAI (if not already sent)...");
            if let Err(e) = client_guard.send_audio_stream_end().await {
                warn!("Failed to send final audioStreamEnd on shutdown: {:?}", e);
            }
        }
    }
    tokio::time::sleep(Duration::from_millis(500)).await;

    info!("Closing OpenAI Client...");
    client_arc.lock().await.close().await?;
    info!("OpenAI Client closed. Exiting.");
    Ok(())
}

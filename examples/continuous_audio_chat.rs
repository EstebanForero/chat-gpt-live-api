use cpal::{
    BufferSize, SampleFormat, SampleRate, StreamConfig, SupportedStreamConfig,
    traits::{DeviceTrait, HostTrait, StreamTrait},
};
use crossbeam_channel::{Receiver, Sender, bounded};
use gemini_live_api::{
    AiClientBuilder, // Using AiClientBuilder
    client::{ServerContentContext, UsageMetadataContext}, // Using AiLiveClient
    types::*,
};
use std::{
    collections::VecDeque, // <<< ADDED VecDeque
    env,
    sync::{Arc, Mutex as StdMutex},
    time::Duration,
};
use tokio::sync::Notify;
use tracing::{error, info, trace, warn};

#[derive(Clone, Debug)]
struct ContinuousAudioAppState {
    full_response_text: Arc<StdMutex<String>>,
    model_turn_complete_signal: Arc<Notify>,
    playback_sender: Arc<Sender<Vec<i16>>>,
    is_microphone_active: Arc<StdMutex<bool>>,
    user_speech_ended_signal: Arc<Notify>,
}

impl Default for ContinuousAudioAppState {
    fn default() -> Self {
        let (pb_sender, _) = bounded(100);
        ContinuousAudioAppState {
            full_response_text: Arc::new(StdMutex::new(String::new())),
            model_turn_complete_signal: Arc::new(Notify::new()),
            playback_sender: Arc::new(pb_sender),
            is_microphone_active: Arc::new(StdMutex::new(false)),
            user_speech_ended_signal: Arc::new(Notify::new()),
        }
    }
}

const INPUT_SAMPLE_RATE_HZ: u32 = 16000;
const INPUT_CHANNELS_COUNT: u16 = 1;
const OUTPUT_SAMPLE_RATE_HZ: u32 = 24000;
const OUTPUT_CHANNELS_COUNT: u16 = 1;

async fn handle_on_content(ctx: ServerContentContext, app_state: Arc<ContinuousAudioAppState>) {
    if let Some(text) = ctx.text {
        let mut full_res = app_state.full_response_text.lock().unwrap();
        *full_res += &text;
        *full_res += " ";
        info!("[Handler] OpenAI Text: {}", text.trim());
    }
    if let Some(audio_samples) = ctx.audio {
        if !audio_samples.is_empty() {
            // info!("[Handler] OpenAI Audio: {} samples. Attempting playback.", audio_samples.len());
            match app_state.playback_sender.try_send(audio_samples) {
                Ok(_) => { /* Sent */ }
                Err(crossbeam_channel::TrySendError::Full(s)) => {
                    warn!(
                        "[Handler] Playback channel full, dropping {} audio samples.",
                        s.len()
                    );
                }
                Err(crossbeam_channel::TrySendError::Disconnected(_)) => {
                    error!("[Handler] Playback channel DISCONNECTED when trying to send.");
                }
            }
        }
    }
    if ctx.is_done {
        info!("[Handler] OpenAI content segment complete (is_done=true).");
        app_state.model_turn_complete_signal.notify_one();
    }
}

async fn handle_usage_metadata(
    _ctx: UsageMetadataContext,
    _app_state: Arc<ContinuousAudioAppState>,
) {
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
    let config: StreamConfig = supported_config.config(); // Make mutable for buffer_size
    info!("[AudioInput] Initial supported config: {:?}", config);

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
                    info!("[AudioInput] Chunk channel closed (expected on shutdown).")
                }
            }
        },
        |err| error!("[AudioInput] CPAL Error: {}", err),
        None,
    )?;
    stream.play()?;
    info!(
        "[AudioInput] Mic stream started with actual config: {:?}",
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

    // <<< MODIFIED: Use VecDeque for efficient front removal
    let mut internal_samples_buffer: VecDeque<i16> = VecDeque::new();
    let mut callback_invocation_count: u64 = 0;

    let stream = device.build_output_stream(
        &config,
        move |data: &mut [i16], _cb_info: &cpal::OutputCallbackInfo| {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                callback_invocation_count += 1;
                // You can re-enable this trace for detailed debugging:
                // trace!("[AudioOutputCallback #{}] Called. Data len: {}, Info: {:?}", callback_invocation_count, data.len(), cb_info);

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
                                // Channel is disconnected. Fill remaining buffer with silence and return.
                                // The stream should continue playing silence.
                                // The error will be logged by the sender side in handle_on_content.
                                for sample_ref in data.iter_mut().skip(cpal_buffer_filled_count) {
                                    *sample_ref = 0;
                                }
                                trace!("[AudioOutputCallback] Playback channel disconnected. Filling with silence.");
                                return; 
                            }
                        }
                    }
                    if internal_samples_buffer.is_empty() { // Check again after potential extend
                        break;
                    }

                    let available_in_internal_buffer = internal_samples_buffer.len();
                    let needed_for_cpal_data_slice = cpal_buffer_len - cpal_buffer_filled_count;
                    let num_to_copy_now =
                        std::cmp::min(available_in_internal_buffer, needed_for_cpal_data_slice);

                    if num_to_copy_now > 0 {
                        for i in 0..num_to_copy_now {
                            // <<< MODIFIED: Use pop_front() which is O(1)
                            if let Some(sample) = internal_samples_buffer.pop_front() {
                                data[cpal_buffer_filled_count + i] = sample;
                            } else {
                                // Should not happen due to `num_to_copy_now` logic and prior is_empty checks
                                data[cpal_buffer_filled_count + i] = 0; // Fallback to silence
                                error!("[AudioOutputCallback] Unexpected empty internal_samples_buffer during copy!");
                            }
                        }
                        cpal_buffer_filled_count += num_to_copy_now;
                    } else {
                        // This case (num_to_copy_now == 0) should be caught by internal_samples_buffer.is_empty()
                        break; 
                    }
                }
                // Fill any remaining part of the CPAL buffer with silence
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
                // Fill entire buffer with silence, as the stream state is unknown after panic
                for sample_ref in data.iter_mut() {
                    *sample_ref = 0;
                }
            }
        },
        move |err| {
            // This is a critical log. If this appears, CPAL itself has an issue with the stream.
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
    info!("Using OpenAI Model: {}", model_name);

    let (playback_tx, playback_rx) = bounded::<Vec<i16>>(200); // Main playback channel
    let (audio_input_chunk_tx, mut audio_input_chunk_rx) =
        tokio::sync::mpsc::channel::<Vec<i16>>(50);

    let app_state_value = ContinuousAudioAppState {
        // Ensure the main playback_tx is used here
        playback_sender: Arc::new(playback_tx),
        ..Default::default()
    };
    let app_state_for_local_use = Arc::new(app_state_value.clone());

    let mut builder = AiClientBuilder::<ContinuousAudioAppState>::new_with_model_and_state(
        api_key,
        model_name.clone(),
        app_state_value, // Pass S value to builder
    );

    builder = builder.generation_config(GenerationConfig {
        response_modalities: Some(vec![ResponseModality::Audio, ResponseModality::Text]),
        temperature: Some(0.7),
        ..Default::default()
    });
    builder = builder.voice("alloy".to_string());
    builder = builder.input_audio_format("pcm16".to_string());
    builder = builder.output_audio_format("pcm16".to_string());
    builder = builder.output_audio_transcription(AudioTranscriptionConfig {});
    builder = builder.system_instruction(Content {
        parts: vec![Part {
            text: Some("You are a helpful OpenAI voice assistant.".to_string()),
            ..Default::default()
        }],
        role: Some(Role::System),
        ..Default::default()
    });
    builder = builder.on_server_content(move |ctx, state_arc| handle_on_content(ctx, state_arc));
    builder =
        builder.on_usage_metadata(move |ctx, state_arc| handle_usage_metadata(ctx, state_arc));

    info!("Connecting OpenAI client...");
    let client_result = builder.connect_openai().await; 

    // ***** TEMPORARY TEST: START AUDIO OUTPUT AND WAIT *****
    info!("[DEBUG] Setting up audio output ONLY for test.");
    let (pb_tx_test, pb_rx_test) = bounded::<Vec<i16>>(10);
    let output_stream_test_keepalive = match setup_audio_output(pb_rx_test) { // Use the setup_audio_output
        Ok(s) => {
            info!("[DEBUG] Test output stream started. Main thread will sleep for 10s.");
            s // Keep the stream object alive
        }
        Err(e) => {
            error!("[DEBUG] Failed to setup test output stream: {:?}", e);
            return Err(e.into());
        }
    };

    // Keep pb_tx_test alive for the duration of the spawned task by moving it
    let test_sender_task = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(500)).await;
        info!("[DEBUG_SENDER] Attempting to send dummy silence to test playback channel.");
        if let Err(e) = pb_tx_test.try_send(vec![0; 2400]) { 
            error!("[DEBUG_SENDER] Failed to send dummy silence: {}", e); 
        } else {
            info!("[DEBUG_SENDER] Successfully sent dummy silence.");
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
        info!("[DEBUG_SENDER] Attempting to send dummy silence AGAIN to test playback channel.");
        if let Err(e) = pb_tx_test.try_send(vec![0; 2400]) {
            error!(
                "[DEBUG_SENDER] Failed to send dummy silence (2nd attempt): {}",
                e
            );
        } else {
            info!("[DEBUG_SENDER] Successfully sent dummy silence (2nd attempt).");
        }
        // pb_tx_test is dropped when this task finishes
    });

    tokio::time::sleep(Duration::from_secs(10)).await; 
    info!(
        "[DEBUG] Finished 10s sleep. If no CPAL errors or sender disconnects, output stream was stable."
    );
    
    // Explicitly drop the test stream and wait for the sender task to ensure its sender is dropped
    // before proceeding or if you were to exit here.
    drop(output_stream_test_keepalive); 
    let _ = test_sender_task.await; // Wait for the sender task to complete
    info!("[DEBUG] Test audio output resources cleaned up.");
    
    // If you want to exit after the test:
    // return Ok(()); 
    // ***** END TEMPORARY TEST *****


    // Proceed with the actual client and streams
    let client = client_result?; // Handle error from client connection
    let client_arc = Arc::new(tokio::sync::Mutex::new(client));

    info!("Setting up main audio input and output streams...");
    let _input_stream = setup_audio_input(
        audio_input_chunk_tx.clone(), 
        app_state_for_local_use.clone()
    )?;
    // Use the main playback_rx for the actual application output stream
    let _output_stream = setup_audio_output(playback_rx)?; 

    *app_state_for_local_use.is_microphone_active.lock().unwrap() = true;
    info!("Microphone active. Continuous OpenAI audio chat. Press Ctrl+C to exit.");
    
    // Main application loop
    let audio_processing_task_client = client_arc.clone();
    let audio_processing_task_state = app_state_for_local_use.clone();
    let audio_processing_task = tokio::spawn(async move {
        loop {
            tokio::select! {
                biased; // Prioritize shutdown/user speech end signals
                _ = audio_processing_task_state.user_speech_ended_signal.notified() => {
                    info!("[AudioProcessingTask] User speech ended signal. Sending audioStreamEnd and requesting response.");
                    let client_guard = audio_processing_task_client.lock().await;
                    if let Err(e) = client_guard.send_audio_stream_end().await {
                        error!("[AudioProcessingTask] Failed to send audioStreamEnd: {:?}", e);
                    }
                    // For continuous chat, OpenAI might not need explicit request_response after every user utterance end
                    // if VAD is handled by server or if stream is kept open for model to interject.
                    // However, for explicit turns, this is correct.
                    // if let Err(e) = client_guard.request_response().await {
                    //     error!("[AudioProcessingTask] Failed to request final response: {:?}", e);
                    // }
                    *audio_processing_task_state.is_microphone_active.lock().unwrap() = false; // Pause mic until model responds
                }
                maybe_samples = audio_input_chunk_rx.recv() => {
                    if let Some(samples) = maybe_samples {
                        if *audio_processing_task_state.is_microphone_active.lock().unwrap() && !samples.is_empty() {
                            let client_guard = audio_processing_task_client.lock().await;
                            if let Err(e) = client_guard.send_audio_chunk(&samples, INPUT_SAMPLE_RATE_HZ).await {
                                error!("[AudioProcessingTask] Failed to send audio chunk: {:?}", e);
                                if matches!(e, gemini_live_api::error::GeminiError::SendError | gemini_live_api::error::GeminiError::ConnectionClosed) {
                                    break; // Exit task if connection is truly gone
                                }
                            }
                        }
                    } else {
                        info!("[AudioProcessingTask] Audio input channel closed. Exiting task.");
                        break; // Exit task
                    }
                }
            }
        }
        info!("[AudioProcessingTask] Stopped.");
    });

    loop {
        tokio::select! {
            _ = app_state_for_local_use.model_turn_complete_signal.notified() => {
                info!("[MainLoop] Model turn complete. Clearing response text & re-activating mic (if desired).");
                app_state_for_local_use.full_response_text.lock().unwrap().clear();
                 // Re-enable microphone for the user's next turn after model finishes
                *app_state_for_local_use.is_microphone_active.lock().unwrap() = true;
                info!("[MainLoop] Microphone re-activated for user input.");
            }
            _ = tokio::signal::ctrl_c() => {
                info!("Ctrl+C received. Shutting down...");
                break;
            }
        }
    }
    
    info!("Exited main loop. Cleaning up...");
    *app_state_for_local_use.is_microphone_active.lock().unwrap() = false;
    app_state_for_local_use.user_speech_ended_signal.notify_waiters(); // Signal any pending VAD
    drop(audio_input_chunk_tx); // Close audio input channel

    // Wait for audio processing task to finish
    if let Err(e) = audio_processing_task.await {
        error!("Audio processing task panicked or encountered an error: {:?}", e);
    }

    client_arc.lock().await.close().await?;
    info!("OpenAI Client closed. Exiting application.");

    Ok(())
}

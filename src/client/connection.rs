use super::backend::{AppState, BackendConfig, LiveApiBackend, UnifiedServerEvent, WsSink};
use super::handlers::{Handlers, ServerContentContext, UsageMetadataContext};
use crate::error::GeminiError;
use crate::types::*; // Keep types needed for context structs
use futures_util::{SinkExt, StreamExt};
use std::sync::Arc;
use tokio::{
    net::TcpStream,
    sync::{Mutex as TokioMutex, mpsc, oneshot},
};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async, tungstenite::protocol::Message,
};
use tracing::{Instrument, Span, debug, error, info, trace, warn}; // Added Instrument, Span

// Signature updated to use trait objects and BackendConfig
pub(crate) fn spawn_processing_task(
    api_key: String,
    backend: Arc<dyn LiveApiBackend>,
    backend_config: BackendConfig,
    handlers: Arc<Handlers<dyn AppState + Send + Sync>>,
    state: Arc<dyn AppState + Send + Sync>,
    shutdown_rx: oneshot::Receiver<()>,
    outgoing_receiver: mpsc::Receiver<String>, // Receives raw JSON strings
) {
    let task_span = info_span!("connection_task", provider = ?backend.provider_type(), model = %backend_config.model);
    tokio::spawn(
        async move {
            info!("Processing task starting...");
            match connect_and_listen(
                api_key,
                backend,
                backend_config,
                handlers,
                state,
                shutdown_rx,
                outgoing_receiver,
            )
            .await
            {
                Ok(_) => info!("Processing task finished gracefully."),
                Err(e) => error!("Processing task failed: {:?}", e),
            }
        }
        .instrument(task_span), // Apply span to the entire task
    );
}

// Signature updated
async fn connect_and_listen(
    api_key: String,
    backend: Arc<dyn LiveApiBackend>,
    backend_config: BackendConfig,
    handlers: Arc<Handlers<dyn AppState + Send + Sync>>,
    state: Arc<dyn AppState + Send + Sync>,
    mut shutdown_rx: oneshot::Receiver<()>,
    mut outgoing_rx: mpsc::Receiver<String>,
) -> Result<(), GeminiError> {
    let url = backend.get_websocket_url(&api_key)?;
    info!("Attempting to connect to WebSocket: {}", url);

    // Build request using backend trait
    let base_request = http::Request::builder()
        .method("GET")
        .uri(url.as_str())
        .body(())
        .map_err(|e| {
            GeminiError::ConfigurationError(format!("Failed to build base request: {}", e))
        })?;

    let configured_request =
        backend.configure_websocket_request(&api_key, base_request, &backend_config)?;
    trace!("Configured WebSocket request: {:?}", configured_request);

    let (ws_stream, response) = match connect_async(configured_request).await {
        Ok(conn) => conn,
        Err(e) => {
            error!("WebSocket connection failed: {}", e);
            // Provide more context if possible (e.g., response status if available in error)
            return Err(GeminiError::WebSocketError(e));
        }
    };
    info!(
        "WebSocket handshake successful. Status: {}",
        response.status()
    );
    if !response.status().is_success() && response.status().as_u16() != 101 {
        // 101 is Switching Protocols, which is expected for WebSocket
        error!(
            "WebSocket handshake returned non-success status: {}",
            response.status()
        );
        // You might want to read the body here for more error details if available
        return Err(GeminiError::ApiError(format!(
            "WebSocket handshake failed with status {}",
            response.status()
        )));
    }

    let (ws_sink_split, mut ws_stream_split) = ws_stream.split();
    let ws_sink_arc = Arc::new(TokioMutex::new(ws_sink_split));

    // Send initial messages from backend
    let initial_messages = backend.get_initial_messages(&backend_config).await?;
    if !initial_messages.is_empty() {
        debug!("Sending {} initial message(s)...", initial_messages.len());
        let mut sink_guard = ws_sink_arc.lock().await;
        for msg in initial_messages {
            trace!("Sending initial message content: {}", msg);
            if let Err(e) = sink_guard.send(Message::Text(msg)).await {
                error!("Failed to send initial message: {}", e);
                return Err(GeminiError::WebSocketError(e)); // Consider this fatal
            }
        }
        drop(sink_guard); // Release lock
        debug!("Initial message(s) sent.");
    } else {
        debug!("No initial messages to send.");
    }

    // Store output format for audio decoding
    let output_audio_format = backend_config.output_audio_format.clone();

    // Main loop
    info!("Entering main processing loop...");
    loop {
        tokio::select! {
            biased; // Check shutdown first
            _ = &mut shutdown_rx => {
                info!("Shutdown signal received. Closing WebSocket connection.");
                let mut sink_guard = ws_sink_arc.lock().await;
                // Attempt graceful close
                let _ = sink_guard.send(Message::Close(None)).await;
                let _ = sink_guard.close().await;
                 drop(sink_guard);
                info!("WebSocket closed due to shutdown signal.");
                return Ok(());
            }
            maybe_outgoing = outgoing_rx.recv() => {
                if let Some(json_message) = maybe_outgoing {
                     trace!("Sending outgoing message ({} bytes)", json_message.len());
                     let mut sink_guard = ws_sink_arc.lock().await;
                     if let Err(e) = sink_guard.send(Message::Text(json_message)).await {
                          error!("Failed to send outgoing message via WebSocket: {}", e);
                          // Optional: Check if error is fatal (e.g., connection closed)
                           if matches!(e, tokio_tungstenite::tungstenite::Error::ConnectionClosed |
                                         tokio_tungstenite::tungstenite::Error::AlreadyClosed) {
                                error!("Connection closed, cannot send message. Exiting task.");
                                drop(sink_guard);
                                return Err(GeminiError::ConnectionClosed); // Fatal
                           }
                           // Otherwise, log and continue? Or return error? Depends on desired robustness.
                     }
                      drop(sink_guard); // Release lock
                } else {
                    info!("Outgoing message channel closed. Listener will stop accepting new messages.");
                    // Don't exit immediately, wait for WebSocket to close or shutdown signal
                }
            }
            msg_result = ws_stream_split.next() => {
                match msg_result {
                    Some(Ok(message)) => {
                         trace!("Received WebSocket message: {:?}", message);
                        let current_span = Span::current(); // Get span for instrumentation

                        // Use backend to parse message into UnifiedServerEvents
                         match backend.parse_server_message(
                             message,
                             &handlers,
                             &state,
                             &ws_sink_arc,
                             &output_audio_format // Pass format for decoding
                         ).instrument(current_span).await { // Instrument the parsing logic
                            Ok(unified_events) => {
                                if unified_events.is_empty() { continue; } // Skip if no events (e.g., ping/pong)

                                let mut should_stop = false;
                                for event in unified_events {
                                     trace!("Processing unified event: {:?}", event);
                                    if handle_unified_event(event, &handlers, &state).await {
                                        should_stop = true;
                                         break; // Stop processing events if Close received
                                    }
                                }
                                if should_stop {
                                     info!("Stop signal received from event handler (Close event). Exiting loop.");
                                     break; // Exit select loop
                                 }
                            }
                            Err(e) => {
                                error!("Error processing server message: {:?}", e);
                                // Decide if this is fatal. Maybe specific errors?
                                // For now, log and continue. Could return Err(e) to stop the task.
                            }
                        }
                    }
                    Some(Err(e)) => {
                         error!("WebSocket read error: {:?}", e);
                         // Treat read errors as fatal for now
                         return Err(GeminiError::WebSocketError(e));
                    }
                    None => {
                         info!("WebSocket stream ended (server closed connection).");
                         return Ok(()); // Graceful exit
                    }
                }
            }
        }
    } // End main loop

    info!("Listen loop exited. Cleaning up.");
    // Ensure sink is closed on loop exit (e.g., break due to should_stop)
    let mut sink_guard = ws_sink_arc.lock().await;
    let _ = sink_guard.close().await;
    drop(sink_guard);
    Ok(())
}

// Helper function to handle dispatching unified events to handlers
async fn handle_unified_event(
    event: UnifiedServerEvent,
    handlers: &Arc<Handlers<dyn AppState + Send + Sync>>,
    state: &Arc<dyn AppState + Send + Sync>,
) -> bool {
    // Returns true if processing should stop (Close event)
    match event {
        UnifiedServerEvent::SetupComplete => {
            info!("Connection setup complete.");
            // Optional: Call an on_setup_complete handler
        }
        UnifiedServerEvent::ContentUpdate { text, audio, done } => {
            if let Some(handler) = &handlers.on_server_content {
                // Adapt ServerContentContext
                let ctx = ServerContentContext {
                    text,
                    audio,
                    is_done: done,
                };
                handler.call(ctx, state.clone()).await;
            } else {
                // Default logging if no handler
                if let Some(t) = text {
                    info!("[Content Text]: {}", t);
                }
                if let Some(a) = audio {
                    info!("[Content Audio]: {} samples", a.len());
                }
                if done {
                    trace!("[Content Part Done]");
                }
            }
        }
        UnifiedServerEvent::TranscriptionUpdate { text, done } => {
            // Call a new on_transcription handler?
            // Example:
            // if let Some(handler) = &handlers.on_transcription {
            //     let ctx = TranscriptionContext { text, is_final: done };
            //     handler.call(ctx, state.clone()).await;
            // } else {
            info!(
                "[Transcription]: {}{}",
                text,
                if done { " (Final)" } else { "" }
            );
            // }
        }
        UnifiedServerEvent::ToolCall { id, name, args } => {
            // Tool calls are now initiated by the backend's parse_server_message,
            // which calls the handler directly. This event is more for notification.
            info!(
                "[Tool Call Requested] Name: '{}', ID: {:?}, Args: {:?}",
                name, id, args
            );
            // Optional: Call an on_tool_request handler if user needs to intercept/log
        }
        UnifiedServerEvent::ModelTurnComplete => {
            info!("Model turn complete.");
            // Optional: Call an on_turn_complete handler
        }
        UnifiedServerEvent::ModelGenerationComplete => {
            info!("Model generation complete.");
            // Optional: Call an on_generation_complete handler
        }
        UnifiedServerEvent::UsageMetadata(metadata) => {
            if let Some(handler) = &handlers.on_usage_metadata {
                let ctx = UsageMetadataContext { metadata };
                handler.call(ctx, state.clone()).await;
            } else {
                info!("[Usage Metadata]: {:?}", metadata);
            }
        }
        UnifiedServerEvent::Error(api_error) => {
            error!(
                "API Error Received: Code='{}', Msg='{}', ClientEventID={:?}",
                api_error.code, api_error.message, api_error.event_id
            );
            // Optional: Call an on_error handler
        }
        UnifiedServerEvent::ProviderSpecific(value) => {
            debug!("Received provider specific event: {:?}", value);
            // Optional: Call an on_provider_event handler
        }
        UnifiedServerEvent::Close => {
            info!("Server initiated connection close.");
            return true; // Signal to stop processing
        }
    }
    false // Continue processing
}

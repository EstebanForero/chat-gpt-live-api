use super::backend::{BackendConfig, LiveApiBackend, UnifiedServerEvent}; // AppState removed
use super::handlers::{Handlers, ServerContentContext, UsageMetadataContext};
use crate::error::GeminiError;
use futures_util::{SinkExt, StreamExt};
use std::sync::Arc;
use tokio::sync::{Mutex as TokioMutex, mpsc, oneshot};
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};
use tracing::{Instrument, Span, debug, error, info, info_span, trace};

// Now generic over S (state) and B (backend type)
pub(crate) fn spawn_processing_task<
    S: Clone + Send + Sync + 'static,
    B: LiveApiBackend<S> + 'static, // B implements LiveApiBackend<S>
>(
    api_key: String,
    backend: Arc<B>, // Arc<ConcreteBackend>
    backend_config: BackendConfig,
    handlers: Arc<Handlers<S>>,
    state: Arc<S>,
    shutdown_rx: oneshot::Receiver<()>,
    outgoing_receiver: mpsc::Receiver<String>,
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
        .instrument(task_span),
    );
}

async fn connect_and_listen<
    S: Clone + Send + Sync + 'static,
    B: LiveApiBackend<S> + 'static, // B implements LiveApiBackend<S>
>(
    api_key: String,
    backend: Arc<B>, // Arc<ConcreteBackend>
    backend_config: BackendConfig,
    handlers: Arc<Handlers<S>>,
    state: Arc<S>,
    mut shutdown_rx: oneshot::Receiver<()>,
    mut outgoing_rx: mpsc::Receiver<String>,
) -> Result<(), GeminiError> {
    // ... (URL and request setup unchanged, uses `backend` directly) ...
    let url = backend.get_websocket_url(&api_key)?;
    info!("Attempting to connect to WebSocket: {}", url);

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
            return Err(GeminiError::WebSocketError(e));
        }
    };
    info!(
        "WebSocket handshake successful. Status: {}",
        response.status()
    );
    if !response.status().is_success() && response.status().as_u16() != 101 {
        error!(
            "WebSocket handshake returned non-success status: {}",
            response.status()
        );
        return Err(GeminiError::ApiError(format!(
            "WebSocket handshake failed with status {}",
            response.status()
        )));
    }

    let (ws_sink_split, mut ws_stream_split) = ws_stream.split();
    // Provide explicit type for TokioMutex if needed, though often inferred.
    // WsSink is defined in backend.rs
    let ws_sink_arc: Arc<TokioMutex<super::backend::WsSink>> =
        Arc::new(TokioMutex::new(ws_sink_split));

    let initial_messages = backend.get_initial_messages(&backend_config).await?;
    if !initial_messages.is_empty() {
        debug!("Sending {} initial message(s)...", initial_messages.len());
        let mut sink_guard = ws_sink_arc.lock().await;
        for msg in initial_messages {
            trace!("Sending initial message content: {}", msg);
            if let Err(e) = sink_guard.send(Message::Text(msg.into())).await {
                error!("Failed to send initial message: {}", e);
                return Err(GeminiError::WebSocketError(e));
            }
        }
        drop(sink_guard);
        debug!("Initial message(s) sent.");
    } else {
        debug!("No initial messages to send.");
    }
    let output_audio_format = backend_config.output_audio_format.clone();

    info!("Entering main processing loop...");
    loop {
        tokio::select! {
            // ... (select logic unchanged, parse_server_message calls are now on Arc<B>) ...
            biased;
            _ = &mut shutdown_rx => {
                info!("Shutdown signal received. Closing WebSocket connection.");
                let mut sink_guard = ws_sink_arc.lock().await;
                let _ = sink_guard.send(Message::Close(None)).await;
                let _ = sink_guard.close().await;
                drop(sink_guard);
                info!("WebSocket closed due to shutdown signal.");
                return Ok(());
            }
            maybe_outgoing = outgoing_rx.recv() => {
                if let Some(json_message) = maybe_outgoing {
                    trace!("Sending outgoing message ({})", json_message);
                    let mut sink_guard = ws_sink_arc.lock().await;
                    if let Err(e) = sink_guard.send(Message::Text(json_message.into())).await {
                        error!("Failed to send outgoing message via WebSocket: {}", e);
                        if matches!(e, tokio_tungstenite::tungstenite::Error::ConnectionClosed | tokio_tungstenite::tungstenite::Error::AlreadyClosed) {
                            error!("Connection closed, cannot send message. Exiting task.");
                            drop(sink_guard);
                            return Err(GeminiError::ConnectionClosed);
                        }
                    }
                    drop(sink_guard);
                } else {
                    info!("Outgoing message channel closed. Listener will stop accepting new messages.");
                }
            }
            msg_result = ws_stream_split.next() => {
                match msg_result {
                    Some(Ok(message)) => {
                        trace!("Received WebSocket message type: {:?}", message);
                        let current_span = Span::current();
                        match backend.parse_server_message(
                            message, &handlers, &state, &ws_sink_arc, &output_audio_format
                        ).instrument(current_span).await {
                            Ok(unified_events) => {
                                if unified_events.is_empty() { continue; }
                                let mut should_stop = false;
                                for event in unified_events {
                                    trace!("Processing unified event: {:?}", event);
                                    if handle_unified_event(event, &handlers, &state).await {
                                        should_stop = true; break;
                                    }
                                }
                                if should_stop {
                                    info!("Stop signal received from event handler (Close event). Exiting loop.");
                                    break;
                                }
                            }
                            Err(e) => { error!("Error processing server message: {:?}", e); }
                        }
                    }
                    Some(Err(e)) => { error!("WebSocket read error: {:?}", e); return Err(GeminiError::WebSocketError(e)); }
                    None => { info!("WebSocket stream ended (server closed connection)."); return Ok(()); }
                }
            }
        }
    }

    info!("Listen loop exited. Cleaning up.");
    let mut sink_guard = ws_sink_arc.lock().await;
    let _ = sink_guard.close().await;
    drop(sink_guard);
    Ok(())
}

// handle_unified_event is now generic over S
async fn handle_unified_event<S: Clone + Send + Sync + 'static>(
    event: UnifiedServerEvent,
    handlers: &Arc<Handlers<S>>,
    state: &Arc<S>,
) -> bool {
    // ... (logic inside handle_unified_event unchanged)
    match event {
        UnifiedServerEvent::SetupComplete => {
            info!("Connection setup complete.");
        }
        UnifiedServerEvent::ContentUpdate { text, audio, done } => {
            if let Some(handler) = &handlers.on_server_content {
                let ctx = ServerContentContext {
                    text,
                    audio,
                    is_done: done,
                };
                handler.call(ctx, state.clone()).await;
            } else {
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
            info!(
                "[Transcription]: {}{}",
                text,
                if done { " (Final)" } else { "" }
            );
        }
        UnifiedServerEvent::ToolCall { id, name, args } => {
            info!(
                "[Tool Call Requested] Name: '{}', ID: {:?}, Args: {:?}",
                name, id, args
            );
        }
        UnifiedServerEvent::ModelTurnComplete => {
            info!("Model turn complete.");
        }
        UnifiedServerEvent::ModelGenerationComplete => {
            info!("Model generation complete.");
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
        }
        UnifiedServerEvent::ProviderSpecific(value) => {
            debug!("Received provider specific event: {:?}", value);
        }
        UnifiedServerEvent::Close => {
            info!("Server initiated connection close.");
            return true;
        }
    }
    false
}

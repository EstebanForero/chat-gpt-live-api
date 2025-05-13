use super::handlers::{Handlers, ServerContentContext, UsageMetadataContext};
use crate::error::GeminiError;
use crate::types::*;
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use std::sync::Arc;
use tokio::{
    net::TcpStream,
    sync::{Mutex as TokioMutex, mpsc, oneshot},
};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async, tungstenite::protocol::Message,
};
use tracing::{Instrument, Span, debug, error, info, trace, warn};
use url::Url;

const GEMINI_WS_ENDPOINT: &str = "wss://generativelanguage.googleapis.com/ws/google.ai.generativelanguage.v1beta.GenerativeService.BidiGenerateContent";
type WsSink = futures_util::stream::SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>;

pub(crate) fn spawn_processing_task<S: Clone + Send + Sync + 'static>(
    api_key: String,
    initial_setup: BidiGenerateContentSetup,
    handlers: Arc<Handlers<S>>,
    state: Arc<S>,
    shutdown_rx: oneshot::Receiver<()>,
    outgoing_receiver: mpsc::Receiver<ClientMessagePayload>,
) {
    tokio::spawn(async move {
        info!("Processing task starting...");
        match connect_and_listen(
            api_key,
            initial_setup,
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
    });
}

async fn connect_and_listen<S: Clone + Send + Sync + 'static>(
    api_key: String,
    initial_setup: BidiGenerateContentSetup,
    handlers: Arc<Handlers<S>>,
    state: Arc<S>,
    mut shutdown_rx: oneshot::Receiver<()>,
    mut outgoing_rx: mpsc::Receiver<ClientMessagePayload>,
) -> Result<(), GeminiError> {
    let url_with_key_str = format!("{}?key={}", GEMINI_WS_ENDPOINT, &api_key);
    Url::parse(&url_with_key_str)
        .map_err(|e| GeminiError::ApiError(format!("Invalid URL: {}", e)))?;
    info!("Connecting to WebSocket: {}", url_with_key_str);

    let connect_result = connect_async(&url_with_key_str).await;
    let (ws_stream, _) = connect_result.map_err(|e| {
        error!("WebSocket connection failed: {}", e);
        GeminiError::WebSocketError(e)
    })?;
    info!("WebSocket handshake successful.");

    let (ws_sink_split, mut ws_stream_split) = ws_stream.split();
    let ws_sink_arc = Arc::new(TokioMutex::new(ws_sink_split));

    let setup_payload = ClientMessagePayload::Setup(initial_setup.clone());
    let setup_json = serde_json::to_string(&setup_payload)?;
    ws_sink_arc
        .lock()
        .await
        .send(Message::Text(setup_json.into()))
        .await?;
    debug!("Sent initial setup message.");

    match serde_json::to_string_pretty(&initial_setup) {
        Ok(setup_str_pretty) => {
            info!(
                "Attempting to send initial setup message (BidiGenerateContentSetup):\n{}",
                setup_str_pretty
            );
        }
        Err(e) => {
            warn!(
                "Could not serialize initial_setup for pretty logging: {}. Sending raw debug.",
                e
            );
            info!(
                "Attempting to send initial setup message (BidiGenerateContentSetup) (debug):\n{:?}",
                initial_setup
            );
        }
    }

    debug!("Waiting for SetupComplete message...");
    let setup_complete_message = match ws_stream_split.next().await {
        Some(Ok(msg)) => msg,
        Some(Err(e)) => return Err(GeminiError::WebSocketError(e)),
        None => return Err(GeminiError::ConnectionClosed),
    };

    let setup_ok = match setup_complete_message.clone() {
        Message::Text(t) => serde_json::from_str::<ServerMessage>(&t)
            .ok()
            .is_some_and(|m| m.setup_complete.is_some()),
        Message::Binary(b) => String::from_utf8(b.into())
            .ok()
            .and_then(|t| serde_json::from_str::<ServerMessage>(&t).ok())
            .is_some_and(|m| m.setup_complete.is_some()),
        _ => false,
    };

    if setup_ok {
        info!("SetupComplete received and parsed.");
    } else {
        error!(
            "SetupComplete message not successfully received or parsed. First message: {:?}",
            setup_complete_message
        );
        return Err(GeminiError::UnexpectedMessage);
    }
    info!("Setup phase complete. Entering main listen loop...");

    loop {
        tokio::select! {
            biased;
            _ = &mut shutdown_rx => {
                info!("Shutdown signal received. Closing WebSocket.");
                let mut sink_guard = ws_sink_arc.lock().await;
                let _ = sink_guard.send(Message::Close(None)).await;
                let _ = sink_guard.close().await;
                return Ok(());
            }
            maybe_outgoing = outgoing_rx.recv() => {
                if let Some(payload) = maybe_outgoing {
                     trace!("Sending outgoing message: {:?}", payload);
                     let json_message = match serde_json::to_string(&payload) {
                        Ok(json) => json,
                        Err(e) => {
                            error!("Failed to serialize outgoing message: {}", e);
                            continue;
                        }
                     };
                     let mut sink_guard = ws_sink_arc.lock().await;
                     if let Err(e) = sink_guard.send(Message::Text(json_message.into())).await {
                          error!("Failed to send outgoing message via WebSocket: {}", e);
                     }
                } else {
                    info!("Outgoing message channel closed. Listener will exit when WebSocket closes.");
                }
            }
            msg_result = ws_stream_split.next() => {
                match msg_result {
                    Some(Ok(message)) => {
                        let current_span = Span::current();
                        let should_stop = process_server_message(
                            message,
                            &handlers,
                            &state,
                            &ws_sink_arc,
                        ).instrument(current_span).await?;
                        if should_stop {
                            info!("process_server_message indicated stop (e.g., Close frame).");
                            break;
                        }
                    }
                    Some(Err(e)) => {
                         error!("WebSocket read error: {:?}", e);
                         return Err(GeminiError::WebSocketError(e));
                    }
                    None => {
                         info!("WebSocket stream ended (server closed connection).");
                         return Ok(());
                    }
                }
            }
        }
    }
    info!("Listen loop exited. Closing sink.");
    let mut sink_guard = ws_sink_arc.lock().await;
    let _ = sink_guard.close().await;
    Ok(())
}

async fn process_server_message<S: Clone + Send + Sync + 'static>(
    message: Message,
    handlers: &Arc<Handlers<S>>,
    state: &Arc<S>,
    ws_sink_arc: &Arc<TokioMutex<WsSink>>,
) -> Result<bool, GeminiError> {
    let server_msg_text: Option<String> = match message {
        Message::Text(t) => Some(t.to_string()),
        Message::Binary(b) => String::from_utf8(b.into()).ok(),
        Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => return Ok(false),
        Message::Close(close_frame_opt) => {
            if let Some(close_frame) = close_frame_opt {
                error!(
                    "Received WebSocket Close frame from server. Code: {:?}, Reason: '{}'",
                    close_frame.code,
                    close_frame.reason.to_string(),
                );
            } else {
                info!("Received WebSocket Close frame from server (no specific code/reason).");
            }
            return Ok(true);
        }
    };

    if let Some(text_string) = server_msg_text {
        match serde_json::from_str::<ServerMessage>(&text_string) {
            Ok(server_message) => {
                let mut is_turn_complete = false;

                if let Some(content_data) = server_message.server_content {
                    if content_data.turn_complete {
                        is_turn_complete = true;
                    }
                    if let Some(handler) = &handlers.on_server_content {
                        let ctx = ServerContentContext {
                            content: content_data,
                        };
                        // Pass the state Arc<S>
                        handler.call(ctx, state.clone()).await;
                    }
                }

                if let Some(tool_call_data) = server_message.tool_call {
                    let mut responses_to_send = Vec::new();
                    for func_call in tool_call_data.function_calls {
                        if let Some(handler) = handlers.tool_handlers.get(&func_call.name) {
                            let call_id = func_call.id.clone();
                            let call_name = func_call.name.clone();
                            let handler_clone = handler.clone();
                            let state_clone_for_tool = state.clone();

                            let tool_result = handler_clone
                                .call(func_call.args, state_clone_for_tool)
                                .await;

                            let response_for_tool = match tool_result {
                                Ok(response_data) => FunctionResponse {
                                    id: call_id,
                                    name: call_name,
                                    response: response_data,
                                },
                                Err(e) => FunctionResponse {
                                    id: call_id,
                                    name: call_name,
                                    response: json!({"error": e}),
                                },
                            };
                            responses_to_send.push(response_for_tool);
                        } else {
                            warn!("No handler registered for tool: {}", func_call.name);
                            responses_to_send.push(FunctionResponse {
                                id: func_call.id,
                                name: func_call.name,
                                response: json!({"error": "Function not implemented by client."}),
                            });
                        }
                    }
                    let len_resp;
                    if !responses_to_send.is_empty() {
                        len_resp = responses_to_send.len();
                        let tool_resp_msg =
                            ClientMessagePayload::ToolResponse(BidiGenerateContentToolResponse {
                                function_responses: responses_to_send,
                            });
                        let json_msg = serde_json::to_string(&tool_resp_msg)?;
                        let mut sink_guard = ws_sink_arc.lock().await;
                        if let Err(e) = sink_guard.send(Message::Text(json_msg.into())).await {
                            error!("Failed to send tool response(s): {}", e);
                            return Err(GeminiError::WebSocketError(e));
                        }
                        info!("Sent {} tool response(s).", len_resp);
                    }
                }

                if let Some(metadata) = server_message.usage_metadata {
                    if let Some(handler) = &handlers.on_usage_metadata {
                        let ctx = UsageMetadataContext { metadata };
                        handler.call(ctx, state.clone()).await;
                    }
                }

                return Ok(false);
            }
            Err(e) => {
                error!(
                    "Failed to parse ServerMessage: {:?}, raw text size: {}",
                    e,
                    text_string.len()
                );
                trace!("Failed parse raw text: '{}'", text_string);
            }
        }
    }
    Ok(false)
}

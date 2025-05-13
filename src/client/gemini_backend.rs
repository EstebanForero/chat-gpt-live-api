use super::backend::*;
use crate::error::GeminiError;
use crate::types::*;
use async_trait::async_trait;
use base64::Engine as _;
use serde_json::json;
use std::sync::Arc;
use tokio::sync::Mutex as TokioMutex;
use tokio_tungstenite::tungstenite::protocol::Message;
use tracing::{debug, error, info, trace, warn};

pub struct GeminiBackend;

impl GeminiBackend {
    // Helper to decode Gemini audio blob
    fn decode_gemini_audio(blob: &Blob) -> Result<Vec<i16>, GeminiError> {
        // Assume mime_type is "audio/pcm;rate=..." or similar, implying linear16 LE
        match base64::engine::general_purpose::STANDARD.decode(&blob.data) {
            Ok(decoded_bytes) => {
                if decoded_bytes.len() % 2 != 0 {
                    warn!(
                        "Decoded Gemini audio data has odd number of bytes, discarding last byte."
                    );
                }
                let samples = decoded_bytes
                    .chunks_exact(2)
                    .map(|chunk| i16::from_le_bytes([chunk[0], chunk[1]]))
                    .collect();
                Ok(samples)
            }
            Err(e) => {
                error!("Failed to decode base64 audio data: {}", e);
                Err(GeminiError::DeserializationError(format!(
                    "Base64 decode failed: {}",
                    e
                )))
            }
        }
    }
}

#[async_trait]
impl LiveApiBackend for GeminiBackend {
    fn provider_type(&self) -> ApiProvider {
        ApiProvider::Gemini
    }

    fn get_websocket_url(&self, api_key: &str) -> Result<url::Url, GeminiError> {
        let url_str = format!(
            "wss://generativelanguage.googleapis.com/ws/google.ai.generativelanguage.v1beta.GenerativeService.BidiGenerateContent?key={}",
            api_key
        );
        url::Url::parse(&url_str)
            .map_err(|e| GeminiError::ConfigurationError(format!("Invalid Gemini URL: {}", e)))
    }

    fn configure_websocket_request(
        &self,
        _api_key: &str,
        request: http::Request<()>,
        _config: &BackendConfig,
    ) -> Result<http::Request<()>, GeminiError> {
        // No special headers needed for Gemini beyond the key in URL
        Ok(request)
    }

    async fn get_initial_messages(
        &self,
        config: &BackendConfig,
    ) -> Result<Vec<String>, GeminiError> {
        let setup = BidiGenerateContentSetup {
            model: config.model.clone(),
            generation_config: config.generation_config.clone(),
            system_instruction: config.system_instruction.clone(),
            tools: if config.tools.is_empty() {
                None
            } else {
                Some(vec![Tool {
                    function_declarations: config
                        .tools
                        .iter()
                        .map(|t| t.gemini_declaration.clone())
                        .collect(),
                }])
            },
            realtime_input_config: config.realtime_input_config.clone(),
            output_audio_transcription: config.output_audio_transcription.clone(),
            // Add other fields from BackendConfig mapped to Gemini Setup
            session_resumption: None,         // Add if needed
            context_window_compression: None, // Add if needed
        };
        let payload = ClientMessagePayload::Setup(setup);
        let json = serde_json::to_string(&payload)?;
        Ok(vec![json])
    }

    fn build_session_update_message(&self, _config: &BackendConfig) -> Result<String, GeminiError> {
        Err(GeminiError::UnsupportedOperation(
            "Session update message not applicable for Gemini".to_string(),
        ))
    }

    fn build_text_turn_message(
        &self,
        text: String,
        // end_of_turn: bool // Gemini uses turn_complete in ClientContent
    ) -> Result<String, GeminiError> {
        let content_part = Part {
            text: Some(text),
            ..Default::default()
        };
        let content = Content {
            parts: vec![content_part],
            role: Some(Role::User), // Assume user role for simplicity
            ..Default::default()
        };
        // For Gemini, sending text implies a turn. We set turn_complete=true.
        // If multi-part turns are needed, the client handle logic needs adjustment.
        let client_content_msg = BidiGenerateContentClientContent {
            turns: Some(vec![content]),
            turn_complete: Some(true),
        };
        let payload = ClientMessagePayload::ClientContent(client_content_msg);
        serde_json::to_string(&payload).map_err(GeminiError::from)
    }

    fn build_audio_chunk_message(
        &self,
        audio_samples: &[i16],
        sample_rate: u32,
        _config: &BackendConfig, // Format assumed pcm16 for Gemini
    ) -> Result<String, GeminiError> {
        if audio_samples.is_empty() {
            // Sending empty might be valid, but let's prevent it for now
            return Err(GeminiError::ConfigurationError(
                "Audio samples cannot be empty".to_string(),
            ));
        }
        let mut byte_data = Vec::with_capacity(audio_samples.len() * 2);
        for sample in audio_samples {
            byte_data.extend_from_slice(&sample.to_le_bytes());
        }
        let encoded_data = base64::engine::general_purpose::STANDARD.encode(&byte_data);
        // Gemini uses MIME type in the blob
        let mime_type = format!("audio/pcm;rate={}", sample_rate);
        let audio_blob = Blob {
            mime_type,
            data: encoded_data,
        };
        let payload = ClientMessagePayload::RealtimeInput(BidiGenerateContentRealtimeInput {
            audio: Some(audio_blob),
            ..Default::default()
        });
        serde_json::to_string(&payload).map_err(GeminiError::from)
    }

    fn build_audio_stream_end_message(&self) -> Result<String, GeminiError> {
        let payload = ClientMessagePayload::RealtimeInput(BidiGenerateContentRealtimeInput {
            audio_stream_end: Some(true),
            ..Default::default()
        });
        serde_json::to_string(&payload).map_err(GeminiError::from)
    }

    fn build_tool_response_message(
        &self,
        responses: Vec<FunctionResponse>,
    ) -> Result<String, GeminiError> {
        let payload = ClientMessagePayload::ToolResponse(BidiGenerateContentToolResponse {
            function_responses: responses,
        });
        serde_json::to_string(&payload).map_err(GeminiError::from)
    }

    fn build_request_response_message(&self) -> Result<String, GeminiError> {
        Err(GeminiError::UnsupportedOperation(
            "Explicit response request not applicable for Gemini".to_string(),
        ))
    }

    async fn parse_server_message(
        &self,
        message: Message,
        handlers: &Arc<Handlers<dyn AppState + Send + Sync>>,
        state: &Arc<dyn AppState + Send + Sync>,
        ws_sink: &Arc<TokioMutex<WsSink>>,
        _output_audio_format: &Option<String>, // Gemini audio is always linear16 LE based on blob
    ) -> Result<Vec<UnifiedServerEvent>, GeminiError> {
        let text = match message {
            Message::Text(t) => t,
            Message::Binary(b) => String::from_utf8(b).map_err(|e| {
                GeminiError::DeserializationError(format!("Invalid UTF-8 in binary message: {}", e))
            })?,
            Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => return Ok(vec![]),
            Message::Close(_) => return Ok(vec![UnifiedServerEvent::Close]),
        };

        match serde_json::from_str::<ServerMessage>(&text) {
            Ok(gemini_msg) => {
                let mut unified_events = Vec::new();

                if gemini_msg.setup_complete.is_some() {
                    unified_events.push(UnifiedServerEvent::SetupComplete);
                }

                if let Some(content) = gemini_msg.server_content {
                    let mut combined_text = String::new();
                    let mut audio_samples: Option<Vec<i16>> = None;

                    if let Some(turn) = &content.model_turn {
                        for part in &turn.parts {
                            if let Some(t) = &part.text {
                                combined_text.push_str(t);
                                combined_text.push(' '); // Add space between parts
                            }
                            if let Some(blob) = &part.inline_data {
                                if blob.mime_type.starts_with("audio/") {
                                    match Self::decode_gemini_audio(blob) {
                                        Ok(samples) => {
                                            // Combine audio chunks if multiple present? Assume only one for now.
                                            if audio_samples.is_none() {
                                                audio_samples = Some(Vec::new());
                                            }
                                            audio_samples.as_mut().unwrap().extend(samples);
                                        }
                                        Err(e) => unified_events.push(UnifiedServerEvent::Error(
                                            ApiError {
                                                code: "audio_decode_error".to_string(),
                                                message: e.to_string(),
                                                event_id: None,
                                            },
                                        )),
                                    }
                                }
                            }
                        }
                    }
                    let final_text = combined_text.trim();
                    let text_option = if final_text.is_empty() {
                        None
                    } else {
                        Some(final_text.to_string())
                    };

                    // Emit content update if there's text or audio
                    if text_option.is_some() || audio_samples.is_some() {
                        unified_events.push(UnifiedServerEvent::ContentUpdate {
                            text: text_option,
                            audio: audio_samples,
                            // Gemini's 'done' flags are separate ('turn_complete', 'generation_complete')
                            // We mark 'done' here as false, and emit separate events below.
                            done: false,
                        });
                    }

                    if let Some(transcription) = content.output_transcription {
                        // Gemini transcription seems to be final per message
                        unified_events.push(UnifiedServerEvent::TranscriptionUpdate {
                            text: transcription.text,
                            done: true,
                        });
                    }

                    // Map Gemini's completion flags
                    if content.turn_complete {
                        unified_events.push(UnifiedServerEvent::ModelTurnComplete);
                    }
                    if content.generation_complete {
                        unified_events.push(UnifiedServerEvent::ModelGenerationComplete);
                    }
                }

                if let Some(tool_call_data) = gemini_msg.tool_call {
                    let mut responses_to_send = Vec::new();
                    for func_call in tool_call_data.function_calls {
                        let call_id = func_call.id.clone(); // Gemini provides ID
                        let call_name = func_call.name.clone();
                        let args_value = func_call.args.unwrap_or(Value::Null); // Pass args

                        unified_events.push(UnifiedServerEvent::ToolCall {
                            id: call_id.clone(),
                            name: call_name.clone(),
                            args: args_value.clone(),
                        });

                        // Execute handler
                        if let Some(handler) = handlers.tool_handlers.get(&call_name) {
                            let handler_clone = handler.clone();
                            let state_clone = state.clone();
                            match handler_clone.call(Some(args_value), state_clone).await {
                                Ok(response_data) => {
                                    responses_to_send.push(FunctionResponse {
                                        id: call_id,
                                        name: call_name,
                                        response: response_data,
                                    });
                                }
                                Err(e) => {
                                    warn!("Tool handler '{}' failed: {}", call_name, e);
                                    responses_to_send.push(FunctionResponse {
                                        id: call_id,
                                        name: call_name,
                                        response: json!({"error": e}),
                                    });
                                }
                            }
                        } else {
                            warn!("No handler registered for tool: {}", call_name);
                            responses_to_send.push(FunctionResponse {
                                id: call_id,
                                name: call_name,
                                response: json!({"error": "Function not implemented by client."}),
                            });
                        }
                    }

                    if !responses_to_send.is_empty() {
                        match self.build_tool_response_message(responses_to_send) {
                            Ok(json_msg) => {
                                let mut sink = ws_sink.lock().await;
                                if let Err(e) = sink.send(Message::Text(json_msg)).await {
                                    error!("Failed to send tool response(s): {}", e);
                                    // Consider returning error or adding to unified_events
                                } else {
                                    info!("Sent {} tool response(s).", responses_to_send.len());
                                }
                            }
                            Err(e) => {
                                error!("Failed to build tool response message: {}", e);
                            }
                        }
                    }
                }

                if let Some(metadata) = gemini_msg.usage_metadata {
                    unified_events.push(UnifiedServerEvent::UsageMetadata(metadata));
                }

                if gemini_msg.go_away.is_some() {
                    warn!("Received GoAway message from Gemini server.");
                    unified_events.push(UnifiedServerEvent::Close); // Treat GoAway as close signal
                }
                if gemini_msg.tool_call_cancellation.is_some() {
                    warn!(
                        "Received ToolCallCancellation from Gemini - specific handling not implemented."
                    );
                    // Potentially add a specific UnifiedServerEvent::ToolCallCancelled
                }
                if gemini_msg.session_resumption_update.is_some() {
                    info!(
                        "Received SessionResumptionUpdate from Gemini - specific handling not implemented."
                    );
                    // Potentially store the handle in AppState?
                }

                Ok(unified_events)
            }
            Err(e) => {
                error!("Failed to parse Gemini ServerMessage: {}", e);
                trace!("Raw Gemini message: {}", text);
                Err(GeminiError::DeserializationError(e.to_string()))
            }
        }
    }
}

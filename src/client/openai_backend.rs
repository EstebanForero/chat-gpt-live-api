use super::backend::*;
use crate::error::GeminiError;
use crate::types::{FunctionResponse, UsageMetadata}; // Only need specific types
use async_trait::async_trait;
use base64::Engine as _;
use http::header;
use serde_json::{Value, json};
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::Mutex as TokioMutex;
use tokio_tungstenite::tungstenite::protocol::Message;
use tracing::{debug, error, info, trace, warn};

pub struct OpenAiBackend;

impl OpenAiBackend {
    // Helper to decode OpenAI audio (assuming pcm16)
    fn decode_openai_audio(
        b64_data: &str,
        format: &Option<String>,
    ) -> Result<Vec<i16>, GeminiError> {
        // Determine format - default to pcm16 if not specified otherwise
        let is_pcm16 = format.as_deref().unwrap_or("pcm16") == "pcm16"; // Add checks for g711 etc. later

        if !is_pcm16 {
            return Err(GeminiError::UnsupportedOperation(format!(
                "Audio decoding for format '{}' not yet implemented for OpenAI",
                format.as_deref().unwrap_or("unknown")
            )));
        }

        match base64::engine::general_purpose::STANDARD.decode(b64_data) {
            Ok(decoded_bytes) => {
                if decoded_bytes.len() % 2 != 0 {
                    warn!("Decoded OpenAI audio data has odd number of bytes.");
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
impl LiveApiBackend for OpenAiBackend {
    fn provider_type(&self) -> ApiProvider {
        ApiProvider::OpenAi
    }

    fn get_websocket_url(&self, _api_key: &str) -> Result<url::Url, GeminiError> {
        let url_str = "wss://api.openai.com/v1/realtime"; // Key goes in header
        url::Url::parse(url_str)
            .map_err(|e| GeminiError::ConfigurationError(format!("Invalid OpenAI URL: {}", e)))
    }

    fn configure_websocket_request(
        &self,
        api_key: &str,
        mut request: http::Request<()>,
        config: &BackendConfig,
    ) -> Result<http::Request<()>, GeminiError> {
        request.headers_mut().insert(
            header::AUTHORIZATION,
            header::HeaderValue::from_str(&format!("Bearer {}", api_key)).map_err(|e| {
                GeminiError::ConfigurationError(format!("Invalid API key format: {}", e))
            })?,
        );
        request.headers_mut().insert(
            "OpenAI-Beta", // This might change after beta
            header::HeaderValue::from_static("realtime=v1"),
        );

        // Add model as query parameter
        let original_uri = request.uri().clone();
        let mut parts = original_uri.into_parts();
        let current_path = parts
            .path_and_query
            .as_ref()
            .map(|p| p.path())
            .unwrap_or("/");
        let model_param = format!("model={}", config.model); // Use model from config

        let new_query = match parts.path_and_query.as_ref().and_then(|p| p.query()) {
            Some(q) => format!("{}&{}", q, model_param),
            None => model_param,
        };
        let new_path_and_query_str = format!("{}?{}", current_path, new_query);

        parts.path_and_query = Some(
            http::uri::PathAndQuery::from_str(&new_path_and_query_str).map_err(|e| {
                GeminiError::ConfigurationError(format!("Failed to build query string: {}", e))
            })?,
        );

        *request.uri_mut() = http::Uri::from_parts(parts).map_err(|e| {
            GeminiError::ConfigurationError(format!("Failed to reconstruct URI: {}", e))
        })?;

        Ok(request)
    }

    async fn get_initial_messages(
        &self,
        config: &BackendConfig,
    ) -> Result<Vec<String>, GeminiError> {
        // For OpenAI, the primary initial action is session configuration via session.update
        Ok(vec![self.build_session_update_message(config)?])
    }

    fn build_session_update_message(&self, config: &BackendConfig) -> Result<String, GeminiError> {
        let mut session_data = serde_json::Map::new();

        // System Instructions (Prompt)
        if let Some(instr) = &config.system_instruction {
            if let Some(part) = instr.parts.first() {
                if let Some(text) = &part.text {
                    session_data.insert("instructions".to_string(), Value::String(text.clone()));
                }
            }
        }

        // Tools
        if !config.tools.is_empty() {
            let openai_tools = config
                .tools
                .iter()
                .map(|t| t.openai_definition.clone())
                .collect::<Vec<_>>();
            session_data.insert("tools".to_string(), Value::Array(openai_tools));
            session_data.insert("tool_choice".to_string(), Value::String("auto".to_string())); // Make configurable?
        }

        // Voice
        if let Some(voice) = &config.voice {
            session_data.insert("voice".to_string(), Value::String(voice.clone()));
        }

        // Audio Formats
        if let Some(format) = &config.input_audio_format {
            session_data.insert(
                "input_audio_format".to_string(),
                Value::String(format.clone()),
            );
        }
        if let Some(format) = &config.output_audio_format {
            session_data.insert(
                "output_audio_format".to_string(),
                Value::String(format.clone()),
            );
        }

        // Transcription Config (if output_audio_transcription is set)
        if config.output_audio_transcription.is_some() || config.transcription_model.is_some() {
            let mut transcription_config = serde_json::Map::new();
            if let Some(model) = &config.transcription_model {
                transcription_config.insert("model".to_string(), Value::String(model.clone()));
            } else {
                // Default transcription model if none provided but transcription enabled
                transcription_config.insert(
                    "model".to_string(),
                    Value::String("gpt-4o-transcribe".to_string()),
                );
            }
            if let Some(lang) = &config.transcription_language {
                transcription_config.insert("language".to_string(), Value::String(lang.clone()));
            }
            if let Some(prompt) = &config.transcription_prompt {
                transcription_config.insert("prompt".to_string(), Value::String(prompt.clone()));
            }
            session_data.insert(
                "input_audio_transcription".to_string(),
                Value::Object(transcription_config),
            );
        }

        // Include Logprobs
        if config.include_logprobs {
            session_data.insert(
                "include".to_string(),
                Value::Array(vec![Value::String(
                    "item.input_audio_transcription.logprobs".to_string(),
                )]),
            );
        }

        // Generation Config (Map relevant fields)
        // Note: OpenAI Realtime API might have different config options than Chat Completions
        if let Some(gen_config) = &config.generation_config {
            if let Some(temp) = gen_config.temperature {
                // Assuming OpenAI uses 'temperature' directly in session.update
                session_data.insert("temperature".to_string(), json!(temp));
            }
            // Map other relevant fields like top_p, max_tokens if supported
        }

        let update_payload = json!({
            "type": "session.update",
            "session": Value::Object(session_data)
        });

        serde_json::to_string(&update_payload).map_err(GeminiError::from)
    }

    fn build_text_turn_message(
        &self,
        text: String,
        // end_of_turn: bool, // Not applicable here
    ) -> Result<String, GeminiError> {
        let payload = json!({
            "type": "conversation.item.create",
            "item": {
                "type": "message",
                "role": "user", // Assuming user role
                "content": [{ "type": "input_text", "text": text }]
            }
            // Client needs to call request_response() separately to trigger generation
        });
        serde_json::to_string(&payload).map_err(GeminiError::from)
    }

    fn build_audio_chunk_message(
        &self,
        audio_samples: &[i16],
        _sample_rate: u32, // Rate is usually set in session.update format
        config: &BackendConfig,
    ) -> Result<String, GeminiError> {
        if audio_samples.is_empty() {
            return Err(GeminiError::ConfigurationError(
                "Audio samples cannot be empty".to_string(),
            ));
        }

        // Assume input format needs pcm16 encoding
        let is_pcm16 = config.input_audio_format.as_deref().unwrap_or("pcm16") == "pcm16";
        if !is_pcm16 {
            return Err(GeminiError::UnsupportedOperation(format!(
                "Audio encoding for format '{}' not yet implemented for OpenAI",
                config.input_audio_format.as_deref().unwrap_or("unknown")
            )));
        }

        let mut byte_data = Vec::with_capacity(audio_samples.len() * 2);
        for sample in audio_samples {
            byte_data.extend_from_slice(&sample.to_le_bytes());
        }
        let encoded_data = base64::engine::general_purpose::STANDARD.encode(&byte_data);

        let payload = json!({
            "type": "input_audio_buffer.append",
            "audio": encoded_data
            // Format is part of session config
        });
        serde_json::to_string(&payload).map_err(GeminiError::from)
    }

    fn build_audio_stream_end_message(&self) -> Result<String, GeminiError> {
        // This likely corresponds to committing the buffer if VAD is off,
        // or is handled automatically by VAD if on. We send commit.
        let payload = json!({ "type": "input_audio_buffer.commit" });
        serde_json::to_string(&payload).map_err(GeminiError::from)
    }

    fn build_tool_response_message(
        &self,
        responses: Vec<FunctionResponse>,
    ) -> Result<String, GeminiError> {
        if let Some(response) = responses.first() {
            // OpenAI expects the response 'output' to be a JSON *string*
            let output_string = match serde_json::to_string(&response.response) {
                Ok(s) => s,
                Err(e) => {
                    // Fallback: send error string if serialization fails
                    warn!(
                        "Failed to serialize tool response value: {}. Sending as error string.",
                        e
                    );
                    json!({ "error": format!("Failed to serialize response: {}", e) }).to_string()
                }
            };

            let payload = json!({
                "type": "conversation.item.create",
                "item": {
                    "type": "function_call_output",
                    "call_id": response.id, // MUST match the ID from the function_call event
                    "output": output_string // Send as JSON string
                }
            });
            // Client must call request_response() afterwards
            serde_json::to_string(&payload).map_err(GeminiError::from)
        } else {
            Err(GeminiError::ConfigurationError(
                "No tool responses provided for OpenAI".to_string(),
            ))
        }
    }

    fn build_request_response_message(&self) -> Result<String, GeminiError> {
        let payload = json!({
            "type": "response.create",
            "response": {
                // Can specify modalities here if needed, e.g., ["text", "audio"]
                // Defaults to session config.
            }
        });
        serde_json::to_string(&payload).map_err(GeminiError::from)
    }

    async fn parse_server_message(
        &self,
        message: Message,
        handlers: &Arc<Handlers<dyn AppState + Send + Sync>>,
        state: &Arc<dyn AppState + Send + Sync>,
        ws_sink: &Arc<TokioMutex<WsSink>>,
        output_audio_format: &Option<String>,
    ) -> Result<Vec<UnifiedServerEvent>, GeminiError> {
        let text = match message {
            Message::Text(t) => t,
            Message::Binary(b) => String::from_utf8(b).map_err(|e| {
                GeminiError::DeserializationError(format!("Invalid UTF-8 in binary message: {}", e))
            })?,
            Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => return Ok(vec![]),
            Message::Close(_) => return Ok(vec![UnifiedServerEvent::Close]),
        };

        match serde_json::from_str::<Value>(&text) {
            Ok(openai_event) => {
                let event_type = openai_event.get("type").and_then(|v| v.as_str());
                let mut unified_events = Vec::new();
                let event_id = openai_event
                    .get("event_id")
                    .and_then(|v| v.as_str())
                    .map(String::from); // Correlate errors

                trace!("Received OpenAI event: {:?}", event_type);

                match event_type {
                    Some("session.created") | Some("session.updated") => {
                        // Consider setup complete after first created/updated
                         unified_events.push(UnifiedServerEvent::SetupComplete);
                         // TODO: Could parse session details and update local config/state if needed
                    }
                    Some("response.created") => {
                         // Indicates model is starting to generate
                         debug!("OpenAI response generation started.");
                    }
                    Some("response.text.delta") => {
                        let delta = openai_event
                            .get("delta")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        if !delta.is_empty() {
                             unified_events.push(UnifiedServerEvent::ContentUpdate {
                                text: Some(delta),
                                audio: None,
                                done: false,
                            });
                        }
                    }
                    Some("response.audio.delta") => {
                        if let Some(audio_b64) = openai_event.get("delta").and_then(|v| v.as_str())
                        {
                             match Self::decode_openai_audio(audio_b64, output_audio_format) {
                                Ok(samples) => {
                                     unified_events.push(UnifiedServerEvent::ContentUpdate { text: None, audio: Some(samples), done: false });
                                },
                                Err(e) => unified_events.push(UnifiedServerEvent::Error(ApiError{ code: "audio_decode_error".to_string(), message: e.to_string(), event_id })),
                            }
                        }
                    }
                     Some("conversation.item.input_audio_transcription.delta") => {
                         let delta = openai_event.get("delta").and_then(|v| v.as_str()).unwrap_or("").to_string();
                          if !delta.is_empty() {
                             unified_events.push(UnifiedServerEvent::TranscriptionUpdate { text: delta, done: false });
                         }
                     }
                     Some("conversation.item.input_audio_transcription.completed") => {
                         let transcript = openai_event.get("transcript").and_then(|v| v.as_str()).unwrap_or("").to_string();
                         unified_events.push(UnifiedServerEvent::TranscriptionUpdate { text: transcript, done: true });
                         // Also check for logprobs if requested
                     }
                    Some("response.done") => {
                        // Check for function calls within the response.output array
                        if let Some(outputs) = openai_event
                            .get("response")
                            .and_then(|r| r.get("output"))
                            .and_then(|o| o.as_array())
                        {
                            for output in outputs {
                                if output.get("type").and_then(|t| t.as_str())
                                    == Some("function_call")
                                {
                                    let name = output
                                        .get("name")
                                        .and_then(|n| n.as_str())
                                        .unwrap_or("")
                                        .to_string();
                                    let args_str = output
                                        .get("arguments")
                                        .and_then(|a| a.as_str())
                                        .unwrap_or("{}"); // Arguments are a JSON string
                                    let call_id = output
                                        .get("call_id")
                                        .and_then(|id| id.as_str())
                                        .map(String::from);

                                    // Attempt to parse the arguments string
                                    match serde_json::from_str::<Value>(args_str) {
                                        Ok(args_value) => {
                                             unified_events.push(UnifiedServerEvent::ToolCall {
                                                 id: call_id.clone(),
                                                 name: name.clone(),
                                                 args: args_value.clone(),
                                             });

                                             // Execute handler
                                             if let Some(handler) = handlers.tool_handlers.get(&name) {
                                                 let handler_clone = handler.clone();
                                                 let state_clone = state.clone();
                                                 match handler_clone.call(Some(args_value), state_clone).await {
                                                     Ok(response_data) => {
                                                          match self.build_tool_response_message(vec![FunctionResponse { id: call_id.clone(), name: name.clone(), response: response_data }]) {
                                                             Ok(json_msg) => {
                                                                 let mut sink = ws_sink.lock().await;
                                                                 // Send response AND trigger next model turn
                                                                 if sink.send(Message::Text(json_msg)).await.is_ok() {
                                                                      if let Ok(req_resp_msg) = self.build_request_response_message() {
                                                                         let _ = sink.send(Message::Text(req_resp_msg)).await; // Ignore error here?
                                                                         info!("Sent tool response and requested next response for '{}'.", name);
                                                                      } else { error!("Failed to build request_response message after tool call."); }
                                                                 } else { error!("Failed to send tool response for '{}'.", name); }
                                                             }
                                                             Err(e) => error!("Failed to build tool response message for '{}': {}", name, e),
                                                         }
                                                     },
                                                     Err(e) => {
                                                         warn!("Tool handler '{}' failed: {}", name, e);
                                                          // Send error back to model
                                                          match self.build_tool_response_message(vec![FunctionResponse { id: call_id.clone(), name: name.clone(), response: json!({"error": e}) }]) {
                                                             Ok(json_msg) => {
                                                                 let mut sink = ws_sink.lock().await;
                                                                 if sink.send(Message::Text(json_msg)).await.is_ok() {
                                                                      if let Ok(req_resp_msg) = self.build_request_response_message() {
                                                                         let _ = sink.send(Message::Text(req_resp_msg)).await;
                                                                         info!("Sent tool error response and requested next response for '{}'.", name);
                                                                      } else { error!("Failed to build request_response message after tool error."); }
                                                                 } else { error!("Failed to send tool error response for '{}'.", name); }
                                                             }
                                                             Err(e) => error!("Failed to build tool error response message for '{}': {}", name, e),
                                                         }
                                                     }
                                                 }
                                             } else {
                                                  warn!("No handler registered for tool: {}", name);
                                                   // Send generic error back?
                                             }
                                        }
                                        Err(e) => {
                                            error!("Failed to parse tool arguments for '{}': {}", name, e);
                                            unified_events.push(UnifiedServerEvent::Error(ApiError {
                                                code: "tool_args_parse_error".to_string(),
                                                message: format!("Failed to parse args for {}: {}", name, e),
                                                event_id: None, // No client event ID here
                                            }));
                                        }
                                    }
                                }
                                // Handle other output types if necessary
                            }
                        }
                        // Mark content update as done (might have been text or audio)
                        unified_events.push(UnifiedServerEvent::ContentUpdate {
                            text: None,
                            audio: None,
                            done: true,
                        });
                         unified_events.push(UnifiedServerEvent::ModelTurnComplete);
                         // Does response.done mean generation is fully complete too? Assume so for now.
                         unified_events.push(UnifiedServerEvent::ModelGenerationComplete);
                    }
                    Some("error") => {
                        let code = openai_event
                            .get("code")
                            .and_then(|c| c.as_str())
                            .unwrap_or("unknown")
                            .to_string();
                        let message = openai_event
                            .get("message")
                            .and_then(|m| m.as_str())
                            .unwrap_or("")
                            .to_string();
                         unified_events.push(UnifiedServerEvent::Error(ApiError {
                            code,
                            message,
                            event_id,
                        }));
                    }
                     Some("rate_limits.updated") => {
                        // Parse rate limit info if needed
                         if let Some(usage_val) = openai_event.get("response").and_then(|r| r.get("usage")) {
                             // Attempt to map OpenAI usage to Gemini's UsageMetadata struct
                             // This might be an inexact mapping
                             match serde_json::from_value::<UsageMetadata>(usage_val.clone()) {
                                Ok(metadata) => unified_events.push(UnifiedServerEvent::UsageMetadata(metadata)),
                                Err(e) => warn!("Failed to parse OpenAI usage metadata: {}", e),
                            }
                         }
                    }
                     Some(other_type @ "input_audio_buffer.speech_started") |
                     Some(other_type @ "input_audio_buffer.speech_stopped") |
                     Some(other_type @ "input_audio_buffer.committed") |
                     Some(other_type @ "input_audio_buffer.cleared") |
                     Some(other_type @ "response.audio.done") | // Contains transcript
                     Some(other_type @ "response.text.done") | // Redundant with response.done?
                     Some(other_type @ "response.content_part.added") | // Intermediate events
                     Some(other_type @ "response.content_part.done") |
                     Some(other_type @ "response.output_item.added") |
                     Some(other_type @ "response.output_item.done") |
                     Some(other_type @ "conversation.item.created") |
                     Some(other_type @ "conversation.item.updated") |
                     Some(other_type @ "conversation.item.deleted")
                      => {
                        // Log potentially useful intermediate events but don't necessarily surface them
                        debug!("Received intermediate OpenAI event: {}", other_type);
                        // Could add to ProviderSpecific if needed by advanced users
                        // unified_events.push(UnifiedServerEvent::ProviderSpecific(openai_event.clone()));
                    }
                    Some(unhandled_type) => {
                        warn!("Unhandled OpenAI event type: {}", unhandled_type);
                        unified_events.push(UnifiedServerEvent::ProviderSpecific(
                            openai_event.clone(),
                        ));
                    }
                    None => {
                        warn!("Received OpenAI message without a 'type' field: {}", text);
                    }
                }
                Ok(unified_events)
            }
            Err(e) => {
                error!("Failed to parse OpenAI message JSON: {}", e);
                trace!("Raw OpenAI message: {}", text);
                Err(GeminiError::DeserializationError(e.to_string()))
            }
        }
    }
}

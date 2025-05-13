// src/client/openai_backend.rs
use super::backend::*;
use crate::ResponseModality;
use crate::client::handlers::Handlers;
use crate::error::GeminiError;
use crate::types::{FunctionResponse, UsageMetadata};
use async_trait::async_trait;
use base64::Engine as _;
use futures_util::SinkExt;
use http::header;
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::sync::Mutex as TokioMutex;
use tokio_tungstenite::tungstenite::client::IntoClientRequest; // For request manipulation
use tokio_tungstenite::tungstenite::protocol::Message;
use tracing::{debug, error, info, trace, warn};

pub struct OpenAiBackend;

impl OpenAiBackend {
    fn decode_openai_audio(
        /* ... unchanged ... */
        b64_data: &str,
        format: &Option<String>,
    ) -> Result<Vec<i16>, GeminiError> {
        let format_str = format.as_deref().unwrap_or("pcm16");
        if format_str != "pcm16" {
            return Err(GeminiError::UnsupportedOperation(format!(
                "Audio decoding for format '{}' not yet implemented for OpenAI",
                format_str
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
            Err(e) => Err(GeminiError::DeserializationError(format!(
                "Base64 decode failed: {}",
                e
            ))),
        }
    }
}

#[async_trait]
impl<S: Clone + Send + Sync + 'static> LiveApiBackend<S> for OpenAiBackend {
    fn provider_type(&self) -> ApiProvider {
        ApiProvider::OpenAi
    }

    fn get_websocket_url(&self, _api_key: &str) -> Result<url::Url, GeminiError> {
        // The model parameter will be added to the request URI.
        let url_str = "wss://api.openai.com/v1/realtime";
        url::Url::parse(url_str)
            .map_err(|e| GeminiError::ConfigurationError(format!("Invalid OpenAI URL: {}", e)))
    }

    fn configure_websocket_request(
        &self,
        api_key: &str,
        _base_request_ignored: http::Request<()>, // We'll build from scratch using tungstenite's types
        config: &BackendConfig,
    ) -> Result<http::Request<()>, GeminiError> {
        let model_query = format!("model={}", config.model);
        let uri_str = format!("wss://api.openai.com/v1/realtime?{}", model_query);

        // Use tungstenite's IntoClientRequest to build the request.
        // This ensures that tungstenite itself can properly process it and add necessary handshake headers.
        let mut request = uri_str.into_client_request()?;

        // Add OpenAI specific headers to the tungstenite Request's headers
        let headers = request.headers_mut();
        headers.append(
            header::AUTHORIZATION,
            header::HeaderValue::from_str(&format!("Bearer {}", api_key)).map_err(|e| {
                GeminiError::ConfigurationError(format!(
                    "Invalid API key format for Authorization header: {}",
                    e
                ))
            })?,
        );
        headers.append(
            "OpenAI-Beta",
            header::HeaderValue::from_static("realtime=v1"),
        );
        // The Host header is derived by into_client_request from the URI.
        // Standard WebSocket headers (Upgrade, Connection, Sec-WebSocket-Key, Sec-WebSocket-Version)
        // will be added by the tungstenite library when it performs the handshake.

        trace!(
            "Constructed OpenAI WebSocket request (via tungstenite): Headers: {:?}",
            request.headers()
        );

        // Convert tungstenite::handshake::client::Request to http::Request<()>
        // This part is a bit manual if IntoClientRequest doesn't directly give http::Request
        // but tokio_tungstenite::connect_async takes U: IntoClientRequest + Unpin,
        // so we can pass the tungstenite::handshake::client::Request directly.

        // However, the `connect_async` in `connection.rs` expects `http::Request<()>`.
        // We need to bridge this. The `IntoClientRequest` trait has a method:
        // fn into_client_request(self) -> Result<Request, Error>;
        // where `Request` is `tungstenite::handshake::client::Request`.
        // And `tungstenite::handshake::client::Request` can be converted to `http::Request`.
        //
        // Let's ensure the request object passed to connect_async in connection.rs
        // is what tokio-tungstenite expects to correctly add its own headers.
        // The simplest way is to pass the URL string and let `connect_async` build the http::Request,
        // then use a connector to add custom headers.
        //
        // If we must return http::Request from here for the current connection.rs structure:
        let http_request = http::Request::builder()
            .method(request.method().clone())
            .uri(request.uri().clone())
            .version(http::Version::HTTP_11); // Or request.version() if available and correct type

        let mut final_request = http_request.body(()).map_err(GeminiError::HttpError)?;

        // Copy headers from tungstenite's request to http::Request
        for (name, value) in request.headers() {
            final_request
                .headers_mut()
                .append(name.clone(), value.clone());
        }

        Ok(final_request)
    }

    async fn get_initial_messages(
        /* ... unchanged ... */
        &self,
        config: &BackendConfig,
    ) -> Result<Vec<String>, GeminiError> {
        Ok(vec![
            <Self as LiveApiBackend<S>>::build_session_update_message(self, config)?,
        ])
    }

    fn build_session_update_message(&self, config: &BackendConfig) -> Result<String, GeminiError> {
        let mut session_object_map = serde_json::Map::new();

        // Model (already there from your example JSON structure)
        session_object_map.insert("model".to_string(), Value::String(config.model.clone()));

        // Modalities (already there from your example JSON structure)
        if let Some(gen_config) = &config.generation_config {
            if let Some(modalities) = &gen_config.response_modalities {
                let modalities_str: Vec<String> = modalities
                    .iter()
                    .map(|m| match m {
                        ResponseModality::Text => "text".to_string(),
                        ResponseModality::Audio => "audio".to_string(),
                        ResponseModality::Other(s) => s.clone(),
                    })
                    .collect();
                session_object_map.insert("modalities".to_string(), json!(modalities_str));
            }
            if let Some(temp) = gen_config.temperature {
                session_object_map.insert("temperature".to_string(), json!(temp));
            }
            if let Some(max_tokens) = gen_config.max_output_tokens {
                // Assuming this maps to max_response_output_tokens
                session_object_map
                    .insert("max_response_output_tokens".to_string(), json!(max_tokens));
            }
        }

        if let Some(instr) = &config.system_instruction {
            if let Some(part) = instr.parts.first() {
                if let Some(text) = &part.text {
                    session_object_map
                        .insert("instructions".to_string(), Value::String(text.clone()));
                }
            }
        }
        if !config.tools.is_empty() {
            let openai_tools = config
                .tools
                .iter()
                .map(|t| t.openai_definition.clone())
                .collect::<Vec<_>>();
            session_object_map.insert("tools".to_string(), Value::Array(openai_tools));
            session_object_map.insert("tool_choice".to_string(), Value::String("auto".to_string())); // Or "none" / specific tool
        } else {
            // Explicitly add empty tools array and tool_choice: "none" if no tools
            // to match the example JSON more closely if that's the desired default.
            session_object_map.insert("tools".to_string(), json!([]));
            session_object_map.insert("tool_choice".to_string(), Value::String("none".to_string()));
        }

        if let Some(voice) = &config.voice {
            session_object_map.insert("voice".to_string(), Value::String(voice.clone()));
        }
        if let Some(format) = &config.input_audio_format {
            session_object_map.insert(
                "input_audio_format".to_string(),
                Value::String(format.clone()),
            );
        }
        if let Some(format) = &config.output_audio_format {
            session_object_map.insert(
                "output_audio_format".to_string(),
                Value::String(format.clone()),
            );
        }

        // --- NEW: Input Audio Noise Reduction ---
        if let Some(nr_config) = &config.input_audio_noise_reduction {
            // OpenAI expects `null` to turn it off, or an object like {"type": "near_field"}
            if nr_config.r#type.is_some() {
                session_object_map
                    .insert("input_audio_noise_reduction".to_string(), json!(nr_config));
            } else {
                // If r#type is None in our struct, send null to API to disable it.
                // Or, if you want to omit the field entirely if type is None, you can do that.
                // The example shows `null`, so let's go with that if type is not specified.
                // However, the doc says "This can be set to null to turn off."
                // So if nr_config exists but type is None, it means "configure it, but as off".
                // If nr_config itself is None, the field is omitted (OpenAI uses default).
                session_object_map.insert("input_audio_noise_reduction".to_string(), Value::Null);
            }
        }

        // --- NEW: Audio Transcription Config ---
        if let Some(trans_config) = &config.output_audio_transcription {
            let mut trans_map = serde_json::Map::new();
            if let Some(model) = &trans_config.model {
                trans_map.insert("model".to_string(), json!(model));
            }
            if let Some(lang) = &trans_config.language {
                trans_map.insert("language".to_string(), json!(lang));
            }
            if let Some(prompt) = &trans_config.prompt {
                trans_map.insert("prompt".to_string(), json!(prompt));
            }
            if !trans_map.is_empty() {
                session_object_map.insert(
                    "input_audio_transcription".to_string(),
                    Value::Object(trans_map),
                );
            } else {
                // If the AudioTranscriptionConfig is present but all its fields are None,
                // OpenAI might interpret an empty object {} as "default on" or an error.
                // The example shows it being present if configured. If you want to disable, send null.
                // For simplicity, if all fields are None, we could omit `input_audio_transcription`
                // or send `null` if AudioTranscriptionConfig was explicitly set to default.
                // Let's assume if AudioTranscriptionConfig is Some(), we send the object, even if empty.
                // Or to match example `null`:
                // session_object_map.insert("input_audio_transcription".to_string(), Value::Null);
            }
        }

        // The main `session` object in the payload
        let payload = json!({
            "type": "session.update",
            "session": Value::Object(session_object_map) // Use the map we built
        });
        let json_string = serde_json::to_string(&payload)?;

        info!(
            "[OpenAI Backend] Sending session.update message: {}",
            json_string
        );
        Ok(json_string)
    }

    fn build_text_turn_message(&self, text: String) -> Result<String, GeminiError> {
        serde_json::to_string(&json!({ "type": "conversation.item.create", "item": { "type": "message", "role": "user", "content": [{ "type": "input_text", "text": text }] }})).map_err(GeminiError::from)
    }
    fn build_audio_chunk_message(
        &self,
        audio_samples: &[i16],
        _sample_rate: u32,
        config: &BackendConfig,
    ) -> Result<String, GeminiError> {
        if audio_samples.is_empty() {
            return Err(GeminiError::ConfigurationError(
                "Audio samples cannot be empty".to_string(),
            ));
        }
        let format_str = config.input_audio_format.as_deref().unwrap_or("pcm16");
        if format_str != "pcm16" {
            return Err(GeminiError::UnsupportedOperation(format!(
                "Audio encoding for format '{}' not yet implemented for OpenAI",
                format_str
            )));
        }
        let mut byte_data = Vec::with_capacity(audio_samples.len() * 2);
        for sample in audio_samples {
            byte_data.extend_from_slice(&sample.to_le_bytes());
        }
        let encoded_data = base64::engine::general_purpose::STANDARD.encode(&byte_data);
        serde_json::to_string(
            &json!({ "type": "input_audio_buffer.append", "audio": encoded_data }),
        )
        .map_err(GeminiError::from)
    }
    fn build_audio_stream_end_message(&self) -> Result<String, GeminiError> {
        serde_json::to_string(&json!({ "type": "input_audio_buffer.commit" }))
            .map_err(GeminiError::from)
    }
    fn build_tool_response_message(
        &self,
        responses: Vec<FunctionResponse>,
    ) -> Result<String, GeminiError> {
        if let Some(response) = responses.first() {
            let output_string = match serde_json::to_string(&response.response) {
                Ok(s) => s,
                Err(e) => {
                    warn!(
                        "Failed to serialize tool response value: {}. Sending as error string.",
                        e
                    );
                    json!({ "error": format!("Failed to serialize response: {}", e) }).to_string()
                }
            };
            serde_json::to_string(&json!({ "type": "conversation.item.create", "item": { "type": "function_call_output", "call_id": response.id, "output": output_string }})).map_err(GeminiError::from)
        } else {
            Err(GeminiError::ConfigurationError(
                "No tool responses provided for OpenAI".to_string(),
            ))
        }
    }
    fn build_request_response_message(&self) -> Result<String, GeminiError> {
        serde_json::to_string(&json!({ "type": "response.create", "response": {}}))
            .map_err(GeminiError::from)
    }

    async fn parse_server_message(
        /* ... unchanged, ensure Message::Binary(b_vec) is correct ... */
        &self,
        message: Message,
        handlers: &Arc<Handlers<S>>,
        state: &Arc<S>,
        ws_sink: &Arc<TokioMutex<WsSink>>,
        output_audio_format: &Option<String>,
    ) -> Result<Vec<UnifiedServerEvent>, GeminiError> {
        let text = match message {
            Message::Text(t_str) => t_str.to_string(),
            Message::Binary(b_vec) => String::from_utf8(b_vec.to_vec())
                .map_err(|e| {
                    GeminiError::DeserializationError(format!(
                        "Invalid UTF-8 in binary message: {}",
                        e
                    ))
                })?
                .to_string(),
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
                    .map(String::from);

                trace!(
                    "Received OpenAI event: {:?}, content: {}",
                    event_type,
                    openai_event.to_string()
                );

                match event_type {
                    Some("session.created") | Some("session.updated") => {
                        unified_events.push(UnifiedServerEvent::SetupComplete);
                    }
                    Some("response.created") => {
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
                                    if !samples.is_empty() {
                                        unified_events.push(UnifiedServerEvent::ContentUpdate {
                                            text: None,
                                            audio: Some(samples),
                                            done: false,
                                        });
                                    }
                                }
                                Err(e) => {
                                    unified_events.push(UnifiedServerEvent::Error(ApiError {
                                        code: "audio_decode_error".to_string(),
                                        message: e.to_string(),
                                        event_id,
                                    }))
                                }
                            }
                        }
                    }
                    Some("conversation.item.input_audio_transcription.delta") => {
                        let delta = openai_event
                            .get("delta")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        if !delta.is_empty() {
                            unified_events.push(UnifiedServerEvent::TranscriptionUpdate {
                                text: delta,
                                done: false,
                            });
                        }
                    }
                    Some("conversation.item.input_audio_transcription.completed") => {
                        let transcript = openai_event
                            .get("transcript")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        unified_events.push(UnifiedServerEvent::TranscriptionUpdate {
                            text: transcript,
                            done: true,
                        });
                    }
                    Some("response.done") => {
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
                                        .unwrap_or("{}");
                                    let call_id = output
                                        .get("call_id")
                                        .and_then(|id| id.as_str())
                                        .map(String::from);
                                    match serde_json::from_str::<Value>(args_str) {
                                        Ok(args_value) => {
                                            unified_events.push(UnifiedServerEvent::ToolCall {
                                                id: call_id.clone(),
                                                name: name.clone(),
                                                args: args_value.clone(),
                                            });
                                            if let Some(handler) = handlers.tool_handlers.get(&name)
                                            {
                                                let handler_clone = handler.clone();
                                                let state_clone = state.clone();
                                                match handler_clone
                                                    .call(Some(args_value), state_clone)
                                                    .await
                                                {
                                                    Ok(response_data) => {
                                                        match <Self as LiveApiBackend<S>>::build_tool_response_message(self, vec![FunctionResponse { id: call_id.clone(), name: name.clone(), response: response_data }]) {
                                                            Ok(json_msg) => { let mut sink = ws_sink.lock().await;
                                                                if sink.send(Message::Text(json_msg.into())).await.is_ok() {
                                                                    if let Ok(req_resp_msg) = <Self as LiveApiBackend<S>>::build_request_response_message(self) {
                                                                        let _ = sink.send(Message::Text(req_resp_msg.into())).await; info!("Sent tool response and requested next response for '{}'.", name);
                                                                    } else { error!("Failed to build request_response message after tool call."); }
                                                                } else { error!("Failed to send tool response for '{}'.", name); }
                                                            } Err(e) => error!("Failed to build tool response message for '{}': {}", name, e),
                                                        }
                                                    }
                                                    Err(e) => {
                                                        warn!(
                                                            "Tool handler '{}' failed: {}",
                                                            name, e
                                                        );
                                                        match <Self as LiveApiBackend<S>>::build_tool_response_message(self, vec![FunctionResponse { id: call_id.clone(), name: name.clone(), response: json!({"error": e}) }]) {
                                                            Ok(json_msg) => { let mut sink = ws_sink.lock().await;
                                                                if sink.send(Message::Text(json_msg.into())).await.is_ok() {
                                                                    if let Ok(req_resp_msg) = <Self as LiveApiBackend<S>>::build_request_response_message(self) {
                                                                        let _ = sink.send(Message::Text(req_resp_msg.into())).await; info!("Sent tool error response and requested next response for '{}'.", name);
                                                                    } else { error!("Failed to build request_response message after tool error."); }
                                                                } else { error!("Failed to send tool error response for '{}'.", name); }
                                                            } Err(e) => error!("Failed to build tool error response message for '{}': {}", name, e),
                                                        }
                                                    }
                                                }
                                            } else {
                                                warn!("No handler registered for tool: {}", name);
                                            }
                                        }
                                        Err(e) => {
                                            error!(
                                                "Failed to parse tool arguments for '{}': {}",
                                                name, e
                                            );
                                            unified_events.push(UnifiedServerEvent::Error(
                                                ApiError {
                                                    code: "tool_args_parse_error".to_string(),
                                                    message: format!(
                                                        "Failed to parse args for {}: {}",
                                                        name, e
                                                    ),
                                                    event_id: None,
                                                },
                                            ));
                                        }
                                    }
                                }
                            }
                        }
                        unified_events.push(UnifiedServerEvent::ContentUpdate {
                            text: None,
                            audio: None,
                            done: true,
                        });
                        unified_events.push(UnifiedServerEvent::ModelTurnComplete);
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
                        if let Some(usage_val) =
                            openai_event.get("response").and_then(|r| r.get("usage"))
                        {
                            match serde_json::from_value::<UsageMetadata>(usage_val.clone()) {
                                Ok(mut metadata) => {
                                    metadata.prompt_token_count = usage_val
                                        .get("input_tokens")
                                        .and_then(|v| v.as_i64())
                                        .map(|v| v as i32);
                                    metadata.response_token_count = usage_val
                                        .get("output_tokens")
                                        .and_then(|v| v.as_i64())
                                        .map(|v| v as i32);
                                    metadata.total_token_count = usage_val
                                        .get("total_tokens")
                                        .and_then(|v| v.as_i64())
                                        .map(|v| v as i32);
                                    unified_events.push(UnifiedServerEvent::UsageMetadata(metadata))
                                }
                                Err(e) => warn!("Failed to parse OpenAI usage metadata: {}", e),
                            }
                        }
                    }
                    Some(
                        other_type @ ("input_audio_buffer.speech_started"
                        | "input_audio_buffer.speech_stopped"
                        | "input_audio_buffer.committed"
                        | "input_audio_buffer.cleared"),
                    ) => {
                        debug!("Received audio buffer event: {}", other_type);
                    }
                    Some(other_type) => {
                        debug!("Received unhandled OpenAI event type: {}", other_type);
                        unified_events
                            .push(UnifiedServerEvent::ProviderSpecific(openai_event.clone()));
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

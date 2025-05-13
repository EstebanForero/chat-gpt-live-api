use crate::client::handlers::Handlers;
use crate::error::GeminiError;
use crate::types::*;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio::sync::Mutex as TokioMutex;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, tungstenite::protocol::Message};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiProvider {
    Gemini,
    OpenAi,
}

#[derive(Debug, Clone)]
pub enum UnifiedServerEvent {
    /* ... (unchanged) ... */
    SetupComplete,
    ContentUpdate {
        text: Option<String>,
        audio: Option<Vec<i16>>,
        done: bool,
    },
    TranscriptionUpdate {
        text: String,
        done: bool,
    },
    ToolCall {
        id: Option<String>,
        name: String,
        args: Value,
    },
    ModelTurnComplete,
    ModelGenerationComplete,
    UsageMetadata(UsageMetadata),
    Error(ApiError),
    ProviderSpecific(Value),
    Close,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiError {
    /* ... (unchanged) ... */
    pub code: String,
    pub message: String,
    pub event_id: Option<String>,
}

pub type WsSink =
    futures_util::stream::SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>;

// AppState trait and DowncastArc REMOVED

#[derive(Clone, Debug, Default)]
pub struct BackendConfig {
    /* ... (unchanged) ... */
    pub model: String,
    pub system_instruction: Option<Content>,
    pub generation_config: Option<GenerationConfig>,
    pub tools: Vec<ToolDefinition>,
    pub realtime_input_config: Option<RealtimeInputConfig>,
    pub output_audio_transcription: Option<AudioTranscriptionConfig>,
    pub voice: Option<String>,
    pub input_audio_format: Option<String>,
    pub output_audio_format: Option<String>,
    pub transcription_model: Option<String>,
    pub transcription_language: Option<String>,
    pub transcription_prompt: Option<String>,
    pub include_logprobs: bool,
    pub input_audio_noise_reduction: Option<InputAudioNoiseReduction>,
}

#[derive(Clone, Debug)]
pub struct ToolDefinition {
    /* ... (unchanged) ... */
    pub name: String,
    pub gemini_declaration: FunctionDeclaration,
    pub openai_definition: Value,
}

// Trait is now generic over S
#[async_trait]
pub trait LiveApiBackend<S: Clone + Send + Sync + 'static>: Send + Sync + 'static {
    fn provider_type(&self) -> ApiProvider;
    fn get_websocket_url(&self, api_key: &str) -> Result<url::Url, GeminiError>;
    fn configure_websocket_request(
        &self,
        api_key: &str,
        request: http::Request<()>,
        config: &BackendConfig,
    ) -> Result<http::Request<()>, GeminiError>;
    async fn get_initial_messages(
        &self,
        config: &BackendConfig,
    ) -> Result<Vec<String>, GeminiError>;
    fn build_session_update_message(&self, config: &BackendConfig) -> Result<String, GeminiError>;
    fn build_text_turn_message(&self, text: String) -> Result<String, GeminiError>;
    fn build_audio_chunk_message(
        &self,
        audio_samples: &[i16],
        sample_rate: u32,
        config: &BackendConfig,
    ) -> Result<String, GeminiError>;
    fn build_audio_stream_end_message(&self) -> Result<String, GeminiError>;
    fn build_tool_response_message(
        &self,
        responses: Vec<FunctionResponse>,
    ) -> Result<String, GeminiError>;
    fn build_request_response_message(&self) -> Result<String, GeminiError>;

    // parse_server_message now uses the S from the trait bound
    async fn parse_server_message(
        &self,
        message: Message,
        handlers: &Arc<Handlers<S>>, // S from trait
        state: &Arc<S>,              // S from trait
        ws_sink: &Arc<TokioMutex<WsSink>>,
        output_audio_format: &Option<String>, // Pass by reference
    ) -> Result<Vec<UnifiedServerEvent>, GeminiError>;
}

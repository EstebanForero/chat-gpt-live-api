// src/client/builder.rs
use super::backend::{ApiProvider, BackendConfig, LiveApiBackend, ToolDefinition};
use super::gemini_backend::GeminiBackend;
use super::handle::AiLiveClient; // Use Renamed AiLiveClient
use super::handlers::{
    EventHandlerSimple, Handlers, ServerContentContext, ToolHandler, UsageMetadataContext,
};
use super::openai_backend::OpenAiBackend;
use crate::error::GeminiError;
use crate::types::*;
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};
use tracing::info;

//=========== RENAMED HERE ===========
pub struct AiClientBuilder<S: Clone + Send + Sync + 'static> {
    //====================================
    api_key: String,
    backend_config: BackendConfig,
    handlers: Handlers<S>,
    state: S,
    tool_declarations: Vec<ToolDefinition>,
}

//=========== RENAMED HERE ===========
impl<S: Clone + Send + Sync + 'static + Default> AiClientBuilder<S> {
    //====================================
    pub fn new(api_key: String, model: String) -> Self {
        Self::new_with_model_and_state(api_key, model, S::default())
    }
}

//=========== RENAMED HERE ===========
impl<S: Clone + Send + Sync + 'static> AiClientBuilder<S> {
    //====================================
    pub fn new_with_model_and_state(api_key: String, model: String, state: S) -> Self {
        Self {
            api_key,
            backend_config: BackendConfig {
                model,
                ..Default::default()
            },
            handlers: Handlers::default(),
            state,
            tool_declarations: Vec::new(),
        }
    }

    pub fn input_audio_noise_reduction(mut self, config: InputAudioNoiseReduction) -> Self {
        self.backend_config.input_audio_noise_reduction = Some(config);
        self
    }

    // ... (All .generation_config(), .voice(), .add_tool_internal(), etc. methods unchanged) ...
    pub fn generation_config(mut self, config: GenerationConfig) -> Self {
        self.backend_config.generation_config = Some(config);
        self
    }
    pub fn system_instruction(mut self, instruction: Content) -> Self {
        self.backend_config.system_instruction = Some(instruction);
        self
    }
    pub fn realtime_input_config(mut self, config: RealtimeInputConfig) -> Self {
        self.backend_config.realtime_input_config = Some(config);
        self
    }

    pub fn output_audio_transcription(mut self, config: AudioTranscriptionConfig) -> Self {
        self.backend_config.output_audio_transcription = Some(config);
        self
    }
    pub fn voice(mut self, voice: String) -> Self {
        self.backend_config.voice = Some(voice);
        self
    }
    pub fn input_audio_format(mut self, format: String) -> Self {
        self.backend_config.input_audio_format = Some(format);
        self
    }
    pub fn output_audio_format(mut self, format: String) -> Self {
        self.backend_config.output_audio_format = Some(format);
        self
    }
    pub fn transcription_model(mut self, model: String) -> Self {
        self.backend_config.transcription_model = Some(model);
        self
    }
    pub fn transcription_language(mut self, lang: String) -> Self {
        self.backend_config.transcription_language = Some(lang);
        self
    }
    pub fn transcription_prompt(mut self, prompt: String) -> Self {
        self.backend_config.transcription_prompt = Some(prompt);
        self
    }
    pub fn include_logprobs(mut self, include: bool) -> Self {
        self.backend_config.include_logprobs = include;
        self
    }
    #[doc(hidden)]
    pub fn add_tool_internal(
        mut self,
        name: String,
        gemini_decl: FunctionDeclaration,
        openai_def: Value,
        handler: Arc<dyn ToolHandler<S>>,
    ) -> Self {
        self.tool_declarations.push(ToolDefinition {
            name: name.clone(),
            gemini_declaration: gemini_decl,
            openai_definition: openai_def,
        });
        self.handlers.tool_handlers.insert(name, handler);
        self
    }
    pub fn on_server_content(
        mut self,
        handler: impl EventHandlerSimple<ServerContentContext, S> + 'static,
    ) -> Self {
        self.handlers.on_server_content = Some(Arc::new(handler));
        self
    }
    pub fn on_usage_metadata(
        mut self,
        handler: impl EventHandlerSimple<UsageMetadataContext, S> + 'static,
    ) -> Self {
        self.handlers.on_usage_metadata = Some(Arc::new(handler));
        self
    }
    //-------------------------------------------------------------------------

    // Generic PRIVATE connect method used by public ones
    async fn connect_internal<B: LiveApiBackend<S> + 'static>(
        mut self,
        backend_instance: B,
    ) -> Result<AiLiveClient<S, B>, GeminiError> {
        self.backend_config.tools = self.tool_declarations; // Move tools into final config

        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let (outgoing_sender, outgoing_receiver) = mpsc::channel::<String>(100);

        let state_arc = Arc::new(self.state);
        let handlers_arc = Arc::new(self.handlers);
        let concrete_backend_arc = Arc::new(backend_instance);

        info!(
            "Spawning processing task for provider: {:?}",
            concrete_backend_arc.provider_type()
        );

        super::connection::spawn_processing_task(
            self.api_key.clone(),
            concrete_backend_arc.clone(),
            self.backend_config.clone(),
            handlers_arc,
            state_arc.clone(),
            shutdown_rx,
            outgoing_receiver,
        );

        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        info!("Client connection process initiated.");
        Ok(AiLiveClient {
            shutdown_tx: Some(shutdown_tx),
            outgoing_sender: Some(outgoing_sender),
            state: state_arc,
            backend: concrete_backend_arc,
            backend_config: self.backend_config,
        })
    }

    /// Connect using the Gemini Backend.
    pub async fn connect_gemini(self) -> Result<AiLiveClient<S, GeminiBackend>, GeminiError> {
        self.connect_internal(GeminiBackend).await
    }

    /// Connect using the OpenAI Backend.
    pub async fn connect_openai(self) -> Result<AiLiveClient<S, OpenAiBackend>, GeminiError> {
        self.connect_internal(OpenAiBackend).await
    }
}

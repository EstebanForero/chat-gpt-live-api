use super::backend::{
    ApiProvider,
    AppState,
    BackendConfig,
    LiveApiBackend,
    ToolDefinition,
    WsSink, // Added WsSink (maybe not needed here)
};
use super::gemini_backend::GeminiBackend;
use super::handle::GeminiLiveClient;
use super::handlers::{
    EventHandlerSimple, Handlers, ServerContentContext, ToolHandler, UsageMetadataContext,
};
use super::openai_backend::OpenAiBackend;
use crate::error::GeminiError;
use crate::types::*; // Keep needed types like GenerationConfig etc.
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};
use tracing::{info, warn}; // Added warn

// Builder now takes S: AppState + Clone...
pub struct GeminiLiveClientBuilder<S: AppState + Clone + Send + Sync + 'static> {
    api_key: String,
    provider: ApiProvider,
    backend_config: BackendConfig,
    // Handlers expect Arc<dyn AppState> internally now
    handlers: Handlers<dyn AppState + Send + Sync>,
    state: Arc<S>,
    // Store tool declarations and handlers separately before building BackendConfig
    tool_declarations: Vec<ToolDefinition>,
    // Store handlers expecting Arc<dyn AppState> after wrapping
    raw_tool_handlers: HashMap<String, Arc<dyn ToolHandler<dyn AppState + Send + Sync>>>,
}

impl<S: AppState + Clone + Send + Sync + 'static + Default> GeminiLiveClientBuilder<S> {
    // Constructor requires provider
    pub fn new(api_key: String, model: String, provider: ApiProvider) -> Self {
        Self::new_with_state(api_key, model, provider, S::default())
    }
}

impl<S: AppState + Clone + Send + Sync + 'static> GeminiLiveClientBuilder<S> {
    // Constructor requires provider
    pub fn new_with_state(api_key: String, model: String, provider: ApiProvider, state: S) -> Self {
        Self {
            api_key,
            provider,
            backend_config: BackendConfig {
                model,
                ..Default::default()
            },
            handlers: Handlers::default(),
            state: Arc::new(state),
            tool_declarations: Vec::new(),
            raw_tool_handlers: HashMap::new(),
        }
    }

    // --- Configuration Methods ---
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
    // --- OpenAI Specific Config ---
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
    // ... other config methods ...

    // --- Tool Handling (Internal Storage) ---
    // Called by the macro-generated _register_tool function
    #[doc(hidden)]
    pub fn add_tool_internal(
        mut self,
        name: String,
        gemini_decl: FunctionDeclaration,
        openai_def: Value,
        // Handler now must be wrapped to accept Arc<dyn AppState>
        handler: Arc<dyn ToolHandler<dyn AppState + Send + Sync>>,
    ) -> Self {
        self.tool_declarations.push(ToolDefinition {
            name: name.clone(),
            gemini_declaration: gemini_decl,
            openai_definition: openai_def,
        });
        // Store the already wrapped handler
        self.raw_tool_handlers.insert(name, handler);
        self
    }

    // --- Event Handlers ---
    // These now wrap the user's handler to accept Arc<dyn AppState>
    pub fn on_server_content(
        mut self,
        // User provides handler expecting concrete Arc<S>
        handler: impl EventHandlerSimple<ServerContentContext, S> + 'static,
    ) -> Self {
        let state_clone = self.state.clone(); // Clone Arc<S> for the wrapper
        let wrapped_handler = Arc::new(
            move |ctx: ServerContentContext, _state_dyn: Arc<dyn AppState + Send + Sync>| {
                // We ignore _state_dyn and use the captured concrete state_clone
                // This avoids downcasting issues here.
                handler.call(ctx, state_clone.clone()) // Pass captured Arc<S>
            },
        );
        self.handlers.on_server_content = Some(wrapped_handler);
        self
    }

    pub fn on_usage_metadata(
        mut self,
        // User provides handler expecting concrete Arc<S>
        handler: impl EventHandlerSimple<UsageMetadataContext, S> + 'static,
    ) -> Self {
        let state_clone = self.state.clone(); // Clone Arc<S> for the wrapper
        let wrapped_handler = Arc::new(
            move |ctx: UsageMetadataContext, _state_dyn: Arc<dyn AppState + Send + Sync>| {
                // Ignore _state_dyn, use captured concrete state_clone
                handler.call(ctx, state_clone.clone()) // Pass captured Arc<S>
            },
        );
        self.handlers.on_usage_metadata = Some(wrapped_handler);
        self
    }
    // Add on_transcription, on_error handlers similarly if needed

    // --- Connect ---
    pub async fn connect(mut self) -> Result<GeminiLiveClient<S>, GeminiError> {
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        // Channel sends raw JSON strings now
        let (outgoing_sender, outgoing_receiver) = mpsc::channel::<String>(100);

        // Finalize backend config with tools
        self.backend_config.tools = self.tool_declarations;

        // Create the specific backend instance
        let backend: Arc<dyn LiveApiBackend> = match self.provider {
            ApiProvider::Gemini => Arc::new(GeminiBackend),
            ApiProvider::OpenAi => Arc::new(OpenAiBackend),
        };
        info!("Selected backend: {:?}", self.provider);

        // Populate final Handlers struct with wrapped tool handlers
        self.handlers.tool_handlers = self.raw_tool_handlers; // Move handlers over

        let state_arc_dyn: Arc<dyn AppState + Send + Sync> = self.state.clone(); // Type erase state for connection task

        info!("Spawning processing task...");
        super::connection::spawn_processing_task(
            self.api_key.clone(),
            backend.clone(),             // Pass backend trait object
            self.backend_config.clone(), // Pass final config
            Arc::new(self.handlers),     // Pass handlers expecting Arc<dyn AppState>
            state_arc_dyn,               // Pass type-erased state
            shutdown_rx,
            outgoing_receiver,
        );

        // Allow a bit more time for potential session setup messages
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        info!("Client connection process initiated.");
        Ok(GeminiLiveClient {
            shutdown_tx: Some(shutdown_tx),
            outgoing_sender: Some(outgoing_sender),
            state: self.state, // Keep original Arc<S> in the client handle
            backend: backend,  // Store backend instance in client handle
            output_audio_format: self.backend_config.output_audio_format, // Store for decoding
        })
    }
}

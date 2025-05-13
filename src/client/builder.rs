use super::handle::GeminiLiveClient;
use super::handlers::{
    EventHandlerSimple, Handlers, ServerContentContext, ToolHandler, UsageMetadataContext,
};
use crate::error::GeminiError;
use crate::types::*;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};

pub struct GeminiLiveClientBuilder<S: Clone + Send + Sync + 'static> {
    pub(crate) api_key: String,
    pub(crate) initial_setup: BidiGenerateContentSetup,
    pub(crate) handlers: Handlers<S>,
    pub(crate) state: S,
}

impl<S: Clone + Send + Sync + 'static + Default> GeminiLiveClientBuilder<S> {
    pub fn new(api_key: String, model: String) -> Self {
        Self::new_with_state(api_key, model, S::default())
    }
}

impl<S: Clone + Send + Sync + 'static> GeminiLiveClientBuilder<S> {
    pub fn new_with_state(api_key: String, model: String, state: S) -> Self {
        Self {
            api_key,
            initial_setup: BidiGenerateContentSetup {
                model,
                ..Default::default()
            },
            handlers: Handlers::default(),
            state,
        }
    }

    pub fn generation_config(mut self, config: GenerationConfig) -> Self {
        self.initial_setup.generation_config = Some(config);
        self
    }

    pub fn system_instruction(mut self, instruction: Content) -> Self {
        self.initial_setup.system_instruction = Some(instruction);
        self
    }

    #[doc(hidden)]
    pub fn add_tool_declaration(mut self, declaration: FunctionDeclaration) -> Self {
        let tools_vec = self.initial_setup.tools.get_or_insert_with(Vec::new);
        if let Some(tool_struct) = tools_vec.first_mut() {
            tool_struct.function_declarations.push(declaration);
        } else {
            tools_vec.push(Tool {
                function_declarations: vec![declaration],
            });
        }
        self
    }

    #[doc(hidden)]
    pub fn on_tool_call<F>(mut self, tool_name: impl Into<String>, handler: F) -> Self
    where
        F: ToolHandler<S> + 'static,
    {
        self.handlers
            .tool_handlers
            .insert(tool_name.into(), Arc::new(handler));
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

    pub fn realtime_input_config(mut self, config: RealtimeInputConfig) -> Self {
        self.initial_setup.realtime_input_config = Some(config);
        self
    }

    pub fn output_audio_transcription(mut self, config: AudioTranscriptionConfig) -> Self {
        self.initial_setup.output_audio_transcription = Some(config);
        self
    }

    pub async fn connect(self) -> Result<GeminiLiveClient<S>, GeminiError> {
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let (outgoing_sender, outgoing_receiver) = mpsc::channel(100);

        let state_arc = Arc::new(self.state);
        let handlers_arc = Arc::new(self.handlers);

        super::connection::spawn_processing_task(
            self.api_key.clone(),
            self.initial_setup,
            handlers_arc,
            state_arc.clone(),
            shutdown_rx,
            outgoing_receiver,
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        Ok(GeminiLiveClient {
            shutdown_tx: Some(shutdown_tx),
            outgoing_sender: Some(outgoing_sender),
            state: state_arc,
        })
    }
}

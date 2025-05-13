// src/client/handle.rs
use super::backend::{ApiProvider, BackendConfig, LiveApiBackend};
use crate::error::GeminiError;
use crate::types::*;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, error, info, trace, warn};

//=========== RENAMED HERE ===========
pub struct AiLiveClient<S: Clone + Send + Sync + 'static, B: LiveApiBackend<S>> {
    //====================================
    pub(crate) shutdown_tx: Option<oneshot::Sender<()>>,
    pub(crate) outgoing_sender: Option<mpsc::Sender<String>>,
    pub(crate) state: Arc<S>,
    pub(crate) backend: Arc<B>,
    pub(crate) backend_config: BackendConfig,
}

//=========== RENAMED HERE ===========
impl<S: Clone + Send + Sync + 'static, B: LiveApiBackend<S>> AiLiveClient<S, B> {
    //====================================

    pub async fn close(&mut self) -> Result<(), GeminiError> {
        // ... (unchanged)
        info!("Client close requested.");
        if let Some(tx) = self.shutdown_tx.take() {
            if tx.send(()).is_err() {
                debug!("Shutdown signal send failed: Listener task likely already stopped.");
            } else {
                info!("Shutdown signal sent to listener task.");
            }
        }
        self.outgoing_sender.take();
        info!("Client resources released.");
        Ok(())
    }

    async fn send_raw_message(&self, json_message: String) -> Result<(), GeminiError> {
        // ... (unchanged)
        if let Some(sender) = &self.outgoing_sender {
            trace!("Queueing message for sending: {}", json_message);
            sender.send(json_message).await.map_err(|e| {
                error!("Failed to send message to listener task: {}", e);
                GeminiError::SendError
            })
        } else {
            error!("Cannot send message: Client is closed or sender missing.");
            Err(GeminiError::NotReady)
        }
    }

    pub async fn send_text_turn(&self, text: String) -> Result<(), GeminiError> {
        // ... (unchanged)
        info!("Sending text turn: '{}'", text);
        let message = self.backend.build_text_turn_message(text)?;
        self.send_raw_message(message).await?;
        if self.backend.provider_type() == ApiProvider::OpenAi {
            debug!("Requesting response from OpenAI after text turn.");
            self.request_response().await?;
        }
        Ok(())
    }

    pub async fn send_audio_chunk(
        &self,
        audio_samples: &[i16],
        sample_rate: u32,
    ) -> Result<(), GeminiError> {
        // ... (unchanged)
        if audio_samples.is_empty() {
            warn!("Attempted to send empty audio chunk.");
            return Ok(());
        }
        trace!(
            "Sending audio chunk: {} samples, {} Hz",
            audio_samples.len(),
            sample_rate
        );
        let message = self.backend.build_audio_chunk_message(
            audio_samples,
            sample_rate,
            &self.backend_config,
        )?;
        self.send_raw_message(message).await
    }

    pub async fn send_audio_stream_end(&self) -> Result<(), GeminiError> {
        // ... (unchanged)
        info!("Sending audio stream end signal.");
        let message = self.backend.build_audio_stream_end_message()?;
        self.send_raw_message(message).await?;
        if self.backend.provider_type() == ApiProvider::OpenAi {
            debug!("Requesting response from OpenAI after audio stream end.");
            self.request_response().await?;
        }
        Ok(())
    }

    pub async fn send_tool_responses(
        &self,
        responses: Vec<FunctionResponse>,
    ) -> Result<(), GeminiError> {
        // ... (unchanged)
        info!("Sending {} tool response(s).", responses.len());
        let message = self.backend.build_tool_response_message(responses)?;
        self.send_raw_message(message).await?;
        if self.backend.provider_type() == ApiProvider::OpenAi {
            debug!("Requesting response from OpenAI after tool response.");
            self.request_response().await?;
        }
        Ok(())
    }

    pub async fn request_response(&self) -> Result<(), GeminiError> {
        // ... (unchanged)
        if self.backend.provider_type() == ApiProvider::OpenAi {
            debug!("Sending explicit response request.");
            let message = self.backend.build_request_response_message()?;
            self.send_raw_message(message).await
        } else {
            Ok(())
        }
    }

    pub fn state(&self) -> Arc<S> {
        // ... (unchanged)
        self.state.clone()
    }

    pub fn provider(&self) -> ApiProvider {
        // ... (unchanged)
        self.backend.provider_type()
    }

    pub fn is_closed(&self) -> bool {
        self.shutdown_tx.is_none()
    }
}

//=========== RENAMED HERE ===========
impl<S: Clone + Send + Sync + 'static, B: LiveApiBackend<S>> Drop for AiLiveClient<S, B> {
    //====================================
    fn drop(&mut self) {
        if self.shutdown_tx.is_some() {
            warn!("AiLiveClient dropped without explicit close(). Attempting shutdown."); // Updated name
            if let Some(tx) = self.shutdown_tx.take() {
                let _ = tx.send(());
            }
            self.outgoing_sender.take();
        }
    }
}

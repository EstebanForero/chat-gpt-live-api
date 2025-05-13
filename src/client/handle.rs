use super::backend::{ApiProvider, AppState, LiveApiBackend}; // Use AppState trait
use crate::error::GeminiError;
use crate::types::*;
use base64::Engine as _; // Keep for potential direct use if needed, though backend handles it mostly
use serde_json::Value; // Added
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, error, info, trace, warn}; // Added debug

// Client handle remains generic over S: AppState
pub struct GeminiLiveClient<S: AppState + Clone + Send + Sync + 'static> {
    pub(crate) shutdown_tx: Option<oneshot::Sender<()>>,
    // Channel sends raw JSON strings
    pub(crate) outgoing_sender: Option<mpsc::Sender<String>>,
    pub(crate) state: Arc<S>,
    pub(crate) backend: Arc<dyn LiveApiBackend>,
    // Store output format for potential use (though parsing happens in backend)
    pub(crate) output_audio_format: Option<String>,
}

impl<S: AppState + Clone + Send + Sync + 'static> GeminiLiveClient<S> {
    pub async fn close(&mut self) -> Result<(), GeminiError> {
        info!("Client close requested.");
        if let Some(tx) = self.shutdown_tx.take() {
            if tx.send(()).is_err() {
                // Listener task might have already finished, which is fine.
                debug!("Shutdown signal send failed: Listener task likely already stopped.");
            } else {
                info!("Shutdown signal sent to listener task.");
            }
        }
        // Drop the sender to signal the listener task if it's waiting on recv()
        self.outgoing_sender.take();
        info!("Client resources released.");
        Ok(())
    }

    // Renamed for clarity - this sends raw JSON
    async fn send_raw_message(&self, json_message: String) -> Result<(), GeminiError> {
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

    // --- User-facing Send Methods ---
    pub async fn send_text_turn(&self, text: String) -> Result<(), GeminiError> {
        info!("Sending text turn: '{}'", text);
        let message = self.backend.build_text_turn_message(text)?;
        self.send_raw_message(message).await?;
        // Only OpenAI needs an explicit response request after sending user text/tool results
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
        // channels: u16, // Channels might be implicit in format/config now
        config_override: Option<&BackendConfig>, // Allow overriding format per call if needed
    ) -> Result<(), GeminiError> {
        if audio_samples.is_empty() {
            warn!("Attempted to send empty audio chunk.");
            return Ok(());
        }
        trace!(
            "Sending audio chunk: {} samples, {} Hz",
            audio_samples.len(),
            sample_rate
        );
        // Need the config for audio format - how to get it?
        // Store BackendConfig in client handle? Or pass it here? Let's assume stored temporarily.
        // Hacky: Create temporary config. A better way is needed.
        let temp_config = BackendConfig {
            input_audio_format: config_override.and_then(|c| c.input_audio_format.clone()),
            ..Default::default() // Other fields don't matter for this build call
        };
        let message =
            self.backend
                .build_audio_chunk_message(audio_samples, sample_rate, &temp_config)?;
        self.send_raw_message(message).await
    }

    pub async fn send_audio_stream_end(&self) -> Result<(), GeminiError> {
        info!("Sending audio stream end signal.");
        let message = self.backend.build_audio_stream_end_message()?;
        self.send_raw_message(message).await?;
        // For OpenAI, committing the buffer requires requesting a response
        if self.backend.provider_type() == ApiProvider.OpenAi {
            debug!("Requesting response from OpenAI after audio stream end.");
            self.request_response().await?;
        }
        Ok(())
    }

    // Send tool responses (internal detail, usually called by backend)
    // Made public in case direct sending is needed, but normally backend handles this.
    pub async fn send_tool_responses(
        &self,
        responses: Vec<FunctionResponse>,
    ) -> Result<(), GeminiError> {
        info!("Sending {} tool response(s).", responses.len());
        let message = self.backend.build_tool_response_message(responses)?;
        self.send_raw_message(message).await?;
        // OpenAI needs an explicit response request after sending tool results
        if self.backend.provider_type() == ApiProvider.OpenAi {
            debug!("Requesting response from OpenAI after tool response.");
            self.request_response().await?;
        }
        Ok(())
    }

    // Method specifically for OpenAI to trigger a response generation
    pub async fn request_response(&self) -> Result<(), GeminiError> {
        if self.backend.provider_type() == ApiProvider.OpenAi {
            debug!("Sending explicit response request.");
            let message = self.backend.build_request_response_message()?;
            self.send_raw_message(message).await
        } else {
            // No-op for Gemini, maybe warn?
            // warn!("request_response() called for non-OpenAI provider; no action taken.");
            Ok(())
        }
    }

    // send_realtime_text, send_activity_start/end need backend implementation if desired

    pub fn state(&self) -> Arc<S> {
        self.state.clone()
    }

    // Expose provider type
    pub fn provider(&self) -> ApiProvider {
        self.backend.provider_type()
    }
}

impl<S: AppState + Clone + Send + Sync + 'static> Drop for GeminiLiveClient<S> {
    fn drop(&mut self) {
        if self.shutdown_tx.is_some() {
            warn!("GeminiLiveClient dropped without explicit close(). Attempting shutdown.");
            // Best effort shutdown signal
            if let Some(tx) = self.shutdown_tx.take() {
                let _ = tx.send(()); // Ignore error as the task might be gone
            }
            // Ensure the sender is dropped to close the channel
            self.outgoing_sender.take();
        }
    }
}

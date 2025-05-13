use crate::error::GeminiError;
use crate::types::*;
use base64::Engine as _;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};
use tracing::{error, info, trace, warn};

use super::GeminiLiveClientBuilder;

pub struct GeminiLiveClient<S: Clone + Send + Sync + 'static> {
    pub(crate) shutdown_tx: Option<oneshot::Sender<()>>,
    pub(crate) outgoing_sender: Option<mpsc::Sender<ClientMessagePayload>>,
    pub(crate) state: Arc<S>,
}

impl<S: Clone + Send + Sync + 'static> GeminiLiveClient<S> {
    pub async fn close(&mut self) -> Result<(), GeminiError> {
        info!("Client close requested.");
        if let Some(tx) = self.shutdown_tx.take() {
            if tx.send(()).is_err() {
                info!("Shutdown signal failed: Listener task already gone.");
            } else {
                info!("Shutdown signal sent to listener task.");
            }
        }
        self.outgoing_sender.take();
        Ok(())
    }

    pub fn get_outgoing_mpsc_sender_clone(
        &self,
    ) -> Option<tokio::sync::mpsc::Sender<ClientMessagePayload>> {
        self.outgoing_sender.clone()
    }

    pub fn builder_with_state(
        api_key: String,
        model: String,
        state: S,
    ) -> GeminiLiveClientBuilder<S> {
        GeminiLiveClientBuilder::new_with_state(api_key, model, state)
    }

    async fn send_message(&self, payload: ClientMessagePayload) -> Result<(), GeminiError> {
        if let Some(sender) = &self.outgoing_sender {
            let sender = sender.clone();
            match sender.send(payload).await {
                Ok(_) => {
                    trace!("Message sent to listener task via channel.");
                    Ok(())
                }
                Err(_) => {
                    error!("Failed to send message to listener task: Channel closed.");
                    Err(GeminiError::SendError)
                }
            }
        } else {
            error!("Cannot send message: Client is closed or sender missing.");
            Err(GeminiError::NotReady)
        }
    }

    pub async fn send_text_turn(&self, text: String, end_of_turn: bool) -> Result<(), GeminiError> {
        let content_part = Part {
            text: Some(text),
            ..Default::default()
        };
        let content = Content {
            parts: vec![content_part],
            role: Some(Role::User),
        };
        let client_content_msg = BidiGenerateContentClientContent {
            turns: Some(vec![content]),
            turn_complete: Some(end_of_turn),
        };
        self.send_message(ClientMessagePayload::ClientContent(client_content_msg))
            .await
    }

    pub async fn send_audio_chunk(
        &self,
        audio_samples: &[i16],
        sample_rate: u32,
        channels: u16,
    ) -> Result<(), GeminiError> {
        if audio_samples.is_empty() {
            return Ok(());
        }
        let mut byte_data = Vec::with_capacity(audio_samples.len() * 2);
        for sample in audio_samples {
            byte_data.extend_from_slice(&sample.to_le_bytes());
        }

        let encoded_data = base64::engine::general_purpose::STANDARD.encode(&byte_data);
        let mime_type = format!("audio/pcm;rate={}", sample_rate);

        let audio_blob = Blob {
            mime_type,
            data: encoded_data,
        };

        let realtime_input = BidiGenerateContentRealtimeInput {
            audio: Some(audio_blob),
            ..Default::default()
        };

        self.send_message(ClientMessagePayload::RealtimeInput(realtime_input))
            .await
    }

    pub async fn send_realtime_text(&self, text: String) -> Result<(), GeminiError> {
        let realtime_input = BidiGenerateContentRealtimeInput {
            text: Some(text),
            ..Default::default()
        };
        self.send_message(ClientMessagePayload::RealtimeInput(realtime_input))
            .await
    }

    pub async fn send_activity_start(&self) -> Result<(), GeminiError> {
        let realtime_input = BidiGenerateContentRealtimeInput {
            activity_start: Some(ActivityStart {}),
            ..Default::default()
        };
        self.send_message(ClientMessagePayload::RealtimeInput(realtime_input))
            .await
    }

    pub async fn send_activity_end(&self) -> Result<(), GeminiError> {
        let realtime_input = BidiGenerateContentRealtimeInput {
            activity_end: Some(ActivityEnd {}),
            ..Default::default()
        };
        self.send_message(ClientMessagePayload::RealtimeInput(realtime_input))
            .await
    }

    pub async fn send_audio_stream_end(&self) -> Result<(), GeminiError> {
        let realtime_input = BidiGenerateContentRealtimeInput {
            audio_stream_end: Some(true),
            ..Default::default()
        };
        self.send_message(ClientMessagePayload::RealtimeInput(realtime_input))
            .await
    }

    pub fn state(&self) -> Arc<S> {
        self.state.clone()
    }
}

impl<S: Clone + Send + Sync + 'static> Drop for GeminiLiveClient<S> {
    fn drop(&mut self) {
        if self.shutdown_tx.is_some() {
            warn!("GeminiLiveClient dropped without explicit close(). Attempting shutdown.");
            if let Some(tx) = self.shutdown_tx.take() {
                let _ = tx.send(());
            }
            self.outgoing_sender.take();
        }
    }
}

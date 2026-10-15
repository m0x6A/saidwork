use async_trait::async_trait;

use crate::domain::{AudioInput, Transcript, TranscriptionError, TranscriptionRequest};

/// Turns audio into text.
#[async_trait]
pub trait Transcriber: Send + Sync {
    async fn transcribe(
        &self,
        audio: &AudioInput,
        request: &TranscriptionRequest,
    ) -> Result<Transcript, TranscriptionError>;
}

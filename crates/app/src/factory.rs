use std::sync::Arc;

use saidwork_providers::{Provider, ProviderKind};
use saidwork_rewriting::{OllamaRewriter, OpenAiChatRewriter, Rewriter};
use saidwork_transcription::{OpenAiTranscriber, Transcriber};

/// Build the transcription adapter matching the provider's protocol.
#[must_use]
pub fn build_transcriber(provider: &Provider) -> Arc<dyn Transcriber> {
    match provider.kind {
        // Ollama has no transcription endpoint; its OpenAI-compatible surface is the best effort.
        ProviderKind::OpenAiCompatible | ProviderKind::Ollama => Arc::new(OpenAiTranscriber::new(
            &provider.base_url,
            provider.api_key.clone(),
            provider.model.clone(),
        )),
    }
}

/// Build the rewriting adapter matching the provider's protocol.
#[must_use]
pub fn build_rewriter(provider: &Provider) -> Arc<dyn Rewriter> {
    match provider.kind {
        ProviderKind::OpenAiCompatible => Arc::new(OpenAiChatRewriter::new(
            &provider.base_url,
            provider.api_key.clone(),
            provider.model.clone(),
        )),
        ProviderKind::Ollama => Arc::new(OllamaRewriter::new(
            &provider.base_url,
            provider.api_key.clone(),
            provider.model.clone(),
        )),
    }
}

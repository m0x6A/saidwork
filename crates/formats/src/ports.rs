use async_trait::async_trait;

use crate::domain::{Format, FormatError, FormatId};

/// Where formats are stored.
pub trait FormatRepository: Send + Sync {
    /// All available formats, sorted by id.
    fn list(&self) -> Result<Vec<Format>, FormatError>;

    /// One format by id.
    fn get(&self, id: &FormatId) -> Result<Format, FormatError> {
        self.list()?
            .into_iter()
            .find(|f| &f.id == id)
            .ok_or_else(|| FormatError::NotFound { id: id.to_string() })
    }
}

/// Errors while choosing a format.
#[derive(Debug, thiserror::Error)]
pub enum SelectionError {
    #[error("no formats are available to choose from")]
    NoCandidates,
    #[error("the pre-selected format `{id}` is not among the candidates")]
    NotACandidate { id: String },
    #[error("format selection failed: {0}")]
    Other(String),
}

/// Decides which format a transcript should be rewritten into.
///
/// Today the user decides ([`crate::ManualSelector`]); the `when_to_use` field on each
/// [`Format`] exists so that a future LLM-backed selector can decide from the transcript.
#[async_trait]
pub trait FormatSelector: Send + Sync {
    /// Pick one of `candidates` for `transcript`.
    async fn select(
        &self,
        candidates: &[Format],
        transcript: &str,
    ) -> Result<FormatId, SelectionError>;
}

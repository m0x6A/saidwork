use async_trait::async_trait;

use crate::domain::{RewriteError, RewriteRequest, RewrittenNote};

/// Rewrites a transcript into Markdown following a format.
#[async_trait]
pub trait Rewriter: Send + Sync {
    async fn rewrite(&self, request: &RewriteRequest) -> Result<RewrittenNote, RewriteError>;
}

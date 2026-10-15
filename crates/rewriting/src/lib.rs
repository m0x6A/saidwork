//! Rewriting transcripts into a Markdown format: the [`Rewriter`] port, the
//! prompt construction shared by all adapters, and the HTTP adapters.

mod adapters;
mod domain;
mod ports;
mod prompt;

pub use adapters::{OllamaRewriter, OpenAiChatRewriter};
pub use domain::{FormatSpec, RewriteError, RewriteRequest, RewrittenNote};
pub use ports::Rewriter;
pub use prompt::{Prompt, build_prompt, post_process};

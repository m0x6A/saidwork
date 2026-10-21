/// The target format as the rewriter sees it: a name and a Markdown template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormatSpec {
    pub name: String,
    pub template: String,
}

/// Everything the rewriter needs for one note.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RewriteRequest {
    pub transcript: String,
    pub format: FormatSpec,
    /// Human readable date substituted for `{{date}}` in templates.
    pub date: String,
    /// Optional extra guidance from the user (e.g. "this was the weekly sync").
    pub context: Option<String>,
}

/// The rewritten Markdown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RewrittenNote {
    pub markdown: String,
    /// First level-one heading, if any.
    pub title: Option<String>,
    /// Problems worth telling the user about even though a note came back.
    pub warnings: Vec<String>,
}

/// Errors of the rewriting slice.
#[derive(Debug, thiserror::Error)]
pub enum RewriteError {
    #[error("rewrite request failed: {0}")]
    Transport(#[source] reqwest::Error),
    #[error("rewrite server rejected the credentials (HTTP {status})")]
    Unauthorized { status: u16 },
    #[error("rewrite server answered HTTP {status}: {body}")]
    Status { status: u16, body: String },
    #[error("could not decode rewrite response: {0}")]
    Decode(String),
    #[error("the model returned an empty answer")]
    Empty,
}

impl RewriteError {
    #[must_use]
    pub fn is_transient(&self) -> bool {
        match self {
            Self::Transport(e) => e.is_timeout() || e.is_connect() || e.is_request(),
            Self::Status { status, .. } => *status == 429 || *status >= 500,
            Self::Unauthorized { .. } | Self::Decode(_) | Self::Empty => false,
        }
    }
}

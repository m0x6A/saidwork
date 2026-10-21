use std::path::PathBuf;

use saidwork_providers::{ProviderName, Readiness};

/// Progress notifications emitted while a session is processed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionEvent {
    /// Checking (and possibly launching) a provider.
    ProviderStarting(ProviderName),
    ProviderReady(ProviderName, Readiness),
    /// Raw audio persisted for debugging / re-processing.
    RecordingSaved(PathBuf),
    Transcribing,
    Transcribed {
        characters: usize,
        path: PathBuf,
    },
    Rewriting {
        format: String,
    },
    NoteSaved(PathBuf),
    /// Something went wrong without failing the session, e.g. a truncated prompt.
    Warning(String),
}

impl std::fmt::Display for SessionEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ProviderStarting(name) => write!(f, "checking provider {name}"),
            Self::ProviderReady(name, Readiness::AlreadyUp) => write!(f, "provider {name} is up"),
            Self::ProviderReady(name, Readiness::Started) => write!(f, "provider {name} started"),
            Self::RecordingSaved(path) => write!(f, "recording saved to {}", path.display()),
            Self::Transcribing => f.write_str("transcribing…"),
            Self::Transcribed { characters, path } => write!(
                f,
                "transcript ({characters} characters) saved to {}",
                path.display()
            ),
            Self::Rewriting { format } => write!(f, "rewriting as “{format}”…"),
            Self::NoteSaved(path) => write!(f, "note saved to {}", path.display()),
            Self::Warning(message) => write!(f, "warning: {message}"),
        }
    }
}

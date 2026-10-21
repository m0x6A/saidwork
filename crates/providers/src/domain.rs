use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use saidwork_kernel::SecretString;
use saidwork_kernel::urls::join_path;
use url::Url;

/// Name of a provider as used in configuration, e.g. `whisper-local`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ProviderName(String);

impl ProviderName {
    pub fn new(raw: impl AsRef<str>) -> Result<Self, ProviderError> {
        let name = raw.as_ref().trim();
        if name.is_empty() {
            return Err(ProviderError::EmptyName);
        }
        Ok(Self(name.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ProviderName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Which wire protocol a provider speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderKind {
    /// `/v1/audio/transcriptions` and `/v1/chat/completions` as defined by OpenAI;
    /// also spoken by whisper.cpp, llama.cpp, Groq, OpenRouter and many others.
    OpenAiCompatible,
    /// Ollama's native `/api/chat`.
    Ollama,
}

/// How to start a local provider when it is not running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchSpec {
    /// Executable to run, resolved via `PATH`.
    pub command: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    /// Path probed on the provider's base URL to decide readiness; `None` probes the root.
    pub health_path: Option<String>,
    pub startup_timeout: Duration,
    /// Terminate the process when the application exits (only if we started it).
    pub stop_on_exit: bool,
    /// Where the child's stdout/stderr go; `None` discards them.
    pub log_file: Option<PathBuf>,
    /// Files that must exist before launching, e.g. a server script passed in `args`.
    pub required_files: Vec<PathBuf>,
}

impl LaunchSpec {
    /// A spec that just runs `command` with defaults (60 s timeout, stop on exit).
    #[must_use]
    pub fn new(command: impl Into<String>) -> Self {
        Self {
            command: command.into(),
            args: Vec::new(),
            env: BTreeMap::new(),
            health_path: None,
            startup_timeout: Duration::from_secs(60),
            stop_on_exit: true,
            log_file: None,
            required_files: Vec::new(),
        }
    }
}

/// Something a launch spec needs on this machine that is not there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MissingRequirement {
    /// The executable is neither a path to a file nor found on `PATH`.
    Command(String),
    /// A file listed in [`LaunchSpec::required_files`] does not exist.
    File(PathBuf),
}

impl fmt::Display for MissingRequirement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Command(c) if c.contains('/') => write!(f, "`{c}` does not exist"),
            Self::Command(c) => write!(f, "`{c}` is not installed (not found on PATH)"),
            Self::File(p) => write!(f, "`{}` does not exist", p.display()),
        }
    }
}

/// An LLM endpoint the application can talk to.
#[derive(Clone)]
pub struct Provider {
    pub name: ProviderName,
    pub kind: ProviderKind,
    /// Server root, without `/v1`; adapters append their own paths.
    pub base_url: Url,
    pub api_key: Option<SecretString>,
    pub model: String,
    /// Requested context window in tokens; only the Ollama rewriter uses it.
    pub context_window: Option<u32>,
    pub launch: Option<LaunchSpec>,
}

impl Provider {
    /// URL used to decide whether the provider is up.
    #[must_use]
    pub fn health_url(&self) -> Url {
        match self.launch.as_ref().and_then(|l| l.health_path.as_deref()) {
            Some(path) => join_path(&self.base_url, path),
            None => self.base_url.clone(),
        }
    }
}

impl fmt::Debug for Provider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Provider")
            .field("name", &self.name)
            .field("kind", &self.kind)
            .field("base_url", &self.base_url.as_str())
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("model", &self.model)
            .field("context_window", &self.context_window)
            .field("launch", &self.launch)
            .finish()
    }
}

/// Errors of the providers slice.
#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("provider name must not be empty")]
    EmptyName,
    #[error("unknown provider `{0}`")]
    Unknown(ProviderName),
    #[error("provider `{name}`: environment variable `{var}` for the API key is not set")]
    MissingApiKeyEnv { name: ProviderName, var: String },
    #[error("provider `{name}`: invalid base_url: {source}")]
    InvalidUrl {
        name: ProviderName,
        #[source]
        source: url::ParseError,
    },
    #[error("provider `{0}` is not running and has no launch command configured")]
    NotRunning(ProviderName),
    #[error(
        "provider `{name}` did not become healthy within {timeout:?} after launching `{command}`"
    )]
    StartupTimeout {
        name: ProviderName,
        command: String,
        timeout: Duration,
    },
    #[error("provider `{name}`: launched process `{command}` exited early")]
    ExitedEarly { name: ProviderName, command: String },
    #[error("could not launch `{command}`: {source}")]
    Spawn {
        command: String,
        #[source]
        source: std::io::Error,
    },
    #[error("could not stop process: {0}")]
    Terminate(String),
    #[error("provider `{name}` cannot be started: {missing}")]
    Missing {
        name: ProviderName,
        missing: MissingRequirement,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider(launch: Option<LaunchSpec>) -> Provider {
        Provider {
            name: ProviderName::new("p").unwrap(),
            kind: ProviderKind::Ollama,
            base_url: Url::parse("http://127.0.0.1:11434").unwrap(),
            api_key: Some(SecretString::from("hunter2")),
            model: "m".into(),
            context_window: None,
            launch,
        }
    }

    #[test]
    fn health_url_defaults_to_base() {
        assert_eq!(
            provider(None).health_url().as_str(),
            "http://127.0.0.1:11434/"
        );
    }

    #[test]
    fn health_url_uses_launch_path() {
        let mut spec = LaunchSpec::new("x");
        spec.health_path = Some("/health".into());
        assert_eq!(
            provider(Some(spec)).health_url().as_str(),
            "http://127.0.0.1:11434/health"
        );
    }

    #[test]
    fn debug_never_prints_the_key() {
        let dbg = format!("{:?}", provider(None));
        assert!(!dbg.contains("hunter2"));
        assert!(dbg.contains("<redacted>"));
    }
}

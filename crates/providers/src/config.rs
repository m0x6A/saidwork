use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use saidwork_kernel::SecretString;
use serde::Deserialize;
use url::Url;

use crate::domain::{LaunchSpec, Provider, ProviderError, ProviderKind, ProviderName};

/// Serialisable shape of one `[providers.<name>]` table.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProviderConfig {
    pub kind: ProviderKindConfig,
    pub base_url: String,
    pub model: String,
    /// Context window in tokens to request from the server (Ollama's `num_ctx`). Unset keeps
    /// the server's default, which is often too small for a long transcript.
    #[serde(default)]
    pub context_window: Option<u32>,
    /// Name of an environment variable holding the API key (preferred).
    #[serde(default)]
    pub api_key_env: Option<String>,
    /// Inline API key; discouraged, but handy for throw-away local setups.
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub launch: Option<LaunchConfig>,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderKindConfig {
    OpenaiCompatible,
    Ollama,
}

/// Serialisable shape of `[providers.<name>.launch]`.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LaunchConfig {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub health_path: Option<String>,
    #[serde(default = "default_startup_timeout_secs")]
    pub startup_timeout_secs: u64,
    #[serde(default = "default_true")]
    pub stop_on_exit: bool,
    #[serde(default)]
    pub log_file: Option<PathBuf>,
    /// Files the server needs, such as a model; checked before launching.
    #[serde(default)]
    pub requires: Vec<PathBuf>,
}

fn default_startup_timeout_secs() -> u64 {
    60
}

fn default_true() -> bool {
    true
}

impl ProviderConfig {
    /// Turn configuration into a [`Provider`], resolving the API key through `env`.
    ///
    /// `env` is injected (rather than reading `std::env` directly) so tests stay hermetic.
    pub fn resolve(
        &self,
        name: &str,
        env: impl Fn(&str) -> Option<String>,
    ) -> Result<Provider, ProviderError> {
        let name = ProviderName::new(name)?;
        let base_url =
            Url::parse(self.base_url.trim()).map_err(|source| ProviderError::InvalidUrl {
                name: name.clone(),
                source,
            })?;
        let api_key = match (&self.api_key_env, &self.api_key) {
            (Some(var), _) => Some(env(var).ok_or_else(|| ProviderError::MissingApiKeyEnv {
                name: name.clone(),
                var: var.clone(),
            })?),
            (None, Some(inline)) => Some(inline.clone()),
            (None, None) => None,
        }
        .filter(|k| !k.trim().is_empty())
        .map(SecretString::from);
        Ok(Provider {
            name,
            kind: match self.kind {
                ProviderKindConfig::OpenaiCompatible => ProviderKind::OpenAiCompatible,
                ProviderKindConfig::Ollama => ProviderKind::Ollama,
            },
            base_url,
            api_key,
            model: self.model.clone(),
            context_window: self.context_window,
            launch: self.launch.as_ref().map(LaunchConfig::to_spec),
        })
    }
}

impl LaunchConfig {
    fn to_spec(&self) -> LaunchSpec {
        LaunchSpec {
            command: self.command.clone(),
            args: self.args.clone(),
            env: self.env.clone(),
            health_path: self.health_path.clone(),
            startup_timeout: Duration::from_secs(self.startup_timeout_secs),
            stop_on_exit: self.stop_on_exit,
            log_file: self.log_file.clone(),
            required_files: self.requires.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use saidwork_kernel::ExposeSecret;

    use super::*;

    fn parse(toml: &str) -> ProviderConfig {
        toml::from_str(toml).unwrap()
    }

    #[test]
    fn resolves_api_key_from_environment() {
        let cfg = parse(
            r#"
            kind = "openai-compatible"
            base_url = "https://api.openai.com"
            model = "whisper-1"
            api_key_env = "OPENAI_API_KEY"
            "#,
        );
        let p = cfg
            .resolve("openai", |v| {
                (v == "OPENAI_API_KEY").then(|| "sk-test".to_owned())
            })
            .unwrap();
        assert_eq!(p.kind, ProviderKind::OpenAiCompatible);
        assert_eq!(p.api_key.unwrap().expose_secret(), "sk-test");
        assert!(p.launch.is_none());
    }

    #[test]
    fn missing_env_var_is_an_error() {
        let cfg = parse(
            "kind = \"ollama\"\nbase_url = \"http://x\"\nmodel = \"m\"\napi_key_env = \"NOPE\"",
        );
        let err = cfg.resolve("o", |_| None).unwrap_err();
        assert!(matches!(err, ProviderError::MissingApiKeyEnv { var, .. } if var == "NOPE"));
    }

    #[test]
    fn inline_key_and_no_key_are_supported() {
        let with =
            parse("kind = \"ollama\"\nbase_url = \"http://x\"\nmodel = \"m\"\napi_key = \"abc\"");
        assert_eq!(
            with.resolve("o", |_| None)
                .unwrap()
                .api_key
                .unwrap()
                .expose_secret(),
            "abc"
        );
        let without = parse("kind = \"ollama\"\nbase_url = \"http://x\"\nmodel = \"m\"");
        assert!(without.resolve("o", |_| None).unwrap().api_key.is_none());
        let blank =
            parse("kind = \"ollama\"\nbase_url = \"http://x\"\nmodel = \"m\"\napi_key = \"  \"");
        assert!(blank.resolve("o", |_| None).unwrap().api_key.is_none());
    }

    #[test]
    fn launch_block_maps_to_spec_with_defaults() {
        let cfg = parse(
            r#"
            kind = "ollama"
            base_url = "http://127.0.0.1:11434"
            model = "qwen3:8b"
            [launch]
            command = "ollama"
            args = ["serve"]
            env = { OLLAMA_KEEP_ALIVE = "10m" }
            requires = ["/models/a.bin"]
            "#,
        );
        let spec = cfg.resolve("ollama", |_| None).unwrap().launch.unwrap();
        assert_eq!(spec.command, "ollama");
        assert_eq!(spec.args, ["serve"]);
        assert_eq!(spec.env["OLLAMA_KEEP_ALIVE"], "10m");
        assert_eq!(spec.startup_timeout, Duration::from_secs(60));
        assert!(spec.stop_on_exit);
        assert!(spec.health_path.is_none());
        assert_eq!(spec.required_files, [PathBuf::from("/models/a.bin")]);
    }

    #[test]
    fn context_window_is_optional() {
        let without = parse("kind = \"ollama\"\nbase_url = \"http://x\"\nmodel = \"m\"");
        assert_eq!(without.resolve("o", |_| None).unwrap().context_window, None);
        let with = parse(
            "kind = \"ollama\"\nbase_url = \"http://x\"\nmodel = \"m\"\ncontext_window = 16384",
        );
        assert_eq!(
            with.resolve("o", |_| None).unwrap().context_window,
            Some(16384)
        );
    }

    #[test]
    fn invalid_url_and_unknown_fields_are_rejected() {
        let cfg = parse("kind = \"ollama\"\nbase_url = \"not a url\"\nmodel = \"m\"");
        assert!(matches!(
            cfg.resolve("o", |_| None).unwrap_err(),
            ProviderError::InvalidUrl { .. }
        ));
        let err = toml::from_str::<ProviderConfig>(
            "kind = \"ollama\"\nbase_url = \"http://x\"\nmodel = \"m\"\nbogus = 1",
        )
        .unwrap_err();
        assert!(err.to_string().contains("bogus"));
    }
}

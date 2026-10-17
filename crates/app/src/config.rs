use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use saidwork_kernel::{AppPaths, expand_tilde};
use saidwork_providers::ProviderConfig;
use serde::Deserialize;

/// The shipped example configuration; also used as the template for `config init`.
pub const EXAMPLE_CONFIG: &str = include_str!("../../../config/saidwork.example.toml");

/// A complete configuration `config init --preset` can start from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Preset {
    pub id: &'static str,
    pub description: &'static str,
    pub toml: &'static str,
}

/// The shipped presets; the first is the default.
pub const PRESETS: &[Preset] = &[
    Preset {
        id: "local",
        description: "Swedish, fully local: Pianissimo (uv) + Gemma 4 on llama-server",
        toml: EXAMPLE_CONFIG,
    },
    Preset {
        id: "whisper-ollama",
        description: "any language, fully local: whisper.cpp server + Ollama",
        toml: include_str!("../../../config/presets/whisper-ollama.toml"),
    },
    Preset {
        id: "openai",
        description: "hosted: OpenAI transcription and chat (needs OPENAI_API_KEY)",
        toml: include_str!("../../../config/presets/openai.toml"),
    },
];

/// Look up a preset by id.
#[must_use]
pub fn preset(id: &str) -> Option<&'static Preset> {
    PRESETS.iter().find(|p| p.id == id)
}

/// Top-level configuration file.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub capture: CaptureConfig,
    pub pipeline: PipelineConfig,
    #[serde(default)]
    pub notes: NotesConfig,
    #[serde(default)]
    pub formats: FormatsConfig,
    #[serde(default)]
    pub providers: BTreeMap<String, ProviderConfig>,
}

/// Which device to capture for one source kind.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum DeviceChoice {
    #[default]
    Default,
    None,
    Id(String),
}

impl<'de> Deserialize<'de> for DeviceChoice {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Ok(match s.trim() {
            "" | "default" => Self::Default,
            "none" | "off" => Self::None,
            id => Self::Id(id.to_owned()),
        })
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct CaptureConfig {
    #[serde(default)]
    pub microphone: DeviceChoice,
    #[serde(default)]
    pub system_audio: DeviceChoice,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PipelineConfig {
    pub transcriber: String,
    pub rewriter: String,
    #[serde(default)]
    pub default_format: Option<String>,
    #[serde(default)]
    pub language: Option<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct NotesConfig {
    #[serde(default)]
    pub directory: Option<PathBuf>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct FormatsConfig {
    #[serde(default)]
    pub directory: Option<PathBuf>,
}

/// Errors while loading configuration.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("no configuration file at `{path}`; run `saidwork config init` to create one")]
    Missing { path: PathBuf },
    #[error("could not read `{path}`: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid configuration in `{path}`: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("pipeline.{role} refers to unknown provider `{name}`")]
    UnknownProvider { role: &'static str, name: String },
}

impl Config {
    /// Parse TOML text; `path` is only used in error messages.
    pub fn parse(text: &str, path: &Path) -> Result<Self, ConfigError> {
        let config: Self = toml::from_str(text).map_err(|source| ConfigError::Parse {
            path: path.to_path_buf(),
            source,
        })?;
        config.validate()?;
        Ok(config)
    }

    /// Read and parse the file at `path`.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        if !path.exists() {
            return Err(ConfigError::Missing {
                path: path.to_path_buf(),
            });
        }
        let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        Self::parse(&text, path)
    }

    fn validate(&self) -> Result<(), ConfigError> {
        for (role, name) in [
            ("transcriber", &self.pipeline.transcriber),
            ("rewriter", &self.pipeline.rewriter),
        ] {
            if !self.providers.contains_key(name) {
                return Err(ConfigError::UnknownProvider {
                    role,
                    name: name.clone(),
                });
            }
        }
        Ok(())
    }

    /// Notes directory, falling back to the platform default.
    #[must_use]
    pub fn notes_dir(&self, paths: &AppPaths) -> PathBuf {
        self.notes
            .directory
            .as_deref()
            .map_or_else(|| paths.default_notes_dir.clone(), expand_tilde)
    }

    /// Formats directory, falling back to the platform default.
    #[must_use]
    pub fn formats_dir(&self, paths: &AppPaths) -> PathBuf {
        self.formats
            .directory
            .as_deref()
            .map_or_else(|| paths.formats_dir(), expand_tilde)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_config_parses_and_validates() {
        let cfg = Config::parse(EXAMPLE_CONFIG, Path::new("example")).unwrap();
        assert_eq!(cfg.pipeline.transcriber, "pianissimo");
        assert_eq!(cfg.pipeline.rewriter, "gemma-local");
        assert_eq!(cfg.providers.len(), 2);
        assert!(cfg.providers["pianissimo"].launch.is_some());
        assert_eq!(cfg.pipeline.language.as_deref(), Some("sv"));
        assert_eq!(cfg.capture.microphone, DeviceChoice::Default);
    }

    #[test]
    fn every_preset_parses_and_validates() {
        assert_eq!(PRESETS[0].toml, EXAMPLE_CONFIG);
        for p in PRESETS {
            let cfg = Config::parse(p.toml, Path::new(p.id)).unwrap_or_else(|e| panic!("{e}"));
            assert!(
                p.toml.contains(&format!("--preset {}", p.id)) || p.id == "local",
                "preset {} documents how to select it",
                p.id
            );
            assert!(cfg.pipeline.default_format.is_some(), "{}", p.id);
        }
        assert_eq!(preset("openai").unwrap().id, "openai");
        assert!(preset("nope").is_none());
    }

    #[test]
    fn example_config_has_no_machine_specific_paths() {
        assert!(
            !EXAMPLE_CONFIG.contains("develop/"),
            "points into a source checkout"
        );
        assert!(EXAMPLE_CONFIG.contains("{share}/pianissimo-server/server.py"));
    }

    #[test]
    fn device_choice_words() {
        let cfg: CaptureConfig =
            toml::from_str("microphone = \"none\"\nsystem_audio = \"alsa_output.x.monitor\"")
                .unwrap();
        assert_eq!(cfg.microphone, DeviceChoice::None);
        assert_eq!(
            cfg.system_audio,
            DeviceChoice::Id("alsa_output.x.monitor".into())
        );
    }

    #[test]
    fn unknown_provider_reference_is_rejected() {
        let err = Config::parse(
            "[pipeline]\ntranscriber = \"a\"\nrewriter = \"b\"",
            Path::new("x"),
        )
        .unwrap_err();
        assert!(
            matches!(
                err,
                ConfigError::UnknownProvider {
                    role: "transcriber",
                    ..
                }
            ),
            "{err}"
        );
    }

    #[test]
    fn missing_file_is_reported() {
        let err = Config::load(Path::new("/definitely/not/here.toml")).unwrap_err();
        assert!(matches!(err, ConfigError::Missing { .. }));
    }

    #[test]
    fn directories_fall_back_to_platform_defaults() {
        let paths = AppPaths::rooted_at(Path::new("/r"));
        let cfg = Config::parse(EXAMPLE_CONFIG, Path::new("x")).unwrap();
        assert_eq!(cfg.notes_dir(&paths), PathBuf::from("/r/notes"));
        assert_eq!(cfg.formats_dir(&paths), PathBuf::from("/r/config/formats"));
        let mut custom = cfg;
        custom.notes.directory = Some(PathBuf::from("/elsewhere"));
        assert_eq!(custom.notes_dir(&paths), PathBuf::from("/elsewhere"));
    }
}

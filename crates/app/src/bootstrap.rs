use std::sync::Arc;

use saidwork_capture::AudioBackend;
use saidwork_formats::{BuiltinSync, FormatError, FsFormatRepository};
use saidwork_kernel::{AppPaths, SystemClock};
use saidwork_notes::FsNoteStore;
use saidwork_providers::{
    HttpHealthProbe, ProviderError, ProviderName, ProviderSupervisor, TokioLauncher,
};

use crate::capture_service::CaptureService;
use crate::config::Config;
use crate::factory::{build_rewriter, build_transcriber};
use crate::format_drafts::FormatDrafts;
use crate::pipeline::Pipeline;

/// Errors while assembling the application.
#[derive(Debug, thiserror::Error)]
pub enum BootstrapError {
    #[error(transparent)]
    Provider(#[from] ProviderError),
    #[error(transparent)]
    Format(#[from] FormatError),
    #[error("no audio backend is available on this platform")]
    NoAudioBackend,
}

/// The assembled application: everything a user interface needs.
#[derive(Debug)]
pub struct App {
    pub config: Config,
    pub paths: AppPaths,
    pub pipeline: Arc<Pipeline>,
    pub capture: CaptureService,
    pub formats: Arc<FsFormatRepository>,
    pub drafts: FormatDrafts,
    /// What startup did with the built-in formats.
    pub builtin_formats: BuiltinSync,
}

impl App {
    /// Wire real adapters around `config`.
    pub fn build(
        config: Config,
        paths: AppPaths,
        backend: Arc<dyn AudioBackend>,
    ) -> Result<Self, BootstrapError> {
        let in_use = [
            config.pipeline.transcriber.as_str(),
            config.pipeline.rewriter.as_str(),
        ];
        let mut providers = Vec::new();
        for (name, cfg) in &config.providers {
            let mut provider = match cfg.resolve(name, |var| std::env::var(var).ok()) {
                Ok(p) => p,
                // A hosted provider that is configured but not selected must not block startup.
                Err(err @ ProviderError::MissingApiKeyEnv { .. })
                    if !in_use.contains(&name.as_str()) =>
                {
                    tracing::warn!(provider = %name, %err, "skipping provider");
                    continue;
                }
                Err(err) => return Err(err.into()),
            };
            if let Some(launch) = provider.launch.as_mut() {
                launch.command =
                    saidwork_kernel::expand_tilde(std::path::Path::new(&launch.command))
                        .display()
                        .to_string();
                launch.args = launch
                    .args
                    .iter()
                    .map(|a| {
                        saidwork_kernel::expand_tilde(std::path::Path::new(a))
                            .display()
                            .to_string()
                    })
                    .collect();
                if launch.log_file.is_none() {
                    launch.log_file =
                        Some(paths.data_dir.join("providers").join(format!("{name}.log")));
                }
            }
            providers.push(provider);
        }
        let supervisor = Arc::new(ProviderSupervisor::new(
            providers,
            Arc::new(HttpHealthProbe::new()),
            Arc::new(TokioLauncher::new()),
        ));

        let transcriber_name = ProviderName::new(&config.pipeline.transcriber)?;
        let rewriter_name = ProviderName::new(&config.pipeline.rewriter)?;
        let transcriber = build_transcriber(supervisor.get(&transcriber_name)?);
        let rewriter = build_rewriter(supervisor.get(&rewriter_name)?);

        let formats = Arc::new(FsFormatRepository::new(config.formats_dir(&paths)));
        let builtin_formats = formats.ensure_initialised()?;
        for path in &builtin_formats.written {
            tracing::info!(path = %path.display(), "installed built-in format");
        }

        let pipeline = Arc::new(Pipeline {
            supervisor,
            transcriber_name,
            rewriter_name,
            transcriber,
            rewriter,
            formats: formats.clone(),
            notes: Arc::new(FsNoteStore::new(config.notes_dir(&paths))),
            sessions_dir: paths.sessions_dir(),
            clock: Arc::new(SystemClock),
            language: config.pipeline.language.clone(),
        });

        let drafts = FormatDrafts::new(formats.clone(), paths.drafts_dir());
        Ok(Self {
            config,
            paths,
            pipeline,
            capture: CaptureService::new(backend),
            drafts,
            formats,
            builtin_formats,
        })
    }

    /// One warning per built-in format the user has changed, so they know it no longer
    /// matches the shipped version.
    #[must_use]
    pub fn format_warnings(&self) -> Vec<String> {
        self.builtin_formats
            .customised
            .iter()
            .map(|path| {
                let id = path.file_stem().unwrap_or_default().to_string_lossy();
                format!(
                    "{} differs from the built-in format; `saidwork formats reset {id}` restores it",
                    path.display()
                )
            })
            .collect()
    }

    /// Stop any providers this process launched.
    pub async fn shutdown(&self) {
        self.pipeline.supervisor.shutdown().await;
    }
}

/// The platform's native audio backend.
pub fn default_backend() -> Result<Arc<dyn AudioBackend>, BootstrapError> {
    #[cfg(target_os = "linux")]
    {
        Ok(Arc::new(saidwork_capture::PulseBackend::new()))
    }
    #[cfg(not(target_os = "linux"))]
    {
        Err(BootstrapError::NoAudioBackend)
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use saidwork_capture::FakeBackend;
    use saidwork_formats::FormatRepository;

    use super::*;
    use crate::config::EXAMPLE_CONFIG;

    #[tokio::test]
    async fn builds_from_the_example_config() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = AppPaths::rooted_at(tmp.path());
        let config = Config::parse(EXAMPLE_CONFIG, Path::new("example")).unwrap();
        let app = App::build(config, paths.clone(), Arc::new(FakeBackend::new())).unwrap();
        assert_eq!(
            app.formats.list().unwrap().len(),
            saidwork_formats::BUILTIN_FORMATS.len(),
            "built-in formats materialised"
        );
        assert!(paths.formats_dir().join("meeting-notes.md").exists());
        let launch = app
            .pipeline
            .supervisor
            .get(&ProviderName::new("gemma-local").unwrap())
            .unwrap()
            .launch
            .clone()
            .unwrap();
        assert!(
            launch
                .log_file
                .unwrap()
                .ends_with("providers/gemma-local.log")
        );
        assert!(
            !launch.args.iter().any(|a| a.starts_with('~')),
            "tilde expanded in args"
        );
        let names: Vec<_> = app
            .pipeline
            .supervisor
            .providers()
            .map(|p| p.name.to_string())
            .collect();
        assert!(
            names.contains(&"pianissimo".to_owned()) && names.contains(&"gemma-local".to_owned()),
            "{names:?}"
        );
        app.shutdown().await;
    }

    #[test]
    fn missing_key_for_a_selected_provider_is_fatal() {
        let tmp = tempfile::tempdir().unwrap();
        let toml = r#"
            [pipeline]
            transcriber = "cloud"
            rewriter = "cloud"
            [providers.cloud]
            kind = "openai-compatible"
            base_url = "https://example.invalid"
            model = "m"
            api_key_env = "SAIDWORK_TEST_UNSET_KEY_VAR"
        "#;
        let config = Config::parse(toml, Path::new("t")).unwrap();
        let err = App::build(
            config,
            AppPaths::rooted_at(tmp.path()),
            Arc::new(FakeBackend::new()),
        )
        .unwrap_err();
        assert!(
            matches!(
                err,
                BootstrapError::Provider(ProviderError::MissingApiKeyEnv { .. })
            ),
            "{err}"
        );
    }
}

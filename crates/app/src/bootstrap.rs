use std::path::{Path, PathBuf};
use std::sync::Arc;

use saidwork_capture::AudioBackend;
use saidwork_formats::{BuiltinSync, FormatError, FsFormatRepository};
use saidwork_kernel::{AppPaths, SystemClock, expand_tilde};
use saidwork_notes::FsNoteStore;
use saidwork_providers::{
    HttpHealthProbe, ProviderError, ProviderName, ProviderSupervisor, TokioLauncher,
    http_client_available,
};

use crate::capture_service::CaptureService;
use crate::config::Config;
use crate::factory::{build_rewriter, build_transcriber};
use crate::format_drafts::FormatDrafts;
use crate::pipeline::Pipeline;

/// `{share}/<file>` in a launch command or argument names a file shipped with saidwork,
/// looked up in [`AppPaths::resource_dirs`].
pub const SHARE_PLACEHOLDER: &str = "{share}";

/// Expand `~` and `{share}/…` in one launch command or argument. Shipped files are
/// recorded in `required` so a missing one is reported before launching; one that is
/// not found keeps its `{share}/…` spelling so the report names it.
fn resolve_launch_word(word: &str, paths: &AppPaths, required: &mut Vec<PathBuf>) -> String {
    let Some(relative) = word
        .strip_prefix(SHARE_PLACEHOLDER)
        .and_then(|r| r.strip_prefix('/'))
    else {
        return expand_tilde(Path::new(word)).display().to_string();
    };
    // Configs always use `/`; rebuild it with the platform's separator.
    let relative: PathBuf = relative.split('/').collect();
    let path = paths
        .find_resource(&relative)
        .unwrap_or_else(|| PathBuf::from(word));
    let resolved = path.display().to_string();
    required.push(path);
    resolved
}

/// Errors while assembling the application.
#[derive(Debug, thiserror::Error)]
pub enum BootstrapError {
    #[error(transparent)]
    Provider(#[from] ProviderError),
    #[error(transparent)]
    Format(#[from] FormatError),
    #[error("no audio backend is available on this platform")]
    NoAudioBackend,
    #[error(
        "cannot make HTTP requests: {0}; install the system CA certificates (`{fix}`)",
        fix = crate::requirements::CA_CERTIFICATES.install_steps(crate::requirements::Distro::detect())
    )]
    NoTls(String),
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
        http_client_available().map_err(BootstrapError::NoTls)?;
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
                let declared = std::mem::take(&mut launch.required_files);
                let required = &mut launch.required_files;
                for file in &declared {
                    let recorded = required.len();
                    let word = resolve_launch_word(&file.to_string_lossy(), &paths, required);
                    // `{share}/…` entries are recorded by the resolution itself.
                    if required.len() == recorded {
                        required.push(PathBuf::from(word));
                    }
                }
                launch.command = resolve_launch_word(&launch.command, &paths, required);
                launch.args = launch
                    .args
                    .iter()
                    .map(|a| resolve_launch_word(a, &paths, required))
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
    #[cfg(windows)]
    {
        Ok(Arc::new(saidwork_capture::WasapiBackend::new()))
    }
    #[cfg(not(any(target_os = "linux", windows)))]
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
        let pianissimo = app
            .pipeline
            .supervisor
            .get(&ProviderName::new("pianissimo").unwrap())
            .unwrap()
            .launch
            .clone()
            .unwrap();
        assert!(
            pianissimo.required_files[0].ends_with("pianissimo-server/server.py"),
            "{:?}",
            pianissimo.required_files
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
    fn share_placeholder_resolves_to_a_shipped_file() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = AppPaths::rooted_at(tmp.path());
        let script = tmp
            .path()
            .join("share")
            .join("pianissimo-server")
            .join("server.py");
        std::fs::create_dir_all(script.parent().unwrap()).unwrap();
        std::fs::write(&script, "").unwrap();

        let mut required = Vec::new();
        let word =
            resolve_launch_word("{share}/pianissimo-server/server.py", &paths, &mut required);
        assert_eq!(word, script.display().to_string());
        assert_eq!(required, [script]);

        let missing = resolve_launch_word("{share}/nope/x.py", &paths, &mut required);
        assert_eq!(missing, "{share}/nope/x.py");
        assert_eq!(required[1], PathBuf::from("{share}/nope/x.py"));

        assert_eq!(
            resolve_launch_word("--port", &paths, &mut required),
            "--port"
        );
        assert_eq!(required.len(), 2, "plain words are not required files");
    }

    #[test]
    fn declared_requirements_are_expanded_once() {
        let tmp = tempfile::tempdir().unwrap();
        let toml = r#"
            [pipeline]
            transcriber = "w"
            rewriter = "w"
            [providers.w]
            kind = "openai-compatible"
            base_url = "http://127.0.0.1:9"
            model = "m"
            [providers.w.launch]
            command = "whisper-server"
            args = ["-m", "~/models/x.bin"]
            requires = ["~/models/x.bin", "{share}/pianissimo-server/server.py"]
        "#;
        let config = Config::parse(toml, Path::new("t")).unwrap();
        let app = App::build(
            config,
            AppPaths::rooted_at(tmp.path()),
            Arc::new(FakeBackend::new()),
        )
        .unwrap();
        let launch = app
            .pipeline
            .supervisor
            .get(&ProviderName::new("w").unwrap())
            .unwrap()
            .launch
            .clone()
            .unwrap();
        assert_eq!(
            launch.required_files.len(),
            2,
            "{:?}",
            launch.required_files
        );
        assert!(!launch.required_files[0].starts_with("~"));
        assert!(launch.required_files[0].ends_with("models/x.bin"));
        assert!(launch.required_files[1].ends_with("pianissimo-server/server.py"));
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

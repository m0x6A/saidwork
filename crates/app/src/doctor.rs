//! Checks that tell the user what is missing and how to fix it (`saidwork doctor` and
//! the start-up check).

use std::fmt;

use saidwork_capture::AudioBackend;
use saidwork_providers::{MissingRequirement, Provider, ProviderName, ProviderStatus};

use crate::bootstrap::{App, SHARE_PLACEHOLDER};
use crate::requirements::{AUDIO_SERVER, Distro, known_command};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckStatus {
    Ok,
    /// Works, but something deserves attention.
    Warn,
    /// saidwork cannot do its job until this is fixed.
    Fail,
}

/// One finding: what was checked, how it went, and how to fix it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    pub subject: String,
    pub status: CheckStatus,
    pub detail: String,
    pub fix: Option<String>,
    /// The provider this is about, if any.
    pub provider: Option<ProviderName>,
}

impl Check {
    fn new(subject: impl Into<String>, status: CheckStatus, detail: impl Into<String>) -> Self {
        Self {
            subject: subject.into(),
            status,
            detail: detail.into(),
            fix: None,
            provider: None,
        }
    }

    fn with_fix(mut self, fix: impl Into<String>) -> Self {
        self.fix = Some(fix.into());
        self
    }

    #[must_use]
    pub fn is_failure(&self) -> bool {
        self.status == CheckStatus::Fail
    }
}

impl fmt::Display for Check {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mark = match self.status {
            CheckStatus::Ok => "ok  ",
            CheckStatus::Warn => "warn",
            CheckStatus::Fail => "FAIL",
        };
        write!(f, "[{mark}] {}: {}", self.subject, self.detail)?;
        if let Some(fix) = &self.fix {
            write!(f, "\n       fix: {fix}")?;
        }
        Ok(())
    }
}

/// Can the audio backend reach a sound server and list devices?
pub fn check_audio(backend: &dyn AudioBackend, distro: Distro) -> Check {
    match backend.list_devices() {
        Ok(devices) if devices.is_empty() => Check::new(
            "audio",
            CheckStatus::Warn,
            "sound server answers but lists no devices",
        )
        .with_fix(if distro == Distro::Windows {
            "connect a microphone, or check Settings › System › Sound"
        } else {
            "connect a microphone, or check `pactl list sources short`"
        }),
        Ok(devices) => Check::new(
            "audio",
            CheckStatus::Ok,
            format!(
                "{} capture devices (`saidwork devices` lists them)",
                devices.len()
            ),
        ),
        Err(err) => Check::new("audio", CheckStatus::Fail, err.to_string()).with_fix(format!(
            "saidwork needs a {}: {}",
            AUDIO_SERVER.purpose,
            AUDIO_SERVER.install_steps(distro)
        )),
    }
}

impl App {
    /// Check the transcriber and rewriter the pipeline uses. Probes their health URLs.
    pub async fn check_providers(&self, distro: Distro) -> Vec<Check> {
        let pipeline = &self.pipeline;
        let mut names = vec![("transcriber", &pipeline.transcriber_name)];
        if pipeline.rewriter_name != pipeline.transcriber_name {
            names.push(("rewriter", &pipeline.rewriter_name));
        }
        let mut checks = Vec::new();
        for (role, name) in names {
            let Ok(provider) = pipeline.supervisor.get(name) else {
                continue;
            };
            let subject = format!("{role} `{name}`");
            checks.extend(
                self.check_provider(provider, &subject, distro)
                    .await
                    .into_iter()
                    .map(|mut c| {
                        c.provider = Some(name.clone());
                        c
                    }),
            );
        }
        checks
    }

    async fn check_provider(
        &self,
        provider: &Provider,
        subject: &str,
        distro: Distro,
    ) -> Vec<Check> {
        let name = &provider.name;
        let supervisor = &self.pipeline.supervisor;
        if matches!(supervisor.status(name).await, Ok(ProviderStatus::Up)) {
            return vec![Check::new(
                subject,
                CheckStatus::Ok,
                format!("answering at {}", provider.base_url),
            )];
        }
        let Some(launch) = &provider.launch else {
            return vec![
                Check::new(
                    subject,
                    CheckStatus::Fail,
                    format!(
                        "not answering at {} and has no launch block",
                        provider.base_url
                    ),
                )
                .with_fix(format!(
                    "start the server yourself, or add a [providers.{name}.launch] block \
                     (see `saidwork config presets`)"
                )),
            ];
        };
        let missing = supervisor.missing_requirements(name).unwrap_or_default();
        if missing.is_empty() {
            let note = known_command(&launch.command)
                .and_then(|d| d.note)
                .map(|n| format!("; {n}"))
                .unwrap_or_default();
            return vec![Check::new(
                subject,
                CheckStatus::Ok,
                format!(
                    "not running; started on demand with `{}`{note}",
                    launch.command
                ),
            )];
        }
        missing
            .iter()
            .map(|m| {
                Check::new(subject, CheckStatus::Fail, m.to_string())
                    .with_fix(self.fix_for(provider, m, distro))
            })
            .collect()
    }

    /// The failing provider checks: what start-up reports before anything is recorded.
    pub async fn preflight(&self) -> Vec<Check> {
        let mut checks = self.check_providers(Distro::detect()).await;
        checks.retain(Check::is_failure);
        checks
    }

    fn fix_for(&self, provider: &Provider, missing: &MissingRequirement, distro: Distro) -> String {
        let name = &provider.name;
        match missing {
            MissingRequirement::Command(command) => match known_command(command) {
                Some(dep) => format!(
                    "install {} ({}): {}",
                    dep.id,
                    dep.purpose,
                    dep.install_steps(distro)
                ),
                None => format!(
                    "install `{command}`, or point providers.{name}.launch.command at an \
                     executable"
                ),
            },
            MissingRequirement::File(path) if path.starts_with(SHARE_PLACEHOLDER) => {
                let searched: Vec<String> = self
                    .paths
                    .resource_dirs
                    .iter()
                    .map(|d| d.display().to_string())
                    .collect();
                format!(
                    "this file ships with saidwork; reinstall from a release package or set \
                     SAIDWORK_SHARE_DIR to the directory that holds it (searched {})",
                    searched.join(", ")
                )
            }
            MissingRequirement::File(path) => format!(
                "download or create `{}` (a model is usually fetched with the server's own \
                 tooling), or fix the path in providers.{name}.launch",
                path.display()
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;

    use saidwork_capture::FakeBackend;
    use saidwork_kernel::AppPaths;

    use super::*;
    use crate::config::Config;

    fn app(toml: &str, root: &Path) -> App {
        let config = Config::parse(toml, Path::new("t")).unwrap();
        App::build(
            config,
            AppPaths::rooted_at(root),
            Arc::new(FakeBackend::new()),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn missing_command_and_shipped_file_fail_with_install_hints() {
        let tmp = tempfile::tempdir().unwrap();
        let app = app(
            r#"
            [pipeline]
            transcriber = "stt"
            rewriter = "llm"
            [providers.stt]
            kind = "openai-compatible"
            base_url = "http://127.0.0.1:9"
            model = "m"
            [providers.stt.launch]
            command = "uv"
            args = ["run", "{share}/no-such-dir/server.py"]
            [providers.llm]
            kind = "openai-compatible"
            base_url = "http://127.0.0.1:9"
            model = "m"
            [providers.llm.launch]
            command = "definitely-not-a-real-command-xyz"
            "#,
            tmp.path(),
        );
        let checks = app.check_providers(Distro::Fedora).await;
        let shipped = checks
            .iter()
            .find(|c| c.detail.contains("no-such-dir/server.py"))
            .expect("unresolved {share} file reported");
        assert!(shipped.is_failure());
        assert!(
            shipped
                .fix
                .as_deref()
                .unwrap()
                .contains("SAIDWORK_SHARE_DIR")
        );

        let unknown = checks
            .iter()
            .find(|c| c.subject == "rewriter `llm`")
            .unwrap();
        assert!(unknown.is_failure());
        assert!(unknown.fix.as_deref().unwrap().contains("launch.command"));

        if checks
            .iter()
            .any(|c| c.detail.contains("`uv` is not installed"))
        {
            let uv = checks.iter().find(|c| c.detail.contains("`uv`")).unwrap();
            assert!(uv.fix.as_deref().unwrap().contains("sudo dnf install uv"));
        }
        app.shutdown().await;
    }

    #[tokio::test]
    async fn provider_without_launch_block_fails_when_down() {
        let tmp = tempfile::tempdir().unwrap();
        let app = app(
            r#"
            [pipeline]
            transcriber = "remote"
            rewriter = "remote"
            [providers.remote]
            kind = "ollama"
            base_url = "http://127.0.0.1:9"
            model = "m"
            "#,
            tmp.path(),
        );
        let checks = app.preflight().await;
        assert_eq!(
            checks.len(),
            1,
            "one check for a shared provider: {checks:?}"
        );
        assert!(checks[0].detail.contains("no launch block"));
        let shown = checks[0].to_string();
        assert!(shown.starts_with("[FAIL] transcriber `remote`"), "{shown}");
        assert!(shown.contains("fix: start the server yourself"), "{shown}");
    }

    #[test]
    fn audio_check_reports_devices() {
        let check = check_audio(&FakeBackend::new(), Distro::Other);
        assert_ne!(check.status, CheckStatus::Fail, "{check}");
    }
}

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Mutex;
use tokio::time::Instant;

use crate::domain::{Provider, ProviderError, ProviderName};
use crate::ports::{HealthProbe, ManagedProcess, ProcessLauncher};

/// Observed state of a provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderStatus {
    Up,
    Down,
}

/// Outcome of [`ProviderSupervisor::ensure_running`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Readiness {
    /// It was already answering; nothing was started.
    AlreadyUp,
    /// We launched it and it became healthy.
    Started,
}

/// Knows every configured provider, checks their health and starts local ones on demand.
pub struct ProviderSupervisor {
    providers: BTreeMap<ProviderName, Provider>,
    probe: Arc<dyn HealthProbe>,
    launcher: Arc<dyn ProcessLauncher>,
    poll_interval: Duration,
    running: Mutex<BTreeMap<ProviderName, Box<dyn ManagedProcess>>>,
}

impl std::fmt::Debug for ProviderSupervisor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderSupervisor")
            .field("providers", &self.providers.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

impl ProviderSupervisor {
    pub fn new(
        providers: impl IntoIterator<Item = Provider>,
        probe: Arc<dyn HealthProbe>,
        launcher: Arc<dyn ProcessLauncher>,
    ) -> Self {
        Self {
            providers: providers.into_iter().map(|p| (p.name.clone(), p)).collect(),
            probe,
            launcher,
            poll_interval: Duration::from_millis(500),
            running: Mutex::new(BTreeMap::new()),
        }
    }

    /// How often to re-probe while waiting for a launched provider.
    #[must_use]
    pub fn with_poll_interval(mut self, interval: Duration) -> Self {
        self.poll_interval = interval;
        self
    }

    /// Look up a provider by name.
    pub fn get(&self, name: &ProviderName) -> Result<&Provider, ProviderError> {
        self.providers
            .get(name)
            .ok_or_else(|| ProviderError::Unknown(name.clone()))
    }

    /// All providers, sorted by name.
    pub fn providers(&self) -> impl Iterator<Item = &Provider> {
        self.providers.values()
    }

    /// Probe one provider.
    pub async fn status(&self, name: &ProviderName) -> Result<ProviderStatus, ProviderError> {
        let provider = self.get(name)?;
        Ok(if self.probe.is_up(&provider.health_url()).await {
            ProviderStatus::Up
        } else {
            ProviderStatus::Down
        })
    }

    /// Make sure the provider answers, launching it if it is configured to be launchable.
    pub async fn ensure_running(&self, name: &ProviderName) -> Result<Readiness, ProviderError> {
        let provider = self.get(name)?;
        let url = provider.health_url();
        if self.probe.is_up(&url).await {
            return Ok(Readiness::AlreadyUp);
        }
        let spec = provider
            .launch
            .as_ref()
            .ok_or_else(|| ProviderError::NotRunning(name.clone()))?;

        let mut running = self.running.lock().await;
        if let Some(existing) = running.get_mut(name) {
            if !existing.has_exited() {
                // Someone else already launched it; just wait for health below.
                drop(running);
                return self
                    .wait_until_up(provider, &url)
                    .await
                    .map(|()| Readiness::Started);
            }
            running.remove(name);
        }
        tracing::info!(provider = %name, command = %spec.command, "launching provider");
        let process = self.launcher.spawn(spec).await?;
        running.insert(name.clone(), process);
        drop(running);

        match self.wait_until_up(provider, &url).await {
            Ok(()) => Ok(Readiness::Started),
            Err(err) => {
                if let Some(mut p) = self.running.lock().await.remove(name) {
                    let _ = p.terminate().await;
                }
                Err(err)
            }
        }
    }

    async fn wait_until_up(
        &self,
        provider: &Provider,
        url: &url::Url,
    ) -> Result<(), ProviderError> {
        let spec = provider
            .launch
            .as_ref()
            .expect("only called for launchable providers");
        let deadline = Instant::now() + spec.startup_timeout;
        loop {
            if self.probe.is_up(url).await {
                return Ok(());
            }
            if let Some(p) = self.running.lock().await.get_mut(&provider.name)
                && p.has_exited()
            {
                return Err(ProviderError::ExitedEarly {
                    name: provider.name.clone(),
                    command: spec.command.clone(),
                });
            }
            if Instant::now() >= deadline {
                return Err(ProviderError::StartupTimeout {
                    name: provider.name.clone(),
                    command: spec.command.clone(),
                    timeout: spec.startup_timeout,
                });
            }
            tokio::time::sleep(self.poll_interval).await;
        }
    }

    /// Stop every process we started whose spec says `stop_on_exit`.
    pub async fn shutdown(&self) {
        let mut running = self.running.lock().await;
        for (name, mut process) in std::mem::take(&mut *running) {
            let stop = self
                .providers
                .get(&name)
                .and_then(|p| p.launch.as_ref())
                .is_none_or(|l| l.stop_on_exit);
            if stop {
                tracing::info!(provider = %name, "stopping provider");
                if let Err(err) = process.terminate().await {
                    tracing::warn!(provider = %name, %err, "failed to stop provider");
                }
            }
        }
    }

    /// Names of providers whose processes we currently hold.
    pub async fn launched(&self) -> Vec<ProviderName> {
        self.running.lock().await.keys().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex as StdMutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;
    use url::Url;

    use super::*;
    use crate::domain::{LaunchSpec, ProviderKind};

    /// Probe answering a scripted sequence (last answer repeats).
    struct ScriptedProbe {
        answers: Vec<bool>,
        calls: AtomicUsize,
    }

    impl ScriptedProbe {
        fn new(answers: &[bool]) -> Arc<Self> {
            Arc::new(Self {
                answers: answers.to_vec(),
                calls: AtomicUsize::new(0),
            })
        }
    }

    #[async_trait]
    impl HealthProbe for ScriptedProbe {
        async fn is_up(&self, _url: &Url) -> bool {
            let i = self.calls.fetch_add(1, Ordering::SeqCst);
            *self
                .answers
                .get(i)
                .or(self.answers.last())
                .unwrap_or(&false)
        }
    }

    #[derive(Debug)]
    struct FakeProcess {
        exited: bool,
        terminated: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl ManagedProcess for FakeProcess {
        fn has_exited(&mut self) -> bool {
            self.exited
        }
        async fn terminate(&mut self) -> Result<(), ProviderError> {
            self.terminated.fetch_add(1, Ordering::SeqCst);
            self.exited = true;
            Ok(())
        }
    }

    struct FakeLauncher {
        spawned: StdMutex<Vec<String>>,
        terminated: Arc<AtomicUsize>,
        exits_immediately: bool,
    }

    impl FakeLauncher {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                spawned: StdMutex::new(vec![]),
                terminated: Arc::new(AtomicUsize::new(0)),
                exits_immediately: false,
            })
        }
    }

    #[async_trait]
    impl ProcessLauncher for FakeLauncher {
        async fn spawn(&self, spec: &LaunchSpec) -> Result<Box<dyn ManagedProcess>, ProviderError> {
            self.spawned.lock().unwrap().push(spec.command.clone());
            Ok(Box::new(FakeProcess {
                exited: self.exits_immediately,
                terminated: self.terminated.clone(),
            }))
        }
    }

    fn provider(name: &str, launch: Option<LaunchSpec>) -> Provider {
        Provider {
            name: ProviderName::new(name).unwrap(),
            kind: ProviderKind::Ollama,
            base_url: Url::parse("http://127.0.0.1:1").unwrap(),
            api_key: None,
            model: "m".into(),
            launch,
        }
    }

    fn launchable(name: &str, timeout_ms: u64) -> Provider {
        let mut spec = LaunchSpec::new("fake-server");
        spec.startup_timeout = Duration::from_millis(timeout_ms);
        provider(name, Some(spec))
    }

    fn supervisor(
        providers: Vec<Provider>,
        probe: Arc<ScriptedProbe>,
        launcher: Arc<FakeLauncher>,
    ) -> ProviderSupervisor {
        ProviderSupervisor::new(providers, probe, launcher)
            .with_poll_interval(Duration::from_millis(1))
    }

    fn name(s: &str) -> ProviderName {
        ProviderName::new(s).unwrap()
    }

    #[tokio::test]
    async fn already_up_does_not_launch() {
        let launcher = FakeLauncher::new();
        let sup = supervisor(
            vec![launchable("a", 100)],
            ScriptedProbe::new(&[true]),
            launcher.clone(),
        );
        assert_eq!(
            sup.ensure_running(&name("a")).await.unwrap(),
            Readiness::AlreadyUp
        );
        assert_eq!(*launcher.spawned.lock().unwrap(), Vec::<String>::new());
        assert_eq!(sup.status(&name("a")).await.unwrap(), ProviderStatus::Up);
    }

    #[tokio::test]
    async fn down_without_launch_is_an_error() {
        let sup = supervisor(
            vec![provider("a", None)],
            ScriptedProbe::new(&[false]),
            FakeLauncher::new(),
        );
        assert!(matches!(
            sup.ensure_running(&name("a")).await.unwrap_err(),
            ProviderError::NotRunning(_)
        ));
    }

    #[tokio::test]
    async fn unknown_provider_is_an_error() {
        let sup = supervisor(vec![], ScriptedProbe::new(&[true]), FakeLauncher::new());
        assert!(matches!(
            sup.ensure_running(&name("zz")).await.unwrap_err(),
            ProviderError::Unknown(_)
        ));
    }

    #[tokio::test]
    async fn launches_and_waits_until_healthy() {
        let launcher = FakeLauncher::new();
        let probe = ScriptedProbe::new(&[false, false, false, true]);
        let sup = supervisor(
            vec![launchable("a", 5_000)],
            probe.clone(),
            launcher.clone(),
        );
        assert_eq!(
            sup.ensure_running(&name("a")).await.unwrap(),
            Readiness::Started
        );
        assert_eq!(*launcher.spawned.lock().unwrap(), ["fake-server"]);
        assert_eq!(sup.launched().await, [name("a")]);
    }

    #[tokio::test]
    async fn times_out_and_cleans_up_when_never_healthy() {
        let launcher = FakeLauncher::new();
        let sup = supervisor(
            vec![launchable("a", 20)],
            ScriptedProbe::new(&[false]),
            launcher.clone(),
        );
        let err = sup.ensure_running(&name("a")).await.unwrap_err();
        assert!(matches!(err, ProviderError::StartupTimeout { .. }), "{err}");
        assert_eq!(
            launcher.terminated.load(Ordering::SeqCst),
            1,
            "failed launch must be terminated"
        );
        assert_eq!(sup.launched().await, Vec::<ProviderName>::new());
    }

    #[tokio::test]
    async fn early_exit_is_reported() {
        let mut launcher = FakeLauncher::new();
        Arc::get_mut(&mut launcher).unwrap().exits_immediately = true;
        let sup = supervisor(
            vec![launchable("a", 5_000)],
            ScriptedProbe::new(&[false]),
            launcher,
        );
        let err = sup.ensure_running(&name("a")).await.unwrap_err();
        assert!(matches!(err, ProviderError::ExitedEarly { .. }), "{err}");
    }

    #[tokio::test]
    async fn shutdown_respects_stop_on_exit() {
        let launcher = FakeLauncher::new();
        let mut keep = LaunchSpec::new("keep");
        keep.stop_on_exit = false;
        let sup = supervisor(
            vec![launchable("stop", 5_000), provider("keep", Some(keep))],
            ScriptedProbe::new(&[false, true, false, true]),
            launcher.clone(),
        );
        sup.ensure_running(&name("stop")).await.unwrap();
        sup.ensure_running(&name("keep")).await.unwrap();
        sup.shutdown().await;
        assert_eq!(launcher.terminated.load(Ordering::SeqCst), 1);
        assert_eq!(sup.launched().await, Vec::<ProviderName>::new());
    }
}

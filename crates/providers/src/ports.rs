use std::fmt::Debug;

use async_trait::async_trait;
use url::Url;

use crate::domain::{LaunchSpec, ProviderError};

/// Decides whether a provider is answering.
#[async_trait]
pub trait HealthProbe: Send + Sync {
    /// `true` when something answers at `url`. Any HTTP response counts; only
    /// connection failures and timeouts mean "down".
    async fn is_up(&self, url: &Url) -> bool;
}

/// A child process we started and are responsible for.
#[async_trait]
pub trait ManagedProcess: Send + Sync + Debug {
    /// `true` if the process has already exited on its own.
    fn has_exited(&mut self) -> bool;

    /// Ask the process to stop (gracefully first, forcefully if needed) and wait for it.
    async fn terminate(&mut self) -> Result<(), ProviderError>;
}

/// Starts provider processes.
#[async_trait]
pub trait ProcessLauncher: Send + Sync {
    async fn spawn(&self, spec: &LaunchSpec) -> Result<Box<dyn ManagedProcess>, ProviderError>;
}

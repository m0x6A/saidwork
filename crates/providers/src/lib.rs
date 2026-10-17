//! LLM provider registry, health probing and on-demand launching.
//!
//! A *provider* is an HTTP endpoint (URL + optional API key + model name). Local
//! providers may carry a [`LaunchSpec`]; the [`ProviderSupervisor`] starts them
//! when they are not answering and stops them again on shutdown.

mod adapters;
mod config;
mod domain;
mod ports;
mod supervisor;

pub use adapters::{HttpHealthProbe, TokioLauncher, http_client_available};
pub use config::{LaunchConfig, ProviderConfig};
pub use domain::{
    LaunchSpec, MissingRequirement, Provider, ProviderError, ProviderKind, ProviderName,
};
pub use ports::{HealthProbe, ManagedProcess, ProcessLauncher};
pub use supervisor::{ProviderStatus, ProviderSupervisor, Readiness};

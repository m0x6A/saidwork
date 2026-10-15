//! Application layer: configuration, wiring of the slices, and the use cases
//! the user interfaces (CLI, TUI) call.

mod bootstrap;
mod capture_service;
mod config;
mod events;
mod factory;
mod pipeline;

pub use bootstrap::{App, BootstrapError, default_backend};
pub use capture_service::{CaptureService, DeviceSelection};
pub use config::{
    CaptureConfig, Config, ConfigError, DeviceChoice, EXAMPLE_CONFIG, FormatsConfig, NotesConfig,
    PipelineConfig,
};
pub use events::SessionEvent;
pub use factory::{build_rewriter, build_transcriber};
pub use pipeline::{Pipeline, SessionError, SessionOutcome};

// Re-export the slice types the UIs need so they depend on `app` only.
pub use saidwork_capture::{AudioDevice, AudioSpec, RecorderHandle, Recording, SourceKind};
pub use saidwork_formats::{Format, FormatId, FormatRepository};
pub use saidwork_providers::{Provider, ProviderName, ProviderStatus, Readiness};
pub use saidwork_transcription::Transcript;

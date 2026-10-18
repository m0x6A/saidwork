//! Audio capture: devices, the [`AudioBackend`] port, mixing of several
//! sources into one mono stream, and WAV encoding of the result.
//!
//! The Linux adapter speaks the PulseAudio protocol, which PipeWire serves
//! natively, so microphones and sink monitors (system audio) are both plain
//! sources. The Windows adapter uses WASAPI, recording render endpoints in
//! loopback mode for system audio. Other platforms only have the [`FakeBackend`].

mod adapters;
mod domain;
mod mixer;
mod ports;
mod recorder;

pub use adapters::FakeBackend;
#[cfg(target_os = "linux")]
pub use adapters::PulseBackend;
#[cfg(windows)]
pub use adapters::WasapiBackend;
pub use domain::{
    AudioDevice, AudioSpec, CaptureError, Recording, RecordingId, SourceKind, resample,
};
pub use mixer::{mix_into, rms_level};
pub use ports::{AudioBackend, AudioStream};
pub use recorder::{Recorder, RecorderHandle};

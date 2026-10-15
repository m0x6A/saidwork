//! Speech-to-text: the [`Transcriber`] port and its HTTP adapters.

mod adapters;
mod domain;
mod ports;

pub use adapters::OpenAiTranscriber;
pub use domain::{AudioInput, Segment, Transcript, TranscriptionError, TranscriptionRequest};
pub use ports::Transcriber;

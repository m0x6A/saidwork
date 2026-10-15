//! Persisting finished notes.

mod adapters;
mod domain;
mod ports;

pub use adapters::FsNoteStore;
pub use domain::{Note, NoteError, file_name_for};
pub use ports::NoteStore;

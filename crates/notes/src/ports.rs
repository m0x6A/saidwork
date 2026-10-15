use std::path::PathBuf;

use crate::domain::{Note, NoteError};

/// Where finished notes go.
pub trait NoteStore: Send + Sync {
    /// Persist the note and return where it ended up.
    fn save(&self, note: &Note) -> Result<PathBuf, NoteError>;
}

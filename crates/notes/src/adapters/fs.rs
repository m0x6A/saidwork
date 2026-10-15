use std::fs;
use std::path::{Path, PathBuf};

use crate::domain::{Note, NoteError, file_name_for};
use crate::ports::NoteStore;

/// Notes as Markdown files in one directory.
#[derive(Debug, Clone)]
pub struct FsNoteStore {
    dir: PathBuf,
}

impl FsNoteStore {
    #[must_use]
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn io(path: &Path, source: std::io::Error) -> NoteError {
        NoteError::Io {
            path: path.display().to_string(),
            source,
        }
    }

    /// First free path for `name`, adding `-2`, `-3`, ... on collisions.
    fn unique_path(&self, name: &str) -> PathBuf {
        let stem = name.trim_end_matches(".md");
        let mut candidate = self.dir.join(name);
        let mut n = 2;
        while candidate.exists() {
            candidate = self.dir.join(format!("{stem}-{n}.md"));
            n += 1;
        }
        candidate
    }
}

impl NoteStore for FsNoteStore {
    fn save(&self, note: &Note) -> Result<PathBuf, NoteError> {
        fs::create_dir_all(&self.dir).map_err(|e| Self::io(&self.dir, e))?;
        let path = self.unique_path(&file_name_for(note));
        fs::write(&path, &note.markdown).map_err(|e| Self::io(&path, e))?;
        Ok(path)
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;

    fn note() -> Note {
        Note {
            markdown: "# hi\n".into(),
            format_id: "f".into(),
            title: None,
            created_at: Utc::now(),
        }
    }

    #[test]
    fn creates_directory_and_writes_file() {
        let tmp = tempfile::tempdir().unwrap();
        let store = FsNoteStore::new(tmp.path().join("a").join("b"));
        let path = store.save(&note()).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "# hi\n");
        assert!(path.starts_with(store.dir()));
    }

    #[test]
    fn collisions_get_a_numeric_suffix() {
        let tmp = tempfile::tempdir().unwrap();
        let store = FsNoteStore::new(tmp.path());
        let n = note();
        let first = store.save(&n).unwrap();
        let second = store.save(&n).unwrap();
        let third = store.save(&n).unwrap();
        assert_ne!(first, second);
        assert!(second.to_string_lossy().ends_with("-f-2.md"), "{second:?}");
        assert!(third.to_string_lossy().ends_with("-f-3.md"), "{third:?}");
    }
}

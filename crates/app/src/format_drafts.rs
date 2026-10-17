//! Editing formats through draft files that are validated before they replace anything.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use saidwork_formats::{Format, FormatError, FormatId, FsFormatRepository, NEW_FORMAT_TEMPLATE};

/// A format draft the user edits in their editor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormatDraft {
    /// File to open in the editor.
    pub path: PathBuf,
    /// The format being edited, or `None` for a new one.
    pub target: Option<FormatId>,
    /// What the draft started from; saving it unchanged is a no-op.
    seed: String,
}

/// Result of [`FormatDrafts::finish`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DraftOutcome {
    Saved(Format),
    Unchanged,
}

/// Drafts live in their own directory and only reach the formats directory once they parse,
/// so a mistake never breaks the format list and an invalid draft is kept to fix later.
#[derive(Debug, Clone)]
pub struct FormatDrafts {
    formats: Arc<FsFormatRepository>,
    dir: PathBuf,
}

impl FormatDrafts {
    #[must_use]
    pub fn new(formats: Arc<FsFormatRepository>, dir: impl Into<PathBuf>) -> Self {
        Self {
            formats,
            dir: dir.into(),
        }
    }

    /// Draft for a new format, resuming a previously rejected one if it exists.
    pub fn begin_new(&self) -> Result<FormatDraft, FormatError> {
        seed_draft(
            self.dir.join("new-format.md"),
            None,
            NEW_FORMAT_TEMPLATE.to_owned(),
        )
    }

    /// Draft for editing the format `id`, resuming a previously rejected one if it exists.
    pub fn begin_edit(&self, id: &FormatId) -> Result<FormatDraft, FormatError> {
        let original = self.formats.path_of(id);
        let seed = fs::read_to_string(&original).map_err(|source| match source.kind() {
            std::io::ErrorKind::NotFound => FormatError::NotFound { id: id.to_string() },
            _ => io_error(&original, source),
        })?;
        seed_draft(
            self.dir.join("edit").join(format!("{id}.md")),
            Some(id.clone()),
            seed,
        )
    }

    /// Validate the edited draft and store it. An invalid draft is kept and the error returned.
    pub fn finish(&self, draft: &FormatDraft) -> Result<DraftOutcome, FormatError> {
        let source = fs::read_to_string(&draft.path).map_err(|e| io_error(&draft.path, e))?;
        let outcome = if source.trim() == draft.seed.trim() || source.trim().is_empty() {
            DraftOutcome::Unchanged
        } else {
            DraftOutcome::Saved(match &draft.target {
                Some(id) => self.formats.replace(id, &source)?,
                None => self.formats.create(&source)?,
            })
        };
        let _ = fs::remove_file(&draft.path);
        Ok(outcome)
    }
}

/// Write `seed` to `path` unless a draft from an earlier attempt is already there.
fn seed_draft(
    path: PathBuf,
    target: Option<FormatId>,
    seed: String,
) -> Result<FormatDraft, FormatError> {
    if !path.exists() {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| io_error(parent, e))?;
        }
        fs::write(&path, &seed).map_err(|e| io_error(&path, e))?;
    }
    Ok(FormatDraft { path, target, seed })
}

fn io_error(path: &Path, source: std::io::Error) -> FormatError {
    FormatError::Io {
        path: path.display().to_string(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use saidwork_formats::FormatRepository as _;

    use super::*;

    fn setup() -> (tempfile::TempDir, FormatDrafts, Arc<FsFormatRepository>) {
        let tmp = tempfile::tempdir().unwrap();
        let formats = Arc::new(FsFormatRepository::new(tmp.path().join("formats")));
        let drafts = FormatDrafts::new(formats.clone(), tmp.path().join("drafts"));
        (tmp, drafts, formats)
    }

    #[test]
    fn new_draft_saved_unchanged_is_a_no_op() {
        let (_tmp, drafts, formats) = setup();
        let draft = drafts.begin_new().unwrap();
        assert_eq!(drafts.finish(&draft).unwrap(), DraftOutcome::Unchanged);
        assert!(!draft.path.exists());
        assert!(!formats.dir().exists(), "nothing written");
    }

    #[test]
    fn new_draft_is_created_under_its_name() {
        let (_tmp, drafts, formats) = setup();
        let draft = drafts.begin_new().unwrap();
        fs::write(&draft.path, "---\nname: Retro\ndescription: d\n---\nbody").unwrap();
        let DraftOutcome::Saved(f) = drafts.finish(&draft).unwrap() else {
            panic!("expected saved")
        };
        assert_eq!(f.id.as_str(), "retro");
        assert_eq!(formats.get(&f.id).unwrap().template, "body");
        assert!(!draft.path.exists());
    }

    #[test]
    fn invalid_draft_is_kept_and_resumed() {
        let (_tmp, drafts, _formats) = setup();
        let draft = drafts.begin_new().unwrap();
        fs::write(&draft.path, "oops").unwrap();
        assert!(drafts.finish(&draft).is_err());
        let again = drafts.begin_new().unwrap();
        assert_eq!(fs::read_to_string(&again.path).unwrap(), "oops");
    }

    #[test]
    fn edit_replaces_the_existing_format() {
        let (_tmp, drafts, formats) = setup();
        let id = formats
            .create("---\nname: A\ndescription: d\n---\nold")
            .unwrap()
            .id;
        let draft = drafts.begin_edit(&id).unwrap();
        assert_eq!(drafts.finish(&draft).unwrap(), DraftOutcome::Unchanged);

        let draft = drafts.begin_edit(&id).unwrap();
        fs::write(&draft.path, "---\nname: A\ndescription: d\n---\nnew").unwrap();
        assert!(matches!(drafts.finish(&draft), Ok(DraftOutcome::Saved(_))));
        assert_eq!(formats.get(&id).unwrap().template, "new");
        assert!(matches!(
            drafts.begin_edit(&FormatId::new("missing").unwrap()),
            Err(FormatError::NotFound { .. })
        ));
    }
}

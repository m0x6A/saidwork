use std::fs;
use std::path::{Path, PathBuf};

use crate::builtin::BUILTIN_FORMATS;
use crate::domain::{Format, FormatError, FormatId, parse_format};
use crate::ports::FormatRepository;

/// Formats stored as `*.md` files in one directory.
#[derive(Debug, Clone)]
pub struct FsFormatRepository {
    dir: PathBuf,
}

impl FsFormatRepository {
    /// Use `dir` as-is; it does not have to exist yet.
    #[must_use]
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// Create the directory and, if it holds no `*.md` files, write the built-in formats into it.
    ///
    /// Returns the number of files written.
    pub fn ensure_initialised(&self) -> Result<usize, FormatError> {
        fs::create_dir_all(&self.dir).map_err(|source| self.io(source))?;
        if self.markdown_files()?.next().is_some() {
            return Ok(0);
        }
        for (name, contents) in BUILTIN_FORMATS {
            fs::write(self.dir.join(name), contents).map_err(|source| self.io(source))?;
        }
        Ok(BUILTIN_FORMATS.len())
    }

    /// Directory this repository reads from.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn io(&self, source: std::io::Error) -> FormatError {
        FormatError::Io {
            path: self.dir.display().to_string(),
            source,
        }
    }

    fn markdown_files(&self) -> Result<impl Iterator<Item = PathBuf>, FormatError> {
        let entries = fs::read_dir(&self.dir).map_err(|source| self.io(source))?;
        Ok(entries
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.is_file() && p.extension().is_some_and(|e| e == "md")))
    }
}

impl FormatRepository for FsFormatRepository {
    fn list(&self) -> Result<Vec<Format>, FormatError> {
        let mut formats = Vec::new();
        for path in self.markdown_files()? {
            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default();
            let id = FormatId::new(stem)?;
            let source = fs::read_to_string(&path).map_err(|source| FormatError::Io {
                path: path.display().to_string(),
                source,
            })?;
            formats.push(parse_format(id, &source)?);
        }
        formats.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(formats)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initialises_builtins_only_when_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = FsFormatRepository::new(tmp.path().join("formats"));
        assert_eq!(repo.ensure_initialised().unwrap(), BUILTIN_FORMATS.len());
        assert_eq!(
            repo.ensure_initialised().unwrap(),
            0,
            "second run must not overwrite"
        );
        let ids: Vec<_> = repo
            .list()
            .unwrap()
            .into_iter()
            .map(|f| f.id.to_string())
            .collect();
        assert_eq!(ids, ["journal-entry", "meeting-notes", "task-list"]);
    }

    #[test]
    fn lists_user_files_sorted_and_ignores_other_extensions() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("zeta.md"),
            "---\nname: Z\ndescription: z\n---\nz",
        )
        .unwrap();
        fs::write(
            tmp.path().join("alpha.md"),
            "---\nname: A\ndescription: a\n---\na",
        )
        .unwrap();
        fs::write(tmp.path().join("README.txt"), "not a format").unwrap();
        let repo = FsFormatRepository::new(tmp.path());
        let formats = repo.list().unwrap();
        let ids: Vec<_> = formats.iter().map(|f| f.id.as_str()).collect();
        assert_eq!(ids, ["alpha", "zeta"]);
        assert_eq!(repo.get(&FormatId::new("zeta").unwrap()).unwrap().name, "Z");
    }

    #[test]
    fn get_reports_missing_format() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = FsFormatRepository::new(tmp.path());
        let err = repo.get(&FormatId::new("nope").unwrap()).unwrap_err();
        assert!(matches!(err, FormatError::NotFound { .. }));
    }

    #[test]
    fn a_broken_file_fails_the_listing_with_its_id() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("broken.md"), "no frontmatter").unwrap();
        let err = FsFormatRepository::new(tmp.path()).list().unwrap_err();
        assert!(err.to_string().contains("broken"), "{err}");
    }
}

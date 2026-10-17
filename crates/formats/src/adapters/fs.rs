use std::fs;
use std::path::{Path, PathBuf};

use crate::builtin::BUILTIN_FORMATS;
use crate::domain::{Format, FormatError, FormatId, parse_format};
use crate::ports::FormatRepository;

/// Hidden file listing the built-in formats already installed in a formats directory.
const INSTALLED_MANIFEST: &str = ".installed-builtins";

/// What [`FsFormatRepository::ensure_initialised`] did with the built-in formats.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BuiltinSync {
    /// Built-ins that were missing and have been written.
    pub written: Vec<PathBuf>,
    /// Built-ins whose file exists but differs from the shipped version.
    pub customised: Vec<PathBuf>,
}

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

    /// Create the directory and write every built-in format that was never installed.
    ///
    /// Installed built-ins are recorded in a manifest next to the formats, so one the user
    /// deletes stays deleted. Existing files are never overwritten; built-ins whose file
    /// differs from the shipped version are reported so the caller can warn about them.
    pub fn ensure_initialised(&self) -> Result<BuiltinSync, FormatError> {
        fs::create_dir_all(&self.dir).map_err(|source| self.io(source))?;
        let mut installed = self.installed_builtins()?;
        let before = installed.len();
        let mut sync = BuiltinSync::default();
        for (name, contents) in BUILTIN_FORMATS {
            let path = self.dir.join(name);
            match fs::read_to_string(&path) {
                Ok(existing) if existing.trim() == contents.trim() => {}
                Ok(_) => sync.customised.push(path),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    if installed.iter().any(|n| n == name) {
                        continue;
                    }
                    fs::write(&path, contents).map_err(|source| file_io(&path, source))?;
                    sync.written.push(path);
                }
                Err(source) => return Err(file_io(&path, source)),
            }
            if !installed.iter().any(|n| n == name) {
                installed.push((*name).to_owned());
            }
        }
        if installed.len() != before {
            self.write_installed_builtins(&installed)?;
        }
        Ok(sync)
    }

    /// Overwrite the built-in format `id` with the shipped version, recreating it if deleted.
    pub fn restore_builtin(&self, id: &FormatId) -> Result<Format, FormatError> {
        let file = format!("{id}.md");
        let (_, contents) = BUILTIN_FORMATS
            .iter()
            .find(|(name, _)| *name == file)
            .ok_or_else(|| FormatError::NotBuiltin { id: id.to_string() })?;
        self.write(id, contents)?;
        let mut installed = self.installed_builtins()?;
        if !installed.contains(&file) {
            installed.push(file);
            self.write_installed_builtins(&installed)?;
        }
        parse_format(id.clone(), contents)
    }

    fn manifest_path(&self) -> PathBuf {
        self.dir.join(INSTALLED_MANIFEST)
    }

    fn installed_builtins(&self) -> Result<Vec<String>, FormatError> {
        let path = self.manifest_path();
        match fs::read_to_string(&path) {
            Ok(text) => Ok(text
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty() && !l.starts_with('#'))
                .map(str::to_owned)
                .collect()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(source) => Err(file_io(&path, source)),
        }
    }

    fn write_installed_builtins(&self, names: &[String]) -> Result<(), FormatError> {
        let path = self.manifest_path();
        let mut text = String::from(
            "# Built-in formats saidwork has installed here; a deleted one is not reinstalled.\n",
        );
        for name in names {
            text.push_str(name);
            text.push('\n');
        }
        fs::write(&path, text).map_err(|source| file_io(&path, source))
    }

    /// Directory this repository reads from.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// File a format with this id is stored in.
    #[must_use]
    pub fn path_of(&self, id: &FormatId) -> PathBuf {
        self.dir.join(format!("{id}.md"))
    }

    /// Store a new format from its Markdown source.
    ///
    /// The id is derived from the frontmatter `name`; `-2`, `-3`, … is appended when a
    /// format with that id already exists. Nothing is written if the source is invalid.
    pub fn create(&self, source: &str) -> Result<Format, FormatError> {
        let draft = parse_format(FormatId::new("new-format")?, source)?;
        let base = FormatId::new(&draft.name)?;
        let mut id = base.clone();
        let mut n = 2;
        while self.path_of(&id).exists() {
            id = FormatId::new(format!("{base}-{n}"))?;
            n += 1;
        }
        self.write(&id, source)?;
        Ok(Format { id, ..draft })
    }

    /// Overwrite the format `id` with `source`, which is validated first.
    pub fn replace(&self, id: &FormatId, source: &str) -> Result<Format, FormatError> {
        let format = parse_format(id.clone(), source)?;
        self.write(id, source)?;
        Ok(format)
    }

    fn write(&self, id: &FormatId, source: &str) -> Result<(), FormatError> {
        fs::create_dir_all(&self.dir).map_err(|source| self.io(source))?;
        let path = self.path_of(id);
        fs::write(&path, source).map_err(|source| file_io(&path, source))
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

fn file_io(path: &Path, source: std::io::Error) -> FormatError {
    FormatError::Io {
        path: path.display().to_string(),
        source,
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
    fn writes_missing_builtins_and_reports_customised_ones() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = FsFormatRepository::new(tmp.path().join("formats"));
        let sync = repo.ensure_initialised().unwrap();
        assert_eq!(sync.written.len(), BUILTIN_FORMATS.len());
        assert_eq!(sync.customised, Vec::<PathBuf>::new());
        let ids: Vec<_> = repo
            .list()
            .unwrap()
            .into_iter()
            .map(|f| f.id.to_string())
            .collect();
        assert_eq!(
            ids,
            [
                "conversation",
                "journal-entry",
                "meeting-notes",
                "task-list"
            ]
        );

        let custom = "---\nname: Mine\ndescription: d\n---\nmine";
        fs::write(repo.dir().join("task-list.md"), custom).unwrap();
        fs::remove_file(repo.dir().join("conversation.md")).unwrap();
        let sync = repo.ensure_initialised().unwrap();
        assert_eq!(
            sync.written,
            Vec::<PathBuf>::new(),
            "deleted built-ins stay deleted"
        );
        assert_eq!(sync.customised, [repo.dir().join("task-list.md")]);
        assert_eq!(
            fs::read_to_string(repo.dir().join("task-list.md")).unwrap(),
            custom,
            "customised built-ins are never overwritten"
        );
    }

    #[test]
    fn installs_builtins_added_since_the_last_run() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = FsFormatRepository::new(tmp.path());
        // A directory set up before the manifest or `conversation` existed.
        fs::write(
            tmp.path().join("task-list.md"),
            "---\nname: T\ndescription: d\n---\nt",
        )
        .unwrap();
        let sync = repo.ensure_initialised().unwrap();
        assert_eq!(sync.written.len(), BUILTIN_FORMATS.len() - 1);
        assert!(tmp.path().join("conversation.md").exists());
    }

    #[test]
    fn restore_builtin_overwrites_and_recreates() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = FsFormatRepository::new(tmp.path());
        repo.ensure_initialised().unwrap();
        let id = FormatId::new("task-list").unwrap();
        fs::remove_file(repo.path_of(&id)).unwrap();
        repo.ensure_initialised().unwrap();
        assert!(!repo.path_of(&id).exists());

        assert_eq!(repo.restore_builtin(&id).unwrap().name, "Task list");
        let sync = repo.ensure_initialised().unwrap();
        assert_eq!(sync, BuiltinSync::default());
        assert!(matches!(
            repo.restore_builtin(&FormatId::new("mine").unwrap()),
            Err(FormatError::NotBuiltin { .. })
        ));
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
    fn create_derives_the_id_from_the_name_and_avoids_clashes() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = FsFormatRepository::new(tmp.path());
        let src = "---\nname: Stand-up Notes\ndescription: d\n---\nbody";
        assert_eq!(repo.create(src).unwrap().id.as_str(), "stand-up-notes");
        assert_eq!(repo.create(src).unwrap().id.as_str(), "stand-up-notes-2");
        assert_eq!(repo.list().unwrap().len(), 2);
    }

    #[test]
    fn create_and_replace_write_nothing_when_invalid() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = FsFormatRepository::new(tmp.path());
        assert!(repo.create("---\nname: A\n---\nbody").is_err());
        assert_eq!(repo.list().unwrap().len(), 0);

        let id = repo
            .create("---\nname: A\ndescription: d\n---\nbody")
            .unwrap()
            .id;
        assert!(repo.replace(&id, "no frontmatter").is_err());
        assert_eq!(repo.get(&id).unwrap().template, "body");
        repo.replace(&id, "---\nname: A\ndescription: d\n---\nnew")
            .unwrap();
        assert_eq!(repo.get(&id).unwrap().template, "new");
    }

    #[test]
    fn a_broken_file_fails_the_listing_with_its_id() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("broken.md"), "no frontmatter").unwrap();
        let err = FsFormatRepository::new(tmp.path()).list().unwrap_err();
        assert!(err.to_string().contains("broken"), "{err}");
    }
}

use std::path::{Path, PathBuf};

use directories::{ProjectDirs, UserDirs};

/// Errors while resolving the application directories.
#[derive(Debug, thiserror::Error)]
pub enum PathsError {
    /// The platform has no notion of a home directory for this user.
    #[error("could not determine a home directory for the current user")]
    NoHomeDirectory,
}

/// Well-known locations the application reads from and writes to.
///
/// Resolved from the platform conventions (XDG on Linux, `Application Support`
/// on macOS, `AppData` on Windows) so the rest of the code never hard-codes paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppPaths {
    /// Directory holding `saidwork.toml` and the user's format templates.
    pub config_dir: PathBuf,
    /// Directory for application data (sessions, logs).
    pub data_dir: PathBuf,
    /// Default directory where finished notes are written.
    pub default_notes_dir: PathBuf,
}

impl AppPaths {
    /// Resolve the paths from the platform conventions.
    pub fn discover() -> Result<Self, PathsError> {
        let project =
            ProjectDirs::from("nu", "xdr", "saidwork").ok_or(PathsError::NoHomeDirectory)?;
        let user = UserDirs::new().ok_or(PathsError::NoHomeDirectory)?;
        let documents = user
            .document_dir()
            .map_or_else(|| project.data_dir().join("notes"), |d| d.join("saidwork"));
        Ok(Self {
            config_dir: project.config_dir().to_path_buf(),
            data_dir: project.data_dir().to_path_buf(),
            default_notes_dir: documents,
        })
    }

    /// Paths rooted under a single directory; for tests and `--home` style overrides.
    #[must_use]
    pub fn rooted_at(root: &Path) -> Self {
        Self {
            config_dir: root.join("config"),
            data_dir: root.join("data"),
            default_notes_dir: root.join("notes"),
        }
    }

    /// Main configuration file.
    #[must_use]
    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join("saidwork.toml")
    }

    /// Directory holding the user's Markdown format templates.
    #[must_use]
    pub fn formats_dir(&self) -> PathBuf {
        self.config_dir.join("formats")
    }

    /// Directory where raw recordings and transcripts of each session are kept.
    #[must_use]
    pub fn sessions_dir(&self) -> PathBuf {
        self.data_dir.join("sessions")
    }

    /// Log file; the TUI owns the terminal so logs never go to stderr.
    #[must_use]
    pub fn log_file(&self) -> PathBuf {
        self.data_dir.join("saidwork.log")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rooted_paths_derive_everything_from_root() {
        let paths = AppPaths::rooted_at(Path::new("/tmp/x"));
        assert_eq!(
            paths.config_file(),
            PathBuf::from("/tmp/x/config/saidwork.toml")
        );
        assert_eq!(paths.formats_dir(), PathBuf::from("/tmp/x/config/formats"));
        assert_eq!(paths.sessions_dir(), PathBuf::from("/tmp/x/data/sessions"));
        assert_eq!(paths.log_file(), PathBuf::from("/tmp/x/data/saidwork.log"));
        assert_eq!(paths.default_notes_dir, PathBuf::from("/tmp/x/notes"));
    }

    #[test]
    fn discover_finds_a_config_dir_named_after_the_app() {
        let paths = AppPaths::discover().expect("home dir available in tests");
        assert!(paths.config_dir.ends_with("saidwork"));
    }
}

/// Expand a leading `~` or `~/` to the user's home directory.
///
/// Paths without a leading tilde are returned unchanged; if no home directory is
/// known the tilde is left in place.
#[must_use]
pub fn expand_tilde(path: &Path) -> PathBuf {
    let Some(rest) = path.to_str().and_then(|s| s.strip_prefix('~')) else {
        return path.to_path_buf();
    };
    if !(rest.is_empty() || rest.starts_with('/') || rest.starts_with('\\')) {
        return path.to_path_buf();
    }
    match directories::BaseDirs::new() {
        Some(base) => base.home_dir().join(rest.trim_start_matches(['/', '\\'])),
        None => path.to_path_buf(),
    }
}

#[cfg(test)]
mod tilde_tests {
    use super::*;

    #[test]
    fn expands_only_leading_tilde() {
        let home = directories::BaseDirs::new()
            .unwrap()
            .home_dir()
            .to_path_buf();
        assert_eq!(expand_tilde(Path::new("~/notes")), home.join("notes"));
        assert_eq!(expand_tilde(Path::new("~")), home);
        assert_eq!(expand_tilde(Path::new("/abs/~x")), PathBuf::from("/abs/~x"));
        assert_eq!(expand_tilde(Path::new("~user/x")), PathBuf::from("~user/x"));
    }
}

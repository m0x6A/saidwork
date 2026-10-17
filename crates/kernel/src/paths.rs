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
    /// Directories searched, in order, for files shipped with saidwork (such as the
    /// transcription server script); see [`AppPaths::find_resource`].
    pub resource_dirs: Vec<PathBuf>,
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
        let data_dir = project.data_dir().to_path_buf();
        Ok(Self {
            config_dir: project.config_dir().to_path_buf(),
            resource_dirs: resource_dirs(Some(&data_dir)),
            data_dir,
            default_notes_dir: documents,
        })
    }

    /// Paths rooted under a single directory; for tests and `--home` style overrides.
    ///
    /// Resources are looked up under `<root>/share` first, then in the system locations.
    #[must_use]
    pub fn rooted_at(root: &Path) -> Self {
        let mut resource_dirs = vec![root.join("share")];
        resource_dirs.extend(self::resource_dirs(None));
        Self {
            config_dir: root.join("config"),
            data_dir: root.join("data"),
            default_notes_dir: root.join("notes"),
            resource_dirs,
        }
    }

    /// First existing `<dir>/<relative>` over [`AppPaths::resource_dirs`].
    #[must_use]
    pub fn find_resource(&self, relative: &Path) -> Option<PathBuf> {
        self.resource_dirs
            .iter()
            .map(|dir| dir.join(relative))
            .find(|p| p.exists())
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

    /// Directory for format drafts being edited, so a rejected edit is never lost.
    #[must_use]
    pub fn drafts_dir(&self) -> PathBuf {
        self.data_dir.join("drafts")
    }

    /// Log file; the TUI owns the terminal so logs never go to stderr.
    #[must_use]
    pub fn log_file(&self) -> PathBuf {
        self.data_dir.join("saidwork.log")
    }
}

/// Standard places for shipped resources, most specific first:
///
/// 1. `$SAIDWORK_SHARE_DIR`, if set
/// 2. the user data directory (`~/.local/share/saidwork`, where `install.sh` puts them)
/// 3. `<exe>/../share/saidwork`, which covers `/usr`, `/usr/local`, `~/.local`, any
///    `PREFIX` and an unpacked release tarball
/// 4. `saidwork` under each of `$XDG_DATA_DIRS` (default `/usr/local/share:/usr/share`)
/// 5. the source checkout's `tools/`, so `cargo run` and `cargo install --path` work
fn resource_dirs(data_dir: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("SAIDWORK_SHARE_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .into_iter()
        .collect();
    dirs.extend(data_dir.map(Path::to_path_buf));
    if let Some(prefix) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent()?.parent().map(Path::to_path_buf))
    {
        dirs.push(prefix.join("share").join("saidwork"));
    }
    let xdg = std::env::var("XDG_DATA_DIRS")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".to_owned());
    dirs.extend(
        xdg.split(':')
            .filter(|d| !d.is_empty())
            .map(|d| Path::new(d).join("saidwork")),
    );
    // crates/kernel -> repository root; only where that checkout exists, so release
    // binaries built elsewhere do not list a path from the build machine.
    if let Some(root) = Path::new(env!("CARGO_MANIFEST_DIR")).ancestors().nth(2) {
        let tools = root.join("tools");
        if tools.is_dir() {
            dirs.push(tools);
        }
    }
    let mut seen = std::collections::HashSet::new();
    dirs.retain(|d| seen.insert(d.clone()));
    dirs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resources_are_found_in_order_and_checkout_is_a_fallback() {
        let tmp = tempfile::tempdir().unwrap();
        let mut paths = AppPaths::rooted_at(tmp.path());
        assert_eq!(paths.resource_dirs[0], tmp.path().join("share"));
        let rel = Path::new("pianissimo-server/server.py");
        let from_checkout = paths.find_resource(rel).expect("checkout tools/ fallback");
        assert!(from_checkout.ends_with("tools/pianissimo-server/server.py"));

        let ours = tmp.path().join("share").join(rel);
        std::fs::create_dir_all(ours.parent().unwrap()).unwrap();
        std::fs::write(&ours, "").unwrap();
        assert_eq!(paths.find_resource(rel), Some(ours));
        assert_eq!(paths.find_resource(Path::new("nope.txt")), None);

        paths.resource_dirs.clear();
        assert_eq!(paths.find_resource(rel), None);
    }

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

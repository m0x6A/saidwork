use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use tokio::process::{Child, Command};

use crate::domain::{LaunchSpec, MissingRequirement, ProviderError};
use crate::ports::{ManagedProcess, ProcessLauncher};

/// Launcher backed by `tokio::process`.
///
/// On Unix the child is put in its own process group so that helpers it spawns
/// (e.g. a shell wrapper's children) are terminated together with it. On Windows the
/// whole process tree is killed with `taskkill /T`.
#[derive(Debug, Default, Clone, Copy)]
pub struct TokioLauncher {
    /// How long to wait after a graceful stop before killing.
    grace: Duration,
}

impl TokioLauncher {
    #[must_use]
    pub fn new() -> Self {
        Self {
            grace: Duration::from_secs(5),
        }
    }

    #[must_use]
    pub fn with_grace_period(mut self, grace: Duration) -> Self {
        self.grace = grace;
        self
    }
}

/// Where `command` would be run from: itself if it names a path, else the first
/// executable match on `PATH`.
#[must_use]
fn find_command(command: &str) -> Option<PathBuf> {
    if command.contains('/') || command.contains(std::path::MAIN_SEPARATOR) {
        return candidates(Path::new(command)).find(|p| is_executable(p));
    }
    std::env::split_paths(&std::env::var_os("PATH")?)
        .flat_map(|dir| candidates(&dir.join(command)))
        .find(|p| is_executable(p))
}

/// `path` itself and, on Windows, `path` with each `%PATHEXT%` extension (`uv` → `uv.exe`).
fn candidates(path: &Path) -> impl Iterator<Item = PathBuf> + use<> {
    #[cfg(windows)]
    let extensions: Vec<PathBuf> = std::env::var("PATHEXT")
        .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_owned())
        .split(';')
        .filter(|ext| !ext.is_empty())
        .map(|ext| {
            let mut name = path.as_os_str().to_owned();
            name.push(ext);
            PathBuf::from(name)
        })
        .collect();
    #[cfg(not(windows))]
    let extensions: Vec<PathBuf> = Vec::new();
    std::iter::once(path.to_path_buf()).chain(extensions)
}

fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        path.metadata()
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

#[async_trait]
impl ProcessLauncher for TokioLauncher {
    fn missing(&self, spec: &LaunchSpec) -> Vec<MissingRequirement> {
        let command = find_command(&spec.command)
            .is_none()
            .then(|| MissingRequirement::Command(spec.command.clone()));
        command
            .into_iter()
            .chain(
                spec.required_files
                    .iter()
                    .filter(|f| !f.exists())
                    .map(|f| MissingRequirement::File(f.clone())),
            )
            .collect()
    }

    async fn spawn(&self, spec: &LaunchSpec) -> Result<Box<dyn ManagedProcess>, ProviderError> {
        let mut cmd = Command::new(&spec.command);
        cmd.args(&spec.args)
            .envs(&spec.env)
            .stdin(Stdio::null())
            .kill_on_drop(true);
        match &spec.log_file {
            Some(path) => {
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent).map_err(|source| ProviderError::Spawn {
                        command: spec.command.clone(),
                        source,
                    })?;
                }
                let file = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)
                    .map_err(|source| ProviderError::Spawn {
                        command: spec.command.clone(),
                        source,
                    })?;
                let err = file.try_clone().map_err(|source| ProviderError::Spawn {
                    command: spec.command.clone(),
                    source,
                })?;
                cmd.stdout(Stdio::from(file)).stderr(Stdio::from(err));
            }
            None => {
                cmd.stdout(Stdio::null()).stderr(Stdio::null());
            }
        }
        #[cfg(unix)]
        cmd.process_group(0);
        // CREATE_NO_WINDOW: console servers would otherwise flash up a window of their own.
        #[cfg(windows)]
        cmd.creation_flags(0x0800_0000);
        let child = cmd.spawn().map_err(|source| ProviderError::Spawn {
            command: spec.command.clone(),
            source,
        })?;
        tracing::debug!(command = %spec.command, pid = child.id(), "spawned provider process");
        Ok(Box::new(TokioProcess {
            child,
            grace: self.grace,
        }))
    }
}

#[derive(Debug)]
struct TokioProcess {
    child: Child,
    grace: Duration,
}

impl TokioProcess {
    #[cfg(unix)]
    fn signal_group(&self, signal: nix::sys::signal::Signal) -> Result<(), ProviderError> {
        use nix::sys::signal::killpg;
        use nix::unistd::Pid;
        let Some(pid) = self.child.id() else {
            return Ok(());
        };
        let pid = i32::try_from(pid).map_err(|e| ProviderError::Terminate(e.to_string()))?;
        match killpg(Pid::from_raw(pid), signal) {
            Ok(()) | Err(nix::errno::Errno::ESRCH) => Ok(()),
            Err(e) => Err(ProviderError::Terminate(e.to_string())),
        }
    }
}

#[async_trait]
impl ManagedProcess for TokioProcess {
    fn has_exited(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(Some(_)))
    }

    async fn terminate(&mut self) -> Result<(), ProviderError> {
        if self.has_exited() {
            return Ok(());
        }
        #[cfg(unix)]
        {
            self.signal_group(nix::sys::signal::Signal::SIGTERM)?;
            if tokio::time::timeout(self.grace, self.child.wait())
                .await
                .is_ok()
            {
                return Ok(());
            }
            self.signal_group(nix::sys::signal::Signal::SIGKILL)?;
        }
        // Windowless console processes ignore a polite `taskkill`, so the tree is killed
        // outright; the direct child is killed as well if that did not take it down.
        #[cfg(windows)]
        {
            if let Some(pid) = self.child.id() {
                let _ = Command::new("taskkill")
                    .args(["/T", "/F", "/PID", &pid.to_string()])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .await;
            }
            if tokio::time::timeout(self.grace, self.child.wait())
                .await
                .is_ok()
            {
                return Ok(());
            }
        }
        #[cfg(not(unix))]
        {
            self.child
                .kill()
                .await
                .map_err(|e| ProviderError::Terminate(e.to_string()))?;
        }
        self.child
            .wait()
            .await
            .map(|_| ())
            .map_err(|e| ProviderError::Terminate(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The platform shell running `unix` (`sh -c`) or `windows` (`cmd /C`).
    fn shell(unix: &str, windows: &str) -> LaunchSpec {
        let (command, flag, script) = if cfg!(windows) {
            ("cmd", "/C", windows)
        } else {
            ("sh", "-c", unix)
        };
        let mut spec = LaunchSpec::new(command);
        spec.args = vec![flag.into(), script.into()];
        spec
    }

    #[tokio::test]
    async fn spawns_and_terminates_a_process_group() {
        let spec = shell("sleep 30 & wait", "ping -n 30 127.0.0.1 > NUL");
        let launcher = TokioLauncher::new().with_grace_period(Duration::from_secs(2));
        let mut process = launcher.spawn(&spec).await.unwrap();
        assert!(!process.has_exited());
        process.terminate().await.unwrap();
        assert!(process.has_exited());
    }

    #[tokio::test]
    async fn detects_a_process_that_exits_on_its_own() {
        let spec = shell("exit 0", "exit 0");
        let mut process = TokioLauncher::new().spawn(&spec).await.unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(process.has_exited());
        process.terminate().await.unwrap();
    }

    #[tokio::test]
    async fn missing_command_is_a_spawn_error() {
        let spec = LaunchSpec::new("definitely-not-a-real-command-xyz");
        let err = TokioLauncher::new().spawn(&spec).await.unwrap_err();
        assert!(matches!(err, ProviderError::Spawn { .. }));
    }

    #[test]
    fn missing_reports_absent_commands_and_files() {
        let launcher = TokioLauncher::new();
        let shell = if cfg!(windows) { "cmd" } else { "sh" };
        assert_eq!(launcher.missing(&LaunchSpec::new(shell)), []);
        assert!(find_command(shell).is_some());

        let mut spec = LaunchSpec::new("definitely-not-a-real-command-xyz");
        spec.required_files = vec![PathBuf::from("/definitely/not/here.py")];
        assert_eq!(
            launcher.missing(&spec),
            [
                MissingRequirement::Command("definitely-not-a-real-command-xyz".into()),
                MissingRequirement::File("/definitely/not/here.py".into()),
            ]
        );
        assert_eq!(
            launcher.missing(&LaunchSpec::new("/definitely/not/a/binary")),
            [MissingRequirement::Command(
                "/definitely/not/a/binary".into()
            )]
        );
    }

    #[tokio::test]
    async fn child_output_goes_to_the_log_file() {
        let tmp = tempfile::tempdir().unwrap();
        let log = tmp.path().join("logs").join("p.log");
        let mut spec = shell(
            "echo hello-from-child; echo oops >&2",
            "echo hello-from-child & echo oops 1>&2",
        );
        spec.log_file = Some(log.clone());
        let mut process = TokioLauncher::new().spawn(&spec).await.unwrap();
        for _ in 0..50 {
            if process.has_exited() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        process.terminate().await.unwrap();
        let contents = std::fs::read_to_string(log).unwrap();
        assert!(contents.contains("hello-from-child"));
        assert!(contents.contains("oops"));
    }
}

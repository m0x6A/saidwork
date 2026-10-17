//! Launching the user's text editor on a file.

use std::path::Path;
use std::process::ExitStatus;

/// Editor used when neither `$VISUAL` nor `$EDITOR` is set.
#[cfg(not(windows))]
pub const FALLBACK_EDITOR: &str = "nvim";
/// Editor used when neither `%VISUAL%` nor `%EDITOR%` is set.
#[cfg(windows)]
pub const FALLBACK_EDITOR: &str = "notepad";

/// The user's editor command: `$VISUAL`, then `$EDITOR`, then [`FALLBACK_EDITOR`].
#[must_use]
pub fn resolve_editor() -> String {
    pick_editor(|var| std::env::var(var).ok())
}

fn pick_editor(lookup: impl Fn(&str) -> Option<String>) -> String {
    ["VISUAL", "EDITOR"]
        .into_iter()
        .filter_map(lookup)
        .map(|e| e.trim().to_owned())
        .find(|e| !e.is_empty())
        .unwrap_or_else(|| FALLBACK_EDITOR.to_owned())
}

/// Open `path` in the user's editor and wait for it to exit.
///
/// The editor command goes through the shell (`sh`, or `cmd` on Windows) like git does, so
/// values with arguments such as `code --wait` work. The child inherits the terminal;
/// callers owning the screen (the TUI) must release it first.
pub async fn open_in_editor(path: &Path) -> std::io::Result<ExitStatus> {
    editor_command(&resolve_editor(), path).status().await
}

#[cfg(not(windows))]
fn editor_command(editor: &str, path: &Path) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new("sh");
    cmd.arg("-c")
        .arg(format!("{editor} \"$1\""))
        .arg("sh")
        .arg(path);
    cmd
}

#[cfg(windows)]
fn editor_command(editor: &str, path: &Path) -> tokio::process::Command {
    // `/S` makes cmd strip only the outer quotes, so the quoted path survives intact.
    let mut cmd = tokio::process::Command::new("cmd");
    cmd.raw_arg(format!("/S /C \"{editor} \"{}\"\"", path.display()));
    cmd
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefers_visual_then_editor_then_fallback() {
        let env = |pairs: &'static [(&str, &str)]| {
            move |var: &str| {
                pairs
                    .iter()
                    .find(|(k, _)| *k == var)
                    .map(|(_, v)| (*v).to_owned())
            }
        };
        assert_eq!(
            pick_editor(env(&[("VISUAL", "hx"), ("EDITOR", "vi")])),
            "hx"
        );
        assert_eq!(pick_editor(env(&[("VISUAL", " "), ("EDITOR", "vi")])), "vi");
        assert_eq!(pick_editor(env(&[])), FALLBACK_EDITOR);
    }
}

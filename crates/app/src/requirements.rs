//! What saidwork needs from the machine and how to install it.
//!
//! The catalogue is data rather than prose so the same entries drive the hints printed
//! by `saidwork doctor`, the start-up check, and any future `saidwork setup` that runs
//! the install commands itself.

use std::path::Path;

/// Linux distribution family, used to pick the right install command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Distro {
    /// Fedora, RHEL, CentOS and friends (dnf).
    Fedora,
    /// Debian, Ubuntu and friends (apt).
    Debian,
    /// Arch and friends (pacman).
    Arch,
    /// openSUSE and SLE (zypper).
    Suse,
    /// Windows (winget).
    Windows,
    Other,
}

impl Distro {
    /// [`Distro::Windows`] on Windows, else read `/etc/os-release`; anything unreadable or
    /// unknown is [`Distro::Other`].
    #[must_use]
    pub fn detect() -> Self {
        if cfg!(windows) {
            return Self::Windows;
        }
        std::fs::read_to_string("/etc/os-release")
            .map_or(Self::Other, |text| Self::from_os_release(&text))
    }

    /// Classify an `os-release` file by its `ID` and `ID_LIKE` fields.
    #[must_use]
    pub fn from_os_release(text: &str) -> Self {
        let ids: Vec<String> = text
            .lines()
            .filter_map(|l| l.strip_prefix("ID=").or_else(|| l.strip_prefix("ID_LIKE=")))
            .flat_map(|v| v.trim_matches('"').split_whitespace())
            .map(str::to_ascii_lowercase)
            .collect();
        let any = |names: &[&str]| ids.iter().any(|id| names.contains(&id.as_str()));
        if any(&["fedora", "rhel", "centos"]) {
            Self::Fedora
        } else if any(&["debian", "ubuntu"]) {
            Self::Debian
        } else if any(&["arch"]) {
            Self::Arch
        } else if any(&["suse", "opensuse"]) || ids.iter().any(|id| id.starts_with("opensuse")) {
            Self::Suse
        } else {
            Self::Other
        }
    }
}

/// An external program or library saidwork can use, with install instructions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dependency {
    /// Executable or package name, as it appears in a `launch.command`.
    pub id: &'static str,
    /// What saidwork uses it for.
    pub purpose: &'static str,
    /// Install commands for specific distribution families.
    install: &'static [(Distro, &'static str)],
    /// Instructions for every other system.
    fallback: &'static str,
    /// Anything worth knowing before the first run, such as a large download.
    pub note: Option<&'static str>,
}

impl Dependency {
    /// The install instructions for `distro`.
    #[must_use]
    pub fn install_steps(&self, distro: Distro) -> &'static str {
        self.install
            .iter()
            .find(|(d, _)| *d == distro)
            .map_or(self.fallback, |(_, steps)| steps)
    }
}

/// The sound server; libpulse needs one to talk to (PipeWire's pulse layer or PulseAudio).
pub const AUDIO_SERVER: Dependency = Dependency {
    id: "pipewire-pulse",
    purpose: "sound server that microphone and system audio are captured from",
    install: &[
        (
            Distro::Fedora,
            "sudo dnf install pipewire-pulseaudio && systemctl --user enable --now pipewire-pulse.socket",
        ),
        (
            Distro::Debian,
            "sudo apt install pipewire-pulse && systemctl --user enable --now pipewire-pulse.socket",
        ),
        (
            Distro::Arch,
            "sudo pacman -S pipewire-pulse && systemctl --user enable --now pipewire-pulse.socket",
        ),
        (
            Distro::Suse,
            "sudo zypper install pipewire-pulseaudio && systemctl --user enable --now pipewire-pulse.socket",
        ),
        (
            Distro::Windows,
            "make sure the Windows Audio service is running and turn on Settings › Privacy & \
             security › Microphone › \"Let desktop apps access your microphone\"",
        ),
    ],
    fallback: "install and start PipeWire with its PulseAudio layer (pipewire-pulse), or PulseAudio",
    note: None,
};

/// Root certificates; without them no HTTP client can be built, not even for localhost.
pub const CA_CERTIFICATES: Dependency = Dependency {
    id: "ca-certificates",
    purpose: "root certificates for HTTPS",
    install: &[
        (Distro::Fedora, "sudo dnf install ca-certificates"),
        (Distro::Debian, "sudo apt install ca-certificates"),
        (Distro::Arch, "sudo pacman -S ca-certificates"),
        (Distro::Suse, "sudo zypper install ca-certificates"),
        (
            Distro::Windows,
            "saidwork uses the Windows certificate store; run Windows Update to refresh it",
        ),
    ],
    fallback: "install your distribution's ca-certificates package",
    note: None,
};

/// Programs that the example configurations launch.
pub const KNOWN_COMMANDS: &[Dependency] = &[
    Dependency {
        id: "uv",
        purpose: "runs the bundled Pianissimo transcription server in its own Python environment",
        install: &[
            (Distro::Fedora, "sudo dnf install uv"),
            (Distro::Arch, "sudo pacman -S uv"),
            (Distro::Windows, "winget install astral-sh.uv"),
        ],
        fallback: "curl -LsSf https://astral.sh/uv/install.sh | sh",
        note: Some("the first start downloads the Pianissimo model (about 660 MB)"),
    },
    Dependency {
        id: "llama-server",
        purpose: "serves a local GGUF model (llama.cpp) for rewriting",
        install: &[
            (
                Distro::Fedora,
                "download a release from https://github.com/ggml-org/llama.cpp/releases and put \
                 llama-server on PATH (`sudo dnf install llama-cpp` also works, but may be too \
                 old for recent models)",
            ),
            (Distro::Windows, "winget install ggml.llamacpp"),
        ],
        fallback: "download a release from https://github.com/ggml-org/llama.cpp/releases (or \
                   build it) and put llama-server on PATH",
        note: Some("with `-hf`, the first start downloads the model from Hugging Face"),
    },
    Dependency {
        id: "whisper-server",
        purpose: "serves a local Whisper model (whisper.cpp) for transcription",
        install: &[(
            Distro::Windows,
            "download whisper-bin-x64.zip from https://github.com/ggml-org/whisper.cpp/releases, \
             put the folder with whisper-server.exe on PATH and download a ggml model from \
             https://huggingface.co/ggerganov/whisper.cpp",
        )],
        fallback: "git clone https://github.com/ggml-org/whisper.cpp && cd whisper.cpp && \
                   cmake -B build && cmake --build build -j --config Release, then put \
                   build/bin/whisper-server on PATH and download a model with \
                   models/download-ggml-model.sh",
        note: None,
    },
    Dependency {
        id: "ollama",
        purpose: "serves local models through Ollama",
        install: &[
            (Distro::Fedora, "sudo dnf install ollama"),
            (Distro::Arch, "sudo pacman -S ollama"),
            (Distro::Windows, "winget install Ollama.Ollama"),
        ],
        fallback: "curl -fsSL https://ollama.com/install.sh | sh",
        note: Some("pull the model once with `ollama pull <model>`"),
    },
];

/// The catalogue entry for a launch command, matched on its file name so
/// `~/bin/llama-server` and `llama-server.exe` find the `llama-server` entry.
#[must_use]
pub fn known_command(command: &str) -> Option<&'static Dependency> {
    let file_name = Path::new(command).file_name()?.to_str()?;
    let file_name = file_name.strip_suffix(".exe").unwrap_or(file_name);
    KNOWN_COMMANDS.iter().find(|d| d.id == file_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_distro_families() {
        assert_eq!(
            Distro::from_os_release("NAME=\"Fedora Linux\"\nID=fedora\n"),
            Distro::Fedora
        );
        assert_eq!(
            Distro::from_os_release("ID=almalinux\nID_LIKE=\"rhel centos fedora\"\n"),
            Distro::Fedora
        );
        assert_eq!(
            Distro::from_os_release("ID=pop\nID_LIKE=\"ubuntu debian\"\n"),
            Distro::Debian
        );
        assert_eq!(
            Distro::from_os_release("ID=endeavouros\nID_LIKE=arch\n"),
            Distro::Arch
        );
        assert_eq!(
            Distro::from_os_release("ID=\"opensuse-tumbleweed\"\n"),
            Distro::Suse
        );
        assert_eq!(Distro::from_os_release("ID=nixos\n"), Distro::Other);
    }

    #[test]
    fn known_commands_match_on_file_name_and_pick_the_distro_steps() {
        let uv = known_command("/home/me/.local/bin/uv").unwrap();
        assert_eq!(uv.install_steps(Distro::Fedora), "sudo dnf install uv");
        assert!(uv.install_steps(Distro::Debian).contains("astral.sh"));
        assert!(known_command("llama-server").is_some());
        assert!(known_command("llama-server.exe").is_some());
        assert_eq!(
            uv.install_steps(Distro::Windows),
            "winget install astral-sh.uv"
        );
        assert!(known_command("my-own-wrapper.sh").is_none());
    }
}

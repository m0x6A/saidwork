# Installing saidwork

saidwork itself is one binary. What else you need depends on where speech is
transcribed and notes are rewritten: on this machine (local servers that saidwork
starts for you) or by a hosted API. This guide covers installing saidwork, picking a
configuration, installing what that configuration needs, and checking the result with
`saidwork doctor`.

- [1. Install saidwork](#1-install-saidwork)
- [2. Pick a configuration](#2-pick-a-configuration)
- [3. Install what the configuration needs](#3-install-what-the-configuration-needs)
- [4. Check everything with `saidwork doctor`](#4-check-everything-with-saidwork-doctor)
- [Where things live](#where-things-live)
- [Launch blocks](#launch-blocks)
- [Troubleshooting](#troubleshooting)
- [Uninstalling](#uninstalling)

## 1. Install saidwork

saidwork runs on x86_64 Linux. It records through libpulse, so it needs a sound server:
PipeWire with its PulseAudio layer (the default on current Fedora, Ubuntu and most
desktops) or PulseAudio itself.

Download the file for your system from the
[releases page](https://github.com/m0x6A/saidwork/releases). The packages declare
libpulse as a dependency, so the package manager installs it along with saidwork.

**Fedora, RHEL, openSUSE**

```sh
sudo dnf install ./saidwork-*.x86_64.rpm       # openSUSE: sudo zypper install ./saidwork-*.rpm
```

**Debian, Ubuntu**

```sh
sudo apt install ./saidwork_*_amd64.deb
```

**Any other distribution (tarball)**

```sh
tar xzf saidwork-v*-x86_64-linux.tar.gz
cd saidwork-v*-x86_64-linux
./install.sh              # to ~/.local; `sudo ./install.sh` installs to /usr/local
```

`install.sh` installs libpulse with dnf, apt, pacman or zypper when it is missing, then
copies `bin/saidwork` and `share/saidwork/` under the prefix. `PREFIX=/opt/saidwork`
changes the destination and `SKIP_DEPS=1` skips the package manager. You can also run
`bin/saidwork` straight from the unpacked directory.

**From source**

```sh
sudo dnf install rustup pulseaudio-libs-devel   # Debian/Ubuntu: libpulse-dev pkg-config
rustup-init -y && source ~/.cargo/env
cargo install --path crates/saidwork
```

A binary built this way finds the bundled transcription server in the checkout's
`tools/` directory, so keep the checkout where it is (or set `SAIDWORK_SHARE_DIR`).

## 2. Pick a configuration

saidwork ships three presets:

```console
$ saidwork config presets
local            Swedish, fully local: Pianissimo (uv) + Gemma 4 on llama-server
whisper-ollama   any language, fully local: whisper.cpp server + Ollama
openai           hosted: OpenAI transcription and chat (needs OPENAI_API_KEY)
```

Write one to `~/.config/saidwork/saidwork.toml`, together with the built-in note
formats:

```sh
saidwork config init                          # the "local" preset
saidwork config init --preset whisper-ollama
saidwork config init --preset openai --force  # replace an existing file
```

The files are commented, and you can mix them. Transcriber and rewriter are independent
providers, so a local transcriber with a hosted rewriter is just two
`[providers.*]` tables. The packages also install the presets under
`/usr/share/doc/saidwork/examples/`, and the tarball has them in `examples/`.

## 3. Install what the configuration needs

You don't need to start any of these servers yourself. When a provider has a
`[providers.<name>.launch]` block, saidwork starts the server the first time it is
needed and stops it again on exit. Only the programs and models have to be installed.

### `local`: Pianissimo + llama.cpp

| Needs | Install | First start |
|---|---|---|
| [`uv`](https://docs.astral.sh/uv/) | Fedora `sudo dnf install uv`, Arch `sudo pacman -S uv`, elsewhere `curl -LsSf https://astral.sh/uv/install.sh \| sh` | creates a Python environment and downloads the Pianissimo model (~660 MB) |
| `llama-server` from [llama.cpp](https://github.com/ggml-org/llama.cpp) | a release from [github.com/ggml-org/llama.cpp/releases](https://github.com/ggml-org/llama.cpp/releases) on your `PATH`, or build it | downloads Gemma 4 12B Q4_0 (~7 GB) into `~/.cache/llama.cpp` |

Pianissimo is a Swedish speech-to-text model. It runs on the CPU, and saidwork ships
the small server that wraps it (`pianissimo-server/server.py`).

Pick the llama.cpp build that matches your GPU: CUDA for NVIDIA, ROCm/HIP or Vulkan for
AMD, or the CPU build. Fedora's `llama-cpp` package works but can lag
behind newer models. On a machine without a usable GPU, remove `"-ngl", "999"` from the
`args` so the model runs on the CPU (it will be slow).

The first start downloads the models, so `startup_timeout_secs` is generous (600 s for
Pianissimo, 1800 s for Gemma). To download them ahead of time, run
`saidwork providers start pianissimo` and `saidwork providers start gemma-local`.

### `whisper-ollama`: whisper.cpp + Ollama

**whisper-server** (whisper.cpp) has no distribution package with the server, so build it:

```sh
git clone https://github.com/ggml-org/whisper.cpp
cd whisper.cpp
cmake -B build             # add -DGGML_CUDA=1 / -DGGML_HIP=1 / -DGGML_VULKAN=1 for a GPU
cmake --build build -j --config Release
install -Dm755 build/bin/whisper-server ~/.local/bin/whisper-server
sh models/download-ggml-model.sh large-v3-turbo
install -Dm644 models/ggml-large-v3-turbo.bin ~/.local/share/whisper/ggml-large-v3-turbo.bin
```

**Ollama**: `sudo dnf install ollama` (Fedora), `sudo pacman -S ollama` (Arch), or
`curl -fsSL https://ollama.com/install.sh | sh`. Then pull the model once:

```sh
ollama pull qwen3:8b
```

### `openai`: hosted

Nothing to install. Export the key in the shell that starts saidwork, for example in
`~/.bashrc` or `~/.zshrc`:

```sh
export OPENAI_API_KEY=sk-...
```

Other OpenAI-compatible services (Groq, OpenRouter, vLLM, LM Studio, ...) work the same
way with their own `base_url`, `model` and `api_key_env`.

## 4. Check everything with `saidwork doctor`

`saidwork doctor` checks the configuration, the sound server, and each provider the
pipeline uses. For a provider, that means whether it answers, or, if not, whether its
launch command and the files it needs are installed. Every problem comes with the fix
for your distribution:

```console
$ saidwork doctor
[ok  ] config: /home/me/.config/saidwork/saidwork.toml
[ok  ] audio: 7 capture devices (`saidwork devices` lists them)
[ok  ] transcriber `pianissimo`: not running; started on demand with `uv`; the first start downloads the Pianissimo model (about 660 MB)
[FAIL] rewriter `gemma-local`: `llama-server` is not installed (not found on PATH)
       fix: install llama-server (serves a local GGUF model (llama.cpp) for rewriting): download a release from https://github.com/ggml-org/llama.cpp/releases and put llama-server on PATH
error: 1 problem(s) found; the install guide is at https://github.com/m0x6A/saidwork/blob/main/docs/INSTALL.md
```

It exits non-zero when anything blocks recording, so it also works in scripts. The same
checks run when saidwork starts. The TUI lists the problems in its log, and `record`,
`transcribe` and `rewrite` stop before doing any work. Once `doctor` is clean:

```sh
saidwork devices            # microphones and system-audio monitors
saidwork                    # the TUI: r record/stop, ↑/↓ format, q quit
```

## Where things live

| What | Default |
|---|---|
| Configuration | `~/.config/saidwork/saidwork.toml` (`--config` or `SAIDWORK_CONFIG` to override) |
| Note formats | `~/.config/saidwork/formats/` |
| Notes | `~/Documents/saidwork/` |
| Recordings, transcripts, drafts | `~/.local/share/saidwork/` |
| Log | `~/.local/share/saidwork/saidwork.log` (`RUST_LOG=debug` for more) |
| Provider server logs | `~/.local/share/saidwork/providers/<name>.log` |

`SAIDWORK_HOME=/some/dir` puts config, data and notes under one directory.

**Shipped files** such as the Pianissimo server are named `{share}/<file>` in the
configuration and looked up in this order:

1. `$SAIDWORK_SHARE_DIR`
2. `~/.local/share/saidwork` (or `$SAIDWORK_HOME/share`)
3. `<prefix>/share/saidwork` next to the binary (`/usr`, `/usr/local`, `~/.local`, an unpacked tarball)
4. `saidwork/` under each of `$XDG_DATA_DIRS` (default `/usr/local/share:/usr/share`)
5. `tools/` in the source checkout the binary was built from

## Launch blocks

```toml
[providers.gemma-local.launch]
command = "llama-server"            # on PATH, or a path (`~` works)
args = ["-hf", "ggml-org/gemma-4-12B-it-GGUF:Q4_0", "--port", "8080"]
requires = ["~/models/extra.gguf"]  # files that must exist; checked before launching
env = { HIP_VISIBLE_DEVICES = "0" }
health_path = "/health"             # probed on base_url; any HTTP answer means "up"
startup_timeout_secs = 1800
stop_on_exit = true                 # stop it when saidwork exits (if saidwork started it)
# log_file = "~/llama.log"          # default: ~/.local/share/saidwork/providers/<name>.log
```

In `command`, `args` and `requires`, `~` expands to your home directory and
`{share}/<file>` to a shipped file. If you already start a server some other way (a
systemd unit, a wrapper script), either point `command` at your script or remove the
launch block and saidwork will only connect to it.

## Troubleshooting

**`error while loading shared libraries: libpulse-simple.so.0`**: libpulse is missing.
This happens before saidwork itself runs, so it can only come from a tarball or source
install. Install it with `sudo dnf install pulseaudio-libs`, `sudo apt install libpulse0`,
`sudo pacman -S libpulse` or `sudo zypper install libpulse0`. You can also rerun
`./install.sh`.

**`cannot make HTTP requests: … No CA certificates were loaded from the system`**:
the system has no root certificates, which saidwork needs to make any HTTP request, even
to a local server. Install `ca-certificates` with your package manager. The packages and
`install.sh` normally take care of it.

**`audio backend unavailable: connect failed`**: libpulse is there but no sound server
is answering. Install and start PipeWire's PulseAudio layer: `sudo dnf install
pipewire-pulseaudio` (Debian/Ubuntu/Arch: `pipewire-pulse`), then
`systemctl --user enable --now pipewire-pulse.socket`.

**`provider … cannot be started: `…` is not installed`**: install the program
(`saidwork doctor` prints the command for your distribution), or point
`launch.command` at it.

**`provider … did not become healthy within …`**: the server started but never
answered. Its output is in `~/.local/share/saidwork/providers/<name>.log`. On a first
start it is usually still downloading the model: raise `startup_timeout_secs`, or run
`saidwork providers start <name>` once and let it finish.

**`provider … launched process … exited early`**: the server crashed on start (wrong
flags, port in use, model not found). The provider log says why.

**`environment variable `…` for the API key is not set`**: export it in the shell that
starts saidwork.

## Uninstalling

```sh
sudo dnf remove saidwork      # or: sudo apt remove saidwork
# tarball installs:
rm ~/.local/bin/saidwork && rm -r ~/.local/share/saidwork/pianissimo-server
```

Your configuration, notes and recordings are left in place. Remove
`~/.config/saidwork`, `~/.local/share/saidwork` and `~/Documents/saidwork` to delete
them too.

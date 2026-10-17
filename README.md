# saidwork

Record what is said (microphone and/or what the computer plays), transcribe it with a
speech-to-text model, and let a second model rewrite the transcript into a Markdown note
in a format you define.

```
saidwork            # interactive terminal UI
saidwork record     # headless: Enter stops, note path is printed
```

## How it works

1. **Capture** – microphone and system audio (the default sink's monitor) are captured
   through PipeWire's PulseAudio interface, resampled to 16 kHz mono and mixed.
2. **Transcribe** – the WAV is posted to an OpenAI-compatible
   `/v1/audio/transcriptions` endpoint (whisper.cpp `whisper-server`, faster-whisper,
   Groq, OpenAI, …).
3. **Rewrite** – the transcript plus the chosen format template are sent to a chat
   model (Ollama `/api/chat` or OpenAI-compatible `/v1/chat/completions`).
4. **Save** – the note lands in your notes directory as
   `YYYYMMDD-HHMMSS-<format>-<title>.md`. The raw WAV and transcript are kept under the
   data directory so a note can be regenerated with another format (`saidwork rewrite`).

Local servers can be started on demand: give a provider a `launch` block and saidwork
spawns it when it is not answering, waits for the health check, and stops it on exit.

## Install (Fedora)

```sh
sudo dnf install rustup pulseaudio-libs-devel   # libpulse headers for the capture backend
rustup-init -y && source ~/.cargo/env
cargo install --path crates/saidwork
saidwork config init                           # writes ~/.config/saidwork/saidwork.toml + formats
```

Edit the providers in `saidwork.toml` (see `config/saidwork.example.toml`), then:

```sh
saidwork providers status        # probe endpoints
saidwork devices                 # list microphones and system-audio monitors
saidwork formats list            # the Markdown formats in ~/.config/saidwork/formats
saidwork                         # start the TUI: r record/stop, ↑/↓ format, n/e/o editor, q quit
```

Logs go to `~/.local/share/saidwork/saidwork.log` (`RUST_LOG=debug` for more). Set
`SAIDWORK_HOME=/some/dir` to keep config, data and notes under one directory.

## Local models (the default configuration)

The example config is wired for a fully local, Swedish-first setup:

- **Transcription: Klang Pianissimo** ([`KlangAI/pianissimo-sv`](https://huggingface.co/KlangAI/pianissimo-sv),
  a Parakeet v3 fine-tune, CC BY 4.0) served by `tools/pianissimo-server/server.py`, a
  small OpenAI-compatible server around [`onnx-asr`](https://github.com/istupakov/onnx-asr)
  and Klang's int8 ONNX export. It runs on the CPU at roughly 35× realtime, splits long
  recordings at pauses with Silero VAD, and needs only [`uv`](https://docs.astral.sh/uv/):
  saidwork starts it with `uv run …/server.py --port 8178` and the first start downloads
  about 660 MB.
- **Rewriting: Gemma 4 12B on llama.cpp** (ROCm/HIP build) through an OpenAI-compatible
  `llama-server` on port 8080, started on demand with `--reasoning-budget 0` so the answer
  is the note itself rather than its reasoning.

Swap either for whisper.cpp, Ollama or a hosted API by editing the provider block; the
example file shows the shapes.

## Formats

A format is a Markdown file with YAML frontmatter:

```markdown
---
name: Meeting notes
description: Structured minutes from a meeting or call.
when_to_use: Several people discussing topics, decisions and work items.
---
# {{title}}
## Summary
...
```

The body is handed to the rewriting model as the template to follow; `{{date}}` and
`{{title}}` are filled in. `when_to_use` is reserved for a future selector that lets the
LLM pick the format from the transcript.

The built-in formats (`conversation`, the default, plus `meeting-notes`, `journal-entry`
and `task-list`) are written to the formats directory at startup if they have never been installed there,
so new built-ins arrive with upgrades. Installed ones are tracked in
`.installed-builtins` in that directory, so a built-in you delete stays deleted. One you
have edited is never overwritten; saidwork warns that it differs from the shipped
version, and `saidwork formats reset <id>` puts the shipped version back (or brings back
a deleted one).

### Editing in your editor

Formats and notes open in `$VISUAL`, else `$EDITOR`, else `nvim`:

```sh
saidwork formats new             # start from a skeleton; the id comes from `name`
saidwork formats edit task-list  # edit an existing format
saidwork formats reset task-list # restore a built-in to the shipped version
saidwork record --review         # open the finished note when it is saved (also `rewrite`)
```

In the TUI, `n` defines a new format, `e` edits the selected one and `o` opens the note
that was just saved; the interface comes back when you quit the editor.

Formats are edited as drafts under the data directory and only saved when they still
parse, so a mistake never breaks the format list. A rejected draft is kept and reopened
the next time, and saving a draft unchanged does nothing.

## Architecture

A modular monolith: one Cargo workspace, one crate per vertical slice, hexagonal
ports/adapters inside each slice, and an application crate that composes them.

```
crates/kernel         clock, paths, secrets, retry, url helpers (no business rules)
crates/formats        Format templates: parsing, filesystem repository, selector port
crates/providers      Provider registry, API keys, health probe, on-demand launcher
crates/transcription  Transcriber port + OpenAI-compatible adapter
crates/rewriting      Rewriter port, prompt builder, OpenAI-chat and Ollama adapters
crates/capture        AudioBackend port, mixer, WAV, Pulse (Linux) and fake backends
crates/notes          NoteStore port + filesystem adapter
crates/app            Config, bootstrap, the record→transcribe→rewrite→save pipeline
crates/tui            ratatui UI (Elm-style model / update / view)
crates/saidwork       clap binary: TUI by default, headless subcommands
```

Slices never depend on each other; `app` maps between their models. Other platforms
compile without a native audio backend today; adding one means implementing
`AudioBackend` (e.g. with cpal) in `crates/capture/src/adapters`.

## Development

```sh
just check     # cargo fmt --check, clippy -D warnings (pedantic), cargo test
cargo test -p saidwork-capture -- --ignored    # hardware smoke test against the sound server
```

Tests use fakes for audio, transcription and rewriting, `wiremock` for the HTTP
adapters, `insta` snapshots for prompts and screens, and `proptest` for the mixer.

CI (`.github/workflows/ci.yml`) runs the same checks plus an MSRV build on every push and
pull request. Pushing a `v*` tag builds a release tarball for x86_64 Linux and attaches it
to a GitHub release:

```sh
git tag v0.1.0 && git push origin v0.1.0
```

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.

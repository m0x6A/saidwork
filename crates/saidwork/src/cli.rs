use std::future::Future;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, bail};
use clap::{Parser, Subcommand};
use saidwork_app::requirements::{CA_CERTIFICATES, Distro};
use saidwork_app::{
    App, BootstrapError, Check, CheckStatus, Config, DeviceSelection, DraftOutcome, FormatDraft,
    FormatId, FormatRepository, PRESETS, ProviderError, ProviderName, ProviderStatus, SessionEvent,
    SourceKind, check_audio, default_backend, http_client_available,
};
use saidwork_capture::{AudioBackend, FakeBackend};
use saidwork_kernel::AppPaths;

use tokio::io::{AsyncBufReadExt, BufReader};

/// Record speech, transcribe it and rewrite it into Markdown notes.
#[derive(Debug, Parser)]
#[command(name = "saidwork", version, about)]
struct Cli {
    /// Configuration file (default: ~/.config/saidwork/saidwork.toml).
    #[arg(long, global = true, env = "SAIDWORK_CONFIG")]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Open the interactive terminal interface (default).
    Tui,
    /// Record headlessly: press Enter to stop, then transcribe, rewrite and save.
    Record {
        /// Format id (see `formats list`); defaults to pipeline.default_format.
        #[arg(long)]
        format: Option<String>,
        /// Extra guidance for the rewriter, e.g. "weekly sync with the platform team".
        #[arg(long)]
        context: Option<String>,
        /// Play this WAV file instead of capturing devices; stops automatically at the end.
        #[arg(long, value_name = "FILE")]
        fake_audio: Option<PathBuf>,
        /// Open the finished note in your editor ($VISUAL, $EDITOR, else nvim).
        #[arg(long)]
        review: bool,
    },
    /// Transcribe a WAV file and print the text.
    Transcribe {
        file: PathBuf,
        /// Use this format's `vocabulary` as a spelling hint for the transcriber.
        #[arg(long)]
        format: Option<String>,
    },
    /// Rewrite a transcript text file into a note.
    Rewrite {
        file: PathBuf,
        #[arg(long)]
        format: Option<String>,
        #[arg(long)]
        context: Option<String>,
        /// Open the finished note in your editor ($VISUAL, $EDITOR, else nvim).
        #[arg(long)]
        review: bool,
    },
    /// List capturable audio devices.
    Devices,
    /// Check that audio, the configured providers and what they need are installed.
    Doctor,
    /// Manage format templates.
    Formats {
        #[command(subcommand)]
        command: FormatsCommand,
    },
    /// Inspect and start LLM providers.
    Providers {
        #[command(subcommand)]
        command: ProvidersCommand,
    },
    /// Manage the configuration file.
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
}

#[derive(Debug, Subcommand)]
enum FormatsCommand {
    /// List available formats.
    List,
    /// Print the directory formats are read from.
    Dir,
    /// Define a new format in your editor; its id is derived from the `name` you give it.
    New,
    /// Edit a format in your editor; it is only saved if it still parses.
    Edit { id: String },
    /// Restore a built-in format to the shipped version, discarding your changes.
    Reset { id: String },
}

#[derive(Debug, Subcommand)]
enum ProvidersCommand {
    /// Probe every configured provider.
    Status,
    /// Start a provider (if it has a launch command) and wait until it answers.
    Start { name: String },
}

#[derive(Debug, Subcommand)]
enum ConfigCommand {
    /// Write the example configuration and built-in formats to the config directory.
    Init {
        /// Overwrite an existing configuration file.
        #[arg(long)]
        force: bool,
        /// Which preset to start from (see `config presets`).
        #[arg(long, default_value = "local")]
        preset: String,
    },
    /// List the configuration presets `config init --preset` can start from.
    Presets,
    /// Print the configuration file path.
    Path,
}

pub async fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();
    // SAIDWORK_HOME roots every directory under one folder; handy for tests and demos.
    let paths = match std::env::var_os("SAIDWORK_HOME") {
        Some(root) => AppPaths::rooted_at(Path::new(&root)),
        None => AppPaths::discover()?,
    };
    let _log_guard = crate::logging::init(&paths.log_file())?;
    let config_path = cli.config.clone().unwrap_or_else(|| paths.config_file());
    let ctx = Context_ { config_path, paths };

    match cli.command.unwrap_or(Command::Tui) {
        Command::Config {
            command: ConfigCommand::Path,
        } => {
            println!("{}", ctx.config_path.display());
            Ok(())
        }
        Command::Config {
            command: ConfigCommand::Init { force, preset },
        } => config_init(&ctx.config_path, &ctx.paths, force, &preset),
        Command::Config {
            command: ConfigCommand::Presets,
        } => {
            for p in PRESETS {
                println!("{:<16} {}", p.id, p.description);
            }
            Ok(())
        }
        Command::Devices => devices(),
        Command::Doctor => doctor(&ctx).await,
        Command::Tui => {
            // The TUI shows the format warnings in its log; stderr would be painted over.
            let app = load_app(&ctx.config_path, &ctx.paths, default_backend()?)?;
            Ok(saidwork_tui::run(app).await?)
        }
        Command::Record {
            format,
            context,
            fake_audio,
            review,
        } => {
            let (backend, fake): (Arc<dyn AudioBackend>, bool) = match &fake_audio {
                Some(file) => (
                    Arc::new(FakeBackend::from_wav_file(
                        file,
                        saidwork_capture::AudioSpec::SPEECH,
                    )?),
                    true,
                ),
                None => (default_backend()?, false),
            };
            let app = ctx.app(backend)?;
            let pipeline = &app.pipeline;
            require_ready(&app, &[&pipeline.transcriber_name, &pipeline.rewriter_name]).await?;
            let note = with_shutdown(&app, record(&app, format, context, fake)).await?;
            review_note(review, &note).await
        }
        Command::Transcribe { file, format } => {
            let app = ctx.headless_app()?;
            require_ready(&app, &[&app.pipeline.transcriber_name]).await?;
            with_shutdown(&app, transcribe(&app, &file, format.as_deref())).await
        }
        Command::Rewrite {
            file,
            format,
            context,
            review,
        } => {
            let app = ctx.headless_app()?;
            require_ready(&app, &[&app.pipeline.rewriter_name]).await?;
            let note = with_shutdown(&app, rewrite(&app, &file, format, context)).await?;
            review_note(review, &note).await
        }
        // `reset` is the fix for the customised-format warning, so do not print it first.
        Command::Formats {
            command: command @ FormatsCommand::Reset { .. },
        } => {
            let app = load_app(&ctx.config_path, &ctx.paths, Arc::new(FakeBackend::new()))?;
            formats(&app, &command).await
        }
        Command::Formats { command } => formats(&ctx.headless_app()?, &command).await,
        Command::Providers {
            command: ProvidersCommand::Status,
        } => providers_status(&ctx.headless_app()?).await,
        Command::Providers {
            command: ProvidersCommand::Start { name },
        } => {
            let app = ctx.headless_app()?;
            with_shutdown(&app, providers_start(&app, &name)).await
        }
    }
}

/// Resolved paths shared by every command.
struct Context_ {
    config_path: PathBuf,
    paths: AppPaths,
}

impl Context_ {
    fn app(&self, backend: Arc<dyn AudioBackend>) -> anyhow::Result<App> {
        let app = load_app(&self.config_path, &self.paths, backend)?;
        for warning in app.format_warnings() {
            eprintln!("warning: {warning}");
        }
        Ok(app)
    }

    /// An app that never touches audio hardware.
    fn headless_app(&self) -> anyhow::Result<App> {
        self.app(Arc::new(FakeBackend::new()))
    }
}

/// Stop before doing any work if a provider in `names` cannot be started, and say how
/// to fix it.
async fn require_ready(app: &App, names: &[&ProviderName]) -> anyhow::Result<()> {
    let failures: Vec<Check> = app
        .preflight()
        .await
        .into_iter()
        .filter(|c| c.provider.as_ref().is_none_or(|p| names.contains(&p)))
        .collect();
    if failures.is_empty() {
        return Ok(());
    }
    for check in &failures {
        eprintln!("{check}");
    }
    bail!(
        "saidwork is not ready; fix the problems above (`saidwork doctor` checks everything, \
         {INSTALL_GUIDE} explains the setup)"
    )
}

const INSTALL_GUIDE: &str = "https://github.com/m0x6A/saidwork/blob/main/docs/INSTALL.md";

/// Report on everything saidwork needs; fails if anything blocks recording.
async fn doctor(ctx: &Context_) -> anyhow::Result<()> {
    let distro = Distro::detect();
    let mut checks = Vec::new();
    if let Err(err) = http_client_available() {
        checks.push(Check {
            subject: "https".into(),
            status: CheckStatus::Fail,
            detail: format!("cannot make HTTP requests: {err}"),
            fix: Some(CA_CERTIFICATES.install_steps(distro).to_owned()),
            provider: None,
        });
    }
    let app = match Config::load(&ctx.config_path) {
        Ok(config) => match App::build(config, ctx.paths.clone(), Arc::new(FakeBackend::new())) {
            Ok(app) => {
                checks.push(config_check(
                    CheckStatus::Ok,
                    ctx.config_path.display().to_string(),
                    None,
                ));
                Some(app)
            }
            // Already reported by the https check above.
            Err(BootstrapError::NoTls(_)) => None,
            Err(err) => {
                let fix = match &err {
                    BootstrapError::Provider(ProviderError::MissingApiKeyEnv { var, .. }) => {
                        if cfg!(windows) {
                            format!("`$env:{var} = \"<your key>\"` before starting saidwork")
                        } else {
                            format!("`export {var}=<your key>` before starting saidwork")
                        }
                    }
                    _ => format!("edit {}", ctx.config_path.display()),
                };
                checks.push(config_check(CheckStatus::Fail, err.to_string(), Some(&fix)));
                None
            }
        },
        Err(err) => {
            checks.push(config_check(
                CheckStatus::Fail,
                err.to_string(),
                Some("`saidwork config init` (`saidwork config presets` lists the choices)"),
            ));
            None
        }
    };
    match default_backend() {
        Ok(backend) => checks.push(check_audio(backend.as_ref(), distro)),
        Err(err) => checks.push(Check {
            subject: "audio".into(),
            status: CheckStatus::Fail,
            detail: err.to_string(),
            fix: None,
            provider: None,
        }),
    }
    if let Some(app) = &app {
        checks.extend(app.check_providers(distro).await);
        for warning in app.format_warnings() {
            checks.push(Check {
                subject: "formats".into(),
                status: CheckStatus::Warn,
                detail: warning,
                fix: None,
                provider: None,
            });
        }
    }
    for check in &checks {
        println!("{check}");
    }
    let failures = checks.iter().filter(|c| c.is_failure()).count();
    if failures > 0 {
        bail!("{failures} problem(s) found; the install guide is at {INSTALL_GUIDE}");
    }
    println!("everything saidwork needs is in place");
    Ok(())
}

fn config_check(status: CheckStatus, detail: String, fix: Option<&str>) -> Check {
    Check {
        subject: "config".into(),
        status,
        detail,
        fix: fix.map(str::to_owned),
        provider: None,
    }
}

/// Run `work`, then stop any providers the app launched, whatever the outcome.
async fn with_shutdown<T>(
    app: &App,
    work: impl Future<Output = anyhow::Result<T>>,
) -> anyhow::Result<T> {
    let result = work.await;
    app.shutdown().await;
    result
}

fn devices() -> anyhow::Result<()> {
    let backend = default_backend()?;
    for d in backend.list_devices()? {
        let default = if d.is_default { "  (default)" } else { "" };
        println!(
            "{:<13} {}{default}\n              id: {}",
            d.kind.to_string(),
            d.label,
            d.id
        );
    }
    Ok(())
}

async fn transcribe(app: &App, file: &Path, format: Option<&str>) -> anyhow::Result<()> {
    let vocabulary = match format {
        Some(id) => app.formats.get(&FormatId::new(id)?)?.vocabulary,
        None => None,
    };
    app.pipeline
        .prepare_providers(Some(&stderr_events()))
        .await?;
    let wav = std::fs::read(file).with_context(|| format!("reading {}", file.display()))?;
    let transcript = app.pipeline.transcribe_wav(wav, vocabulary).await?;
    println!("{}", transcript.as_plain_text());
    Ok(())
}

async fn rewrite(
    app: &App,
    file: &Path,
    format: Option<String>,
    context: Option<String>,
) -> anyhow::Result<PathBuf> {
    let format = pick_format(app, format)?;
    app.pipeline
        .prepare_providers(Some(&stderr_events()))
        .await?;
    let text =
        std::fs::read_to_string(file).with_context(|| format!("reading {}", file.display()))?;
    let (note, path) = app
        .pipeline
        .rewrite_and_save(&text, &format, context, Some(&stderr_events()))
        .await?;
    print_outcome(&path, &note.markdown);
    Ok(path)
}

async fn formats(app: &App, command: &FormatsCommand) -> anyhow::Result<()> {
    match command {
        FormatsCommand::New => edit_format(app, app.drafts.begin_new()?).await?,
        FormatsCommand::Edit { id } => {
            let id = FormatId::new(id)?;
            edit_format(app, app.drafts.begin_edit(&id)?).await?;
        }
        FormatsCommand::Reset { id } => {
            let format = app.formats.restore_builtin(&FormatId::new(id)?)?;
            println!(
                "restored built-in format {} ({})",
                format.id,
                app.formats.path_of(&format.id).display()
            );
        }
        FormatsCommand::Dir => println!("{}", app.formats.dir().display()),
        FormatsCommand::List => {
            let default = app.config.pipeline.default_format.as_deref();
            for f in app.formats.list()? {
                let marker = if default == Some(f.id.as_str()) {
                    "*"
                } else {
                    " "
                };
                println!("{marker} {:<16} {} — {}", f.id, f.name, f.description);
            }
        }
    }
    Ok(())
}

/// Open `draft` in the editor until it is saved, left unchanged, or the user gives up.
async fn edit_format(app: &App, draft: FormatDraft) -> anyhow::Result<()> {
    loop {
        open_editor(&draft.path).await?;
        match app.drafts.finish(&draft) {
            Ok(DraftOutcome::Saved(f)) => {
                println!(
                    "saved format {} ({})",
                    f.id,
                    app.formats.path_of(&f.id).display()
                );
                return Ok(());
            }
            Ok(DraftOutcome::Unchanged) => {
                println!("unchanged; nothing saved");
                return Ok(());
            }
            Err(err) => {
                eprintln!("error: {err}");
                if !confirm("reopen the editor to fix it? [Y/n] ")? {
                    println!("draft kept at {}", draft.path.display());
                    return Ok(());
                }
            }
        }
    }
}

async fn review_note(review: bool, note: &Path) -> anyhow::Result<()> {
    if review {
        open_editor(note).await?;
    }
    Ok(())
}

async fn open_editor(path: &Path) -> anyhow::Result<()> {
    let editor = saidwork_kernel::resolve_editor();
    let status = saidwork_kernel::open_in_editor(path)
        .await
        .with_context(|| format!("starting editor `{editor}`"))?;
    if !status.success() {
        bail!("editor `{editor}` exited with {status}");
    }
    Ok(())
}

/// Ask a yes/no question on stderr; Enter means yes, end of input means no.
fn confirm(question: &str) -> anyhow::Result<bool> {
    eprint!("{question}");
    std::io::stderr().flush()?;
    let mut answer = String::new();
    if std::io::stdin().read_line(&mut answer)? == 0 {
        return Ok(false);
    }
    Ok(!answer.trim().eq_ignore_ascii_case("n"))
}

async fn providers_status(app: &App) -> anyhow::Result<()> {
    for p in app.pipeline.supervisor.providers() {
        let status = match app.pipeline.supervisor.status(&p.name).await? {
            ProviderStatus::Up => "up",
            ProviderStatus::Down if p.launch.is_some() => "down (launchable)",
            ProviderStatus::Down => "down",
        };
        println!("{:<16} {:<18} {} ({})", p.name, status, p.base_url, p.model);
    }
    Ok(())
}

async fn providers_start(app: &App, name: &str) -> anyhow::Result<()> {
    let name = ProviderName::new(name)?;
    let readiness = app.pipeline.supervisor.ensure_running(&name).await?;
    println!("{name}: {readiness:?}");
    if app
        .pipeline
        .supervisor
        .get(&name)?
        .launch
        .as_ref()
        .is_some_and(|l| l.stop_on_exit)
    {
        println!(
            "note: stop_on_exit = true, so it is stopped again when this command exits; set stop_on_exit = false to keep it running"
        );
    }
    Ok(())
}

fn load_app(
    config_path: &Path,
    paths: &AppPaths,
    backend: Arc<dyn AudioBackend>,
) -> anyhow::Result<App> {
    let config = Config::load(config_path)?;
    Ok(App::build(config, paths.clone(), backend)?)
}

fn config_init(
    config_path: &Path,
    paths: &AppPaths,
    force: bool,
    preset: &str,
) -> anyhow::Result<()> {
    let Some(preset) = saidwork_app::preset(preset) else {
        let ids: Vec<_> = PRESETS.iter().map(|p| p.id).collect();
        bail!(
            "unknown preset `{preset}`; choose one of {}",
            ids.join(", ")
        );
    };
    if config_path.exists() && !force {
        bail!(
            "{} already exists; pass --force to overwrite",
            config_path.display()
        );
    }
    if let Some(parent) = config_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(config_path, preset.toml)?;
    println!("wrote {} (preset {})", config_path.display(), preset.id);
    let formats = saidwork_formats::FsFormatRepository::new(paths.formats_dir());
    let sync = formats.ensure_initialised()?;
    println!(
        "formats directory: {} ({} built-in templates written)",
        formats.dir().display(),
        sync.written.len()
    );
    println!("edit the providers section if needed, then run `saidwork doctor`");
    Ok(())
}

fn pick_format(app: &App, requested: Option<String>) -> anyhow::Result<FormatId> {
    let id = requested
        .or_else(|| app.config.pipeline.default_format.clone())
        .context("no format given; pass --format or set pipeline.default_format")?;
    Ok(FormatId::new(id)?)
}

/// Channel whose events are echoed to stderr as they arrive.
fn stderr_events() -> tokio::sync::mpsc::UnboundedSender<SessionEvent> {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<SessionEvent>();
    tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            eprintln!("• {event}");
        }
    });
    tx
}

fn print_outcome(path: &Path, markdown: &str) {
    println!("{}", path.display());
    if !std::io::stdout().is_terminal() {
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "\n{markdown}");
    }
}

async fn record(
    app: &App,
    format: Option<String>,
    context: Option<String>,
    fake: bool,
) -> anyhow::Result<PathBuf> {
    let format = pick_format(app, format)?;
    let events = stderr_events();
    app.pipeline.prepare_providers(Some(&events)).await?;

    let selection = if fake {
        let mic = app
            .capture
            .list_devices()
            .await?
            .into_iter()
            .find(|d| d.kind == SourceKind::Microphone);
        DeviceSelection {
            microphone: mic,
            system_audio: None,
        }
    } else {
        app.capture.resolve(&app.config.capture).await?
    };
    for d in selection.devices() {
        eprintln!("• capturing {}: {}", d.kind, d.label);
    }
    let handle = app.capture.start(&selection).await?;
    if fake {
        eprintln!("• playing file; will stop at the end (or press Enter)");
    } else {
        eprintln!("• recording — press Enter to stop");
    }

    let mut stdin = BufReader::new(tokio::io::stdin());
    let mut line = String::new();
    let mut stdin_open = true;
    let mut ticker = tokio::time::interval(std::time::Duration::from_millis(200));
    loop {
        tokio::select! {
            read = stdin.read_line(&mut line), if stdin_open => match read {
                // EOF (e.g. stdin redirected from /dev/null): keep recording until the
                // source ends or Ctrl-C arrives.
                Ok(0) => stdin_open = false,
                _ => break,
            },
            // Ctrl-C stops the recording and still processes it; a second one aborts.
            _ = tokio::signal::ctrl_c() => break,
            _ = ticker.tick() => {
                if handle.is_finished() { break; }
            }
        }
    }
    let recording = saidwork_app::CaptureService::stop(handle).await?;
    eprintln!("• recorded {:.1} s", recording.duration().as_secs_f64());
    let outcome = app
        .pipeline
        .process(&recording, &format, context, Some(&events))
        .await?;
    print_outcome(&outcome.note_path, &outcome.markdown);
    Ok(outcome.note_path)
}

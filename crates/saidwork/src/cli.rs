use std::future::Future;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, bail};
use clap::{Parser, Subcommand};
use saidwork_app::{
    App, Config, DeviceSelection, EXAMPLE_CONFIG, FormatId, FormatRepository, ProviderName,
    ProviderStatus, SessionEvent, SourceKind, default_backend,
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
    },
    /// Transcribe a WAV file and print the text.
    Transcribe { file: PathBuf },
    /// Rewrite a transcript text file into a note.
    Rewrite {
        file: PathBuf,
        #[arg(long)]
        format: Option<String>,
        #[arg(long)]
        context: Option<String>,
    },
    /// List capturable audio devices.
    Devices,
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
    },
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
            command: ConfigCommand::Init { force },
        } => config_init(&ctx.config_path, &ctx.paths, force),
        Command::Devices => devices(),
        Command::Tui => {
            let app = ctx.app(default_backend()?)?;
            Ok(saidwork_tui::run(app).await?)
        }
        Command::Record {
            format,
            context,
            fake_audio,
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
            with_shutdown(&app, record(&app, format, context, fake)).await
        }
        Command::Transcribe { file } => {
            let app = ctx.headless_app()?;
            with_shutdown(&app, transcribe(&app, &file)).await
        }
        Command::Rewrite {
            file,
            format,
            context,
        } => {
            let app = ctx.headless_app()?;
            with_shutdown(&app, rewrite(&app, &file, format, context)).await
        }
        Command::Formats { command } => formats(&ctx.headless_app()?, &command),
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
        load_app(&self.config_path, &self.paths, backend)
    }

    /// An app that never touches audio hardware.
    fn headless_app(&self) -> anyhow::Result<App> {
        self.app(Arc::new(FakeBackend::new()))
    }
}

/// Run `work`, then stop any providers the app launched, whatever the outcome.
async fn with_shutdown(
    app: &App,
    work: impl Future<Output = anyhow::Result<()>>,
) -> anyhow::Result<()> {
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

async fn transcribe(app: &App, file: &Path) -> anyhow::Result<()> {
    app.pipeline
        .prepare_providers(Some(&stderr_events()))
        .await?;
    let wav = std::fs::read(file).with_context(|| format!("reading {}", file.display()))?;
    let transcript = app.pipeline.transcribe_wav(wav).await?;
    println!("{}", transcript.as_plain_text());
    Ok(())
}

async fn rewrite(
    app: &App,
    file: &Path,
    format: Option<String>,
    context: Option<String>,
) -> anyhow::Result<()> {
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
    Ok(())
}

fn formats(app: &App, command: &FormatsCommand) -> anyhow::Result<()> {
    match command {
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

fn config_init(config_path: &Path, paths: &AppPaths, force: bool) -> anyhow::Result<()> {
    if config_path.exists() && !force {
        bail!(
            "{} already exists; pass --force to overwrite",
            config_path.display()
        );
    }
    if let Some(parent) = config_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(config_path, EXAMPLE_CONFIG)?;
    println!("wrote {}", config_path.display());
    let formats = saidwork_formats::FsFormatRepository::new(paths.formats_dir());
    let written = formats.ensure_initialised()?;
    println!(
        "formats directory: {} ({written} built-in templates written)",
        formats.dir().display()
    );
    println!("edit the providers section, then run `saidwork providers status`");
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
) -> anyhow::Result<()> {
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
    Ok(())
}

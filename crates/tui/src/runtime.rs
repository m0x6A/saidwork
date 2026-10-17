//! The event loop: terminal events, ticks and pipeline tasks all become [`Msg`]s.

use std::sync::Arc;
use std::time::Duration;

use crossterm::event::{Event, EventStream, KeyEventKind};
use futures::StreamExt;
use saidwork_app::{
    App, CaptureService, DeviceSelection, DraftOutcome, FormatId, FormatRepository as _,
    ProviderStatus, RecorderHandle, SessionEvent,
};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use crate::model::{Cmd, EditTarget, Model, Msg, ProviderView};
use crate::view;

/// Errors of the TUI.
#[derive(Debug, thiserror::Error)]
pub enum TuiError {
    #[error("terminal error: {0}")]
    Terminal(#[from] std::io::Error),
    #[error(transparent)]
    Formats(#[from] saidwork_formats::FormatError),
}

const TICK: Duration = Duration::from_millis(100);

/// Run the interactive interface until the user quits.
pub async fn run(app: App) -> Result<(), TuiError> {
    let formats = app.formats.list()?;
    let mut model = Model::new(formats, app.config.pipeline.default_format.as_deref());
    for path in &app.builtin_formats.written {
        model.push_log(format!("installed built-in format {}", path.display()));
    }
    for warning in app.format_warnings() {
        model.push_log(format!("warning: {warning}"));
    }
    let problems = app.preflight().await;
    for check in &problems {
        model.push_log(format!("error: {}: {}", check.subject, check.detail));
        if let Some(fix) = &check.fix {
            model.push_log(format!("  fix: {fix}"));
        }
    }
    if !problems.is_empty() {
        model.push_log(
            "recording will fail until this is fixed; `saidwork doctor` checks everything",
        );
    }
    let terminal = ratatui::init();
    let result = event_loop(terminal, Arc::new(app), model).await;
    ratatui::restore();
    result
}

struct Runtime {
    app: Arc<App>,
    tx: UnboundedSender<Msg>,
    /// Freshly opened recorders arrive here from the task that opened them.
    handle_tx: UnboundedSender<Result<RecorderHandle, String>>,
    recorder: Option<RecorderHandle>,
}

async fn event_loop(
    mut terminal: ratatui::DefaultTerminal,
    app: Arc<App>,
    mut model: Model,
) -> Result<(), TuiError> {
    let (tx, mut rx): (UnboundedSender<Msg>, UnboundedReceiver<Msg>) = unbounded_channel();
    let (handle_tx, mut handle_rx) = unbounded_channel::<Result<RecorderHandle, String>>();
    let mut rt = Runtime {
        app: app.clone(),
        tx: tx.clone(),
        handle_tx,
        recorder: None,
    };
    let mut events = EventStream::new();
    let mut ticker = tokio::time::interval(TICK);

    for cmd in Model::initial_cmds() {
        rt.execute(cmd);
    }
    terminal.draw(|f| view::render(&model, f))?;

    loop {
        let msg = tokio::select! {
            Some(msg) = rx.recv() => msg,
            Some(opened) = handle_rx.recv() => match opened {
                Ok(handle) => {
                    rt.recorder = Some(handle);
                    Msg::RecordingStarted(Ok(()))
                }
                Err(e) => Msg::RecordingStarted(Err(e)),
            },
            Some(Ok(event)) = events.next() => match event {
                Event::Key(key) if key.kind != KeyEventKind::Release => Msg::Key(key),
                _ => continue,
            },
            _ = ticker.tick() => {
                let (level, elapsed) = rt.recorder.as_ref().map_or((0.0, Duration::ZERO), |h| (h.level(), h.elapsed()));
                Msg::Tick { level, elapsed }
            }
        };
        let cmds = crate::model::update(&mut model, msg);
        for cmd in cmds {
            match cmd {
                Cmd::Edit(target) => {
                    let msg = edit_suspended(&mut terminal, &mut events, &app, target).await?;
                    let _ = tx.send(msg);
                }
                cmd => rt.execute(cmd),
            }
        }
        terminal.draw(|f| view::render(&model, f))?;
        if model.should_quit {
            break;
        }
    }
    drop(rt);
    app.shutdown().await;
    Ok(())
}

impl Runtime {
    fn execute(&mut self, cmd: Cmd) {
        let tx = self.tx.clone();
        let app = self.app.clone();
        match cmd {
            Cmd::LoadDevices => {
                tokio::spawn(async move {
                    let result = async {
                        let devices = app.capture.list_devices().await?;
                        let selection = app
                            .capture
                            .resolve(&app.config.capture)
                            .await
                            .unwrap_or_default();
                        Ok::<_, saidwork_capture::CaptureError>((devices, selection))
                    }
                    .await;
                    let _ = tx.send(Msg::Devices(result.map_err(|e| e.to_string())));
                });
            }
            Cmd::ProbeProviders => {
                tokio::spawn(async move {
                    let mut views = Vec::new();
                    let roles = [
                        (&app.pipeline.transcriber_name, "stt"),
                        (&app.pipeline.rewriter_name, "llm"),
                    ];
                    for (name, role) in roles {
                        let status = match app.pipeline.supervisor.status(name).await {
                            Ok(ProviderStatus::Up) => "up".to_owned(),
                            Ok(ProviderStatus::Down) => "down".to_owned(),
                            Err(e) => e.to_string(),
                        };
                        views.push(ProviderView {
                            name: name.clone(),
                            role,
                            status,
                        });
                    }
                    let _ = tx.send(Msg::Providers(views));
                });
            }
            Cmd::Prepare => {
                tokio::spawn(async move {
                    let (etx, erx) = unbounded_channel::<SessionEvent>();
                    forward_events(erx, tx.clone());
                    let result = app
                        .pipeline
                        .prepare_providers(Some(&etx))
                        .await
                        .map_err(|e| e.to_string());
                    let _ = tx.send(Msg::Prepared(result));
                });
            }
            Cmd::StartRecording(selection) => self.start_recording(selection),
            Cmd::StopAndProcess(format) => self.stop_and_process(format),
            Cmd::Edit(_) => unreachable!("the event loop opens the editor itself"),
            Cmd::Discard => {
                if let Some(handle) = self.recorder.take() {
                    tokio::spawn(async move {
                        let _ = CaptureService::stop(handle).await;
                    });
                }
            }
        }
    }

    fn start_recording(&self, selection: DeviceSelection) {
        let app = self.app.clone();
        let handle_tx = self.handle_tx.clone();
        tokio::spawn(async move {
            let result = app
                .capture
                .start(&selection)
                .await
                .map_err(|e| e.to_string());
            let _ = handle_tx.send(result);
        });
    }

    fn stop_and_process(&mut self, format: FormatId) {
        let Some(handle) = self.recorder.take() else {
            let _ = self
                .tx
                .send(Msg::Finished(Err("no recording in progress".into())));
            return;
        };
        let tx = self.tx.clone();
        let app = self.app.clone();
        tokio::spawn(async move {
            let (etx, erx) = unbounded_channel::<SessionEvent>();
            forward_events(erx, tx.clone());
            let result = async {
                let recording = CaptureService::stop(handle).await?;
                app.pipeline
                    .process(&recording, &format, None, Some(&etx))
                    .await
            }
            .await;
            let _ = tx.send(Msg::Finished(result.map_err(|e| e.to_string())));
        });
    }
}

/// Hand the terminal to the editor for `target`, then take it back.
///
/// Runs inline in the event loop: nothing may draw or read keys while the editor owns the
/// terminal. Queued messages (pipeline events, a running recording's progress) are handled
/// once it exits.
async fn edit_suspended(
    terminal: &mut ratatui::DefaultTerminal,
    events: &mut EventStream,
    app: &App,
    target: EditTarget,
) -> Result<Msg, TuiError> {
    let with_path = |d: saidwork_app::FormatDraft| (Some(d.clone()), d.path);
    let begun = match target {
        EditTarget::NewFormat => app.drafts.begin_new().map(with_path),
        EditTarget::Format(id) => app.drafts.begin_edit(&id).map(with_path),
        EditTarget::Note(path) => Ok((None, path)),
    };
    let (draft, path) = match begun {
        Ok(begun) => begun,
        Err(e) => return Ok(Msg::Edited(format!("error: {e}"))),
    };

    // A fresh stream drops the old one, which stops crossterm reading stdin under the
    // editor; the new one only starts reading when the loop polls it again.
    *events = EventStream::new();
    ratatui::restore();
    let status = saidwork_kernel::open_in_editor(&path).await;
    crossterm::terminal::enable_raw_mode()?;
    crossterm::execute!(
        std::io::stdout(),
        crossterm::terminal::EnterAlternateScreen,
        crossterm::terminal::Clear(crossterm::terminal::ClearType::All)
    )?;
    // A fresh terminal has empty buffers, so the next draw repaints everything.
    // (`Terminal::clear` would query the cursor position, which some terminals never answer.)
    *terminal = ratatui::Terminal::new(ratatui::backend::CrosstermBackend::new(std::io::stdout()))?;

    let editor = saidwork_kernel::resolve_editor();
    match status {
        Err(e) => {
            return Ok(Msg::Edited(format!(
                "error: could not start `{editor}`: {e}"
            )));
        }
        Ok(s) if !s.success() => {
            return Ok(Msg::Edited(format!(
                "`{editor}` exited with {s}; nothing saved"
            )));
        }
        Ok(_) => {}
    }
    let Some(draft) = draft else {
        return Ok(Msg::Edited(format!("closed {}", path.display())));
    };
    Ok(match app.drafts.finish(&draft) {
        Ok(DraftOutcome::Unchanged) => Msg::Edited("format unchanged".into()),
        Ok(DraftOutcome::Saved(format)) => match app.formats.list() {
            Ok(formats) => Msg::FormatSaved {
                formats,
                id: format.id,
            },
            Err(e) => Msg::Edited(format!("error: {e}")),
        },
        Err(e) => Msg::Edited(format!(
            "format not saved: {e}; the draft is kept, press {} to fix it",
            if draft.target.is_some() { 'e' } else { 'n' }
        )),
    })
}

fn forward_events(mut erx: UnboundedReceiver<SessionEvent>, tx: UnboundedSender<Msg>) {
    tokio::spawn(async move {
        while let Some(e) = erx.recv().await {
            let _ = tx.send(Msg::Session(e));
        }
    });
}

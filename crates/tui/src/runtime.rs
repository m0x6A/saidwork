//! The event loop: terminal events, ticks and pipeline tasks all become [`Msg`]s.

use std::sync::Arc;
use std::time::Duration;

use crossterm::event::{Event, EventStream, KeyEventKind};
use futures::StreamExt;
use saidwork_app::{
    App, CaptureService, DeviceSelection, FormatId, ProviderStatus, RecorderHandle, SessionEvent,
};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use crate::model::{Cmd, Model, Msg, ProviderView};
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
    use saidwork_app::FormatRepository as _;
    let formats = app.formats.list()?;
    let model = Model::new(formats, app.config.pipeline.default_format.as_deref());
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
            rt.execute(cmd);
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

fn forward_events(mut erx: UnboundedReceiver<SessionEvent>, tx: UnboundedSender<Msg>) {
    tokio::spawn(async move {
        while let Some(e) = erx.recv().await {
            let _ = tx.send(Msg::Session(e));
        }
    });
}

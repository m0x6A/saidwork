//! Pure UI state and its reducer.

use std::path::PathBuf;
use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use saidwork_app::{
    AudioDevice, DeviceSelection, Format, FormatId, ProviderName, SessionEvent, SessionOutcome,
    SourceKind,
};

/// What the session is doing right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Phase {
    Idle,
    /// Providers are being checked / launched before recording starts.
    Preparing,
    Recording,
    Processing,
    Done {
        note_path: PathBuf,
    },
    Failed {
        message: String,
    },
}

/// Probed state of one provider, for the header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderView {
    pub name: ProviderName,
    pub role: &'static str,
    pub status: String,
}

/// All state the view needs.
#[derive(Debug, Clone, PartialEq)]
pub struct Model {
    pub phase: Phase,
    pub formats: Vec<Format>,
    pub selected_format: usize,
    pub devices: Vec<AudioDevice>,
    /// `false` until the backend has answered the first device query.
    pub devices_loaded: bool,
    pub selection: DeviceSelection,
    pub providers: Vec<ProviderView>,
    pub level: f32,
    pub elapsed: Duration,
    pub log: Vec<String>,
    pub should_quit: bool,
}

/// Inputs to the reducer.
#[derive(Debug)]
pub enum Msg {
    Key(KeyEvent),
    /// Periodic tick with the recorder's current level and elapsed time.
    Tick {
        level: f32,
        elapsed: Duration,
    },
    Devices(Result<(Vec<AudioDevice>, DeviceSelection), String>),
    Providers(Vec<ProviderView>),
    Prepared(Result<(), String>),
    RecordingStarted(Result<(), String>),
    Session(SessionEvent),
    Finished(Result<SessionOutcome, String>),
    /// A format was saved from the editor; `formats` is the reloaded list.
    FormatSaved {
        formats: Vec<Format>,
        id: FormatId,
    },
    /// The editor closed; the line describes what came of it.
    Edited(String),
}

/// What to open in the user's editor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditTarget {
    NewFormat,
    Format(FormatId),
    Note(PathBuf),
}

/// Side effects the runtime performs on behalf of the reducer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cmd {
    LoadDevices,
    ProbeProviders,
    Prepare,
    StartRecording(DeviceSelection),
    /// Stop the recorder and run the pipeline with this format.
    StopAndProcess(FormatId),
    /// Stop the recorder and throw the audio away.
    Discard,
    /// Suspend the interface and open the editor.
    Edit(EditTarget),
}

const LOG_LINES: usize = 200;

impl Model {
    #[must_use]
    pub fn new(formats: Vec<Format>, default_format: Option<&str>) -> Self {
        let selected_format = default_format
            .and_then(|d| formats.iter().position(|f| f.id.as_str() == d))
            .unwrap_or(0);
        Self {
            phase: Phase::Idle,
            formats,
            selected_format,
            devices: Vec::new(),
            devices_loaded: false,
            selection: DeviceSelection::default(),
            providers: Vec::new(),
            level: 0.0,
            elapsed: Duration::ZERO,
            log: Vec::new(),
            should_quit: false,
        }
    }

    #[must_use]
    pub fn selected_format(&self) -> Option<&Format> {
        self.formats.get(self.selected_format)
    }

    pub fn push_log(&mut self, line: impl Into<String>) {
        self.log.push(line.into());
        if self.log.len() > LOG_LINES {
            self.log.remove(0);
        }
    }

    /// Commands to issue right after start-up.
    #[must_use]
    pub fn initial_cmds() -> Vec<Cmd> {
        vec![Cmd::LoadDevices, Cmd::ProbeProviders]
    }

    fn cycle_device(&mut self, kind: SourceKind) {
        let options: Vec<Option<AudioDevice>> = std::iter::once(None)
            .chain(
                self.devices
                    .iter()
                    .filter(|d| d.kind == kind)
                    .cloned()
                    .map(Some),
            )
            .collect();
        let slot = match kind {
            SourceKind::Microphone => &mut self.selection.microphone,
            SourceKind::SystemAudio => &mut self.selection.system_audio,
        };
        let current = options.iter().position(|o| o == slot).unwrap_or(0);
        slot.clone_from(&options[(current + 1) % options.len()]);
    }
}

/// Apply `msg` to `model`, returning the side effects to run.
pub fn update(model: &mut Model, msg: Msg) -> Vec<Cmd> {
    match msg {
        Msg::Key(key) => on_key(model, key),
        Msg::Tick { level, elapsed } => {
            if model.phase == Phase::Recording {
                model.level = level;
                model.elapsed = elapsed;
            }
            vec![]
        }
        Msg::Devices(Ok((devices, selection))) => {
            model.devices = devices;
            model.selection = selection;
            model.devices_loaded = true;
            vec![]
        }
        Msg::Devices(Err(err)) => {
            model.devices_loaded = true;
            model.push_log(format!("audio devices unavailable: {err}"));
            vec![]
        }
        Msg::Providers(providers) => {
            model.providers = providers;
            vec![]
        }
        Msg::Prepared(Ok(())) if model.phase == Phase::Preparing => {
            model.phase = Phase::Recording;
            model.level = 0.0;
            model.elapsed = Duration::ZERO;
            vec![
                Cmd::StartRecording(model.selection.clone()),
                Cmd::ProbeProviders,
            ]
        }
        Msg::Prepared(Ok(())) => vec![],
        Msg::Prepared(Err(err)) | Msg::RecordingStarted(Err(err)) => {
            model.phase = Phase::Failed {
                message: err.clone(),
            };
            model.push_log(format!("error: {err}"));
            vec![Cmd::ProbeProviders]
        }
        Msg::RecordingStarted(Ok(())) => {
            for d in model.selection.devices() {
                model.push_log(format!("capturing {}: {}", d.kind, d.label));
            }
            vec![]
        }
        Msg::Session(event) => {
            model.push_log(event.to_string());
            vec![]
        }
        Msg::Finished(Ok(outcome)) => {
            model.push_log(format!("done: {}", outcome.note_path.display()));
            model.phase = Phase::Done {
                note_path: outcome.note_path,
            };
            vec![]
        }
        Msg::Finished(Err(err)) => {
            model.push_log(format!("error: {err}"));
            model.phase = Phase::Failed { message: err };
            vec![]
        }
        Msg::FormatSaved { formats, id } => {
            model.push_log(format!("saved format {id}"));
            model.formats = formats;
            if let Some(i) = model.formats.iter().position(|f| f.id == id) {
                model.selected_format = i;
            }
            model.selected_format = model
                .selected_format
                .min(model.formats.len().saturating_sub(1));
            vec![]
        }
        Msg::Edited(line) => {
            model.push_log(line);
            vec![]
        }
    }
}

fn on_key(model: &mut Model, key: KeyEvent) -> Vec<Cmd> {
    let ctrl_c = key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => quit(model),
        _ if ctrl_c => quit(model),
        KeyCode::Char('r' | ' ') | KeyCode::Enter => toggle_recording(model),
        KeyCode::Up | KeyCode::Char('k') => {
            if model.selected_format > 0 {
                model.selected_format -= 1;
            }
            vec![]
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if model.selected_format + 1 < model.formats.len() {
                model.selected_format += 1;
            }
            vec![]
        }
        KeyCode::Char('m') if model.phase != Phase::Recording => {
            model.cycle_device(SourceKind::Microphone);
            vec![]
        }
        KeyCode::Char('s') if model.phase != Phase::Recording => {
            model.cycle_device(SourceKind::SystemAudio);
            vec![]
        }
        KeyCode::Char('p') => vec![Cmd::ProbeProviders],
        KeyCode::Char('n') => vec![Cmd::Edit(EditTarget::NewFormat)],
        KeyCode::Char('e') => {
            if let Some(f) = model.selected_format() {
                vec![Cmd::Edit(EditTarget::Format(f.id.clone()))]
            } else {
                model.push_log("no format selected");
                vec![]
            }
        }
        KeyCode::Char('o') => {
            if let Phase::Done { note_path } = &model.phase {
                vec![Cmd::Edit(EditTarget::Note(note_path.clone()))]
            } else {
                model.push_log("no note to open yet; record one first");
                vec![]
            }
        }
        _ => vec![],
    }
}

fn quit(model: &mut Model) -> Vec<Cmd> {
    model.should_quit = true;
    if model.phase == Phase::Recording {
        vec![Cmd::Discard]
    } else {
        vec![]
    }
}

fn toggle_recording(model: &mut Model) -> Vec<Cmd> {
    match &model.phase {
        Phase::Recording => {
            let Some(format) = model.selected_format() else {
                model.push_log("no format selected");
                return vec![];
            };
            let id = format.id.clone();
            model.phase = Phase::Processing;
            model.level = 0.0;
            vec![Cmd::StopAndProcess(id)]
        }
        Phase::Idle | Phase::Done { .. } | Phase::Failed { .. } => {
            if !model.devices_loaded {
                model.push_log("audio devices are still loading; try again in a moment");
                return vec![];
            }
            if model.selection.devices().is_empty() {
                model.push_log("select a microphone (m) or system audio (s) first");
                return vec![];
            }
            if model.formats.is_empty() {
                model.push_log("no formats available; add *.md files to the formats directory");
                return vec![];
            }
            model.phase = Phase::Preparing;
            model.push_log("preparing providers…");
            vec![Cmd::Prepare]
        }
        Phase::Preparing | Phase::Processing => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn format(id: &str) -> Format {
        Format {
            id: FormatId::new(id).unwrap(),
            name: id.to_owned(),
            description: String::new(),
            when_to_use: None,
            vocabulary: None,
            template: "t".into(),
        }
    }

    fn device(id: &str, kind: SourceKind) -> AudioDevice {
        AudioDevice {
            id: id.into(),
            label: id.into(),
            kind,
            is_default: false,
        }
    }

    fn key(code: KeyCode) -> Msg {
        Msg::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn ready_model() -> Model {
        let mut m = Model::new(vec![format("a"), format("b")], Some("b"));
        let mic = device("mic", SourceKind::Microphone);
        let mon = device("mon", SourceKind::SystemAudio);
        update(
            &mut m,
            Msg::Devices(Ok((
                vec![mic.clone(), mon.clone()],
                DeviceSelection {
                    microphone: Some(mic),
                    system_audio: Some(mon),
                },
            ))),
        );
        m
    }

    #[test]
    fn default_format_is_preselected_and_navigation_is_bounded() {
        let mut m = ready_model();
        assert_eq!(m.selected_format, 1);
        update(&mut m, key(KeyCode::Down));
        assert_eq!(m.selected_format, 1);
        update(&mut m, key(KeyCode::Up));
        update(&mut m, key(KeyCode::Up));
        assert_eq!(m.selected_format, 0);
    }

    #[test]
    fn record_flow_idle_prepare_record_process_done() {
        let mut m = ready_model();
        assert_eq!(update(&mut m, key(KeyCode::Char('r'))), vec![Cmd::Prepare]);
        assert_eq!(m.phase, Phase::Preparing);
        assert_eq!(
            update(&mut m, key(KeyCode::Char('r'))),
            vec![],
            "ignored while preparing"
        );

        let cmds = update(&mut m, Msg::Prepared(Ok(())));
        assert_eq!(cmds[0], Cmd::StartRecording(m.selection.clone()));
        assert_eq!(m.phase, Phase::Recording);

        update(
            &mut m,
            Msg::Tick {
                level: 0.4,
                elapsed: Duration::from_secs(3),
            },
        );
        assert_eq!(m.level, 0.4);

        assert_eq!(
            update(&mut m, key(KeyCode::Char('r'))),
            vec![Cmd::StopAndProcess(FormatId::new("b").unwrap())]
        );
        assert_eq!(m.phase, Phase::Processing);

        update(&mut m, Msg::Session(SessionEvent::Transcribing));
        assert!(m.log.iter().any(|l| l.contains("transcribing")));

        let outcome = SessionOutcome {
            note_path: PathBuf::from("/n/x.md"),
            transcript_path: None,
            recording_path: None,
            title: None,
            markdown: String::new(),
        };
        update(&mut m, Msg::Finished(Ok(outcome)));
        assert_eq!(
            m.phase,
            Phase::Done {
                note_path: PathBuf::from("/n/x.md")
            }
        );
        assert_eq!(
            update(&mut m, key(KeyCode::Char('r'))),
            vec![Cmd::Prepare],
            "can record again"
        );
    }

    #[test]
    fn failures_land_in_failed_phase_and_log() {
        let mut m = ready_model();
        update(&mut m, key(KeyCode::Char('r')));
        update(&mut m, Msg::Prepared(Err("provider x down".into())));
        assert_eq!(
            m.phase,
            Phase::Failed {
                message: "provider x down".into()
            }
        );
        assert!(m.log.last().unwrap().contains("provider x down"));
    }

    #[test]
    fn cannot_record_without_devices() {
        let mut m = Model::new(vec![format("a")], None);
        assert_eq!(update(&mut m, key(KeyCode::Char('r'))), vec![]);
        assert_eq!(m.phase, Phase::Idle);
        assert!(m.log.last().unwrap().contains("still loading"));
        update(
            &mut m,
            Msg::Devices(Ok((vec![], DeviceSelection::default()))),
        );
        assert_eq!(update(&mut m, key(KeyCode::Char('r'))), vec![]);
        assert!(m.log.last().unwrap().contains("select a microphone"));
    }

    #[test]
    fn device_keys_cycle_through_none_and_each_device() {
        let mut m = ready_model();
        m.devices.push(device("mic2", SourceKind::Microphone));
        update(&mut m, key(KeyCode::Char('m')));
        assert_eq!(
            m.selection.microphone.as_ref().map(|d| d.id.as_str()),
            Some("mic2")
        );
        update(&mut m, key(KeyCode::Char('m')));
        assert_eq!(m.selection.microphone, None);
        update(&mut m, key(KeyCode::Char('m')));
        assert_eq!(
            m.selection.microphone.as_ref().map(|d| d.id.as_str()),
            Some("mic")
        );
        update(&mut m, key(KeyCode::Char('s')));
        assert_eq!(m.selection.system_audio, None);
    }

    #[test]
    fn quit_while_recording_discards() {
        let mut m = ready_model();
        update(&mut m, key(KeyCode::Char('r')));
        update(&mut m, Msg::Prepared(Ok(())));
        assert_eq!(update(&mut m, key(KeyCode::Char('q'))), vec![Cmd::Discard]);
        assert!(m.should_quit);
        let mut idle = ready_model();
        assert_eq!(
            update(
                &mut idle,
                Msg::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL))
            ),
            vec![]
        );
        assert!(idle.should_quit);
    }

    #[test]
    fn editor_keys() {
        let mut m = ready_model();
        assert_eq!(
            update(&mut m, key(KeyCode::Char('n'))),
            vec![Cmd::Edit(EditTarget::NewFormat)]
        );
        assert_eq!(
            update(&mut m, key(KeyCode::Char('e'))),
            vec![Cmd::Edit(EditTarget::Format(FormatId::new("b").unwrap()))]
        );
        assert_eq!(update(&mut m, key(KeyCode::Char('o'))), vec![]);
        assert!(m.log.last().unwrap().contains("no note"));
        m.phase = Phase::Done {
            note_path: PathBuf::from("/n/x.md"),
        };
        assert_eq!(
            update(&mut m, key(KeyCode::Char('o'))),
            vec![Cmd::Edit(EditTarget::Note(PathBuf::from("/n/x.md")))]
        );
    }

    #[test]
    fn saved_format_is_selected() {
        let mut m = ready_model();
        update(
            &mut m,
            Msg::FormatSaved {
                formats: vec![format("a"), format("aa"), format("b")],
                id: FormatId::new("aa").unwrap(),
            },
        );
        assert_eq!(m.formats.len(), 3);
        assert_eq!(m.selected_format, 1);
    }

    #[test]
    fn log_is_bounded() {
        let mut m = ready_model();
        for i in 0..300 {
            m.push_log(i.to_string());
        }
        assert_eq!(m.log.len(), LOG_LINES);
        assert_eq!(m.log[0], "100");
    }
}

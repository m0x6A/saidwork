//! PulseAudio protocol backend. On Fedora this is served by `pipewire-pulse`,
//! so microphones and sink monitors are all ordinary sources.

use std::cell::RefCell;
use std::rc::Rc;

use libpulse_binding as pulse;
use libpulse_simple_binding::Simple;
use pulse::callbacks::ListResult;
use pulse::context::{Context, FlagSet as ContextFlagSet, State};
use pulse::def::BufferAttr;
use pulse::mainloop::standard::{IterateResult, Mainloop};
use pulse::operation::State as OpState;
use pulse::sample::{Format, Spec};
use pulse::stream::Direction;

use crate::domain::{AudioDevice, AudioSpec, CaptureError, SourceKind};
use crate::ports::{AudioBackend, AudioStream};

const APP_NAME: &str = "saidwork";

/// Backend talking to the PulseAudio server (or PipeWire's compatible one).
#[derive(Debug, Clone, Default)]
pub struct PulseBackend;

impl PulseBackend {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

#[derive(Debug, Default, Clone)]
struct Defaults {
    source: Option<String>,
    sink: Option<String>,
}

#[derive(Debug, Clone)]
struct RawSource {
    name: String,
    description: String,
    monitor_of_sink: bool,
}

/// Run `f` with a connected context, pumping the mainloop until `done` is set.
fn with_context<T: Default + 'static>(
    f: impl FnOnce(&Context, Rc<RefCell<T>>, Rc<RefCell<bool>>) -> Result<(), CaptureError>,
) -> Result<T, CaptureError> {
    let unavailable = |what: &str| CaptureError::BackendUnavailable(what.to_owned());
    let mut mainloop = Mainloop::new().ok_or_else(|| unavailable("could not create mainloop"))?;
    let mut context =
        Context::new(&mainloop, APP_NAME).ok_or_else(|| unavailable("could not create context"))?;
    context
        .connect(None, ContextFlagSet::NOFLAGS, None)
        .map_err(|e| CaptureError::BackendUnavailable(format!("connect failed: {e}")))?;
    loop {
        match mainloop.iterate(true) {
            IterateResult::Quit(_) | IterateResult::Err(_) => {
                return Err(unavailable("mainloop stopped while connecting"));
            }
            IterateResult::Success(_) => {}
        }
        match context.get_state() {
            State::Ready => break,
            State::Failed | State::Terminated => {
                return Err(unavailable(
                    "no PulseAudio/PipeWire server is running (is pipewire-pulse active?)",
                ));
            }
            _ => {}
        }
    }
    let result = Rc::new(RefCell::new(T::default()));
    let done = Rc::new(RefCell::new(false));
    f(&context, result.clone(), done.clone())?;
    while !*done.borrow() {
        match mainloop.iterate(true) {
            IterateResult::Quit(_) | IterateResult::Err(_) => {
                return Err(unavailable("mainloop stopped"));
            }
            IterateResult::Success(_) => {}
        }
    }
    context.disconnect();
    let value = std::mem::take(&mut *result.borrow_mut());
    Ok(value)
}

fn server_defaults() -> Result<Defaults, CaptureError> {
    with_context(|context, result: Rc<RefCell<Defaults>>, done| {
        context.introspect().get_server_info(move |info| {
            *result.borrow_mut() = Defaults {
                source: info.default_source_name.as_ref().map(ToString::to_string),
                sink: info.default_sink_name.as_ref().map(ToString::to_string),
            };
            *done.borrow_mut() = true;
        });
        Ok(())
    })
}

fn list_sources() -> Result<Vec<RawSource>, CaptureError> {
    with_context(|context, result: Rc<RefCell<Vec<RawSource>>>, done| {
        let op = context
            .introspect()
            .get_source_info_list(move |item| match item {
                ListResult::Item(info) => {
                    result.borrow_mut().push(RawSource {
                        name: info
                            .name
                            .as_ref()
                            .map(ToString::to_string)
                            .unwrap_or_default(),
                        description: info
                            .description
                            .as_ref()
                            .map(ToString::to_string)
                            .unwrap_or_default(),
                        monitor_of_sink: info.monitor_of_sink.is_some(),
                    });
                }
                ListResult::End | ListResult::Error => *done.borrow_mut() = true,
            });
        if op.get_state() == OpState::Cancelled {
            return Err(CaptureError::BackendUnavailable(
                "source listing was cancelled".into(),
            ));
        }
        Ok(())
    })
}

impl AudioBackend for PulseBackend {
    fn list_devices(&self) -> Result<Vec<AudioDevice>, CaptureError> {
        let defaults = server_defaults()?;
        let default_monitor = defaults.sink.as_ref().map(|s| format!("{s}.monitor"));
        let mut devices: Vec<AudioDevice> = list_sources()?
            .into_iter()
            .filter(|s| !s.name.is_empty())
            .map(|s| {
                let kind = if s.monitor_of_sink {
                    SourceKind::SystemAudio
                } else {
                    SourceKind::Microphone
                };
                let is_default = match kind {
                    SourceKind::Microphone => defaults.source.as_deref() == Some(s.name.as_str()),
                    SourceKind::SystemAudio => default_monitor.as_deref() == Some(s.name.as_str()),
                };
                let label = if s.description.is_empty() {
                    s.name.clone()
                } else {
                    s.description
                };
                AudioDevice {
                    id: s.name,
                    label,
                    kind,
                    is_default,
                }
            })
            .collect();
        devices.sort_by(|a, b| {
            (a.kind as u8, !a.is_default, &a.label).cmp(&(b.kind as u8, !b.is_default, &b.label))
        });
        Ok(devices)
    }

    fn open(
        &self,
        device: &AudioDevice,
        spec: AudioSpec,
    ) -> Result<Box<dyn AudioStream>, CaptureError> {
        let sample_spec = Spec {
            format: Format::S16le,
            channels: u8::try_from(spec.channels).unwrap_or(1),
            rate: spec.sample_rate,
        };
        if !sample_spec.is_valid() {
            return Err(CaptureError::OpenFailed {
                device: device.id.clone(),
                reason: format!("invalid sample spec {spec:?}"),
            });
        }
        let fragsize = u32::try_from(spec.samples_for(std::time::Duration::from_millis(50)) * 2)
            .unwrap_or(u32::MAX);
        let attr = BufferAttr {
            maxlength: u32::MAX,
            tlength: u32::MAX,
            prebuf: u32::MAX,
            minreq: u32::MAX,
            fragsize,
        };
        let simple = Simple::new(
            None,
            APP_NAME,
            Direction::Record,
            Some(&device.id),
            "capture",
            &sample_spec,
            None,
            Some(&attr),
        )
        .map_err(|e| CaptureError::OpenFailed {
            device: device.id.clone(),
            reason: e.to_string().unwrap_or_else(|| "unknown".into()),
        })?;
        tracing::info!(device = %device.id, kind = %device.kind, "opened pulse source");
        Ok(Box::new(PulseStream {
            simple,
            bytes: Vec::new(),
        }))
    }
}

struct PulseStream {
    simple: Simple,
    bytes: Vec<u8>,
}

impl AudioStream for PulseStream {
    fn read(&mut self, buf: &mut [i16]) -> Result<usize, CaptureError> {
        self.bytes.resize(buf.len() * 2, 0);
        self.simple.read(&mut self.bytes).map_err(|e| {
            CaptureError::Read(e.to_string().unwrap_or_else(|| "pulse read failed".into()))
        })?;
        for (sample, pair) in buf.iter_mut().zip(self.bytes.as_chunks::<2>().0) {
            *sample = i16::from_le_bytes(*pair);
        }
        Ok(buf.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Needs a running PipeWire/PulseAudio server; run with `cargo test -- --ignored`.
    #[test]
    #[ignore = "requires a sound server"]
    fn lists_real_devices_and_reads_a_chunk() {
        let backend = PulseBackend::new();
        let devices = backend.list_devices().unwrap();
        assert_ne!(devices.len(), 0);
        let mic = backend
            .default_device(SourceKind::Microphone)
            .unwrap()
            .expect("a default microphone");
        let mut stream = backend.open(&mic, AudioSpec::SPEECH).unwrap();
        let mut buf = vec![0i16; 1600];
        assert_eq!(stream.read(&mut buf).unwrap(), 1600);
    }
}

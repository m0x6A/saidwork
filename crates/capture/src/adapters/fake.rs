use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::domain::{AudioDevice, AudioSpec, CaptureError, Recording, SourceKind};
use crate::ports::{AudioBackend, AudioStream};

#[derive(Debug, Clone)]
enum Source {
    Finite(Arc<Vec<i16>>),
    Endless(i16),
}

/// In-memory backend for tests and headless runs.
///
/// Streams are paced in real time (a chunk takes as long as it would to record)
/// so that the recorder behaves like it does with hardware.
#[derive(Debug, Clone, Default)]
pub struct FakeBackend {
    devices: Vec<(AudioDevice, Source)>,
    realtime: bool,
}

impl FakeBackend {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Pace streams like real hardware instead of returning data as fast as possible.
    #[must_use]
    pub fn realtime(mut self) -> Self {
        self.realtime = true;
        self
    }

    /// Add a device that plays `samples` once and then ends.
    #[must_use]
    pub fn with_device(mut self, kind: SourceKind, id: &str, samples: Vec<i16>) -> Self {
        let is_default = !self.devices.iter().any(|(d, _)| d.kind == kind);
        self.devices.push((
            AudioDevice {
                id: id.to_owned(),
                label: format!("fake {id}"),
                kind,
                is_default,
            },
            Source::Finite(Arc::new(samples)),
        ));
        self
    }

    /// Add a device that produces a constant `value` forever.
    #[must_use]
    pub fn with_endless_device(mut self, kind: SourceKind, id: &str, value: i16) -> Self {
        let is_default = !self.devices.iter().any(|(d, _)| d.kind == kind);
        self.devices.push((
            AudioDevice {
                id: id.to_owned(),
                label: format!("fake {id}"),
                kind,
                is_default,
            },
            Source::Endless(value),
        ));
        self
    }

    /// A single microphone device playing the WAV file at `path` (converted to `spec`) in real time.
    pub fn from_wav_file(path: &Path, spec: AudioSpec) -> Result<Self, CaptureError> {
        let bytes = std::fs::read(path).map_err(|source| CaptureError::Io {
            path: path.display().to_string(),
            source,
        })?;
        let recording = Recording::from_wav(&bytes, spec)?;
        let label = path
            .file_name()
            .map_or_else(|| "file".into(), |n| n.to_string_lossy().into_owned());
        Ok(Self::new()
            .realtime()
            .with_device(SourceKind::Microphone, &label, recording.samples))
    }
}

impl AudioBackend for FakeBackend {
    fn list_devices(&self) -> Result<Vec<AudioDevice>, CaptureError> {
        Ok(self.devices.iter().map(|(d, _)| d.clone()).collect())
    }

    fn open(
        &self,
        device: &AudioDevice,
        spec: AudioSpec,
    ) -> Result<Box<dyn AudioStream>, CaptureError> {
        let (_, source) = self
            .devices
            .iter()
            .find(|(d, _)| d.id == device.id)
            .ok_or_else(|| CaptureError::OpenFailed {
                device: device.id.clone(),
                reason: "unknown fake device".into(),
            })?;
        Ok(Box::new(FakeStream {
            source: source.clone(),
            position: Arc::new(Mutex::new(0)),
            spec,
            realtime: self.realtime,
        }))
    }
}

struct FakeStream {
    source: Source,
    position: Arc<Mutex<usize>>,
    spec: AudioSpec,
    realtime: bool,
}

impl AudioStream for FakeStream {
    fn read(&mut self, buf: &mut [i16]) -> Result<usize, CaptureError> {
        let n = match &self.source {
            Source::Endless(v) => {
                buf.fill(*v);
                buf.len()
            }
            Source::Finite(samples) => {
                let mut pos = self
                    .position
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let remaining = &samples[(*pos).min(samples.len())..];
                let n = remaining.len().min(buf.len());
                buf[..n].copy_from_slice(&remaining[..n]);
                *pos += n;
                n
            }
        };
        if self.realtime && n > 0 {
            let per_sample = Duration::from_secs(1)
                / (self.spec.sample_rate * u32::from(self.spec.channels)).max(1);
            std::thread::sleep(per_sample * u32::try_from(n).unwrap_or(u32::MAX));
        }
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_device_of_each_kind_is_default() {
        let b = FakeBackend::new()
            .with_device(SourceKind::Microphone, "m1", vec![])
            .with_device(SourceKind::Microphone, "m2", vec![])
            .with_endless_device(SourceKind::SystemAudio, "s1", 0);
        let defaults: Vec<_> = b
            .list_devices()
            .unwrap()
            .into_iter()
            .filter(|d| d.is_default)
            .map(|d| d.id)
            .collect();
        assert_eq!(defaults, ["m1", "s1"]);
        assert_eq!(
            b.default_device(SourceKind::SystemAudio)
                .unwrap()
                .unwrap()
                .id,
            "s1"
        );
    }

    #[test]
    fn finite_stream_ends_with_zero() {
        let b = FakeBackend::new().with_device(SourceKind::Microphone, "m", vec![1, 2, 3]);
        let dev = b.list_devices().unwrap().remove(0);
        let mut s = b.open(&dev, AudioSpec::SPEECH).unwrap();
        let mut buf = [0i16; 2];
        assert_eq!(s.read(&mut buf).unwrap(), 2);
        assert_eq!(buf, [1, 2]);
        assert_eq!(s.read(&mut buf).unwrap(), 1);
        assert_eq!(s.read(&mut buf).unwrap(), 0);
    }

    #[test]
    fn unknown_device_fails_to_open() {
        let b = FakeBackend::new();
        let dev = AudioDevice {
            id: "x".into(),
            label: "x".into(),
            kind: SourceKind::Microphone,
            is_default: false,
        };
        let Err(err) = b.open(&dev, AudioSpec::SPEECH) else {
            panic!("expected failure")
        };
        assert!(matches!(err, CaptureError::OpenFailed { .. }));
    }

    #[test]
    fn from_wav_file_loads_and_converts() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("a.wav");
        let rec = Recording {
            id: crate::domain::RecordingId::random(),
            started_at: chrono::Utc::now(),
            spec: AudioSpec {
                sample_rate: 8_000,
                channels: 1,
            },
            sources: vec![],
            samples: vec![5; 80],
        };
        std::fs::write(&path, rec.to_wav().unwrap()).unwrap();
        let b = FakeBackend::from_wav_file(&path, AudioSpec::SPEECH).unwrap();
        let dev = b.default_device(SourceKind::Microphone).unwrap().unwrap();
        assert_eq!(dev.label, "fake a.wav");
        let mut s = b.open(&dev, AudioSpec::SPEECH).unwrap();
        let mut buf = vec![0i16; 1000];
        assert_eq!(s.read(&mut buf).unwrap(), 160);
    }
}

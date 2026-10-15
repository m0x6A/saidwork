use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use chrono::Utc;

use crate::domain::{AudioDevice, AudioSpec, CaptureError, Recording, RecordingId};
use crate::mixer::{mix_into, rms_level};
use crate::ports::AudioBackend;

/// Size of one mixing tick.
const CHUNK: Duration = Duration::from_millis(100);

enum Chunk {
    Samples(Vec<i16>),
    End,
}

/// Captures one or more devices concurrently and mixes them into one stream.
#[derive(Debug, Clone, Copy, Default)]
pub struct Recorder;

impl Recorder {
    /// Open every device in `sources` and start recording at `spec`.
    ///
    /// Each stream is read on its own thread; a mixer thread sums one chunk from
    /// every source per tick. A source that produces nothing for a while (e.g. a
    /// monitor of an idle sink) contributes silence for that tick instead of stalling.
    pub fn start(
        backend: &dyn AudioBackend,
        sources: Vec<AudioDevice>,
        spec: AudioSpec,
    ) -> Result<RecorderHandle, CaptureError> {
        if sources.is_empty() {
            return Err(CaptureError::NoSources);
        }
        let stop = Arc::new(AtomicBool::new(false));
        let chunk_len = spec.samples_for(CHUNK);
        let mut receivers = Vec::with_capacity(sources.len());
        let mut readers = Vec::with_capacity(sources.len());
        for device in &sources {
            let mut stream = backend.open(device, spec)?;
            let (tx, rx) = mpsc::channel();
            let stop = stop.clone();
            let label = device.label.clone();
            readers.push(std::thread::spawn(move || {
                reader_loop(&mut *stream, chunk_len, &tx, &stop, &label);
            }));
            receivers.push(rx);
        }

        let level = Arc::new(Mutex::new(0.0f32));
        let finished = Arc::new(AtomicBool::new(false));
        let mixer = {
            let stop = stop.clone();
            let level = level.clone();
            let finished = finished.clone();
            std::thread::spawn(move || {
                let samples = mixer_loop(&receivers, chunk_len, &stop, &level);
                finished.store(true, Ordering::SeqCst);
                samples
            })
        };

        Ok(RecorderHandle {
            id: RecordingId::random(),
            started_at: Utc::now(),
            started: Instant::now(),
            spec,
            sources,
            stop,
            finished,
            level,
            readers,
            mixer,
        })
    }
}

fn reader_loop(
    stream: &mut dyn crate::ports::AudioStream,
    chunk_len: usize,
    tx: &Sender<Chunk>,
    stop: &AtomicBool,
    label: &str,
) {
    let mut buf = vec![0i16; chunk_len];
    while !stop.load(Ordering::SeqCst) {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if tx.send(Chunk::Samples(buf[..n].to_vec())).is_err() {
                    break;
                }
            }
            Err(err) => {
                tracing::error!(device = label, %err, "audio read failed; treating source as ended");
                break;
            }
        }
    }
    let _ = tx.send(Chunk::End);
}

fn mixer_loop(
    receivers: &[Receiver<Chunk>],
    chunk_len: usize,
    stop: &AtomicBool,
    level: &Mutex<f32>,
) -> Vec<i16> {
    let mut out = Vec::new();
    let mut ended = vec![false; receivers.len()];
    let mut mixed = vec![0i16; chunk_len];
    while !stop.load(Ordering::SeqCst) {
        mixed.fill(0);
        let mut got_any = false;
        for (i, rx) in receivers.iter().enumerate() {
            if ended[i] {
                continue;
            }
            match rx.recv_timeout(CHUNK * 2) {
                Ok(Chunk::Samples(chunk)) => {
                    mix_into(&mut mixed, &chunk);
                    got_any = true;
                }
                Ok(Chunk::End) | Err(RecvTimeoutError::Disconnected) => ended[i] = true,
                Err(RecvTimeoutError::Timeout) => {
                    tracing::debug!(source = i, "no audio this tick; mixing silence");
                }
            }
        }
        if ended.iter().all(|&e| e) {
            break;
        }
        if got_any {
            out.extend_from_slice(&mixed);
            *level
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = rms_level(&mixed);
        }
    }
    out
}

/// A recording in progress. Dropping it stops the capture and discards the audio.
#[derive(Debug)]
pub struct RecorderHandle {
    id: RecordingId,
    started_at: chrono::DateTime<Utc>,
    started: Instant,
    spec: AudioSpec,
    sources: Vec<AudioDevice>,
    stop: Arc<AtomicBool>,
    finished: Arc<AtomicBool>,
    level: Arc<Mutex<f32>>,
    readers: Vec<JoinHandle<()>>,
    mixer: JoinHandle<Vec<i16>>,
}

impl RecorderHandle {
    #[must_use]
    pub fn id(&self) -> RecordingId {
        self.id
    }

    #[must_use]
    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    /// RMS level of the most recent mixed chunk, `0.0..=1.0`.
    #[must_use]
    pub fn level(&self) -> f32 {
        *self
            .level
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// `true` once every source has ended on its own (e.g. a file was played to the end).
    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.finished.load(Ordering::SeqCst)
    }

    /// Stop capturing and return the mixed audio. Blocks until the threads are joined.
    pub fn stop(self) -> Result<Recording, CaptureError> {
        self.stop.store(true, Ordering::SeqCst);
        let samples = self
            .mixer
            .join()
            .map_err(|_| CaptureError::Read("mixer thread panicked".into()))?;
        for reader in self.readers {
            // Readers may be blocked inside the backend; they exit after their current read.
            let _ = reader.join();
        }
        Ok(Recording {
            id: self.id,
            started_at: self.started_at,
            spec: self.spec,
            sources: self.sources,
            samples,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::FakeBackend;
    use crate::domain::SourceKind;

    #[test]
    fn mixes_two_finite_sources_and_finishes() {
        let spec = AudioSpec {
            sample_rate: 1_000,
            channels: 1,
        };
        let chunk = spec.samples_for(CHUNK); // 100 samples
        let backend = FakeBackend::new()
            .with_device(SourceKind::Microphone, "mic", vec![100; chunk * 2])
            .with_device(SourceKind::SystemAudio, "mon", vec![-30; chunk * 3]);
        let devices = backend.list_devices().unwrap();
        let handle = Recorder::start(&backend, devices, spec).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !handle.is_finished() {
            assert!(Instant::now() < deadline, "recorder did not finish");
            std::thread::sleep(Duration::from_millis(5));
        }
        let rec = handle.stop().unwrap();
        assert_eq!(rec.samples.len(), chunk * 3);
        assert!(
            rec.samples[..chunk * 2].iter().all(|&s| s == 70),
            "both sources mixed"
        );
        assert!(
            rec.samples[chunk * 2..].iter().all(|&s| s == -30),
            "mic ended, monitor continues"
        );
        assert_eq!(rec.sources.len(), 2);
    }

    #[test]
    fn stop_interrupts_an_endless_source() {
        let spec = AudioSpec {
            sample_rate: 1_000,
            channels: 1,
        };
        let backend = FakeBackend::new().with_endless_device(SourceKind::Microphone, "mic", 7);
        let devices = backend.list_devices().unwrap();
        let handle = Recorder::start(&backend, devices, spec).unwrap();
        std::thread::sleep(Duration::from_millis(250));
        assert!(handle.level() > 0.0);
        assert!(!handle.is_finished());
        let rec = handle.stop().unwrap();
        assert_ne!(rec.samples.len(), 0);
        assert!(rec.samples.iter().all(|&s| s == 7));
    }

    #[test]
    fn no_sources_is_an_error() {
        let backend = FakeBackend::new();
        assert!(matches!(
            Recorder::start(&backend, vec![], AudioSpec::SPEECH).unwrap_err(),
            CaptureError::NoSources
        ));
    }
}

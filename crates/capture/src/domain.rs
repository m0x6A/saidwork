use std::fmt;
use std::io::Cursor;
use std::time::Duration;

use chrono::{DateTime, Utc};
use uuid::Uuid;

/// Sample layout of a stream. Everything inside the slice is signed 16-bit PCM.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AudioSpec {
    pub sample_rate: u32,
    pub channels: u16,
}

impl AudioSpec {
    /// 16 kHz mono: what speech models expect, and small on the wire.
    pub const SPEECH: Self = Self {
        sample_rate: 16_000,
        channels: 1,
    };

    #[must_use]
    pub fn samples_for(&self, duration: Duration) -> usize {
        let per_second = u64::from(self.sample_rate) * u64::from(self.channels);
        usize::try_from(duration.as_millis() * u128::from(per_second) / 1000).unwrap_or(usize::MAX)
    }
}

impl Default for AudioSpec {
    fn default() -> Self {
        Self::SPEECH
    }
}

/// What a device captures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SourceKind {
    Microphone,
    /// Everything the computer plays (a sink monitor / loopback).
    SystemAudio,
}

impl fmt::Display for SourceKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Microphone => "microphone",
            Self::SystemAudio => "system audio",
        })
    }
}

/// A capturable device as reported by the backend.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AudioDevice {
    /// Backend-specific identifier (PulseAudio source name, ...).
    pub id: String,
    pub label: String,
    pub kind: SourceKind,
    pub is_default: bool,
}

/// Identifier of a recording.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RecordingId(Uuid);

impl RecordingId {
    #[must_use]
    pub fn random() -> Self {
        Self(Uuid::new_v4())
    }
}

impl fmt::Display for RecordingId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0.simple())
    }
}

/// A finished capture: mixed PCM samples plus metadata.
#[derive(Clone, PartialEq, Eq)]
pub struct Recording {
    pub id: RecordingId,
    pub started_at: DateTime<Utc>,
    pub spec: AudioSpec,
    pub sources: Vec<AudioDevice>,
    pub samples: Vec<i16>,
}

impl fmt::Debug for Recording {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Recording")
            .field("id", &self.id)
            .field("started_at", &self.started_at)
            .field("spec", &self.spec)
            .field("sources", &self.sources.len())
            .field("duration", &self.duration())
            .finish_non_exhaustive()
    }
}

impl Recording {
    #[must_use]
    pub fn duration(&self) -> Duration {
        let frames = self.samples.len() as u64 / u64::from(self.spec.channels.max(1));
        Duration::from_millis(frames * 1000 / u64::from(self.spec.sample_rate.max(1)))
    }

    #[must_use]
    pub fn is_silent(&self) -> bool {
        self.samples.iter().all(|&s| s == 0)
    }

    /// Encode as a 16-bit PCM WAV file.
    pub fn to_wav(&self) -> Result<Vec<u8>, CaptureError> {
        let spec = hound::WavSpec {
            channels: self.spec.channels,
            sample_rate: self.spec.sample_rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut writer =
                hound::WavWriter::new(&mut cursor, spec).map_err(|e| CaptureError::wav(&e))?;
            let mut w = writer.get_i16_writer(
                u32::try_from(self.samples.len()).map_err(|_| CaptureError::TooLong)?,
            );
            for &s in &self.samples {
                w.write_sample(s);
            }
            w.flush().map_err(|e| CaptureError::wav(&e))?;
            writer.finalize().map_err(|e| CaptureError::wav(&e))?;
        }
        Ok(cursor.into_inner())
    }

    /// Decode a WAV file (any common PCM layout) into a recording at `target`.
    ///
    /// Multi-channel input is averaged to mono and the sample rate is converted with
    /// linear interpolation. Good enough for feeding test audio; not a mastering tool.
    pub fn from_wav(bytes: &[u8], target: AudioSpec) -> Result<Self, CaptureError> {
        let mut reader =
            hound::WavReader::new(Cursor::new(bytes)).map_err(|e| CaptureError::wav(&e))?;
        let spec = reader.spec();
        let samples: Vec<i16> = match (spec.sample_format, spec.bits_per_sample) {
            (hound::SampleFormat::Int, 16) => reader
                .samples::<i16>()
                .collect::<Result<_, _>>()
                .map_err(|e| CaptureError::wav(&e))?,
            (hound::SampleFormat::Int, bits @ (8 | 24 | 32)) => {
                let shift = i32::from(bits) - 16;
                reader
                    .samples::<i32>()
                    .map(|s| {
                        s.map(|v| if shift >= 0 { v >> shift } else { v << -shift })
                            .map(|v| {
                                i16::try_from(v).unwrap_or(if v < 0 { i16::MIN } else { i16::MAX })
                            })
                    })
                    .collect::<Result<_, _>>()
                    .map_err(|e| CaptureError::wav(&e))?
            }
            (hound::SampleFormat::Float, 32) => reader
                .samples::<f32>()
                .map(|s| s.map(|v| (v.clamp(-1.0, 1.0) * f32::from(i16::MAX)) as i16))
                .collect::<Result<_, _>>()
                .map_err(|e| CaptureError::wav(&e))?,
            (format, bits) => {
                return Err(CaptureError::UnsupportedWav {
                    format: format!("{format:?}"),
                    bits,
                });
            }
        };
        let mono = downmix(&samples, spec.channels);
        let resampled = resample(&mono, spec.sample_rate, target.sample_rate);
        let samples = if target.channels == 1 {
            resampled
        } else {
            resampled
                .iter()
                .flat_map(|&s| std::iter::repeat_n(s, usize::from(target.channels)))
                .collect()
        };
        Ok(Self {
            id: RecordingId::random(),
            started_at: Utc::now(),
            spec: target,
            sources: Vec::new(),
            samples,
        })
    }
}

fn downmix(samples: &[i16], channels: u16) -> Vec<i16> {
    let n = usize::from(channels.max(1));
    if n == 1 {
        return samples.to_vec();
    }
    samples
        .chunks(n)
        .map(|frame| {
            let sum: i64 = frame.iter().map(|&s| i64::from(s)).sum();
            i16::try_from(sum / frame.len() as i64).unwrap_or(0)
        })
        .collect()
}

/// Linear-interpolation resampling of mono PCM.
#[must_use]
pub fn resample(samples: &[i16], from_rate: u32, to_rate: u32) -> Vec<i16> {
    if from_rate == to_rate || samples.is_empty() || from_rate == 0 || to_rate == 0 {
        return samples.to_vec();
    }
    let ratio = f64::from(from_rate) / f64::from(to_rate);
    let out_len = ((samples.len() as f64) / ratio).round().max(1.0) as usize;
    (0..out_len)
        .map(|i| {
            let pos = i as f64 * ratio;
            let idx = pos.floor() as usize;
            let frac = pos - idx as f64;
            let a = f64::from(samples[idx.min(samples.len() - 1)]);
            let b = f64::from(samples[(idx + 1).min(samples.len() - 1)]);
            (a + (b - a) * frac)
                .round()
                .clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16
        })
        .collect()
}

/// Errors of the capture slice.
#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    #[error("audio backend unavailable: {0}")]
    BackendUnavailable(String),
    #[error("no {0} device available")]
    NoDevice(SourceKind),
    #[error("could not open device `{device}`: {reason}")]
    OpenFailed { device: String, reason: String },
    #[error("error while reading audio: {0}")]
    Read(String),
    #[error("nothing to record: no sources were selected")]
    NoSources,
    #[error("recording is longer than a WAV file can hold")]
    TooLong,
    #[error("unsupported WAV layout: {format} {bits}-bit")]
    UnsupportedWav { format: String, bits: u16 },
    #[error("WAV error: {0}")]
    Wav(String),
    #[error("could not read audio file `{path}`: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
}

impl CaptureError {
    fn wav(e: &hound::Error) -> Self {
        Self::Wav(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recording(samples: Vec<i16>, spec: AudioSpec) -> Recording {
        Recording {
            id: RecordingId::random(),
            started_at: Utc::now(),
            spec,
            sources: vec![],
            samples,
        }
    }

    #[test]
    fn wav_round_trip_preserves_samples() {
        let original = recording(vec![0, 1000, -1000, i16::MAX, i16::MIN], AudioSpec::SPEECH);
        let bytes = original.to_wav().unwrap();
        assert_eq!(&bytes[..4], b"RIFF");
        let decoded = Recording::from_wav(&bytes, AudioSpec::SPEECH).unwrap();
        assert_eq!(decoded.samples, original.samples);
        assert_eq!(decoded.spec, AudioSpec::SPEECH);
    }

    #[test]
    fn from_wav_downmixes_and_resamples() {
        let stereo_48k = recording(
            vec![100, 300, 100, 300, 100, 300, 100, 300, 100, 300, 100, 300],
            AudioSpec {
                sample_rate: 48_000,
                channels: 2,
            },
        );
        let bytes = stereo_48k.to_wav().unwrap();
        let decoded = Recording::from_wav(&bytes, AudioSpec::SPEECH).unwrap();
        assert_eq!(decoded.spec, AudioSpec::SPEECH);
        assert_eq!(decoded.samples.len(), 2, "6 frames at 48k -> 2 at 16k");
        assert!(
            decoded.samples.iter().all(|&s| s == 200),
            "{:?}",
            decoded.samples
        );
    }

    #[test]
    fn duration_and_silence() {
        let r = recording(vec![0; 16_000], AudioSpec::SPEECH);
        assert_eq!(r.duration(), Duration::from_secs(1));
        assert!(r.is_silent());
        assert!(!recording(vec![0, 1], AudioSpec::SPEECH).is_silent());
        assert_eq!(
            AudioSpec::SPEECH.samples_for(Duration::from_millis(100)),
            1600
        );
    }

    #[test]
    fn resample_identity_and_ratio() {
        let s: Vec<i16> = (0..100).collect();
        assert_eq!(resample(&s, 16_000, 16_000), s);
        let half = resample(&s, 16_000, 8_000);
        assert_eq!(half.len(), 50);
        assert_eq!(half[1], 2);
        let double = resample(&s, 8_000, 16_000);
        assert_eq!(double.len(), 200);
        assert_eq!(
            double[1], 1,
            "interpolated midpoint between 0 and 1 rounds to 1"
        );
    }

    #[test]
    fn garbage_is_a_wav_error() {
        assert!(matches!(
            Recording::from_wav(b"nope", AudioSpec::SPEECH).unwrap_err(),
            CaptureError::Wav(_)
        ));
    }
}

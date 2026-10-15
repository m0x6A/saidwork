use std::time::Duration;

/// Audio handed to a transcriber: a complete WAV file in memory.
#[derive(Clone, PartialEq, Eq)]
pub struct AudioInput {
    pub wav_bytes: Vec<u8>,
    /// File name sent to the server (some servers sniff the extension).
    pub file_name: String,
}

impl std::fmt::Debug for AudioInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudioInput")
            .field("bytes", &self.wav_bytes.len())
            .field("file_name", &self.file_name)
            .finish()
    }
}

impl AudioInput {
    #[must_use]
    pub fn wav(wav_bytes: Vec<u8>) -> Self {
        Self {
            wav_bytes,
            file_name: "recording.wav".to_owned(),
        }
    }
}

/// Hints for the transcriber.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TranscriptionRequest {
    /// ISO-639-1 language code such as `en` or `sv`; `None` lets the model detect it.
    pub language: Option<String>,
    /// Free-text prompt with vocabulary or style hints.
    pub prompt: Option<String>,
}

/// One timed chunk of a transcript.
#[derive(Debug, Clone, PartialEq)]
pub struct Segment {
    pub start: Duration,
    pub end: Duration,
    pub text: String,
}

/// Result of transcribing a recording.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Transcript {
    pub text: String,
    pub language: Option<String>,
    pub segments: Vec<Segment>,
}

impl Transcript {
    /// Whole transcript as plain text; one segment per line when segments exist.
    #[must_use]
    pub fn as_plain_text(&self) -> String {
        if self.segments.is_empty() {
            self.text.trim().to_owned()
        } else {
            self.segments
                .iter()
                .map(|s| s.text.trim())
                .filter(|t| !t.is_empty())
                .collect::<Vec<_>>()
                .join("\n")
        }
    }
}

/// Errors of the transcription slice.
#[derive(Debug, thiserror::Error)]
pub enum TranscriptionError {
    #[error("transcription request failed: {0}")]
    Transport(#[source] reqwest::Error),
    #[error("transcription server rejected the credentials (HTTP {status})")]
    Unauthorized { status: u16 },
    #[error("transcription server answered HTTP {status}: {body}")]
    Status { status: u16, body: String },
    #[error("could not decode transcription response: {0}")]
    Decode(String),
    #[error("the transcription came back empty")]
    Empty,
}

impl TranscriptionError {
    /// Whether retrying the same request could plausibly succeed.
    #[must_use]
    pub fn is_transient(&self) -> bool {
        match self {
            Self::Transport(e) => e.is_timeout() || e.is_connect() || e.is_request(),
            Self::Status { status, .. } => *status == 429 || *status >= 500,
            Self::Unauthorized { .. } | Self::Decode(_) | Self::Empty => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_prefers_segments() {
        let t = Transcript {
            text: "ignored".into(),
            language: None,
            segments: vec![
                Segment {
                    start: Duration::ZERO,
                    end: Duration::from_secs(1),
                    text: " a ".into(),
                },
                Segment {
                    start: Duration::ZERO,
                    end: Duration::from_secs(1),
                    text: String::new(),
                },
                Segment {
                    start: Duration::ZERO,
                    end: Duration::from_secs(1),
                    text: "b".into(),
                },
            ],
        };
        assert_eq!(t.as_plain_text(), "a\nb");
        let plain = Transcript {
            text: " hi ".into(),
            ..Default::default()
        };
        assert_eq!(plain.as_plain_text(), "hi");
    }

    #[test]
    fn transient_classification() {
        assert!(
            TranscriptionError::Status {
                status: 503,
                body: String::new()
            }
            .is_transient()
        );
        assert!(
            TranscriptionError::Status {
                status: 429,
                body: String::new()
            }
            .is_transient()
        );
        assert!(
            !TranscriptionError::Status {
                status: 400,
                body: String::new()
            }
            .is_transient()
        );
        assert!(!TranscriptionError::Unauthorized { status: 401 }.is_transient());
        assert!(!TranscriptionError::Empty.is_transient());
    }
}

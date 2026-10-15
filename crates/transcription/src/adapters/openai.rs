use std::time::Duration;

use async_trait::async_trait;
use reqwest::multipart::{Form, Part};
use saidwork_kernel::retry::{RetryPolicy, retry};
use saidwork_kernel::urls::join_path;
use saidwork_kernel::{ExposeSecret, SecretString};
use serde::Deserialize;
use url::Url;

use crate::domain::{AudioInput, Segment, Transcript, TranscriptionError, TranscriptionRequest};
use crate::ports::Transcriber;

/// Client for the OpenAI `POST /v1/audio/transcriptions` API.
///
/// Also works against whisper.cpp's `whisper-server`, faster-whisper-server, Groq
/// and other compatible servers.
pub struct OpenAiTranscriber {
    client: reqwest::Client,
    endpoint: Url,
    api_key: Option<SecretString>,
    model: String,
    retry: RetryPolicy,
}

impl std::fmt::Debug for OpenAiTranscriber {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenAiTranscriber")
            .field("endpoint", &self.endpoint.as_str())
            .field("model", &self.model)
            .field("has_api_key", &self.api_key.is_some())
            .finish_non_exhaustive()
    }
}

impl OpenAiTranscriber {
    /// `base_url` is the server root; `/v1/audio/transcriptions` is appended.
    #[must_use]
    pub fn new(base_url: &Url, api_key: Option<SecretString>, model: impl Into<String>) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(600))
            .connect_timeout(Duration::from_secs(10))
            .build()
            .expect("static reqwest client configuration is valid");
        Self {
            client,
            endpoint: join_path(base_url, "v1/audio/transcriptions"),
            api_key,
            model: model.into(),
            retry: RetryPolicy::default_http(),
        }
    }

    #[must_use]
    pub fn with_retry_policy(mut self, retry: RetryPolicy) -> Self {
        self.retry = retry;
        self
    }

    fn form(
        &self,
        audio: &AudioInput,
        request: &TranscriptionRequest,
    ) -> Result<Form, TranscriptionError> {
        let file = Part::bytes(audio.wav_bytes.clone())
            .file_name(audio.file_name.clone())
            .mime_str("audio/wav")
            .map_err(|e| TranscriptionError::Decode(e.to_string()))?;
        let mut form = Form::new()
            .part("file", file)
            .text("model", self.model.clone())
            .text("response_format", "verbose_json");
        if let Some(lang) = &request.language {
            form = form.text("language", lang.clone());
        }
        if let Some(prompt) = &request.prompt {
            form = form.text("prompt", prompt.clone());
        }
        Ok(form)
    }

    async fn send_once(
        &self,
        audio: &AudioInput,
        request: &TranscriptionRequest,
    ) -> Result<Transcript, TranscriptionError> {
        let mut req = self
            .client
            .post(self.endpoint.clone())
            .multipart(self.form(audio, request)?);
        if let Some(key) = &self.api_key {
            req = req.bearer_auth(key.expose_secret());
        }
        let resp = req.send().await.map_err(TranscriptionError::Transport)?;
        let status = resp.status();
        let body = resp.text().await.map_err(TranscriptionError::Transport)?;
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(TranscriptionError::Unauthorized {
                status: status.as_u16(),
            });
        }
        if !status.is_success() {
            return Err(TranscriptionError::Status {
                status: status.as_u16(),
                body: truncate(&body),
            });
        }
        let parsed: VerboseJson =
            serde_json::from_str(&body).map_err(|e| TranscriptionError::Decode(e.to_string()))?;
        let transcript = Transcript::from(parsed);
        if transcript.as_plain_text().is_empty() {
            return Err(TranscriptionError::Empty);
        }
        Ok(transcript)
    }
}

#[async_trait]
impl Transcriber for OpenAiTranscriber {
    async fn transcribe(
        &self,
        audio: &AudioInput,
        request: &TranscriptionRequest,
    ) -> Result<Transcript, TranscriptionError> {
        tracing::info!(endpoint = %self.endpoint, model = %self.model, bytes = audio.wav_bytes.len(), "transcribing");
        retry(self.retry, TranscriptionError::is_transient, |_| {
            self.send_once(audio, request)
        })
        .await
    }
}

fn truncate(body: &str) -> String {
    const MAX: usize = 512;
    if body.len() <= MAX {
        body.to_owned()
    } else {
        let cut = body
            .char_indices()
            .map(|(i, _)| i)
            .take_while(|&i| i <= MAX)
            .last()
            .unwrap_or(0);
        format!("{}…", &body[..cut])
    }
}

/// Response shape for `response_format=verbose_json`; servers that only honour
/// `json` still produce `text`, so everything else is optional.
#[derive(Debug, Deserialize)]
struct VerboseJson {
    text: String,
    #[serde(default)]
    language: Option<String>,
    #[serde(default)]
    segments: Vec<JsonSegment>,
}

#[derive(Debug, Deserialize)]
struct JsonSegment {
    start: f64,
    end: f64,
    text: String,
}

impl From<VerboseJson> for Transcript {
    fn from(v: VerboseJson) -> Self {
        Self {
            text: v.text,
            language: v.language.filter(|l| !l.is_empty()),
            segments: v
                .segments
                .into_iter()
                .map(|s| Segment {
                    start: Duration::from_secs_f64(s.start.max(0.0)),
                    end: Duration::from_secs_f64(s.end.max(0.0)),
                    text: s.text,
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use wiremock::matchers::{header, header_exists, method, path};
    use wiremock::{Mock, MockServer, Request, ResponseTemplate};

    use super::*;

    fn transcriber(server: &MockServer, key: Option<&str>) -> OpenAiTranscriber {
        OpenAiTranscriber::new(
            &Url::parse(&server.uri()).unwrap(),
            key.map(SecretString::from),
            "whisper-1",
        )
        .with_retry_policy(RetryPolicy {
            attempts: 3,
            initial_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(1),
        })
    }

    fn audio() -> AudioInput {
        AudioInput::wav(b"RIFF....WAVEfmt ".to_vec())
    }

    fn body_text(req: &Request) -> String {
        String::from_utf8_lossy(&req.body).into_owned()
    }

    #[tokio::test]
    async fn posts_multipart_with_model_and_file_and_bearer() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/audio/transcriptions"))
            .and(header("authorization", "Bearer sk-1"))
            .and(header_exists("content-type"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "text": "hello world", "language": "en",
                "segments": [{"id": 0, "start": 0.0, "end": 1.5, "text": " hello"}, {"id": 1, "start": 1.5, "end": 2.0, "text": " world"}]
            })))
            .expect(1)
            .mount(&server)
            .await;
        let t = transcriber(&server, Some("sk-1"))
            .transcribe(
                &audio(),
                &TranscriptionRequest {
                    language: Some("en".into()),
                    prompt: Some("saidwork".into()),
                },
            )
            .await
            .unwrap();
        assert_eq!(t.text, "hello world");
        assert_eq!(t.language.as_deref(), Some("en"));
        assert_eq!(t.segments.len(), 2);
        assert_eq!(t.segments[1].start, Duration::from_millis(1500));
        assert_eq!(t.as_plain_text(), "hello\nworld");

        let req = &server.received_requests().await.unwrap()[0];
        let body = body_text(req);
        for needle in [
            "name=\"file\"",
            "filename=\"recording.wav\"",
            "name=\"model\"",
            "whisper-1",
            "verbose_json",
            "name=\"language\"",
            "name=\"prompt\"",
            "RIFF",
        ] {
            assert!(
                body.contains(needle),
                "multipart body missing {needle}: {body}"
            );
        }
    }

    #[tokio::test]
    async fn keeps_base_path_and_omits_auth_without_key() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/openai/v1/audio/transcriptions"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"text": "x"})),
            )
            .expect(1)
            .mount(&server)
            .await;
        let base = Url::parse(&format!("{}/openai", server.uri())).unwrap();
        let t = OpenAiTranscriber::new(&base, None, "m")
            .transcribe(&audio(), &TranscriptionRequest::default())
            .await
            .unwrap();
        assert_eq!(t.text, "x");
        assert_eq!(t.segments, Vec::<Segment>::new());
        let req = &server.received_requests().await.unwrap()[0];
        assert!(!req.headers.contains_key("authorization"));
    }

    #[tokio::test]
    async fn unauthorized_is_not_retried() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(401))
            .expect(1)
            .mount(&server)
            .await;
        let err = transcriber(&server, None)
            .transcribe(&audio(), &TranscriptionRequest::default())
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            TranscriptionError::Unauthorized { status: 401 }
        ));
    }

    #[tokio::test]
    async fn server_errors_are_retried_then_surface() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(503).set_body_string("busy"))
            .expect(3)
            .mount(&server)
            .await;
        let err = transcriber(&server, None)
            .transcribe(&audio(), &TranscriptionRequest::default())
            .await
            .unwrap_err();
        assert!(
            matches!(err, TranscriptionError::Status { status: 503, ref body } if body == "busy"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn malformed_json_and_empty_text_are_errors() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/audio/transcriptions"))
            .respond_with(ResponseTemplate::new(200).set_body_string("<html>"))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        let err = transcriber(&server, None)
            .transcribe(&audio(), &TranscriptionRequest::default())
            .await
            .unwrap_err();
        assert!(matches!(err, TranscriptionError::Decode(_)), "{err}");

        server.reset().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"text": "  "})),
            )
            .mount(&server)
            .await;
        let err = transcriber(&server, None)
            .transcribe(&audio(), &TranscriptionRequest::default())
            .await
            .unwrap_err();
        assert!(matches!(err, TranscriptionError::Empty), "{err}");
    }
}

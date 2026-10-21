use async_trait::async_trait;
use saidwork_kernel::SecretString;
use saidwork_kernel::retry::{RetryPolicy, retry};
use saidwork_kernel::urls::join_path;
use serde::{Deserialize, Serialize};
use url::Url;

use super::http::{self, Message, TEMPERATURE};
use crate::domain::{RewriteError, RewriteRequest, RewrittenNote};
use crate::ports::Rewriter;
use crate::prompt::{build_prompt, post_process};

/// Client for Ollama's native `POST /api/chat`.
pub struct OllamaRewriter {
    client: reqwest::Client,
    endpoint: Url,
    api_key: Option<SecretString>,
    model: String,
    context_window: Option<u32>,
    retry: RetryPolicy,
}

impl std::fmt::Debug for OllamaRewriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OllamaRewriter")
            .field("endpoint", &self.endpoint.as_str())
            .field("model", &self.model)
            .field("context_window", &self.context_window)
            .finish_non_exhaustive()
    }
}

impl OllamaRewriter {
    /// `base_url` is the Ollama root (usually `http://127.0.0.1:11434`).
    #[must_use]
    pub fn new(base_url: &Url, api_key: Option<SecretString>, model: impl Into<String>) -> Self {
        Self {
            client: http::client(),
            endpoint: join_path(base_url, "api/chat"),
            api_key,
            model: model.into(),
            context_window: None,
            retry: RetryPolicy::default_http(),
        }
    }

    /// Ask Ollama for a context window of this many tokens (`num_ctx`); `None` keeps its default.
    #[must_use]
    pub fn with_context_window(mut self, tokens: Option<u32>) -> Self {
        self.context_window = tokens;
        self
    }

    #[must_use]
    pub fn with_retry_policy(mut self, retry: RetryPolicy) -> Self {
        self.retry = retry;
        self
    }

    async fn send_once(&self, request: &RewriteRequest) -> Result<RewrittenNote, RewriteError> {
        let prompt = build_prompt(request);
        let body = ChatRequest {
            model: &self.model,
            messages: vec![
                Message {
                    role: "system",
                    content: &prompt.system,
                },
                Message {
                    role: "user",
                    content: &prompt.user,
                },
            ],
            stream: false,
            think: false,
            options: Options {
                temperature: TEMPERATURE,
                num_ctx: self.context_window,
            },
        };
        let resp: ChatResponse =
            http::post_json(&self.client, &self.endpoint, self.api_key.as_ref(), &body).await?;
        let content = resp
            .message
            .and_then(|m| m.content)
            .filter(|c| !c.trim().is_empty())
            .ok_or(RewriteError::Empty)?;
        let mut note = post_process(&content);
        let prompt_chars = prompt.system.chars().count() + prompt.user.chars().count();
        if let Some(warning) =
            truncation_warning(prompt_chars, self.context_window, resp.prompt_eval_count)
        {
            tracing::warn!(model = %self.model, "{warning}");
            note.warnings.push(warning);
        }
        Ok(note)
    }
}

#[async_trait]
impl Rewriter for OllamaRewriter {
    async fn rewrite(&self, request: &RewriteRequest) -> Result<RewrittenNote, RewriteError> {
        tracing::info!(endpoint = %self.endpoint, model = %self.model, format = %request.format.name, "rewriting");
        retry(self.retry, RewriteError::is_transient, |_| {
            self.send_once(request)
        })
        .await
    }
}

#[derive(Debug, Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: Vec<Message<'a>>,
    stream: bool,
    /// Disable "thinking" output on reasoning models so the answer is the document itself.
    think: bool,
    options: Options,
}

/// Fewer prompt tokens than one per this many characters means text was dropped: real text
/// runs at roughly three to five characters per token, so this leaves a wide margin.
const MAX_CHARS_PER_TOKEN: usize = 8;

/// Ollama silently keeps only the end of a prompt that does not fit its context window, so
/// compare the prompt size it reports with what was sent and with the requested window.
fn truncation_warning(
    prompt_chars: usize,
    context_window: Option<u32>,
    prompt_tokens: Option<u32>,
) -> Option<String> {
    let tokens = prompt_tokens?;
    let filled =
        context_window.is_some_and(|window| u64::from(tokens) * 100 >= u64::from(window) * 95);
    let shrunk = (tokens as usize).saturating_mul(MAX_CHARS_PER_TOKEN) < prompt_chars;
    (filled || shrunk).then(|| {
        let window = context_window.map_or_else(
            || "the server's default context window".to_owned(),
            |w| format!("the {w}-token context window"),
        );
        format!(
            "the prompt ({prompt_chars} characters) was evaluated as {tokens} tokens, so it \
             probably did not fit {window}. Ollama keeps only the end of a prompt that does not \
             fit, so the note may cover just the last part of the transcript; set a larger \
             `context_window` for this provider."
        )
    })
}

#[derive(Debug, Serialize)]
struct Options {
    temperature: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    num_ctx: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct ChatResponse {
    #[serde(default)]
    message: Option<ResponseMessage>,
    /// Number of prompt tokens the model evaluated, after any truncation.
    #[serde(default)]
    prompt_eval_count: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct ResponseMessage {
    #[serde(default)]
    content: Option<String>,
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use wiremock::matchers::{body_partial_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;
    use crate::domain::FormatSpec;

    fn request() -> RewriteRequest {
        RewriteRequest {
            transcript: "buy milk".into(),
            format: FormatSpec {
                name: "Tasks".into(),
                template: "- [ ] task".into(),
            },
            date: "2026-10-14".into(),
            context: None,
        }
    }

    #[tokio::test]
    async fn sends_native_chat_request() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .and(body_partial_json(serde_json::json!({"model": "qwen3", "stream": false, "think": false, "options": {"temperature": 0.2}})))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "model": "qwen3", "message": {"role": "assistant", "content": "# Tasks\n- [ ] buy milk"}, "done": true
            })))
            .expect(1)
            .mount(&server)
            .await;
        let rewriter = OllamaRewriter::new(&Url::parse(&server.uri()).unwrap(), None, "qwen3");
        let note = rewriter.rewrite(&request()).await.unwrap();
        assert_eq!(note.title.as_deref(), Some("Tasks"));
        assert!(note.markdown.contains("- [ ] buy milk"));
        let req = &server.received_requests().await.unwrap()[0];
        assert!(!req.headers.contains_key("authorization"));
    }

    #[tokio::test]
    async fn sends_context_window_and_warns_on_truncation() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .and(body_partial_json(
                serde_json::json!({"options": {"num_ctx": 4096}}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "message": {"role": "assistant", "content": "# Tasks"},
                "prompt_eval_count": 4096,
                "done": true
            })))
            .expect(1)
            .mount(&server)
            .await;
        let rewriter = OllamaRewriter::new(&Url::parse(&server.uri()).unwrap(), None, "m")
            .with_context_window(Some(4096));
        let note = rewriter.rewrite(&request()).await.unwrap();
        assert_eq!(note.warnings.len(), 1);
        assert!(note.warnings[0].contains("4096-token context window"));
    }

    #[tokio::test]
    async fn omits_num_ctx_when_unset() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "message": {"role": "assistant", "content": "# Tasks"},
                "prompt_eval_count": 300,
                "done": true
            })))
            .mount(&server)
            .await;
        let rewriter = OllamaRewriter::new(&Url::parse(&server.uri()).unwrap(), None, "m");
        let note = rewriter.rewrite(&request()).await.unwrap();
        assert_eq!(note.warnings, Vec::<String>::new());
        let req = &server.received_requests().await.unwrap()[0];
        let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
        assert!(body["options"].get("num_ctx").is_none());
    }

    #[test]
    fn truncation_heuristic() {
        // No count reported: nothing to compare.
        assert!(truncation_warning(100_000, None, None).is_none());
        // ~4 characters per token: fits.
        assert!(truncation_warning(40_000, None, Some(10_000)).is_none());
        // 100k characters squeezed into 4096 tokens: the default window cut it.
        let w = truncation_warning(100_000, None, Some(4096)).unwrap();
        assert!(w.contains("server's default context window"));
        // Filling the configured window counts even when the ratio looks plausible.
        assert!(truncation_warning(60_000, Some(16_384), Some(16_000)).is_some());
        assert!(truncation_warning(20_000, Some(16_384), Some(5_000)).is_none());
    }

    #[tokio::test]
    async fn missing_message_is_empty_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"done": true})),
            )
            .mount(&server)
            .await;
        let rewriter = OllamaRewriter::new(&Url::parse(&server.uri()).unwrap(), None, "m")
            .with_retry_policy(RetryPolicy {
                attempts: 1,
                initial_delay: Duration::ZERO,
                max_delay: Duration::ZERO,
            });
        assert!(matches!(
            rewriter.rewrite(&request()).await.unwrap_err(),
            RewriteError::Empty
        ));
    }
}

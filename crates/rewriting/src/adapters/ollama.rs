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
    retry: RetryPolicy,
}

impl std::fmt::Debug for OllamaRewriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OllamaRewriter")
            .field("endpoint", &self.endpoint.as_str())
            .field("model", &self.model)
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
            retry: RetryPolicy::default_http(),
        }
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
            },
        };
        let resp: ChatResponse =
            http::post_json(&self.client, &self.endpoint, self.api_key.as_ref(), &body).await?;
        let content = resp
            .message
            .and_then(|m| m.content)
            .filter(|c| !c.trim().is_empty())
            .ok_or(RewriteError::Empty)?;
        Ok(post_process(&content))
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

#[derive(Debug, Serialize)]
struct Options {
    temperature: f32,
}

#[derive(Debug, Deserialize)]
struct ChatResponse {
    #[serde(default)]
    message: Option<ResponseMessage>,
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

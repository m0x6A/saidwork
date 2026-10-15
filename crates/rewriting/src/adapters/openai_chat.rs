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

/// Client for the OpenAI `POST /v1/chat/completions` API (llama.cpp, OpenRouter, Groq, ...).
pub struct OpenAiChatRewriter {
    client: reqwest::Client,
    endpoint: Url,
    api_key: Option<SecretString>,
    model: String,
    retry: RetryPolicy,
}

impl std::fmt::Debug for OpenAiChatRewriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenAiChatRewriter")
            .field("endpoint", &self.endpoint.as_str())
            .field("model", &self.model)
            .field("has_api_key", &self.api_key.is_some())
            .finish_non_exhaustive()
    }
}

impl OpenAiChatRewriter {
    /// `base_url` is the server root; `/v1/chat/completions` is appended.
    #[must_use]
    pub fn new(base_url: &Url, api_key: Option<SecretString>, model: impl Into<String>) -> Self {
        Self {
            client: http::client(),
            endpoint: join_path(base_url, "v1/chat/completions"),
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
            temperature: TEMPERATURE,
            stream: false,
        };
        let resp: ChatResponse =
            http::post_json(&self.client, &self.endpoint, self.api_key.as_ref(), &body).await?;
        let content = resp
            .choices
            .into_iter()
            .next()
            .and_then(|c| c.message.content)
            .filter(|c| !c.trim().is_empty())
            .ok_or(RewriteError::Empty)?;
        Ok(post_process(&content))
    }
}

#[async_trait]
impl Rewriter for OpenAiChatRewriter {
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
    temperature: f32,
    stream: bool,
}

#[derive(Debug, Deserialize)]
struct ChatResponse {
    #[serde(default)]
    choices: Vec<Choice>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    message: ChoiceMessage,
}

#[derive(Debug, Deserialize)]
struct ChoiceMessage {
    #[serde(default)]
    content: Option<String>,
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use wiremock::matchers::{body_partial_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;
    use crate::domain::FormatSpec;

    fn request() -> RewriteRequest {
        RewriteRequest {
            transcript: "we ship friday".into(),
            format: FormatSpec {
                name: "Meeting".into(),
                template: "# {{title}}".into(),
            },
            date: "2026-10-14".into(),
            context: None,
        }
    }

    fn rewriter(server: &MockServer, key: Option<&str>) -> OpenAiChatRewriter {
        OpenAiChatRewriter::new(
            &Url::parse(&server.uri()).unwrap(),
            key.map(SecretString::from),
            "gpt-x",
        )
        .with_retry_policy(RetryPolicy {
            attempts: 2,
            initial_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(1),
        })
    }

    #[tokio::test]
    async fn sends_chat_completion_and_cleans_answer() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(header("authorization", "Bearer k"))
            .and(body_partial_json(serde_json::json!({"model": "gpt-x", "stream": false, "messages": [{"role": "system"}, {"role": "user", "content": "Transcript:\n\nwe ship friday"}]})))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "choices": [{"message": {"role": "assistant", "content": "```markdown\n# Ship Friday\n\ntext\n```"}}]
            })))
            .expect(1)
            .mount(&server)
            .await;
        let note = rewriter(&server, Some("k"))
            .rewrite(&request())
            .await
            .unwrap();
        assert_eq!(note.title.as_deref(), Some("Ship Friday"));
        assert_eq!(note.markdown, "# Ship Friday\n\ntext\n");
    }

    #[tokio::test]
    async fn empty_choices_is_an_error_and_5xx_is_retried() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"choices": []})),
            )
            .up_to_n_times(1)
            .mount(&server)
            .await;
        assert!(matches!(
            rewriter(&server, None)
                .rewrite(&request())
                .await
                .unwrap_err(),
            RewriteError::Empty
        ));

        server.reset().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(500))
            .expect(2)
            .mount(&server)
            .await;
        assert!(matches!(
            rewriter(&server, None)
                .rewrite(&request())
                .await
                .unwrap_err(),
            RewriteError::Status { status: 500, .. }
        ));
    }
}

//! Bits shared by the HTTP rewriter adapters.

use std::time::Duration;

use saidwork_kernel::{ExposeSecret, SecretString};
use serde::Serialize;
use serde::de::DeserializeOwned;
use url::Url;

use crate::domain::RewriteError;

/// Temperature used for all rewrites: low, for faithful edits rather than creativity.
pub const TEMPERATURE: f32 = 0.2;

pub fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(600))
        .connect_timeout(Duration::from_secs(10))
        .build()
        .expect("static reqwest client configuration is valid")
}

/// Issue one JSON `POST` and decode the answer, mapping HTTP failures to [`RewriteError`].
pub async fn post_json<Req: Serialize, Resp: DeserializeOwned>(
    client: &reqwest::Client,
    endpoint: &Url,
    api_key: Option<&SecretString>,
    body: &Req,
) -> Result<Resp, RewriteError> {
    let mut req = client.post(endpoint.clone()).json(body);
    if let Some(key) = api_key {
        req = req.bearer_auth(key.expose_secret());
    }
    let resp = req.send().await.map_err(RewriteError::Transport)?;
    let status = resp.status();
    let text = resp.text().await.map_err(RewriteError::Transport)?;
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return Err(RewriteError::Unauthorized {
            status: status.as_u16(),
        });
    }
    if !status.is_success() {
        return Err(RewriteError::Status {
            status: status.as_u16(),
            body: truncate(&text),
        });
    }
    serde_json::from_str(&text).map_err(|e| RewriteError::Decode(e.to_string()))
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

/// One chat message in the shape both OpenAI and Ollama accept.
#[derive(Debug, Serialize)]
pub struct Message<'a> {
    pub role: &'static str,
    pub content: &'a str,
}

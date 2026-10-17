use std::time::Duration;

use async_trait::async_trait;
use url::Url;

use crate::ports::HealthProbe;

/// Can an HTTP client be built here? It cannot when the system has no CA certificates,
/// and every HTTP adapter would then panic on construction, so the app checks first.
pub fn http_client_available() -> Result<(), String> {
    reqwest::Client::builder().build().map(drop).map_err(|e| {
        std::error::Error::source(&e).map_or_else(|| e.to_string(), ToString::to_string)
    })
}

/// Probe that issues an HTTP `GET` and treats any response as "up".
#[derive(Debug, Clone)]
pub struct HttpHealthProbe {
    client: reqwest::Client,
}

impl HttpHealthProbe {
    /// Probe with a 2 second timeout per request.
    #[must_use]
    pub fn new() -> Self {
        Self::with_timeout(Duration::from_secs(2))
    }

    #[must_use]
    pub fn with_timeout(timeout: Duration) -> Self {
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .expect("static reqwest client configuration is valid");
        Self { client }
    }
}

impl Default for HttpHealthProbe {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl HealthProbe for HttpHealthProbe {
    async fn is_up(&self, url: &Url) -> bool {
        match self.client.get(url.clone()).send().await {
            Ok(resp) => {
                tracing::debug!(%url, status = %resp.status(), "health probe answered");
                true
            }
            Err(err) => {
                tracing::debug!(%url, %err, "health probe failed");
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    #[tokio::test]
    async fn any_http_response_counts_as_up() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/health"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;
        let url = Url::parse(&format!("{}/health", server.uri())).unwrap();
        assert!(HttpHealthProbe::new().is_up(&url).await);
    }

    #[tokio::test]
    async fn not_found_still_counts_as_up() {
        let server = MockServer::start().await;
        let url = Url::parse(&format!("{}/nothing", server.uri())).unwrap();
        assert!(HttpHealthProbe::new().is_up(&url).await);
    }

    #[tokio::test]
    async fn connection_refused_is_down() {
        let url = Url::parse("http://127.0.0.1:1/").unwrap();
        assert!(!HttpHealthProbe::new().is_up(&url).await);
    }
}

use crate::api::{IncomingCallResponse, IncomingCallWebhook};
use crate::config::WebhookConfig;
use crate::webhook::WebhookEvent;
use std::time::Duration;

#[derive(Clone)]
pub struct WebhookClient {
    http: reqwest::Client,
    url: String,
    retry: u32,
}

impl WebhookClient {
    pub fn new(config: &WebhookConfig) -> Self {
        let timeout = parse_timeout(&config.timeout);
        let http = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .expect("failed to build HTTP client");

        Self {
            http,
            url: config.url.clone(),
            retry: config.retry,
        }
    }

    /// Send an incoming call webhook and return the app's response.
    pub async fn send_incoming(
        &self,
        hook: &IncomingCallWebhook,
    ) -> Result<IncomingCallResponse, WebhookError> {
        let url = format!("{}/incoming", self.url);
        let resp = self
            .http
            .post(&url)
            .json(hook)
            .send()
            .await
            .map_err(WebhookError::Http)?;

        if !resp.status().is_success() {
            return Err(WebhookError::Status(resp.status().as_u16()));
        }

        resp.json().await.map_err(WebhookError::Http)
    }

    /// Fire-and-forget event delivery with retry.
    pub async fn send_event(&self, event: &WebhookEvent) {
        let attempts = 1 + self.retry;
        for attempt in 1..=attempts {
            match self.http.post(&self.url).json(event).send().await {
                Ok(resp) if resp.status().is_success() => return,
                Ok(resp) => {
                    tracing::warn!(
                        "webhook POST {} returned {} (attempt {attempt}/{attempts})",
                        self.url,
                        resp.status()
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        "webhook POST {} failed: {e} (attempt {attempt}/{attempts})",
                        self.url
                    );
                }
            }
        }
    }
}

fn parse_timeout(s: &str) -> Duration {
    let s = s.trim();
    if let Some(ms) = s.strip_suffix("ms") {
        if let Ok(n) = ms.parse::<u64>() {
            return Duration::from_millis(n);
        }
    } else if let Some(secs) = s.strip_suffix('s') {
        if let Ok(n) = secs.parse::<u64>() {
            return Duration::from_secs(n);
        }
    }
    Duration::from_secs(5) // fallback
}

#[derive(Debug)]
pub enum WebhookError {
    Http(reqwest::Error),
    Status(u16),
}

impl std::fmt::Display for WebhookError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Http(e) => write!(f, "webhook HTTP error: {e}"),
            Self::Status(code) => write!(f, "webhook returned {code}"),
        }
    }
}

impl std::error::Error for WebhookError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_timeout_seconds() {
        assert_eq!(parse_timeout("5s"), Duration::from_secs(5));
        assert_eq!(parse_timeout("30s"), Duration::from_secs(30));
    }

    #[test]
    fn parse_timeout_millis() {
        assert_eq!(parse_timeout("500ms"), Duration::from_millis(500));
    }

    #[test]
    fn parse_timeout_invalid_falls_back() {
        assert_eq!(parse_timeout("invalid"), Duration::from_secs(5));
        assert_eq!(parse_timeout(""), Duration::from_secs(5));
    }
}

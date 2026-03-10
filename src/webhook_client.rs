use crate::api::{IncomingCallResponse, IncomingCallWebhook};
use crate::config::WebhookConfig;
use crate::webhook::WebhookEvent;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const DLQ_MAX_SIZE: usize = 1000;
const BACKOFF_BASE_MS: u64 = 100;

#[derive(Clone)]
pub struct WebhookClient {
    http: reqwest::Client,
    url: String,
    retry: u32,
    dlq: Arc<Mutex<VecDeque<FailedWebhook>>>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FailedWebhook {
    pub event: WebhookEvent,
    pub error: String,
    pub attempts: u32,
    pub timestamp: String,
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
            dlq: Arc::new(Mutex::new(VecDeque::new())),
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

    /// Fire-and-forget event delivery with exponential backoff retry.
    /// Failed events are pushed to the dead letter queue.
    pub async fn send_event(&self, event: &WebhookEvent) {
        let attempts = 1 + self.retry;
        let mut last_error = String::new();

        for attempt in 1..=attempts {
            match self.http.post(&self.url).json(event).send().await {
                Ok(resp) if resp.status().is_success() => return,
                Ok(resp) => {
                    last_error = format!("HTTP {}", resp.status());
                    tracing::warn!(
                        "webhook POST {} returned {} (attempt {attempt}/{attempts})",
                        self.url,
                        resp.status()
                    );
                }
                Err(e) => {
                    last_error = e.to_string();
                    tracing::warn!(
                        "webhook POST {} failed: {e} (attempt {attempt}/{attempts})",
                        self.url
                    );
                }
            }

            // Exponential backoff before next retry (skip after last attempt)
            if attempt < attempts {
                let delay = BACKOFF_BASE_MS * 2u64.saturating_pow(attempt - 1);
                tokio::time::sleep(Duration::from_millis(delay)).await;
            }
        }

        // All retries exhausted — push to dead letter queue
        tracing::error!(
            "webhook delivery failed after {attempts} attempts, queuing to DLQ: {last_error}"
        );
        self.push_dlq(event.clone(), last_error, attempts);
    }

    fn push_dlq(&self, event: WebhookEvent, error: String, attempts: u32) {
        let entry = FailedWebhook {
            event,
            error,
            attempts,
            timestamp: chrono_now(),
        };
        let mut dlq = self.dlq.lock().unwrap();
        if dlq.len() >= DLQ_MAX_SIZE {
            dlq.pop_front(); // evict oldest
        }
        dlq.push_back(entry);
    }

    /// Return all failed webhooks without removing them.
    pub fn dlq_list(&self) -> Vec<FailedWebhook> {
        self.dlq.lock().unwrap().iter().cloned().collect()
    }

    /// Drain and return all failed webhooks.
    pub fn dlq_drain(&self) -> Vec<FailedWebhook> {
        self.dlq.lock().unwrap().drain(..).collect()
    }

    /// Number of entries in the dead letter queue.
    pub fn dlq_len(&self) -> usize {
        self.dlq.lock().unwrap().len()
    }
}

/// Simple ISO 8601 timestamp without pulling in chrono.
fn chrono_now() -> String {
    let now = std::time::SystemTime::now();
    let since_epoch = now
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = since_epoch.as_secs();

    // Convert to date/time components
    let days = secs / 86400;
    let time_secs = secs % 86400;
    let hours = time_secs / 3600;
    let minutes = (time_secs % 3600) / 60;
    let seconds = time_secs % 60;

    // Days since epoch to y/m/d (simplified — good enough for logging)
    let (year, month, day) = epoch_days_to_ymd(days);

    format!("{year:04}-{month:02}-{day:02}T{hours:02}:{minutes:02}:{seconds:02}Z")
}

fn epoch_days_to_ymd(mut days: u64) -> (u64, u64, u64) {
    // Algorithm from Howard Hinnant's date library
    days += 719468;
    let era = days / 146097;
    let doe = days - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
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

    #[test]
    fn dlq_push_and_list() {
        let client = WebhookClient::new(&WebhookConfig {
            url: "http://localhost/events".into(),
            timeout: "5s".into(),
            retry: 0,
        });

        client.push_dlq(
            WebhookEvent::Answered {
                call_id: "c1".into(),
            },
            "HTTP 500".into(),
            1,
        );
        client.push_dlq(
            WebhookEvent::Answered {
                call_id: "c2".into(),
            },
            "timeout".into(),
            3,
        );

        let list = client.dlq_list();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].error, "HTTP 500");
        assert_eq!(list[1].error, "timeout");
        assert_eq!(list[1].attempts, 3);
    }

    #[test]
    fn dlq_drain_clears() {
        let client = WebhookClient::new(&WebhookConfig {
            url: "http://localhost/events".into(),
            timeout: "5s".into(),
            retry: 0,
        });

        client.push_dlq(
            WebhookEvent::Answered {
                call_id: "c1".into(),
            },
            "err".into(),
            1,
        );

        let drained = client.dlq_drain();
        assert_eq!(drained.len(), 1);
        assert_eq!(client.dlq_len(), 0);
    }

    #[test]
    fn dlq_evicts_oldest_when_full() {
        let client = WebhookClient::new(&WebhookConfig {
            url: "http://localhost/events".into(),
            timeout: "5s".into(),
            retry: 0,
        });

        for i in 0..DLQ_MAX_SIZE + 5 {
            client.push_dlq(
                WebhookEvent::Answered {
                    call_id: format!("c{i}"),
                },
                "err".into(),
                1,
            );
        }

        assert_eq!(client.dlq_len(), DLQ_MAX_SIZE);
        // Oldest entries (c0..c4) should have been evicted
        let list = client.dlq_list();
        assert_eq!(list[0].event, WebhookEvent::Answered { call_id: "c5".into() });
    }

    #[test]
    fn chrono_now_format() {
        let ts = chrono_now();
        // Should match YYYY-MM-DDTHH:MM:SSZ
        assert_eq!(ts.len(), 20);
        assert!(ts.ends_with('Z'));
        assert_eq!(&ts[4..5], "-");
        assert_eq!(&ts[10..11], "T");
    }

    #[test]
    fn backoff_delays_are_exponential() {
        // Verify the formula: base * 2^(attempt-1)
        for attempt in 1..=5u32 {
            let delay = BACKOFF_BASE_MS * 2u64.saturating_pow(attempt - 1);
            let expected = match attempt {
                1 => 100,
                2 => 200,
                3 => 400,
                4 => 800,
                5 => 1600,
                _ => unreachable!(),
            };
            assert_eq!(delay, expected, "attempt {attempt}");
        }
    }
}

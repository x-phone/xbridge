use tokio::net::TcpListener;

use xbridge::call::{CallDirection, CallInfo, CallStatus};
use xbridge::config::{
    AuthConfig, Config, ListenConfig, RateLimitConfig, SipConfig, StreamConfig, TlsConfig,
    WebhookConfig,
};
use xbridge::router::app;
use xbridge::state::AppState;
use xbridge::webhook_client::WebhookClient;

// ── Helpers ──

fn test_config() -> Config {
    Config {
        listen: ListenConfig {
            http: "127.0.0.1:0".into(),
        },
        sip: SipConfig::default(),
        trunks: Vec::new(),
        webhook: WebhookConfig {
            url: "http://localhost:19876/events".into(),
            timeout: "5s".into(),
            retry: 0,
        },
        stream: StreamConfig::default(),
        auth: AuthConfig::default(),
        tls: TlsConfig::default(),
        rate_limit: RateLimitConfig::default(),
        server: None,
    }
}

fn test_state_from(config: Config) -> AppState {
    let metrics = xbridge::metrics::Metrics::new();
    let webhook = WebhookClient::new(&config.webhook, metrics.clone());
    let (ended_tx, _) = tokio::sync::mpsc::channel(32);
    let (dtmf_tx, _) = tokio::sync::mpsc::channel(32);
    let (state_tx, _) = tokio::sync::mpsc::channel(32);
    AppState::new(config, webhook, ended_tx, dtmf_tx, state_tx, metrics)
}

/// Spawn the full axum server on an ephemeral port. Returns the base URL.
async fn spawn_server(state: AppState) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app(state)).await.unwrap();
    });
    format!("http://127.0.0.1:{}", addr.port())
}

// ── Health endpoint ──

#[tokio::test]
async fn health_returns_json() {
    let state = test_state_from(test_config());
    let base = spawn_server(state).await;

    let resp = reqwest::get(format!("{base}/health")).await.unwrap();
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "starting");
    assert_eq!(body["sip_trunks"], 0);
    assert_eq!(body["active_calls"], 0);
}

#[tokio::test]
async fn health_shows_active_calls() {
    let state = test_state_from(test_config());
    state.calls.write().await.insert(
        "c1".into(),
        CallInfo {
            call_id: "c1".into(),
            from: "+1".into(),
            to: "+2".into(),
            direction: CallDirection::Inbound,
            status: CallStatus::InProgress,
            peer: None,
        },
    );
    let base = spawn_server(state).await;

    let body: serde_json::Value = reqwest::get(format!("{base}/health"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["active_calls"], 1);
}

// ── Metrics endpoint ──

#[tokio::test]
async fn metrics_returns_prometheus_text() {
    let state = test_state_from(test_config());
    let base = spawn_server(state).await;

    let resp = reqwest::get(format!("{base}/metrics")).await.unwrap();
    assert_eq!(resp.status(), 200);

    let ct = resp
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap();
    assert!(ct.contains("text/plain"));

    let body = resp.text().await.unwrap();
    assert!(body.contains("xbridge_calls_total"));
    assert!(body.contains("xbridge_active_calls"));
    assert!(body.contains("xbridge_ws_connections"));
    assert!(body.contains("xbridge_webhooks_total"));
}

// ── REST API ──

#[tokio::test]
async fn list_calls_empty() {
    let state = test_state_from(test_config());
    let base = spawn_server(state).await;

    let resp = reqwest::get(format!("{base}/v1/calls")).await.unwrap();
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["calls"], serde_json::json!([]));
}

#[tokio::test]
async fn list_calls_returns_inserted_call() {
    let state = test_state_from(test_config());
    state.calls.write().await.insert(
        "call_42".into(),
        CallInfo {
            call_id: "call_42".into(),
            from: "+15551234567".into(),
            to: "+15559876543".into(),
            direction: CallDirection::Outbound,
            status: CallStatus::Dialing,
            peer: None,
        },
    );
    let base = spawn_server(state).await;

    let body: serde_json::Value = reqwest::get(format!("{base}/v1/calls"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let calls = body["calls"].as_array().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["call_id"], "call_42");
    assert_eq!(calls[0]["status"], "dialing");
}

#[tokio::test]
async fn get_call_found() {
    let state = test_state_from(test_config());
    state.calls.write().await.insert(
        "c1".into(),
        CallInfo {
            call_id: "c1".into(),
            from: "+1".into(),
            to: "+2".into(),
            direction: CallDirection::Inbound,
            status: CallStatus::InProgress,
            peer: None,
        },
    );
    let base = spawn_server(state).await;

    let resp = reqwest::get(format!("{base}/v1/calls/c1")).await.unwrap();
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["call_id"], "c1");
    assert_eq!(body["direction"], "inbound");
}

#[tokio::test]
async fn get_call_not_found() {
    let state = test_state_from(test_config());
    let base = spawn_server(state).await;

    let resp = reqwest::get(format!("{base}/v1/calls/nonexistent"))
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn hangup_removes_call() {
    let state = test_state_from(test_config());
    state.calls.write().await.insert(
        "c1".into(),
        CallInfo {
            call_id: "c1".into(),
            from: "+1".into(),
            to: "+2".into(),
            direction: CallDirection::Inbound,
            status: CallStatus::InProgress,
            peer: None,
        },
    );
    let base = spawn_server(state).await;

    let client = reqwest::Client::new();
    let resp = client
        .delete(format!("{base}/v1/calls/c1"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);

    // Verify it's gone
    let resp = client
        .get(format!("{base}/v1/calls/c1"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn create_call_returns_503_when_no_phones() {
    let state = test_state_from(test_config());
    let base = spawn_server(state).await;

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{base}/v1/calls"))
        .json(&serde_json::json!({
            "to": "+15551234567",
            "from": "+15559876543"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 503);
}

// ── Auth ──

#[tokio::test]
async fn auth_blocks_without_key() {
    let mut config = test_config();
    config.auth.api_key = Some("secret-key".into());
    let state = test_state_from(config);
    let base = spawn_server(state).await;

    let resp = reqwest::get(format!("{base}/v1/calls")).await.unwrap();
    assert_eq!(resp.status(), 401);
}

#[tokio::test]
async fn auth_allows_correct_key() {
    let mut config = test_config();
    config.auth.api_key = Some("secret-key".into());
    let state = test_state_from(config);
    let base = spawn_server(state).await;

    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{base}/v1/calls"))
        .header("Authorization", "Bearer secret-key")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn auth_rejects_wrong_key() {
    let mut config = test_config();
    config.auth.api_key = Some("secret-key".into());
    let state = test_state_from(config);
    let base = spawn_server(state).await;

    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{base}/v1/calls"))
        .header("Authorization", "Bearer wrong-key")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
}

#[tokio::test]
async fn auth_does_not_protect_health_or_metrics() {
    let mut config = test_config();
    config.auth.api_key = Some("secret-key".into());
    let state = test_state_from(config);
    let base = spawn_server(state).await;

    let health = reqwest::get(format!("{base}/health")).await.unwrap();
    assert_eq!(health.status(), 200);

    let metrics = reqwest::get(format!("{base}/metrics")).await.unwrap();
    assert_eq!(metrics.status(), 200);
}

// ── Rate limiting ──

#[tokio::test]
async fn rate_limiting_returns_429() {
    let mut config = test_config();
    config.rate_limit.requests_per_second = Some(1); // burst = 2
    let state = test_state_from(config);
    let base = spawn_server(state).await;

    let client = reqwest::Client::new();

    // First 2 should pass (burst = 2x rate)
    for _ in 0..2 {
        let resp = client.get(format!("{base}/v1/calls")).send().await.unwrap();
        assert_eq!(resp.status(), 200);
    }

    // Third should be rate limited
    let resp = client.get(format!("{base}/v1/calls")).send().await.unwrap();
    assert_eq!(resp.status(), 429);
}

#[tokio::test]
async fn rate_limiting_does_not_affect_health() {
    let mut config = test_config();
    config.rate_limit.requests_per_second = Some(1);
    let state = test_state_from(config);
    let base = spawn_server(state).await;

    let client = reqwest::Client::new();

    // Exhaust rate limit
    for _ in 0..5 {
        let _ = client.get(format!("{base}/v1/calls")).send().await;
    }

    // Health should still work
    let resp = reqwest::get(format!("{base}/health")).await.unwrap();
    assert_eq!(resp.status(), 200);
}

// ── WebSocket ──

#[tokio::test]
async fn ws_rejects_unknown_call() {
    let state = test_state_from(test_config());
    let base = spawn_server(state).await;
    let ws_url = base.replace("http://", "ws://");

    let err = tokio_tungstenite::connect_async(format!("{ws_url}/ws/nonexistent"))
        .await
        .unwrap_err();

    match err {
        tokio_tungstenite::tungstenite::Error::Http(resp) => {
            assert_eq!(resp.status(), 404u16);
        }
        other => panic!("expected HTTP 404, got: {other}"),
    }
}

// ── Webhook delivery (mock server) ──

#[tokio::test]
async fn webhook_delivers_to_mock_server() {
    use std::sync::Arc;
    use tokio::sync::Mutex;

    // Spawn a mock webhook receiver
    let received = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
    let received_clone = received.clone();

    let mock_app = axum::Router::new().route(
        "/events",
        axum::routing::post(move |body: axum::Json<serde_json::Value>| {
            let store = received_clone.clone();
            async move {
                store.lock().await.push(body.0);
                axum::http::StatusCode::OK
            }
        }),
    );

    let mock_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mock_addr = mock_listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(mock_listener, mock_app).await.unwrap();
    });

    // Create webhook client pointing to mock
    let config = WebhookConfig {
        url: format!("http://127.0.0.1:{}/events", mock_addr.port()),
        timeout: "5s".into(),
        retry: 0,
    };
    let client = WebhookClient::new(&config, xbridge::metrics::Metrics::new());

    // Send an event
    let event = xbridge::webhook::WebhookEvent::Answered {
        call_id: "test_call".into(),
    };
    client.send_event(&event).await;

    // Verify delivery
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let events = received.lock().await;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["event"], "call.answered");
    assert_eq!(events[0]["call_id"], "test_call");
}

#[tokio::test]
async fn webhook_retries_on_failure() {
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    let attempt_count = Arc::new(AtomicU32::new(0));
    let count_clone = attempt_count.clone();

    let mock_app = axum::Router::new().route(
        "/events",
        axum::routing::post(move || {
            let count = count_clone.clone();
            async move {
                count.fetch_add(1, Ordering::SeqCst);
                // Always fail
                axum::http::StatusCode::INTERNAL_SERVER_ERROR
            }
        }),
    );

    let mock_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mock_addr = mock_listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(mock_listener, mock_app).await.unwrap();
    });

    let config = WebhookConfig {
        url: format!("http://127.0.0.1:{}/events", mock_addr.port()),
        timeout: "5s".into(),
        retry: 2, // 1 initial + 2 retries = 3 total
    };
    let client = WebhookClient::new(&config, xbridge::metrics::Metrics::new());

    let event = xbridge::webhook::WebhookEvent::Answered {
        call_id: "retry_test".into(),
    };
    client.send_event(&event).await;

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert_eq!(attempt_count.load(Ordering::SeqCst), 3);

    // Failed event should be in the DLQ
    assert_eq!(client.dlq_len(), 1);
    let failures = client.dlq_list();
    assert_eq!(failures[0].attempts, 3);
    assert!(failures[0].error.contains("500"));
}

#[tokio::test]
async fn webhook_backoff_increases_delay() {
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    let attempt_count = Arc::new(AtomicU32::new(0));
    let count_clone = attempt_count.clone();

    let mock_app = axum::Router::new().route(
        "/events",
        axum::routing::post(move || {
            let count = count_clone.clone();
            async move {
                count.fetch_add(1, Ordering::SeqCst);
                axum::http::StatusCode::INTERNAL_SERVER_ERROR
            }
        }),
    );

    let mock_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mock_addr = mock_listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(mock_listener, mock_app).await.unwrap();
    });

    let config = WebhookConfig {
        url: format!("http://127.0.0.1:{}/events", mock_addr.port()),
        timeout: "5s".into(),
        retry: 3, // 1 initial + 3 retries = 4 attempts, backoff: 100ms + 200ms + 400ms = 700ms
    };
    let client = WebhookClient::new(&config, xbridge::metrics::Metrics::new());

    let start = std::time::Instant::now();
    let event = xbridge::webhook::WebhookEvent::Answered {
        call_id: "backoff_test".into(),
    };
    client.send_event(&event).await;
    let elapsed = start.elapsed();

    assert_eq!(attempt_count.load(Ordering::SeqCst), 4);
    // Total backoff should be at least 700ms (100 + 200 + 400)
    assert!(
        elapsed >= std::time::Duration::from_millis(600),
        "elapsed {elapsed:?} should be >= 600ms"
    );
}

#[tokio::test]
async fn dlq_endpoint_shows_failures() {
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    let attempt_count = Arc::new(AtomicU32::new(0));
    let count_clone = attempt_count.clone();

    // Mock webhook that always fails
    let mock_app = axum::Router::new().route(
        "/events",
        axum::routing::post(move || {
            let count = count_clone.clone();
            async move {
                count.fetch_add(1, Ordering::SeqCst);
                axum::http::StatusCode::INTERNAL_SERVER_ERROR
            }
        }),
    );

    let mock_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mock_addr = mock_listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(mock_listener, mock_app).await.unwrap();
    });

    // Build state with webhook pointing to failing mock
    let mut config = test_config();
    config.webhook.url = format!("http://127.0.0.1:{}/events", mock_addr.port());
    config.webhook.retry = 0; // no retries, fail fast
    let state = test_state_from(config);
    let base = spawn_server(state.clone()).await;

    // Trigger a failed webhook delivery
    let event = xbridge::webhook::WebhookEvent::Answered {
        call_id: "dlq_test".into(),
    };
    state.webhook.send_event(&event).await;

    // Check DLQ via REST
    let client = reqwest::Client::new();
    let body: serde_json::Value = client
        .get(format!("{base}/v1/webhooks/failures"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let failures = body["failures"].as_array().unwrap();
    assert_eq!(failures.len(), 1);
    assert_eq!(failures[0]["event"]["call_id"], "dlq_test");

    // Drain via REST
    let body: serde_json::Value = client
        .delete(format!("{base}/v1/webhooks/failures"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["drained"], 1);

    // Verify empty
    let body: serde_json::Value = client
        .get(format!("{base}/v1/webhooks/failures"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["failures"].as_array().unwrap().len(), 0);
}

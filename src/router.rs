use axum::extract::ws::{WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};

use crate::api::CallListResponse;
use crate::call::CallInfo;
use crate::state::AppState;

pub fn app(state: AppState) -> Router {
    Router::new()
        .route("/ws/{call_id}", get(ws_handler))
        .route("/v1/calls", get(list_calls))
        .route(
            "/v1/calls/{call_id}",
            get(get_call).delete(hangup_call),
        )
        .with_state(state)
}

async fn list_calls(State(state): State<AppState>) -> Json<CallListResponse> {
    let calls = state.calls.read().await;
    Json(CallListResponse {
        calls: calls.values().cloned().collect(),
    })
}

async fn get_call(
    State(state): State<AppState>,
    Path(call_id): Path<String>,
) -> Result<Json<CallInfo>, StatusCode> {
    let calls = state.calls.read().await;
    calls
        .get(&call_id)
        .cloned()
        .map(Json)
        .ok_or(StatusCode::NOT_FOUND)
}

async fn hangup_call(
    State(state): State<AppState>,
    Path(call_id): Path<String>,
) -> StatusCode {
    let mut calls = state.calls.write().await;
    if calls.remove(&call_id).is_some() {
        // TODO: hang up the SIP call via xphone
        StatusCode::NO_CONTENT
    } else {
        StatusCode::NOT_FOUND
    }
}

async fn ws_handler(
    State(state): State<AppState>,
    Path(call_id): Path<String>,
    ws: WebSocketUpgrade,
) -> Result<impl IntoResponse, StatusCode> {
    let calls = state.calls.read().await;
    if !calls.contains_key(&call_id) {
        return Err(StatusCode::NOT_FOUND);
    }
    drop(calls);
    Ok(ws.on_upgrade(move |socket| handle_ws(socket, state, call_id)))
}

async fn handle_ws(_socket: WebSocket, _state: AppState, _call_id: String) {
    // TODO: bidirectional audio streaming
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::call::{CallDirection, CallStatus};
    use crate::config::{
        Config, ListenConfig, SipConfig, SipTransport, StreamConfig, WebhookConfig,
    };
    use axum::body::Body;
    use http_body_util::BodyExt;
    use tokio::net::TcpListener;
    use tower::ServiceExt;

    fn test_config() -> Config {
        Config {
            listen: ListenConfig {
                http: "0.0.0.0:8080".into(),
            },
            sip: SipConfig {
                username: "test".into(),
                password: "test".into(),
                host: "sip.test.com".into(),
                transport: SipTransport::Udp,
                rtp_port_min: 10000,
                rtp_port_max: 20000,
                srtp: false,
                stun_server: None,
            },
            webhook: WebhookConfig {
                url: "http://localhost:9090/events".into(),
                timeout: "5s".into(),
                retry: 1,
            },
            stream: StreamConfig::default(),
        }
    }

    fn test_state() -> AppState {
        AppState::new(test_config())
    }

    fn sample_call(id: &str) -> CallInfo {
        CallInfo {
            call_id: id.into(),
            from: "+15551234567".into(),
            to: "+15559876543".into(),
            direction: CallDirection::Inbound,
            status: CallStatus::InProgress,
        }
    }

    async fn body_json<T: serde::de::DeserializeOwned>(
        resp: axum::http::Response<Body>,
    ) -> T {
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    /// Spawn a real server on a random port, return the base URL.
    async fn spawn_server(state: AppState) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app(state)).await.unwrap();
        });
        format!("127.0.0.1:{}", addr.port())
    }

    // ── GET /v1/calls ──

    #[tokio::test]
    async fn list_calls_empty() {
        let resp = app(test_state())
            .oneshot(
                axum::http::Request::builder()
                    .uri("/v1/calls")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let list: CallListResponse = body_json(resp).await;
        assert!(list.calls.is_empty());
    }

    #[tokio::test]
    async fn list_calls_returns_active_calls() {
        let state = test_state();
        state
            .calls
            .write()
            .await
            .insert("call_001".into(), sample_call("call_001"));

        let resp = app(state)
            .oneshot(
                axum::http::Request::builder()
                    .uri("/v1/calls")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let list: CallListResponse = body_json(resp).await;
        assert_eq!(list.calls.len(), 1);
        assert_eq!(list.calls[0].call_id, "call_001");
    }

    // ── GET /v1/calls/{id} ──

    #[tokio::test]
    async fn get_call_not_found() {
        let resp = app(test_state())
            .oneshot(
                axum::http::Request::builder()
                    .uri("/v1/calls/nonexistent")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn get_call_returns_details() {
        let state = test_state();
        state
            .calls
            .write()
            .await
            .insert("call_001".into(), sample_call("call_001"));

        let resp = app(state)
            .oneshot(
                axum::http::Request::builder()
                    .uri("/v1/calls/call_001")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let info: CallInfo = body_json(resp).await;
        assert_eq!(info.call_id, "call_001");
        assert_eq!(info.status, CallStatus::InProgress);
        assert_eq!(info.direction, CallDirection::Inbound);
    }

    // ── DELETE /v1/calls/{id} ──

    #[tokio::test]
    async fn hangup_not_found() {
        let resp = app(test_state())
            .oneshot(
                axum::http::Request::builder()
                    .method("DELETE")
                    .uri("/v1/calls/nonexistent")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn hangup_removes_call_and_returns_204() {
        let state = test_state();
        state
            .calls
            .write()
            .await
            .insert("call_001".into(), sample_call("call_001"));

        let resp = app(state.clone())
            .oneshot(
                axum::http::Request::builder()
                    .method("DELETE")
                    .uri("/v1/calls/call_001")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
        assert!(state.calls.read().await.get("call_001").is_none());
    }

    // ── WebSocket /ws/{call_id} ──
    // These tests use a real TCP server because axum's WebSocketUpgrade
    // extractor requires a genuine HTTP connection (hyper::upgrade::OnUpgrade).

    #[tokio::test]
    async fn ws_rejects_unknown_call() {
        let addr = spawn_server(test_state()).await;
        let url = format!("ws://{addr}/ws/nonexistent");

        let err = tokio_tungstenite::connect_async(&url).await.unwrap_err();
        match err {
            tokio_tungstenite::tungstenite::Error::Http(resp) => {
                assert_eq!(resp.status(), 404u16);
            }
            other => panic!("expected HTTP 404 error, got: {other}"),
        }
    }

    #[tokio::test]
    async fn ws_upgrades_for_active_call() {
        let state = test_state();
        state
            .calls
            .write()
            .await
            .insert("call_001".into(), sample_call("call_001"));

        let addr = spawn_server(state).await;
        let url = format!("ws://{addr}/ws/call_001");

        let (ws_stream, resp) = tokio_tungstenite::connect_async(&url).await.unwrap();
        assert_eq!(resp.status(), 101u16);
        drop(ws_stream);
    }
}

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use std::sync::Arc;

use crate::api::{
    CallListResponse, CreateCallRequest, CreateCallResponse, DtmfRequest, TransferRequest,
};
use crate::audio;
use crate::bridge;
use crate::call::{CallDirection, CallInfo, CallStatus};
use crate::config::AudioEncoding;
use crate::state::AppState;
use crate::webhook::WebhookEvent;
use crate::ws::{ClientEvent, MediaFormat, ServerEvent, ServerMediaPayload, StartPayload};

pub fn app(state: AppState) -> Router {
    Router::new()
        .route("/ws/{call_id}", get(ws_handler))
        .route("/v1/calls", get(list_calls).post(create_call))
        .route(
            "/v1/calls/{call_id}",
            get(get_call).delete(hangup_call),
        )
        .route("/v1/calls/{call_id}/hold", post(hold_call))
        .route("/v1/calls/{call_id}/resume", post(resume_call))
        .route("/v1/calls/{call_id}/transfer", post(transfer_call))
        .route("/v1/calls/{call_id}/dtmf", post(send_dtmf))
        .route("/v1/calls/{call_id}/mute", post(mute_call))
        .route("/v1/calls/{call_id}/unmute", post(unmute_call))
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
    // Remove xphone call first, then end it outside the lock
    let xphone_call = state.xphone_calls.write().await.remove(&call_id);
    if let Some(xphone_call) = xphone_call {
        let _ = xphone_call.end();
    }

    let mut calls = state.calls.write().await;
    if calls.remove(&call_id).is_some() {
        StatusCode::NO_CONTENT
    } else {
        StatusCode::NOT_FOUND
    }
}

async fn create_call(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CreateCallRequest>,
) -> Result<Json<CreateCallResponse>, StatusCode> {
    if state.phone.get().is_none() {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }

    let from = req.from.clone();
    let to = req.to.clone();

    let opts = xphone::DialOptions {
        caller_id: Some(from.clone()),
        ..Default::default()
    };

    // Phone::dial is blocking
    let phone_ref = state.phone.clone();
    let call = tokio::task::spawn_blocking(move || {
        let phone = phone_ref.get().ok_or(xphone::Error::NotConnected)?;
        phone.dial(&to, opts)
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .map_err(|e| {
        tracing::error!("dial failed: {e}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let call_id = call.id();

    let info = CallInfo {
        call_id: call_id.clone(),
        from,
        to: req.to.clone(),
        direction: CallDirection::Outbound,
        status: CallStatus::Dialing,
    };

    state.calls.write().await.insert(call_id.clone(), info);
    state
        .xphone_calls
        .write()
        .await
        .insert(call_id.clone(), call.clone());

    // Wire callbacks
    bridge::wire_call_callbacks(&call, &call_id, &state);
    bridge::wire_outbound_state_callbacks(&call, &call_id, &state);

    // Build ws_url from Host header or config
    let host = headers
        .get("host")
        .and_then(|v| v.to_str().ok())
        .unwrap_or(state.config.listen.http.as_str());
    let ws_url = format!("ws://{host}/ws/{call_id}");

    Ok(Json(CreateCallResponse {
        call_id,
        status: CallStatus::Dialing,
        ws_url,
    }))
}

/// Helper to look up the xphone Call by ID.
async fn get_xphone_call(
    state: &AppState,
    call_id: &str,
) -> Result<Arc<xphone::Call>, StatusCode> {
    state
        .xphone_calls
        .read()
        .await
        .get(call_id)
        .cloned()
        .ok_or(StatusCode::NOT_FOUND)
}

async fn hold_call(
    State(state): State<AppState>,
    Path(call_id): Path<String>,
) -> StatusCode {
    let call = match get_xphone_call(&state, &call_id).await {
        Ok(c) => c,
        Err(s) => return s,
    };

    if let Err(e) = call.hold() {
        tracing::error!("hold failed for {call_id}: {e}");
        return StatusCode::INTERNAL_SERVER_ERROR;
    }

    if let Some(info) = state.calls.write().await.get_mut(&call_id) {
        info.status = CallStatus::OnHold;
    }
    let webhook = state.webhook.clone();
    tokio::spawn(async move {
        webhook.send_event(&WebhookEvent::Hold { call_id }).await;
    });

    StatusCode::OK
}

async fn resume_call(
    State(state): State<AppState>,
    Path(call_id): Path<String>,
) -> StatusCode {
    let call = match get_xphone_call(&state, &call_id).await {
        Ok(c) => c,
        Err(s) => return s,
    };

    if let Err(e) = call.resume() {
        tracing::error!("resume failed for {call_id}: {e}");
        return StatusCode::INTERNAL_SERVER_ERROR;
    }

    if let Some(info) = state.calls.write().await.get_mut(&call_id) {
        info.status = CallStatus::InProgress;
    }
    let webhook = state.webhook.clone();
    tokio::spawn(async move {
        webhook
            .send_event(&WebhookEvent::Resumed { call_id })
            .await;
    });

    StatusCode::OK
}

async fn transfer_call(
    State(state): State<AppState>,
    Path(call_id): Path<String>,
    Json(req): Json<TransferRequest>,
) -> StatusCode {
    let call = match get_xphone_call(&state, &call_id).await {
        Ok(c) => c,
        Err(s) => return s,
    };

    if let Err(e) = call.blind_transfer(&req.target) {
        tracing::error!("transfer failed for {call_id}: {e}");
        return StatusCode::INTERNAL_SERVER_ERROR;
    }

    StatusCode::OK
}

async fn send_dtmf(
    State(state): State<AppState>,
    Path(call_id): Path<String>,
    Json(req): Json<DtmfRequest>,
) -> StatusCode {
    let call = match get_xphone_call(&state, &call_id).await {
        Ok(c) => c,
        Err(s) => return s,
    };

    let result = tokio::task::spawn_blocking(move || {
        for digit in req.digits.chars() {
            call.send_dtmf(&digit.to_string())?;
        }
        Ok::<(), xphone::Error>(())
    })
    .await;

    match result {
        Ok(Ok(())) => StatusCode::OK,
        Ok(Err(e)) => {
            tracing::error!("dtmf send failed for {call_id}: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        }
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

async fn mute_call(
    State(state): State<AppState>,
    Path(call_id): Path<String>,
) -> StatusCode {
    let call = match get_xphone_call(&state, &call_id).await {
        Ok(c) => c,
        Err(s) => return s,
    };

    if let Err(e) = call.mute() {
        tracing::error!("mute failed for {call_id}: {e}");
        return StatusCode::INTERNAL_SERVER_ERROR;
    }

    StatusCode::OK
}

async fn unmute_call(
    State(state): State<AppState>,
    Path(call_id): Path<String>,
) -> StatusCode {
    let call = match get_xphone_call(&state, &call_id).await {
        Ok(c) => c,
        Err(s) => return s,
    };

    if let Err(e) = call.unmute() {
        tracing::error!("unmute failed for {call_id}: {e}");
        return StatusCode::INTERNAL_SERVER_ERROR;
    }

    StatusCode::OK
}

async fn ws_handler(
    State(state): State<AppState>,
    Path(call_id): Path<String>,
    ws: WebSocketUpgrade,
) -> Result<impl IntoResponse, StatusCode> {
    let call = get_xphone_call(&state, &call_id).await?;

    let encoding = state.config.stream.encoding.clone();
    let sample_rate = state.config.stream.sample_rate;

    Ok(ws.on_upgrade(move |socket| {
        handle_ws(socket, call_id, call, encoding, sample_rate)
    }))
}

async fn handle_ws(
    socket: WebSocket,
    call_id: String,
    call: Arc<xphone::Call>,
    encoding: AudioEncoding,
    sample_rate: u32,
) {
    let encoding_str = match encoding {
        AudioEncoding::Mulaw => "audio/x-mulaw",
        AudioEncoding::L16 => "audio/x-l16",
    };

    let (mut ws_tx, mut ws_rx) = socket.split();

    // Send "connected" event
    let connected = ServerEvent::Connected {
        protocol: "Call".into(),
        version: "1.0.0".into(),
    };
    if send_json(&mut ws_tx, &connected).await.is_err() {
        return;
    }

    // Send "start" event
    let start = ServerEvent::Start {
        stream_sid: call_id.clone(),
        start: StartPayload {
            call_sid: call_id.clone(),
            tracks: vec!["inbound".into()],
            media_format: MediaFormat {
                encoding: encoding_str.to_string(),
                sample_rate,
                channels: 1,
            },
        },
    };
    if send_json(&mut ws_tx, &start).await.is_err() {
        return;
    }

    // Get audio channels from xphone
    let (Some(pcm_rx), Some(pcm_tx)) = (call.pcm_reader(), call.pcm_writer()) else {
        tracing::error!("call {call_id}: audio channels not available");
        let _ = send_json(
            &mut ws_tx,
            &ServerEvent::Stop {
                stream_sid: call_id,
            },
        )
        .await;
        return;
    };

    let (audio_tx, mut audio_rx) = tokio::sync::mpsc::channel::<String>(64);

    // Blocking reader: pcm_rx → audio_tx
    let encoding_reader = encoding.clone();
    let cid_sender = call_id.clone();
    let mut reader_handle = tokio::task::spawn_blocking(move || {
        let b64 = base64::engine::general_purpose::STANDARD;
        let mut timestamp: u64 = 0;

        let stream_sid = call_id;
        let mut encode_buf = Vec::new();

        while let Ok(frame) = pcm_rx.recv() {
            encode_buf.clear();
            match encoding_reader {
                AudioEncoding::Mulaw => audio::pcm16_to_mulaw_into(&frame, &mut encode_buf),
                AudioEncoding::L16 => audio::pcm16_to_bytes_into(&frame, &mut encode_buf),
            };
            let payload = b64.encode(&encode_buf);

            let event = ServerEvent::Media {
                stream_sid: stream_sid.clone(),
                media: ServerMediaPayload {
                    timestamp: timestamp.to_string(),
                    payload,
                },
            };

            if let Ok(json) = serde_json::to_string(&event) {
                if audio_tx.blocking_send(json).is_err() {
                    break; // WS closed
                }
            }

            timestamp += frame.len() as u64;
        }
    });

    // Async sender: audio_rx → ws_tx
    let mut sender_handle = tokio::spawn(async move {
        while let Some(json) = audio_rx.recv().await {
            if ws_tx.send(Message::Text(json.into())).await.is_err() {
                break;
            }
        }
        // Send stop event (best effort)
        let stop = ServerEvent::Stop {
            stream_sid: cid_sender,
        };
        if let Ok(json) = serde_json::to_string(&stop) {
            let _ = ws_tx.send(Message::Text(json.into())).await;
        }
        let _ = ws_tx.close().await;
    });

    // Task 2: WebSocket → xphone audio (client audio to call)
    let mut receiver_handle = tokio::spawn(async move {
        let b64 = base64::engine::general_purpose::STANDARD;
        while let Some(Ok(msg)) = ws_rx.next().await {
            match msg {
                Message::Text(text) => {
                    if let Ok(event) = serde_json::from_str::<ClientEvent>(&text) {
                        match event {
                            ClientEvent::Media { media, .. } => {
                                if let Ok(bytes) = b64.decode(&media.payload) {
                                    let pcm = match encoding {
                                        AudioEncoding::Mulaw => audio::mulaw_to_pcm16(&bytes),
                                        AudioEncoding::L16 => audio::bytes_to_pcm16(&bytes),
                                    };
                                    let _ = pcm_tx.try_send(pcm);
                                }
                            }
                            ClientEvent::Clear { .. } => {
                                // Clear pending audio — no-op for now
                            }
                            ClientEvent::Mark { .. } => {
                                // Mark acknowledgment — no-op for now
                            }
                        }
                    }
                }
                Message::Close(_) => break,
                _ => {}
            }
        }
    });

    // Wait for any direction to finish, then abort the others
    tokio::select! {
        _ = &mut reader_handle => { sender_handle.abort(); receiver_handle.abort(); },
        _ = &mut sender_handle => { reader_handle.abort(); receiver_handle.abort(); },
        _ = &mut receiver_handle => { reader_handle.abort(); sender_handle.abort(); },
    }
}

async fn send_json<T: serde::Serialize>(
    tx: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    event: &T,
) -> Result<(), ()> {
    let json = serde_json::to_string(event).map_err(|_| ())?;
    tx.send(Message::Text(json.into())).await.map_err(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        Config, ListenConfig, SipConfig, SipTransport, StreamConfig, WebhookConfig,
    };
    use crate::webhook_client::WebhookClient;
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
        let config = test_config();
        let webhook = WebhookClient::new(&config.webhook);
        let (ended_tx, _ended_rx) = tokio::sync::mpsc::channel(32);
        let (dtmf_tx, _dtmf_rx) = tokio::sync::mpsc::channel(32);
        let (state_tx, _state_rx) = tokio::sync::mpsc::channel(32);
        AppState::new(config, webhook, ended_tx, dtmf_tx, state_tx)
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
    // WS tests need xphone::Call objects which we can't easily mock.
    // The upgrade/reject tests verify routing; audio streaming is
    // tested via integration tests with a real SIP trunk.

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

    // ── POST /v1/calls (outbound) ──
    // Requires a connected xphone::Phone which we can't create in unit tests.
    // Test the 503 case (phone not connected).

    #[tokio::test]
    async fn create_call_returns_503_when_not_connected() {
        let resp = app(test_state())
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/v1/calls")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"to":"+15551234567","from":"+15559876543"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    // ── Call control endpoints ──
    // These need xphone::Call objects; test 404 behavior only.

    #[tokio::test]
    async fn hold_not_found() {
        let resp = app(test_state())
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/v1/calls/nonexistent/hold")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn resume_not_found() {
        let resp = app(test_state())
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/v1/calls/nonexistent/resume")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn transfer_not_found() {
        let resp = app(test_state())
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/v1/calls/nonexistent/transfer")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"target":"sip:1003@pbx"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn dtmf_not_found() {
        let resp = app(test_state())
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/v1/calls/nonexistent/dtmf")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"digits":"1234"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn mute_not_found() {
        let resp = app(test_state())
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/v1/calls/nonexistent/mute")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn unmute_not_found() {
        let resp = app(test_state())
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/v1/calls/nonexistent/unmute")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }
}

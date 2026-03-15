use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use std::sync::Arc;
use std::time::Instant;

use crate::api::{
    CallListResponse, CreateCallRequest, CreateCallResponse, DtmfRequest, PlayRequest,
    PlayResponse, TransferRequest,
};
use crate::audio;
use crate::bridge;
use crate::call::{CallDirection, CallInfo, CallStatus};
use crate::call_control::{CallControl, CallError, XphoneCall};
use crate::config::{AudioEncoding, StreamMode};
use crate::state::AppState;
use crate::webhook::WebhookEvent;
use crate::ws::{ClientEvent, MediaFormat, ServerEvent, ServerMediaPayload, StartPayload};

/// Build the WebSocket URL from the request's Host header (or config fallback).
fn ws_url(headers: &HeaderMap, state: &AppState, call_id: &str) -> String {
    let host = headers
        .get("host")
        .and_then(|v| v.to_str().ok())
        .unwrap_or(state.config.listen.http.as_str());
    format!("ws://{host}/ws/{call_id}")
}

pub fn app(state: AppState) -> Router {
    let rate_limiter = state
        .config
        .rate_limit
        .requests_per_second
        .map(RateLimiter::new);

    let api_routes = Router::new()
        .route("/ws/{call_id}", get(ws_handler))
        .route("/v1/calls", get(list_calls).post(create_call))
        .route("/v1/calls/{call_id}", get(get_call).delete(hangup_call))
        .route("/v1/calls/{call_id}/hold", post(hold_call))
        .route("/v1/calls/{call_id}/resume", post(resume_call))
        .route("/v1/calls/{call_id}/transfer", post(transfer_call))
        .route("/v1/calls/{call_id}/dtmf", post(send_dtmf))
        .route("/v1/calls/{call_id}/mute", post(mute_call))
        .route("/v1/calls/{call_id}/unmute", post(unmute_call))
        .route("/v1/calls/{call_id}/play", post(play_call))
        .route("/v1/calls/{call_id}/play/stop", post(stop_play))
        .route(
            "/v1/webhooks/failures",
            get(list_webhook_failures).delete(drain_webhook_failures),
        )
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth_middleware,
        ))
        .route_layer(middleware::from_fn_with_state(
            (rate_limiter, state.metrics.clone()),
            rate_limit_middleware,
        ))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            http_metrics_middleware,
        ));

    Router::new()
        .route("/health", get(health_check))
        .route("/metrics", get(metrics_handler))
        .merge(api_routes)
        .with_state(state)
}

#[derive(Clone)]
struct RateLimiter {
    inner: Arc<std::sync::Mutex<TokenBucket>>,
}

struct TokenBucket {
    tokens: f64,
    max_tokens: f64,
    refill_rate: f64, // tokens per second
    last_refill: Instant,
}

impl RateLimiter {
    fn new(rps: u32) -> Self {
        let max_tokens = rps as f64 * 2.0; // burst = 2x rate
        Self {
            inner: Arc::new(std::sync::Mutex::new(TokenBucket {
                tokens: max_tokens,
                max_tokens,
                refill_rate: rps as f64,
                last_refill: Instant::now(),
            })),
        }
    }

    fn try_acquire(&self) -> bool {
        let mut bucket = self.inner.lock().unwrap();
        let now = Instant::now();
        let elapsed = now.duration_since(bucket.last_refill).as_secs_f64();
        bucket.tokens = (bucket.tokens + elapsed * bucket.refill_rate).min(bucket.max_tokens);
        bucket.last_refill = now;

        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

async fn rate_limit_middleware(
    State((limiter, metrics)): State<(Option<RateLimiter>, crate::metrics::Metrics)>,
    req: Request,
    next: Next,
) -> Result<impl IntoResponse, StatusCode> {
    if let Some(ref limiter) = limiter {
        if !limiter.try_acquire() {
            metrics.inc_rate_limit_rejections();
            return Err(StatusCode::TOO_MANY_REQUESTS);
        }
    }
    Ok(next.run(req).await)
}

async fn auth_middleware(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Result<impl IntoResponse, StatusCode> {
    let Some(ref expected_key) = state.config.auth.api_key else {
        // No API key configured — auth disabled
        return Ok(next.run(req).await);
    };

    let auth_header = req
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok());

    match auth_header {
        Some(header) if header.strip_prefix("Bearer ") == Some(expected_key) => {
            Ok(next.run(req).await)
        }
        _ => Err(StatusCode::UNAUTHORIZED),
    }
}

async fn http_metrics_middleware(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> impl IntoResponse {
    let start = Instant::now();
    let response = next.run(req).await;
    state.metrics.inc_http_requests();
    state
        .metrics
        .observe_http_request_duration(start.elapsed().as_secs_f64());
    response
}

async fn health_check(State(state): State<AppState>) -> Json<serde_json::Value> {
    let phones = state.phones.read().await;
    let trunk_count = phones.len();
    drop(phones);
    let has_server = state.xphone_server.read().await.is_some();
    let active_calls = state.calls.read().await.len();
    let status = if trunk_count > 0 || has_server { "ok" } else { "starting" };

    Json(serde_json::json!({
        "status": status,
        "sip_trunks": trunk_count,
        "sip_server": has_server,
        "active_calls": active_calls,
    }))
}

async fn metrics_handler(State(state): State<AppState>) -> impl IntoResponse {
    let active_calls = state.calls.read().await.len();
    let body = state.metrics.render(active_calls);
    (
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        body,
    )
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

async fn hangup_call(State(state): State<AppState>, Path(call_id): Path<String>) -> StatusCode {
    // Remove xphone call first, then end it outside the lock
    let xphone_call = state.xphone_calls.write().await.remove(&call_id);
    if let Some(xphone_call) = xphone_call {
        let _ = xphone_call.end();
    }

    let removed = state.calls.write().await.remove(&call_id).is_some();
    if !removed {
        return StatusCode::NOT_FOUND;
    }

    // Clean up associated resources
    if let Ok(mut senders) = state.ws_senders.write() {
        senders.remove(&call_id);
    }
    if let Some(handle) = state.plays.write().await.remove(&call_id) {
        handle
            .cancel
            .store(true, std::sync::atomic::Ordering::Relaxed);
        handle.task.abort();
    }

    StatusCode::NO_CONTENT
}

async fn create_call(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CreateCallRequest>,
) -> Result<Json<CreateCallResponse>, StatusCode> {
    // Route to peer or trunk provider based on request.
    if let Some(ref peer_name) = req.peer {
        return create_call_to_peer(&state, &headers, &req, peer_name).await;
    }

    let trunk_name = req.trunk.as_deref().unwrap_or("default");

    let phones = state.phones.read().await;
    if !phones.contains_key(trunk_name) {
        if phones.is_empty() {
            return Err(StatusCode::SERVICE_UNAVAILABLE);
        }
        return Err(StatusCode::NOT_FOUND);
    }
    drop(phones);

    let from = req.from.clone();
    let to = req.to.clone();

    let opts = xphone::DialOptions {
        caller_id: Some(from.clone()),
        ..Default::default()
    };

    // Phone::dial is blocking
    let phones_ref = state.phones.clone();
    let trunk = trunk_name.to_string();
    let call = tokio::task::spawn_blocking(move || {
        let phones = phones_ref.blocking_read();
        let phone = phones.get(&trunk).ok_or(xphone::Error::NotConnected)?;
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
        peer: None,
    };

    // Insert into both registries (brief TOCTOU window between the two awaits)
    {
        state.calls.write().await.insert(call_id.clone(), info);
        state
            .xphone_calls
            .write()
            .await
            .insert(call_id.clone(), Arc::new(XphoneCall(call.clone())));
    }

    state.metrics.inc_calls_outbound();

    // Wire callbacks
    bridge::wire_call_callbacks(&call, &call_id, &state);
    bridge::wire_outbound_state_callbacks(&call, &call_id, &state);

    let ws = ws_url(&headers, &state, &call_id);
    Ok(Json(CreateCallResponse {
        call_id,
        status: CallStatus::Dialing,
        ws_url: ws,
    }))
}

/// Create an outbound call to a configured peer via the trunk host server.
async fn create_call_to_peer(
    state: &AppState,
    headers: &HeaderMap,
    req: &CreateCallRequest,
    peer_name: &str,
) -> Result<Json<CreateCallResponse>, StatusCode> {
    // Verify peer exists in config.
    let server_config = state.config.server.as_ref().ok_or_else(|| {
        tracing::error!("peer '{peer_name}' requested but no trunk server configured");
        StatusCode::NOT_FOUND
    })?;
    if !server_config.peers.iter().any(|p| p.name == peer_name) {
        tracing::error!("peer '{peer_name}' not found in server config");
        return Err(StatusCode::NOT_FOUND);
    }

    // Get trunk server handle.
    let server = state.xphone_server.read().await.clone().ok_or_else(|| {
        tracing::error!("trunk server not running, cannot dial peer");
        StatusCode::SERVICE_UNAVAILABLE
    })?;

    // Dial via xphone::Server.
    let peer = peer_name.to_string();
    let to = req.to.clone();
    let from = req.from.clone();
    let call = tokio::task::spawn_blocking(move || server.dial(&peer, &to, &from))
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .map_err(|e| {
            tracing::error!("dial to peer '{peer_name}' failed: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let call_id = call.id();

    let info = CallInfo {
        call_id: call_id.clone(),
        from: req.from.clone(),
        to: req.to.clone(),
        direction: CallDirection::Outbound,
        status: CallStatus::Dialing,
        peer: Some(peer_name.to_string()),
    };

    state.metrics.inc_calls_outbound();

    {
        state.calls.write().await.insert(call_id.clone(), info);
        state
            .xphone_calls
            .write()
            .await
            .insert(call_id.clone(), Arc::new(XphoneCall(call.clone())));
    }

    bridge::wire_call_callbacks(&call, &call_id, state);
    bridge::wire_outbound_state_callbacks(&call, &call_id, state);

    tracing::info!(
        "outbound call {call_id} to peer '{peer_name}': {} → {}",
        req.from,
        req.to
    );

    let ws = ws_url(headers, state, &call_id);
    Ok(Json(CreateCallResponse {
        call_id,
        status: CallStatus::Dialing,
        ws_url: ws,
    }))
}

/// Helper to look up the call control interface by ID.
async fn get_xphone_call(
    state: &AppState,
    call_id: &str,
) -> Result<Arc<dyn CallControl>, StatusCode> {
    state
        .xphone_calls
        .read()
        .await
        .get(call_id)
        .cloned()
        .ok_or(StatusCode::NOT_FOUND)
}

async fn hold_call(State(state): State<AppState>, Path(call_id): Path<String>) -> StatusCode {
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

async fn resume_call(State(state): State<AppState>, Path(call_id): Path<String>) -> StatusCode {
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
        webhook.send_event(&WebhookEvent::Resumed { call_id }).await;
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
        Ok::<(), CallError>(())
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

async fn mute_call(State(state): State<AppState>, Path(call_id): Path<String>) -> StatusCode {
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

async fn unmute_call(State(state): State<AppState>, Path(call_id): Path<String>) -> StatusCode {
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

async fn play_call(
    State(state): State<AppState>,
    Path(call_id): Path<String>,
    Json(req): Json<PlayRequest>,
) -> Result<Json<PlayResponse>, StatusCode> {
    let call = get_xphone_call(&state, &call_id).await?;

    // Resolve PCM audio from request
    let pcm_data = match (&req.url, &req.audio) {
        (Some(url), _) => {
            let bytes = reqwest::get(url)
                .await
                .map_err(|e| {
                    tracing::error!("failed to fetch audio from {url}: {e}");
                    StatusCode::BAD_REQUEST
                })?
                .bytes()
                .await
                .map_err(|e| {
                    tracing::error!("failed to read audio body from {url}: {e}");
                    StatusCode::BAD_REQUEST
                })?;

            let (header, data) = crate::wav::parse_wav(&bytes).map_err(|e| {
                tracing::error!("WAV parse error: {e}");
                StatusCode::BAD_REQUEST
            })?;
            crate::wav::ensure_8khz_mono_16bit(&header).map_err(|e| {
                tracing::error!("WAV format error: {e}");
                StatusCode::BAD_REQUEST
            })?;

            audio::bytes_to_pcm16(data)
        }
        (None, Some(b64)) => {
            let b64_engine = base64::engine::general_purpose::STANDARD;
            let bytes = b64_engine.decode(b64).map_err(|e| {
                tracing::error!("base64 decode error: {e}");
                StatusCode::BAD_REQUEST
            })?;
            audio::bytes_to_pcm16(&bytes)
        }
        (None, None) => return Err(StatusCode::BAD_REQUEST),
    };

    let Some(pcm_tx) = call.paced_pcm_writer() else {
        tracing::error!("call {call_id}: audio writer not available");
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
    };

    // Cancel any existing playback on this call
    if let Some(prev) = state.plays.write().await.remove(&call_id) {
        prev.cancel
            .store(true, std::sync::atomic::Ordering::Relaxed);
        prev.task.abort();
    }

    let play_id = format!(
        "play_{}",
        state
            .play_counter
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    );

    let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let cancel_flag = cancel.clone();
    let pid = play_id.clone();
    let cid = call_id.clone();
    let webhook = state.webhook.clone();
    let loop_count = req.loop_count;

    let task = tokio::task::spawn_blocking(move || {
        let loops = if loop_count == 0 {
            u32::MAX
        } else {
            loop_count
        };

        for _ in 0..loops {
            if cancel_flag.load(std::sync::atomic::Ordering::Relaxed) {
                return true; // interrupted
            }
            // paced_pcm_writer handles chunking into codec frames and real-time pacing
            if pcm_tx.send(pcm_data.clone()).is_err() {
                return true; // call ended
            }
        }
        false // completed naturally
    });

    let plays = state.plays.clone();
    let pid_done = pid.clone();
    let cid_done = cid.clone();

    // Wrapper task that waits for completion and fires webhook
    let handle = tokio::spawn(async move {
        let interrupted = task.await.unwrap_or(true);
        plays.write().await.remove(&cid_done);
        webhook
            .send_event(&WebhookEvent::PlayFinished {
                call_id: cid_done,
                play_id: pid_done,
                interrupted,
            })
            .await;
    });

    state.plays.write().await.insert(
        call_id,
        crate::state::PlayHandle {
            cancel,
            task: handle,
        },
    );

    Ok(Json(PlayResponse { play_id: pid }))
}

async fn stop_play(State(state): State<AppState>, Path(call_id): Path<String>) -> StatusCode {
    if let Some(handle) = state.plays.write().await.remove(&call_id) {
        handle
            .cancel
            .store(true, std::sync::atomic::Ordering::Relaxed);
        // The wrapper task will fire the play_finished webhook with interrupted=true
        StatusCode::OK
    } else {
        StatusCode::NOT_FOUND
    }
}

async fn list_webhook_failures(State(state): State<AppState>) -> Json<serde_json::Value> {
    let failures = state.webhook.dlq_list();
    Json(serde_json::json!({ "failures": failures }))
}

async fn drain_webhook_failures(State(state): State<AppState>) -> Json<serde_json::Value> {
    let failures = state.webhook.dlq_drain();
    Json(serde_json::json!({ "drained": failures.len() }))
}

/// Query parameters for the WebSocket endpoint.
#[derive(Debug, serde::Deserialize, Default)]
struct WsQuery {
    /// Stream mode: "twilio" (default) or "native".
    mode: Option<StreamMode>,
}

async fn ws_handler(
    State(state): State<AppState>,
    Path(call_id): Path<String>,
    axum::extract::Query(query): axum::extract::Query<WsQuery>,
    ws: WebSocketUpgrade,
) -> Result<impl IntoResponse, StatusCode> {
    let call = get_xphone_call(&state, &call_id).await?;
    let mode = query.mode.unwrap_or(StreamMode::Twilio);

    Ok(ws.on_upgrade(move |socket| handle_ws(socket, call_id, call, state, mode)))
}

async fn handle_ws(
    socket: WebSocket,
    call_id: String,
    call: Arc<dyn CallControl>,
    state: AppState,
    mode: StreamMode,
) {
    let encoding = state.config.stream.encoding.clone();
    let sample_rate = state.config.stream.sample_rate;
    let metrics = state.metrics.clone();
    let ws_senders = state.ws_senders.clone();

    metrics.inc_ws_connections();

    let (mut ws_tx, mut ws_rx) = socket.split();

    // Send "connected" + "start" events (JSON text in both modes)
    let encoding_str = match encoding {
        AudioEncoding::Mulaw => "audio/x-mulaw",
        AudioEncoding::L16 => "audio/x-l16",
    };
    let connected = ServerEvent::Connected {
        protocol: "Call".into(),
        version: "1.0.0".into(),
    };
    if send_json(&mut ws_tx, &connected).await.is_err() {
        metrics.dec_ws_connections();
        return;
    }
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
        metrics.dec_ws_connections();
        return;
    }

    // Get audio channels from xphone (paced writer handles RTP timing internally)
    let (Some(pcm_rx), Some(pcm_tx)) = (call.pcm_reader(), call.paced_pcm_writer()) else {
        tracing::error!("call {call_id}: audio channels not available");
        let _ = send_json(
            &mut ws_tx,
            &ServerEvent::Stop {
                stream_sid: call_id,
            },
        )
        .await;
        metrics.dec_ws_connections();
        return;
    };

    let (audio_tx, mut audio_rx) = tokio::sync::mpsc::channel::<Message>(64);

    // Register WS sender so DTMF callbacks can forward events
    if let Ok(mut senders) = ws_senders.write() {
        senders.insert(call_id.clone(), audio_tx.clone());
    }

    let cid_cleanup = call_id.clone();
    let mark_tx = audio_tx.clone();

    // Blocking reader: pcm_rx → audio_tx
    let reader_mode = mode;
    let encoding_reader = encoding.clone();
    let cid_sender = call_id.clone();
    let mut reader_handle = tokio::task::spawn_blocking(move || {
        let stream_sid = call_id;
        let mut encode_buf = Vec::new();

        match reader_mode {
            StreamMode::Twilio => {
                let b64 = base64::engine::general_purpose::STANDARD;
                let mut timestamp: u64 = 0;

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
                        if audio_tx.blocking_send(Message::Text(json.into())).is_err() {
                            break;
                        }
                    }
                    timestamp += frame.len() as u64;
                }
            }
            StreamMode::Native => {
                while let Ok(frame) = pcm_rx.recv() {
                    encode_buf.clear();
                    audio::pcm16_to_bytes_into(&frame, &mut encode_buf);
                    if let Some(binary) = crate::ws::encode_native_audio(&encode_buf) {
                        if audio_tx
                            .blocking_send(Message::Binary(binary.into()))
                            .is_err()
                        {
                            break;
                        }
                    }
                }
            }
        }
    });

    // Async sender: audio_rx → ws_tx
    let sender_metrics = metrics.clone();
    let mut sender_handle = tokio::spawn(async move {
        while let Some(msg) = audio_rx.recv().await {
            if ws_tx.send(msg).await.is_err() {
                break;
            }
            sender_metrics.inc_ws_frames_sent();
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

    // Receiver: WebSocket → xphone audio (client audio to call)
    let receiver_metrics = metrics.clone();
    let mut receiver_handle = tokio::spawn(async move {
        let b64 = base64::engine::general_purpose::STANDARD;
        while let Some(Ok(msg)) = ws_rx.next().await {
            match msg {
                Message::Binary(data) if mode == StreamMode::Native => {
                    if let Some(pcm_bytes) = crate::ws::decode_native_audio(&data) {
                        receiver_metrics.inc_ws_frames_received();
                        let pcm = audio::bytes_to_pcm16(pcm_bytes);
                        let _ = pcm_tx.try_send(pcm);
                    }
                }
                Message::Text(text) => {
                    if let Ok(event) = serde_json::from_str::<ClientEvent>(&text) {
                        match event {
                            ClientEvent::Media { media, .. } => {
                                if let Ok(bytes) = b64.decode(&media.payload) {
                                    receiver_metrics.inc_ws_frames_received();
                                    let pcm = match encoding {
                                        AudioEncoding::Mulaw => audio::mulaw_to_pcm16(&bytes),
                                        AudioEncoding::L16 => audio::bytes_to_pcm16(&bytes),
                                    };
                                    let _ = pcm_tx.try_send(pcm);
                                }
                            }
                            ClientEvent::Mark {
                                stream_sid, mark, ..
                            } => {
                                // Echo mark back as confirmation
                                let echo = crate::ws::ServerEvent::Mark { stream_sid, mark };
                                if let Ok(json) = serde_json::to_string(&echo) {
                                    let _ = mark_tx.send(Message::Text(json.into())).await;
                                }
                            }
                            ClientEvent::Clear { .. } => {}
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

    // Deregister WS sender
    if let Ok(mut senders) = ws_senders.write() {
        senders.remove(&cid_cleanup);
    }
    metrics.dec_ws_connections();
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
        AuthConfig, Config, ListenConfig, SipConfig, SipTransport, StreamConfig, WebhookConfig,
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
            trunks: Vec::new(),
            webhook: WebhookConfig {
                url: "http://localhost:9090/events".into(),
                timeout: "5s".into(),
                retry: 1,
            },
            stream: StreamConfig::default(),
            auth: AuthConfig::default(),
            tls: crate::config::TlsConfig::default(),
            rate_limit: crate::config::RateLimitConfig::default(),
            server: None,
        }
    }

    fn test_state_from_config(config: Config) -> AppState {
        let metrics = crate::metrics::Metrics::new();
        let webhook = WebhookClient::new(&config.webhook, metrics.clone());
        let (ended_tx, _) = tokio::sync::mpsc::channel(32);
        let (dtmf_tx, _) = tokio::sync::mpsc::channel(32);
        let (state_tx, _) = tokio::sync::mpsc::channel(32);
        AppState::new(config, webhook, ended_tx, dtmf_tx, state_tx, metrics)
    }

    fn test_state() -> AppState {
        test_state_from_config(test_config())
    }

    fn sample_call(id: &str) -> CallInfo {
        CallInfo {
            call_id: id.into(),
            from: "+15551234567".into(),
            to: "+15559876543".into(),
            direction: CallDirection::Inbound,
            status: CallStatus::InProgress,
            peer: None,
        }
    }

    async fn body_json<T: serde::de::DeserializeOwned>(resp: axum::http::Response<Body>) -> T {
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

    // ── GET /health ──

    #[tokio::test]
    async fn health_returns_starting_when_phone_not_connected() {
        let resp = app(test_state())
            .oneshot(
                axum::http::Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body: serde_json::Value = body_json(resp).await;
        assert_eq!(body["status"], "starting");
        assert_eq!(body["sip_trunks"], 0);
        assert_eq!(body["sip_server"], false);
        assert_eq!(body["active_calls"], 0);
    }

    #[tokio::test]
    async fn health_reports_active_call_count() {
        let state = test_state();
        state
            .calls
            .write()
            .await
            .insert("call_001".into(), sample_call("call_001"));

        let resp = app(state)
            .oneshot(
                axum::http::Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body: serde_json::Value = body_json(resp).await;
        assert_eq!(body["active_calls"], 1);
    }

    // ── GET /metrics ──

    #[tokio::test]
    async fn metrics_returns_prometheus_format() {
        let resp = app(test_state())
            .oneshot(
                axum::http::Request::builder()
                    .uri("/metrics")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let ct = resp
            .headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap();
        assert!(ct.contains("text/plain"));
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let body = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(body.contains("xbridge_active_calls"));
        assert!(body.contains("xbridge_calls_total"));
        assert!(body.contains("xbridge_ws_connections"));
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

    #[tokio::test]
    async fn ws_upgrade_success() {
        let state = test_state();
        let (mock, _play_rx, _audio_tx) = crate::call_control::mock::MockCall::with_pcm_channels();
        insert_mock_call_custom(&state, "c1", mock).await;
        let addr = spawn_server(state).await;

        let (mut ws, resp) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws/c1"))
            .await
            .unwrap();
        assert_eq!(resp.status(), 101u16);

        // Should receive "connected" event
        use futures_util::StreamExt;
        let msg = ws.next().await.unwrap().unwrap();
        let text = msg.into_text().unwrap();
        let event: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(event["event"], "connected");
        assert_eq!(event["protocol"], "Call");

        // Should receive "start" event
        let msg = ws.next().await.unwrap().unwrap();
        let text = msg.into_text().unwrap();
        let event: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(event["event"], "start");
        assert_eq!(event["streamSid"], "c1");
    }

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
                    .body(Body::from(r#"{"to":"+15551234567","from":"+15559876543"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    // ── Call control endpoints ──

    /// Insert a MockCall into both registries, returning the Arc<MockCall> for assertions.
    async fn insert_mock_call(
        state: &AppState,
        id: &str,
    ) -> Arc<crate::call_control::mock::MockCall> {
        let mock = Arc::new(crate::call_control::mock::MockCall::default());
        state.calls.write().await.insert(id.into(), sample_call(id));
        state
            .xphone_calls
            .write()
            .await
            .insert(id.into(), mock.clone());
        mock
    }

    /// Insert a MockCall with custom config.
    async fn insert_mock_call_custom(
        state: &AppState,
        id: &str,
        mock: crate::call_control::mock::MockCall,
    ) -> Arc<crate::call_control::mock::MockCall> {
        let mock = Arc::new(mock);
        state.calls.write().await.insert(id.into(), sample_call(id));
        state
            .xphone_calls
            .write()
            .await
            .insert(id.into(), mock.clone());
        mock
    }

    // ── Hold ──

    #[tokio::test]
    async fn hold_success() {
        let state = test_state();
        insert_mock_call(&state, "c1").await;

        let resp = app(state.clone())
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/v1/calls/c1/hold")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            state.calls.read().await.get("c1").unwrap().status,
            CallStatus::OnHold
        );
    }

    #[tokio::test]
    async fn hold_error_returns_500() {
        let state = test_state();
        let mock = crate::call_control::mock::MockCall {
            hold_ok: false,
            ..Default::default()
        };
        insert_mock_call_custom(&state, "c1", mock).await;

        let resp = app(state)
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/v1/calls/c1/hold")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

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

    // ── Resume ──

    #[tokio::test]
    async fn resume_success() {
        let state = test_state();
        insert_mock_call(&state, "c1").await;
        // Set status to OnHold first
        state.calls.write().await.get_mut("c1").unwrap().status = CallStatus::OnHold;

        let resp = app(state.clone())
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/v1/calls/c1/resume")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            state.calls.read().await.get("c1").unwrap().status,
            CallStatus::InProgress
        );
    }

    #[tokio::test]
    async fn resume_error_returns_500() {
        let state = test_state();
        let mock = crate::call_control::mock::MockCall {
            resume_ok: false,
            ..Default::default()
        };
        insert_mock_call_custom(&state, "c1", mock).await;

        let resp = app(state)
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/v1/calls/c1/resume")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
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

    // ── Transfer ──

    #[tokio::test]
    async fn transfer_success() {
        let state = test_state();
        let mock = insert_mock_call(&state, "c1").await;

        let resp = app(state)
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/v1/calls/c1/transfer")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"target":"sip:1003@pbx"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let targets = mock.transfer_log.lock().unwrap();
        assert_eq!(*targets, vec!["sip:1003@pbx"]);
    }

    #[tokio::test]
    async fn transfer_error_returns_500() {
        let state = test_state();
        let mock = crate::call_control::mock::MockCall {
            transfer_ok: false,
            ..Default::default()
        };
        insert_mock_call_custom(&state, "c1", mock).await;

        let resp = app(state)
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/v1/calls/c1/transfer")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"target":"sip:1003@pbx"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
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

    // ── DTMF ──

    #[tokio::test]
    async fn dtmf_success() {
        let state = test_state();
        let mock = insert_mock_call(&state, "c1").await;

        let resp = app(state)
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/v1/calls/c1/dtmf")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"digits":"1234"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let digits = mock.dtmf_log.lock().unwrap();
        assert_eq!(*digits, vec!["1", "2", "3", "4"]);
    }

    #[tokio::test]
    async fn dtmf_sends_star_and_hash() {
        let state = test_state();
        let mock = insert_mock_call(&state, "c1").await;

        let resp = app(state)
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/v1/calls/c1/dtmf")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"digits":"*#0"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let digits = mock.dtmf_log.lock().unwrap();
        assert_eq!(*digits, vec!["*", "#", "0"]);
    }

    #[tokio::test]
    async fn dtmf_error_returns_500() {
        let state = test_state();
        let mock = crate::call_control::mock::MockCall {
            dtmf_ok: false,
            ..Default::default()
        };
        insert_mock_call_custom(&state, "c1", mock).await;

        let resp = app(state)
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/v1/calls/c1/dtmf")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"digits":"1"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
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

    // ── Mute / Unmute ──

    #[tokio::test]
    async fn mute_success() {
        let state = test_state();
        insert_mock_call(&state, "c1").await;

        let resp = app(state)
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/v1/calls/c1/mute")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn mute_error_returns_500() {
        let state = test_state();
        let mock = crate::call_control::mock::MockCall {
            mute_ok: false,
            ..Default::default()
        };
        insert_mock_call_custom(&state, "c1", mock).await;

        let resp = app(state)
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/v1/calls/c1/mute")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
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
    async fn unmute_success() {
        let state = test_state();
        insert_mock_call(&state, "c1").await;

        let resp = app(state)
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/v1/calls/c1/unmute")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn unmute_error_returns_500() {
        let state = test_state();
        let mock = crate::call_control::mock::MockCall {
            unmute_ok: false,
            ..Default::default()
        };
        insert_mock_call_custom(&state, "c1", mock).await;

        let resp = app(state)
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/v1/calls/c1/unmute")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
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

    // ── Hangup with xphone call ──

    #[tokio::test]
    async fn hangup_ends_xphone_call() {
        let state = test_state();
        insert_mock_call(&state, "c1").await;

        let resp = app(state.clone())
            .oneshot(
                axum::http::Request::builder()
                    .method("DELETE")
                    .uri("/v1/calls/c1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
        assert!(state.calls.read().await.get("c1").is_none());
        assert!(state.xphone_calls.read().await.get("c1").is_none());
    }

    // ── Play audio ──

    #[tokio::test]
    async fn play_base64_success() {
        let state = test_state();
        let (mock, _play_rx, _audio_tx) = crate::call_control::mock::MockCall::with_pcm_channels();
        insert_mock_call_custom(&state, "c1", mock).await;

        // 4 bytes of raw PCM = 2 samples
        let pcm_bytes = [0x00u8, 0x01, 0xFF, 0x7F];
        let b64 = base64::engine::general_purpose::STANDARD.encode(pcm_bytes);
        let body = serde_json::json!({"audio": b64, "loop_count": 1});

        let resp = app(state)
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/v1/calls/c1/play")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body: serde_json::Value = body_json(resp).await;
        assert!(body["play_id"].as_str().unwrap().starts_with("play_"));
    }

    #[tokio::test]
    async fn play_no_audio_source_returns_400() {
        let state = test_state();
        let (mock, _play_rx, _audio_tx) = crate::call_control::mock::MockCall::with_pcm_channels();
        insert_mock_call_custom(&state, "c1", mock).await;

        let resp = app(state)
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/v1/calls/c1/play")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"loop_count":1}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn play_no_pcm_writer_returns_500() {
        let state = test_state();
        // Default mock has no pcm channels
        insert_mock_call(&state, "c1").await;

        let pcm_bytes = [0x00u8, 0x01, 0xFF, 0x7F];
        let b64 = base64::engine::general_purpose::STANDARD.encode(pcm_bytes);
        let body = serde_json::json!({"audio": b64, "loop_count": 1});

        let resp = app(state)
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/v1/calls/c1/play")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn play_increments_play_id() {
        let state = test_state();
        let (mock1, _, _) = crate::call_control::mock::MockCall::with_pcm_channels();
        insert_mock_call_custom(&state, "c1", mock1).await;

        let pcm_bytes = [0x00u8; 4];
        let b64 = base64::engine::general_purpose::STANDARD.encode(pcm_bytes);
        let body = serde_json::json!({"audio": &b64, "loop_count": 1});

        let resp1 = app(state.clone())
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/v1/calls/c1/play")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();

        let body1: serde_json::Value = body_json(resp1).await;
        let id1 = body1["play_id"].as_str().unwrap().to_string();

        // Need to re-insert since the app consumed it
        let (mock2, _, _) = crate::call_control::mock::MockCall::with_pcm_channels();
        insert_mock_call_custom(&state, "c1", mock2).await;

        let resp2 = app(state)
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/v1/calls/c1/play")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({"audio": &b64, "loop_count": 1}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        let body2: serde_json::Value = body_json(resp2).await;
        let id2 = body2["play_id"].as_str().unwrap().to_string();

        assert_ne!(id1, id2);
    }

    // ── Stop play ──

    #[tokio::test]
    async fn stop_play_success() {
        let state = test_state();
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let cancel_check = cancel.clone();
        let handle = crate::state::PlayHandle {
            cancel,
            task: tokio::spawn(futures_util::future::pending()),
        };
        state.plays.write().await.insert("c1".into(), handle);

        let resp = app(state)
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/v1/calls/c1/play/stop")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        assert!(cancel_check.load(std::sync::atomic::Ordering::Relaxed));
    }

    // ── Play audio (error paths) ──

    #[tokio::test]
    async fn play_not_found() {
        let resp = app(test_state())
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/v1/calls/nonexistent/play")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"audio":"AAAA","loop_count":1}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn play_stop_not_found() {
        let resp = app(test_state())
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/v1/calls/nonexistent/play/stop")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn play_rejects_missing_xphone_call() {
        let state = test_state();
        state
            .calls
            .write()
            .await
            .insert("c1".into(), sample_call("c1"));

        let resp = app(state)
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/v1/calls/c1/play")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"loop_count":1}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        // Call exists in calls registry but not in xphone_calls → NOT_FOUND
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    // ── Trunk selection in create_call ──

    #[tokio::test]
    async fn create_call_unknown_trunk_returns_404() {
        let state = test_state();
        // Insert a phone so phones isn't empty (otherwise we'd get 503)
        state.phones.write().await.insert(
            "default".into(),
            xphone::Phone::new(xphone::Config::default()),
        );

        let resp = app(state)
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/v1/calls")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"to":"+15551234567","from":"+15559876543","trunk":"nonexistent"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    // ── Peer call error paths ──

    #[tokio::test]
    async fn create_call_to_peer_no_server_config_returns_404() {
        let state = test_state(); // server: None
        let resp = app(state)
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/v1/calls")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"to":"+15551234567","from":"+15559876543","peer":"office-pbx"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn create_call_to_peer_unknown_peer_returns_404() {
        let mut config = test_config();
        config.server = Some(crate::trunk::config::ServerConfig {
            listen: "0.0.0.0:5080".into(),
            peers: vec![],
            rtp_port_min: 0,
            rtp_port_max: 0,
            rtp_address: None,
        });
        let state = test_state_from_config(config);

        let resp = app(state)
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/v1/calls")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"to":"+15551234567","from":"+15559876543","peer":"nonexistent"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn create_call_to_peer_trunk_not_running_returns_503() {
        let mut config = test_config();
        config.server = Some(crate::trunk::config::ServerConfig {
            listen: "0.0.0.0:5080".into(),
            peers: vec![crate::trunk::config::PeerConfig {
                name: "office".into(),
                host: Some(std::net::IpAddr::V4(std::net::Ipv4Addr::new(10, 0, 0, 1))),
                hosts: vec![],
                port: 5060,
                auth: None,
                codecs: vec![],
                rtp_address: None,
            }],
            rtp_port_min: 0,
            rtp_port_max: 0,
            rtp_address: None,
        });
        let state = test_state_from_config(config);
        // xphone_server is None (server not started) → 503

        let resp = app(state)
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/v1/calls")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"to":"+15551234567","from":"+15559876543","peer":"office"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    // ── Webhook DLQ endpoints ──

    #[tokio::test]
    async fn list_webhook_failures_empty() {
        let resp = app(test_state())
            .oneshot(
                axum::http::Request::builder()
                    .uri("/v1/webhooks/failures")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body: serde_json::Value = body_json(resp).await;
        assert_eq!(body["failures"], serde_json::json!([]));
    }

    #[tokio::test]
    async fn drain_webhook_failures_returns_count() {
        let resp = app(test_state())
            .oneshot(
                axum::http::Request::builder()
                    .method("DELETE")
                    .uri("/v1/webhooks/failures")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body: serde_json::Value = body_json(resp).await;
        assert_eq!(body["drained"], 0);
    }

    // ── Auth middleware ──

    fn test_state_with_auth(api_key: &str) -> AppState {
        let mut config = test_config();
        config.auth = AuthConfig {
            api_key: Some(api_key.into()),
        };
        test_state_from_config(config)
    }

    #[tokio::test]
    async fn auth_rejects_without_header() {
        let state = test_state_with_auth("secret-key");
        let resp = app(state)
            .oneshot(
                axum::http::Request::builder()
                    .uri("/v1/calls")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn auth_rejects_wrong_key() {
        let state = test_state_with_auth("secret-key");
        let resp = app(state)
            .oneshot(
                axum::http::Request::builder()
                    .uri("/v1/calls")
                    .header("authorization", "Bearer wrong-key")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn auth_accepts_correct_key() {
        let state = test_state_with_auth("secret-key");
        let resp = app(state)
            .oneshot(
                axum::http::Request::builder()
                    .uri("/v1/calls")
                    .header("authorization", "Bearer secret-key")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn auth_does_not_protect_health() {
        let state = test_state_with_auth("secret-key");
        let resp = app(state)
            .oneshot(
                axum::http::Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn no_auth_config_allows_all() {
        // Default test_state has no api_key — all requests pass
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
    }

    // ── Rate limiting ──

    #[test]
    fn rate_limiter_allows_within_limit() {
        let limiter = RateLimiter::new(100);
        for _ in 0..100 {
            assert!(limiter.try_acquire());
        }
    }

    #[test]
    fn rate_limiter_rejects_over_burst() {
        let limiter = RateLimiter::new(10);
        // Burst is 2x rate = 20 tokens
        for _ in 0..20 {
            assert!(limiter.try_acquire());
        }
        assert!(!limiter.try_acquire());
    }

    #[tokio::test]
    async fn rate_limit_returns_429() {
        let mut config = test_config();
        config.rate_limit.requests_per_second = Some(1); // 1 rps, burst = 2
        let state = test_state_from_config(config);

        let router = app(state);

        // First 2 requests should succeed (burst = 2)
        for _ in 0..2 {
            let resp = router
                .clone()
                .oneshot(
                    axum::http::Request::builder()
                        .uri("/v1/calls")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::OK);
        }

        // Third should be rate limited
        let resp = router
            .oneshot(
                axum::http::Request::builder()
                    .uri("/v1/calls")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
    }

    #[tokio::test]
    async fn rate_limit_does_not_affect_health() {
        let mut config = test_config();
        config.rate_limit.requests_per_second = Some(1);
        let state = test_state_from_config(config);

        let router = app(state);

        // Exhaust rate limit
        for _ in 0..3 {
            let _ = router
                .clone()
                .oneshot(
                    axum::http::Request::builder()
                        .uri("/v1/calls")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await;
        }

        // Health should still work
        let resp = router
            .oneshot(
                axum::http::Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }
}

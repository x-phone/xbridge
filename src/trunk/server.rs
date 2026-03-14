//! SIP trunk host server — thin wrapper around xphone::Server.
//!
//! Converts xbridge config to xphone types, wires incoming calls to the webhook
//! pipeline, and exposes the Server handle for outbound calls via the REST API.

use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{error, info};

use crate::bridge;
use crate::call_control::XphoneCall;
use crate::state::AppState;
use crate::trunk::config::ServerConfig;

/// Run the SIP trunk host server.
///
/// Creates an xphone::Server from config, wires incoming call handling,
/// and awaits `server.listen()`.
pub async fn run(
    config: ServerConfig,
    state: AppState,
) -> Result<(), Box<dyn std::error::Error>> {
    let xphone_config = config.to_xphone();
    let server = xphone::Server::new(xphone_config);

    // Store server handle in state so the router can use it for outbound calls.
    *state.xphone_server.write().await = Some(server.clone());

    info!("trunk host listening on {}", config.listen);

    // Channel for incoming calls (xphone callback → async handler).
    let (incoming_tx, mut incoming_rx) = mpsc::channel::<Arc<xphone::Call>>(256);

    server.on_incoming(move |call: Arc<xphone::Call>| {
        if let Err(e) = incoming_tx.try_send(call) {
            error!("incoming trunk call channel full or closed");
            if let tokio::sync::mpsc::error::TrySendError::Full(rejected) = e {
                let _ = rejected.reject(503, "overloaded");
            }
        }
    });

    // Spawn incoming call handler.
    let handler_state = state.clone();
    tokio::spawn(async move {
        while let Some(call) = incoming_rx.recv().await {
            let state = handler_state.clone();
            tokio::spawn(async move {
                handle_incoming(call, state).await;
            });
        }
    });

    server.listen().await.map_err(|e| e.into())
}

/// Handle an authenticated incoming trunk call: dispatch webhook, accept/reject,
/// register in state, wire callbacks.
async fn handle_incoming(call: Arc<xphone::Call>, state: AppState) {
    let call_id = call.id();
    let from = call.from();
    let to = call.to();

    info!("trunk incoming call {call_id}: {from} → {to}");

    // ── Webhook dispatch ──
    let hook = crate::api::IncomingCallWebhook {
        call_id: call_id.clone(),
        from: from.clone(),
        to: to.clone(),
        direction: crate::call::CallDirection::Inbound,
        peer: None,
    };

    let response = match state.webhook.send_incoming(&hook).await {
        Ok(resp) => resp,
        Err(e) => {
            error!("incoming webhook failed for trunk call {call_id}: {e}");
            let _ = call.reject(503, "service unavailable");
            return;
        }
    };

    match response.action {
        crate::api::IncomingCallAction::Accept => {
            if let Err(e) = call.accept() {
                error!("failed to accept trunk call {call_id}: {e}");
                return;
            }

            let info = crate::call::CallInfo {
                call_id: call_id.clone(),
                from,
                to,
                direction: crate::call::CallDirection::Inbound,
                status: crate::call::CallStatus::InProgress,
                peer: None,
            };

            state.metrics.inc_calls_inbound();
            state.metrics.inc_trunk_calls_inbound();

            {
                state.calls.write().await.insert(call_id.clone(), info);
                state
                    .xphone_calls
                    .write()
                    .await
                    .insert(call_id.clone(), Arc::new(XphoneCall(call.clone())));
            }

            bridge::wire_call_callbacks(&call, &call_id, &state);

            state
                .webhook
                .send_event(&crate::webhook::WebhookEvent::Answered {
                    call_id: call_id.clone(),
                })
                .await;
        }
        crate::api::IncomingCallAction::Reject => {
            let reason = response.reason.as_deref().unwrap_or("busy");
            let code = reject_reason_to_sip_code(reason);
            info!("rejecting trunk call {call_id}: {reason}");
            let _ = call.reject(code, reason);
        }
    }
}

/// Map a reject reason string to a SIP status code.
pub(crate) fn reject_reason_to_sip_code(reason: &str) -> u16 {
    match reason {
        "busy" => 486,
        "declined" => 603,
        _ => 486,
    }
}


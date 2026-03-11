use std::sync::Arc;
use tokio::sync::mpsc;

use crate::api::{IncomingCallAction, IncomingCallWebhook};
use crate::call::{CallDirection, CallInfo, CallStatus};
use crate::call_control::XphoneCall;
use crate::config::{Config, SipConfig};
use crate::state::AppState;
use crate::webhook::WebhookEvent;

/// Start the SIP bridge: register with all configured trunks, handle incoming calls.
pub async fn run(
    config: &Config,
    state: AppState,
    mut ended_rx: mpsc::Receiver<(String, xphone::EndReason, std::time::Duration)>,
    mut dtmf_rx: mpsc::Receiver<(String, String)>,
    mut state_rx: mpsc::Receiver<(String, xphone::CallState)>,
) -> Result<(), Box<dyn std::error::Error>> {
    let trunks = config.resolved_trunks();
    if trunks.is_empty() {
        return Err("no SIP trunks configured".into());
    }

    // Channel for incoming calls (shared across all trunks)
    let (incoming_tx, mut incoming_rx) = mpsc::channel::<Arc<xphone::Call>>(32);

    // Connect each trunk
    for trunk in &trunks {
        let xphone_config = build_xphone_config(&trunk.sip);
        let phone = xphone::Phone::new(xphone_config);

        let trunk_name = trunk.name.clone();
        let tx = incoming_tx.clone();

        phone.on_registered({
            let name = trunk_name.clone();
            move || tracing::info!("SIP registration successful for trunk '{name}'")
        });

        phone.on_unregistered({
            let name = trunk_name.clone();
            move || tracing::warn!("SIP registration lost for trunk '{name}'")
        });

        phone.on_incoming(move |call: Arc<xphone::Call>| {
            if tx.blocking_send(call).is_err() {
                tracing::error!("incoming call channel closed");
            }
        });

        let phone = {
            let phone_for_connect = phone;
            let name = trunk_name.clone();
            tokio::task::spawn_blocking(move || {
                phone_for_connect.connect().map_err(|e| {
                    tracing::error!("trunk '{name}' connect failed: {e}");
                    e
                })?;
                Ok::<_, xphone::Error>(phone_for_connect)
            })
            .await??
        };

        tracing::info!("trunk '{}' connected", trunk_name);
        state.phones.write().await.insert(trunk_name, phone);
    }

    // Drop our copy so the channel closes when all phones drop theirs
    drop(incoming_tx);

    // Spawn call-ended cleanup task
    let ended_state = state.clone();
    tokio::spawn(async move {
        while let Some((call_id, reason, duration)) = ended_rx.recv().await {
            ended_state.calls.write().await.remove(&call_id);
            ended_state.xphone_calls.write().await.remove(&call_id);

            let reason_str = end_reason_str(reason);

            ended_state
                .webhook
                .send_event(&WebhookEvent::Ended {
                    call_id,
                    reason: reason_str.to_string(),
                    duration: duration.as_secs(),
                })
                .await;
        }
    });

    // Spawn DTMF webhook delivery task
    let dtmf_state = state.clone();
    tokio::spawn(async move {
        while let Some((call_id, digit)) = dtmf_rx.recv().await {
            dtmf_state
                .webhook
                .send_event(&WebhookEvent::Dtmf { call_id, digit })
                .await;
        }
    });

    // Spawn outbound call-state transition task
    let state_state = state.clone();
    tokio::spawn(async move {
        while let Some((call_id, new_state)) = state_rx.recv().await {
            match new_state {
                xphone::CallState::RemoteRinging => {
                    let (from, to) = {
                        let mut calls = state_state.calls.write().await;
                        if let Some(info) = calls.get_mut(&call_id) {
                            info.status = CallStatus::Ringing;
                            (info.from.clone(), info.to.clone())
                        } else {
                            continue;
                        }
                    };
                    state_state
                        .webhook
                        .send_event(&WebhookEvent::Ringing { call_id, from, to })
                        .await;
                }
                xphone::CallState::Active => {
                    // Only send Answered on the initial answer (Dialing/Ringing → Active).
                    // Resume from hold triggers Active too, but REST handler sends Resumed.
                    let should_notify = {
                        let mut calls = state_state.calls.write().await;
                        if let Some(info) = calls.get_mut(&call_id) {
                            if info.status == CallStatus::Dialing
                                || info.status == CallStatus::Ringing
                            {
                                info.status = CallStatus::InProgress;
                                true
                            } else {
                                false
                            }
                        } else {
                            false
                        }
                    };
                    if should_notify {
                        state_state
                            .webhook
                            .send_event(&WebhookEvent::Answered { call_id })
                            .await;
                    }
                }
                _ => {}
            }
        }
    });

    // Handle incoming calls
    while let Some(call) = incoming_rx.recv().await {
        let state = state.clone();
        tokio::spawn(async move {
            handle_incoming(call, state).await;
        });
    }

    Ok(())
}

async fn handle_incoming(call: Arc<xphone::Call>, state: AppState) {
    let call_id = call.id();
    let from = call.from();
    let to = call.to();

    tracing::info!("incoming call {call_id} from {from} to {to}");

    // Dispatch incoming webhook to app
    let hook = IncomingCallWebhook {
        call_id: call_id.clone(),
        from: from.clone(),
        to: to.clone(),
        direction: CallDirection::Inbound,
    };

    let response = match state.webhook.send_incoming(&hook).await {
        Ok(resp) => resp,
        Err(e) => {
            tracing::error!("incoming webhook failed for {call_id}: {e}");
            let _ = call.reject(503, "service unavailable");
            return;
        }
    };

    match response.action {
        IncomingCallAction::Accept => {
            if let Err(e) = call.accept() {
                tracing::error!("failed to accept call {call_id}: {e}");
                return;
            }

            let info = CallInfo {
                call_id: call_id.clone(),
                from,
                to,
                direction: CallDirection::Inbound,
                status: CallStatus::InProgress,
            };

            state.metrics.inc_calls_total();
            state.metrics.inc_calls_inbound();

            state.calls.write().await.insert(call_id.clone(), info);
            state
                .xphone_calls
                .write()
                .await
                .insert(call_id.clone(), Arc::new(XphoneCall(call.clone())));

            // Send call.answered webhook
            state
                .webhook
                .send_event(&WebhookEvent::Answered {
                    call_id: call_id.clone(),
                })
                .await;

            wire_call_callbacks(&call, &call_id, &state);
        }
        IncomingCallAction::Reject => {
            let reason = response.reason.as_deref().unwrap_or("busy");
            let code = match reason {
                "busy" => 486,
                "declined" => 603,
                _ => 486,
            };
            tracing::info!("rejecting call {call_id}: {reason}");
            let _ = call.reject(code, reason);
        }
    }
}

/// Wire on_ended and on_dtmf callbacks for a call. Used by both inbound and outbound paths.
pub(crate) fn wire_call_callbacks(call: &Arc<xphone::Call>, call_id: &str, state: &AppState) {
    // Wire call-ended callback
    let call_for_ended = call.clone();
    let cid = call_id.to_string();
    let ended_tx = state.ended_tx.clone();
    call.on_ended(move |reason: xphone::EndReason| {
        let duration = call_for_ended.duration();
        let _ = ended_tx.blocking_send((cid.clone(), reason, duration));
    });

    // Wire DTMF callback — sends via both webhook channel and active WebSocket
    let cid = call_id.to_string();
    let dtmf_tx = state.dtmf_tx.clone();
    let ws_senders = state.ws_senders.clone();
    call.on_dtmf(move |digit: String| {
        // Forward to active WebSocket (if connected)
        if let Ok(senders) = ws_senders.read() {
            if let Some(ws_tx) = senders.get(&cid) {
                let event = crate::ws::ServerEvent::Dtmf {
                    stream_sid: cid.clone(),
                    dtmf: crate::ws::DtmfPayload {
                        digit: digit.clone(),
                    },
                };
                if let Ok(json) = serde_json::to_string(&event) {
                    let _ = ws_tx.blocking_send(axum::extract::ws::Message::Text(json.into()));
                }
            }
        }

        // Forward to webhook drain task (consumes digit)
        let _ = dtmf_tx.blocking_send((cid.clone(), digit));
    });
}

/// Wire on_state callback for outbound calls to track ringing/answered transitions.
pub(crate) fn wire_outbound_state_callbacks(
    call: &Arc<xphone::Call>,
    call_id: &str,
    state: &AppState,
) {
    let cid = call_id.to_string();
    let state_tx = state.state_tx.clone();
    call.on_state(move |new_state: xphone::CallState| {
        let _ = state_tx.blocking_send((cid.clone(), new_state));
    });
}

pub(crate) fn end_reason_str(reason: xphone::EndReason) -> &'static str {
    match reason {
        xphone::EndReason::Local => "local",
        xphone::EndReason::Remote => "normal",
        xphone::EndReason::Transfer => "transfer",
        xphone::EndReason::Rejected => "rejected",
        xphone::EndReason::Cancelled => "cancelled",
        xphone::EndReason::Timeout => "timeout",
        xphone::EndReason::Error => "error",
    }
}

fn build_xphone_config(sip: &SipConfig) -> xphone::Config {
    let transport = match sip.transport {
        crate::config::SipTransport::Udp => "udp",
        crate::config::SipTransport::Tcp => "tcp",
        crate::config::SipTransport::Tls => "tls",
    };

    let mut builder = xphone::PhoneBuilder::new()
        .credentials(&sip.username, &sip.password, &sip.host)
        .transport(transport)
        .rtp_ports(sip.rtp_port_min, sip.rtp_port_max)
        .srtp(sip.srtp);

    if let Some(ref stun) = sip.stun_server {
        builder = builder.stun_server(stun);
    }

    builder.build()
}

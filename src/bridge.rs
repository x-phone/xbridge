use std::sync::Arc;
use tokio::sync::mpsc;

use crate::api::{IncomingCallAction, IncomingCallWebhook};
use crate::call::{CallDirection, CallInfo, CallStatus};
use crate::config::Config;
use crate::state::AppState;
use crate::webhook::WebhookEvent;
use crate::webhook_client::WebhookClient;

/// Start the SIP bridge: register with the trunk, handle incoming calls.
pub async fn run(config: &Config, state: AppState) -> Result<(), Box<dyn std::error::Error>> {
    let webhook = WebhookClient::new(&config.webhook);

    let xphone_config = build_xphone_config(config);
    let phone = xphone::Phone::new(xphone_config);

    // Channel for incoming calls (sync callback → async handler)
    let (incoming_tx, mut incoming_rx) = mpsc::channel::<Arc<xphone::Call>>(32);

    // Channel for call-ended events (sync callback → async cleanup)
    let (ended_tx, mut ended_rx) =
        mpsc::channel::<(String, xphone::EndReason, std::time::Duration)>(32);

    // Channel for DTMF events (sync callback → async webhook)
    let (dtmf_tx, mut dtmf_rx) = mpsc::channel::<(String, String)>(32);

    phone.on_registered(|| {
        tracing::info!("SIP registration successful");
    });

    phone.on_unregistered(|| {
        tracing::warn!("SIP registration lost");
    });

    phone.on_incoming(move |call: Arc<xphone::Call>| {
        if incoming_tx.blocking_send(call).is_err() {
            tracing::error!("incoming call channel closed");
        }
    });

    // Connect to SIP trunk (blocking — runs registration)
    let phone = {
        let phone_for_connect = phone;
        tokio::task::spawn_blocking(move || {
            phone_for_connect.connect()?;
            Ok::<_, xphone::Error>(phone_for_connect)
        })
        .await??
    };

    // Keep phone alive for the lifetime of the bridge
    let _phone = phone;

    // Spawn call-ended cleanup task
    let ended_state = state.clone();
    let ended_webhook = webhook.clone();
    tokio::spawn(async move {
        while let Some((call_id, reason, duration)) = ended_rx.recv().await {
            ended_state.calls.write().await.remove(&call_id);
            ended_state.xphone_calls.write().await.remove(&call_id);

            let reason_str = match reason {
                xphone::EndReason::Local => "local",
                xphone::EndReason::Remote => "normal",
                xphone::EndReason::Transfer => "transfer",
                xphone::EndReason::Rejected => "rejected",
                xphone::EndReason::Cancelled => "cancelled",
                xphone::EndReason::Timeout => "timeout",
                xphone::EndReason::Error => "error",
            };

            ended_webhook
                .send_event(&WebhookEvent::Ended {
                    call_id,
                    reason: reason_str.to_string(),
                    duration: duration.as_secs(),
                })
                .await;
        }
    });

    // Spawn DTMF webhook delivery task
    let dtmf_webhook = webhook.clone();
    tokio::spawn(async move {
        while let Some((call_id, digit)) = dtmf_rx.recv().await {
            dtmf_webhook
                .send_event(&WebhookEvent::Dtmf { call_id, digit })
                .await;
        }
    });

    // Handle incoming calls
    while let Some(call) = incoming_rx.recv().await {
        let state = state.clone();
        let webhook = webhook.clone();
        let ended_tx = ended_tx.clone();
        let dtmf_tx = dtmf_tx.clone();
        tokio::spawn(async move {
            handle_incoming(call, state, webhook, ended_tx, dtmf_tx).await;
        });
    }

    Ok(())
}

async fn handle_incoming(
    call: Arc<xphone::Call>,
    state: AppState,
    webhook: WebhookClient,
    ended_tx: mpsc::Sender<(String, xphone::EndReason, std::time::Duration)>,
    dtmf_tx: mpsc::Sender<(String, String)>,
) {
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

    let response = match webhook.send_incoming(&hook).await {
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

            state.calls.write().await.insert(call_id.clone(), info);
            state
                .xphone_calls
                .write()
                .await
                .insert(call_id.clone(), call.clone());

            // Send call.answered webhook
            webhook
                .send_event(&WebhookEvent::Answered {
                    call_id: call_id.clone(),
                })
                .await;

            // Wire call-ended callback
            let call_for_ended = call.clone();
            let cid = call_id.clone();
            call.on_ended(move |reason: xphone::EndReason| {
                let duration = call_for_ended.duration();
                let _ = ended_tx.blocking_send((cid.clone(), reason, duration));
            });

            // Wire DTMF callback
            let cid = call_id;
            call.on_dtmf(move |digit: String| {
                let _ = dtmf_tx.blocking_send((cid.clone(), digit));
            });
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

fn build_xphone_config(config: &Config) -> xphone::Config {
    let transport = match config.sip.transport {
        crate::config::SipTransport::Udp => "udp",
        crate::config::SipTransport::Tcp => "tcp",
        crate::config::SipTransport::Tls => "tls",
    };

    xphone::PhoneBuilder::new()
        .credentials(
            &config.sip.username,
            &config.sip.password,
            &config.sip.host,
        )
        .transport(transport)
        .rtp_ports(config.sip.rtp_port_min, config.sip.rtp_port_max)
        .srtp(config.sip.srtp)
        .build()
}

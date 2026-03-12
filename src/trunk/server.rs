//! SIP trunk host server — listens for incoming SIP traffic from PBX peers.
//!
//! Authenticated INVITEs are wired through xphone::Call (via TrunkDialog) so that
//! xphone handles codec negotiation, SDP answer generation, and the full RTP media
//! pipeline. The trunk server only handles SIP signalling transport.

use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use crate::bridge;
use crate::call_control::XphoneCall;
use crate::state::{AppState, TrunkDialogEntry, TrunkDialogMap};
use crate::trunk::auth::{self, AuthResult};
use crate::trunk::config::ServerConfig;
use crate::trunk::dialog::{SipOutgoing, TrunkDialog};
use crate::trunk::sip_msg::{self, SipMessage};
use crate::trunk::util::{ensure_to_tag, generate_tag, reject_reason_to_sip_code, uuid_v4};

/// Run the SIP trunk host server.
///
/// Listens on the configured address, authenticates incoming SIP traffic,
/// and forwards authenticated INVITEs into xbridge's call flow via xphone::Call.
pub async fn run(
    config: ServerConfig,
    state: AppState,
) -> Result<(), Box<dyn std::error::Error>> {
    let socket = Arc::new(UdpSocket::bind(&config.listen).await?);
    let local_addr = socket.local_addr()?;
    info!("trunk host listening on {}", config.listen);

    let dialogs = state.trunk_dialogs.clone();

    // Channel for TrunkDialog → socket outgoing messages (bounded to prevent OOM).
    let (sip_tx, mut sip_rx) = mpsc::channel::<SipOutgoing>(4096);

    // Spawn send task: drains outgoing SIP messages and sends via the server socket.
    let send_socket = socket.clone();
    tokio::spawn(async move {
        while let Some(msg) = sip_rx.recv().await {
            if let Err(e) = send_socket.send_to(&msg.data, msg.addr).await {
                warn!("trunk SIP send error: {e}");
            }
        }
    });

    let mut buf = vec![0u8; 65535];

    loop {
        let (len, addr) = socket.recv_from(&mut buf).await?;
        let data = &buf[..len];

        let msg = match sip_msg::parse(data) {
            Ok(m) => m,
            Err(e) => {
                debug!("ignoring unparseable SIP from {addr}: {e}");
                continue;
            }
        };

        if msg.is_response() {
            debug!("ignoring SIP response from {addr}");
            continue;
        }

        match msg.method.as_str() {
            "OPTIONS" => {
                handle_options(&socket, addr, &msg).await;
            }
            "INVITE" => {
                handle_invite(
                    &config, &socket, local_addr, addr, msg, &state, &dialogs, &sip_tx,
                )
                .await;
            }
            "ACK" => {
                debug!("ACK received for Call-ID={}", msg.call_id());
            }
            "BYE" => {
                handle_bye(&socket, addr, &msg, &state, &dialogs).await;
            }
            "CANCEL" => {
                handle_cancel(&socket, addr, &msg, &dialogs).await;
            }
            other => {
                debug!("unsupported SIP method '{other}' from {addr}");
                send_response(&socket, addr, &msg, 405, "Method Not Allowed").await;
            }
        }
    }
}

async fn handle_options(socket: &UdpSocket, addr: SocketAddr, msg: &SipMessage) {
    debug!("OPTIONS from {addr}");
    let mut resp = SipMessage::new_response(200, "OK");
    copy_dialog_headers(msg, &mut resp);
    resp.set_header("Allow", "INVITE,ACK,BYE,CANCEL,OPTIONS");
    let data = resp.to_bytes();
    let _ = socket.send_to(&data, addr).await;
}

#[allow(clippy::too_many_arguments)]
async fn handle_invite(
    config: &ServerConfig,
    socket: &Arc<UdpSocket>,
    local_addr: SocketAddr,
    addr: SocketAddr,
    msg: SipMessage,
    state: &AppState,
    dialogs: &TrunkDialogMap,
    sip_tx: &mpsc::Sender<SipOutgoing>,
) {
    let source_ip = addr.ip();

    match auth::authenticate(config, &msg, source_ip) {
        AuthResult::Authenticated(peer_name) => {
            info!("authenticated INVITE from peer '{peer_name}' at {addr}");
            state.metrics.inc_trunk_calls_inbound();

            // Send 100 Trying immediately (before webhook dispatch).
            send_response(socket, addr, &msg, 100, "Trying").await;

            let sip_call_id = msg.call_id().to_string();
            dialogs.write().await.insert(
                sip_call_id.clone(),
                TrunkDialogEntry {
                    xbridge_call_id: None,
                    xphone_call: None,
                },
            );

            let state = state.clone();
            let dialogs = dialogs.clone();
            let sip_tx = sip_tx.clone();
            let rtp_port_min = config.rtp_port_min;
            let rtp_port_max = config.rtp_port_max;
            tokio::spawn(async move {
                handle_trunk_incoming(
                    &msg,
                    peer_name,
                    state,
                    dialogs,
                    sip_call_id,
                    sip_tx,
                    local_addr,
                    addr,
                    rtp_port_min,
                    rtp_port_max,
                )
                .await;
            });
        }
        AuthResult::Challenge { realm, nonce } => {
            info!("challenging INVITE from {addr} (no IP match)");
            let mut resp = SipMessage::new_response(401, "Unauthorized");
            copy_dialog_headers(&msg, &mut resp);
            resp.set_header(
                "WWW-Authenticate",
                &auth::build_www_authenticate(&realm, &nonce),
            );
            let data = resp.to_bytes();
            let _ = socket.send_to(&data, addr).await;
        }
        AuthResult::Rejected => {
            warn!("rejected INVITE from unknown source {addr}");
            state.metrics.inc_trunk_auth_failures();
            send_response(socket, addr, &msg, 403, "Forbidden").await;
        }
    }
}

/// Handle an authenticated incoming trunk call: create xphone::Call, dispatch webhook,
/// accept/reject, wire callbacks.
#[allow(clippy::too_many_arguments)]
async fn handle_trunk_incoming(
    invite: &SipMessage,
    peer_name: String,
    state: AppState,
    dialogs: TrunkDialogMap,
    sip_call_id: String,
    sip_tx: mpsc::Sender<SipOutgoing>,
    local_addr: SocketAddr,
    remote_addr: SocketAddr,
    rtp_port_min: u16,
    rtp_port_max: u16,
) {
    let call_id = format!("trunk-{}", uuid_v4());
    let from = invite.from_user().to_string();
    let to = invite.to_user().to_string();

    info!(
        "trunk incoming call {call_id} from peer '{peer_name}': {from} → {to}"
    );

    // ── Webhook dispatch ──

    let hook = crate::api::IncomingCallWebhook {
        call_id: call_id.clone(),
        from: from.clone(),
        to: to.clone(),
        direction: crate::call::CallDirection::Inbound,
        peer: Some(peer_name.clone()),
    };

    let response = match state.webhook.send_incoming(&hook).await {
        Ok(resp) => resp,
        Err(e) => {
            error!("incoming webhook failed for trunk call {call_id}: {e}");
            send_reject_via_channel(&sip_tx, invite, remote_addr, 503, "Service Unavailable");
            dialogs.write().await.remove(&sip_call_id);
            return;
        }
    };

    match response.action {
        crate::api::IncomingCallAction::Accept => {
            // ── Allocate RTP port ──
            let (rtp_socket, rtp_port) =
                match xphone::media::listen_rtp_port(rtp_port_min, rtp_port_max) {
                    Ok(pair) => pair,
                    Err(e) => {
                        error!("RTP port allocation failed for trunk call {call_id}: {e}");
                        send_reject_via_channel(
                            &sip_tx,
                            invite,
                            remote_addr,
                            503,
                            "Service Unavailable",
                        );
                        dialogs.write().await.remove(&sip_call_id);
                        return;
                    }
                };

            // ── Create TrunkDialog + xphone::Call ──
            let local_tag = generate_tag();
            let dialog = Arc::new(TrunkDialog::new(
                sip_tx,
                local_addr,
                remote_addr,
                invite,
                local_tag,
            ));

            let call = xphone::Call::new_inbound(dialog);

            let local_ip = local_addr.ip().to_string();

            call.set_local_media(&local_ip, rtp_port as i32);
            call.set_rtp_socket(rtp_socket);

            // Set remote SDP from INVITE body (enables codec negotiation in accept()).
            if !invite.body.is_empty() {
                if let Ok(sdp_str) = std::str::from_utf8(&invite.body) {
                    call.set_remote_sdp(sdp_str);
                }
            }

            // ── Accept the call ──
            // Call::accept() negotiates codecs, builds SDP answer, sends 200 OK via
            // TrunkDialog::respond(), and starts the RTP media pipeline.
            if let Err(e) = call.accept() {
                error!("failed to accept trunk call {call_id}: {e}");
                dialogs.write().await.remove(&sip_call_id);
                return;
            }

            // ── Track in state registries ──
            let info = crate::call::CallInfo {
                call_id: call_id.clone(),
                from,
                to,
                direction: crate::call::CallDirection::Inbound,
                status: crate::call::CallStatus::InProgress,
                peer: Some(peer_name),
            };

            state.metrics.inc_calls_total();
            state.metrics.inc_calls_inbound();

            {
                state.calls.write().await.insert(call_id.clone(), info);
                state
                    .xphone_calls
                    .write()
                    .await
                    .insert(call_id.clone(), Arc::new(XphoneCall(call.clone())));
            }

            // Track xphone call in dialog for BYE cleanup.
            if let Some(dialog) = dialogs.write().await.get_mut(&sip_call_id) {
                dialog.xbridge_call_id = Some(call_id.clone());
                dialog.xphone_call = Some(call.clone());
            }

            // Wire on_ended / on_dtmf callbacks (shared with trunk-provider path).
            bridge::wire_call_callbacks(&call, &call_id, &state);

            // Send call.answered webhook.
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
            send_reject_via_channel(&sip_tx, invite, remote_addr, code, reason);
            dialogs.write().await.remove(&sip_call_id);
        }
    }
}

/// Build and send a SIP error response via the outgoing channel (for pre-Call paths).
fn send_reject_via_channel(
    tx: &mpsc::Sender<SipOutgoing>,
    invite: &SipMessage,
    remote_addr: SocketAddr,
    code: u16,
    reason: &str,
) {
    let mut resp = SipMessage::new_response(code, reason);
    copy_dialog_headers(invite, &mut resp);
    let _ = tx.try_send(SipOutgoing {
        data: resp.to_bytes(),
        addr: remote_addr,
    });
}

async fn handle_bye(
    socket: &UdpSocket,
    addr: SocketAddr,
    msg: &SipMessage,
    _state: &AppState,
    dialogs: &TrunkDialogMap,
) {
    let sip_call_id = msg.call_id().to_string();
    debug!("BYE from {addr} for Call-ID={sip_call_id}");

    // Remove dialog and tell xphone::Call about remote hangup.
    // simulate_bye() fires on_ended → bridge cleanup task removes from all registries.
    if let Some(dialog) = dialogs.write().await.remove(&sip_call_id) {
        if let Some(call) = dialog.xphone_call {
            call.simulate_bye();
        }
    }

    send_response(socket, addr, msg, 200, "OK").await;
}

async fn handle_cancel(
    socket: &UdpSocket,
    addr: SocketAddr,
    msg: &SipMessage,
    dialogs: &TrunkDialogMap,
) {
    let sip_call_id = msg.call_id().to_string();
    debug!("CANCEL from {addr} for Call-ID={sip_call_id}");

    let removed = dialogs.write().await.remove(&sip_call_id);
    send_response(socket, addr, msg, 200, "OK").await;

    if let Some(dialog) = removed {
        // Tell xphone::Call about cancellation (if call was created but not yet accepted).
        if let Some(call) = dialog.xphone_call {
            call.simulate_bye();
        }

        // Send 487 Request Terminated for the original INVITE.
        let mut resp = SipMessage::new_response(487, "Request Terminated");
        copy_dialog_headers(msg, &mut resp);
        let (seq, _) = msg.cseq();
        resp.set_header("CSeq", &format!("{seq} INVITE"));
        let data = resp.to_bytes();
        let _ = socket.send_to(&data, addr).await;
    }
}

async fn send_response(
    socket: &UdpSocket,
    addr: SocketAddr,
    req: &SipMessage,
    code: u16,
    reason: &str,
) {
    let mut resp = SipMessage::new_response(code, reason);
    copy_dialog_headers(req, &mut resp);
    let data = resp.to_bytes();
    let _ = socket.send_to(&data, addr).await;
}

fn copy_dialog_headers(req: &SipMessage, resp: &mut SipMessage) {
    for via in req.header_values("Via") {
        resp.add_header("Via", via);
    }
    resp.set_header("From", req.header("From"));
    resp.set_header("To", &ensure_to_tag(req.header("To"), resp.status_code));
    resp.set_header("Call-ID", req.header("Call-ID"));
    resp.set_header("CSeq", req.header("CSeq"));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_dialog_headers_preserves_via() {
        let mut req = SipMessage::new_request("INVITE", "sip:1002@xbridge:5080");
        req.add_header("Via", "SIP/2.0/UDP 10.0.0.1:5060;branch=z9hG4bK111");
        req.add_header("Via", "SIP/2.0/UDP 10.0.0.2:5060;branch=z9hG4bK222");
        req.set_header("From", "<sip:1001@pbx.local>;tag=from1");
        req.set_header("To", "<sip:1002@xbridge:5080>");
        req.set_header("Call-ID", "test@host");
        req.set_header("CSeq", "1 INVITE");

        let mut resp = SipMessage::new_response(200, "OK");
        copy_dialog_headers(&req, &mut resp);

        assert_eq!(resp.header_values("Via").len(), 2);
        assert_eq!(resp.header("Call-ID"), "test@host");
        assert_eq!(resp.header("CSeq"), "1 INVITE");
        assert!(resp.header("From").contains("tag=from1"));
        assert!(resp.header("To").contains("tag="));
    }

    #[test]
    fn copy_dialog_headers_100_no_to_tag() {
        let mut req = SipMessage::new_request("INVITE", "sip:1002@xbridge:5080");
        req.add_header("Via", "SIP/2.0/UDP 10.0.0.1:5060;branch=z9hG4bK111");
        req.set_header("From", "<sip:1001@pbx.local>;tag=from1");
        req.set_header("To", "<sip:1002@xbridge:5080>");
        req.set_header("Call-ID", "test@host");
        req.set_header("CSeq", "1 INVITE");

        let mut resp = SipMessage::new_response(100, "Trying");
        copy_dialog_headers(&req, &mut resp);

        assert!(!resp.header("To").contains("tag="));
    }

    #[test]
    fn send_reject_via_channel_builds_response() {
        let (tx, mut rx) = mpsc::channel(64);
        let mut invite = SipMessage::new_request("INVITE", "sip:1002@xbridge:5080");
        invite.add_header("Via", "SIP/2.0/UDP 10.0.0.1:5060;branch=z9hG4bK111");
        invite.set_header("From", "<sip:1001@pbx.local>;tag=from1");
        invite.set_header("To", "<sip:1002@xbridge:5080>");
        invite.set_header("Call-ID", "test@host");
        invite.set_header("CSeq", "1 INVITE");

        send_reject_via_channel(
            &tx,
            &invite,
            "10.0.0.1:5060".parse().unwrap(),
            486,
            "Busy Here",
        );

        let outgoing = rx.try_recv().unwrap();
        let msg = sip_msg::parse(&outgoing.data).unwrap();
        assert!(msg.is_response());
        assert_eq!(msg.status_code, 486);
        assert_eq!(msg.header("Call-ID"), "test@host");
        assert_eq!(outgoing.addr, "10.0.0.1:5060".parse::<SocketAddr>().unwrap());
    }
}

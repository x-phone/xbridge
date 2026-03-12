//! SIP trunk host server — listens for SIP traffic from PBX peers.
//!
//! Handles both inbound (peer → xbridge) and outbound (xbridge → peer) calls.
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
use crate::trunk::util::{ensure_to_tag, generate_branch, generate_tag, reject_reason_to_sip_code, uuid_v4};

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

    // Store sip_tx and local_addr in state so the router can use them for outbound calls.
    *state.trunk_sip_tx.write().await = Some(sip_tx.clone());
    *state.trunk_local_addr.write().await = Some(local_addr);

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
            handle_response(&msg, &dialogs, &sip_tx, local_addr).await;
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
                    trunk_dialog: None,
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
            if let Some(entry) = dialogs.write().await.get_mut(&sip_call_id) {
                entry.xbridge_call_id = Some(call_id.clone());
                entry.xphone_call = Some(call.clone());
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
    if let Err(e) = tx.try_send(SipOutgoing {
        data: resp.to_bytes(),
        addr: remote_addr,
    }) {
        warn!("failed to send SIP {code} reject to {remote_addr}: {e}");
    }
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

/// Handle a SIP response (for outbound calls to peers).
/// Routes 1xx/2xx/error responses to the corresponding xphone::Call.
async fn handle_response(
    msg: &SipMessage,
    dialogs: &TrunkDialogMap,
    sip_tx: &mpsc::Sender<SipOutgoing>,
    local_addr: SocketAddr,
) {
    let sip_call_id = msg.call_id().to_string();
    let code = msg.status_code;
    let (_, cseq_method) = msg.cseq();

    debug!("SIP response {code} for Call-ID={sip_call_id} (CSeq method={cseq_method})");

    // Only handle responses to INVITE (ignore BYE/CANCEL responses).
    if cseq_method != "INVITE" {
        return;
    }

    let dialogs_read = dialogs.read().await;
    let dialog_entry = match dialogs_read.get(&sip_call_id) {
        Some(entry) => entry,
        None => {
            debug!("no dialog for response Call-ID={sip_call_id}");
            return;
        }
    };

    let call = match dialog_entry.xphone_call.as_ref() {
        Some(c) => c.clone(),
        None => return,
    };
    let trunk_dialog = dialog_entry.trunk_dialog.clone();
    drop(dialogs_read);

    match code {
        100 => {
            debug!("100 Trying for outbound Call-ID={sip_call_id}");
        }
        180 | 183 => {
            if let Some(ref dlg) = trunk_dialog {
                dlg.update_from_response(msg);
            }
            call.simulate_response(code, &msg.reason);
        }
        200..=299 => {
            if let Some(ref dlg) = trunk_dialog {
                dlg.update_from_response(msg);
            }
            if !msg.body.is_empty() {
                if let Ok(sdp_str) = std::str::from_utf8(&msg.body) {
                    call.set_remote_sdp(sdp_str);
                }
            }
            call.simulate_response(200, "OK");

            // Send ACK.
            send_ack(sip_tx, msg, &sip_call_id, local_addr);
        }
        _ => {
            // 3xx/4xx/5xx/6xx: call failed.
            warn!("outbound call {sip_call_id} rejected with {code}");
            call.simulate_bye();
            dialogs.write().await.remove(&sip_call_id);
        }
    }
}

/// Send ACK for a 200 OK response to our outbound INVITE.
fn send_ack(
    tx: &mpsc::Sender<SipOutgoing>,
    ok_response: &SipMessage,
    sip_call_id: &str,
    local_addr: SocketAddr,
) {
    // ACK Request-URI comes from the Contact header of the 200 OK,
    // or fall back to the To header URI.
    let contact = ok_response.header("Contact");
    let request_uri = if !contact.is_empty() {
        crate::trunk::dialog::extract_uri(contact).to_string()
    } else {
        let to = ok_response.header("To");
        crate::trunk::dialog::extract_uri(to).to_string()
    };

    // ACK destination: parse host:port from Contact URI or Request-URI.
    let dest_addr = parse_addr_from_uri(&request_uri)
        .unwrap_or_else(|| "0.0.0.0:5060".parse().unwrap());

    let branch = generate_branch();
    let mut ack = SipMessage::new_request("ACK", &request_uri);
    ack.set_header(
        "Via",
        &format!("SIP/2.0/UDP {local_addr};branch={branch}"),
    );
    ack.set_header("From", ok_response.header("From"));
    ack.set_header("To", ok_response.header("To"));
    ack.set_header("Call-ID", sip_call_id);
    // ACK CSeq must match the INVITE's CSeq number.
    let (cseq_num, _) = ok_response.cseq();
    ack.set_header("CSeq", &format!("{cseq_num} ACK"));

    if let Err(e) = tx.try_send(SipOutgoing {
        data: ack.to_bytes(),
        addr: dest_addr,
    }) {
        warn!("failed to send ACK for Call-ID={sip_call_id}: {e}");
    }
}

/// Parse a SocketAddr from a SIP URI (e.g., "sip:1001@10.0.0.1:5060" → 10.0.0.1:5060).
fn parse_addr_from_uri(uri: &str) -> Option<SocketAddr> {
    let host_part = uri.split('@').nth(1)?;
    // Try parsing as SocketAddr directly.
    if let Ok(addr) = host_part.parse::<SocketAddr>() {
        return Some(addr);
    }
    // Try parsing as IP (default port 5060).
    if let Ok(ip) = host_part.parse::<std::net::IpAddr>() {
        return Some(SocketAddr::new(ip, 5060));
    }
    None
}

/// Build an outbound INVITE to a peer and send it via the SIP channel.
pub(crate) fn build_and_send_invite(
    sip_tx: &mpsc::Sender<SipOutgoing>,
    local_addr: SocketAddr,
    remote_addr: SocketAddr,
    sip_call_id: &str,
    local_tag: &str,
    from: &str,
    to: &str,
    sdp: &str,
) {
    let branch = generate_branch();
    let request_uri = format!("sip:{}@{}", to, remote_addr);

    let mut invite = SipMessage::new_request("INVITE", &request_uri);
    invite.set_header(
        "Via",
        &format!("SIP/2.0/UDP {local_addr};branch={branch}"),
    );
    invite.set_header(
        "From",
        &format!("<sip:{from}@{local_addr}>;tag={local_tag}"),
    );
    invite.set_header("To", &format!("<sip:{to}@{remote_addr}>"));
    invite.set_header("Call-ID", sip_call_id);
    invite.set_header("CSeq", "1 INVITE");
    invite.set_header(
        "Contact",
        &format!("<sip:xbridge@{local_addr}>"),
    );
    invite.set_header("Max-Forwards", "70");
    invite.set_header("Content-Type", "application/sdp");
    invite.body = sdp.as_bytes().to_vec();

    if let Err(e) = sip_tx.try_send(SipOutgoing {
        data: invite.to_bytes(),
        addr: remote_addr,
    }) {
        warn!("failed to send INVITE to {remote_addr}: {e}");
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

    #[test]
    fn parse_addr_from_sip_uri() {
        assert_eq!(
            parse_addr_from_uri("sip:1001@10.0.0.1:5060"),
            Some("10.0.0.1:5060".parse().unwrap())
        );
        assert_eq!(
            parse_addr_from_uri("sip:1001@192.168.1.1"),
            Some("192.168.1.1:5060".parse().unwrap())
        );
        assert!(parse_addr_from_uri("sip:1001").is_none());
    }

    #[test]
    fn build_outbound_invite_message() {
        let (tx, mut rx) = mpsc::channel(64);
        build_and_send_invite(
            &tx,
            "127.0.0.1:5080".parse().unwrap(),
            "10.0.0.1:5060".parse().unwrap(),
            "test-call-id@xbridge",
            "localtag1",
            "1001",
            "1002",
            "v=0\r\n",
        );

        let outgoing = rx.try_recv().unwrap();
        let msg = sip_msg::parse(&outgoing.data).unwrap();
        assert!(!msg.is_response());
        assert_eq!(msg.method, "INVITE");
        assert_eq!(msg.request_uri, "sip:1002@10.0.0.1:5060");
        assert_eq!(msg.header("Call-ID"), "test-call-id@xbridge");
        assert!(msg.header("From").contains("1001@127.0.0.1:5080"));
        assert!(msg.header("From").contains("tag=localtag1"));
        assert!(msg.header("To").contains("1002@10.0.0.1:5060"));
        assert_eq!(msg.header("CSeq"), "1 INVITE");
        assert_eq!(msg.header("Content-Type"), "application/sdp");
        assert_eq!(String::from_utf8_lossy(&msg.body), "v=0\r\n");
        assert_eq!(outgoing.addr, "10.0.0.1:5060".parse::<SocketAddr>().unwrap());
    }
}

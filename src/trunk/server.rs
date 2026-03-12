//! SIP trunk host server — listens for incoming SIP traffic from PBX peers.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::UdpSocket;
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

use crate::state::AppState;
use crate::trunk::auth::{self, AuthResult};
use crate::trunk::config::ServerConfig;
use crate::trunk::sip_msg::{self, parse_cseq, SipMessage};
use crate::trunk::util::{ensure_to_tag, generate_branch, reject_reason_to_sip_code, uuid_v4};

/// An authenticated incoming call ready for xbridge's call flow.
#[derive(Debug)]
pub struct IncomingTrunkCall {
    /// Authenticated peer name.
    pub peer: String,
    /// Caller (From user).
    pub from: String,
    /// Callee (To user / Request-URI user).
    pub to: String,
    /// SDP offer body from the INVITE.
    pub sdp: Vec<u8>,
    /// Handle to send SIP responses back.
    pub responder: TrunkResponder,
}

/// Handle for sending SIP responses back to a peer on a specific dialog.
#[derive(Debug, Clone)]
pub struct TrunkResponder {
    socket: Arc<UdpSocket>,
    remote_addr: SocketAddr,
    via: String,
    from: String,
    to: String,
    call_id: String,
    cseq: String,
    /// Stable to-tag for this dialog (generated once, reused for all responses).
    to_tag: String,
}

impl TrunkResponder {
    /// Send a SIP response to the peer.
    pub async fn respond(&self, code: u16, reason: &str, body: &[u8]) -> std::io::Result<()> {
        let mut msg = SipMessage::new_response(code, reason);
        msg.add_header("Via", &self.via);
        msg.set_header("From", &self.from);
        msg.set_header("To", &self.to_with_tag(code));
        msg.set_header("Call-ID", &self.call_id);
        msg.set_header("CSeq", &self.cseq);
        if !body.is_empty() {
            msg.set_header("Content-Type", "application/sdp");
            msg.body = body.to_vec();
        }
        let data = msg.to_bytes();
        self.socket.send_to(&data, self.remote_addr).await?;
        Ok(())
    }

    /// Send a SIP request to the peer (BYE, etc.).
    pub async fn send_request(
        &self,
        method: &str,
        request_uri: &str,
    ) -> std::io::Result<()> {
        let branch = generate_branch();
        let local_addr = self.socket.local_addr()?;
        let mut msg = SipMessage::new_request(method, request_uri);
        msg.set_header(
            "Via",
            &format!("SIP/2.0/UDP {};branch={}", local_addr, branch),
        );
        msg.set_header("From", &self.from);
        msg.set_header("To", &self.to);
        msg.set_header("Call-ID", &self.call_id);
        let (seq, _) = parse_cseq(&self.cseq);
        msg.set_header("CSeq", &format!("{} {}", seq + 1, method));
        let data = msg.to_bytes();
        self.socket.send_to(&data, self.remote_addr).await?;
        Ok(())
    }

    /// The SIP Call-ID for this dialog.
    pub fn sip_call_id(&self) -> &str {
        &self.call_id
    }

    fn to_with_tag(&self, status_code: u16) -> String {
        if status_code > 100 && !self.to.contains("tag=") {
            format!("{};tag={}", self.to, self.to_tag)
        } else {
            self.to.clone()
        }
    }
}

/// Active dialog state for in-progress calls.
struct ActiveDialog {
    /// Maps SIP Call-ID → xbridge call_id for cleanup.
    xbridge_call_id: Option<String>,
}

type DialogMap = Arc<RwLock<HashMap<String, ActiveDialog>>>;

/// Run the SIP trunk host server.
///
/// Listens on the configured address, authenticates incoming SIP traffic,
/// and forwards authenticated INVITEs into xbridge's call flow via AppState.
pub async fn run(
    config: ServerConfig,
    state: AppState,
) -> Result<(), Box<dyn std::error::Error>> {
    let socket = Arc::new(UdpSocket::bind(&config.listen).await?);
    info!("trunk host listening on {}", config.listen);

    let dialogs: DialogMap = Arc::new(RwLock::new(HashMap::new()));

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
                handle_invite(&config, &socket, addr, msg, &state, &dialogs).await;
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

async fn handle_invite(
    config: &ServerConfig,
    socket: &Arc<UdpSocket>,
    addr: SocketAddr,
    msg: SipMessage,
    state: &AppState,
    dialogs: &DialogMap,
) {
    let source_ip = addr.ip();

    match auth::authenticate(config, &msg, source_ip) {
        AuthResult::Authenticated(peer_name) => {
            info!("authenticated INVITE from peer '{peer_name}' at {addr}");
            state.metrics.inc_trunk_calls_inbound();

            send_response(socket, addr, &msg, 100, "Trying").await;

            let to_tag = crate::trunk::util::generate_tag();
            let responder = TrunkResponder {
                socket: socket.clone(),
                remote_addr: addr,
                via: msg.header("Via").to_string(),
                from: msg.header("From").to_string(),
                to: msg.header("To").to_string(),
                call_id: msg.call_id().to_string(),
                cseq: msg.header("CSeq").to_string(),
                to_tag,
            };

            let sip_call_id = msg.call_id().to_string();
            dialogs.write().await.insert(
                sip_call_id.clone(),
                ActiveDialog { xbridge_call_id: None },
            );

            let call = IncomingTrunkCall {
                peer: peer_name,
                from: msg.from_user().to_string(),
                to: msg.to_user().to_string(),
                sdp: msg.body.clone(),
                responder,
            };

            let state = state.clone();
            let dialogs = dialogs.clone();
            let sip_cid = sip_call_id.clone();
            tokio::spawn(async move {
                handle_trunk_incoming(call, state, dialogs, sip_cid).await;
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

/// Handle an authenticated incoming trunk call through xbridge's webhook/REST flow.
async fn handle_trunk_incoming(
    call: IncomingTrunkCall,
    state: AppState,
    dialogs: DialogMap,
    sip_call_id: String,
) {
    let call_id = format!("trunk-{}", uuid_v4());

    info!(
        "trunk incoming call {call_id} from peer '{}': {} → {}",
        call.peer, call.from, call.to
    );

    let hook = crate::api::IncomingCallWebhook {
        call_id: call_id.clone(),
        from: call.from.clone(),
        to: call.to.clone(),
        direction: crate::call::CallDirection::Inbound,
        peer: Some(call.peer.clone()),
    };

    let response = match state.webhook.send_incoming(&hook).await {
        Ok(resp) => resp,
        Err(e) => {
            error!("incoming webhook failed for trunk call {call_id}: {e}");
            let _ = call.responder.respond(503, "Service Unavailable", &[]).await;
            dialogs.write().await.remove(&sip_call_id);
            return;
        }
    };

    match response.action {
        crate::api::IncomingCallAction::Accept => {
            if let Err(e) = call.responder.respond(200, "OK", &[]).await {
                error!("failed to send 200 OK for trunk call {call_id}: {e}");
                dialogs.write().await.remove(&sip_call_id);
                return;
            }

            // Track the xbridge call_id in the dialog for BYE cleanup.
            if let Some(dialog) = dialogs.write().await.get_mut(&sip_call_id) {
                dialog.xbridge_call_id = Some(call_id.clone());
            }

            let info = crate::call::CallInfo {
                call_id: call_id.clone(),
                from: call.from,
                to: call.to,
                direction: crate::call::CallDirection::Inbound,
                status: crate::call::CallStatus::InProgress,
                peer: Some(call.peer),
            };

            state.metrics.inc_calls_total();
            state.metrics.inc_calls_inbound();

            state.calls.write().await.insert(call_id.clone(), info);

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
            let _ = call.responder.respond(code, reason, &[]).await;
            dialogs.write().await.remove(&sip_call_id);
        }
    }
}

async fn handle_bye(
    socket: &UdpSocket,
    addr: SocketAddr,
    msg: &SipMessage,
    state: &AppState,
    dialogs: &DialogMap,
) {
    let sip_call_id = msg.call_id().to_string();
    debug!("BYE from {addr} for Call-ID={sip_call_id}");

    // Remove dialog and clean up xbridge state.
    if let Some(dialog) = dialogs.write().await.remove(&sip_call_id) {
        if let Some(call_id) = dialog.xbridge_call_id {
            state.calls.write().await.remove(&call_id);
            state.xphone_calls.write().await.remove(&call_id);
            if let Ok(mut senders) = state.ws_senders.write() {
                senders.remove(&call_id);
            }
            state
                .webhook
                .send_event(&crate::webhook::WebhookEvent::Ended {
                    call_id,
                    reason: "normal".to_string(),
                    duration: 0,
                })
                .await;
        }
    }

    send_response(socket, addr, msg, 200, "OK").await;
}

async fn handle_cancel(
    socket: &UdpSocket,
    addr: SocketAddr,
    msg: &SipMessage,
    dialogs: &DialogMap,
) {
    let sip_call_id = msg.call_id().to_string();
    debug!("CANCEL from {addr} for Call-ID={sip_call_id}");

    let existed = dialogs.write().await.remove(&sip_call_id).is_some();
    send_response(socket, addr, msg, 200, "OK").await;

    if existed {
        let mut resp = SipMessage::new_response(487, "Request Terminated");
        copy_dialog_headers(msg, &mut resp);
        let (seq, _) = msg.cseq();
        resp.set_header("CSeq", &format!("{seq} INVITE"));
        let data = resp.to_bytes();
        let _ = socket.send_to(&data, addr).await;
    }
}

async fn send_response(socket: &UdpSocket, addr: SocketAddr, req: &SipMessage, code: u16, reason: &str) {
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
    use crate::trunk::util::generate_tag;

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

    #[tokio::test]
    async fn responder_builds_valid_response() {
        let socket = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let receiver = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let recv_addr = receiver.local_addr().unwrap();

        let responder = TrunkResponder {
            socket: socket.clone(),
            remote_addr: recv_addr,
            via: "SIP/2.0/UDP 10.0.0.1:5060;branch=z9hG4bK111".into(),
            from: "<sip:1001@pbx.local>;tag=from1".into(),
            to: "<sip:1002@xbridge:5080>".into(),
            call_id: "test@host".into(),
            cseq: "1 INVITE".into(),
            to_tag: "stable123".into(),
        };

        responder.respond(200, "OK", b"v=0\r\n").await.unwrap();

        let mut buf = vec![0u8; 4096];
        let (len, _) = receiver.recv_from(&mut buf).await.unwrap();
        let msg = sip_msg::parse(&buf[..len]).unwrap();

        assert!(msg.is_response());
        assert_eq!(msg.status_code, 200);
        assert_eq!(msg.header("Call-ID"), "test@host");
        assert_eq!(msg.header("Content-Type"), "application/sdp");
        assert!(msg.header("To").contains("tag=stable123"));
        assert_eq!(String::from_utf8_lossy(&msg.body), "v=0\r\n");
    }

    #[tokio::test]
    async fn responder_stable_to_tag() {
        let socket = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let receiver = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let recv_addr = receiver.local_addr().unwrap();

        let responder = TrunkResponder {
            socket: socket.clone(),
            remote_addr: recv_addr,
            via: "SIP/2.0/UDP 10.0.0.1:5060;branch=z9hG4bK111".into(),
            from: "<sip:1001@pbx.local>;tag=from1".into(),
            to: "<sip:1002@xbridge:5080>".into(),
            call_id: "test@host".into(),
            cseq: "1 INVITE".into(),
            to_tag: "fixedtag".into(),
        };

        // Send two responses — to-tag should be identical.
        responder.respond(180, "Ringing", &[]).await.unwrap();
        responder.respond(200, "OK", &[]).await.unwrap();

        let mut buf = vec![0u8; 4096];
        let (len, _) = receiver.recv_from(&mut buf).await.unwrap();
        let msg1 = sip_msg::parse(&buf[..len]).unwrap();

        let (len, _) = receiver.recv_from(&mut buf).await.unwrap();
        let msg2 = sip_msg::parse(&buf[..len]).unwrap();

        assert!(msg1.header("To").contains("tag=fixedtag"));
        assert!(msg2.header("To").contains("tag=fixedtag"));
    }

    #[tokio::test]
    async fn responder_builds_valid_bye() {
        let socket = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let receiver = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let recv_addr = receiver.local_addr().unwrap();

        let responder = TrunkResponder {
            socket: socket.clone(),
            remote_addr: recv_addr,
            via: "SIP/2.0/UDP 10.0.0.1:5060;branch=z9hG4bK111".into(),
            from: "<sip:1001@pbx.local>;tag=from1".into(),
            to: "<sip:1002@xbridge:5080>;tag=to1".into(),
            call_id: "test@host".into(),
            cseq: "1 INVITE".into(),
            to_tag: generate_tag(),
        };

        responder
            .send_request("BYE", "sip:1001@10.0.0.1:5060")
            .await
            .unwrap();

        let mut buf = vec![0u8; 4096];
        let (len, _) = receiver.recv_from(&mut buf).await.unwrap();
        let msg = sip_msg::parse(&buf[..len]).unwrap();

        assert!(!msg.is_response());
        assert_eq!(msg.method, "BYE");
        assert_eq!(msg.header("Call-ID"), "test@host");
        let (seq, method) = msg.cseq();
        assert_eq!(seq, 2);
        assert_eq!(method, "BYE");
    }
}

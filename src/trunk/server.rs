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
use crate::trunk::sip_msg::{self, SipMessage};

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
    /// SIP Call-ID.
    pub sip_call_id: String,
    /// Source address of the peer.
    pub source_addr: SocketAddr,
    /// Handle to send SIP responses back.
    pub responder: TrunkResponder,
}

/// Handle for sending SIP responses back to a peer on a specific dialog.
#[derive(Debug, Clone)]
pub struct TrunkResponder {
    socket: Arc<UdpSocket>,
    remote_addr: SocketAddr,
    /// SIP headers needed to build responses (Via, From, To, Call-ID, CSeq).
    pub(crate) via: String,
    pub(crate) from: String,
    pub(crate) to: String,
    pub(crate) call_id: String,
    pub(crate) cseq: String,
}

impl TrunkResponder {
    /// Send a SIP response to the peer.
    pub async fn respond(&self, code: u16, reason: &str, body: &[u8]) -> std::io::Result<()> {
        let mut msg = SipMessage::new_response(code, reason);
        msg.add_header("Via", &self.via);
        msg.set_header("From", &self.from);
        // Add to-tag for non-100 responses.
        let to = if code > 100 && !self.to.contains("tag=") {
            format!("{};tag={}", self.to, generate_tag())
        } else {
            self.to.clone()
        };
        msg.set_header("To", &to);
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
}

/// Active dialog state for in-progress calls.
struct ActiveDialog {
    _responder: TrunkResponder,
}

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

    // Active dialogs keyed by SIP Call-ID.
    let dialogs: Arc<RwLock<HashMap<String, ActiveDialog>>> =
        Arc::new(RwLock::new(HashMap::new()));

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

        // Skip responses — we're a UAS.
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
                    &config,
                    &socket,
                    addr,
                    msg,
                    &state,
                    &dialogs,
                )
                .await;
            }
            "ACK" => {
                debug!("ACK received for Call-ID={}", msg.call_id());
            }
            "BYE" => {
                handle_bye(&socket, addr, &msg, &dialogs).await;
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
    dialogs: &Arc<RwLock<HashMap<String, ActiveDialog>>>,
) {
    let source_ip = addr.ip();

    match auth::authenticate(config, &msg, source_ip) {
        AuthResult::Authenticated(peer_name) => {
            info!("authenticated INVITE from peer '{peer_name}' at {addr}");
            state.metrics.inc_trunk_calls_inbound();

            // Send 100 Trying.
            send_response_via(socket, addr, &msg, 100, "Trying").await;

            let responder = TrunkResponder {
                socket: socket.clone(),
                remote_addr: addr,
                via: msg.header("Via").to_string(),
                from: msg.header("From").to_string(),
                to: msg.header("To").to_string(),
                call_id: msg.call_id().to_string(),
                cseq: msg.header("CSeq").to_string(),
            };

            let sip_call_id = msg.call_id().to_string();
            dialogs.write().await.insert(
                sip_call_id.clone(),
                ActiveDialog {
                    _responder: responder.clone(),
                },
            );

            let call = IncomingTrunkCall {
                peer: peer_name,
                from: msg.from_user().to_string(),
                to: msg.to_user().to_string(),
                sdp: msg.body.clone(),
                sip_call_id,
                source_addr: addr,
                responder,
            };

            // Handle the incoming call in a separate task so we don't block the listener.
            let state = state.clone();
            tokio::spawn(async move {
                handle_trunk_incoming(call, state).await;
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
            send_response_via(socket, addr, &msg, 403, "Forbidden").await;
        }
    }
}

/// Handle an authenticated incoming trunk call through xbridge's webhook/REST flow.
async fn handle_trunk_incoming(call: IncomingTrunkCall, state: AppState) {
    let call_id = format!("trunk-{}", uuid_v4());

    info!(
        "trunk incoming call {call_id} from peer '{}': {} → {}",
        call.peer, call.from, call.to
    );

    // Dispatch incoming webhook.
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
            return;
        }
    };

    match response.action {
        crate::api::IncomingCallAction::Accept => {
            // Send 200 OK (SDP answer would go here once xphone::Call is wired).
            // For now, accept with empty body — full media integration is next step.
            if let Err(e) = call.responder.respond(200, "OK", &[]).await {
                error!("failed to send 200 OK for trunk call {call_id}: {e}");
                return;
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
            let code = match reason {
                "busy" => 486,
                "declined" => 603,
                _ => 486,
            };
            info!("rejecting trunk call {call_id}: {reason}");
            let _ = call.responder.respond(code, reason, &[]).await;
        }
    }
}

async fn handle_bye(
    socket: &UdpSocket,
    addr: SocketAddr,
    msg: &SipMessage,
    dialogs: &Arc<RwLock<HashMap<String, ActiveDialog>>>,
) {
    let call_id = msg.call_id().to_string();
    debug!("BYE from {addr} for Call-ID={call_id}");
    dialogs.write().await.remove(&call_id);
    send_response(socket, addr, msg, 200, "OK").await;
}

async fn handle_cancel(
    socket: &UdpSocket,
    addr: SocketAddr,
    msg: &SipMessage,
    dialogs: &Arc<RwLock<HashMap<String, ActiveDialog>>>,
) {
    let call_id = msg.call_id().to_string();
    debug!("CANCEL from {addr} for Call-ID={call_id}");

    let existed = dialogs.write().await.remove(&call_id).is_some();
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

async fn send_response_via(socket: &Arc<UdpSocket>, addr: SocketAddr, req: &SipMessage, code: u16, reason: &str) {
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
    let to = req.header("To");
    if resp.status_code > 100 && !to.contains("tag=") {
        resp.set_header("To", &format!("{to};tag={}", generate_tag()));
    } else {
        resp.set_header("To", to);
    }
    resp.set_header("Call-ID", req.header("Call-ID"));
    resp.set_header("CSeq", req.header("CSeq"));
}

fn generate_tag() -> String {
    use rand::Rng;
    let mut rng = rand::rng();
    let bytes: [u8; 8] = rng.random();
    hex_encode(&bytes)
}

fn generate_branch() -> String {
    use rand::Rng;
    let mut rng = rand::rng();
    let bytes: [u8; 12] = rng.random();
    format!("z9hG4bK{}", hex_encode(&bytes))
}

fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{:02x}", b);
    }
    s
}

fn uuid_v4() -> String {
    use rand::Rng;
    let mut rng = rand::rng();
    let bytes: [u8; 16] = rng.random();
    format!(
        "{:08x}-{:04x}-4{:03x}-{:04x}-{:012x}",
        u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
        u16::from_be_bytes([bytes[4], bytes[5]]),
        u16::from_be_bytes([bytes[6], bytes[7]]) & 0x0FFF,
        (u16::from_be_bytes([bytes[8], bytes[9]]) & 0x3FFF) | 0x8000,
        u64::from_be_bytes([0, 0, bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15]]),
    )
}

fn parse_cseq(cseq: &str) -> (u32, &str) {
    let val = cseq.trim();
    if let Some(space) = val.find(' ') {
        if let Ok(n) = val[..space].parse() {
            return (n, &val[space + 1..]);
        }
    }
    (0, "")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_tag_is_16_hex_chars() {
        let tag = generate_tag();
        assert_eq!(tag.len(), 16);
        assert!(tag.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn generate_branch_has_magic_cookie() {
        let branch = generate_branch();
        assert!(branch.starts_with("z9hG4bK"));
    }

    #[test]
    fn uuid_v4_format() {
        let id = uuid_v4();
        assert_eq!(id.len(), 36);
        assert_eq!(id.chars().filter(|c| *c == '-').count(), 4);
        // Version nibble should be '4'.
        assert_eq!(id.chars().nth(14), Some('4'));
    }

    #[test]
    fn parse_cseq_valid() {
        assert_eq!(parse_cseq("1 INVITE"), (1, "INVITE"));
        assert_eq!(parse_cseq("42 BYE"), (42, "BYE"));
    }

    #[test]
    fn parse_cseq_invalid() {
        assert_eq!(parse_cseq(""), (0, ""));
        assert_eq!(parse_cseq("bad"), (0, ""));
    }

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
        };

        responder.respond(200, "OK", b"v=0\r\n").await.unwrap();

        let mut buf = vec![0u8; 4096];
        let (len, _) = receiver.recv_from(&mut buf).await.unwrap();
        let msg = sip_msg::parse(&buf[..len]).unwrap();

        assert!(msg.is_response());
        assert_eq!(msg.status_code, 200);
        assert_eq!(msg.header("Call-ID"), "test@host");
        assert_eq!(msg.header("Content-Type"), "application/sdp");
        assert!(msg.header("To").contains("tag="));
        assert_eq!(String::from_utf8_lossy(&msg.body), "v=0\r\n");
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

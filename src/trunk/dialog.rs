//! `xphone::Dialog` implementation backed by xbridge's trunk SIP transport.
//!
//! Bridges xphone's synchronous Dialog trait to the trunk server's async UDP
//! socket by queuing outgoing SIP messages through an unbounded channel.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;

use crate::trunk::sip_msg::{self, SipMessage};
use crate::trunk::util::generate_branch;

/// An outgoing SIP datagram to be sent by the server's send task.
pub(crate) struct SipOutgoing {
    pub data: Vec<u8>,
    pub addr: SocketAddr,
}

/// xphone::Dialog backed by xbridge's trunk SIP transport.
///
/// When xphone's `Call` calls `respond()`, `send_bye()`, etc., this impl builds
/// the SIP message and enqueues it for async delivery by the server's send task.
pub(crate) struct TrunkDialog {
    /// Channel for outgoing SIP datagrams.
    tx: mpsc::UnboundedSender<SipOutgoing>,
    /// Remote peer address.
    remote_addr: SocketAddr,
    /// Local server listen address (for Via headers in outgoing requests).
    local_addr: SocketAddr,
    /// SIP Call-ID for this dialog.
    sip_call_id: String,
    /// Our local to-tag (stable per dialog, RFC 3261).
    local_tag: String,
    /// Original INVITE headers (preserved for building responses/requests).
    invite_via: String,
    invite_from: String,
    invite_to: String,
    invite_cseq_num: u32,
    /// Request-URI from the INVITE (used for BYE Contact routing).
    contact_uri: String,
    /// Headers from the INVITE (for `header()`/`headers()` methods).
    invite_headers: HashMap<String, Vec<String>>,
    /// CSeq counter for outgoing requests (BYE, re-INVITE, etc.).
    cseq_counter: Mutex<u32>,
    /// on_notify callback storage.
    on_notify_fn: Mutex<Option<Arc<dyn Fn(u16) + Send + Sync>>>,
}

impl TrunkDialog {
    /// Create a new TrunkDialog from an incoming INVITE.
    pub(crate) fn new(
        tx: mpsc::UnboundedSender<SipOutgoing>,
        local_addr: SocketAddr,
        remote_addr: SocketAddr,
        invite: &SipMessage,
        local_tag: String,
    ) -> Self {
        // Build headers map from the INVITE.
        let mut headers = HashMap::new();
        for name in &["From", "To", "Call-ID", "Via", "Contact", "CSeq"] {
            let vals: Vec<String> = invite
                .header_values(name)
                .into_iter()
                .map(|v| v.to_string())
                .collect();
            if !vals.is_empty() {
                headers.insert(name.to_string(), vals);
            }
        }

        let (cseq_num, _) = sip_msg::parse_cseq(invite.header("CSeq"));

        // Derive contact URI from the INVITE's Contact header or From header.
        let contact = invite.header("Contact");
        let contact_uri = if !contact.is_empty() {
            extract_uri(contact).to_string()
        } else {
            invite.request_uri.clone()
        };

        Self {
            tx,
            remote_addr,
            local_addr,
            sip_call_id: invite.call_id().to_string(),
            local_tag,
            invite_via: invite.header("Via").to_string(),
            invite_from: invite.header("From").to_string(),
            invite_to: invite.header("To").to_string(),
            invite_cseq_num: cseq_num,
            contact_uri,
            invite_headers: headers,
            cseq_counter: Mutex::new(cseq_num),
            on_notify_fn: Mutex::new(None),
        }
    }

    /// Build and enqueue a SIP response.
    fn send_response(&self, code: u16, reason: &str, body: &[u8]) -> xphone::Result<()> {
        let mut resp = SipMessage::new_response(code, reason);
        resp.add_header("Via", &self.invite_via);
        resp.set_header("From", &self.invite_from);
        resp.set_header("To", &self.to_with_tag());
        resp.set_header("Call-ID", &self.sip_call_id);
        resp.set_header(
            "CSeq",
            &format!("{} INVITE", self.invite_cseq_num),
        );
        resp.set_header(
            "Contact",
            &format!("<sip:xbridge@{}>", self.local_addr),
        );
        if !body.is_empty() {
            resp.set_header("Content-Type", "application/sdp");
            resp.body = body.to_vec();
        }
        self.enqueue(resp)
    }

    /// Build and enqueue a SIP request (BYE, re-INVITE, REFER, INFO).
    fn send_sip_request(&self, method: &str, body: &[u8], extra_headers: &[(&str, &str)]) -> xphone::Result<()> {
        let branch = generate_branch();
        let cseq = self.next_cseq();

        let mut req = SipMessage::new_request(method, &self.contact_uri);
        req.set_header(
            "Via",
            &format!("SIP/2.0/UDP {};branch={}", self.local_addr, branch),
        );
        // In a UAS dialog, From/To are swapped for outgoing requests.
        req.set_header("From", &self.to_with_tag());
        req.set_header("To", &self.invite_from);
        req.set_header("Call-ID", &self.sip_call_id);
        req.set_header("CSeq", &format!("{} {}", cseq, method));
        req.set_header(
            "Contact",
            &format!("<sip:xbridge@{}>", self.local_addr),
        );
        for (name, value) in extra_headers {
            req.set_header(name, value);
        }
        if !body.is_empty() {
            req.body = body.to_vec();
        }
        self.enqueue(req)
    }

    fn enqueue(&self, msg: SipMessage) -> xphone::Result<()> {
        let data = msg.to_bytes();
        self.tx
            .send(SipOutgoing {
                data,
                addr: self.remote_addr,
            })
            .map_err(|_| xphone::Error::Other("trunk send channel closed".into()))
    }

    fn to_with_tag(&self) -> String {
        if self.invite_to.contains("tag=") {
            self.invite_to.clone()
        } else {
            format!("{};tag={}", self.invite_to, self.local_tag)
        }
    }

    fn next_cseq(&self) -> u32 {
        let mut counter = self.cseq_counter.lock().unwrap();
        *counter += 1;
        *counter
    }
}

impl xphone::dialog::Dialog for TrunkDialog {
    fn respond(&self, code: u16, reason: &str, body: &[u8]) -> xphone::Result<()> {
        self.send_response(code, reason, body)
    }

    fn send_bye(&self) -> xphone::Result<()> {
        self.send_sip_request("BYE", &[], &[])
    }

    fn send_cancel(&self) -> xphone::Result<()> {
        // UAS cannot send CANCEL (only UAC can).
        Err(xphone::Error::InvalidState)
    }

    fn send_reinvite(&self, sdp: &[u8]) -> xphone::Result<()> {
        self.send_sip_request(
            "INVITE",
            sdp,
            &[("Content-Type", "application/sdp")],
        )
    }

    fn send_refer(&self, target: &str) -> xphone::Result<()> {
        self.send_sip_request("REFER", &[], &[("Refer-To", target)])
    }

    fn send_info_dtmf(&self, digit: &str, duration_ms: u32) -> xphone::Result<()> {
        let body = format!("Signal={}\r\nDuration={}\r\n", digit, duration_ms);
        self.send_sip_request(
            "INFO",
            body.as_bytes(),
            &[("Content-Type", "application/dtmf-relay")],
        )
    }

    fn on_notify(&self, f: Box<dyn Fn(u16) + Send + Sync>) {
        *self.on_notify_fn.lock().unwrap() = Some(Arc::from(f));
    }

    fn fire_notify(&self, code: u16) {
        let cb = self.on_notify_fn.lock().unwrap().clone();
        if let Some(f) = cb {
            f(code);
        }
    }

    fn call_id(&self) -> String {
        self.sip_call_id.clone()
    }

    fn header(&self, name: &str) -> Vec<String> {
        let name_lower = name.to_lowercase();
        for (k, v) in &self.invite_headers {
            if k.to_lowercase() == name_lower {
                return v.clone();
            }
        }
        Vec::new()
    }

    fn headers(&self) -> HashMap<String, Vec<String>> {
        self.invite_headers.clone()
    }
}

/// Extract a bare SIP URI from a header value.
/// `<sip:1001@10.0.0.1:5060>` → `sip:1001@10.0.0.1:5060`
fn extract_uri(header_val: &str) -> &str {
    if let Some(start) = header_val.find('<') {
        if let Some(end) = header_val[start..].find('>') {
            return &header_val[start + 1..start + end];
        }
    }
    header_val.trim()
}

#[cfg(test)]
mod tests {
    use super::*;
    use xphone::dialog::Dialog;

    fn sample_invite() -> SipMessage {
        let mut msg = SipMessage::new_request("INVITE", "sip:1002@xbridge:5080");
        msg.add_header("Via", "SIP/2.0/UDP 10.0.0.1:5060;branch=z9hG4bK111");
        msg.set_header("From", "<sip:1001@pbx.local>;tag=from1");
        msg.set_header("To", "<sip:1002@xbridge:5080>");
        msg.set_header("Call-ID", "testcall@host");
        msg.set_header("CSeq", "1 INVITE");
        msg.set_header("Contact", "<sip:1001@10.0.0.1:5060>");
        msg
    }

    fn make_dialog() -> (TrunkDialog, mpsc::UnboundedReceiver<SipOutgoing>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let invite = sample_invite();
        let dialog = TrunkDialog::new(
            tx,
            "127.0.0.1:5080".parse().unwrap(),
            "10.0.0.1:5060".parse().unwrap(),
            &invite,
            "localtag123".into(),
        );
        (dialog, rx)
    }

    #[test]
    fn respond_builds_valid_sip_response() {
        let (dialog, mut rx) = make_dialog();
        dialog.respond(200, "OK", b"v=0\r\n").unwrap();

        let outgoing = rx.try_recv().unwrap();
        let msg = sip_msg::parse(&outgoing.data).unwrap();

        assert!(msg.is_response());
        assert_eq!(msg.status_code, 200);
        assert_eq!(msg.header("Call-ID"), "testcall@host");
        assert!(msg.header("To").contains("tag=localtag123"));
        assert_eq!(msg.header("Content-Type"), "application/sdp");
        assert_eq!(String::from_utf8_lossy(&msg.body), "v=0\r\n");
        assert_eq!(outgoing.addr, "10.0.0.1:5060".parse::<SocketAddr>().unwrap());
    }

    #[test]
    fn respond_without_body_has_no_content_type() {
        let (dialog, mut rx) = make_dialog();
        dialog.respond(486, "Busy Here", &[]).unwrap();

        let outgoing = rx.try_recv().unwrap();
        let msg = sip_msg::parse(&outgoing.data).unwrap();

        assert_eq!(msg.status_code, 486);
        assert_eq!(msg.header("Content-Type"), "");
    }

    #[test]
    fn to_tag_is_stable_across_responses() {
        let (dialog, mut rx) = make_dialog();
        dialog.respond(180, "Ringing", &[]).unwrap();
        dialog.respond(200, "OK", &[]).unwrap();

        let msg1 = sip_msg::parse(&rx.try_recv().unwrap().data).unwrap();
        let msg2 = sip_msg::parse(&rx.try_recv().unwrap().data).unwrap();

        assert!(msg1.header("To").contains("tag=localtag123"));
        assert!(msg2.header("To").contains("tag=localtag123"));
    }

    #[test]
    fn send_bye_builds_valid_request() {
        let (dialog, mut rx) = make_dialog();
        dialog.send_bye().unwrap();

        let outgoing = rx.try_recv().unwrap();
        let msg = sip_msg::parse(&outgoing.data).unwrap();

        assert!(!msg.is_response());
        assert_eq!(msg.method, "BYE");
        assert_eq!(msg.request_uri, "sip:1001@10.0.0.1:5060");
        assert_eq!(msg.header("Call-ID"), "testcall@host");
        // In UAS dialog, From/To are swapped for outgoing requests.
        assert!(msg.header("From").contains("tag=localtag123"));
        assert!(msg.header("To").contains("from1"));
        let (seq, method) = msg.cseq();
        assert_eq!(seq, 2);
        assert_eq!(method, "BYE");
    }

    #[test]
    fn send_cancel_returns_invalid_state() {
        let (dialog, _rx) = make_dialog();
        assert!(dialog.send_cancel().is_err());
    }

    #[test]
    fn send_reinvite_includes_sdp() {
        let (dialog, mut rx) = make_dialog();
        let sdp = b"v=0\r\no=- 0 0 IN IP4 10.0.0.2\r\n";
        dialog.send_reinvite(sdp).unwrap();

        let msg = sip_msg::parse(&rx.try_recv().unwrap().data).unwrap();

        assert_eq!(msg.method, "INVITE");
        assert_eq!(msg.header("Content-Type"), "application/sdp");
        assert_eq!(msg.body, sdp);
    }

    #[test]
    fn send_refer_includes_refer_to() {
        let (dialog, mut rx) = make_dialog();
        dialog.send_refer("sip:1003@pbx.local").unwrap();

        let msg = sip_msg::parse(&rx.try_recv().unwrap().data).unwrap();

        assert_eq!(msg.method, "REFER");
        assert_eq!(msg.header("Refer-To"), "sip:1003@pbx.local");
    }

    #[test]
    fn send_info_dtmf_format() {
        let (dialog, mut rx) = make_dialog();
        dialog.send_info_dtmf("5", 250).unwrap();

        let msg = sip_msg::parse(&rx.try_recv().unwrap().data).unwrap();

        assert_eq!(msg.method, "INFO");
        assert_eq!(msg.header("Content-Type"), "application/dtmf-relay");
        let body = String::from_utf8_lossy(&msg.body);
        assert!(body.contains("Signal=5"));
        assert!(body.contains("Duration=250"));
    }

    #[test]
    fn cseq_increments() {
        let (dialog, mut rx) = make_dialog();
        dialog.send_bye().unwrap();
        // BYE would end the dialog, but for testing CSeq we just send again.
        let _ = dialog.send_sip_request("INFO", &[], &[]);

        let msg1 = sip_msg::parse(&rx.try_recv().unwrap().data).unwrap();
        let msg2 = sip_msg::parse(&rx.try_recv().unwrap().data).unwrap();

        let (seq1, _) = msg1.cseq();
        let (seq2, _) = msg2.cseq();
        assert_eq!(seq2, seq1 + 1);
    }

    #[test]
    fn call_id_returns_sip_call_id() {
        let (dialog, _rx) = make_dialog();
        assert_eq!(dialog.call_id(), "testcall@host");
    }

    #[test]
    fn header_returns_invite_headers() {
        let (dialog, _rx) = make_dialog();
        let from = dialog.header("From");
        assert_eq!(from.len(), 1);
        assert!(from[0].contains("1001@pbx.local"));

        let call_id = dialog.header("Call-ID");
        assert_eq!(call_id, vec!["testcall@host"]);
    }

    #[test]
    fn header_case_insensitive() {
        let (dialog, _rx) = make_dialog();
        assert_eq!(dialog.header("call-id"), dialog.header("Call-ID"));
    }

    #[test]
    fn header_missing_returns_empty() {
        let (dialog, _rx) = make_dialog();
        assert!(dialog.header("X-Nonexistent").is_empty());
    }

    #[test]
    fn headers_returns_all() {
        let (dialog, _rx) = make_dialog();
        let all = dialog.headers();
        assert!(all.contains_key("From"));
        assert!(all.contains_key("To"));
        assert!(all.contains_key("Call-ID"));
    }

    #[test]
    fn on_notify_fires_callback() {
        let (dialog, _rx) = make_dialog();
        let received = Arc::new(Mutex::new(None));
        let r = received.clone();
        dialog.on_notify(Box::new(move |code| {
            *r.lock().unwrap() = Some(code);
        }));
        dialog.fire_notify(200);
        assert_eq!(*received.lock().unwrap(), Some(200));
    }

    #[test]
    fn extract_uri_angle_brackets() {
        assert_eq!(
            extract_uri("<sip:1001@10.0.0.1:5060>"),
            "sip:1001@10.0.0.1:5060"
        );
    }

    #[test]
    fn extract_uri_bare() {
        assert_eq!(extract_uri("sip:1001@10.0.0.1"), "sip:1001@10.0.0.1");
    }

    #[test]
    fn extract_uri_with_params() {
        assert_eq!(
            extract_uri("<sip:1001@10.0.0.1:5060>;transport=udp"),
            "sip:1001@10.0.0.1:5060"
        );
    }
}

//! Minimal SIP message parser and builder for the trunk host server.
//!
//! Handles only the subset needed for a UAS: INVITE, ACK, BYE, CANCEL, OPTIONS.
//! Intentionally independent from xphone's internal SIP parser.

use std::fmt::Write;

/// A parsed SIP message (request or response).
#[derive(Debug, Clone)]
pub struct SipMessage {
    /// Request method (INVITE, BYE, etc.). Empty for responses.
    pub method: String,
    /// Request-URI. Empty for responses.
    pub request_uri: String,
    /// Status code. 0 for requests.
    pub status_code: u16,
    /// Reason phrase. Empty for requests.
    pub reason: String,
    /// Headers in order.
    headers: Vec<(String, String)>,
    /// Message body (SDP, etc.).
    pub body: Vec<u8>,
}

#[derive(Debug)]
pub enum ParseError {
    Empty,
    InvalidUtf8,
    MalformedStartLine,
    InvalidStatusCode,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => write!(f, "empty SIP message"),
            Self::InvalidUtf8 => write!(f, "invalid UTF-8 in SIP headers"),
            Self::MalformedStartLine => write!(f, "malformed SIP start line"),
            Self::InvalidStatusCode => write!(f, "invalid SIP status code"),
        }
    }
}

impl std::error::Error for ParseError {}

impl SipMessage {
    /// Create a new SIP request.
    pub fn new_request(method: &str, request_uri: &str) -> Self {
        Self {
            method: method.into(),
            request_uri: request_uri.into(),
            status_code: 0,
            reason: String::new(),
            headers: Vec::new(),
            body: Vec::new(),
        }
    }

    /// Create a new SIP response.
    pub fn new_response(status_code: u16, reason: &str) -> Self {
        Self {
            method: String::new(),
            request_uri: String::new(),
            status_code,
            reason: reason.into(),
            headers: Vec::new(),
            body: Vec::new(),
        }
    }

    pub fn is_response(&self) -> bool {
        self.status_code > 0
    }

    /// Get the first value for a header (case-insensitive). Returns empty string if missing.
    pub fn header(&self, name: &str) -> &str {
        for (n, v) in &self.headers {
            if n.eq_ignore_ascii_case(name) {
                return v;
            }
        }
        ""
    }

    /// Get all values for a header (case-insensitive).
    pub fn header_values(&self, name: &str) -> Vec<&str> {
        self.headers
            .iter()
            .filter(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
            .collect()
    }

    /// Set a header, replacing any existing values with the same name.
    pub fn set_header(&mut self, name: &str, value: &str) {
        let mut found = false;
        self.headers.retain_mut(|(n, v)| {
            if n.eq_ignore_ascii_case(name) {
                if !found {
                    *v = value.into();
                    found = true;
                    true
                } else {
                    false
                }
            } else {
                true
            }
        });
        if !found {
            self.headers.push((name.into(), value.into()));
        }
    }

    /// Append a header value (does not replace existing).
    pub fn add_header(&mut self, name: &str, value: &str) {
        self.headers.push((name.into(), value.into()));
    }

    /// Returns the branch parameter from the top Via header.
    pub fn via_branch(&self) -> &str {
        param_value(self.header("Via"), "branch")
    }

    /// Parses the CSeq header into (sequence number, method).
    pub fn cseq(&self) -> (u32, &str) {
        let val = self.header("CSeq").trim();
        if val.is_empty() {
            return (0, "");
        }
        if let Some(space) = val.find(' ') {
            if let Ok(n) = val[..space].parse() {
                return (n, &val[space + 1..]);
            }
        }
        (0, "")
    }

    /// Returns the Call-ID header value.
    pub fn call_id(&self) -> &str {
        self.header("Call-ID")
    }

    /// Returns the tag parameter from the From header.
    pub fn from_tag(&self) -> &str {
        param_value(self.header("From"), "tag")
    }

    /// Returns the tag parameter from the To header.
    pub fn to_tag(&self) -> &str {
        param_value(self.header("To"), "tag")
    }

    /// Extracts the SIP URI user part from the From header.
    /// E.g., `<sip:1001@pbx.local>;tag=abc` → `"1001"`
    pub fn from_user(&self) -> &str {
        extract_uri_user(self.header("From"))
    }

    /// Extracts the SIP URI user part from the To header or Request-URI.
    pub fn to_user(&self) -> &str {
        // Prefer Request-URI for requests (more reliable for routing).
        if !self.request_uri.is_empty() {
            let user = extract_uri_user(&self.request_uri);
            if !user.is_empty() {
                return user;
            }
        }
        extract_uri_user(self.header("To"))
    }

    /// Serialize to SIP wire format.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = String::new();

        if self.is_response() {
            let _ = write!(buf, "SIP/2.0 {} {}\r\n", self.status_code, self.reason);
        } else {
            let _ = write!(buf, "{} {} SIP/2.0\r\n", self.method, self.request_uri);
        }

        for (name, value) in &self.headers {
            if name.eq_ignore_ascii_case("content-length") {
                continue;
            }
            let _ = write!(buf, "{}: {}\r\n", name, value);
        }

        let _ = write!(buf, "Content-Length: {}\r\n", self.body.len());
        buf.push_str("\r\n");

        let mut bytes = buf.into_bytes();
        if !self.body.is_empty() {
            bytes.extend_from_slice(&self.body);
        }
        bytes
    }
}

/// Parse a raw SIP message from bytes.
pub fn parse(data: &[u8]) -> Result<SipMessage, ParseError> {
    if data.is_empty() {
        return Err(ParseError::Empty);
    }

    let head_end = data
        .windows(4)
        .position(|w| w == b"\r\n\r\n");

    let (head, body) = match head_end {
        Some(pos) => (&data[..pos], &data[pos + 4..]),
        None => (data, &[] as &[u8]),
    };

    let head_str = std::str::from_utf8(head).map_err(|_| ParseError::InvalidUtf8)?;
    let mut lines = head_str.split("\r\n");

    let start_line = lines.next().ok_or(ParseError::MalformedStartLine)?;
    if start_line.is_empty() {
        return Err(ParseError::MalformedStartLine);
    }

    let mut msg = SipMessage {
        method: String::new(),
        request_uri: String::new(),
        status_code: 0,
        reason: String::new(),
        headers: Vec::new(),
        body: Vec::new(),
    };

    if let Some(rest) = start_line.strip_prefix("SIP/2.0 ") {
        let space = rest.find(' ').ok_or(ParseError::MalformedStartLine)?;
        msg.status_code = rest[..space].parse().map_err(|_| ParseError::InvalidStatusCode)?;
        msg.reason = rest[space + 1..].into();
    } else {
        let parts: Vec<&str> = start_line.splitn(3, ' ').collect();
        if parts.len() < 3 || parts[2] != "SIP/2.0" {
            return Err(ParseError::MalformedStartLine);
        }
        msg.method = parts[0].into();
        msg.request_uri = parts[1].into();
    }

    for line in lines {
        if line.is_empty() {
            continue;
        }
        if let Some(colon) = line.find(':') {
            let name = &line[..colon];
            let value = line[colon + 1..].trim();
            msg.headers.push((name.into(), value.into()));
        }
    }

    if head_end.is_some() && !body.is_empty() {
        let cl_str = msg.header("Content-Length");
        if !cl_str.is_empty() {
            if let Ok(cl) = cl_str.parse::<usize>() {
                if cl > 0 && cl <= body.len() {
                    msg.body = body[..cl].to_vec();
                } else if cl > body.len() {
                    msg.body = body.to_vec();
                }
            } else {
                msg.body = body.to_vec();
            }
        } else {
            msg.body = body.to_vec();
        }
    }

    Ok(msg)
}

/// Extract a parameter value from a SIP header value.
/// E.g., `param_value("SIP/2.0/UDP 10.0.0.1;branch=z9hG4bK123", "branch")` → `"z9hG4bK123"`
fn param_value<'a>(header_val: &'a str, param: &str) -> &'a str {
    let search = format!("{}=", param);
    for part in header_val.split(';') {
        let trimmed = part.trim();
        if trimmed.len() >= search.len()
            && trimmed[..search.len()].eq_ignore_ascii_case(&search)
        {
            let val = &trimmed[search.len()..];
            let end = val.find([',', ' ', '\t', '>']);
            return match end {
                Some(e) => &val[..e],
                None => val,
            };
        }
    }
    ""
}

/// Extract user part from a SIP URI.
/// `<sip:1001@pbx.local>` → `"1001"`
/// `sip:1001@pbx.local` → `"1001"`
fn extract_uri_user(s: &str) -> &str {
    // Find "sip:" (possibly inside angle brackets)
    let start = match s.find("sip:") {
        Some(i) => i + 4,
        None => match s.find("sips:") {
            Some(i) => i + 5,
            None => return "",
        },
    };
    let rest = &s[start..];
    // User is before '@'
    match rest.find('@') {
        Some(at) => &rest[..at],
        None => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Parsing ──

    #[test]
    fn parse_invite_with_sdp() {
        let sdp = "v=0\r\no=- 0 0 IN IP4 10.0.0.1\r\ns=-\r\nc=IN IP4 10.0.0.1\r\nt=0 0\r\nm=audio 10000 RTP/AVP 0\r\n";
        let raw = format!(
            "INVITE sip:1002@pbx.local SIP/2.0\r\n\
             Via: SIP/2.0/UDP 10.0.0.1:5060;branch=z9hG4bKinv1\r\n\
             From: <sip:1001@pbx.local>;tag=from1\r\n\
             To: <sip:1002@pbx.local>\r\n\
             Call-ID: invite001@10.0.0.1\r\n\
             CSeq: 1 INVITE\r\n\
             Contact: <sip:1001@10.0.0.1:5060>\r\n\
             Content-Type: application/sdp\r\n\
             Content-Length: {}\r\n\
             \r\n\
             {}",
            sdp.len(),
            sdp
        );

        let msg = parse(raw.as_bytes()).unwrap();
        assert!(!msg.is_response());
        assert_eq!(msg.method, "INVITE");
        assert_eq!(msg.request_uri, "sip:1002@pbx.local");
        assert_eq!(msg.call_id(), "invite001@10.0.0.1");
        assert_eq!(msg.from_tag(), "from1");
        assert_eq!(msg.cseq(), (1, "INVITE"));
        assert_eq!(String::from_utf8_lossy(&msg.body), sdp);
    }

    #[test]
    fn parse_bye() {
        let raw = "BYE sip:1001@10.0.0.1:5060 SIP/2.0\r\n\
                   Via: SIP/2.0/UDP pbx.local:5060;branch=z9hG4bKbye1\r\n\
                   From: <sip:1002@pbx.local>;tag=from2\r\n\
                   To: <sip:1001@pbx.local>;tag=to2\r\n\
                   Call-ID: invite001@10.0.0.1\r\n\
                   CSeq: 2 BYE\r\n\
                   Content-Length: 0\r\n\
                   \r\n";

        let msg = parse(raw.as_bytes()).unwrap();
        assert_eq!(msg.method, "BYE");
        assert_eq!(msg.cseq(), (2, "BYE"));
    }

    #[test]
    fn parse_cancel() {
        let raw = "CANCEL sip:1002@pbx.local SIP/2.0\r\n\
                   Via: SIP/2.0/UDP 10.0.0.1:5060;branch=z9hG4bKinv1\r\n\
                   From: <sip:1001@pbx.local>;tag=from1\r\n\
                   To: <sip:1002@pbx.local>\r\n\
                   Call-ID: invite001@10.0.0.1\r\n\
                   CSeq: 1 CANCEL\r\n\
                   Content-Length: 0\r\n\
                   \r\n";

        let msg = parse(raw.as_bytes()).unwrap();
        assert_eq!(msg.method, "CANCEL");
    }

    #[test]
    fn parse_options() {
        let raw = "OPTIONS sip:xbridge@10.0.0.2:5080 SIP/2.0\r\n\
                   Via: SIP/2.0/UDP 10.0.0.1:5060;branch=z9hG4bKopt1\r\n\
                   From: <sip:pbx@10.0.0.1>;tag=opt1\r\n\
                   To: <sip:xbridge@10.0.0.2:5080>\r\n\
                   Call-ID: options001@10.0.0.1\r\n\
                   CSeq: 1 OPTIONS\r\n\
                   Content-Length: 0\r\n\
                   \r\n";

        let msg = parse(raw.as_bytes()).unwrap();
        assert_eq!(msg.method, "OPTIONS");
    }

    #[test]
    fn parse_ack() {
        let raw = "ACK sip:1002@10.0.0.2:5080 SIP/2.0\r\n\
                   Via: SIP/2.0/UDP 10.0.0.1:5060;branch=z9hG4bKack1\r\n\
                   From: <sip:1001@pbx.local>;tag=from1\r\n\
                   To: <sip:1002@pbx.local>;tag=to1\r\n\
                   Call-ID: invite001@10.0.0.1\r\n\
                   CSeq: 1 ACK\r\n\
                   Content-Length: 0\r\n\
                   \r\n";

        let msg = parse(raw.as_bytes()).unwrap();
        assert_eq!(msg.method, "ACK");
    }

    #[test]
    fn parse_response() {
        let raw = "SIP/2.0 200 OK\r\n\
                   Via: SIP/2.0/UDP 10.0.0.1:5060;branch=z9hG4bK123\r\n\
                   From: <sip:1001@pbx.local>;tag=abc\r\n\
                   To: <sip:1002@pbx.local>;tag=def\r\n\
                   Call-ID: test@host\r\n\
                   CSeq: 1 INVITE\r\n\
                   Content-Length: 0\r\n\
                   \r\n";

        let msg = parse(raw.as_bytes()).unwrap();
        assert!(msg.is_response());
        assert_eq!(msg.status_code, 200);
        assert_eq!(msg.reason, "OK");
    }

    #[test]
    fn parse_empty_fails() {
        assert!(parse(b"").is_err());
    }

    #[test]
    fn parse_garbage_fails() {
        assert!(parse(b"not a SIP message").is_err());
    }

    // ── Header access ──

    #[test]
    fn header_case_insensitive() {
        let raw = "SIP/2.0 200 OK\r\n\
                   call-id: lower@host\r\n\
                   CSeq: 1 REGISTER\r\n\
                   Content-Length: 0\r\n\
                   \r\n";

        let msg = parse(raw.as_bytes()).unwrap();
        assert_eq!(msg.header("Call-ID"), "lower@host");
        assert_eq!(msg.header("CALL-ID"), "lower@host");
        assert_eq!(msg.header("call-id"), "lower@host");
    }

    #[test]
    fn header_missing_returns_empty() {
        let raw = "SIP/2.0 200 OK\r\nCall-ID: x@y\r\nCSeq: 1 REGISTER\r\nContent-Length: 0\r\n\r\n";
        let msg = parse(raw.as_bytes()).unwrap();
        assert_eq!(msg.header("X-Nonexistent"), "");
    }

    #[test]
    fn multiple_via_headers() {
        let raw = "SIP/2.0 200 OK\r\n\
                   Via: SIP/2.0/UDP 10.0.0.1:5060;branch=z9hG4bK111\r\n\
                   Via: SIP/2.0/UDP 10.0.0.2:5060;branch=z9hG4bK222\r\n\
                   Call-ID: multi@host\r\n\
                   CSeq: 1 INVITE\r\n\
                   Content-Length: 0\r\n\
                   \r\n";

        let msg = parse(raw.as_bytes()).unwrap();
        let vias = msg.header_values("Via");
        assert_eq!(vias.len(), 2);
    }

    // ── Via branch / tags ──

    #[test]
    fn via_branch_extraction() {
        let raw = "SIP/2.0 200 OK\r\n\
                   Via: SIP/2.0/UDP 10.0.0.1:5060;branch=z9hG4bKmybranch;rport\r\n\
                   Call-ID: via@host\r\n\
                   CSeq: 1 REGISTER\r\n\
                   Content-Length: 0\r\n\
                   \r\n";

        let msg = parse(raw.as_bytes()).unwrap();
        assert_eq!(msg.via_branch(), "z9hG4bKmybranch");
    }

    #[test]
    fn from_to_tags() {
        let raw = "INVITE sip:1002@pbx.local SIP/2.0\r\n\
                   From: <sip:1001@pbx.local>;tag=fromtag123\r\n\
                   To: <sip:1002@pbx.local>;tag=totag456\r\n\
                   Call-ID: tag@host\r\n\
                   CSeq: 1 INVITE\r\n\
                   Content-Length: 0\r\n\
                   \r\n";

        let msg = parse(raw.as_bytes()).unwrap();
        assert_eq!(msg.from_tag(), "fromtag123");
        assert_eq!(msg.to_tag(), "totag456");
    }

    // ── User extraction ──

    #[test]
    fn from_user_extraction() {
        let raw = "INVITE sip:1002@pbx.local SIP/2.0\r\n\
                   From: <sip:1001@pbx.local>;tag=abc\r\n\
                   To: <sip:1002@pbx.local>\r\n\
                   Call-ID: user@host\r\n\
                   CSeq: 1 INVITE\r\n\
                   Content-Length: 0\r\n\
                   \r\n";

        let msg = parse(raw.as_bytes()).unwrap();
        assert_eq!(msg.from_user(), "1001");
        assert_eq!(msg.to_user(), "1002");
    }

    #[test]
    fn to_user_from_request_uri() {
        let raw = "INVITE sip:+15551234567@gateway.com SIP/2.0\r\n\
                   From: <sip:1001@pbx.local>;tag=abc\r\n\
                   To: <sip:+15551234567@gateway.com>\r\n\
                   Call-ID: uri@host\r\n\
                   CSeq: 1 INVITE\r\n\
                   Content-Length: 0\r\n\
                   \r\n";

        let msg = parse(raw.as_bytes()).unwrap();
        assert_eq!(msg.to_user(), "+15551234567");
    }

    // ── Building ──

    #[test]
    fn build_response_roundtrip() {
        let mut msg = SipMessage::new_response(200, "OK");
        msg.set_header("Via", "SIP/2.0/UDP 10.0.0.1:5060;branch=z9hG4bK111");
        msg.set_header("From", "<sip:1001@pbx.local>;tag=t1");
        msg.set_header("To", "<sip:1001@pbx.local>;tag=t2");
        msg.set_header("Call-ID", "build@host");
        msg.set_header("CSeq", "1 INVITE");

        let bytes = msg.to_bytes();
        let parsed = parse(&bytes).unwrap();
        assert!(parsed.is_response());
        assert_eq!(parsed.status_code, 200);
        assert_eq!(parsed.header("Call-ID"), "build@host");
        assert_eq!(parsed.header("Content-Length"), "0");
    }

    #[test]
    fn build_request_with_body() {
        let sdp = "v=0\r\no=- 0 0 IN IP4 10.0.0.1\r\ns=-\r\n";
        let mut msg = SipMessage::new_request("INVITE", "sip:1002@pbx.local");
        msg.set_header("Via", "SIP/2.0/UDP 10.0.0.1:5060;branch=z9hG4bK111");
        msg.set_header("From", "<sip:1001@pbx.local>;tag=f1");
        msg.set_header("To", "<sip:1002@pbx.local>");
        msg.set_header("Call-ID", "inv@host");
        msg.set_header("CSeq", "1 INVITE");
        msg.set_header("Content-Type", "application/sdp");
        msg.body = sdp.as_bytes().to_vec();

        let bytes = msg.to_bytes();
        let parsed = parse(&bytes).unwrap();
        assert_eq!(parsed.method, "INVITE");
        assert_eq!(String::from_utf8_lossy(&parsed.body), sdp);
        assert_eq!(parsed.header("Content-Length"), sdp.len().to_string());
    }

    #[test]
    fn set_header_replaces() {
        let mut msg = SipMessage::new_request("REGISTER", "sip:pbx.local");
        msg.set_header("Call-ID", "first");
        msg.set_header("Call-ID", "second");
        assert_eq!(msg.header("Call-ID"), "second");
        assert_eq!(msg.header_values("Call-ID").len(), 1);
    }

    #[test]
    fn add_header_appends() {
        let mut msg = SipMessage::new_response(200, "OK");
        msg.add_header("Via", "SIP/2.0/UDP 10.0.0.1;branch=z9hG4bK111");
        msg.add_header("Via", "SIP/2.0/UDP 10.0.0.2;branch=z9hG4bK222");
        assert_eq!(msg.header_values("Via").len(), 2);
    }

    // ── extract_uri_user ──

    #[test]
    fn extract_user_from_angle_bracket_uri() {
        assert_eq!(extract_uri_user("<sip:1001@pbx.local>"), "1001");
        assert_eq!(extract_uri_user("<sip:+15551234567@gw.com>"), "+15551234567");
    }

    #[test]
    fn extract_user_from_bare_uri() {
        assert_eq!(extract_uri_user("sip:1001@pbx.local"), "1001");
    }

    #[test]
    fn extract_user_with_tag() {
        assert_eq!(
            extract_uri_user("<sip:1001@pbx.local>;tag=abc123"),
            "1001"
        );
    }

    #[test]
    fn extract_user_sips() {
        assert_eq!(extract_uri_user("<sips:secure@tls.example.com>"), "secure");
    }

    #[test]
    fn extract_user_no_uri() {
        assert_eq!(extract_uri_user("not-a-sip-uri"), "");
    }

    #[test]
    fn extract_user_no_at() {
        assert_eq!(extract_uri_user("sip:pbx.local"), "");
    }
}

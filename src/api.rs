use serde::{Deserialize, Serialize};

use crate::call::{CallDirection, CallInfo, CallStatus};

// ── POST /v1/calls (outbound) ──

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CreateCallRequest {
    pub to: String,
    pub from: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub webhook_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trunk: Option<String>,
    /// Target peer name for outbound calls via trunk host server.
    /// Mutually exclusive with `trunk`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub peer: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CreateCallResponse {
    pub call_id: String,
    pub status: CallStatus,
    pub ws_url: String,
}

// ── GET /v1/calls ──

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CallListResponse {
    pub calls: Vec<CallInfo>,
}

// ── POST /v1/calls/{id}/transfer ──

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TransferRequest {
    pub target: String,
}

// ── POST /v1/calls/{id}/dtmf ──

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DtmfRequest {
    pub digits: String,
}

// ── Incoming call webhook (xbridge → your app) ──

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IncomingCallWebhook {
    pub call_id: String,
    pub from: String,
    pub to: String,
    pub direction: CallDirection,
    /// Name of the trunk host peer, if this call came from a peer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub peer: Option<String>,
}

// ── Incoming call response (your app → xbridge) ──

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IncomingCallResponse {
    pub action: IncomingCallAction,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum IncomingCallAction {
    Accept,
    Reject,
}

// ── POST /v1/calls/{id}/play ──

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PlayRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio: Option<String>,
    #[serde(default = "default_loop_count")]
    pub loop_count: u32,
}

fn default_loop_count() -> u32 {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PlayResponse {
    pub play_id: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ── CreateCallRequest ──

    #[test]
    fn create_call_request_full() {
        let req = CreateCallRequest {
            to: "+15551234567".into(),
            from: "+15559876543".into(),
            webhook_url: Some("https://app.com/events".into()),
            stream: Some(true),
            trunk: Some("primary".into()),
            peer: None,
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["to"], "+15551234567");
        assert_eq!(json["from"], "+15559876543");
        assert_eq!(json["webhook_url"], "https://app.com/events");
        assert_eq!(json["stream"], true);

        let back: CreateCallRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req, back);
    }

    #[test]
    fn create_call_request_minimal() {
        let raw = r#"{"to":"+15551234567","from":"+15559876543"}"#;
        let req: CreateCallRequest = serde_json::from_str(raw).unwrap();
        assert_eq!(req.to, "+15551234567");
        assert!(req.webhook_url.is_none());
        assert!(req.stream.is_none());
    }

    #[test]
    fn create_call_request_omits_none_fields() {
        let req = CreateCallRequest {
            to: "+15551234567".into(),
            from: "+15559876543".into(),
            webhook_url: None,
            stream: None,
            trunk: None,
            peer: None,
        };
        let json = serde_json::to_value(&req).unwrap();
        assert!(json.get("webhook_url").is_none());
        assert!(json.get("stream").is_none());
        assert!(json.get("trunk").is_none());
    }

    // ── CreateCallResponse ──

    #[test]
    fn create_call_response_matches_spec() {
        let resp = CreateCallResponse {
            call_id: "abc123".into(),
            status: CallStatus::Dialing,
            ws_url: "ws://xbridge:8080/ws/abc123".into(),
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["call_id"], "abc123");
        assert_eq!(json["status"], "dialing");
        assert_eq!(json["ws_url"], "ws://xbridge:8080/ws/abc123");

        let back: CreateCallResponse = serde_json::from_value(json).unwrap();
        assert_eq!(resp, back);
    }

    // ── CallListResponse ──

    #[test]
    fn call_list_response_empty() {
        let resp = CallListResponse { calls: vec![] };
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json, json!({"calls": []}));
    }

    #[test]
    fn call_list_response_with_calls() {
        let resp = CallListResponse {
            calls: vec![CallInfo {
                call_id: "abc123".into(),
                from: "+15551234567".into(),
                to: "+15559876543".into(),
                direction: CallDirection::Inbound,
                status: CallStatus::InProgress,
                peer: None,
            }],
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["calls"].as_array().unwrap().len(), 1);
        assert_eq!(json["calls"][0]["call_id"], "abc123");
    }

    // ── TransferRequest / DtmfRequest ──

    #[test]
    fn transfer_request_roundtrip() {
        let req = TransferRequest {
            target: "sip:1003@pbx".into(),
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json, json!({"target": "sip:1003@pbx"}));

        let back: TransferRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req, back);
    }

    #[test]
    fn dtmf_request_roundtrip() {
        let req = DtmfRequest {
            digits: "1234".into(),
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json, json!({"digits": "1234"}));

        let back: DtmfRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req, back);
    }

    // ── IncomingCallWebhook ──

    #[test]
    fn incoming_call_webhook_matches_spec() {
        let hook = IncomingCallWebhook {
            call_id: "abc123".into(),
            from: "+15551234567".into(),
            to: "+15559876543".into(),
            direction: CallDirection::Inbound,
            peer: None,
        };
        let json = serde_json::to_value(&hook).unwrap();
        assert_eq!(json["call_id"], "abc123");
        assert_eq!(json["direction"], "inbound");

        let back: IncomingCallWebhook = serde_json::from_value(json).unwrap();
        assert_eq!(hook, back);
    }

    // ── IncomingCallResponse ──

    #[test]
    fn incoming_call_response_accept() {
        let resp = IncomingCallResponse {
            action: IncomingCallAction::Accept,
            stream: Some(true),
            reason: None,
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["action"], "accept");
        assert_eq!(json["stream"], true);
        assert!(json.get("reason").is_none());
    }

    #[test]
    fn incoming_call_response_reject() {
        let resp = IncomingCallResponse {
            action: IncomingCallAction::Reject,
            stream: None,
            reason: Some("busy".into()),
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["action"], "reject");
        assert_eq!(json["reason"], "busy");
        assert!(json.get("stream").is_none());
    }

    #[test]
    fn incoming_call_response_deserializes_from_spec() {
        let raw = r#"{"action":"accept","stream":true}"#;
        let resp: IncomingCallResponse = serde_json::from_str(raw).unwrap();
        assert_eq!(resp.action, IncomingCallAction::Accept);
        assert_eq!(resp.stream, Some(true));
        assert!(resp.reason.is_none());

        let raw = r#"{"action":"reject","reason":"busy"}"#;
        let resp: IncomingCallResponse = serde_json::from_str(raw).unwrap();
        assert_eq!(resp.action, IncomingCallAction::Reject);
        assert_eq!(resp.reason.as_deref(), Some("busy"));
    }
}

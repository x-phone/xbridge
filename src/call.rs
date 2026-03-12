use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum CallStatus {
    Dialing,
    Ringing,
    InProgress,
    OnHold,
    Completed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum CallDirection {
    Inbound,
    Outbound,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CallInfo {
    pub call_id: String,
    pub from: String,
    pub to: String,
    pub direction: CallDirection,
    pub status: CallStatus,
    /// Name of the trunk host peer, if this call came from/to a peer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub peer: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn call_status_serializes_snake_case() {
        assert_eq!(
            serde_json::to_value(CallStatus::InProgress).unwrap(),
            serde_json::json!("in_progress")
        );
        assert_eq!(
            serde_json::to_value(CallStatus::OnHold).unwrap(),
            serde_json::json!("on_hold")
        );
        assert_eq!(
            serde_json::to_value(CallStatus::Dialing).unwrap(),
            serde_json::json!("dialing")
        );
    }

    #[test]
    fn call_status_deserializes_snake_case() {
        assert_eq!(
            serde_json::from_str::<CallStatus>("\"in_progress\"").unwrap(),
            CallStatus::InProgress
        );
        assert_eq!(
            serde_json::from_str::<CallStatus>("\"on_hold\"").unwrap(),
            CallStatus::OnHold
        );
    }

    #[test]
    fn call_direction_roundtrip() {
        for dir in [CallDirection::Inbound, CallDirection::Outbound] {
            let json = serde_json::to_string(&dir).unwrap();
            let back: CallDirection = serde_json::from_str(&json).unwrap();
            assert_eq!(dir, back);
        }
    }

    #[test]
    fn call_info_roundtrip() {
        let info = CallInfo {
            call_id: "abc123".into(),
            from: "+15551234567".into(),
            to: "+15559876543".into(),
            direction: CallDirection::Inbound,
            status: CallStatus::InProgress,
            peer: None,
        };
        let json = serde_json::to_value(&info).unwrap();
        assert_eq!(json["call_id"], "abc123");
        assert_eq!(json["direction"], "inbound");
        assert_eq!(json["status"], "in_progress");

        let back: CallInfo = serde_json::from_value(json).unwrap();
        assert_eq!(info, back);
    }
}

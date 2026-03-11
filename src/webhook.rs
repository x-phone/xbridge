use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "event")]
pub enum WebhookEvent {
    #[serde(rename = "call.ringing")]
    Ringing {
        call_id: String,
        from: String,
        to: String,
    },

    #[serde(rename = "call.answered")]
    Answered { call_id: String },

    #[serde(rename = "call.ended")]
    Ended {
        call_id: String,
        reason: String,
        duration: u64,
    },

    #[serde(rename = "call.dtmf")]
    Dtmf { call_id: String, digit: String },

    #[serde(rename = "call.hold")]
    Hold { call_id: String },

    #[serde(rename = "call.resumed")]
    Resumed { call_id: String },

    #[serde(rename = "call.play_finished")]
    PlayFinished {
        call_id: String,
        play_id: String,
        interrupted: bool,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn ringing_matches_spec() {
        let event = WebhookEvent::Ringing {
            call_id: "abc123".into(),
            from: "+15551234567".into(),
            to: "+15559876543".into(),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(
            json,
            json!({
                "event": "call.ringing",
                "call_id": "abc123",
                "from": "+15551234567",
                "to": "+15559876543"
            })
        );
        let back: WebhookEvent = serde_json::from_value(json).unwrap();
        assert_eq!(event, back);
    }

    #[test]
    fn answered_matches_spec() {
        let event = WebhookEvent::Answered {
            call_id: "abc123".into(),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json, json!({"event": "call.answered", "call_id": "abc123"}));

        let back: WebhookEvent = serde_json::from_value(json).unwrap();
        assert_eq!(event, back);
    }

    #[test]
    fn ended_matches_spec() {
        let event = WebhookEvent::Ended {
            call_id: "abc123".into(),
            reason: "normal".into(),
            duration: 45,
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["event"], "call.ended");
        assert_eq!(json["reason"], "normal");
        assert_eq!(json["duration"], 45);

        let back: WebhookEvent = serde_json::from_value(json).unwrap();
        assert_eq!(event, back);
    }

    #[test]
    fn dtmf_matches_spec() {
        let event = WebhookEvent::Dtmf {
            call_id: "abc123".into(),
            digit: "5".into(),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json, json!({"event": "call.dtmf", "call_id": "abc123", "digit": "5"}));

        let back: WebhookEvent = serde_json::from_value(json).unwrap();
        assert_eq!(event, back);
    }

    #[test]
    fn hold_matches_spec() {
        let event = WebhookEvent::Hold {
            call_id: "abc123".into(),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json, json!({"event": "call.hold", "call_id": "abc123"}));

        let back: WebhookEvent = serde_json::from_value(json).unwrap();
        assert_eq!(event, back);
    }

    #[test]
    fn resumed_matches_spec() {
        let event = WebhookEvent::Resumed {
            call_id: "abc123".into(),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json, json!({"event": "call.resumed", "call_id": "abc123"}));

        let back: WebhookEvent = serde_json::from_value(json).unwrap();
        assert_eq!(event, back);
    }

    #[test]
    fn play_finished_matches_spec() {
        let event = WebhookEvent::PlayFinished {
            call_id: "abc123".into(),
            play_id: "play_0".into(),
            interrupted: false,
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["event"], "call.play_finished");
        assert_eq!(json["call_id"], "abc123");
        assert_eq!(json["play_id"], "play_0");
        assert_eq!(json["interrupted"], false);

        let back: WebhookEvent = serde_json::from_value(json).unwrap();
        assert_eq!(event, back);
    }

    #[test]
    fn deserialize_all_events_from_raw_json() {
        let cases = vec![
            r#"{"event":"call.ringing","call_id":"c1","from":"+1","to":"+2"}"#,
            r#"{"event":"call.answered","call_id":"c1"}"#,
            r#"{"event":"call.ended","call_id":"c1","reason":"normal","duration":10}"#,
            r##"{"event":"call.dtmf","call_id":"c1","digit":"#"}"##,
            r#"{"event":"call.hold","call_id":"c1"}"#,
            r#"{"event":"call.resumed","call_id":"c1"}"#,
            r#"{"event":"call.play_finished","call_id":"c1","play_id":"play_0","interrupted":false}"#,
        ];
        for raw in cases {
            let event: WebhookEvent = serde_json::from_str(raw).unwrap();
            // Roundtrip
            let json = serde_json::to_string(&event).unwrap();
            let back: WebhookEvent = serde_json::from_str(&json).unwrap();
            assert_eq!(event, back);
        }
    }
}

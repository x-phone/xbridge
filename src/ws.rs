use serde::{Deserialize, Serialize};

// ── Server → Client events (Twilio-compatible mode) ──

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "event")]
pub enum ServerEvent {
    #[serde(rename = "connected")]
    Connected { protocol: String, version: String },

    #[serde(rename = "start")]
    Start {
        #[serde(rename = "streamSid")]
        stream_sid: String,
        start: StartPayload,
    },

    #[serde(rename = "media")]
    Media {
        #[serde(rename = "streamSid")]
        stream_sid: String,
        media: ServerMediaPayload,
    },

    #[serde(rename = "stop")]
    Stop {
        #[serde(rename = "streamSid")]
        stream_sid: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StartPayload {
    #[serde(rename = "callSid")]
    pub call_sid: String,
    pub tracks: Vec<String>,
    #[serde(rename = "mediaFormat")]
    pub media_format: MediaFormat,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MediaFormat {
    pub encoding: String,
    #[serde(rename = "sampleRate")]
    pub sample_rate: u32,
    pub channels: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ServerMediaPayload {
    pub timestamp: String,
    pub payload: String,
}

// ── Client → Server events (Twilio-compatible mode) ──

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "event")]
pub enum ClientEvent {
    #[serde(rename = "media")]
    Media {
        #[serde(rename = "streamSid")]
        stream_sid: String,
        media: ClientMediaPayload,
    },

    #[serde(rename = "mark")]
    Mark {
        #[serde(rename = "streamSid")]
        stream_sid: String,
        mark: MarkPayload,
    },

    #[serde(rename = "clear")]
    Clear {
        #[serde(rename = "streamSid")]
        stream_sid: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ClientMediaPayload {
    pub payload: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MarkPayload {
    pub name: String,
}

// ── Native mode ──
// Binary frames: [0x01][2 bytes: length BE][PCM16 LE audio]
// Control messages remain JSON text frames.

pub const NATIVE_AUDIO_TAG: u8 = 0x01;

pub fn encode_native_audio(pcm_data: &[u8]) -> Option<Vec<u8>> {
    let len: u16 = pcm_data.len().try_into().ok()?;
    let mut frame = Vec::with_capacity(3 + pcm_data.len());
    frame.push(NATIVE_AUDIO_TAG);
    frame.extend_from_slice(&len.to_be_bytes());
    frame.extend_from_slice(pcm_data);
    Some(frame)
}

pub fn decode_native_audio(frame: &[u8]) -> Option<&[u8]> {
    if frame.len() < 3 || frame[0] != NATIVE_AUDIO_TAG {
        return None;
    }
    let len = u16::from_be_bytes([frame[1], frame[2]]) as usize;
    if frame.len() < 3 + len {
        return None;
    }
    Some(&frame[3..3 + len])
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ── ServerEvent tests ──

    #[test]
    fn server_connected_matches_spec() {
        let event = ServerEvent::Connected {
            protocol: "Call".into(),
            version: "1.0.0".into(),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(
            json,
            json!({"event": "connected", "protocol": "Call", "version": "1.0.0"})
        );
        let back: ServerEvent = serde_json::from_value(json).unwrap();
        assert_eq!(event, back);
    }

    #[test]
    fn server_start_matches_spec() {
        let event = ServerEvent::Start {
            stream_sid: "call_001".into(),
            start: StartPayload {
                call_sid: "call_001".into(),
                tracks: vec!["inbound".into()],
                media_format: MediaFormat {
                    encoding: "audio/x-mulaw".into(),
                    sample_rate: 8000,
                    channels: 1,
                },
            },
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["event"], "start");
        assert_eq!(json["streamSid"], "call_001");
        assert_eq!(json["start"]["callSid"], "call_001");
        assert_eq!(json["start"]["tracks"], json!(["inbound"]));
        assert_eq!(json["start"]["mediaFormat"]["encoding"], "audio/x-mulaw");
        assert_eq!(json["start"]["mediaFormat"]["sampleRate"], 8000);
        assert_eq!(json["start"]["mediaFormat"]["channels"], 1);

        let back: ServerEvent = serde_json::from_value(json).unwrap();
        assert_eq!(event, back);
    }

    #[test]
    fn server_media_matches_spec() {
        let event = ServerEvent::Media {
            stream_sid: "call_001".into(),
            media: ServerMediaPayload {
                timestamp: "0".into(),
                payload: "dGVzdA==".into(),
            },
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["event"], "media");
        assert_eq!(json["streamSid"], "call_001");
        assert_eq!(json["media"]["timestamp"], "0");
        assert_eq!(json["media"]["payload"], "dGVzdA==");

        let back: ServerEvent = serde_json::from_value(json).unwrap();
        assert_eq!(event, back);
    }

    #[test]
    fn server_stop_matches_spec() {
        let event = ServerEvent::Stop {
            stream_sid: "call_001".into(),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json, json!({"event": "stop", "streamSid": "call_001"}));

        let back: ServerEvent = serde_json::from_value(json).unwrap();
        assert_eq!(event, back);
    }

    // ── ClientEvent tests ──

    #[test]
    fn client_media_matches_spec() {
        let event = ClientEvent::Media {
            stream_sid: "call_001".into(),
            media: ClientMediaPayload {
                payload: "dGVzdA==".into(),
            },
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["event"], "media");
        assert_eq!(json["streamSid"], "call_001");
        assert_eq!(json["media"]["payload"], "dGVzdA==");

        let back: ClientEvent = serde_json::from_value(json).unwrap();
        assert_eq!(event, back);
    }

    #[test]
    fn client_mark_matches_spec() {
        let event = ClientEvent::Mark {
            stream_sid: "call_001".into(),
            mark: MarkPayload {
                name: "utterance_end".into(),
            },
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["event"], "mark");
        assert_eq!(json["mark"]["name"], "utterance_end");

        let back: ClientEvent = serde_json::from_value(json).unwrap();
        assert_eq!(event, back);
    }

    #[test]
    fn client_clear_matches_spec() {
        let event = ClientEvent::Clear {
            stream_sid: "call_001".into(),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(
            json,
            json!({"event": "clear", "streamSid": "call_001"})
        );

        let back: ClientEvent = serde_json::from_value(json).unwrap();
        assert_eq!(event, back);
    }

    // ── Native mode tests ──

    #[test]
    fn native_audio_encode_decode_roundtrip() {
        let pcm = vec![0x01, 0x02, 0x03, 0x04];
        let frame = encode_native_audio(&pcm).unwrap();

        assert_eq!(frame[0], NATIVE_AUDIO_TAG);
        assert_eq!(u16::from_be_bytes([frame[1], frame[2]]), 4);
        assert_eq!(&frame[3..], &pcm);

        let decoded = decode_native_audio(&frame).unwrap();
        assert_eq!(decoded, &pcm);
    }

    #[test]
    fn native_audio_empty_payload() {
        let frame = encode_native_audio(&[]).unwrap();
        assert_eq!(frame.len(), 3);
        let decoded = decode_native_audio(&frame).unwrap();
        assert!(decoded.is_empty());
    }

    #[test]
    fn native_audio_encode_rejects_oversized_payload() {
        let oversized = vec![0u8; u16::MAX as usize + 1];
        assert!(encode_native_audio(&oversized).is_none());
    }

    #[test]
    fn native_audio_decode_rejects_bad_tag() {
        let frame = vec![0x02, 0x00, 0x01, 0xFF];
        assert!(decode_native_audio(&frame).is_none());
    }

    #[test]
    fn native_audio_decode_rejects_truncated_frame() {
        // Header says 4 bytes but only 2 present
        let frame = vec![0x01, 0x00, 0x04, 0xAA, 0xBB];
        assert!(decode_native_audio(&frame).is_none());
    }

    #[test]
    fn native_audio_decode_rejects_too_short() {
        assert!(decode_native_audio(&[]).is_none());
        assert!(decode_native_audio(&[0x01]).is_none());
        assert!(decode_native_audio(&[0x01, 0x00]).is_none());
    }

    // ── Deserialization from raw JSON strings (simulating WS messages) ──

    #[test]
    fn deserialize_server_event_from_json_string() {
        let raw = r#"{"event":"connected","protocol":"Call","version":"1.0.0"}"#;
        let event: ServerEvent = serde_json::from_str(raw).unwrap();
        assert_eq!(
            event,
            ServerEvent::Connected {
                protocol: "Call".into(),
                version: "1.0.0".into(),
            }
        );
    }

    #[test]
    fn deserialize_client_event_from_json_string() {
        let raw = r#"{"event":"media","streamSid":"abc","media":{"payload":"AQID"}}"#;
        let event: ClientEvent = serde_json::from_str(raw).unwrap();
        assert_eq!(
            event,
            ClientEvent::Media {
                stream_sid: "abc".into(),
                media: ClientMediaPayload {
                    payload: "AQID".into(),
                },
            }
        );
    }
}

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Config {
    pub listen: ListenConfig,
    pub sip: SipConfig,
    pub webhook: WebhookConfig,
    #[serde(default)]
    pub stream: StreamConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ListenConfig {
    pub http: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SipConfig {
    pub username: String,
    #[serde(skip_serializing)]
    pub password: String,
    pub host: String,
    #[serde(default = "default_transport")]
    pub transport: SipTransport,
    #[serde(default = "default_rtp_port_min")]
    pub rtp_port_min: u16,
    #[serde(default = "default_rtp_port_max")]
    pub rtp_port_max: u16,
    #[serde(default)]
    pub srtp: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stun_server: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum SipTransport {
    Udp,
    Tcp,
    Tls,
}

fn default_transport() -> SipTransport {
    SipTransport::Udp
}

fn default_rtp_port_min() -> u16 {
    10000
}

fn default_rtp_port_max() -> u16 {
    20000
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WebhookConfig {
    pub url: String,
    #[serde(default = "default_timeout")]
    pub timeout: String,
    #[serde(default = "default_retry")]
    pub retry: u32,
}

fn default_timeout() -> String {
    "5s".into()
}

fn default_retry() -> u32 {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StreamConfig {
    #[serde(default = "default_stream_mode")]
    pub mode: StreamMode,
    #[serde(default = "default_encoding")]
    pub encoding: AudioEncoding,
    #[serde(default = "default_sample_rate")]
    pub sample_rate: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum StreamMode {
    Twilio,
    Native,
}

fn default_stream_mode() -> StreamMode {
    StreamMode::Twilio
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum AudioEncoding {
    #[serde(rename = "audio/x-mulaw")]
    Mulaw,
    #[serde(rename = "audio/x-l16")]
    L16,
}

fn default_encoding() -> AudioEncoding {
    AudioEncoding::Mulaw
}

fn default_sample_rate() -> u32 {
    8000
}

impl Default for Config {
    fn default() -> Self {
        Self {
            listen: ListenConfig {
                http: "0.0.0.0:8080".into(),
            },
            sip: SipConfig {
                username: String::new(),
                password: String::new(),
                host: "localhost".into(),
                transport: default_transport(),
                rtp_port_min: default_rtp_port_min(),
                rtp_port_max: default_rtp_port_max(),
                srtp: false,
                stun_server: None,
            },
            webhook: WebhookConfig {
                url: "http://localhost:3000/events".into(),
                timeout: default_timeout(),
                retry: default_retry(),
            },
            stream: StreamConfig::default(),
        }
    }
}

impl Default for StreamConfig {
    fn default() -> Self {
        Self {
            mode: default_stream_mode(),
            encoding: AudioEncoding::Mulaw,
            sample_rate: default_sample_rate(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn full_config_json() -> serde_json::Value {
        json!({
            "listen": { "http": "0.0.0.0:8080" },
            "sip": {
                "username": "1001",
                "password": "secret",
                "host": "sip.telnyx.com",
                "transport": "udp",
                "rtp_port_min": 10000,
                "rtp_port_max": 20000,
                "srtp": true,
                "stun_server": "stun.l.google.com:19302"
            },
            "webhook": {
                "url": "https://your-app.com/events",
                "timeout": "5s",
                "retry": 1
            },
            "stream": {
                "mode": "twilio",
                "encoding": "audio/x-mulaw",
                "sample_rate": 8000
            }
        })
    }

    #[test]
    fn full_config_roundtrip() {
        let json = full_config_json();
        let config: Config = serde_json::from_value(json.clone()).unwrap();

        assert_eq!(config.listen.http, "0.0.0.0:8080");
        assert_eq!(config.sip.username, "1001");
        assert_eq!(config.sip.host, "sip.telnyx.com");
        assert_eq!(config.sip.transport, SipTransport::Udp);
        assert!(config.sip.srtp);
        assert_eq!(
            config.sip.stun_server.as_deref(),
            Some("stun.l.google.com:19302")
        );
        assert_eq!(config.webhook.url, "https://your-app.com/events");
        assert_eq!(config.stream.mode, StreamMode::Twilio);
        assert_eq!(config.stream.encoding, AudioEncoding::Mulaw);
        assert_eq!(config.stream.sample_rate, 8000);

        // password is excluded from serialization
        let back = serde_json::to_value(&config).unwrap();
        assert!(back["sip"].get("password").is_none());
    }

    #[test]
    fn minimal_config_uses_defaults() {
        let json = json!({
            "listen": { "http": "0.0.0.0:8080" },
            "sip": {
                "username": "1001",
                "password": "secret",
                "host": "sip.telnyx.com"
            },
            "webhook": {
                "url": "https://app.com/events"
            }
        });
        let config: Config = serde_json::from_value(json).unwrap();

        // SIP defaults
        assert_eq!(config.sip.transport, SipTransport::Udp);
        assert_eq!(config.sip.rtp_port_min, 10000);
        assert_eq!(config.sip.rtp_port_max, 20000);
        assert!(!config.sip.srtp);
        assert!(config.sip.stun_server.is_none());

        // Webhook defaults
        assert_eq!(config.webhook.timeout, "5s");
        assert_eq!(config.webhook.retry, 1);

        // Stream defaults
        assert_eq!(config.stream.mode, StreamMode::Twilio);
        assert_eq!(config.stream.encoding, AudioEncoding::Mulaw);
        assert_eq!(config.stream.sample_rate, 8000);
    }

    #[test]
    fn sip_transport_variants() {
        for (s, expected) in [
            ("\"udp\"", SipTransport::Udp),
            ("\"tcp\"", SipTransport::Tcp),
            ("\"tls\"", SipTransport::Tls),
        ] {
            let t: SipTransport = serde_json::from_str(s).unwrap();
            assert_eq!(t, expected);
        }
    }

    #[test]
    fn stream_mode_variants() {
        assert_eq!(
            serde_json::from_str::<StreamMode>("\"twilio\"").unwrap(),
            StreamMode::Twilio
        );
        assert_eq!(
            serde_json::from_str::<StreamMode>("\"native\"").unwrap(),
            StreamMode::Native
        );
    }

    #[test]
    fn config_rejects_missing_required_fields() {
        // Missing sip.host
        let json = json!({
            "listen": { "http": "0.0.0.0:8080" },
            "sip": { "username": "1001", "password": "secret" },
            "webhook": { "url": "https://app.com" }
        });
        assert!(serde_json::from_value::<Config>(json).is_err());
    }
}

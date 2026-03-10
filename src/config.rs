use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::Path;

// ── Error ──

#[derive(Debug)]
pub enum ConfigError {
    Io(std::io::Error),
    Yaml(serde_yaml::Error),
    Toml(toml::de::Error),
    UnsupportedFormat(String),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "config I/O error: {e}"),
            Self::Yaml(e) => write!(f, "YAML parse error: {e}"),
            Self::Toml(e) => write!(f, "TOML parse error: {e}"),
            Self::UnsupportedFormat(ext) => {
                write!(f, "unsupported config format: .{ext} (expected .yaml, .yml, or .toml)")
            }
        }
    }
}

impl std::error::Error for ConfigError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::Yaml(e) => Some(e),
            Self::Toml(e) => Some(e),
            Self::UnsupportedFormat(_) => None,
        }
    }
}

// ── Types ──

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Config {
    pub listen: ListenConfig,
    pub sip: SipConfig,
    pub webhook: WebhookConfig,
    #[serde(default)]
    pub stream: StreamConfig,
    #[serde(default)]
    pub auth: AuthConfig,
    #[serde(default)]
    pub tls: TlsConfig,
    #[serde(default)]
    pub rate_limit: RateLimitConfig,
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AuthConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct RateLimitConfig {
    #[serde(default)]
    pub requests_per_second: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct TlsConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cert: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}

fn default_sample_rate() -> u32 {
    8000
}

// ── Defaults ──

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
            auth: AuthConfig::default(),
            tls: TlsConfig::default(),
            rate_limit: RateLimitConfig::default(),
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

// ── Loading ──

impl Config {
    /// Load config from an optional file path, then apply env var overrides.
    /// If no path is given, starts from defaults.
    pub fn load(path: Option<&Path>) -> Result<Self, ConfigError> {
        let mut config = match path {
            Some(p) => Self::from_file(p)?,
            None => Self::default(),
        };
        Self::apply_env_overrides(&mut config);
        Ok(config)
    }

    /// Parse config from a file, detecting format by extension.
    pub fn from_file(path: &Path) -> Result<Self, ConfigError> {
        let content = std::fs::read_to_string(path).map_err(ConfigError::Io)?;
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        match ext {
            "yaml" | "yml" => Self::from_yaml(&content),
            "toml" => Self::from_toml(&content),
            other => Err(ConfigError::UnsupportedFormat(other.to_string())),
        }
    }

    pub fn from_yaml(content: &str) -> Result<Self, ConfigError> {
        serde_yaml::from_str(content).map_err(ConfigError::Yaml)
    }

    pub fn from_toml(content: &str) -> Result<Self, ConfigError> {
        toml::from_str(content).map_err(ConfigError::Toml)
    }

    /// Apply XBRIDGE_* environment variable overrides.
    pub fn apply_env_overrides(config: &mut Self) {
        Self::apply_env_overrides_with(config, |key| std::env::var(key));
    }

    fn apply_env_overrides_with<F>(config: &mut Self, get_var: F)
    where
        F: Fn(&str) -> Result<String, std::env::VarError>,
    {
        if let Ok(v) = get_var("XBRIDGE_LISTEN_HTTP") {
            config.listen.http = v;
        }

        // SIP
        if let Ok(v) = get_var("XBRIDGE_SIP_USERNAME") {
            config.sip.username = v;
        }
        if let Ok(v) = get_var("XBRIDGE_SIP_PASSWORD") {
            config.sip.password = v;
        }
        if let Ok(v) = get_var("XBRIDGE_SIP_HOST") {
            config.sip.host = v;
        }
        if let Ok(v) = get_var("XBRIDGE_SIP_TRANSPORT") {
            match v.as_str() {
                "udp" => config.sip.transport = SipTransport::Udp,
                "tcp" => config.sip.transport = SipTransport::Tcp,
                "tls" => config.sip.transport = SipTransport::Tls,
                _ => {}
            }
        }
        if let Ok(v) = get_var("XBRIDGE_SIP_RTP_PORT_MIN") {
            if let Ok(n) = v.parse() {
                config.sip.rtp_port_min = n;
            }
        }
        if let Ok(v) = get_var("XBRIDGE_SIP_RTP_PORT_MAX") {
            if let Ok(n) = v.parse() {
                config.sip.rtp_port_max = n;
            }
        }
        if let Ok(v) = get_var("XBRIDGE_SIP_SRTP") {
            match v.as_str() {
                "true" | "1" => config.sip.srtp = true,
                "false" | "0" => config.sip.srtp = false,
                _ => {}
            }
        }
        if let Ok(v) = get_var("XBRIDGE_SIP_STUN_SERVER") {
            config.sip.stun_server = Some(v);
        }

        // Webhook
        if let Ok(v) = get_var("XBRIDGE_WEBHOOK_URL") {
            config.webhook.url = v;
        }
        if let Ok(v) = get_var("XBRIDGE_WEBHOOK_TIMEOUT") {
            config.webhook.timeout = v;
        }
        if let Ok(v) = get_var("XBRIDGE_WEBHOOK_RETRY") {
            if let Ok(n) = v.parse() {
                config.webhook.retry = n;
            }
        }

        // Stream
        if let Ok(v) = get_var("XBRIDGE_STREAM_MODE") {
            match v.as_str() {
                "twilio" => config.stream.mode = StreamMode::Twilio,
                "native" => config.stream.mode = StreamMode::Native,
                _ => {}
            }
        }
        if let Ok(v) = get_var("XBRIDGE_STREAM_ENCODING") {
            match v.as_str() {
                "audio/x-mulaw" => config.stream.encoding = AudioEncoding::Mulaw,
                "audio/x-l16" => config.stream.encoding = AudioEncoding::L16,
                _ => {}
            }
        }
        if let Ok(v) = get_var("XBRIDGE_STREAM_SAMPLE_RATE") {
            if let Ok(n) = v.parse() {
                config.stream.sample_rate = n;
            }
        }

        // Auth
        if let Ok(v) = get_var("XBRIDGE_AUTH_API_KEY") {
            config.auth.api_key = Some(v);
        }

        // Rate limit
        if let Ok(v) = get_var("XBRIDGE_RATE_LIMIT_RPS") {
            if let Ok(n) = v.parse() {
                config.rate_limit.requests_per_second = Some(n);
            }
        }

        // TLS
        if let Ok(v) = get_var("XBRIDGE_TLS_CERT") {
            config.tls.cert = Some(v);
        }
        if let Ok(v) = get_var("XBRIDGE_TLS_KEY") {
            config.tls.key = Some(v);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::HashMap;

    // ── Serde tests ──

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
        let config: Config = serde_json::from_value(json).unwrap();

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

        assert_eq!(config.sip.transport, SipTransport::Udp);
        assert_eq!(config.sip.rtp_port_min, 10000);
        assert_eq!(config.sip.rtp_port_max, 20000);
        assert!(!config.sip.srtp);
        assert!(config.sip.stun_server.is_none());
        assert_eq!(config.webhook.timeout, "5s");
        assert_eq!(config.webhook.retry, 1);
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
        let json = json!({
            "listen": { "http": "0.0.0.0:8080" },
            "sip": { "username": "1001", "password": "secret" },
            "webhook": { "url": "https://app.com" }
        });
        assert!(serde_json::from_value::<Config>(json).is_err());
    }

    // ── YAML loading ──

    const FULL_YAML: &str = r#"
listen:
  http: "0.0.0.0:9090"

sip:
  username: "1001"
  password: "secret"
  host: "sip.telnyx.com"
  transport: "tls"
  rtp_port_min: 16000
  rtp_port_max: 32000
  srtp: true
  stun_server: "stun.l.google.com:19302"

webhook:
  url: "https://app.com/events"
  timeout: "10s"
  retry: 3

stream:
  mode: "native"
  encoding: "audio/x-l16"
  sample_rate: 16000
"#;

    const MINIMAL_YAML: &str = r#"
listen:
  http: "0.0.0.0:8080"
sip:
  username: "user"
  password: "pass"
  host: "sip.example.com"
webhook:
  url: "https://app.com/hook"
"#;

    #[test]
    fn from_yaml_full() {
        let config = Config::from_yaml(FULL_YAML).unwrap();
        assert_eq!(config.listen.http, "0.0.0.0:9090");
        assert_eq!(config.sip.username, "1001");
        assert_eq!(config.sip.password, "secret");
        assert_eq!(config.sip.host, "sip.telnyx.com");
        assert_eq!(config.sip.transport, SipTransport::Tls);
        assert_eq!(config.sip.rtp_port_min, 16000);
        assert_eq!(config.sip.rtp_port_max, 32000);
        assert!(config.sip.srtp);
        assert_eq!(config.webhook.timeout, "10s");
        assert_eq!(config.webhook.retry, 3);
        assert_eq!(config.stream.mode, StreamMode::Native);
        assert_eq!(config.stream.encoding, AudioEncoding::L16);
        assert_eq!(config.stream.sample_rate, 16000);
    }

    #[test]
    fn from_yaml_minimal_uses_defaults() {
        let config = Config::from_yaml(MINIMAL_YAML).unwrap();
        assert_eq!(config.sip.transport, SipTransport::Udp);
        assert_eq!(config.sip.rtp_port_min, 10000);
        assert!(!config.sip.srtp);
        assert_eq!(config.webhook.timeout, "5s");
        assert_eq!(config.stream.mode, StreamMode::Twilio);
        assert_eq!(config.stream.encoding, AudioEncoding::Mulaw);
    }

    #[test]
    fn from_yaml_rejects_invalid() {
        assert!(Config::from_yaml("not: [valid: yaml: config").is_err());
    }

    // ── TOML loading ──

    const FULL_TOML: &str = r#"
[listen]
http = "0.0.0.0:9090"

[sip]
username = "1001"
password = "secret"
host = "sip.telnyx.com"
transport = "tls"
rtp_port_min = 16000
rtp_port_max = 32000
srtp = true
stun_server = "stun.l.google.com:19302"

[webhook]
url = "https://app.com/events"
timeout = "10s"
retry = 3

[stream]
mode = "native"
encoding = "audio/x-l16"
sample_rate = 16000
"#;

    #[test]
    fn from_toml_full() {
        let config = Config::from_toml(FULL_TOML).unwrap();
        assert_eq!(config.listen.http, "0.0.0.0:9090");
        assert_eq!(config.sip.transport, SipTransport::Tls);
        assert_eq!(config.sip.rtp_port_min, 16000);
        assert_eq!(config.stream.mode, StreamMode::Native);
        assert_eq!(config.stream.encoding, AudioEncoding::L16);
    }

    #[test]
    fn from_toml_rejects_invalid() {
        assert!(Config::from_toml("[missing\nfields").is_err());
    }

    // ── File loading ──

    #[test]
    fn from_file_yaml() {
        let dir = std::env::temp_dir();
        let path = dir.join("xbridge_test_config.yaml");
        std::fs::write(&path, MINIMAL_YAML).unwrap();

        let config = Config::from_file(&path).unwrap();
        assert_eq!(config.sip.host, "sip.example.com");

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn from_file_toml() {
        let dir = std::env::temp_dir();
        let path = dir.join("xbridge_test_config.toml");
        std::fs::write(&path, FULL_TOML).unwrap();

        let config = Config::from_file(&path).unwrap();
        assert_eq!(config.sip.host, "sip.telnyx.com");

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn from_file_unsupported_extension() {
        let dir = std::env::temp_dir();
        let path = dir.join("xbridge_test_config.json");
        std::fs::write(&path, "{}").unwrap();

        let err = Config::from_file(&path).unwrap_err();
        assert!(matches!(err, ConfigError::UnsupportedFormat(_)));

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn from_file_not_found() {
        let err = Config::from_file(Path::new("/nonexistent/xbridge.yaml")).unwrap_err();
        assert!(matches!(err, ConfigError::Io(_)));
    }

    // ── Env var overrides ──
    // Uses dependency injection (get_var closure) to avoid process-global env state.

    fn make_env(vars: &[(&str, &str)]) -> impl Fn(&str) -> Result<String, std::env::VarError> {
        let map: HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |key: &str| {
            map.get(key)
                .cloned()
                .ok_or(std::env::VarError::NotPresent)
        }
    }

    #[test]
    fn env_overrides_string_fields() {
        let mut config = Config::default();
        let env = make_env(&[
            ("XBRIDGE_LISTEN_HTTP", "0.0.0.0:3000"),
            ("XBRIDGE_SIP_USERNAME", "user1"),
            ("XBRIDGE_SIP_PASSWORD", "pass1"),
            ("XBRIDGE_SIP_HOST", "sip.override.com"),
            ("XBRIDGE_WEBHOOK_URL", "https://override.com/events"),
            ("XBRIDGE_WEBHOOK_TIMEOUT", "30s"),
            ("XBRIDGE_SIP_STUN_SERVER", "stun.override.com:3478"),
        ]);
        Config::apply_env_overrides_with(&mut config, env);

        assert_eq!(config.listen.http, "0.0.0.0:3000");
        assert_eq!(config.sip.username, "user1");
        assert_eq!(config.sip.password, "pass1");
        assert_eq!(config.sip.host, "sip.override.com");
        assert_eq!(config.webhook.url, "https://override.com/events");
        assert_eq!(config.webhook.timeout, "30s");
        assert_eq!(
            config.sip.stun_server.as_deref(),
            Some("stun.override.com:3478")
        );
    }

    #[test]
    fn env_overrides_numeric_fields() {
        let mut config = Config::default();
        let env = make_env(&[
            ("XBRIDGE_SIP_RTP_PORT_MIN", "20000"),
            ("XBRIDGE_SIP_RTP_PORT_MAX", "40000"),
            ("XBRIDGE_WEBHOOK_RETRY", "5"),
            ("XBRIDGE_STREAM_SAMPLE_RATE", "16000"),
        ]);
        Config::apply_env_overrides_with(&mut config, env);

        assert_eq!(config.sip.rtp_port_min, 20000);
        assert_eq!(config.sip.rtp_port_max, 40000);
        assert_eq!(config.webhook.retry, 5);
        assert_eq!(config.stream.sample_rate, 16000);
    }

    #[test]
    fn env_overrides_enum_fields() {
        let mut config = Config::default();
        let env = make_env(&[
            ("XBRIDGE_SIP_TRANSPORT", "tls"),
            ("XBRIDGE_STREAM_MODE", "native"),
            ("XBRIDGE_STREAM_ENCODING", "audio/x-l16"),
        ]);
        Config::apply_env_overrides_with(&mut config, env);

        assert_eq!(config.sip.transport, SipTransport::Tls);
        assert_eq!(config.stream.mode, StreamMode::Native);
        assert_eq!(config.stream.encoding, AudioEncoding::L16);
    }

    #[test]
    fn env_overrides_bool_field() {
        let mut config = Config::default();
        assert!(!config.sip.srtp);

        Config::apply_env_overrides_with(&mut config, make_env(&[("XBRIDGE_SIP_SRTP", "true")]));
        assert!(config.sip.srtp);

        Config::apply_env_overrides_with(&mut config, make_env(&[("XBRIDGE_SIP_SRTP", "0")]));
        assert!(!config.sip.srtp);

        Config::apply_env_overrides_with(&mut config, make_env(&[("XBRIDGE_SIP_SRTP", "1")]));
        assert!(config.sip.srtp);
    }

    #[test]
    fn env_overrides_ignore_invalid_values() {
        let mut config = Config::default();
        let env = make_env(&[
            ("XBRIDGE_SIP_TRANSPORT", "invalid"),
            ("XBRIDGE_SIP_RTP_PORT_MIN", "not_a_number"),
            ("XBRIDGE_SIP_SRTP", "maybe"),
            ("XBRIDGE_STREAM_MODE", "unknown"),
            ("XBRIDGE_STREAM_ENCODING", "audio/opus"),
        ]);
        Config::apply_env_overrides_with(&mut config, env);

        // All should remain at defaults
        assert_eq!(config.sip.transport, SipTransport::Udp);
        assert_eq!(config.sip.rtp_port_min, 10000);
        assert!(!config.sip.srtp);
        assert_eq!(config.stream.mode, StreamMode::Twilio);
        assert_eq!(config.stream.encoding, AudioEncoding::Mulaw);
    }

    #[test]
    fn env_overrides_unset_vars_are_noop() {
        let original = Config::default();
        let mut config = Config::default();
        Config::apply_env_overrides_with(&mut config, make_env(&[]));
        assert_eq!(config, original);
    }

    // ── load() combines file + env ──

    #[test]
    fn load_without_file_uses_defaults() {
        let config = Config::load(None).unwrap();
        assert_eq!(config, Config::default());
    }

    #[test]
    fn load_from_yaml_file() {
        let dir = std::env::temp_dir();
        let path = dir.join("xbridge_test_load.yaml");
        std::fs::write(&path, MINIMAL_YAML).unwrap();

        let config = Config::load(Some(&path)).unwrap();
        assert_eq!(config.sip.host, "sip.example.com");

        std::fs::remove_file(&path).ok();
    }
}

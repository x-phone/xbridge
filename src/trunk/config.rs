use serde::{Deserialize, Serialize};
use std::net::IpAddr;

/// Configuration for the trunk host SIP server (serde-friendly wrapper).
///
/// Deserializes from YAML/TOML and converts to `xphone::ServerConfig` at startup.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ServerConfig {
    /// Address to listen on (e.g., "0.0.0.0:5080").
    pub listen: String,
    /// Configured peers that are allowed to connect.
    #[serde(default)]
    pub peers: Vec<PeerConfig>,
    /// Minimum RTP port for media. 0 = OS-assigned.
    #[serde(default)]
    pub rtp_port_min: u16,
    /// Maximum RTP port for media. 0 = OS-assigned.
    #[serde(default)]
    pub rtp_port_max: u16,
    /// IP address advertised in SDP for RTP media. When the server listens on
    /// 0.0.0.0 this must be set to the reachable IP (e.g. the container IP).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rtp_address: Option<IpAddr>,
}

/// A known SIP peer (PBX system) that can send/receive calls.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PeerConfig {
    /// Human-readable name for this peer (e.g., "office-pbx").
    pub name: String,
    /// Single IP address for IP-based authentication (simple case).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<IpAddr>,
    /// Multiple IPs or CIDR ranges for IP-based authentication.
    /// Supports exact IPs ("54.172.60.1") and CIDRs ("54.172.60.0/22").
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hosts: Vec<String>,
    /// SIP port for outbound calls to this peer. Defaults to 5060.
    #[serde(default = "default_sip_port")]
    pub port: u16,
    /// Digest authentication credentials. If set, INVITEs are challenged with 401.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<PeerAuthConfig>,
    /// Allowed codecs (e.g., ["ulaw", "alaw"]). Empty means accept any.
    #[serde(default)]
    pub codecs: Vec<String>,
    /// Per-peer RTP address override. If set, SDP for calls from this peer
    /// uses this address instead of the server-level `rtp_address`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rtp_address: Option<IpAddr>,
}

fn default_sip_port() -> u16 {
    5060
}

/// Digest auth credentials for a peer.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PeerAuthConfig {
    pub username: String,
    #[serde(skip_serializing)]
    pub password: String,
}

impl PeerAuthConfig {
    pub fn new(username: impl Into<String>, password: impl Into<String>) -> Self {
        Self {
            username: username.into(),
            password: password.into(),
        }
    }
}

// ── Conversion to xphone types ──

impl ServerConfig {
    /// Convert to xphone::ServerConfig for use with xphone::Server.
    pub fn to_xphone(&self) -> xphone::ServerConfig {
        xphone::ServerConfig {
            listen: self.listen.clone(),
            peers: self.peers.iter().map(|p| p.to_xphone()).collect(),
            rtp_port_min: self.rtp_port_min,
            rtp_port_max: self.rtp_port_max,
            rtp_address: self.rtp_address,
            ..Default::default()
        }
    }
}

impl PeerConfig {
    fn to_xphone(&self) -> xphone::PeerConfig {
        xphone::PeerConfig {
            name: self.name.clone(),
            host: self.host,
            hosts: self.hosts.clone(),
            port: self.port,
            auth: self.auth.as_ref().map(|a| a.to_xphone()),
            codecs: self.codecs.clone(),
            rtp_address: self.rtp_address,
            ..Default::default()
        }
    }
}

impl PeerAuthConfig {
    fn to_xphone(&self) -> xphone::PeerAuthConfig {
        xphone::PeerAuthConfig::new(&self.username, &self.password)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[test]
    fn server_config_yaml_roundtrip() {
        let yaml = r#"
listen: "0.0.0.0:5080"
peers:
  - name: "office-pbx"
    host: "192.168.1.10"
    codecs: ["ulaw", "alaw"]
  - name: "remote-office"
    auth:
      username: "remote-trunk"
      password: "secret"
    codecs: ["ulaw"]
"#;
        let config: ServerConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.listen, "0.0.0.0:5080");
        assert_eq!(config.peers.len(), 2);

        let p0 = &config.peers[0];
        assert_eq!(p0.name, "office-pbx");
        assert_eq!(p0.host, Some(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 10))));
        assert!(p0.auth.is_none());
        assert_eq!(p0.codecs, vec!["ulaw", "alaw"]);

        let p1 = &config.peers[1];
        assert_eq!(p1.name, "remote-office");
        assert!(p1.host.is_none());
        let auth = p1.auth.as_ref().unwrap();
        assert_eq!(auth.username, "remote-trunk");
        assert_eq!(auth.password, "secret");
    }

    #[test]
    fn server_config_toml_roundtrip() {
        let toml = r#"
listen = "0.0.0.0:5080"

[[peers]]
name = "office-pbx"
host = "192.168.1.10"
codecs = ["ulaw", "alaw"]

[[peers]]
name = "remote-office"
codecs = ["ulaw"]

[peers.auth]
username = "remote-trunk"
password = "secret"
"#;
        let config: ServerConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.listen, "0.0.0.0:5080");
        assert_eq!(config.peers.len(), 2);
        assert_eq!(config.peers[0].name, "office-pbx");
        assert_eq!(
            config.peers[1].auth.as_ref().unwrap().username,
            "remote-trunk"
        );
    }

    #[test]
    fn server_config_no_peers() {
        let yaml = r#"
listen: "0.0.0.0:5080"
"#;
        let config: ServerConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(config.peers.is_empty());
    }

    #[test]
    fn password_excluded_from_serialization() {
        let config = ServerConfig {
            listen: "0.0.0.0:5080".into(),
            rtp_port_min: 0,
            rtp_port_max: 0,
            rtp_address: None,
            peers: vec![PeerConfig {
                name: "test".into(),
                host: None,
                hosts: vec![],
                port: 5060,
                auth: Some(PeerAuthConfig::new("user", "secret")),
                codecs: vec![],
                rtp_address: None,
            }],
        };
        let json = serde_json::to_value(&config).unwrap();
        let auth = &json["peers"][0]["auth"];
        assert_eq!(auth["username"], "user");
        assert!(auth.get("password").is_none());
    }

    #[test]
    fn ipv6_peer_host() {
        let yaml = r#"
listen: "[::]:5080"
peers:
  - name: "ipv6-pbx"
    host: "::1"
"#;
        let config: ServerConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(
            config.peers[0].host,
            Some(IpAddr::V6("::1".parse().unwrap()))
        );
    }

    #[test]
    fn to_xphone_converts_all_fields() {
        let config = ServerConfig {
            listen: "0.0.0.0:5080".into(),
            rtp_port_min: 10200,
            rtp_port_max: 10300,
            rtp_address: Some(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))),
            peers: vec![
                PeerConfig {
                    name: "pbx".into(),
                    host: Some(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 10))),
                    hosts: vec![],
                    port: 5060,
                    auth: None,
                    codecs: vec!["ulaw".into()],
                    rtp_address: None,
                },
                PeerConfig {
                    name: "twilio".into(),
                    host: None,
                    hosts: vec!["54.172.60.0/30".into()],
                    port: 5060,
                    auth: None,
                    codecs: vec![],
                    rtp_address: Some(IpAddr::V4(Ipv4Addr::new(1, 2, 3, 4))),
                },
            ],
        };

        let xp = config.to_xphone();
        assert_eq!(xp.listen, "0.0.0.0:5080");
        assert_eq!(xp.rtp_port_min, 10200);
        assert_eq!(xp.rtp_port_max, 10300);
        assert_eq!(xp.rtp_address, Some(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))));
        assert_eq!(xp.peers.len(), 2);
        assert_eq!(xp.peers[0].name, "pbx");
        assert_eq!(
            xp.peers[0].host,
            Some(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 10)))
        );
        assert_eq!(xp.peers[1].name, "twilio");
        assert_eq!(xp.peers[1].hosts, vec!["54.172.60.0/30"]);
        assert_eq!(
            xp.peers[1].rtp_address,
            Some(IpAddr::V4(Ipv4Addr::new(1, 2, 3, 4)))
        );
    }
}

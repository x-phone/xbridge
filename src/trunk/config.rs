use serde::{Deserialize, Serialize};
use std::net::IpAddr;

/// Configuration for the trunk host SIP server.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ServerConfig {
    /// Address to listen on (e.g., "0.0.0.0:5080").
    pub listen: String,
    /// Configured peers that are allowed to connect.
    #[serde(default)]
    pub peers: Vec<PeerConfig>,
}

/// A known SIP peer (PBX system) that can send/receive calls.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PeerConfig {
    /// Human-readable name for this peer (e.g., "office-pbx").
    pub name: String,
    /// IP address for IP-based authentication. If set, INVITEs from this IP are accepted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<IpAddr>,
    /// Digest authentication credentials. If set, INVITEs are challenged with 401.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<PeerAuthConfig>,
    /// Allowed codecs (e.g., ["ulaw", "alaw"]). Empty means accept any.
    #[serde(default)]
    pub codecs: Vec<String>,
}

/// Digest auth credentials for a peer.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PeerAuthConfig {
    pub username: String,
    #[serde(skip_serializing)]
    pub password: String,
}

impl PeerConfig {
    /// Returns true if this peer has at least one auth method configured.
    pub fn has_auth(&self) -> bool {
        self.host.is_some() || self.auth.is_some()
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
        assert_eq!(config.peers[1].auth.as_ref().unwrap().username, "remote-trunk");
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
    fn peer_has_auth_ip_only() {
        let peer = PeerConfig {
            name: "test".into(),
            host: Some(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))),
            auth: None,
            codecs: vec![],
        };
        assert!(peer.has_auth());
    }

    #[test]
    fn peer_has_auth_digest_only() {
        let peer = PeerConfig {
            name: "test".into(),
            host: None,
            auth: Some(PeerAuthConfig {
                username: "user".into(),
                password: "pass".into(),
            }),
            codecs: vec![],
        };
        assert!(peer.has_auth());
    }

    #[test]
    fn peer_has_auth_none() {
        let peer = PeerConfig {
            name: "test".into(),
            host: None,
            auth: None,
            codecs: vec![],
        };
        assert!(!peer.has_auth());
    }

    #[test]
    fn password_excluded_from_serialization() {
        let config = ServerConfig {
            listen: "0.0.0.0:5080".into(),
            peers: vec![PeerConfig {
                name: "test".into(),
                host: None,
                auth: Some(PeerAuthConfig {
                    username: "user".into(),
                    password: "secret".into(),
                }),
                codecs: vec![],
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
}

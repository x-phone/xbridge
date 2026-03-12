use std::fmt::Write;

/// Encode bytes as lowercase hex string.
pub(crate) fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{:02x}", b);
    }
    s
}

/// Generate a random SIP tag (16 hex chars).
pub(crate) fn generate_tag() -> String {
    use rand::Rng;
    let bytes: [u8; 8] = rand::rng().random();
    hex_encode(&bytes)
}

/// Generate a random SIP branch with the RFC 3261 magic cookie prefix.
pub(crate) fn generate_branch() -> String {
    use rand::Rng;
    let bytes: [u8; 12] = rand::rng().random();
    format!("z9hG4bK{}", hex_encode(&bytes))
}

/// Generate a random UUID v4 string.
pub(crate) fn uuid_v4() -> String {
    use rand::Rng;
    let bytes: [u8; 16] = rand::rng().random();
    format!(
        "{:08x}-{:04x}-4{:03x}-{:04x}-{:012x}",
        u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
        u16::from_be_bytes([bytes[4], bytes[5]]),
        u16::from_be_bytes([bytes[6], bytes[7]]) & 0x0FFF,
        (u16::from_be_bytes([bytes[8], bytes[9]]) & 0x3FFF) | 0x8000,
        u64::from_be_bytes([0, 0, bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15]]),
    )
}

/// Add a to-tag to a SIP To header if missing (for non-100 responses).
pub(crate) fn ensure_to_tag(to: &str, status_code: u16) -> String {
    if status_code > 100 && !to.contains("tag=") {
        format!("{to};tag={}", generate_tag())
    } else {
        to.to_string()
    }
}

/// Map a reject reason string to a SIP status code.
pub(crate) fn reject_reason_to_sip_code(reason: &str) -> u16 {
    match reason {
        "busy" => 486,
        "declined" => 603,
        _ => 486,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_encode_empty() {
        assert_eq!(hex_encode(&[]), "");
    }

    #[test]
    fn hex_encode_bytes() {
        assert_eq!(hex_encode(&[0x0a, 0xff, 0x00]), "0aff00");
    }

    #[test]
    fn tag_format() {
        let tag = generate_tag();
        assert_eq!(tag.len(), 16);
        assert!(tag.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn branch_format() {
        let branch = generate_branch();
        assert!(branch.starts_with("z9hG4bK"));
        assert_eq!(branch.len(), 7 + 24);
    }

    #[test]
    fn uuid_format() {
        let id = uuid_v4();
        assert_eq!(id.len(), 36);
        assert_eq!(id.chars().filter(|c| *c == '-').count(), 4);
        assert_eq!(id.chars().nth(14), Some('4'));
    }

    #[test]
    fn ensure_to_tag_adds_for_200() {
        let result = ensure_to_tag("<sip:1002@pbx>", 200);
        assert!(result.contains("tag="));
    }

    #[test]
    fn ensure_to_tag_skips_for_100() {
        let result = ensure_to_tag("<sip:1002@pbx>", 100);
        assert!(!result.contains("tag="));
    }

    #[test]
    fn ensure_to_tag_preserves_existing() {
        let result = ensure_to_tag("<sip:1002@pbx>;tag=existing", 200);
        assert!(result.contains("tag=existing"));
        // Should not have double tags.
        assert_eq!(result.matches("tag=").count(), 1);
    }

    #[test]
    fn reject_reason_codes() {
        assert_eq!(reject_reason_to_sip_code("busy"), 486);
        assert_eq!(reject_reason_to_sip_code("declined"), 603);
        assert_eq!(reject_reason_to_sip_code("anything-else"), 486);
    }
}

// G.711 mu-law codec and PCM16 byte conversion utilities.

// ── Mu-law encode/decode (ITU-T G.711) ──

const MULAW_BIAS: i32 = 0x84; // 132
const MULAW_CLIP: i32 = 32635;

#[rustfmt::skip]
const COMPRESS_TABLE: [u8; 256] = [
    0,0,1,1,2,2,2,2,3,3,3,3,3,3,3,3,
    4,4,4,4,4,4,4,4,4,4,4,4,4,4,4,4,
    5,5,5,5,5,5,5,5,5,5,5,5,5,5,5,5,
    5,5,5,5,5,5,5,5,5,5,5,5,5,5,5,5,
    6,6,6,6,6,6,6,6,6,6,6,6,6,6,6,6,
    6,6,6,6,6,6,6,6,6,6,6,6,6,6,6,6,
    6,6,6,6,6,6,6,6,6,6,6,6,6,6,6,6,
    6,6,6,6,6,6,6,6,6,6,6,6,6,6,6,6,
    7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,
    7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,
    7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,
    7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,
    7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,
    7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,
    7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,
    7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,
];

fn encode_sample(sample: i16) -> u8 {
    let sign;
    let mut val = sample as i32;

    if val < 0 {
        val = -val;
        sign = 0x80i32;
    } else {
        sign = 0;
    }

    if val > MULAW_CLIP {
        val = MULAW_CLIP;
    }
    val += MULAW_BIAS;

    let exponent = COMPRESS_TABLE[((val >> 7) & 0xFF) as usize] as i32;
    let mantissa = (val >> (exponent + 3)) & 0x0F;

    !(sign | (exponent << 4) | mantissa) as u8
}

fn decode_sample(mulaw: u8) -> i16 {
    let mu = !mulaw as u16;
    let sign = (mu & 0x80) != 0;
    let exponent = (mu >> 4) & 0x07;
    let mantissa = mu & 0x0F;

    let magnitude = ((mantissa << 1) | 0x21) << (exponent + 2);
    let sample = magnitude as i16 - MULAW_BIAS as i16;

    if sign {
        -sample
    } else {
        sample
    }
}

/// Encode PCM16 samples to mu-law bytes.
pub fn pcm16_to_mulaw(pcm: &[i16]) -> Vec<u8> {
    pcm.iter().map(|&s| encode_sample(s)).collect()
}

/// Encode PCM16 samples to mu-law bytes, reusing the output buffer.
pub fn pcm16_to_mulaw_into(pcm: &[i16], out: &mut Vec<u8>) {
    out.extend(pcm.iter().map(|&s| encode_sample(s)));
}

/// Decode mu-law bytes to PCM16 samples.
pub fn mulaw_to_pcm16(mulaw: &[u8]) -> Vec<i16> {
    mulaw.iter().map(|&b| decode_sample(b)).collect()
}

// ── PCM16 <-> bytes (little-endian) ──

/// Convert PCM16 samples to little-endian bytes.
pub fn pcm16_to_bytes(pcm: &[i16]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(pcm.len() * 2);
    for &sample in pcm {
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    bytes
}

/// Convert PCM16 samples to little-endian bytes, reusing the output buffer.
pub fn pcm16_to_bytes_into(pcm: &[i16], out: &mut Vec<u8>) {
    out.reserve(pcm.len() * 2);
    for &sample in pcm {
        out.extend_from_slice(&sample.to_le_bytes());
    }
}

/// Convert little-endian bytes to PCM16 samples.
pub fn bytes_to_pcm16(bytes: &[u8]) -> Vec<i16> {
    bytes
        .chunks_exact(2)
        .map(|chunk| i16::from_le_bytes([chunk[0], chunk[1]]))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mulaw_roundtrip_preserves_sign_and_magnitude() {
        // Test key PCM16 values through encode/decode roundtrip.
        // Mu-law is lossy, so we check the decoded value is close.
        for &original in &[0i16, 100, -100, 1000, -1000, 8000, -8000, 32000, -32000] {
            let encoded = encode_sample(original);
            let decoded = decode_sample(encoded);
            let error = (original as i32 - decoded as i32).unsigned_abs();

            // Mu-law quantization error grows with magnitude.
            // For small values (<256), error should be <=4.
            // For large values, relative error should be <4%.
            if original.unsigned_abs() < 256 {
                assert!(
                    error <= 4,
                    "sample={original}, decoded={decoded}, error={error}"
                );
            } else {
                let rel = error as f64 / original.unsigned_abs() as f64;
                assert!(
                    rel < 0.04,
                    "sample={original}, decoded={decoded}, rel_error={rel}"
                );
            }
        }
    }

    #[test]
    fn mulaw_silence_encodes_to_0xff() {
        // PCM silence (0) should encode to 0xFF in mu-law.
        assert_eq!(encode_sample(0), 0xFF);
    }

    #[test]
    fn mulaw_preserves_sign() {
        let pos = decode_sample(encode_sample(1000));
        let neg = decode_sample(encode_sample(-1000));
        assert!(pos > 0);
        assert!(neg < 0);
        assert_eq!(pos, -neg);
    }

    #[test]
    fn pcm16_to_mulaw_batch() {
        let pcm = vec![0i16, 100, -100, 8000, -8000];
        let mulaw = pcm16_to_mulaw(&pcm);
        assert_eq!(mulaw.len(), 5);
        let back = mulaw_to_pcm16(&mulaw);
        assert_eq!(back.len(), 5);
        // Silence roundtrips exactly
        assert_eq!(back[0], decode_sample(0xFF));
    }

    #[test]
    fn pcm16_bytes_roundtrip() {
        let pcm = vec![0i16, 1, -1, 32767, -32768, 12345];
        let bytes = pcm16_to_bytes(&pcm);
        assert_eq!(bytes.len(), 12);
        let back = bytes_to_pcm16(&bytes);
        assert_eq!(pcm, back);
    }

    #[test]
    fn pcm16_bytes_little_endian() {
        let pcm = vec![0x0102i16];
        let bytes = pcm16_to_bytes(&pcm);
        assert_eq!(bytes, vec![0x02, 0x01]); // LE
    }

    #[test]
    fn bytes_to_pcm16_ignores_trailing_byte() {
        let bytes = vec![0x00, 0x01, 0xFF]; // 3 bytes -> 1 sample, trailing ignored
        let pcm = bytes_to_pcm16(&bytes);
        assert_eq!(pcm.len(), 1);
        assert_eq!(pcm[0], 0x0100);
    }

    #[test]
    fn mulaw_full_range_no_panic() {
        // Encode every possible PCM16 value.
        for i in i16::MIN..=i16::MAX {
            let _ = encode_sample(i);
        }
        // Decode every possible mu-law byte.
        for b in 0u8..=255 {
            let _ = decode_sample(b);
        }
    }
}

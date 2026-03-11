//! Minimal WAV file parser — extracts raw PCM16 data from RIFF/WAVE files.

use std::fmt;

#[derive(Debug)]
pub struct WavHeader {
    pub channels: u16,
    pub sample_rate: u32,
    pub bits_per_sample: u16,
}

#[derive(Debug)]
pub enum WavError {
    TooShort,
    InvalidRiff,
    InvalidWave,
    MissingFmt,
    MissingData,
    UnsupportedFormat(u16),
    NotMono,
    Not8kHz(u32),
    Not16Bit(u16),
}

impl fmt::Display for WavError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooShort => write!(f, "WAV file too short"),
            Self::InvalidRiff => write!(f, "not a RIFF file"),
            Self::InvalidWave => write!(f, "not a WAVE file"),
            Self::MissingFmt => write!(f, "missing fmt chunk"),
            Self::MissingData => write!(f, "missing data chunk"),
            Self::UnsupportedFormat(tag) => {
                write!(f, "unsupported audio format: {tag} (expected PCM/1)")
            }
            Self::NotMono => write!(f, "only mono audio supported"),
            Self::Not8kHz(rate) => write!(f, "unsupported sample rate: {rate}Hz (expected 8000)"),
            Self::Not16Bit(bits) => write!(f, "unsupported bit depth: {bits} (expected 16)"),
        }
    }
}

impl std::error::Error for WavError {}

/// Parse a WAV file and return the header + raw PCM data bytes.
pub fn parse_wav(bytes: &[u8]) -> Result<(WavHeader, &[u8]), WavError> {
    if bytes.len() < 12 {
        return Err(WavError::TooShort);
    }

    // RIFF header
    if &bytes[0..4] != b"RIFF" {
        return Err(WavError::InvalidRiff);
    }
    if &bytes[8..12] != b"WAVE" {
        return Err(WavError::InvalidWave);
    }

    let mut pos = 12;
    let mut header: Option<WavHeader> = None;
    let mut data: Option<&[u8]> = None;

    // Walk chunks
    while pos + 8 <= bytes.len() {
        let chunk_id = &bytes[pos..pos + 4];
        let chunk_size = u32::from_le_bytes([
            bytes[pos + 4],
            bytes[pos + 5],
            bytes[pos + 6],
            bytes[pos + 7],
        ]) as usize;
        let chunk_data_start = pos + 8;

        if chunk_id == b"fmt " {
            if chunk_size < 16 || chunk_data_start + 16 > bytes.len() {
                return Err(WavError::TooShort);
            }
            let d = &bytes[chunk_data_start..];
            let audio_format = u16::from_le_bytes([d[0], d[1]]);
            if audio_format != 1 {
                return Err(WavError::UnsupportedFormat(audio_format));
            }
            header = Some(WavHeader {
                channels: u16::from_le_bytes([d[2], d[3]]),
                sample_rate: u32::from_le_bytes([d[4], d[5], d[6], d[7]]),
                bits_per_sample: u16::from_le_bytes([d[14], d[15]]),
            });
        } else if chunk_id == b"data" {
            let end = (chunk_data_start + chunk_size).min(bytes.len());
            data = Some(&bytes[chunk_data_start..end]);
        }

        // Advance to next chunk (chunks are word-aligned)
        pos = chunk_data_start + chunk_size;
        if pos % 2 != 0 {
            pos += 1;
        }
    }

    let header = header.ok_or(WavError::MissingFmt)?;
    let data = data.ok_or(WavError::MissingData)?;

    Ok((header, data))
}

/// Validate that the WAV is 8kHz mono 16-bit PCM (what xphone expects).
pub fn ensure_8khz_mono_16bit(header: &WavHeader) -> Result<(), WavError> {
    if header.channels != 1 {
        return Err(WavError::NotMono);
    }
    if header.sample_rate != 8000 {
        return Err(WavError::Not8kHz(header.sample_rate));
    }
    if header.bits_per_sample != 16 {
        return Err(WavError::Not16Bit(header.bits_per_sample));
    }
    Ok(())
}

/// Build a minimal WAV file from raw PCM16 mono 8kHz data.
#[cfg(test)]
fn build_test_wav(pcm_data: &[u8]) -> Vec<u8> {
    let data_size = pcm_data.len() as u32;
    let file_size = 36 + data_size;
    let mut wav = Vec::with_capacity(44 + pcm_data.len());

    // RIFF header
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&file_size.to_le_bytes());
    wav.extend_from_slice(b"WAVE");

    // fmt chunk
    wav.extend_from_slice(b"fmt ");
    wav.extend_from_slice(&16u32.to_le_bytes()); // chunk size
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM format
    wav.extend_from_slice(&1u16.to_le_bytes()); // mono
    wav.extend_from_slice(&8000u32.to_le_bytes()); // sample rate
    wav.extend_from_slice(&16000u32.to_le_bytes()); // byte rate
    wav.extend_from_slice(&2u16.to_le_bytes()); // block align
    wav.extend_from_slice(&16u16.to_le_bytes()); // bits per sample

    // data chunk
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_size.to_le_bytes());
    wav.extend_from_slice(pcm_data);

    wav
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_valid_wav() {
        let pcm = vec![0x01, 0x00, 0xFF, 0x7F]; // 2 samples
        let wav = build_test_wav(&pcm);

        let (header, data) = parse_wav(&wav).unwrap();
        assert_eq!(header.channels, 1);
        assert_eq!(header.sample_rate, 8000);
        assert_eq!(header.bits_per_sample, 16);
        assert_eq!(data, &pcm);
    }

    #[test]
    fn parse_validates_8khz_mono_16bit() {
        let pcm = vec![0x00; 320];
        let wav = build_test_wav(&pcm);

        let (header, _) = parse_wav(&wav).unwrap();
        ensure_8khz_mono_16bit(&header).unwrap();
    }

    #[test]
    fn parse_empty_data() {
        let wav = build_test_wav(&[]);
        let (_, data) = parse_wav(&wav).unwrap();
        assert!(data.is_empty());
    }

    #[test]
    fn parse_rejects_too_short() {
        assert!(matches!(parse_wav(&[0; 5]), Err(WavError::TooShort)));
    }

    #[test]
    fn parse_rejects_invalid_riff() {
        let mut wav = build_test_wav(&[0; 4]);
        wav[0..4].copy_from_slice(b"XXXX");
        assert!(matches!(parse_wav(&wav), Err(WavError::InvalidRiff)));
    }

    #[test]
    fn parse_rejects_invalid_wave() {
        let mut wav = build_test_wav(&[0; 4]);
        wav[8..12].copy_from_slice(b"XXXX");
        assert!(matches!(parse_wav(&wav), Err(WavError::InvalidWave)));
    }

    #[test]
    fn parse_rejects_non_pcm_format() {
        let mut wav = build_test_wav(&[0; 4]);
        // Set audio format to 3 (IEEE float) instead of 1 (PCM)
        wav[20..22].copy_from_slice(&3u16.to_le_bytes());
        assert!(matches!(
            parse_wav(&wav),
            Err(WavError::UnsupportedFormat(3))
        ));
    }

    #[test]
    fn ensure_rejects_stereo() {
        let header = WavHeader {
            channels: 2,
            sample_rate: 8000,
            bits_per_sample: 16,
        };
        assert!(matches!(
            ensure_8khz_mono_16bit(&header),
            Err(WavError::NotMono)
        ));
    }

    #[test]
    fn ensure_rejects_wrong_sample_rate() {
        let header = WavHeader {
            channels: 1,
            sample_rate: 44100,
            bits_per_sample: 16,
        };
        assert!(matches!(
            ensure_8khz_mono_16bit(&header),
            Err(WavError::Not8kHz(44100))
        ));
    }

    #[test]
    fn ensure_rejects_wrong_bit_depth() {
        let header = WavHeader {
            channels: 1,
            sample_rate: 8000,
            bits_per_sample: 8,
        };
        assert!(matches!(
            ensure_8khz_mono_16bit(&header),
            Err(WavError::Not16Bit(8))
        ));
    }

    #[test]
    fn parse_wav_with_extra_chunks() {
        // Build a WAV with an extra "LIST" chunk between fmt and data
        let pcm = vec![0x01, 0x00, 0x02, 0x00];

        let mut wav = Vec::new();
        // RIFF header (size placeholder)
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&0u32.to_le_bytes()); // fill later
        wav.extend_from_slice(b"WAVE");

        // fmt chunk
        wav.extend_from_slice(b"fmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&8000u32.to_le_bytes());
        wav.extend_from_slice(&16000u32.to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());

        // Extra chunk
        wav.extend_from_slice(b"LIST");
        wav.extend_from_slice(&4u32.to_le_bytes());
        wav.extend_from_slice(b"INFO");

        // data chunk
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&(pcm.len() as u32).to_le_bytes());
        wav.extend_from_slice(&pcm);

        // Fix file size
        let file_size = (wav.len() - 8) as u32;
        wav[4..8].copy_from_slice(&file_size.to_le_bytes());

        let (header, data) = parse_wav(&wav).unwrap();
        assert_eq!(header.channels, 1);
        assert_eq!(data, &pcm);
    }
}

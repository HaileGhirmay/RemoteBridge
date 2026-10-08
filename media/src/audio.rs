//! Audio: encoder trait.
//!
//! Browsers expect Opus. The Opus encoder itself needs libopus (a native
//! build), so it plugs in here behind [`AudioEncoder`] instead of being a hard
//! dependency of this crate. [`SilentOpusEncoder`] emits valid Opus silence
//! frames; it keeps the track alive in tests and is never used for real audio.

use std::time::Duration;

use rb_core::types::AudioChunk;

use crate::error::MediaError;

/// One encoded packet and the time it covers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodedAudio {
    pub data: Vec<u8>,
    pub duration: Duration,
}

pub trait AudioEncoder: Send {
    /// Turn a chunk of PCM into zero or more packets (Opus uses 20 ms frames).
    fn encode(&mut self, chunk: &AudioChunk) -> Result<Vec<EncodedAudio>, MediaError>;
}

/// A 20 ms Opus frame of silence (a TOC byte plus two bytes: a valid CELT frame).
pub const OPUS_SILENCE: [u8; 3] = [0xF8, 0xFF, 0xFE];

/// Emits one Opus silence frame per 20 ms of input. For tests only.
#[derive(Debug, Default)]
pub struct SilentOpusEncoder {
    carry: Duration,
}

impl AudioEncoder for SilentOpusEncoder {
    fn encode(&mut self, chunk: &AudioChunk) -> Result<Vec<EncodedAudio>, MediaError> {
        if chunk.sample_rate == 0 || chunk.channels == 0 {
            return Err(MediaError::Encoder("invalid audio format".into()));
        }
        let frames = chunk.samples.len() as u64 / u64::from(chunk.channels);
        let covered = Duration::from_micros(frames * 1_000_000 / u64::from(chunk.sample_rate));
        self.carry += covered;
        let frame = Duration::from_millis(20);
        let mut out = Vec::new();
        while self.carry >= frame {
            self.carry -= frame;
            out.push(EncodedAudio {
                data: OPUS_SILENCE.to_vec(),
                duration: frame,
            });
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(frames: usize) -> AudioChunk {
        AudioChunk {
            sample_rate: 48_000,
            channels: 2,
            timestamp_us: 0,
            samples: vec![0; frames * 2],
        }
    }

    #[test]
    fn one_frame_per_twenty_milliseconds() {
        let mut enc = SilentOpusEncoder::default();
        // 10 ms of audio is not enough for a frame yet.
        assert!(enc.encode(&chunk(480)).unwrap().is_empty());
        // Another 10 ms completes the first one.
        let out = enc.encode(&chunk(480)).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].duration, Duration::from_millis(20));
        assert_eq!(out[0].data, OPUS_SILENCE);
        // 60 ms gives three.
        assert_eq!(enc.encode(&chunk(2880)).unwrap().len(), 3);
    }

    #[test]
    fn invalid_formats_are_refused() {
        let mut enc = SilentOpusEncoder::default();
        let mut bad = chunk(480);
        bad.sample_rate = 0;
        assert!(enc.encode(&bad).is_err());
        let mut bad = chunk(480);
        bad.channels = 0;
        assert!(enc.encode(&bad).is_err());
    }
}

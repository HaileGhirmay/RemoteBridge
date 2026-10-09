//! Real Opus audio for the host, through libopus.
//!
//! Browsers decode 48 kHz Opus. This encoder takes 48 kHz PCM, mono or stereo,
//! buffers it into 20 ms frames, and emits one Opus packet per frame.
//!
//! Other sample rates and channel layouts are refused, not converted. A track
//! that is quietly resampled badly would click, and a silent one would hide the
//! problem; an error in the log says what to fix.

use std::time::Duration;

use rb_core::types::AudioChunk;

use crate::audio::{AudioEncoder, EncodedAudio};
use crate::error::MediaError;

/// The only sample rate browsers' Opus decoders are asked for here.
pub const SAMPLE_RATE: u32 = 48_000;

/// One Opus packet covers this much audio.
const FRAME: Duration = Duration::from_millis(20);

/// 20 ms at 48 kHz, per channel.
const FRAME_SAMPLES: usize = 960;

/// Upper bound for one packet (libopus recommends 4000 bytes).
const MAX_PACKET: usize = 4000;

/// Enough for music and speech over a home connection.
const BITRATE_BITS: i32 = 96_000;

pub struct OpusAudioEncoder {
    /// Created on the first chunk, once the channel count is known.
    encoder: Option<Encoder>,
    /// Interleaved samples not yet forming a whole frame.
    pending: Vec<i16>,
}

struct Encoder {
    inner: opus::Encoder,
    channels: u16,
}

impl OpusAudioEncoder {
    pub fn new() -> Self {
        Self {
            encoder: None,
            pending: Vec::new(),
        }
    }

    fn ensure_encoder(&mut self, channels: u16) -> Result<(), MediaError> {
        if let Some(existing) = &self.encoder
            && existing.channels != channels
        {
            return Err(MediaError::Encoder(
                "the audio channel count changed mid-stream".into(),
            ));
        }
        if self.encoder.is_none() {
            let layout = match channels {
                1 => opus::Channels::Mono,
                2 => opus::Channels::Stereo,
                n => {
                    return Err(MediaError::Encoder(format!(
                        "{n} audio channels; Opus here takes mono or stereo"
                    )));
                }
            };
            let mut inner = opus::Encoder::new(SAMPLE_RATE, layout, opus::Application::Audio)
                .map_err(opus_error)?;
            inner
                .set_bitrate(opus::Bitrate::Bits(BITRATE_BITS))
                .map_err(opus_error)?;
            self.encoder = Some(Encoder { inner, channels });
        }
        Ok(())
    }
}

impl Default for OpusAudioEncoder {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioEncoder for OpusAudioEncoder {
    fn encode(&mut self, chunk: &AudioChunk) -> Result<Vec<EncodedAudio>, MediaError> {
        if chunk.sample_rate != SAMPLE_RATE {
            return Err(MediaError::Encoder(format!(
                "audio at {} Hz; Opus needs {SAMPLE_RATE} Hz",
                chunk.sample_rate
            )));
        }
        self.ensure_encoder(chunk.channels)?;
        self.pending.extend_from_slice(&chunk.samples);
        let Some(encoder) = self.encoder.as_mut() else {
            return Err(MediaError::Encoder("opus encoder missing".into()));
        };

        let frame_len = FRAME_SAMPLES * usize::from(chunk.channels);
        let mut out = Vec::new();
        let mut packet = vec![0u8; MAX_PACKET];
        let mut consumed = 0;
        while self.pending.len() - consumed >= frame_len {
            let frame = &self.pending[consumed..consumed + frame_len];
            let written = encoder
                .inner
                .encode(frame, &mut packet)
                .map_err(opus_error)?;
            out.push(EncodedAudio {
                data: packet[..written].to_vec(),
                duration: FRAME,
            });
            consumed += frame_len;
        }
        self.pending.drain(..consumed);
        Ok(out)
    }
}

fn opus_error(e: opus::Error) -> MediaError {
    MediaError::Encoder(format!("opus: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(channels: u16, frames: usize, sample_rate: u32, tone: bool) -> AudioChunk {
        let mut samples = Vec::with_capacity(frames * usize::from(channels));
        for i in 0..frames {
            // A 440 Hz tone at moderate level, or silence.
            let v = if tone {
                let t = i as f32 / sample_rate as f32;
                ((t * 440.0 * std::f32::consts::TAU).sin() * 8000.0) as i16
            } else {
                0
            };
            for _ in 0..channels {
                samples.push(v);
            }
        }
        AudioChunk {
            sample_rate,
            channels,
            timestamp_us: 0,
            samples,
        }
    }

    #[test]
    fn one_packet_per_twenty_milliseconds_of_audio() {
        let mut enc = OpusAudioEncoder::new();
        // 10 ms is not a frame yet.
        assert!(
            enc.encode(&chunk(2, 480, SAMPLE_RATE, false))
                .unwrap()
                .is_empty()
        );
        // The second 10 ms completes one.
        let out = enc.encode(&chunk(2, 480, SAMPLE_RATE, false)).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].duration, FRAME);
        assert!(!out[0].data.is_empty());
        // 60 ms gives three more.
        assert_eq!(
            enc.encode(&chunk(2, 2880, SAMPLE_RATE, false))
                .unwrap()
                .len(),
            3
        );
    }

    #[test]
    fn packets_decode_back_to_the_tone_they_came_from() {
        let mut enc = OpusAudioEncoder::new();
        let packets = enc.encode(&chunk(2, 4800, SAMPLE_RATE, true)).unwrap();
        assert_eq!(packets.len(), 5, "100 ms is five frames");

        let mut decoder = opus::Decoder::new(SAMPLE_RATE, opus::Channels::Stereo).unwrap();
        let mut pcm = vec![0i16; FRAME_SAMPLES * 2];
        let mut energy = 0u64;
        for packet in &packets {
            let frames = decoder.decode(&packet.data, &mut pcm, false).unwrap();
            assert_eq!(frames, FRAME_SAMPLES, "each packet is exactly 20 ms");
            energy += pcm.iter().map(|s| u64::from(s.unsigned_abs())).sum::<u64>();
        }
        assert!(energy > 0, "the decoded audio is not silence");
    }

    #[test]
    fn mono_is_encoded_too() {
        let mut enc = OpusAudioEncoder::new();
        let out = enc.encode(&chunk(1, 960, SAMPLE_RATE, true)).unwrap();
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn other_sample_rates_are_refused_not_converted() {
        let mut enc = OpusAudioEncoder::new();
        let err = enc.encode(&chunk(2, 441, 44_100, true)).unwrap_err();
        assert!(err.to_string().contains("44100"), "{err}");
    }

    #[test]
    fn more_than_two_channels_are_refused() {
        let mut enc = OpusAudioEncoder::new();
        assert!(enc.encode(&chunk(6, 960, SAMPLE_RATE, false)).is_err());
    }

    #[test]
    fn a_channel_count_change_mid_stream_is_refused() {
        let mut enc = OpusAudioEncoder::new();
        enc.encode(&chunk(2, 480, SAMPLE_RATE, false)).unwrap();
        assert!(enc.encode(&chunk(1, 480, SAMPLE_RATE, false)).is_err());
    }
}

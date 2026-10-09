//! Real Opus audio for the host, through libopus.
//!
//! Browsers decode 48 kHz Opus. This encoder takes PCM at any common sample
//! rate, mono or stereo, converts it to 48 kHz, buffers it into 20 ms frames,
//! and emits one Opus packet per frame.
//!
//! Conversion is linear interpolation, carried across chunks so the output does
//! not depend on how the input is split. That is fine for speech and typical
//! desktop audio; it is not studio quality. More than two channels is refused.

use std::time::Duration;

use rb_core::types::AudioChunk;

use crate::audio::{AudioEncoder, EncodedAudio};
use crate::error::MediaError;

/// The sample rate Opus is encoded at, and the rate browsers decode.
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
    /// Set when the input is not already 48 kHz.
    resampler: Option<Resampler>,
    /// 48 kHz interleaved samples not yet forming a whole frame.
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
            resampler: None,
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

    /// The samples at 48 kHz. Input already at 48 kHz is passed through.
    fn convert(&mut self, chunk: &AudioChunk) -> Result<Vec<i16>, MediaError> {
        if chunk.sample_rate == SAMPLE_RATE {
            return Ok(chunk.samples.clone());
        }
        let stale = match &self.resampler {
            Some(r) => r.rate != chunk.sample_rate || r.channels != usize::from(chunk.channels),
            None => true,
        };
        if stale {
            // A rate change starts a new converter; the seam is a small click at most.
            self.resampler = Some(Resampler::new(
                chunk.sample_rate,
                usize::from(chunk.channels),
            ));
        }
        match self.resampler.as_mut() {
            Some(r) => Ok(r.push(&chunk.samples)),
            None => Err(MediaError::Encoder("resampler missing".into())),
        }
    }
}

impl Default for OpusAudioEncoder {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioEncoder for OpusAudioEncoder {
    fn encode(&mut self, chunk: &AudioChunk) -> Result<Vec<EncodedAudio>, MediaError> {
        if chunk.sample_rate == 0 {
            return Err(MediaError::Encoder("audio sample rate is 0".into()));
        }
        self.ensure_encoder(chunk.channels)?;
        let samples = self.convert(chunk)?;
        self.pending.extend_from_slice(&samples);
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

/// Linear-interpolation sample-rate converter to [`SAMPLE_RATE`].
///
/// It keeps the last input frame it needs between calls, so splitting the input
/// into chunks gives the same output as converting it in one piece.
struct Resampler {
    rate: u32,
    channels: usize,
    /// Input frames per output frame.
    step: f64,
    /// Interleaved input frames. The first one is the frame `pos` is inside.
    buf: Vec<f32>,
    /// Read position in frames, relative to the start of `buf`.
    pos: f64,
}

impl Resampler {
    fn new(rate: u32, channels: usize) -> Self {
        Self {
            rate,
            channels,
            step: f64::from(rate) / f64::from(SAMPLE_RATE),
            buf: Vec::new(),
            pos: 0.0,
        }
    }

    fn push(&mut self, input: &[i16]) -> Vec<i16> {
        self.buf.extend(input.iter().map(|&s| f32::from(s)));
        let c = self.channels;
        let frames = self.buf.len() / c;
        let mut out = Vec::new();
        while self.pos + 1.0 < frames as f64 {
            let i = self.pos as usize;
            let frac = (self.pos - i as f64) as f32;
            for ch in 0..c {
                let a = self.buf[i * c + ch];
                let b = self.buf[(i + 1) * c + ch];
                let v = a + (b - a) * frac;
                out.push(v.round().clamp(-32768.0, 32767.0) as i16);
            }
            self.pos += self.step;
        }
        // Frames before `pos` are no longer needed; keep the one `pos` is in.
        let consumed = self.pos as usize;
        if consumed > 0 {
            self.buf.drain(..consumed * c);
            self.pos -= consumed as f64;
        }
        out
    }
}

fn opus_error(e: opus::Error) -> MediaError {
    MediaError::Encoder(format!("opus: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone_samples(channels: u16, frames: usize, sample_rate: u32, amplitude: f32) -> Vec<i16> {
        let mut samples = Vec::with_capacity(frames * usize::from(channels));
        for i in 0..frames {
            let t = i as f32 / sample_rate as f32;
            let v = ((t * 440.0 * std::f32::consts::TAU).sin() * amplitude) as i16;
            for _ in 0..channels {
                samples.push(v);
            }
        }
        samples
    }

    fn chunk(channels: u16, frames: usize, sample_rate: u32, tone: bool) -> AudioChunk {
        let samples = if tone {
            tone_samples(channels, frames, sample_rate, 8000.0)
        } else {
            vec![0; frames * usize::from(channels)]
        };
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
    fn a_44_1_khz_stream_is_converted_and_encoded() {
        // 100 ms at 44.1 kHz is 4410 frames, about 4800 at 48 kHz: four or five packets.
        let mut enc = OpusAudioEncoder::new();
        let packets = enc.encode(&chunk(2, 4410, 44_100, true)).unwrap();
        assert!(
            (4..=5).contains(&packets.len()),
            "got {} packets",
            packets.len()
        );
        let mut decoder = opus::Decoder::new(SAMPLE_RATE, opus::Channels::Stereo).unwrap();
        let mut pcm = vec![0i16; FRAME_SAMPLES * 2];
        let mut energy = 0u64;
        for packet in &packets {
            decoder.decode(&packet.data, &mut pcm, false).unwrap();
            energy += pcm.iter().map(|s| u64::from(s.unsigned_abs())).sum::<u64>();
        }
        assert!(energy > 0, "the converted audio is not silence");
    }

    #[test]
    fn resampling_keeps_length_and_loudness() {
        let mut resampler = Resampler::new(44_100, 1);
        let input = tone_samples(1, 44_100, 44_100, 8000.0);
        let out = resampler.push(&input);
        // One second in is one second out, within a frame of rounding.
        assert!(
            (out.len() as i64 - 48_000).abs() <= 2,
            "got {} samples",
            out.len()
        );
        let peak = out.iter().map(|s| i32::from(*s).abs()).max().unwrap();
        assert!(
            (7800..=8100).contains(&peak),
            "peak {peak} should stay near the input's 8000"
        );
    }

    #[test]
    fn chunked_and_whole_input_convert_the_same() {
        let input = tone_samples(2, 10_000, 44_100, 6000.0);

        let mut whole = Resampler::new(44_100, 2);
        let expected = whole.push(&input);

        let mut chunked = Resampler::new(44_100, 2);
        let mut got = Vec::new();
        for piece in input.chunks(2 * 333) {
            got.extend(chunked.push(piece));
        }

        assert_eq!(got.len(), expected.len());
        let worst = got
            .iter()
            .zip(&expected)
            .map(|(a, b)| (i32::from(*a) - i32::from(*b)).abs())
            .max()
            .unwrap_or(0);
        assert!(worst <= 1, "chunking changed the audio by {worst} steps");
    }

    #[test]
    fn more_than_two_channels_are_refused() {
        let mut enc = OpusAudioEncoder::new();
        assert!(enc.encode(&chunk(6, 960, SAMPLE_RATE, false)).is_err());
    }

    #[test]
    fn a_zero_sample_rate_is_refused() {
        let mut enc = OpusAudioEncoder::new();
        let mut bad = chunk(2, 480, SAMPLE_RATE, false);
        bad.sample_rate = 0;
        assert!(enc.encode(&bad).is_err());
    }

    #[test]
    fn a_channel_count_change_mid_stream_is_refused() {
        let mut enc = OpusAudioEncoder::new();
        enc.encode(&chunk(2, 480, SAMPLE_RATE, false)).unwrap();
        assert!(enc.encode(&chunk(1, 480, SAMPLE_RATE, false)).is_err());
    }
}

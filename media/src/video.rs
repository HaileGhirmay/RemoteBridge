//! Video: H.264 encoding and adaptive bitrate.
//!
//! The encoder is software OpenH264 (Cisco's BSD-licensed implementation,
//! built from source). A hardware encoder (Media Foundation, VideoToolbox) can
//! implement [`VideoEncoder`] later; nothing else changes. The frame rate cap
//! (30 fps) lives in the media session, not here.

use std::time::Duration;

use openh264::OpenH264API;
use openh264::encoder::{
    BitRate, Encoder, EncoderConfig, FrameRate, FrameType, Profile, RateControlMode, UsageType,
};
use openh264::formats::{RgbSliceU8, YUVBuffer};
use rb_core::types::{FrameData, VideoFrame};

use crate::error::MediaError;

/// One encoded frame, in Annex B form (start codes), ready for the packetizer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodedVideo {
    pub data: Vec<u8>,
    pub keyframe: bool,
}

pub trait VideoEncoder: Send {
    /// `Ok(None)` when the encoder chose to skip this frame.
    fn encode(&mut self, frame: &VideoFrame) -> Result<Option<EncodedVideo>, MediaError>;
    /// Change the target bitrate (bits per second).
    fn set_bitrate(&mut self, bps: u32) -> Result<(), MediaError>;
    fn bitrate(&self) -> u32;
    /// Make the next frame a keyframe (a viewer joined or lost packets).
    fn request_keyframe(&mut self);
}

/// Software H.264, constrained baseline (`profile-level-id=42e01f`), tuned for screen content.
pub struct OpenH264Encoder {
    encoder: Option<Encoder>,
    size: (u32, u32),
    bitrate: u32,
    max_fps: f32,
    force_key: bool,
}

impl OpenH264Encoder {
    pub const DEFAULT_BITRATE: u32 = 2_000_000;

    pub fn new(bitrate: u32, max_fps: f32) -> Self {
        Self {
            encoder: None,
            size: (0, 0),
            bitrate,
            max_fps,
            force_key: false,
        }
    }

    fn build(&self) -> Result<Encoder, MediaError> {
        let config = EncoderConfig::new()
            .bitrate(BitRate::from_bps(self.bitrate))
            .max_frame_rate(FrameRate::from_hz(self.max_fps))
            .usage_type(UsageType::ScreenContentRealTime)
            .rate_control_mode(RateControlMode::Bitrate)
            .profile(Profile::Baseline);
        Encoder::with_api_config(OpenH264API::from_source(), config)
            .map_err(|e| MediaError::Encoder(e.to_string()))
    }
}

impl Default for OpenH264Encoder {
    fn default() -> Self {
        Self::new(Self::DEFAULT_BITRATE, 30.0)
    }
}

/// BGRA to packed RGB. Alpha is dropped.
fn bgra_to_rgb(bgra: &[u8]) -> Vec<u8> {
    let mut rgb = Vec::with_capacity(bgra.len() / 4 * 3);
    for px in bgra.as_chunks::<4>().0 {
        rgb.extend_from_slice(&[px[2], px[1], px[0]]);
    }
    rgb
}

impl VideoEncoder for OpenH264Encoder {
    fn encode(&mut self, frame: &VideoFrame) -> Result<Option<EncodedVideo>, MediaError> {
        let FrameData::Bgra(pixels) = &frame.data else {
            return Err(MediaError::Encoder(
                "native frames need a hardware encoder".into(),
            ));
        };
        let (w, h) = (frame.width, frame.height);
        if w == 0 || h == 0 || w % 2 != 0 || h % 2 != 0 {
            return Err(MediaError::Encoder(format!(
                "frame size {w}x{h} must be non-zero and even"
            )));
        }
        if pixels.len() != w as usize * h as usize * 4 {
            return Err(MediaError::Encoder(
                "pixel buffer does not match the frame size".into(),
            ));
        }
        if self.encoder.is_none() || self.size != (w, h) {
            self.encoder = Some(self.build()?);
            self.size = (w, h);
            self.force_key = true; // a new stream always starts with a keyframe
        }
        let rgb = bgra_to_rgb(pixels);
        let yuv = YUVBuffer::from_rgb_source(RgbSliceU8::new(&rgb, (w as usize, h as usize)));
        let encoder = self.encoder.as_mut().expect("just built");
        if std::mem::take(&mut self.force_key) {
            encoder.force_intra_frame();
        }
        let stream = encoder
            .encode(&yuv)
            .map_err(|e| MediaError::Encoder(e.to_string()))?;
        if matches!(stream.frame_type(), FrameType::Skip | FrameType::Invalid) {
            return Ok(None);
        }
        let keyframe = matches!(stream.frame_type(), FrameType::IDR | FrameType::I);
        Ok(Some(EncodedVideo {
            data: stream.to_vec(),
            keyframe,
        }))
    }

    fn set_bitrate(&mut self, bps: u32) -> Result<(), MediaError> {
        if bps != self.bitrate {
            self.bitrate = bps;
            // Rebuild on the next frame with the new target. The stream restarts
            // with a keyframe, which every decoder handles.
            self.encoder = None;
            self.force_key = true;
        }
        Ok(())
    }

    fn bitrate(&self) -> u32 {
        self.bitrate
    }

    fn request_keyframe(&mut self) {
        self.force_key = true;
    }
}

/// What the receiver reports back (from RTCP receiver reports).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NetworkReport {
    /// Fraction of packets lost since the last report, 0.0 to 1.0.
    pub loss_fraction: f64,
    /// Round-trip time in milliseconds.
    pub rtt_ms: f64,
}

/// Adaptive bitrate: back off fast on loss, probe up slowly when it is clean.
/// Pure, so it is unit-tested without a network.
#[derive(Debug, Clone)]
pub struct BitrateController {
    target: u32,
    min: u32,
    max: u32,
    last_change: Option<Duration>,
}

impl BitrateController {
    pub const MIN_BPS: u32 = 300_000;
    pub const MAX_BPS: u32 = 8_000_000;
    /// How often the target may move.
    const HOLD: Duration = Duration::from_secs(2);

    pub fn new(start: u32) -> Self {
        Self {
            target: start.clamp(Self::MIN_BPS, Self::MAX_BPS),
            min: Self::MIN_BPS,
            max: Self::MAX_BPS,
            last_change: None,
        }
    }

    pub fn target(&self) -> u32 {
        self.target
    }

    /// Feed a report. Returns the new target when it changed.
    pub fn update(&mut self, now: Duration, report: NetworkReport) -> Option<u32> {
        if let Some(last) = self.last_change
            && now.saturating_sub(last) < Self::HOLD
        {
            return None;
        }
        let next = if report.loss_fraction > 0.10 {
            f64::from(self.target) * 0.80
        } else if report.loss_fraction < 0.02 && report.rtt_ms < 300.0 {
            f64::from(self.target) * 1.08
        } else {
            return None;
        };
        let next = (next as u32).clamp(self.min, self.max);
        if next == self.target {
            return None;
        }
        self.target = next;
        self.last_change = Some(now);
        Some(next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(w: u32, h: u32, shade: u8) -> VideoFrame {
        let mut px = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                px.extend_from_slice(&[
                    shade.wrapping_add((x % 251) as u8),
                    shade.wrapping_add((y % 241) as u8),
                    shade,
                    255,
                ]);
            }
        }
        VideoFrame {
            width: w,
            height: h,
            timestamp_us: 0,
            data: FrameData::Bgra(px),
        }
    }

    #[test]
    fn encodes_annex_b_h264_and_starts_with_a_keyframe() {
        let mut enc = OpenH264Encoder::default();
        let first = enc
            .encode(&frame(320, 180, 10))
            .unwrap()
            .expect("first frame");
        assert!(first.keyframe, "the stream must start with a keyframe");
        assert!(
            first.data.starts_with(&[0, 0, 0, 1]) || first.data.starts_with(&[0, 0, 1]),
            "Annex B start code"
        );
        assert!(first.data.len() > 100);
        // Later frames are delta frames unless a keyframe is requested.
        let second = enc.encode(&frame(320, 180, 12)).unwrap();
        if let Some(second) = second {
            assert!(!second.keyframe);
        }
        enc.request_keyframe();
        let forced = enc.encode(&frame(320, 180, 14)).unwrap().expect("forced");
        assert!(forced.keyframe);
    }

    #[test]
    fn a_size_change_restarts_the_stream_with_a_keyframe() {
        let mut enc = OpenH264Encoder::default();
        enc.encode(&frame(320, 180, 1)).unwrap();
        let bigger = enc.encode(&frame(640, 360, 1)).unwrap().expect("frame");
        assert!(bigger.keyframe);
    }

    #[test]
    fn changing_the_bitrate_takes_effect() {
        let mut enc = OpenH264Encoder::new(2_000_000, 30.0);
        enc.encode(&frame(320, 180, 1)).unwrap();
        enc.set_bitrate(500_000).unwrap();
        assert_eq!(enc.bitrate(), 500_000);
        let next = enc.encode(&frame(320, 180, 2)).unwrap().expect("frame");
        assert!(next.keyframe, "a rebuilt encoder starts with a keyframe");
    }

    #[test]
    fn bad_frames_are_refused_not_crashed_on() {
        let mut enc = OpenH264Encoder::default();
        assert!(enc.encode(&frame(321, 180, 0)).is_err(), "odd width");
        assert!(enc.encode(&frame(320, 181, 0)).is_err(), "odd height");
        let mut short = frame(320, 180, 0);
        short.data = FrameData::Bgra(vec![0; 10]);
        assert!(enc.encode(&short).is_err());
        let native = VideoFrame {
            width: 320,
            height: 180,
            timestamp_us: 0,
            data: FrameData::Native(7),
        };
        assert!(enc.encode(&native).is_err());
    }

    fn report(loss: f64, rtt: f64) -> NetworkReport {
        NetworkReport {
            loss_fraction: loss,
            rtt_ms: rtt,
        }
    }

    #[test]
    fn heavy_loss_backs_off_clean_links_probe_up() {
        let mut c = BitrateController::new(2_000_000);
        assert_eq!(
            c.update(Duration::from_secs(10), report(0.2, 50.0)),
            Some(1_600_000)
        );
        assert_eq!(
            c.update(Duration::from_secs(13), report(0.0, 50.0)),
            Some(1_728_000)
        );
        // Moderate loss: hold.
        assert_eq!(c.update(Duration::from_secs(20), report(0.05, 50.0)), None);
        // High latency: do not probe up.
        assert_eq!(c.update(Duration::from_secs(30), report(0.0, 800.0)), None);
    }

    #[test]
    fn the_target_moves_at_most_every_two_seconds() {
        let mut c = BitrateController::new(2_000_000);
        assert!(
            c.update(Duration::from_secs(10), report(0.5, 50.0))
                .is_some()
        );
        assert_eq!(
            c.update(Duration::from_millis(11_500), report(0.5, 50.0)),
            None
        );
        assert!(
            c.update(Duration::from_millis(12_000), report(0.5, 50.0))
                .is_some()
        );
    }

    #[test]
    fn the_target_stays_within_bounds() {
        let mut c = BitrateController::new(BitrateController::MIN_BPS);
        for i in 0..20 {
            c.update(Duration::from_secs(10 + i * 3), report(0.9, 50.0));
        }
        assert_eq!(c.target(), BitrateController::MIN_BPS);
        let mut c = BitrateController::new(BitrateController::MAX_BPS);
        for i in 0..20 {
            c.update(Duration::from_secs(10 + i * 3), report(0.0, 10.0));
        }
        assert_eq!(c.target(), BitrateController::MAX_BPS);
        assert_eq!(
            BitrateController::new(1).target(),
            BitrateController::MIN_BPS
        );
    }
}

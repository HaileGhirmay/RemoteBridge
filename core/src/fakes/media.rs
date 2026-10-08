use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::Trace;
use crate::traits::{AudioCapture, Capture, MicCapture};
use crate::types::{AudioChunk, DisplayId, DisplayInfo, FrameData, VideoFrame};
use crate::{PlatformError, PlatformResult};

#[derive(Debug, Default)]
struct CaptureState {
    running: bool,
    start_error: Option<PlatformError>,
    frames: VecDeque<VideoFrame>,
    /// When set, `next_frame` makes a frame of this size (about 30 per second)
    /// whenever none is queued.
    auto_frames: Option<(u32, u32)>,
    generated: u64,
    starts: u32,
    stops: u32,
    trace: Option<Trace>,
}

/// Scripted screen capture.
#[derive(Debug, Clone, Default)]
pub struct FakeCapture {
    state: Arc<Mutex<CaptureState>>,
}

impl FakeCapture {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record `capture:start` and `capture:stop` in a shared trace, so a test
    /// can assert the order against other components.
    pub fn with_trace(trace: Trace) -> Self {
        let capture = Self::default();
        capture.state.lock().unwrap().trace = Some(trace);
        capture
    }

    /// Queue a small solid frame for `next_frame`.
    pub fn push_frame(&self, timestamp_us: u64) {
        self.state.lock().unwrap().frames.push_back(VideoFrame {
            width: 2,
            height: 2,
            timestamp_us,
            data: FrameData::Bgra(vec![0; 16]),
        });
    }

    /// Make the next `start` fail, e.g. `SecureDesktop` or `PermissionDenied`.
    pub fn fail_start_with(&self, err: Option<PlatformError>) {
        self.state.lock().unwrap().start_error = err;
    }

    /// Produce a `width` x `height` frame about 30 times a second while running.
    pub fn set_auto_frames(&self, size: Option<(u32, u32)>) {
        self.state.lock().unwrap().auto_frames = size;
    }

    pub fn start_count(&self) -> u32 {
        self.state.lock().unwrap().starts
    }

    pub fn stop_count(&self) -> u32 {
        self.state.lock().unwrap().stops
    }

    pub fn is_running(&self) -> bool {
        self.state.lock().unwrap().running
    }
}

fn generated_frame(width: u32, height: u32, n: u64) -> VideoFrame {
    let shade = (n % 200) as u8;
    let mut px = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            px.extend_from_slice(&[
                shade.wrapping_add((x % 200) as u8),
                (y % 200) as u8,
                shade,
                255,
            ]);
        }
    }
    VideoFrame {
        width,
        height,
        timestamp_us: n * 33_000,
        data: FrameData::Bgra(px),
    }
}

impl Capture for FakeCapture {
    fn displays(&self) -> PlatformResult<Vec<DisplayInfo>> {
        Ok(vec![DisplayInfo {
            id: DisplayId(1),
            name: "Fake display".into(),
            width: 1920,
            height: 1080,
            is_primary: true,
        }])
    }

    fn start(&mut self, _display: DisplayId) -> PlatformResult<()> {
        let mut s = self.state.lock().unwrap();
        if let Some(err) = s.start_error.clone() {
            return Err(err);
        }
        s.running = true;
        s.starts += 1;
        Ok(())
    }

    fn next_frame(&mut self, _timeout: Duration) -> PlatformResult<Option<VideoFrame>> {
        let auto = {
            let mut s = self.state.lock().unwrap();
            if !s.running {
                return Err(PlatformError::NotStarted);
            }
            if let Some(frame) = s.frames.pop_front() {
                return Ok(Some(frame));
            }
            s.auto_frames
        };
        let Some((w, h)) = auto else {
            return Ok(None);
        };
        // Pace like a real display, outside the lock so `stop()` is not held up.
        std::thread::sleep(Duration::from_millis(33));
        let mut s = self.state.lock().unwrap();
        if !s.running {
            return Err(PlatformError::NotStarted);
        }
        s.generated += 1;
        Ok(Some(generated_frame(w, h, s.generated)))
    }

    fn is_running(&self) -> bool {
        self.state.lock().unwrap().running
    }

    fn stop(&mut self) {
        let mut s = self.state.lock().unwrap();
        if s.running
            && let Some(trace) = &s.trace
        {
            trace.push("capture:stop");
        }
        s.running = false;
        s.stops += 1;
    }
}

#[derive(Debug, Default)]
struct AudioState {
    running: bool,
    start_error: Option<PlatformError>,
    chunks: VecDeque<AudioChunk>,
    /// When set, `next_chunk` makes a 20 ms silent chunk whenever none is queued.
    auto_chunks: bool,
    starts: u32,
    stops: u32,
}

macro_rules! fake_audio {
    ($name:ident, $trait:ident, $doc:literal) => {
        #[doc = $doc]
        #[derive(Debug, Clone, Default)]
        pub struct $name {
            state: Arc<Mutex<AudioState>>,
        }

        impl $name {
            pub fn new() -> Self {
                Self::default()
            }

            /// Queue a short silent chunk.
            pub fn push_chunk(&self, timestamp_us: u64) {
                self.state.lock().unwrap().chunks.push_back(AudioChunk {
                    sample_rate: 48_000,
                    channels: 2,
                    timestamp_us,
                    samples: vec![0; 960],
                });
            }

            pub fn fail_start_with(&self, err: Option<PlatformError>) {
                self.state.lock().unwrap().start_error = err;
            }

            /// Produce a 20 ms silent chunk every 20 ms while running.
            pub fn set_auto_chunks(&self, on: bool) {
                self.state.lock().unwrap().auto_chunks = on;
            }

            pub fn start_count(&self) -> u32 {
                self.state.lock().unwrap().starts
            }

            pub fn stop_count(&self) -> u32 {
                self.state.lock().unwrap().stops
            }
        }

        impl $trait for $name {
            fn start(&mut self) -> PlatformResult<()> {
                let mut s = self.state.lock().unwrap();
                if let Some(err) = s.start_error.clone() {
                    return Err(err);
                }
                s.running = true;
                s.starts += 1;
                Ok(())
            }

            fn next_chunk(&mut self, _timeout: Duration) -> PlatformResult<Option<AudioChunk>> {
                let auto = {
                    let mut s = self.state.lock().unwrap();
                    if !s.running {
                        return Err(PlatformError::NotStarted);
                    }
                    if let Some(chunk) = s.chunks.pop_front() {
                        return Ok(Some(chunk));
                    }
                    s.auto_chunks
                };
                if !auto {
                    return Ok(None);
                }
                std::thread::sleep(Duration::from_millis(20));
                if !self.state.lock().unwrap().running {
                    return Err(PlatformError::NotStarted);
                }
                Ok(Some(AudioChunk {
                    sample_rate: 48_000,
                    channels: 2,
                    timestamp_us: 0,
                    samples: vec![0; 960 * 2],
                }))
            }

            fn is_running(&self) -> bool {
                self.state.lock().unwrap().running
            }

            fn stop(&mut self) {
                let mut s = self.state.lock().unwrap();
                s.running = false;
                s.stops += 1;
            }
        }
    };
}

fake_audio!(
    FakeAudioCapture,
    AudioCapture,
    "Scripted system-audio capture."
);
fake_audio!(FakeMicCapture, MicCapture, "Scripted microphone capture.");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_streams_frames_until_stopped() {
        let probe = FakeCapture::new();
        let mut cap: Box<dyn Capture> = Box::new(probe.clone());

        assert_eq!(
            cap.next_frame(Duration::ZERO),
            Err(PlatformError::NotStarted)
        );
        probe.push_frame(10);
        cap.start(DisplayId(1)).unwrap();
        assert!(cap.is_running());
        assert_eq!(
            cap.next_frame(Duration::ZERO)
                .unwrap()
                .unwrap()
                .timestamp_us,
            10
        );
        assert_eq!(cap.next_frame(Duration::ZERO), Ok(None));

        cap.stop();
        cap.stop();
        assert!(!cap.is_running());
        assert_eq!(probe.stop_count(), 2);
    }

    #[test]
    fn capture_reports_secure_desktop_and_missing_permission() {
        let probe = FakeCapture::new();
        let mut cap = probe.clone();

        probe.fail_start_with(Some(PlatformError::SecureDesktop));
        assert_eq!(cap.start(DisplayId(1)), Err(PlatformError::SecureDesktop));
        probe.fail_start_with(Some(PlatformError::PermissionDenied("screen recording")));
        assert!(matches!(
            cap.start(DisplayId(1)),
            Err(PlatformError::PermissionDenied(_))
        ));
        assert!(!cap.is_running());
        assert_eq!(probe.start_count(), 0);
    }

    #[test]
    fn audio_and_mic_are_independent() {
        let sys = FakeAudioCapture::new();
        let mic = FakeMicCapture::new();
        let mut sys_dyn: Box<dyn AudioCapture> = Box::new(sys.clone());
        let mut mic_dyn: Box<dyn MicCapture> = Box::new(mic.clone());

        sys.push_chunk(1);
        sys_dyn.start().unwrap();
        assert!(!mic_dyn.is_running());
        assert!(sys_dyn.next_chunk(Duration::ZERO).unwrap().is_some());

        mic_dyn.start().unwrap();
        mic_dyn.stop();
        sys_dyn.stop();
        assert_eq!((sys.stop_count(), mic.stop_count()), (1, 1));
    }
}

use std::time::Duration;

use crate::PlatformResult;
use crate::types::{AudioChunk, DisplayId, DisplayInfo, VideoFrame};

/// Screen capture. `stop()` must take effect immediately and be idempotent:
/// the host stops capture the moment the server reports a terminal state.
pub trait Capture: Send {
    fn displays(&self) -> PlatformResult<Vec<DisplayInfo>>;

    /// Start capturing one display, cursor included. Fails with
    /// `SecureDesktop` or `PermissionDenied` where the OS requires it; never
    /// tries to get around either.
    fn start(&mut self, display: DisplayId) -> PlatformResult<()>;

    /// Wait up to `timeout` for the next frame. `Ok(None)` means none arrived.
    fn next_frame(&mut self, timeout: Duration) -> PlatformResult<Option<VideoFrame>>;

    fn is_running(&self) -> bool;
    fn stop(&mut self);
}

/// System audio (WASAPI loopback, ScreenCaptureKit audio).
pub trait AudioCapture: Send {
    fn start(&mut self) -> PlatformResult<()>;
    fn next_chunk(&mut self, timeout: Duration) -> PlatformResult<Option<AudioChunk>>;
    fn is_running(&self) -> bool;
    fn stop(&mut self);
}

/// Microphone. A separate trait from [`AudioCapture`] so the two can never be
/// mixed up: the microphone is its own opt-in and never available unattended.
pub trait MicCapture: Send {
    fn start(&mut self) -> PlatformResult<()>;
    fn next_chunk(&mut self, timeout: Duration) -> PlatformResult<Option<AudioChunk>>;
    fn is_running(&self) -> bool;
    fn stop(&mut self);
}

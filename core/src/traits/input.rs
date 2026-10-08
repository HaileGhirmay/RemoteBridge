use crate::PlatformResult;
use crate::types::MouseButton;

/// Injects remote input. Callers must already have passed the message through
/// the permission filter; this trait does no policy checks.
///
/// Implementations tag every event they post so the local hotkey hook can
/// recognise and reject it (`LLKHF_INJECTED` / `dwExtraInfo` on Windows, a
/// private `eventSourceUserData` on macOS).
pub trait InputInjector: Send {
    /// `code` is the DOM `KeyboardEvent.code` value.
    fn key(&mut self, code: &str, down: bool) -> PlatformResult<()>;
    fn button(&mut self, button: MouseButton, down: bool) -> PlatformResult<()>;
    fn wheel(&mut self, dx: f32, dy: f32) -> PlatformResult<()>;
    /// `x` and `y` are in 0..=1 of the captured display.
    fn move_pointer(&mut self, x: f32, y: f32) -> PlatformResult<()>;
    /// Release every key and button currently held down by injected input.
    fn release_all(&mut self) -> PlatformResult<()>;
}

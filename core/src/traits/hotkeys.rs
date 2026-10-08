use crate::PlatformResult;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyEvent {
    /// Ctrl+Alt+Shift+B (Windows), Control+Option+Shift+B (macOS).
    RestoreIndicators,
    /// Ctrl+Alt+Shift+X (Windows), Control+Option+Shift+X (macOS).
    EmergencyDisconnect,
}

/// Which shortcuts the OS accepted. `false` means a collision; the host then
/// warns the user and relies on the tray menu items instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HotkeyRegistration {
    pub restore: bool,
    pub emergency_disconnect: bool,
}

impl HotkeyRegistration {
    pub fn collision(&self) -> bool {
        !(self.restore && self.emergency_disconnect)
    }
}

/// Global shortcuts. Contract: only the local physical keyboard may produce
/// events. Injected input (remote control, other software) is dropped by the
/// implementation and never reaches [`Hotkeys::poll`].
pub trait Hotkeys: Send {
    fn register(&mut self) -> PlatformResult<HotkeyRegistration>;
    fn unregister(&mut self);
    fn poll(&mut self) -> Option<HotkeyEvent>;
    /// Human-readable label shown to the user, for example `Ctrl+Alt+Shift+B`.
    fn restore_label(&self) -> String;
    fn disconnect_label(&self) -> String;
}

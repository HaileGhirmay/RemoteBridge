//! Windows 11 implementations of the `rb-core` platform traits.
//!
//! Compiles on every OS so the workspace builds everywhere, but the adapters
//! themselves are Windows-only. Done: `KeyStore` (CNG, TPM when available),
//! `Hotkeys` (RegisterHotKey plus a low-level hook), `Tray`/`Banner`
//! (per-display windows, excluded from capture), remote input (`SendInput`
//! with scan codes), screen capture, system audio and microphone, the
//! approval window (`ConsentUi`, one checkbox per opt-in) and the text
//! clipboard.

pub mod clipboard;
pub mod keymap;
pub mod prompt;

#[cfg(windows)]
mod approval;
#[cfg(windows)]
mod audio;
#[cfg(windows)]
mod capture;
#[cfg(windows)]
mod consent;
#[cfg(windows)]
mod hotkeys;
#[cfg(windows)]
mod input;
#[cfg(windows)]
mod input_guard;
#[cfg(windows)]
mod key_store;
#[cfg(windows)]
mod ui;

#[cfg(windows)]
pub use audio::{Microphone, SystemAudio};
#[cfg(windows)]
pub use capture::DxgiCapture;
#[cfg(windows)]
pub use clipboard::WindowsClipboard;
#[cfg(windows)]
pub use consent::WindowsConsent;
#[cfg(windows)]
pub use hotkeys::WindowsHotkeys;
#[cfg(windows)]
pub use input::{DisplayRect, INJECT_TAG, WindowsInput, primary_display};
#[cfg(windows)]
pub use input_guard::rejected_count as consent_injected_rejections;
#[cfg(windows)]
pub use key_store::{KeyBackend, KeyScope, ProviderPreference, WindowsKeyStore};
#[cfg(windows)]
pub use ui::{WindowsBanner, WindowsTray, WindowsUi};

/// Short name used in logs and diagnostics.
pub const PLATFORM_NAME: &str = "windows";

#[cfg(test)]
mod tests {
    #[test]
    fn platform_name_is_stable() {
        assert_eq!(super::PLATFORM_NAME, "windows");
    }
}

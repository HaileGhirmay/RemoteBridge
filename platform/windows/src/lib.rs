//! Windows 11 implementations of the `rb-core` platform traits.
//!
//! Compiles on every OS so the workspace builds everywhere, but the adapters
//! themselves are Windows-only. Done: `KeyStore` (CNG, TPM when available).
//! To come: `Tray`/`Banner`/`Hotkeys` in Prompt 05 and
//! `Capture`/`AudioCapture`/`MicCapture`/`InputInjector` in Prompt 10.

#[cfg(windows)]
mod hotkeys;
#[cfg(windows)]
mod key_store;
#[cfg(windows)]
mod ui;

#[cfg(windows)]
pub use hotkeys::WindowsHotkeys;
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

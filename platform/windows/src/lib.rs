//! Windows 11 implementations of the `rb-core` platform traits.
//!
//! Compiles on every OS so the workspace builds everywhere, but the adapters
//! themselves are Windows-only. They arrive in later prompts:
//! `KeyStore` (CNG/TPM) in Prompt 03, `Tray`/`Banner`/`Hotkeys` in Prompt 05,
//! `Capture`/`AudioCapture`/`MicCapture`/`InputInjector` in Prompt 10.

/// Short name used in logs and diagnostics.
pub const PLATFORM_NAME: &str = "windows";

#[cfg(test)]
mod tests {
    #[test]
    fn platform_name_is_stable() {
        assert_eq!(super::PLATFORM_NAME, "windows");
    }
}

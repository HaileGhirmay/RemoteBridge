use std::fmt;

/// Failure reported by a platform adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlatformError {
    /// The OS refused access (for example macOS Screen Recording or
    /// Accessibility). The host reports `permissions_unavailable` and shows a
    /// guide; it never works around the refusal.
    PermissionDenied(&'static str),
    /// Capture is impossible because a secure desktop (UAC prompt,
    /// Ctrl+Alt+Del, lock screen) is showing. Never bypassed.
    SecureDesktop,
    /// This platform or OS version cannot do the requested thing.
    Unsupported(&'static str),
    /// The adapter was used before `start()` or after `stop()`.
    NotStarted,
    /// `create_key` was called while a device key already exists.
    KeyExists,
    /// No device key exists.
    KeyNotFound,
    /// Any other backend failure. Must not contain screen data, keystrokes,
    /// clipboard text or audio.
    Backend(String),
}

impl fmt::Display for PlatformError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PermissionDenied(what) => write!(f, "permission denied: {what}"),
            Self::SecureDesktop => f.write_str("secure desktop is active"),
            Self::Unsupported(what) => write!(f, "unsupported: {what}"),
            Self::NotStarted => f.write_str("not started"),
            Self::KeyExists => f.write_str("device key already exists"),
            Self::KeyNotFound => f.write_str("device key not found"),
            Self::Backend(msg) => write!(f, "platform error: {msg}"),
        }
    }
}

impl std::error::Error for PlatformError {}

pub type PlatformResult<T> = Result<T, PlatformError>;

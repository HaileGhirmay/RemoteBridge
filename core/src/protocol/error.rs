use std::fmt;

use crate::PlatformError;

/// Everything that can go wrong talking to the control plane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostError {
    /// 401 with code `DEVICE_REVOKED`. The caller must wipe the key, end all
    /// sessions and return to the unenrolled state.
    DeviceRevoked,
    /// Any other non-2xx answer. `code` is the server's machine-readable code
    /// when it sent one (for example `MEDIA_UNCONFIGURED`).
    Api {
        status: u16,
        code: Option<String>,
        message: String,
    },
    /// The request did not complete within the timeout.
    Timeout,
    /// Connection-level failure (DNS, TLS, reset).
    Network(String),
    /// A body could not be encoded or decoded.
    Codec(String),
    /// The device key store failed.
    Key(PlatformError),
    /// A signed route was called before the device id was known.
    NotEnrolled,
    /// The server's fingerprint for our key differs from the one we computed.
    FingerprintMismatch,
}

impl HostError {
    /// Worth sending again as a brand-new signed request (new timestamp,
    /// nonce and signature). Client errors (4xx) never are, except 429.
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Timeout | Self::Network(_) => true,
            Self::Api { status, .. } => *status >= 500 || *status == 429,
            _ => false,
        }
    }
}

impl fmt::Display for HostError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DeviceRevoked => f.write_str("this device was removed from the account"),
            Self::Api {
                status,
                code,
                message,
            } => match code {
                Some(code) => write!(f, "server error {status} ({code}): {message}"),
                None => write!(f, "server error {status}: {message}"),
            },
            Self::Timeout => f.write_str("request timed out"),
            Self::Network(msg) => write!(f, "network error: {msg}"),
            Self::Codec(msg) => write!(f, "codec error: {msg}"),
            Self::Key(e) => write!(f, "device key error: {e}"),
            Self::NotEnrolled => f.write_str("device is not enrolled"),
            Self::FingerprintMismatch => {
                f.write_str("server key fingerprint does not match the local key")
            }
        }
    }
}

impl std::error::Error for HostError {}

impl From<PlatformError> for HostError {
    fn from(e: PlatformError) -> Self {
        Self::Key(e)
    }
}

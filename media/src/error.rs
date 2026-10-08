use std::fmt;

/// Everything that can go wrong in the media layer. Messages never contain
/// screen data, keystrokes, clipboard text, audio, tokens or SDP.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaError {
    /// Certificate or crypto provider trouble.
    Crypto(String),
    /// The signaling relay could not be reached or refused us.
    Signaling(String),
    /// The WebRTC stack reported an error.
    WebRtc(String),
    /// The viewer's DTLS fingerprint is not the one the server vouched for.
    FingerprintMismatch,
    /// A video or audio encoder failed or cannot handle this input.
    Encoder(String),
    /// The session is not running (any more).
    Stopped,
}

impl fmt::Display for MediaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Crypto(m) => write!(f, "crypto: {m}"),
            Self::Signaling(m) => write!(f, "signaling: {m}"),
            Self::WebRtc(m) => write!(f, "webrtc: {m}"),
            Self::FingerprintMismatch => f.write_str("viewer fingerprint does not match"),
            Self::Encoder(m) => write!(f, "encoder: {m}"),
            Self::Stopped => f.write_str("media session stopped"),
        }
    }
}

impl std::error::Error for MediaError {}

impl From<webrtc::error::Error> for MediaError {
    fn from(e: webrtc::error::Error) -> Self {
        Self::WebRtc(e.to_string())
    }
}

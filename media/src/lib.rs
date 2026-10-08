//! RemoteBridge host media (Prompt 08).
//!
//! Follows `AGENTS.md`, "Media path contract": the host is the WebRTC
//! offerer, creates the `control` and `pointer` data channels, and checks the
//! viewer's DTLS fingerprint against the one the server vouched for.

pub mod audio;
pub mod error;
pub mod fingerprint;
pub mod peer;
pub mod signaling;
#[cfg(feature = "testing")]
pub mod testing;
pub mod video;

pub use error::MediaError;
pub use peer::{
    AudioEncoderFactory, ChannelInfo, HostIdentity, IceServerConfig, MediaCredentials, MediaEvent,
    MediaFeed, MediaSession, MediaSessionConfig, default_udp_ip,
};

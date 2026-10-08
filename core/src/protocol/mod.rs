//! RA-HOST-V1: the host protocol spoken with the RemoteBridge control plane.
//!
//! See `AGENTS.md`, "Host protocol (RA-HOST-V1)". The server is the source of
//! truth; this module only encodes, signs and decodes.

pub mod client;
pub mod error;
pub mod fingerprint;
pub mod nonce;
pub mod signing;
pub mod superjson;
pub mod transport;
pub mod types;

pub use client::{ClientConfig, HostClient, RetryPolicy};
pub use error::HostError;
pub use nonce::{NonceSource, OsNonceSource};
pub use transport::{HttpRequest, HttpResponse, ReqwestTransport, Transport, TransportError};

/// Value of `protocolVersion` sent to the server.
pub const PROTOCOL_VERSION: u32 = 1;

/// Production control plane.
pub const DEFAULT_BASE_URL: &str = "https://remotebridge.floot.app/_api/";

/// Milliseconds since the Unix epoch. Server `Date` values decode to this.
pub type UnixMs = i64;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};

/// base64url without padding, as used for signatures, nonces and JWK coordinates.
pub(crate) fn b64url(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Lowercase hex SHA-256.
pub(crate) fn sha256_hex(data: &[u8]) -> String {
    let digest = Sha256::digest(data);
    let mut out = String::with_capacity(64);
    for b in digest.iter() {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

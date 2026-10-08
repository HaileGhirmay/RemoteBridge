use crate::PlatformResult;
use crate::types::{PublicKeyJwk, RawSignature};

/// The device identity key: ECDSA P-256, private half non-exportable
/// (Windows CNG/TPM, macOS Secure Enclave or Keychain).
pub trait KeyStore: Send + Sync {
    /// The public key, or `None` if the device has no key (not enrolled).
    fn public_key(&self) -> PlatformResult<Option<PublicKeyJwk>>;

    /// Create the key. Fails with `KeyExists` if one already exists.
    fn create_key(&self) -> PlatformResult<PublicKeyJwk>;

    /// ECDSA-SHA256 over `message`. The store hashes; callers pass the raw
    /// message. May return DER or P1363; the encoding is tagged.
    fn sign(&self, message: &[u8]) -> PlatformResult<RawSignature>;

    /// Delete the key. Used on `DEVICE_REVOKED` and uninstall. Idempotent.
    fn wipe(&self) -> PlatformResult<()>;
}

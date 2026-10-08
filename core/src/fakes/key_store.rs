use std::sync::{Arc, Mutex};

use crate::traits::KeyStore;
use crate::types::{PublicKeyJwk, RawSignature, SignatureEncoding};
use crate::{PlatformError, PlatformResult};

#[derive(Debug, Default)]
struct State {
    key: Option<PublicKeyJwk>,
    signed: Vec<Vec<u8>>,
    wipes: u32,
}

/// In-memory key store. The "signature" is a deterministic 64-byte value, not
/// real ECDSA; use it for plumbing tests only, never for crypto tests.
#[derive(Debug, Clone, Default)]
pub struct FakeKeyStore {
    state: Arc<Mutex<State>>,
}

impl FakeKeyStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Messages passed to `sign`, in order.
    pub fn signed_messages(&self) -> Vec<Vec<u8>> {
        self.state.lock().unwrap().signed.clone()
    }

    pub fn wipe_count(&self) -> u32 {
        self.state.lock().unwrap().wipes
    }
}

impl KeyStore for FakeKeyStore {
    fn public_key(&self) -> PlatformResult<Option<PublicKeyJwk>> {
        Ok(self.state.lock().unwrap().key.clone())
    }

    fn create_key(&self) -> PlatformResult<PublicKeyJwk> {
        let mut s = self.state.lock().unwrap();
        if s.key.is_some() {
            return Err(PlatformError::KeyExists);
        }
        // 43 base64url chars = 32 bytes. Not a real curve point.
        let key = PublicKeyJwk {
            x: "A".repeat(43),
            y: "E".repeat(43),
        };
        s.key = Some(key.clone());
        Ok(key)
    }

    fn sign(&self, message: &[u8]) -> PlatformResult<RawSignature> {
        let mut s = self.state.lock().unwrap();
        if s.key.is_none() {
            return Err(PlatformError::KeyNotFound);
        }
        s.signed.push(message.to_vec());
        // FNV-1a seeded per byte position, so different messages differ.
        let mut bytes = Vec::with_capacity(64);
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in message {
            h = (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3);
        }
        for i in 0..8u64 {
            h = (h ^ i).wrapping_mul(0x0100_0000_01b3);
            bytes.extend_from_slice(&h.to_be_bytes());
        }
        Ok(RawSignature {
            encoding: SignatureEncoding::P1363,
            bytes,
        })
    }

    fn wipe(&self) -> PlatformResult<()> {
        let mut s = self.state.lock().unwrap();
        s.key = None;
        s.wipes += 1;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_sign_wipe_lifecycle() {
        let ks = FakeKeyStore::new();
        assert_eq!(ks.public_key(), Ok(None));
        assert_eq!(ks.sign(b"x"), Err(PlatformError::KeyNotFound));

        let pk = ks.create_key().unwrap();
        assert_eq!(ks.public_key().unwrap(), Some(pk));
        assert_eq!(ks.create_key(), Err(PlatformError::KeyExists));

        let sig = ks.sign(b"hello").unwrap();
        assert_eq!(sig.encoding, SignatureEncoding::P1363);
        assert_eq!(sig.bytes.len(), 64);
        assert_ne!(sig.bytes, ks.sign(b"hellp").unwrap().bytes);
        assert_eq!(ks.signed_messages().len(), 2, "failed sign is not recorded");

        ks.wipe().unwrap();
        ks.wipe().unwrap();
        assert_eq!(ks.public_key(), Ok(None));
        assert_eq!(ks.wipe_count(), 2);
    }
}

//! DEVELOPMENT-ONLY key store: the private key sits in a plain file.
//!
//! The real host never uses this. It exists so the protocol client can be
//! exercised end to end before the hardware-backed stores of Prompt 03 exist.
//! It answers in DER on purpose, so the DER to P1363 conversion in `rb-core`
//! is exercised against the live server.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use p256::ecdsa::signature::Signer;
use p256::ecdsa::{Signature, SigningKey};
use rb_core::traits::KeyStore;
use rb_core::types::{PublicKeyJwk, RawSignature, SignatureEncoding};
use rb_core::{PlatformError, PlatformResult};
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct State {
    /// base64url of the 32-byte private scalar.
    pub private_key: Option<String>,
    pub device_id: Option<String>,
}

pub struct FileKeyStore {
    path: PathBuf,
    lock: Mutex<()>,
}

fn backend(e: impl std::fmt::Display) -> PlatformError {
    PlatformError::Backend(e.to_string())
}

impl FileKeyStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            lock: Mutex::new(()),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> PlatformResult<State> {
        match fs::read(&self.path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(backend),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(State::default()),
            Err(e) => Err(backend(e)),
        }
    }

    pub fn save(&self, state: &State) -> PlatformResult<()> {
        let text = serde_json::to_string_pretty(state).map_err(backend)?;
        fs::write(&self.path, text).map_err(backend)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&self.path, fs::Permissions::from_mode(0o600)).map_err(backend)?;
        }
        Ok(())
    }

    pub fn device_id(&self) -> PlatformResult<Option<String>> {
        Ok(self.load()?.device_id)
    }

    pub fn set_device_id(&self, device_id: &str) -> PlatformResult<()> {
        let _guard = self.lock.lock().unwrap();
        let mut state = self.load()?;
        state.device_id = Some(device_id.to_owned());
        self.save(&state)
    }

    fn signing_key(state: &State) -> PlatformResult<SigningKey> {
        let encoded = state
            .private_key
            .as_deref()
            .ok_or(PlatformError::KeyNotFound)?;
        let bytes = URL_SAFE_NO_PAD.decode(encoded).map_err(backend)?;
        SigningKey::from_slice(&bytes).map_err(backend)
    }

    fn jwk(key: &SigningKey) -> PublicKeyJwk {
        let point = key.verifying_key().to_sec1_point(false);
        let coord = |c: Option<&[u8]>| URL_SAFE_NO_PAD.encode(c.expect("uncompressed point"));
        PublicKeyJwk {
            x: coord(point.x().map(|a| a.as_slice())),
            y: coord(point.y().map(|a| a.as_slice())),
        }
    }
}

impl KeyStore for FileKeyStore {
    fn public_key(&self) -> PlatformResult<Option<PublicKeyJwk>> {
        let _guard = self.lock.lock().unwrap();
        let state = self.load()?;
        if state.private_key.is_none() {
            return Ok(None);
        }
        Ok(Some(Self::jwk(&Self::signing_key(&state)?)))
    }

    fn create_key(&self) -> PlatformResult<PublicKeyJwk> {
        let _guard = self.lock.lock().unwrap();
        let mut state = self.load()?;
        if state.private_key.is_some() {
            return Err(PlatformError::KeyExists);
        }
        let key = loop {
            let mut scalar = [0u8; 32];
            getrandom::fill(&mut scalar).map_err(backend)?;
            // Out-of-range scalars are astronomically rare; just draw again.
            if let Ok(key) = SigningKey::from_slice(&scalar) {
                state.private_key = Some(URL_SAFE_NO_PAD.encode(scalar));
                break key;
            }
        };
        self.save(&state)?;
        Ok(Self::jwk(&key))
    }

    fn sign(&self, message: &[u8]) -> PlatformResult<RawSignature> {
        let _guard = self.lock.lock().unwrap();
        let key = Self::signing_key(&self.load()?)?;
        let signature: Signature = key.sign(message);
        Ok(RawSignature {
            encoding: SignatureEncoding::Der,
            bytes: signature.to_der().as_bytes().to_vec(),
        })
    }

    fn wipe(&self) -> PlatformResult<()> {
        let _guard = self.lock.lock().unwrap();
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(backend(e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use p256::ecdsa::signature::Verifier;
    use rb_core::protocol::signing::to_p1363;

    fn temp_store(name: &str) -> FileKeyStore {
        let path = std::env::temp_dir().join(format!(
            "rb-hostctl-test-{}-{name}.devkey",
            std::process::id()
        ));
        let _ = fs::remove_file(&path);
        FileKeyStore::new(path)
    }

    #[test]
    fn key_lifecycle_and_signature_verifies() {
        let store = temp_store("lifecycle");
        assert_eq!(store.public_key().unwrap(), None);
        assert_eq!(store.sign(b"x"), Err(PlatformError::KeyNotFound));

        let jwk = store.create_key().unwrap();
        assert_eq!(store.create_key(), Err(PlatformError::KeyExists));
        assert_eq!(store.public_key().unwrap(), Some(jwk.clone()));
        assert_eq!(URL_SAFE_NO_PAD.decode(&jwk.x).unwrap().len(), 32);

        let raw = store.sign(b"hello").unwrap();
        assert_eq!(raw.encoding, SignatureEncoding::Der);
        let p1363 = to_p1363(&raw).unwrap();
        let key = FileKeyStore::signing_key(&store.load().unwrap()).unwrap();
        key.verifying_key()
            .verify(b"hello", &Signature::from_slice(&p1363).unwrap())
            .unwrap();

        store.wipe().unwrap();
        store.wipe().unwrap();
        assert_eq!(store.public_key().unwrap(), None);
        assert!(!store.path().exists());
    }

    #[test]
    fn device_id_survives_key_creation() {
        let store = temp_store("device-id");
        store.set_device_id("abc").unwrap();
        store.create_key().unwrap();
        assert_eq!(store.device_id().unwrap().as_deref(), Some("abc"));
        store.wipe().unwrap();
        assert_eq!(
            store.device_id().unwrap(),
            None,
            "wipe removes the device id too"
        );
    }
}

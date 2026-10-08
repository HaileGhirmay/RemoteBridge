//! Device key in Windows CNG.
//!
//! ECDSA P-256, private half non-exportable. The key lives in the Microsoft
//! Platform Crypto Provider (the TPM) when there is one and falls back to the
//! Microsoft Software Key Storage Provider otherwise. `NCryptSignHash` already
//! returns IEEE P1363 (`r || s`), which is what the server wants.
//!
//! No CNG handle outlives a call, so the store is trivially `Send + Sync`.

use std::sync::Mutex;

use rb_core::protocol::b64url;
use rb_core::traits::KeyStore;
use rb_core::types::{PublicKeyJwk, RawSignature, SignatureEncoding};
use rb_core::{PlatformError, PlatformResult};
use sha2::{Digest, Sha256};
use windows::Win32::Security::Cryptography::{
    BCRYPT_ECCPUBLIC_BLOB, BCRYPT_ECDSA_P256_ALGORITHM, CERT_KEY_SPEC, MS_KEY_STORAGE_PROVIDER,
    MS_PLATFORM_CRYPTO_PROVIDER, NCRYPT_FLAGS, NCRYPT_HANDLE, NCRYPT_KEY_HANDLE,
    NCRYPT_MACHINE_KEY_FLAG, NCRYPT_PROV_HANDLE, NCRYPT_SILENT_FLAG, NCryptCreatePersistedKey,
    NCryptDeleteKey, NCryptExportKey, NCryptFinalizeKey, NCryptFreeObject, NCryptOpenKey,
    NCryptOpenStorageProvider, NCryptSignHash,
};
use windows::core::HSTRING;

/// `NTE_BAD_KEYSET`: no such key in this provider.
const NTE_BAD_KEYSET: u32 = 0x8009_0016;
/// `NTE_NO_KEY`.
const NTE_NO_KEY: u32 = 0x8009_000D;
/// `BCRYPT_ECDSA_PUBLIC_P256_MAGIC` ("ECS1").
const ECDSA_PUBLIC_P256_MAGIC: u32 = 0x3153_4345;
const P256_COORD_LEN: usize = 32;

/// Whose key it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyScope {
    /// The signed-in user's key container. No administrator rights needed.
    User,
    /// One key for the whole machine, for the small per-machine service that
    /// starts unattended access at login. Creating one may need elevation.
    Machine,
}

/// Where the private key actually lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyBackend {
    /// Microsoft Platform Crypto Provider: hardware-backed (TPM).
    Tpm,
    /// Microsoft Software Key Storage Provider.
    Software,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderPreference {
    /// TPM when it works, software otherwise.
    Auto,
    /// Never use the TPM. For tests and machines with a misbehaving TPM.
    SoftwareOnly,
}

pub struct WindowsKeyStore {
    name: HSTRING,
    scope: KeyScope,
    preference: ProviderPreference,
    lock: Mutex<()>,
}

struct Provider(NCRYPT_PROV_HANDLE);

impl Drop for Provider {
    fn drop(&mut self) {
        // SAFETY: the handle came from NCryptOpenStorageProvider and is freed once.
        let _ = unsafe { NCryptFreeObject(NCRYPT_HANDLE(self.0.0)) };
    }
}

struct Key(NCRYPT_KEY_HANDLE);

impl Key {
    /// Give up ownership without freeing (the handle was consumed elsewhere).
    fn into_raw(self) -> NCRYPT_KEY_HANDLE {
        let handle = self.0;
        std::mem::forget(self);
        handle
    }
}

impl Drop for Key {
    fn drop(&mut self) {
        // SAFETY: the handle came from NCryptOpenKey/CreatePersistedKey and is freed once.
        let _ = unsafe { NCryptFreeObject(NCRYPT_HANDLE(self.0.0)) };
    }
}

/// An open key plus the provider handle that must outlive it.
struct Opened {
    key: Key,
    _provider: Provider,
    backend: KeyBackend,
}

fn cng(e: windows::core::Error) -> PlatformError {
    PlatformError::Backend(format!("CNG error 0x{:08x}", e.code().0 as u32))
}

fn is_not_found(e: &windows::core::Error) -> bool {
    matches!(e.code().0 as u32, NTE_BAD_KEYSET | NTE_NO_KEY)
}

fn open_provider(backend: KeyBackend) -> windows::core::Result<Provider> {
    let name = match backend {
        KeyBackend::Tpm => MS_PLATFORM_CRYPTO_PROVIDER,
        KeyBackend::Software => MS_KEY_STORAGE_PROVIDER,
    };
    let mut handle = NCRYPT_PROV_HANDLE::default();
    // SAFETY: `handle` is a valid out-pointer; `name` is a static NUL-terminated string.
    unsafe { NCryptOpenStorageProvider(&mut handle, name, 0)? };
    Ok(Provider(handle))
}

impl WindowsKeyStore {
    /// `name` identifies the key inside the provider, for example
    /// `RemoteBridge.DeviceKey.v1`.
    pub fn new(name: &str, scope: KeyScope) -> Self {
        Self::with_preference(name, scope, ProviderPreference::Auto)
    }

    pub fn with_preference(name: &str, scope: KeyScope, preference: ProviderPreference) -> Self {
        Self {
            name: HSTRING::from(name),
            scope,
            preference,
            lock: Mutex::new(()),
        }
    }

    /// Which provider holds the key, or `None` if there is no key.
    pub fn backend(&self) -> PlatformResult<Option<KeyBackend>> {
        let _guard = self.lock.lock().unwrap();
        Ok(self
            .open_existing(&[KeyBackend::Tpm, KeyBackend::Software])?
            .map(|o| o.backend))
    }

    fn flags(&self) -> NCRYPT_FLAGS {
        let machine = match self.scope {
            KeyScope::User => 0,
            KeyScope::Machine => NCRYPT_MACHINE_KEY_FLAG.0,
        };
        NCRYPT_FLAGS(machine | NCRYPT_SILENT_FLAG.0)
    }

    fn creation_order(&self) -> &'static [KeyBackend] {
        match self.preference {
            ProviderPreference::Auto => &[KeyBackend::Tpm, KeyBackend::Software],
            ProviderPreference::SoftwareOnly => &[KeyBackend::Software],
        }
    }

    /// Find our key in the given providers, first match wins. A provider that
    /// cannot be opened (no TPM) simply has no key.
    fn open_existing(&self, backends: &[KeyBackend]) -> PlatformResult<Option<Opened>> {
        for &backend in backends {
            let Ok(provider) = open_provider(backend) else {
                continue;
            };
            let mut handle = NCRYPT_KEY_HANDLE::default();
            // SAFETY: valid provider handle, out-pointer and key name.
            let opened = unsafe {
                NCryptOpenKey(
                    provider.0,
                    &mut handle,
                    &self.name,
                    CERT_KEY_SPEC(0),
                    self.flags(),
                )
            };
            match opened {
                Ok(()) => {
                    return Ok(Some(Opened {
                        key: Key(handle),
                        _provider: provider,
                        backend,
                    }));
                }
                Err(e) if is_not_found(&e) => {}
                Err(e) if backend == KeyBackend::Tpm => {
                    // A TPM that errors on lookup must not block the software path.
                    let _ = e;
                }
                Err(e) => return Err(cng(e)),
            }
        }
        Ok(None)
    }

    fn create_in(&self, backend: KeyBackend) -> windows::core::Result<Opened> {
        let provider = open_provider(backend)?;
        let mut handle = NCRYPT_KEY_HANDLE::default();
        // SAFETY: valid provider handle, out-pointer, algorithm and key name.
        unsafe {
            NCryptCreatePersistedKey(
                provider.0,
                &mut handle,
                BCRYPT_ECDSA_P256_ALGORITHM,
                &self.name,
                CERT_KEY_SPEC(0),
                self.flags(),
            )?;
        }
        let key = Key(handle);
        // The export policy is left at its default, which is "not exportable".
        // SAFETY: `key.0` is a valid, not yet finalized key handle.
        unsafe { NCryptFinalizeKey(key.0, NCRYPT_SILENT_FLAG)? };
        Ok(Opened {
            key,
            _provider: provider,
            backend,
        })
    }
}

/// Uncompressed public point as JWK coordinates.
fn export_public(key: &Key) -> PlatformResult<PublicKeyJwk> {
    let mut len = 0u32;
    // SAFETY: size query with no output buffer.
    unsafe {
        NCryptExportKey(
            key.0,
            None,
            BCRYPT_ECCPUBLIC_BLOB,
            None,
            None,
            &mut len,
            NCRYPT_FLAGS(0),
        )
        .map_err(cng)?;
    }
    let mut blob = vec![0u8; len as usize];
    // SAFETY: `blob` is exactly `len` bytes long.
    unsafe {
        NCryptExportKey(
            key.0,
            None,
            BCRYPT_ECCPUBLIC_BLOB,
            None,
            Some(&mut blob),
            &mut len,
            NCRYPT_FLAGS(0),
        )
        .map_err(cng)?;
    }
    blob.truncate(len as usize);

    // BCRYPT_ECCKEY_BLOB { dwMagic: u32, cbKey: u32 } followed by X and Y.
    let bad = || PlatformError::Backend("unexpected public key blob".into());
    if blob.len() != 8 + 2 * P256_COORD_LEN {
        return Err(bad());
    }
    let magic = u32::from_le_bytes(blob[0..4].try_into().unwrap());
    let cb_key = u32::from_le_bytes(blob[4..8].try_into().unwrap()) as usize;
    if magic != ECDSA_PUBLIC_P256_MAGIC || cb_key != P256_COORD_LEN {
        return Err(bad());
    }
    Ok(PublicKeyJwk {
        x: b64url(&blob[8..8 + P256_COORD_LEN]),
        y: b64url(&blob[8 + P256_COORD_LEN..]),
    })
}

impl KeyStore for WindowsKeyStore {
    fn public_key(&self) -> PlatformResult<Option<PublicKeyJwk>> {
        let _guard = self.lock.lock().unwrap();
        match self.open_existing(&[KeyBackend::Tpm, KeyBackend::Software])? {
            Some(opened) => export_public(&opened.key).map(Some),
            None => Ok(None),
        }
    }

    fn create_key(&self) -> PlatformResult<PublicKeyJwk> {
        let _guard = self.lock.lock().unwrap();
        if self
            .open_existing(&[KeyBackend::Tpm, KeyBackend::Software])?
            .is_some()
        {
            return Err(PlatformError::KeyExists);
        }
        let mut last_error = None;
        for &backend in self.creation_order() {
            match self.create_in(backend) {
                Ok(opened) => return export_public(&opened.key),
                // The TPM is optional: any failure there falls through to software.
                Err(e) => last_error = Some(e),
            }
        }
        Err(last_error.map(cng).unwrap_or(PlatformError::KeyNotFound))
    }

    fn sign(&self, message: &[u8]) -> PlatformResult<RawSignature> {
        let _guard = self.lock.lock().unwrap();
        let opened = self
            .open_existing(&[KeyBackend::Tpm, KeyBackend::Software])?
            .ok_or(PlatformError::KeyNotFound)?;
        // CNG signs a digest, so hash here.
        let digest = Sha256::digest(message);
        let mut len = 0u32;
        // SAFETY: size query with no output buffer.
        unsafe {
            NCryptSignHash(
                opened.key.0,
                None,
                digest.as_slice(),
                None,
                &mut len,
                NCRYPT_FLAGS(0),
            )
            .map_err(cng)?;
        }
        let mut signature = vec![0u8; len as usize];
        // SAFETY: `signature` is exactly `len` bytes long.
        unsafe {
            NCryptSignHash(
                opened.key.0,
                None,
                digest.as_slice(),
                Some(&mut signature),
                &mut len,
                NCRYPT_FLAGS(0),
            )
            .map_err(cng)?;
        }
        signature.truncate(len as usize);
        Ok(RawSignature {
            // CNG's ECDSA output is r || s already.
            encoding: SignatureEncoding::P1363,
            bytes: signature,
        })
    }

    fn wipe(&self) -> PlatformResult<()> {
        let _guard = self.lock.lock().unwrap();
        // Remove the key wherever it is, whatever provider preference is set.
        while let Some(opened) = self.open_existing(&[KeyBackend::Tpm, KeyBackend::Software])? {
            let Opened { key, _provider, .. } = opened;
            // SAFETY: NCryptDeleteKey consumes and frees the handle, so it must
            // not be freed again: into_raw skips the destructor.
            unsafe { NCryptDeleteKey(key.into_raw(), 0).map_err(cng)? };
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use p256::ecdsa::signature::Verifier;
    use p256::ecdsa::{Signature, VerifyingKey};
    use rb_core::protocol::fingerprint::fingerprint;
    use rb_core::protocol::signing::to_p1363;
    use windows::Win32::Security::Cryptography::{
        BCRYPT_ECCPRIVATE_BLOB, NCRYPT_EXPORT_POLICY_PROPERTY, NCRYPT_PKCS8_PRIVATE_KEY_BLOB,
        NCryptGetProperty,
    };
    use windows::Win32::Security::OBJECT_SECURITY_INFORMATION;

    /// Deletes the key when a test ends, pass or fail.
    struct Cleanup(WindowsKeyStore);

    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = self.0.wipe();
        }
    }

    fn store(tag: &str, preference: ProviderPreference) -> Cleanup {
        let name = format!("RemoteBridge.Test.{}.{tag}", std::process::id());
        let s = WindowsKeyStore::with_preference(&name, KeyScope::User, preference);
        let _ = s.wipe();
        Cleanup(s)
    }

    fn verifying_key(jwk: &PublicKeyJwk) -> VerifyingKey {
        let mut sec1 = vec![0x04];
        sec1.extend(URL_SAFE_NO_PAD.decode(&jwk.x).unwrap());
        sec1.extend(URL_SAFE_NO_PAD.decode(&jwk.y).unwrap());
        VerifyingKey::from_sec1_bytes(&sec1).expect("CNG produced an invalid P-256 point")
    }

    fn assert_signs_verifiably(s: &WindowsKeyStore, jwk: &PublicKeyJwk) {
        let raw = s.sign(b"RA-HOST-V1\nhost/poll").unwrap();
        assert_eq!(raw.encoding, SignatureEncoding::P1363);
        assert_eq!(raw.bytes.len(), 64);
        let p1363 = to_p1363(&raw).unwrap();
        verifying_key(jwk)
            .verify(
                b"RA-HOST-V1\nhost/poll",
                &Signature::from_slice(&p1363).unwrap(),
            )
            .expect("signature must verify like the server's WebCrypto verify");
    }

    #[test]
    fn software_key_lifecycle() {
        let t = store("software", ProviderPreference::SoftwareOnly);
        let s = &t.0;
        assert_eq!(s.public_key(), Ok(None));
        assert_eq!(s.backend(), Ok(None));
        assert_eq!(s.sign(b"x"), Err(PlatformError::KeyNotFound));

        let jwk = s.create_key().unwrap();
        assert_eq!(URL_SAFE_NO_PAD.decode(&jwk.x).unwrap().len(), 32);
        assert_eq!(URL_SAFE_NO_PAD.decode(&jwk.y).unwrap().len(), 32);
        assert_eq!(fingerprint(&jwk).unwrap().len(), 64);
        assert_eq!(s.backend(), Ok(Some(KeyBackend::Software)));
        assert_eq!(s.public_key().unwrap(), Some(jwk.clone()));
        assert_eq!(s.create_key(), Err(PlatformError::KeyExists));
        assert_signs_verifiably(s, &jwk);

        s.wipe().unwrap();
        s.wipe().unwrap();
        assert_eq!(s.public_key(), Ok(None));
        assert_eq!(s.sign(b"x"), Err(PlatformError::KeyNotFound));
    }

    #[test]
    fn key_persists_across_store_instances_and_wipe_removes_it_for_all() {
        let t = store("persist", ProviderPreference::SoftwareOnly);
        let jwk = t.0.create_key().unwrap();

        let other = WindowsKeyStore::with_preference(
            "unused",
            KeyScope::User,
            ProviderPreference::SoftwareOnly,
        );
        assert_eq!(
            other.public_key(),
            Ok(None),
            "different name, different key"
        );

        let same_name = format!("RemoteBridge.Test.{}.persist", std::process::id());
        let again = WindowsKeyStore::with_preference(
            &same_name,
            KeyScope::User,
            ProviderPreference::SoftwareOnly,
        );
        assert_eq!(again.public_key().unwrap(), Some(jwk.clone()));
        assert_signs_verifiably(&again, &jwk);

        again.wipe().unwrap();
        assert_eq!(t.0.public_key(), Ok(None));
    }

    #[test]
    fn the_private_key_cannot_be_exported() {
        let t = store("export", ProviderPreference::SoftwareOnly);
        t.0.create_key().unwrap();
        let opened =
            t.0.open_existing(&[KeyBackend::Software])
                .unwrap()
                .expect("key just created");
        assert_private_key_not_exportable(&opened.key);

        // The export policy property reads 0: no export of any kind allowed.
        let mut policy = [0xffu8; 4];
        let mut len = 0u32;
        // SAFETY: `policy` is a valid 4-byte output buffer.
        unsafe {
            NCryptGetProperty(
                NCRYPT_HANDLE(opened.key.0.0),
                NCRYPT_EXPORT_POLICY_PROPERTY,
                Some(&mut policy),
                &mut len,
                OBJECT_SECURITY_INFORMATION(0),
            )
            .unwrap();
        }
        assert_eq!(u32::from_le_bytes(policy), 0, "export policy must be 0");
    }

    /// Plain and PKCS#8 private-key exports must both be refused.
    fn assert_private_key_not_exportable(key: &Key) {
        for blob_type in [BCRYPT_ECCPRIVATE_BLOB, NCRYPT_PKCS8_PRIVATE_KEY_BLOB] {
            let mut buf = vec![0u8; 512];
            let mut len = 0u32;
            // SAFETY: `buf` is a valid output buffer of the stated size.
            let result = unsafe {
                NCryptExportKey(
                    key.0,
                    None,
                    blob_type,
                    None,
                    Some(&mut buf),
                    &mut len,
                    NCRYPT_FLAGS(0),
                )
            };
            assert!(
                result.is_err(),
                "private key export unexpectedly succeeded ({len} bytes)"
            );
        }
    }

    /// Uses the TPM when this machine has one; reports which backend it got.
    #[test]
    fn auto_preference_works_and_keeps_the_key_non_exportable() {
        let t = store("auto", ProviderPreference::Auto);
        let jwk = t.0.create_key().unwrap();
        let backend = t.0.backend().unwrap().unwrap();
        eprintln!("device key backend on this machine: {backend:?}");
        assert_signs_verifiably(&t.0, &jwk);

        let opened =
            t.0.open_existing(&[KeyBackend::Tpm, KeyBackend::Software])
                .unwrap()
                .unwrap();
        assert_eq!(opened.backend, backend);
        assert_private_key_not_exportable(&opened.key);
    }

    #[test]
    fn two_different_keys_do_not_share_signatures() {
        let a = store("a", ProviderPreference::SoftwareOnly);
        let b = store("b", ProviderPreference::SoftwareOnly);
        let ja = a.0.create_key().unwrap();
        let jb = b.0.create_key().unwrap();
        assert_ne!(ja, jb);
        let sig = a.0.sign(b"m").unwrap();
        let p1363 = to_p1363(&sig).unwrap();
        assert!(
            verifying_key(&jb)
                .verify(b"m", &Signature::from_slice(&p1363).unwrap())
                .is_err()
        );
    }

    /// Machine-wide keys may need an elevated process, so this is run by hand:
    /// `cargo test -p rb-platform-windows machine_scope -- --ignored --nocapture`
    #[test]
    #[ignore = "may need an elevated shell"]
    fn machine_scope_key_roundtrip() {
        let name = format!("RemoteBridge.Test.Machine.{}", std::process::id());
        let s = WindowsKeyStore::with_preference(
            &name,
            KeyScope::Machine,
            ProviderPreference::SoftwareOnly,
        );
        let _cleanup = Cleanup(WindowsKeyStore::with_preference(
            &name,
            KeyScope::Machine,
            ProviderPreference::SoftwareOnly,
        ));
        match s.create_key() {
            Ok(jwk) => {
                eprintln!("machine-scope key created");
                assert_signs_verifiably(&s, &jwk);
            }
            Err(e) => eprintln!("machine-scope key creation refused: {e}"),
        }
    }
}

//! `KeyStore` implemented by Swift through C callbacks.
//!
//! Swift owns the key (Secure Enclave, or a non-extractable Keychain key as
//! the fallback). Rust never sees private key material: it asks Swift for the
//! public coordinates and for signatures. `SecKeyCreateSignature` returns
//! ASN.1 DER, so signatures are tagged `Der` and the protocol client converts
//! them to P1363.

use std::ffi::c_void;

use rb_core::protocol::b64url;
use rb_core::traits::KeyStore;
use rb_core::types::{PublicKeyJwk, RawSignature, SignatureEncoding};
use rb_core::{PlatformError, PlatformResult};

/// Callback succeeded.
pub const RB_OK: i32 = 0;
/// There is no device key.
pub const RB_ERR_NO_KEY: i32 = 1;
/// Any other failure inside Swift or the OS.
pub const RB_ERR_BACKEND: i32 = 2;
/// `create_key` found an existing key.
pub const RB_ERR_KEY_EXISTS: i32 = 3;
/// The output buffer given to `sign` was too small.
pub const RB_ERR_BUFFER: i32 = 4;

/// A DER-encoded P-256 ECDSA signature is at most 72 bytes.
const SIGNATURE_BUFFER: usize = 80;
const COORD_LEN: usize = 32;

/// Callbacks implemented by Swift. Each returns one of the `RB_*` codes.
///
/// `out_x` / `out_y` point to 32 writable bytes each (big-endian P-256
/// coordinates). `sign` writes at most `out_cap` bytes of DER to `out_sig`
/// and the length to `out_len`. All callbacks must be safe to call from any
/// thread, and the table (including `user_data`) must outlive the store.
#[repr(C)]
pub struct RbKeyStoreCallbacks {
    pub user_data: *mut c_void,
    pub public_key: extern "C" fn(user_data: *mut c_void, out_x: *mut u8, out_y: *mut u8) -> i32,
    pub create_key: extern "C" fn(user_data: *mut c_void, out_x: *mut u8, out_y: *mut u8) -> i32,
    pub sign: extern "C" fn(
        user_data: *mut c_void,
        message: *const u8,
        message_len: usize,
        out_sig: *mut u8,
        out_cap: usize,
        out_len: *mut usize,
    ) -> i32,
    pub wipe: extern "C" fn(user_data: *mut c_void) -> i32,
}

pub struct ForeignKeyStore {
    callbacks: RbKeyStoreCallbacks,
}

// SAFETY: the callback contract above requires thread-safe callbacks and a
// `user_data` that outlives the store; the table itself is never mutated.
unsafe impl Send for ForeignKeyStore {}
unsafe impl Sync for ForeignKeyStore {}

impl ForeignKeyStore {
    /// # Safety
    /// The callbacks must follow the contract on [`RbKeyStoreCallbacks`].
    pub unsafe fn new(callbacks: RbKeyStoreCallbacks) -> Self {
        Self { callbacks }
    }
}

fn to_error(code: i32) -> PlatformError {
    match code {
        RB_ERR_NO_KEY => PlatformError::KeyNotFound,
        RB_ERR_KEY_EXISTS => PlatformError::KeyExists,
        RB_ERR_BUFFER => PlatformError::Backend("signature buffer too small".into()),
        _ => PlatformError::Backend(format!("keystore callback failed ({code})")),
    }
}

fn jwk_from(x: &[u8; COORD_LEN], y: &[u8; COORD_LEN]) -> PublicKeyJwk {
    PublicKeyJwk {
        x: b64url(x),
        y: b64url(y),
    }
}

impl KeyStore for ForeignKeyStore {
    fn public_key(&self) -> PlatformResult<Option<PublicKeyJwk>> {
        let (mut x, mut y) = ([0u8; COORD_LEN], [0u8; COORD_LEN]);
        let code =
            (self.callbacks.public_key)(self.callbacks.user_data, x.as_mut_ptr(), y.as_mut_ptr());
        match code {
            RB_OK => Ok(Some(jwk_from(&x, &y))),
            RB_ERR_NO_KEY => Ok(None),
            other => Err(to_error(other)),
        }
    }

    fn create_key(&self) -> PlatformResult<PublicKeyJwk> {
        let (mut x, mut y) = ([0u8; COORD_LEN], [0u8; COORD_LEN]);
        let code =
            (self.callbacks.create_key)(self.callbacks.user_data, x.as_mut_ptr(), y.as_mut_ptr());
        match code {
            RB_OK => Ok(jwk_from(&x, &y)),
            other => Err(to_error(other)),
        }
    }

    fn sign(&self, message: &[u8]) -> PlatformResult<RawSignature> {
        let mut buf = [0u8; SIGNATURE_BUFFER];
        let mut len = 0usize;
        let code = (self.callbacks.sign)(
            self.callbacks.user_data,
            message.as_ptr(),
            message.len(),
            buf.as_mut_ptr(),
            buf.len(),
            &mut len,
        );
        if code != RB_OK {
            return Err(to_error(code));
        }
        if len == 0 || len > buf.len() {
            return Err(PlatformError::Backend("bad signature length".into()));
        }
        Ok(RawSignature {
            encoding: SignatureEncoding::Der,
            bytes: buf[..len].to_vec(),
        })
    }

    fn wipe(&self) -> PlatformResult<()> {
        match (self.callbacks.wipe)(self.callbacks.user_data) {
            RB_OK => Ok(()),
            other => Err(to_error(other)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rb_core::protocol::signing::to_p1363;
    use std::sync::Mutex;

    /// Stand-in for the Swift side.
    #[derive(Default)]
    struct Mock {
        has_key: bool,
        wipes: u32,
        last_message: Vec<u8>,
        fail_sign_with: Option<i32>,
    }

    fn mock(user_data: *mut c_void) -> &'static Mutex<Mock> {
        // SAFETY: tests pass a pointer to a leaked `Mutex<Mock>`.
        unsafe { &*(user_data as *const Mutex<Mock>) }
    }

    extern "C" fn cb_public(ud: *mut c_void, x: *mut u8, y: *mut u8) -> i32 {
        if !mock(ud).lock().unwrap().has_key {
            return RB_ERR_NO_KEY;
        }
        // SAFETY: the caller provides 32 writable bytes per coordinate.
        unsafe {
            std::ptr::write_bytes(x, 1, COORD_LEN);
            std::ptr::write_bytes(y, 2, COORD_LEN);
        }
        RB_OK
    }

    extern "C" fn cb_create(ud: *mut c_void, x: *mut u8, y: *mut u8) -> i32 {
        let mut m = mock(ud).lock().unwrap();
        if m.has_key {
            return RB_ERR_KEY_EXISTS;
        }
        m.has_key = true;
        drop(m);
        cb_public(ud, x, y)
    }

    extern "C" fn cb_sign(
        ud: *mut c_void,
        msg: *const u8,
        msg_len: usize,
        out: *mut u8,
        cap: usize,
        out_len: *mut usize,
    ) -> i32 {
        let mut m = mock(ud).lock().unwrap();
        if let Some(code) = m.fail_sign_with {
            return code;
        }
        if !m.has_key {
            return RB_ERR_NO_KEY;
        }
        // SAFETY: `msg` is valid for `msg_len` bytes.
        m.last_message = unsafe { std::slice::from_raw_parts(msg, msg_len) }.to_vec();
        // SEQUENCE { INTEGER 1, INTEGER 2 }
        let der = [0x30, 0x06, 0x02, 0x01, 0x01, 0x02, 0x01, 0x02];
        if cap < der.len() {
            return RB_ERR_BUFFER;
        }
        // SAFETY: `out` has `cap >= der.len()` writable bytes; `out_len` is valid.
        unsafe {
            std::ptr::copy_nonoverlapping(der.as_ptr(), out, der.len());
            *out_len = der.len();
        }
        RB_OK
    }

    extern "C" fn cb_wipe(ud: *mut c_void) -> i32 {
        let mut m = mock(ud).lock().unwrap();
        m.has_key = false;
        m.wipes += 1;
        RB_OK
    }

    fn store() -> (ForeignKeyStore, &'static Mutex<Mock>) {
        let state: &'static Mutex<Mock> = Box::leak(Box::new(Mutex::new(Mock::default())));
        let callbacks = RbKeyStoreCallbacks {
            user_data: state as *const Mutex<Mock> as *mut c_void,
            public_key: cb_public,
            create_key: cb_create,
            sign: cb_sign,
            wipe: cb_wipe,
        };
        // SAFETY: the mock callbacks follow the contract and the state is leaked.
        (unsafe { ForeignKeyStore::new(callbacks) }, state)
    }

    #[test]
    fn lifecycle_goes_through_the_callbacks() {
        let (ks, state) = store();
        assert_eq!(ks.public_key(), Ok(None));
        assert_eq!(ks.sign(b"x"), Err(PlatformError::KeyNotFound));

        let jwk = ks.create_key().unwrap();
        assert_eq!(jwk.x, b64url(&[1u8; 32]));
        assert_eq!(jwk.y, b64url(&[2u8; 32]));
        assert_eq!(jwk.x.len(), 43);
        assert_eq!(ks.public_key().unwrap(), Some(jwk));
        assert_eq!(ks.create_key(), Err(PlatformError::KeyExists));

        ks.wipe().unwrap();
        assert_eq!(ks.public_key(), Ok(None));
        assert_eq!(state.lock().unwrap().wipes, 1);
    }

    #[test]
    fn signatures_are_tagged_der_and_convert_to_p1363() {
        let (ks, state) = store();
        ks.create_key().unwrap();
        let sig = ks.sign(b"RA-HOST-V1\nhost/poll").unwrap();
        assert_eq!(sig.encoding, SignatureEncoding::Der);
        assert_eq!(state.lock().unwrap().last_message, b"RA-HOST-V1\nhost/poll");

        let p1363 = to_p1363(&sig).unwrap();
        assert_eq!(p1363[31], 1, "r");
        assert_eq!(p1363[63], 2, "s");
    }

    #[test]
    fn callback_failures_become_typed_errors() {
        let (ks, state) = store();
        ks.create_key().unwrap();
        for (code, expected) in [
            (RB_ERR_NO_KEY, PlatformError::KeyNotFound),
            (
                RB_ERR_BUFFER,
                PlatformError::Backend("signature buffer too small".into()),
            ),
            (
                RB_ERR_BACKEND,
                PlatformError::Backend("keystore callback failed (2)".into()),
            ),
        ] {
            state.lock().unwrap().fail_sign_with = Some(code);
            assert_eq!(ks.sign(b"m"), Err(expected));
        }
    }

    #[test]
    fn store_can_be_shared_across_threads() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<ForeignKeyStore>();
    }
}

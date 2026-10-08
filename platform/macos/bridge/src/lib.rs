//! C ABI exposed to the macOS Swift app (`platform/macos`).
//!
//! The header is generated with cbindgen (`tools/gen-macos-header.sh`).
//!
//! Swift implements the platform traits (Secure Enclave key, ScreenCaptureKit,
//! CGEvent, menu bar) and hands the Rust core a table of C callbacks; the
//! Rust side wraps each table in a type that implements the matching
//! `rb-core` trait. Done so far: [`ForeignKeyStore`] for `KeyStore`.

mod key_store;

use std::ffi::{CStr, c_char};

pub use key_store::{
    ForeignKeyStore, RB_ERR_BACKEND, RB_ERR_BUFFER, RB_ERR_KEY_EXISTS, RB_ERR_NO_KEY, RB_OK,
    RbKeyStoreCallbacks,
};

static VERSION: &CStr = c"0.1.0";

/// Version of the Rust core. The returned pointer is static and must not be freed.
#[unsafe(no_mangle)]
pub extern "C" fn rb_core_version() -> *const c_char {
    VERSION.as_ptr()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_matches_crate_version() {
        // SAFETY: rb_core_version returns a pointer to a static NUL-terminated string.
        let v = unsafe { CStr::from_ptr(rb_core_version()) };
        assert_eq!(v.to_str().unwrap(), env!("CARGO_PKG_VERSION"));
    }
}

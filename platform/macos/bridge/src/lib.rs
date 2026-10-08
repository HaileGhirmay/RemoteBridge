//! C ABI exposed to the macOS Swift app (`platform/macos`).
//!
//! The header is generated with cbindgen (`tools/gen-macos-header.sh`). The
//! real surface (session manager handle, callbacks for the platform traits)
//! is added as the Swift adapters arrive in Prompts 03, 05 and 10.

use std::ffi::{CStr, c_char};

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

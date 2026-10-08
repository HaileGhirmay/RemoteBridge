use super::b64url;

/// Source of request nonces. Every signed request - including every retry -
/// takes a fresh one; the server rejects a reused nonce as a replay.
pub trait NonceSource: Send + Sync {
    /// 16-64 base64url characters.
    fn next(&self) -> String;
}

/// 18 random bytes from the OS, base64url encoded (24 characters).
#[derive(Debug, Clone, Copy, Default)]
pub struct OsNonceSource;

impl NonceSource for OsNonceSource {
    fn next(&self) -> String {
        let mut bytes = [0u8; 18];
        getrandom::fill(&mut bytes).expect("OS random source unavailable");
        b64url(&bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn nonce_is_24_base64url_chars() {
        let n = OsNonceSource.next();
        assert_eq!(n.len(), 24);
        assert!(
            n.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        );
    }

    #[test]
    fn nonces_do_not_repeat() {
        let seen: HashSet<String> = (0..1000).map(|_| OsNonceSource.next()).collect();
        assert_eq!(seen.len(), 1000);
    }
}

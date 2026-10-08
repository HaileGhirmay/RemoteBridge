use crate::types::PublicKeyJwk;

use super::error::HostError;
use super::sha256_hex;

/// The exact string that is hashed: `{"crv":"P-256","kty":"EC","x":"..","y":".."}`
/// in that key order, no spaces. Matches the server's `JSON.stringify` of
/// `{crv, kty, x, y}`.
pub fn canonical_jwk(jwk: &PublicKeyJwk) -> Result<String, HostError> {
    for (name, v) in [("x", &jwk.x), ("y", &jwk.y)] {
        let ok = !v.is_empty()
            && v.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
        if !ok {
            return Err(HostError::Codec(format!(
                "JWK coordinate {name} is not base64url"
            )));
        }
    }
    Ok(format!(
        r#"{{"crv":"P-256","kty":"EC","x":"{}","y":"{}"}}"#,
        jwk.x, jwk.y
    ))
}

/// Lowercase hex SHA-256 of [`canonical_jwk`].
pub fn fingerprint(jwk: &PublicKeyJwk) -> Result<String, HostError> {
    Ok(sha256_hex(canonical_jwk(jwk)?.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Generated independently with Node: crypto.generateKeyPairSync("ec"),
    // then sha256(JSON.stringify({crv, kty, x, y})) - the server's algorithm.
    const X: &str = "I3SrxzEVbYyO3c1-MGH0pYeRrfTq69UAnknmI9rgphs";
    const Y: &str = "ShcSRupi9e77U5_PCcH9ABXTUWof5IuUbsEogG_m5fE";
    const FP: &str = "1403057ac72180c9b1911d4d8b568ffdac49ae5d022b088687314c7e6ab47375";

    fn jwk() -> PublicKeyJwk {
        PublicKeyJwk {
            x: X.into(),
            y: Y.into(),
        }
    }

    #[test]
    fn canonical_string_has_exact_key_order_and_no_spaces() {
        assert_eq!(
            canonical_jwk(&jwk()).unwrap(),
            format!(r#"{{"crv":"P-256","kty":"EC","x":"{X}","y":"{Y}"}}"#)
        );
    }

    #[test]
    fn matches_server_algorithm() {
        assert_eq!(fingerprint(&jwk()).unwrap(), FP);
    }

    #[test]
    fn rejects_coordinates_that_are_not_base64url() {
        let bad = PublicKeyJwk {
            x: "a\"b".into(),
            y: Y.into(),
        };
        assert!(fingerprint(&bad).is_err());
        let empty = PublicKeyJwk {
            x: X.into(),
            y: String::new(),
        };
        assert!(fingerprint(&empty).is_err());
    }
}

use jsonwebtoken::errors::ErrorKind;
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode};
use serde::Deserialize;

pub const ISSUER: &str = "remotebridge";
pub const AUDIENCE: &str = "rb-signaling";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Host,
    Viewer,
}

impl Role {
    pub fn other(self) -> Self {
        match self {
            Self::Host => Self::Viewer,
            Self::Viewer => Self::Host,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Host => "host",
            Self::Viewer => "viewer",
        }
    }
}

/// What the website put in the token.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Claims {
    /// Session id: names the room.
    pub sid: String,
    pub role: Role,
    /// Own DTLS fingerprint. Peers compare fingerprints themselves; the relay
    /// never looks at SDP.
    #[serde(default)]
    pub fp: Option<String>,
    /// Expected peer fingerprint.
    #[serde(default)]
    pub pfp: Option<String>,
    /// Expiry, seconds since the Unix epoch.
    pub exp: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthError {
    Expired,
    Invalid,
}

/// Check signature, issuer, audience and expiry, and the shape of `sid`.
pub fn verify_token(token: &str, secret: &[u8]) -> Result<Claims, AuthError> {
    let mut validation = Validation::new(Algorithm::HS256);
    validation.set_issuer(&[ISSUER]);
    validation.set_audience(&[AUDIENCE]);
    validation.set_required_spec_claims(&["exp", "iss", "aud"]);
    validation.leeway = 0;
    let data = decode::<Claims>(token, &DecodingKey::from_secret(secret), &validation).map_err(
        |e| match e.kind() {
            ErrorKind::ExpiredSignature => AuthError::Expired,
            _ => AuthError::Invalid,
        },
    )?;
    let sid = &data.claims.sid;
    let sid_ok = !sid.is_empty()
        && sid.len() <= 64
        && sid
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    if !sid_ok {
        return Err(AuthError::Invalid);
    }
    Ok(data.claims)
}

#[cfg(test)]
mod tests {
    use super::*;
    use jsonwebtoken::{EncodingKey, Header, encode};
    use serde_json::json;
    use std::time::{SystemTime, UNIX_EPOCH};

    const SECRET: &[u8] = b"test-secret-test-secret-test-secret";

    fn now() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    fn mint(claims: serde_json::Value, secret: &[u8], alg: jsonwebtoken::Algorithm) -> String {
        encode(
            &Header::new(alg),
            &claims,
            &EncodingKey::from_secret(secret),
        )
        .unwrap()
    }

    fn good() -> serde_json::Value {
        json!({"iss": "remotebridge", "aud": "rb-signaling", "exp": now() + 300,
               "sid": "6f1c3d7e-2b4a-4c8e-9d10-0a1b2c3d4e5f", "role": "host",
               "fp": "sha-256 AA", "pfp": "sha-256 BB"})
    }

    #[test]
    fn accepts_a_good_token() {
        let c = verify_token(&mint(good(), SECRET, Algorithm::HS256), SECRET).unwrap();
        assert_eq!(c.role, Role::Host);
        assert_eq!(c.sid, "6f1c3d7e-2b4a-4c8e-9d10-0a1b2c3d4e5f");
        assert_eq!(c.fp.as_deref(), Some("sha-256 AA"));
        assert_eq!(c.pfp.as_deref(), Some("sha-256 BB"));
    }

    #[test]
    fn rejects_the_wrong_secret_issuer_audience_or_role() {
        let t = mint(good(), SECRET, Algorithm::HS256);
        assert_eq!(
            verify_token(&t, b"another-secret-another-secret-xx"),
            Err(AuthError::Invalid)
        );

        for (key, value) in [
            ("iss", json!("someone-else")),
            ("aud", json!("something-else")),
            ("role", json!("admin")),
            ("sid", json!("")),
            ("sid", json!("../../etc")),
            ("sid", json!("x".repeat(65))),
        ] {
            let mut claims = good();
            claims[key] = value.clone();
            let t = mint(claims, SECRET, Algorithm::HS256);
            assert_eq!(
                verify_token(&t, SECRET),
                Err(AuthError::Invalid),
                "{key}={value}"
            );
        }
    }

    #[test]
    fn rejects_an_expired_token_and_a_missing_expiry() {
        let mut claims = good();
        claims["exp"] = json!(now() - 1);
        let t = mint(claims, SECRET, Algorithm::HS256);
        assert_eq!(verify_token(&t, SECRET), Err(AuthError::Expired));

        let mut claims = good();
        claims.as_object_mut().unwrap().remove("exp");
        let t = mint(claims, SECRET, Algorithm::HS256);
        assert_eq!(verify_token(&t, SECRET), Err(AuthError::Invalid));
    }

    #[test]
    fn rejects_other_algorithms_and_garbage() {
        // HS512 with the right secret is still not HS256.
        let t = mint(good(), SECRET, Algorithm::HS512);
        assert_eq!(verify_token(&t, SECRET), Err(AuthError::Invalid));
        for junk in ["", "abc", "a.b.c", "eyJhbGciOiJub25lIn0.e30."] {
            assert_eq!(
                verify_token(junk, SECRET),
                Err(AuthError::Invalid),
                "{junk}"
            );
        }
    }

    #[test]
    fn fingerprints_are_optional() {
        let mut claims = good();
        claims.as_object_mut().unwrap().remove("fp");
        claims.as_object_mut().unwrap().remove("pfp");
        let c = verify_token(&mint(claims, SECRET, Algorithm::HS256), SECRET).unwrap();
        assert_eq!((c.fp, c.pfp), (None, None));
    }
}

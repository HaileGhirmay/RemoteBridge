//! What gets signed, and how signatures are normalised for the server.

use crate::types::{RawSignature, SignatureEncoding};

use super::error::HostError;
use super::{b64url, sha256_hex};

/// Length of an IEEE P1363 P-256 signature: `r || s`, 32 bytes each.
pub const P1363_LEN: usize = 64;

/// `RA-HOST-V1\n<route>\n<deviceId>\n<timestampMs>\n<nonce>\n<sha256hex(rawBody)>`
///
/// `route` has no prefix (`host/poll`). `raw_body` is the exact bytes sent.
pub fn request_message(
    route: &str,
    device_id: &str,
    timestamp_ms: i64,
    nonce: &str,
    raw_body: &[u8],
) -> String {
    format!(
        "RA-HOST-V1\n{route}\n{device_id}\n{timestamp_ms}\n{nonce}\n{}",
        sha256_hex(raw_body)
    )
}

/// `RA-ENROLL-V1\n<code>\n<fingerprint>`
pub fn enroll_message(code: &str, fingerprint: &str) -> String {
    format!("RA-ENROLL-V1\n{code}\n{fingerprint}")
}

/// base64url (no padding) of a signature normalised to P1363.
pub fn encode_signature(sig: &RawSignature) -> Result<String, HostError> {
    Ok(b64url(&to_p1363(sig)?))
}

/// Normalise to P1363 (`r || s`, 64 bytes). DER input is converted.
pub fn to_p1363(sig: &RawSignature) -> Result<[u8; P1363_LEN], HostError> {
    match sig.encoding {
        SignatureEncoding::P1363 => sig.bytes.as_slice().try_into().map_err(|_| {
            HostError::Codec(format!(
                "P1363 signature must be {P1363_LEN} bytes, got {}",
                sig.bytes.len()
            ))
        }),
        SignatureEncoding::Der => der_to_p1363(&sig.bytes),
    }
}

/// Convert an ASN.1 DER ECDSA signature (`SEQUENCE { INTEGER r, INTEGER s }`),
/// as returned by OpenSSL and Security.framework, to P1363 for P-256.
pub fn der_to_p1363(der: &[u8]) -> Result<[u8; P1363_LEN], HostError> {
    let bad = |what: &str| HostError::Codec(format!("invalid DER signature: {what}"));

    let (tag, body, rest) = read_tlv(der).ok_or_else(|| bad("truncated"))?;
    if tag != 0x30 {
        return Err(bad("not a SEQUENCE"));
    }
    if !rest.is_empty() {
        return Err(bad("trailing bytes"));
    }
    let (r, body) = read_integer(body).ok_or_else(|| bad("bad r"))?;
    let (s, body) = read_integer(body).ok_or_else(|| bad("bad s"))?;
    if !body.is_empty() {
        return Err(bad("extra data in SEQUENCE"));
    }

    let mut out = [0u8; P1363_LEN];
    place(r, &mut out[..32]).ok_or_else(|| bad("r does not fit 32 bytes"))?;
    place(s, &mut out[32..]).ok_or_else(|| bad("s does not fit 32 bytes"))?;
    Ok(out)
}

/// Read one tag-length-value; returns (tag, value, remainder).
fn read_tlv(input: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let (&tag, rest) = input.split_first()?;
    let (&first, rest) = rest.split_first()?;
    let (len, rest) = if first < 0x80 {
        (usize::from(first), rest)
    } else {
        let n = usize::from(first & 0x7f);
        // Signatures are at most 72 bytes, so one length byte is the only
        // sensible long form.
        if n != 1 || rest.is_empty() || rest[0] < 0x80 {
            return None;
        }
        (usize::from(rest[0]), &rest[1..])
    };
    if rest.len() < len {
        return None;
    }
    let (value, remainder) = rest.split_at(len);
    Some((tag, value, remainder))
}

/// Read a positive DER INTEGER, returning its magnitude without leading zeros.
fn read_integer(input: &[u8]) -> Option<(&[u8], &[u8])> {
    let (tag, value, rest) = read_tlv(input)?;
    if tag != 0x02 || value.is_empty() {
        return None;
    }
    if value[0] & 0x80 != 0 {
        return None; // negative
    }
    let mut v = value;
    while v.len() > 1 && v[0] == 0 {
        v = &v[1..];
    }
    Some((v, rest))
}

/// Right-align `magnitude` in `dst`, zero-padding on the left.
fn place(magnitude: &[u8], dst: &mut [u8]) -> Option<()> {
    let magnitude = match magnitude {
        [0] => &[][..],
        m => m,
    };
    if magnitude.len() > dst.len() {
        return None;
    }
    let offset = dst.len() - magnitude.len();
    dst[offset..].copy_from_slice(magnitude);
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use p256::ecdsa::signature::Signer;
    use p256::ecdsa::{Signature, SigningKey};

    fn der(r: &[u8], s: &[u8]) -> Vec<u8> {
        let mut body = vec![0x02, u8::try_from(r.len()).unwrap()];
        body.extend_from_slice(r);
        body.extend_from_slice(&[0x02, u8::try_from(s.len()).unwrap()]);
        body.extend_from_slice(s);
        let mut out = vec![0x30, u8::try_from(body.len()).unwrap()];
        out.extend(body);
        out
    }

    #[test]
    fn message_has_the_documented_layout() {
        let msg = request_message("host/poll", "dev-1", 1_700_000_000_123, "nonce", b"");
        assert_eq!(
            msg,
            "RA-HOST-V1\nhost/poll\ndev-1\n1700000000123\nnonce\n\
             e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn message_binds_the_exact_body_bytes() {
        let a = request_message("host/poll", "d", 1, "n", br#"{"json":{}}"#);
        let b = request_message("host/poll", "d", 1, "n", br#"{"json": {}}"#);
        assert_ne!(a, b);
    }

    #[test]
    fn enroll_message_layout() {
        assert_eq!(
            enroll_message("12345678", "abcd"),
            "RA-ENROLL-V1\n12345678\nabcd"
        );
    }

    #[test]
    fn der_short_integers_are_left_padded() {
        let out = der_to_p1363(&der(&[0x01, 0x02], &[0x7f])).unwrap();
        let mut expect = [0u8; 64];
        expect[30] = 0x01;
        expect[31] = 0x02;
        expect[63] = 0x7f;
        assert_eq!(out, expect);
    }

    #[test]
    fn der_leading_zero_for_high_bit_is_stripped() {
        let mut r = vec![0x00, 0x80];
        r.extend([0xaa; 31]);
        let out = der_to_p1363(&der(&r, &[0x01])).unwrap();
        assert_eq!(out[0], 0x80);
        assert_eq!(&out[1..32], &[0xaa; 31]);
        assert_eq!(out[63], 0x01);
    }

    #[test]
    fn der_zero_value_becomes_all_zero_bytes() {
        let out = der_to_p1363(&der(&[0x00], &[0x01])).unwrap();
        assert_eq!(&out[..32], &[0u8; 32]);
    }

    #[test]
    fn der_rejects_malformed_input() {
        let ok = der(&[1], &[2]);
        assert!(der_to_p1363(&ok).is_ok());
        assert!(der_to_p1363(&[]).is_err());
        assert!(der_to_p1363(&ok[..ok.len() - 1]).is_err(), "truncated");
        let mut trailing = ok.clone();
        trailing.push(0);
        assert!(der_to_p1363(&trailing).is_err(), "trailing byte");
        let mut wrong_tag = ok.clone();
        wrong_tag[0] = 0x31;
        assert!(der_to_p1363(&wrong_tag).is_err(), "not a SEQUENCE");
        assert!(der_to_p1363(&der(&[0x80], &[1])).is_err(), "negative r");
        assert!(der_to_p1363(&der(&[], &[1])).is_err(), "empty r");
        assert!(der_to_p1363(&der(&[1; 33], &[1])).is_err(), "r too big");
    }

    #[test]
    fn p1363_must_be_64_bytes() {
        let bad = RawSignature {
            encoding: SignatureEncoding::P1363,
            bytes: vec![0; 63],
        };
        assert!(to_p1363(&bad).is_err());
        let good = RawSignature {
            encoding: SignatureEncoding::P1363,
            bytes: vec![7; 64],
        };
        assert_eq!(to_p1363(&good).unwrap(), [7u8; 64]);
    }

    /// DER from a real ECDSA implementation converts to the same bytes as the
    /// library's own P1363 output, so the server's WebCrypto verify accepts it.
    #[test]
    fn real_der_signatures_convert_to_the_p1363_form() {
        let key = SigningKey::from_slice(&[0x42; 32]).unwrap();
        for i in 0..64u8 {
            let sig: Signature = key.sign(&[i; 17]);
            let converted = der_to_p1363(sig.to_der().as_bytes()).unwrap();
            assert_eq!(converted.as_slice(), sig.to_bytes().as_slice());
        }
    }

    #[test]
    fn encode_signature_is_86_base64url_chars() {
        let sig = RawSignature {
            encoding: SignatureEncoding::P1363,
            bytes: vec![0xff; 64],
        };
        let s = encode_signature(&sig).unwrap();
        assert_eq!(s.len(), 86);
        assert!(!s.contains('=') && !s.contains('+') && !s.contains('/'));
    }
}

//! DTLS certificate fingerprints in the format the contract requires.
//!
//! `sha-256 AB:CD:…`: the algorithm, one space, then 32 upper-case hex pairs
//! separated by colons, exactly as in an SDP `a=fingerprint` line. The host
//! sends its own as `hostFingerprint` in `host/decide`; the browser refuses to
//! connect if it is not in this format. Each side compares the peer's
//! fingerprint in the SDP with the one the server vouched for, and drops the
//! call on any mismatch.

/// Number of bytes in a SHA-256 digest.
const SHA256_LEN: usize = 32;

/// Is `value` exactly `sha-256 AB:CD:…` with 32 upper-case hex pairs?
pub fn is_valid_format(value: &str) -> bool {
    let Some(hex) = value.strip_prefix("sha-256 ") else {
        return false;
    };
    let pairs: Vec<&str> = hex.split(':').collect();
    pairs.len() == SHA256_LEN
        && pairs.iter().all(|p| {
            p.len() == 2
                && p.bytes()
                    .all(|b| b.is_ascii_digit() || (b'A'..=b'F').contains(&b))
        })
}

/// Format a raw SHA-256 digest.
pub fn format_sha256(digest: &[u8; SHA256_LEN]) -> String {
    let pairs: Vec<String> = digest.iter().map(|b| format!("{b:02X}")).collect();
    format!("sha-256 {}", pairs.join(":"))
}

/// Normalize `value` (any case, `sha-256` or `SHA-256`) to the required
/// format, or `None` if it is not a SHA-256 fingerprint.
pub fn normalize(value: &str) -> Option<String> {
    let (algorithm, hex) = value.trim().split_once(' ')?;
    if !algorithm.eq_ignore_ascii_case("sha-256") {
        return None;
    }
    let upper = hex.trim().to_ascii_uppercase();
    let candidate = format!("sha-256 {upper}");
    is_valid_format(&candidate).then_some(candidate)
}

/// Every fingerprint offered in an SDP, normalized.
pub fn fingerprints_in_sdp(sdp: &str) -> Vec<String> {
    sdp.lines()
        .filter_map(|line| line.trim().strip_prefix("a=fingerprint:"))
        .filter_map(normalize)
        .collect()
}

/// Does the SDP carry the fingerprint the server vouched for, and no other?
///
/// An SDP with several fingerprints is accepted only if all of them equal the
/// expected one: a peer must not be able to smuggle a second identity in.
pub fn sdp_matches(sdp: &str, expected: &str) -> bool {
    let Some(expected) = normalize(expected) else {
        return false;
    };
    let found = fingerprints_in_sdp(sdp);
    // Every a=fingerprint line must parse (a line we cannot read is a failure).
    let raw_lines = sdp
        .lines()
        .filter(|l| l.trim().starts_with("a=fingerprint:"))
        .count();
    !found.is_empty() && found.len() == raw_lines && found.iter().all(|f| *f == expected)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(first: u8) -> String {
        let mut digest = [0xABu8; 32];
        digest[0] = first;
        format_sha256(&digest)
    }

    #[test]
    fn formats_exactly_as_the_contract_says() {
        let fp = sample(0x01);
        assert!(fp.starts_with("sha-256 01:AB:AB:"));
        assert_eq!(fp.len(), "sha-256 ".len() + 32 * 2 + 31);
        assert!(is_valid_format(&fp));
    }

    #[test]
    fn rejects_every_wrong_shape() {
        let good = sample(0x01);
        assert!(is_valid_format(&good));
        for bad in [
            String::new(),
            "sha-256".into(),
            good.replace("sha-256", "SHA-256"),
            good.replace("sha-256", "sha-1"),
            good.to_lowercase(),
            good.replace(':', ""),
            good.replace(' ', ""),
            format!("{good}:00"),
            good[..good.len() - 3].to_owned(),
            good.replace("01:", "G1:"),
            format!(" {good}"),
            format!("{good} "),
        ] {
            assert!(!is_valid_format(&bad), "{bad:?}");
        }
    }

    #[test]
    fn normalize_accepts_sdp_style_lines() {
        let good = sample(0x0F);
        assert_eq!(normalize(&good), Some(good.clone()));
        assert_eq!(
            normalize(&good.to_lowercase().replace("sha-256", "SHA-256")),
            Some(good)
        );
        assert_eq!(normalize("sha-1 AB:CD"), None);
        assert_eq!(normalize("nonsense"), None);
    }

    #[test]
    fn finds_and_checks_fingerprints_in_an_sdp() {
        let fp = sample(0x42);
        let sdp =
            format!("v=0\r\no=- 1 2 IN IP4 0.0.0.0\r\na=fingerprint:{fp}\r\na=setup:actpass\r\n");
        assert_eq!(fingerprints_in_sdp(&sdp), vec![fp.clone()]);
        assert!(sdp_matches(&sdp, &fp));
        // Lower-case in the SDP or in the expectation still matches.
        assert!(sdp_matches(&sdp.to_lowercase(), &fp));
        assert!(sdp_matches(&sdp, &fp.to_lowercase()));
    }

    #[test]
    fn a_mismatch_or_a_missing_fingerprint_is_refused() {
        let fp = sample(0x42);
        let other = sample(0x43);
        let sdp = format!("v=0\r\na=fingerprint:{other}\r\n");
        assert!(!sdp_matches(&sdp, &fp));
        assert!(!sdp_matches("v=0\r\n", &fp));
        assert!(!sdp_matches(
            &format!("a=fingerprint:{fp}\r\n"),
            "not a fingerprint"
        ));
    }

    #[test]
    fn a_second_fingerprint_cannot_be_smuggled_in() {
        let fp = sample(0x42);
        let other = sample(0x43);
        let sdp = format!("a=fingerprint:{fp}\r\na=fingerprint:{other}\r\n");
        assert!(!sdp_matches(&sdp, &fp));
        // Two identical lines (several media sections) are fine.
        let twice = format!("a=fingerprint:{fp}\r\na=fingerprint:{fp}\r\n");
        assert!(sdp_matches(&twice, &fp));
        // An unreadable fingerprint line is a failure, not ignored.
        let junk = format!("a=fingerprint:{fp}\r\na=fingerprint:md5 00:11\r\n");
        assert!(!sdp_matches(&junk, &fp));
    }
}

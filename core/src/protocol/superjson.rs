//! Minimal superjson codec. Only `Date` needs special handling.
//!
//! A value `X` is sent as `{"json": X}`. Responses are
//! `{"json": X, "meta": {"values": {"<path>": ["Date"], ...}}}` where each
//! listed path points at an ISO-8601 string that stands for a `Date`. Decoding
//! replaces those strings with Unix milliseconds (a JSON number), so typed
//! structs read them as `i64` / `Option<i64>`.
//!
//! Paths are dot-separated; a literal `.` or `\` inside a key is escaped with
//! a backslash. Other superjson annotations (`undefined`, `bigint`, `Map`, …)
//! are not used by the host routes and are ignored.

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use super::error::HostError;

/// `{"json": value}` as exact request bytes.
pub fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, HostError> {
    let inner = serde_json::to_value(value).map_err(|e| HostError::Codec(e.to_string()))?;
    let mut wrapper = serde_json::Map::with_capacity(1);
    wrapper.insert("json".into(), inner);
    serde_json::to_vec(&Value::Object(wrapper)).map_err(|e| HostError::Codec(e.to_string()))
}

/// Decode a response body, turning annotated dates into Unix milliseconds.
pub fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, HostError> {
    let value = decode_value(bytes)?;
    serde_json::from_value(value).map_err(|e| HostError::Codec(e.to_string()))
}

/// Like [`decode`] but returns the untyped value.
pub fn decode_value(bytes: &[u8]) -> Result<Value, HostError> {
    let mut root: Value =
        serde_json::from_slice(bytes).map_err(|e| HostError::Codec(e.to_string()))?;
    let Some(obj) = root.as_object_mut() else {
        return Err(HostError::Codec("superjson body is not an object".into()));
    };
    let paths: Vec<String> = obj
        .get("meta")
        .and_then(|m| m.get("values"))
        .and_then(Value::as_object)
        .map(|values| {
            values
                .iter()
                .filter(|(_, ann)| is_date_annotation(ann))
                .map(|(path, _)| path.clone())
                .collect()
        })
        .unwrap_or_default();
    let mut json = obj
        .remove("json")
        .ok_or_else(|| HostError::Codec("superjson body has no \"json\" field".into()))?;
    for path in paths {
        let segments = split_path(&path);
        let slot = walk(&mut json, &segments)
            .ok_or_else(|| HostError::Codec(format!("date path not found: {path}")))?;
        let Value::String(s) = slot else {
            return Err(HostError::Codec(format!("date at {path} is not a string")));
        };
        let ms = parse_iso_ms(s)?;
        *slot = Value::from(ms);
    }
    Ok(json)
}

fn is_date_annotation(ann: &Value) -> bool {
    matches!(ann.as_array().map(Vec::as_slice), Some([Value::String(t)]) if t == "Date")
}

/// Split on unescaped dots; `\.` and `\\` are literal.
fn split_path(path: &str) -> Vec<String> {
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut chars = path.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            }
            '.' => segments.push(std::mem::take(&mut current)),
            other => current.push(other),
        }
    }
    segments.push(current);
    segments
}

fn walk<'a>(mut node: &'a mut Value, segments: &[String]) -> Option<&'a mut Value> {
    for seg in segments {
        node = match node {
            Value::Object(map) => map.get_mut(seg)?,
            Value::Array(items) => items.get_mut(seg.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(node)
}

fn parse_iso_ms(s: &str) -> Result<i64, HostError> {
    let dt = OffsetDateTime::parse(s, &Rfc3339)
        .map_err(|e| HostError::Codec(format!("bad date {s:?}: {e}")))?;
    i64::try_from(dt.unix_timestamp_nanos() / 1_000_000)
        .map_err(|_| HostError::Codec(format!("date out of range: {s}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;
    use serde_json::json;

    #[derive(Debug, Serialize, Deserialize, PartialEq)]
    #[serde(rename_all = "camelCase")]
    struct Sample {
        name: String,
        expires_at: Option<i64>,
    }

    #[test]
    fn encode_wraps_value_in_json_field() {
        let body = encode(&json!({"protocolVersion": 1})).unwrap();
        assert_eq!(body, br#"{"json":{"protocolVersion":1}}"#);
    }

    #[test]
    fn encode_empty_object_for_invite() {
        assert_eq!(encode(&json!({})).unwrap(), br#"{"json":{}}"#);
    }

    #[test]
    fn decode_plain_value_without_meta() {
        let s: Sample = decode(br#"{"json":{"name":"pc","expiresAt":null}}"#).unwrap();
        assert_eq!(
            s,
            Sample {
                name: "pc".into(),
                expires_at: None
            }
        );
    }

    #[test]
    fn decode_turns_date_into_unix_ms() {
        let body = br#"{"json":{"name":"pc","expiresAt":"2026-10-08T12:33:38.897Z"},
            "meta":{"values":{"expiresAt":["Date"]}}}"#;
        let s: Sample = decode(body).unwrap();
        // 2026-10-08T12:33:38.897Z
        assert_eq!(s.expires_at, Some(1_791_462_818_897));
    }

    #[test]
    fn decode_handles_nested_and_array_paths() {
        let body = br#"{"json":{"serverTime":"1970-01-01T00:00:01.000Z",
              "pending":[{"id":"a","at":"1970-01-01T00:00:02.500Z"},{"id":"b","at":null}]},
            "meta":{"values":{"serverTime":["Date"],"pending.0.at":["Date"]}}}"#;
        let v = decode_value(body).unwrap();
        assert_eq!(v["serverTime"], json!(1000));
        assert_eq!(v["pending"][0]["at"], json!(2500));
        assert_eq!(v["pending"][1]["at"], Value::Null);
    }

    #[test]
    fn decode_handles_escaped_dots_in_keys() {
        let body = br#"{"json":{"a.b":{"c":"1970-01-01T00:00:00.007Z"}},
            "meta":{"values":{"a\\.b.c":["Date"]}}}"#;
        let v = decode_value(body).unwrap();
        assert_eq!(v["a.b"]["c"], json!(7));
    }

    #[test]
    fn decode_ignores_other_annotations() {
        let body = br#"{"json":{"x":"keep"},"meta":{"values":{"x":["undefined"]}}}"#;
        assert_eq!(decode_value(body).unwrap()["x"], json!("keep"));
    }

    #[test]
    fn decode_rejects_malformed_bodies() {
        assert!(decode_value(b"not json").is_err());
        assert!(decode_value(b"[]").is_err());
        assert!(decode_value(br#"{"nope":1}"#).is_err());
        let missing = br#"{"json":{},"meta":{"values":{"gone":["Date"]}}}"#;
        assert!(decode_value(missing).is_err());
        let not_string = br#"{"json":{"d":5},"meta":{"values":{"d":["Date"]}}}"#;
        assert!(decode_value(not_string).is_err());
        let bad_date = br#"{"json":{"d":"yesterday"},"meta":{"values":{"d":["Date"]}}}"#;
        assert!(decode_value(bad_date).is_err());
    }
}

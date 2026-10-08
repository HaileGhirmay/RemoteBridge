//! `rb-native-host`: the optional Chrome/Edge native messaging shim.
//!
//! It answers exactly two requests from the extension and nothing else:
//! * `{"type":"status"}` -> `{"installed": bool, "version": "..."}` (version only when installed)
//! * `{"type":"open"}`   -> starts the host app's window and answers `{"ok": true}`
//!
//! Every other message is refused with `{"error":"unsupported"}`. Input is
//! capped at 4 KB, the browser's own limit for messages to a host. The shim
//! does not read or send any screen, input, session or account data.

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};

/// The browser never sends more than this to a native host.
const MAX_REQUEST_BYTES: usize = 4096;
const HOST_EXE: &str = "rb-host.exe";
const VERSION_FILE: &str = "rb-host.version";

/// Where the host app is installed for the current user.
fn install_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("RB_HOST_DIR") {
        return PathBuf::from(dir);
    }
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("RemoteBridge")
}

/// The installed host version, if the host is present.
fn installed_version(dir: &Path) -> Option<String> {
    if !dir.join(HOST_EXE).is_file() {
        return None;
    }
    let text = std::fs::read_to_string(dir.join(VERSION_FILE)).ok()?;
    let version = text.trim();
    // Keep only plain version text (digits, dots, dashes, letters).
    let ok = !version.is_empty()
        && version.len() <= 32
        && version
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-');
    ok.then(|| version.to_owned())
}

/// Answer one request. `dir` is the install directory.
fn handle(request: &Value, dir: &Path) -> Value {
    match request.get("type").and_then(Value::as_str) {
        Some("status") => match installed_version(dir) {
            Some(version) => json!({"installed": true, "version": version}),
            None => json!({"installed": false}),
        },
        Some("open") => match open_host(dir) {
            Ok(()) => json!({"ok": true}),
            Err(_) => json!({"error": "unavailable"}),
        },
        _ => json!({"error": "unsupported"}),
    }
}

fn open_host(dir: &Path) -> io::Result<()> {
    if installed_version(dir).is_none() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "host not installed",
        ));
    }
    Command::new(dir.join(HOST_EXE))
        .arg("--open-window")
        .spawn()?;
    Ok(())
}

/// Read one length-prefixed message (native-endian u32, as Chrome specifies).
fn read_message(input: &mut impl Read) -> io::Result<Option<Vec<u8>>> {
    let mut len_bytes = [0u8; 4];
    match input.read_exact(&mut len_bytes) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u32::from_ne_bytes(len_bytes) as usize;
    if len > MAX_REQUEST_BYTES {
        // Discard the body so the stream stays in step, then refuse.
        io::copy(&mut input.take(len as u64), &mut io::sink())?;
        return Ok(Some(Vec::new()));
    }
    let mut body = vec![0u8; len];
    input.read_exact(&mut body)?;
    Ok(Some(body))
}

fn write_message(output: &mut impl Write, value: &Value) -> io::Result<()> {
    let body = serde_json::to_vec(value)?;
    let len = u32::try_from(body.len()).unwrap_or(u32::MAX);
    output.write_all(&len.to_ne_bytes())?;
    output.write_all(&body)?;
    output.flush()
}

fn main() -> io::Result<()> {
    let dir = install_dir();
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut input = stdin.lock();
    let mut output = stdout.lock();
    while let Some(body) = read_message(&mut input)? {
        let reply = match serde_json::from_slice::<Value>(&body) {
            Ok(request) if !body.is_empty() => handle(&request, &dir),
            _ => json!({"error": "unsupported"}),
        };
        write_message(&mut output, &reply)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn frame(value: &Value) -> Vec<u8> {
        let body = serde_json::to_vec(value).unwrap();
        let mut out = (body.len() as u32).to_ne_bytes().to_vec();
        out.extend(body);
        out
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rb-native-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn status_reports_not_installed_when_the_host_is_missing() {
        let dir = temp_dir("missing");
        assert_eq!(
            handle(&json!({"type": "status"}), &dir),
            json!({"installed": false})
        );
    }

    #[test]
    fn status_reports_the_installed_version() {
        let dir = temp_dir("installed");
        std::fs::write(dir.join(HOST_EXE), b"x").unwrap();
        std::fs::write(dir.join(VERSION_FILE), "0.1.0\n").unwrap();
        assert_eq!(
            handle(&json!({"type": "status"}), &dir),
            json!({"installed": true, "version": "0.1.0"})
        );
    }

    #[test]
    fn a_suspicious_version_file_is_not_echoed() {
        let dir = temp_dir("badversion");
        std::fs::write(dir.join(HOST_EXE), b"x").unwrap();
        std::fs::write(dir.join(VERSION_FILE), "<script>alert(1)</script>").unwrap();
        // An installed host without a readable version is reported as not
        // installed, which is the only shape the extension accepts without a version.
        assert_eq!(
            handle(&json!({"type": "status"}), &dir),
            json!({"installed": false})
        );
    }

    #[test]
    fn open_without_the_host_fails_cleanly() {
        let dir = temp_dir("open-missing");
        assert_eq!(
            handle(&json!({"type": "open"}), &dir),
            json!({"error": "unavailable"})
        );
    }

    #[test]
    fn everything_else_is_refused() {
        let dir = temp_dir("refuse");
        for request in [
            json!({"type": "shell", "cmd": "calc"}),
            json!({"type": "read_screen"}),
            json!({}),
            json!({"type": 5}),
        ] {
            assert_eq!(
                handle(&request, &dir),
                json!({"error": "unsupported"}),
                "{request}"
            );
        }
    }

    #[test]
    fn a_full_exchange_over_the_wire() {
        let dir = temp_dir("wire");
        let mut input = Vec::new();
        input.extend(frame(&json!({"type": "status"})));
        input.extend(frame(&json!({"type": "shell"})));
        let mut cursor = Cursor::new(input);
        let mut out = Vec::new();
        while let Some(body) = read_message(&mut cursor).unwrap() {
            let reply = match serde_json::from_slice::<Value>(&body) {
                Ok(request) if !body.is_empty() => handle(&request, &dir),
                _ => json!({"error": "unsupported"}),
            };
            write_message(&mut out, &reply).unwrap();
        }
        let mut replies = Cursor::new(out);
        let first = read_message(&mut replies).unwrap().unwrap();
        let second = read_message(&mut replies).unwrap().unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&first).unwrap(),
            json!({"installed": false})
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&second).unwrap(),
            json!({"error": "unsupported"})
        );
    }

    #[test]
    fn oversized_input_is_discarded_and_the_stream_stays_in_step() {
        let mut input = Vec::new();
        let big = vec![b'x'; MAX_REQUEST_BYTES + 10];
        input.extend((big.len() as u32).to_ne_bytes());
        input.extend(&big);
        input.extend(frame(&json!({"type": "status"})));
        let mut cursor = Cursor::new(input);
        assert_eq!(
            read_message(&mut cursor).unwrap(),
            Some(Vec::new()),
            "oversize is empty"
        );
        let next = read_message(&mut cursor).unwrap().unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&next).unwrap(),
            json!({"type": "status"})
        );
    }
}

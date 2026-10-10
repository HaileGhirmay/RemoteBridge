//! The one piece of state the host keeps outside the key store: which device
//! id the server gave this PC at enrollment.
//!
//! The key itself lives in the hardware-backed key store, never in this file.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// `device-id.txt` next to the other RemoteBridge data.
pub fn default_path() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("RemoteBridge").join("device-id.txt")
}

/// Load the enrolled device id. `Ok(None)` when the file is absent.
pub fn load(path: &Path) -> io::Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(text) => {
            let id = text.trim();
            if is_device_id(id) {
                Ok(Some(id.to_owned()))
            } else {
                Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("{} does not hold a device id", path.display()),
                ))
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Save the device id, creating the folder if needed.
pub fn save(path: &Path, device_id: &str) -> io::Result<()> {
    if !is_device_id(device_id) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "device id must be a UUID",
        ));
    }
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(path, format!("{device_id}\n"))
}

/// `unattended.txt` next to the device id: `on` when the person at this PC
/// allowed unattended access. Anything else, or no file, means off.
pub fn unattended_path() -> PathBuf {
    default_path().with_file_name("unattended.txt")
}

pub fn load_unattended(path: &Path) -> io::Result<bool> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(text.trim() == "on"),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

pub fn save_unattended(path: &Path, on: bool) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(path, if on { "on\n" } else { "off\n" })
}

/// `logs\rb-host.log` next to the other data, for when there is no console
/// (started at login). Metadata only, like every log in this product.
pub fn log_path() -> PathBuf {
    default_path().with_file_name("logs").join("rb-host.log")
}

pub fn open_log_file() -> io::Result<fs::File> {
    let path = log_path();
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::OpenOptions::new().create(true).append(true).open(path)
}

/// Canonical UUID shape: 8-4-4-4-12 hex digits.
pub fn is_device_id(text: &str) -> bool {
    let groups: Vec<&str> = text.split('-').collect();
    let lengths = [8, 4, 4, 4, 12];
    groups.len() == lengths.len()
        && groups
            .iter()
            .zip(lengths)
            .all(|(g, n)| g.len() == n && g.bytes().all(|b| b.is_ascii_hexdigit()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "b272579b-39b9-49e6-8cfe-f002a0944821";

    fn temp_file(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rb-host-state-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        let _ = fs::remove_file(&path);
        path
    }

    #[test]
    fn a_missing_file_means_not_enrolled() {
        let path = temp_file("missing.txt");
        assert_eq!(load(&path).unwrap(), None);
    }

    #[test]
    fn the_device_id_round_trips() {
        let path = temp_file("round.txt");
        save(&path, ID).unwrap();
        assert_eq!(load(&path).unwrap().as_deref(), Some(ID));
    }

    #[test]
    fn a_damaged_file_is_an_error_not_a_silent_reenrollment() {
        let path = temp_file("damaged.txt");
        fs::write(&path, "not-a-device-id\n").unwrap();
        assert_eq!(load(&path).unwrap_err().kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn only_uuid_shaped_ids_are_saved() {
        let path = temp_file("refused.txt");
        assert!(save(&path, "../../etc/passwd").is_err());
        assert!(!path.exists());
    }

    #[test]
    fn unattended_is_off_without_a_file_and_round_trips() {
        let path = temp_file("unattended.txt");
        assert!(!load_unattended(&path).unwrap());
        save_unattended(&path, true).unwrap();
        assert!(load_unattended(&path).unwrap());
        save_unattended(&path, false).unwrap();
        assert!(!load_unattended(&path).unwrap());
    }

    #[test]
    fn only_an_exact_on_switches_unattended_on() {
        let path = temp_file("unattended-odd.txt");
        fs::write(&path, "yes please\n").unwrap();
        assert!(!load_unattended(&path).unwrap());
    }

    #[test]
    fn the_log_file_sits_in_a_logs_folder_beside_the_data() {
        let path = log_path();
        assert!(path.ends_with(Path::new("logs").join("rb-host.log")));
        assert_eq!(
            path.parent().and_then(Path::parent),
            default_path().parent()
        );
    }

    #[test]
    fn uuid_shape_is_checked_exactly() {
        assert!(is_device_id(ID));
        assert!(!is_device_id("b272579b39b949e68cfef002a0944821"));
        assert!(!is_device_id("b272579b-39b9-49e6-8cfe-f002a094482"));
        assert!(!is_device_id("g272579b-39b9-49e6-8cfe-f002a0944821"));
    }
}

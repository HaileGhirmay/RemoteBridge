//! Start at login: one value under the per-user Run key.
//!
//! `HKCU\Software\Microsoft\Windows\CurrentVersion\Run\RemoteBridge` holds
//! `"<exe>" run`. Per user, no administrator rights, and visible in Task
//! Manager's Startup tab where the person can switch it off too.

use std::path::Path;

/// The value name under the Run key.
pub const VALUE_NAME: &str = "RemoteBridge";

pub const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

/// `"C:\path\rb-host.exe" run`, quoted so spaces in the path survive.
pub fn command_line(exe: &Path) -> String {
    format!("\"{}\" run", exe.display())
}

#[cfg(windows)]
pub use win::{disable, enable, status};

#[cfg(windows)]
mod win {
    use std::io;

    use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, WIN32_ERROR};
    use windows::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ,
        RRF_RT_REG_SZ, RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegGetValueW, RegSetValueExW,
    };
    use windows::core::{HSTRING, PCWSTR};

    use super::RUN_KEY;

    fn failed(what: &str, e: WIN32_ERROR) -> io::Error {
        io::Error::other(format!("{what}: Win32 error {}", e.0))
    }

    /// An open Run key, closed on drop.
    struct Key(HKEY);

    impl Drop for Key {
        fn drop(&mut self) {
            // SAFETY: a key this module opened.
            let _ = unsafe { RegCloseKey(self.0) };
        }
    }

    fn open_run_key() -> io::Result<Key> {
        let mut key = HKEY::default();
        // SAFETY: creating (or opening) a key under the current user's hive.
        let result = unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                &HSTRING::from(RUN_KEY),
                None,
                PCWSTR::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_SET_VALUE | KEY_QUERY_VALUE,
                None,
                &mut key,
                None,
            )
        };
        if result != ERROR_SUCCESS {
            return Err(failed("open Run key", result));
        }
        Ok(Key(key))
    }

    /// Write the command under `value_name`.
    pub fn enable(value_name: &str, command: &str) -> io::Result<()> {
        let key = open_run_key()?;
        let wide: Vec<u16> = command.encode_utf16().chain(std::iter::once(0)).collect();
        let bytes: Vec<u8> = wide.iter().flat_map(|u| u.to_le_bytes()).collect();
        // SAFETY: a REG_SZ written from a NUL-terminated UTF-16 buffer.
        let result = unsafe {
            RegSetValueExW(
                key.0,
                &HSTRING::from(value_name),
                None,
                REG_SZ,
                Some(&bytes),
            )
        };
        if result != ERROR_SUCCESS {
            return Err(failed("set Run value", result));
        }
        Ok(())
    }

    /// Remove the value. Already absent is fine.
    pub fn disable(value_name: &str) -> io::Result<()> {
        let key = open_run_key()?;
        // SAFETY: deleting one value under a key this module opened.
        let result = unsafe { RegDeleteValueW(key.0, &HSTRING::from(value_name)) };
        if result == ERROR_SUCCESS || result == ERROR_FILE_NOT_FOUND {
            Ok(())
        } else {
            Err(failed("delete Run value", result))
        }
    }

    /// The command stored under `value_name`, if any.
    pub fn status(value_name: &str) -> io::Result<Option<String>> {
        let sub_key = HSTRING::from(RUN_KEY);
        let name = HSTRING::from(value_name);
        let mut len: u32 = 0;
        // SAFETY: a size query, then a read into a buffer of that size.
        unsafe {
            let result = RegGetValueW(
                HKEY_CURRENT_USER,
                &sub_key,
                &name,
                RRF_RT_REG_SZ,
                None,
                None,
                Some(&mut len),
            );
            if result == ERROR_FILE_NOT_FOUND {
                return Ok(None);
            }
            if result != ERROR_SUCCESS {
                return Err(failed("read Run value", result));
            }
            let mut buf = vec![0u16; len as usize / 2 + 1];
            let mut cb = (buf.len() * 2) as u32;
            let result = RegGetValueW(
                HKEY_CURRENT_USER,
                &sub_key,
                &name,
                RRF_RT_REG_SZ,
                None,
                Some(buf.as_mut_ptr() as *mut _),
                Some(&mut cb),
            );
            if result != ERROR_SUCCESS {
                return Err(failed("read Run value", result));
            }
            let text = String::from_utf16_lossy(&buf);
            Ok(Some(text.trim_end_matches('\0').to_owned()))
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// Uses its own value name, so it never touches the real setting. Runs
        /// on every Windows machine that runs the tests, including CI.
        #[test]
        fn run_key_value_round_trips_and_is_removed() {
            let name = "RemoteBridgeTestOnly";
            disable(name).unwrap();
            assert_eq!(status(name).unwrap(), None);
            let command = "\"C:\\Program Files\\RemoteBridge\\rb-host.exe\" run";
            enable(name, command).unwrap();
            assert_eq!(status(name).unwrap().as_deref(), Some(command));
            disable(name).unwrap();
            assert_eq!(status(name).unwrap(), None);
            // Removing it again is not an error.
            disable(name).unwrap();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_command_line_quotes_the_exe_and_runs_the_host() {
        let line = command_line(Path::new(r"C:\Program Files\RemoteBridge\rb-host.exe"));
        assert_eq!(line, r#""C:\Program Files\RemoteBridge\rb-host.exe" run"#);
    }
}

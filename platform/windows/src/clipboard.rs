//! Text clipboard for Windows (`CF_UNICODETEXT`).
//!
//! Text only. Images, files and other formats are never read or written. The
//! permission filter decides when this is called; this module only moves text.

/// Clipboard format for NUL-terminated UTF-16 text.
pub const CF_UNICODETEXT: u32 = 13;

/// UTF-16 with a trailing NUL, the layout `CF_UNICODETEXT` expects.
pub fn to_wide_nul(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Text from UTF-16, read up to the first NUL. Unpaired surrogates become
/// U+FFFD rather than failing, because the clipboard may hold anything.
pub fn from_wide(units: &[u16]) -> String {
    let end = units.iter().position(|&u| u == 0).unwrap_or(units.len());
    String::from_utf16_lossy(&units[..end])
}

#[cfg(windows)]
pub use win::WindowsClipboard;

#[cfg(windows)]
mod win {
    use rb_core::traits::Clipboard;
    use rb_core::{PlatformError, PlatformResult};
    use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL};
    use windows::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable,
        OpenClipboard, SetClipboardData,
    };
    use windows::Win32::System::Memory::{
        GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock,
    };

    use super::{CF_UNICODETEXT, from_wide, to_wide_nul};

    fn backend(what: &str, e: windows::core::Error) -> PlatformError {
        PlatformError::Backend(format!("{what}: 0x{:08x}", e.code().0 as u32))
    }

    /// Keeps the clipboard open for one operation and closes it on drop.
    struct Open;

    impl Open {
        fn new() -> PlatformResult<Self> {
            // SAFETY: no pointers are passed; the clipboard is closed by `Drop`.
            unsafe { OpenClipboard(None) }.map_err(|e| backend("OpenClipboard", e))?;
            Ok(Self)
        }
    }

    impl Drop for Open {
        fn drop(&mut self) {
            // SAFETY: this thread opened the clipboard in `Open::new`.
            let _ = unsafe { CloseClipboard() };
        }
    }

    /// The clipboard, one operation at a time. Stateless: every call opens and
    /// closes the clipboard, so other programs are never locked out for long.
    #[derive(Default)]
    pub struct WindowsClipboard;

    impl WindowsClipboard {
        pub fn new() -> Self {
            Self
        }
    }

    impl Clipboard for WindowsClipboard {
        fn get_text(&mut self) -> PlatformResult<Option<String>> {
            // SAFETY: a format query takes no pointers.
            if unsafe { IsClipboardFormatAvailable(CF_UNICODETEXT) }.is_err() {
                return Ok(None);
            }
            let _open = Open::new()?;
            // SAFETY: the clipboard is open on this thread. The handle belongs to
            // the clipboard and is only locked, never freed, here.
            unsafe {
                let handle =
                    GetClipboardData(CF_UNICODETEXT).map_err(|e| backend("GetClipboardData", e))?;
                let memory = HGLOBAL(handle.0);
                let ptr = GlobalLock(memory) as *const u16;
                if ptr.is_null() {
                    return Err(PlatformError::Backend("GlobalLock failed".into()));
                }
                let units = GlobalSize(memory) / 2;
                let text = from_wide(std::slice::from_raw_parts(ptr, units));
                let _ = GlobalUnlock(memory);
                Ok(Some(text))
            }
        }

        fn set_text(&mut self, text: &str) -> PlatformResult<()> {
            let wide = to_wide_nul(text);
            let _open = Open::new()?;
            // SAFETY: the clipboard is open on this thread. On success the system
            // owns `memory`, so it is freed only if the hand-over fails.
            unsafe {
                EmptyClipboard().map_err(|e| backend("EmptyClipboard", e))?;
                let memory = GlobalAlloc(GMEM_MOVEABLE, wide.len() * 2)
                    .map_err(|e| backend("GlobalAlloc", e))?;
                let dst = GlobalLock(memory) as *mut u16;
                if dst.is_null() {
                    let _ = GlobalFree(Some(memory));
                    return Err(PlatformError::Backend("GlobalLock failed".into()));
                }
                std::ptr::copy_nonoverlapping(wide.as_ptr(), dst, wide.len());
                let _ = GlobalUnlock(memory);
                if let Err(e) = SetClipboardData(CF_UNICODETEXT, Some(HANDLE(memory.0))) {
                    let _ = GlobalFree(Some(memory));
                    return Err(backend("SetClipboardData", e));
                }
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_text_is_nul_terminated() {
        assert_eq!(to_wide_nul("hi"), vec![b'h' as u16, b'i' as u16, 0]);
        assert_eq!(to_wide_nul(""), vec![0]);
    }

    #[test]
    fn round_trips_unicode_including_emoji() {
        let text = "Grüße, 世界 🙂";
        assert_eq!(from_wide(&to_wide_nul(text)), text);
    }

    #[test]
    fn reading_stops_at_the_first_nul() {
        assert_eq!(from_wide(&[b'a' as u16, 0, b'b' as u16]), "a");
        assert_eq!(
            from_wide(&[b'a' as u16, b'b' as u16]),
            "ab",
            "no NUL at all"
        );
    }

    #[test]
    fn unpaired_surrogates_do_not_fail_the_read() {
        assert_eq!(from_wide(&[0xD800, b'x' as u16, 0]), "\u{FFFD}x");
    }
}

#[cfg(all(test, windows))]
mod hardware {
    use super::*;

    /// Writes text to the real clipboard and reads it back, then puts back what
    /// was there before. Run by hand, since it touches the clipboard:
    /// `cargo test -p rb-platform-windows clipboard_real -- --ignored`
    #[test]
    #[ignore = "uses the real clipboard"]
    fn clipboard_real_round_trip() {
        use rb_core::traits::Clipboard;
        let mut clipboard = win::WindowsClipboard::new();
        let before = clipboard.get_text().unwrap();
        let sample = "RemoteBridge clipboard test ✓";
        clipboard.set_text(sample).unwrap();
        assert_eq!(clipboard.get_text().unwrap().as_deref(), Some(sample));
        if let Some(text) = before {
            clipboard.set_text(&text).unwrap();
        }
    }
}

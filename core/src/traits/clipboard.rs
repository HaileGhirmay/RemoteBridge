use crate::PlatformResult;

/// The local clipboard, text only.
///
/// Used only while the clipboard permission is granted. Clipboard text is
/// never logged. Files, images and other formats are never touched.
pub trait Clipboard: Send {
    /// Current text, or `None` if the clipboard holds something else (or nothing).
    fn get_text(&mut self) -> PlatformResult<Option<String>>;
    fn set_text(&mut self, text: &str) -> PlatformResult<()>;
}

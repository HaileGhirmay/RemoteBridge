//! DOM `KeyboardEvent.code` to Windows hardware scan codes.
//!
//! Scan codes are the set-1 codes the keyboard itself produces, so a key
//! behaves the same way in every layout. Keys that share a scan code with
//! another key (the arrows and the numeric keypad) are told apart by the
//! extended flag.
//!
//! This table is pure data and is compiled on every platform so it can be
//! tested anywhere.

/// A physical key on the Windows keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanCode {
    pub code: u16,
    /// Sent with `KEYEVENTF_EXTENDEDKEY` (the 0xE0 prefix on the keyboard).
    pub extended: bool,
}

const fn plain(code: u16) -> Option<ScanCode> {
    Some(ScanCode {
        code,
        extended: false,
    })
}

const fn ext(code: u16) -> Option<ScanCode> {
    Some(ScanCode {
        code,
        extended: true,
    })
}

/// The scan code for a DOM `code`, or `None` if we do not know the key.
/// Unknown keys are dropped, never guessed at.
pub fn scan_code(dom_code: &str) -> Option<ScanCode> {
    match dom_code {
        // Letters
        "KeyA" => plain(0x1E),
        "KeyB" => plain(0x30),
        "KeyC" => plain(0x2E),
        "KeyD" => plain(0x20),
        "KeyE" => plain(0x12),
        "KeyF" => plain(0x21),
        "KeyG" => plain(0x22),
        "KeyH" => plain(0x23),
        "KeyI" => plain(0x17),
        "KeyJ" => plain(0x24),
        "KeyK" => plain(0x25),
        "KeyL" => plain(0x26),
        "KeyM" => plain(0x32),
        "KeyN" => plain(0x31),
        "KeyO" => plain(0x18),
        "KeyP" => plain(0x19),
        "KeyQ" => plain(0x10),
        "KeyR" => plain(0x13),
        "KeyS" => plain(0x1F),
        "KeyT" => plain(0x14),
        "KeyU" => plain(0x16),
        "KeyV" => plain(0x2F),
        "KeyW" => plain(0x11),
        "KeyX" => plain(0x2D),
        "KeyY" => plain(0x15),
        "KeyZ" => plain(0x2C),
        // Digits along the top row
        "Digit1" => plain(0x02),
        "Digit2" => plain(0x03),
        "Digit3" => plain(0x04),
        "Digit4" => plain(0x05),
        "Digit5" => plain(0x06),
        "Digit6" => plain(0x07),
        "Digit7" => plain(0x08),
        "Digit8" => plain(0x09),
        "Digit9" => plain(0x0A),
        "Digit0" => plain(0x0B),
        // Punctuation
        "Minus" => plain(0x0C),
        "Equal" => plain(0x0D),
        "BracketLeft" => plain(0x1A),
        "BracketRight" => plain(0x1B),
        "Backslash" => plain(0x2B),
        "Semicolon" => plain(0x27),
        "Quote" => plain(0x28),
        "Backquote" => plain(0x29),
        "Comma" => plain(0x33),
        "Period" => plain(0x34),
        "Slash" => plain(0x35),
        // Whitespace and editing
        "Space" => plain(0x39),
        "Enter" => plain(0x1C),
        "Escape" => plain(0x01),
        "Backspace" => plain(0x0E),
        "Tab" => plain(0x0F),
        "CapsLock" => plain(0x3A),
        // Modifiers
        "ShiftLeft" => plain(0x2A),
        "ShiftRight" => plain(0x36),
        "ControlLeft" => plain(0x1D),
        "ControlRight" => ext(0x1D),
        "AltLeft" => plain(0x38),
        "AltRight" => ext(0x38),
        "MetaLeft" => ext(0x5B),
        "MetaRight" => ext(0x5C),
        "ContextMenu" => ext(0x5D),
        // Function keys
        "F1" => plain(0x3B),
        "F2" => plain(0x3C),
        "F3" => plain(0x3D),
        "F4" => plain(0x3E),
        "F5" => plain(0x3F),
        "F6" => plain(0x40),
        "F7" => plain(0x41),
        "F8" => plain(0x42),
        "F9" => plain(0x43),
        "F10" => plain(0x44),
        "F11" => plain(0x57),
        "F12" => plain(0x58),
        // Navigation (extended)
        "ArrowUp" => ext(0x48),
        "ArrowDown" => ext(0x50),
        "ArrowLeft" => ext(0x4B),
        "ArrowRight" => ext(0x4D),
        "Home" => ext(0x47),
        "End" => ext(0x4F),
        "PageUp" => ext(0x49),
        "PageDown" => ext(0x51),
        "Insert" => ext(0x52),
        "Delete" => ext(0x53),
        // Locks
        "NumLock" => plain(0x45),
        "ScrollLock" => plain(0x46),
        "PrintScreen" => ext(0x37),
        "Pause" => plain(0x45),
        // Numeric keypad (without NumLock these are the navigation keys above)
        "Numpad0" => plain(0x52),
        "Numpad1" => plain(0x4F),
        "Numpad2" => plain(0x50),
        "Numpad3" => plain(0x51),
        "Numpad4" => plain(0x4B),
        "Numpad5" => plain(0x4C),
        "Numpad6" => plain(0x4D),
        "Numpad7" => plain(0x47),
        "Numpad8" => plain(0x48),
        "Numpad9" => plain(0x49),
        "NumpadDecimal" => plain(0x53),
        "NumpadAdd" => plain(0x4E),
        "NumpadSubtract" => plain(0x4A),
        "NumpadMultiply" => plain(0x37),
        "NumpadDivide" => ext(0x35),
        "NumpadEnter" => ext(0x1C),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letters_and_digits_have_the_set_one_codes() {
        assert_eq!(scan_code("KeyA"), plain(0x1E));
        assert_eq!(scan_code("KeyZ"), plain(0x2C));
        assert_eq!(scan_code("Digit1"), plain(0x02));
        assert_eq!(scan_code("Digit0"), plain(0x0B));
    }

    #[test]
    fn shared_codes_are_told_apart_by_the_extended_flag() {
        // Arrow keys and the keypad share scan codes; only the flag differs.
        assert_eq!(scan_code("ArrowUp"), ext(0x48));
        assert_eq!(scan_code("Numpad8"), plain(0x48));
        assert_eq!(scan_code("NumpadEnter"), ext(0x1C));
        assert_eq!(scan_code("Enter"), plain(0x1C));
        assert_eq!(scan_code("AltRight"), ext(0x38));
        assert_eq!(scan_code("AltLeft"), plain(0x38));
    }

    #[test]
    fn every_key_the_viewer_sends_for_common_typing_is_known() {
        for code in [
            "Space",
            "Backspace",
            "Tab",
            "ShiftLeft",
            "ControlLeft",
            "F12",
            "Delete",
            "End",
        ] {
            assert!(scan_code(code).is_some(), "{code}");
        }
    }

    #[test]
    fn unknown_keys_are_refused_not_guessed() {
        assert_eq!(scan_code("Unidentified"), None);
        assert_eq!(scan_code(""), None);
        assert_eq!(scan_code("keya"), None, "codes are case sensitive");
        assert_eq!(scan_code("BrowserBack"), None);
    }
}

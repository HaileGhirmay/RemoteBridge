/// DOM `KeyboardEvent.code` to macOS virtual key codes (`kVK_*` from HIToolbox).
///
/// Virtual key codes follow the physical key position, as the DOM codes do,
/// so the same key is pressed whatever the keyboard layout. Unknown codes
/// return nil and are dropped, never guessed at.
public enum MacKeyMap {
    public static func virtualKey(for domCode: String) -> UInt16? {
        table[domCode]
    }

    private static let table: [String: UInt16] = [
        // Letters
        "KeyA": 0x00, "KeyS": 0x01, "KeyD": 0x02, "KeyF": 0x03, "KeyH": 0x04,
        "KeyG": 0x05, "KeyZ": 0x06, "KeyX": 0x07, "KeyC": 0x08, "KeyV": 0x09,
        "KeyB": 0x0B, "KeyQ": 0x0C, "KeyW": 0x0D, "KeyE": 0x0E, "KeyR": 0x0F,
        "KeyY": 0x10, "KeyT": 0x11, "KeyO": 0x1F, "KeyU": 0x20, "KeyI": 0x22,
        "KeyP": 0x23, "KeyL": 0x25, "KeyJ": 0x26, "KeyK": 0x28, "KeyN": 0x2D,
        "KeyM": 0x2E,
        // Digits along the top row
        "Digit1": 0x12, "Digit2": 0x13, "Digit3": 0x14, "Digit4": 0x15,
        "Digit5": 0x17, "Digit6": 0x16, "Digit7": 0x1A, "Digit8": 0x1C,
        "Digit9": 0x19, "Digit0": 0x1D,
        // Punctuation
        "Equal": 0x18, "Minus": 0x1B, "BracketRight": 0x1E, "BracketLeft": 0x21,
        "Quote": 0x27, "Semicolon": 0x29, "Backslash": 0x2A, "Comma": 0x2B,
        "Slash": 0x2C, "Period": 0x2F, "Backquote": 0x32,
        // Whitespace and editing
        "Enter": 0x24, "Tab": 0x30, "Space": 0x31, "Backspace": 0x33,
        "Escape": 0x35, "CapsLock": 0x39,
        // Modifiers (left and right are distinct keys)
        "MetaLeft": 0x37, "MetaRight": 0x36, "ShiftLeft": 0x38, "ShiftRight": 0x3C,
        "AltLeft": 0x3A, "AltRight": 0x3D, "ControlLeft": 0x3B, "ControlRight": 0x3E,
        // Function keys
        "F1": 0x7A, "F2": 0x78, "F3": 0x63, "F4": 0x76, "F5": 0x60, "F6": 0x61,
        "F7": 0x62, "F8": 0x64, "F9": 0x65, "F10": 0x6D, "F11": 0x67, "F12": 0x6F,
        // Navigation
        "ArrowLeft": 0x7B, "ArrowRight": 0x7C, "ArrowDown": 0x7D, "ArrowUp": 0x7E,
        "Home": 0x73, "End": 0x77, "PageUp": 0x74, "PageDown": 0x79,
        "Delete": 0x75, "Help": 0x72,
        // Keypad
        "Numpad0": 0x52, "Numpad1": 0x53, "Numpad2": 0x54, "Numpad3": 0x55,
        "Numpad4": 0x56, "Numpad5": 0x57, "Numpad6": 0x58, "Numpad7": 0x59,
        "Numpad8": 0x5B, "Numpad9": 0x5C, "NumpadDecimal": 0x41,
        "NumpadAdd": 0x45, "NumpadSubtract": 0x4E, "NumpadMultiply": 0x43,
        "NumpadDivide": 0x4B, "NumpadEnter": 0x4C, "NumpadEqual": 0x51,
        "NumpadClear": 0x47,
    ]
}

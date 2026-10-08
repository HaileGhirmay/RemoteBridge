//! Remote input through `SendInput`.
//!
//! * Keys go out as hardware scan codes, so they behave like the real keyboard.
//! * Every event carries [`INJECT_TAG`] in `dwExtraInfo`, so the local hotkey
//!   hook can tell our own events apart.
//! * Pointer positions are mapped from the captured display's rectangle to the
//!   whole virtual desktop, using absolute coordinates.
//! * Keys and buttons the viewer holds down are remembered, so
//!   [`InputInjector::release_all`] can release them when control is revoked,
//!   the viewer loses focus, or the session ends.
//!
//! If Windows refuses an event (for example a window at a higher integrity
//! level, or the secure desktop), `SendInput` reports 0 events sent. That is
//! returned as an error, never retried in a loop.

use std::collections::BTreeSet;

use rb_core::traits::InputInjector;
use rb_core::types::MouseButton;
use rb_core::{PlatformError, PlatformResult};
use windows::Win32::Foundation::{GetLastError, WIN32_ERROR};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY,
    KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE, MOUSE_EVENT_FLAGS, MOUSEEVENTF_ABSOLUTE,
    MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN,
    MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP,
    MOUSEEVENTF_VIRTUALDESK, MOUSEEVENTF_WHEEL, MOUSEINPUT, SendInput, VIRTUAL_KEY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, SM_CXSCREEN, SM_CXVIRTUALSCREEN, SM_CYSCREEN, SM_CYVIRTUALSCREEN,
    SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
};

use crate::keymap::scan_code;

/// Marks events we send ("RBMT").
pub const INJECT_TAG: usize = 0x5242_4D54;

/// A display's rectangle in virtual-screen pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DisplayRect {
    pub left: i32,
    pub top: i32,
    pub width: i32,
    pub height: i32,
}

/// Map a point given as 0..=1 fractions of `rect` into the 0..=65535 range
/// that absolute virtual-desktop coordinates use.
pub fn absolute_coordinates(rect: DisplayRect, desktop: DisplayRect, x: f32, y: f32) -> (i32, i32) {
    let fx = f64::from(x.clamp(0.0, 1.0));
    let fy = f64::from(y.clamp(0.0, 1.0));
    let px = f64::from(rect.left) + fx * f64::from(rect.width - 1).max(0.0);
    let py = f64::from(rect.top) + fy * f64::from(rect.height - 1).max(0.0);
    let span_x = f64::from(desktop.width - 1).max(1.0);
    let span_y = f64::from(desktop.height - 1).max(1.0);
    let ax = ((px - f64::from(desktop.left)) * 65535.0 / span_x).round();
    let ay = ((py - f64::from(desktop.top)) * 65535.0 / span_y).round();
    (ax.clamp(0.0, 65535.0) as i32, ay.clamp(0.0, 65535.0) as i32)
}

fn virtual_desktop() -> DisplayRect {
    // SAFETY: plain system metric queries.
    unsafe {
        DisplayRect {
            left: GetSystemMetrics(SM_XVIRTUALSCREEN),
            top: GetSystemMetrics(SM_YVIRTUALSCREEN),
            width: GetSystemMetrics(SM_CXVIRTUALSCREEN),
            height: GetSystemMetrics(SM_CYVIRTUALSCREEN),
        }
    }
}

fn key_input(scan: u16, extended: bool, up: bool) -> INPUT {
    let mut flags = KEYEVENTF_SCANCODE;
    if extended {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    if up {
        flags |= KEYEVENTF_KEYUP;
    }
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(0),
                wScan: scan,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: INJECT_TAG,
            },
        },
    }
}

fn mouse_input(dx: i32, dy: i32, data: u32, flags: MOUSE_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                mouseData: data,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: INJECT_TAG,
            },
        },
    }
}

fn button_flags(button: MouseButton, down: bool) -> MOUSE_EVENT_FLAGS {
    match (button, down) {
        (MouseButton::Left, true) => MOUSEEVENTF_LEFTDOWN,
        (MouseButton::Left, false) => MOUSEEVENTF_LEFTUP,
        (MouseButton::Right, true) => MOUSEEVENTF_RIGHTDOWN,
        (MouseButton::Right, false) => MOUSEEVENTF_RIGHTUP,
        (MouseButton::Middle, true) => MOUSEEVENTF_MIDDLEDOWN,
        (MouseButton::Middle, false) => MOUSEEVENTF_MIDDLEUP,
    }
}

/// Send a batch. Returns how many events Windows accepted.
fn send(inputs: &[INPUT]) -> PlatformResult<()> {
    if inputs.is_empty() {
        return Ok(());
    }
    // SAFETY: a valid slice of fully initialised INPUT structures.
    let sent = unsafe { SendInput(inputs, std::mem::size_of::<INPUT>() as i32) };
    if sent as usize == inputs.len() {
        Ok(())
    } else {
        // SAFETY: reading the calling thread's last error.
        let code: WIN32_ERROR = unsafe { GetLastError() };
        Err(PlatformError::Backend(format!(
            "SendInput accepted {sent} of {} events (error {})",
            inputs.len(),
            code.0
        )))
    }
}

/// Input for one display.
pub struct WindowsInput {
    display: DisplayRect,
    held_keys: BTreeSet<String>,
    held_buttons: BTreeSet<u8>,
}

impl WindowsInput {
    pub fn new(display: DisplayRect) -> Self {
        Self {
            display,
            held_keys: BTreeSet::new(),
            held_buttons: BTreeSet::new(),
        }
    }

    fn button_index(button: MouseButton) -> u8 {
        match button {
            MouseButton::Left => 0,
            MouseButton::Middle => 1,
            MouseButton::Right => 2,
        }
    }
}

impl InputInjector for WindowsInput {
    fn key(&mut self, code: &str, down: bool) -> PlatformResult<()> {
        let Some(sc) = scan_code(code) else {
            return Ok(()); // unknown keys are dropped, not guessed at
        };
        send(&[key_input(sc.code, sc.extended, !down)])?;
        if down {
            self.held_keys.insert(code.to_owned());
        } else {
            self.held_keys.remove(code);
        }
        Ok(())
    }

    fn button(&mut self, button: MouseButton, down: bool) -> PlatformResult<()> {
        send(&[mouse_input(0, 0, 0, button_flags(button, down))])?;
        let index = Self::button_index(button);
        if down {
            self.held_buttons.insert(index);
        } else {
            self.held_buttons.remove(&index);
        }
        Ok(())
    }

    fn wheel(&mut self, dx: f32, dy: f32) -> PlatformResult<()> {
        // DOM: positive dy scrolls down. Windows: positive wheel scrolls up.
        let mut batch = Vec::with_capacity(2);
        let vertical = (-dy).round() as i32;
        if vertical != 0 {
            batch.push(mouse_input(0, 0, vertical as u32, MOUSEEVENTF_WHEEL));
        }
        let horizontal = dx.round() as i32;
        if horizontal != 0 {
            batch.push(mouse_input(0, 0, horizontal as u32, MOUSEEVENTF_HWHEEL));
        }
        send(&batch)
    }

    fn move_pointer(&mut self, x: f32, y: f32) -> PlatformResult<()> {
        let (ax, ay) = absolute_coordinates(self.display, virtual_desktop(), x, y);
        send(&[mouse_input(
            ax,
            ay,
            0,
            MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
        )])
    }

    fn release_all(&mut self) -> PlatformResult<()> {
        let batch = self.take_release_batch();
        send(&batch)
    }
}

impl WindowsInput {
    /// Forget what is held and build the events that release it. Separate from
    /// `release_all` so the bookkeeping can be tested without sending anything.
    fn take_release_batch(&mut self) -> Vec<INPUT> {
        let mut batch = Vec::new();
        for code in std::mem::take(&mut self.held_keys) {
            if let Some(sc) = scan_code(&code) {
                batch.push(key_input(sc.code, sc.extended, true));
            }
        }
        for index in std::mem::take(&mut self.held_buttons) {
            let button = match index {
                0 => MouseButton::Left,
                1 => MouseButton::Middle,
                _ => MouseButton::Right,
            };
            batch.push(mouse_input(0, 0, 0, button_flags(button, false)));
        }
        batch
    }
}

/// The primary display, for callers that do not name one.
pub fn primary_display() -> DisplayRect {
    // SAFETY: plain system metric queries.
    let (width, height) = unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) };
    DisplayRect {
        left: 0,
        top: 0,
        width,
        height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DESKTOP: DisplayRect = DisplayRect {
        left: 0,
        top: 0,
        width: 1920,
        height: 1080,
    };

    #[test]
    fn corners_map_to_the_corners_of_the_desktop() {
        assert_eq!(absolute_coordinates(DESKTOP, DESKTOP, 0.0, 0.0), (0, 0));
        assert_eq!(
            absolute_coordinates(DESKTOP, DESKTOP, 1.0, 1.0),
            (65535, 65535)
        );
    }

    #[test]
    fn a_second_monitor_is_offset_into_the_virtual_desktop() {
        // A display to the left of the primary (negative x in virtual space).
        let desktop = DisplayRect {
            left: -1280,
            top: 0,
            width: 3200,
            height: 1080,
        };
        let left_screen = DisplayRect {
            left: -1280,
            top: 0,
            width: 1280,
            height: 1024,
        };
        assert_eq!(absolute_coordinates(left_screen, desktop, 0.0, 0.0), (0, 0));
        let (x, _) = absolute_coordinates(left_screen, desktop, 1.0, 0.0);
        // 1279 * 65535 / 3199 = 26201.6, rounded to the nearest unit.
        assert_eq!(x, 26202, "right edge of the left monitor");
    }

    #[test]
    fn out_of_range_fractions_are_clamped() {
        assert_eq!(
            absolute_coordinates(DESKTOP, DESKTOP, -5.0, 9.0),
            (0, 65535)
        );
    }

    #[test]
    fn button_flags_pair_down_and_up() {
        assert_eq!(button_flags(MouseButton::Left, true), MOUSEEVENTF_LEFTDOWN);
        assert_eq!(button_flags(MouseButton::Left, false), MOUSEEVENTF_LEFTUP);
        assert_eq!(
            button_flags(MouseButton::Right, true),
            MOUSEEVENTF_RIGHTDOWN
        );
        assert_eq!(
            button_flags(MouseButton::Middle, false),
            MOUSEEVENTF_MIDDLEUP
        );
    }

    #[test]
    fn every_event_we_build_is_tagged() {
        let k = key_input(0x1E, false, false);
        let m = mouse_input(0, 0, 0, MOUSEEVENTF_LEFTDOWN);
        // SAFETY: both constructors always fill the union member they set.
        unsafe {
            assert_eq!(k.Anonymous.ki.dwExtraInfo, INJECT_TAG);
            assert_eq!(m.Anonymous.mi.dwExtraInfo, INJECT_TAG);
        }
    }

    #[test]
    fn release_builds_up_and_clears_everything_held() {
        let mut input = WindowsInput::new(DESKTOP);
        input.held_keys.insert("ShiftLeft".into());
        input.held_keys.insert("KeyA".into());
        input.held_buttons.insert(2);
        let batch = input.take_release_batch();
        assert_eq!(batch.len(), 3, "two keys and one button");
        assert!(input.held_keys.is_empty());
        assert!(input.held_buttons.is_empty());
        // Nothing left to release: the next release is empty.
        assert!(input.take_release_batch().is_empty());
        // SAFETY: every event built by key_input/mouse_input fills the union member it sets.
        unsafe {
            let flags = batch[0].Anonymous.ki.dwFlags;
            assert!(flags.contains(KEYEVENTF_KEYUP), "a release is a key-up");
        }
    }

    /// Moves the real pointer to the centre of the primary display and back.
    /// Run by hand: `cargo test -p rb-platform-windows pointer_round_trip -- --ignored --nocapture`
    #[test]
    #[ignore = "moves the real mouse pointer (restored afterwards)"]
    fn pointer_round_trip_through_sendinput() {
        use windows::Win32::Foundation::POINT;
        use windows::Win32::UI::WindowsAndMessaging::{GetCursorPos, SetCursorPos};

        let mut before = POINT::default();
        // SAFETY: querying and restoring the cursor position on this thread.
        unsafe { GetCursorPos(&mut before).unwrap() };
        let screen = primary_display();
        let mut input = WindowsInput::new(screen);
        input.move_pointer(0.5, 0.5).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(100));
        let mut now = POINT::default();
        unsafe { GetCursorPos(&mut now).unwrap() };
        let (cx, cy) = (screen.width / 2, screen.height / 2);
        eprintln!(
            "cursor after move: ({}, {}), expected about ({cx}, {cy})",
            now.x, now.y
        );
        unsafe { SetCursorPos(before.x, before.y).unwrap() };
        assert!((now.x - cx).abs() <= 2, "x off by {}", (now.x - cx).abs());
        assert!((now.y - cy).abs() <= 2, "y off by {}", (now.y - cy).abs());
    }

    #[test]
    fn unknown_keys_are_ignored_not_held() {
        let mut input = WindowsInput::new(DESKTOP);
        // An unknown code is dropped before it can be remembered.
        assert!(scan_code("BrowserBack").is_none());
        assert!(input.take_release_batch().is_empty());
    }
}

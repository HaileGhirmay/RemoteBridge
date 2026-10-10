//! Keeps injected input away from the consent dialogs.
//!
//! Rule 4 in `AGENTS.md`: remote input can never hide, restore, renew or
//! change permissions. The shortcuts already refuse injected keystrokes; this
//! does the same for the dialogs. While a dialog is up, its thread installs
//! low-level mouse and keyboard hooks. An event Windows marks as injected
//! (`SendInput` from any program, our own remote-input path included) is
//! dropped when it is aimed at a window this thread owns. Physical input is
//! untouched, and injected input to other windows passes through.
//!
//! Low-level hooks are delivered through the installing thread's message
//! loop, which every dialog here has: the approval window pumps its own, and
//! `MessageBoxW` pumps internally.

use std::sync::atomic::{AtomicU32, Ordering};

use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GA_ROOT, GetAncestor, GetForegroundWindow, GetWindowThreadProcessId, HC_ACTION,
    HHOOK, KBDLLHOOKSTRUCT, LLKHF_INJECTED, LLKHF_LOWER_IL_INJECTED, LLMHF_INJECTED,
    LLMHF_LOWER_IL_INJECTED, MSLLHOOKSTRUCT, SetWindowsHookExW, UnhookWindowsHookEx,
    WH_KEYBOARD_LL, WH_MOUSE_LL, WindowFromPoint,
};

/// Injected events refused so far. A count only, for diagnostics and tests.
static REJECTED: AtomicU32 = AtomicU32::new(0);

pub fn rejected_count() -> u32 {
    REJECTED.load(Ordering::Relaxed)
}

/// The two hooks, removed on drop. Install on the dialog's own thread, before
/// the dialog is shown, and keep it alive until the dialog is gone.
pub struct InputGuard {
    mouse: HHOOK,
    keyboard: HHOOK,
}

impl InputGuard {
    pub fn install() -> Option<Self> {
        // SAFETY: global low-level hooks whose callbacks live in this module;
        // both are removed in `Drop`.
        unsafe {
            let instance = GetModuleHandleW(None).ok().map(|m| HINSTANCE(m.0));
            let mouse = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook), instance, 0).ok()?;
            let keyboard = match SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_hook), instance, 0)
            {
                Ok(h) => h,
                Err(_) => {
                    let _ = UnhookWindowsHookEx(mouse);
                    return None;
                }
            };
            Some(Self { mouse, keyboard })
        }
    }
}

impl Drop for InputGuard {
    fn drop(&mut self) {
        // SAFETY: hooks this guard installed on this thread.
        unsafe {
            let _ = UnhookWindowsHookEx(self.keyboard);
            let _ = UnhookWindowsHookEx(self.mouse);
        }
    }
}

/// Whether `hwnd`'s top-level window belongs to the calling thread. The hook
/// callbacks run on the installing thread, so this means "one of the dialog's
/// own windows".
fn owned_by_this_thread(hwnd: HWND) -> bool {
    if hwnd.0.is_null() {
        return false;
    }
    // SAFETY: plain window queries on a handle Windows just gave us.
    unsafe {
        let root = GetAncestor(hwnd, GA_ROOT);
        let root = if root.0.is_null() { hwnd } else { root };
        GetWindowThreadProcessId(root, None) == GetCurrentThreadId()
    }
}

fn refuse() -> LRESULT {
    REJECTED.fetch_add(1, Ordering::Relaxed);
    // A non-zero return stops the event here.
    LRESULT(1)
}

unsafe extern "system" fn mouse_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        // SAFETY: for WH_MOUSE_LL with HC_ACTION, lparam points to an MSLLHOOKSTRUCT.
        let info = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };
        let injected = info.flags & (LLMHF_INJECTED | LLMHF_LOWER_IL_INJECTED) != 0;
        // SAFETY: a plain window query.
        if injected && owned_by_this_thread(unsafe { WindowFromPoint(info.pt) }) {
            return refuse();
        }
    }
    // SAFETY: passing the event on unchanged.
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

unsafe extern "system" fn keyboard_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        // SAFETY: for WH_KEYBOARD_LL with HC_ACTION, lparam points to a KBDLLHOOKSTRUCT.
        let info = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        let injected = info.flags.0 & (LLKHF_INJECTED.0 | LLKHF_LOWER_IL_INJECTED.0) != 0;
        // SAFETY: a plain window query. Keys go to the foreground window.
        if injected && owned_by_this_thread(unsafe { GetForegroundWindow() }) {
            return refuse();
        }
    }
    // SAFETY: passing the event on unchanged.
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::time::Duration;

    use rb_core::Permissions;
    use rb_core::traits::{ConsentAnswer, InputInjector, PromptId};
    use rb_core::types::MouseButton;
    use windows::Win32::Foundation::{LPARAM, RECT, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{
        FindWindowW, GetDlgItem, GetWindowRect, IDOK, PostMessageW, SetCursorPos, WM_CLOSE,
    };
    use windows::core::{HSTRING, PCWSTR};

    use super::rejected_count;
    use crate::input::{WindowsInput, primary_display};

    /// Opens the real approval window, then uses the host's own remote-input
    /// injector to click Allow and press Enter. Neither may answer the prompt.
    /// `cargo test -p rb-platform-windows injected_input -- --ignored --nocapture`
    #[test]
    #[ignore = "opens the approval window and injects input into it"]
    fn injected_input_cannot_press_allow() {
        let (tx, rx) = mpsc::channel();
        let caption = "RemoteBridge guard test".to_string();
        crate::approval::show(
            PromptId(900),
            caption.clone(),
            "Test".into(),
            "test@example.com".into(),
            true,
            Permissions {
                control: true,
                ..Permissions::NONE
            },
            tx,
        )
        .unwrap();

        let cap = HSTRING::from(caption.as_str());
        let mut window = None;
        for _ in 0..50 {
            // SAFETY: a lookup by caption.
            if let Ok(h) = unsafe { FindWindowW(PCWSTR::null(), &cap) } {
                window = Some(h);
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let hwnd = window.expect("the approval window appears");
        std::thread::sleep(Duration::from_millis(300));

        // Put the real cursor over Allow (not injected), then inject the click.
        // SAFETY: querying our own window's button and moving the cursor.
        let (cx, cy) = unsafe {
            let allow = GetDlgItem(Some(hwnd), IDOK.0).expect("Allow button");
            let mut r = RECT::default();
            GetWindowRect(allow, &mut r).unwrap();
            ((r.left + r.right) / 2, (r.top + r.bottom) / 2)
        };
        // SAFETY: a plain cursor move.
        unsafe { SetCursorPos(cx, cy) }.unwrap();
        std::thread::sleep(Duration::from_millis(100));

        let before = rejected_count();
        let mut input = WindowsInput::new(primary_display());
        input.button(MouseButton::Left, true).unwrap();
        input.button(MouseButton::Left, false).unwrap();
        input.key("Enter", true).unwrap();
        input.key("Enter", false).unwrap();
        std::thread::sleep(Duration::from_millis(400));

        assert!(
            rx.try_recv().is_err(),
            "an injected click or Enter must not answer the prompt"
        );
        // SAFETY: a lookup by caption.
        assert!(
            unsafe { FindWindowW(PCWSTR::null(), &cap) }.is_ok(),
            "the window is still open"
        );
        assert!(
            rejected_count() > before,
            "the guard refused the injected events"
        );

        // Closing it (as the local user would) still works and denies.
        // SAFETY: posting to our own window.
        unsafe {
            let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
        let (id, answer) = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(id, PromptId(900));
        assert_eq!(answer, ConsentAnswer::Deny);
    }
}

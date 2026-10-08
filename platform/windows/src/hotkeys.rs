//! Global shortcuts on Windows.
//!
//! * `RegisterHotKey` for `Ctrl+Alt+Shift+B` (restore indicators) and
//!   `Ctrl+Alt+Shift+X` (emergency disconnect). A refused registration is
//!   reported as a collision so the host can fall back to the tray menu.
//! * A low-level keyboard hook sees every key event with its
//!   `LLKHF_INJECTED` flag and feeds a [`HotkeyGate`]. Windows raises
//!   `WM_HOTKEY` for injected input too, so every notification is confirmed
//!   against the gate: only a chord typed on the physical keyboard is
//!   delivered. If the hook cannot be installed the shortcuts are **not**
//!   registered at all (fail closed), because they could not be told apart
//!   from remote input.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::Instant;

use rb_core::indicators::{HotkeyGate, Key, KeyEvent};
use rb_core::traits::{HotkeyEvent, HotkeyRegistration, Hotkeys};
use rb_core::{PlatformError, PlatformResult};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, RegisterHotKey,
    UnregisterHotKey,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
    HHOOK, HWND_MESSAGE, KBDLLHOOKSTRUCT, MSG, PostThreadMessageW, RegisterClassW,
    SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx, UnregisterClassW, WH_KEYBOARD_LL,
    WINDOW_EX_STYLE, WINDOW_STYLE, WM_HOTKEY, WM_KEYDOWN, WM_KEYUP, WM_QUIT, WM_SYSKEYDOWN,
    WM_SYSKEYUP, WNDCLASSW,
};
use windows::core::w;

const ID_RESTORE: i32 = 1;
const ID_DISCONNECT: i32 = 2;
const VK_B: u32 = 0x42;
const VK_X: u32 = 0x58;
/// `LLKHF_INJECTED`: posted by software.
const LLKHF_INJECTED: u32 = 0x10;
/// `LLKHF_LOWER_IL_INJECTED`: posted by a lower-integrity process.
const LLKHF_LOWER_IL_INJECTED: u32 = 0x02;

const RESTORE_LABEL: &str = "Ctrl+Alt+Shift+B";
const DISCONNECT_LABEL: &str = "Ctrl+Alt+Shift+X";

#[derive(Default)]
struct Shared {
    events: Mutex<VecDeque<HotkeyEvent>>,
    rejected: AtomicU32,
}

struct ThreadState {
    gate: HotkeyGate,
    started: Instant,
    shared: Arc<Shared>,
}

thread_local! {
    // The hook and the window procedure both run on the hotkey thread.
    static STATE: RefCell<Option<ThreadState>> = const { RefCell::new(None) };
}

fn now_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn classify(vk: u32) -> Key {
    match vk {
        0x11 | 0xA2 | 0xA3 => Key::Ctrl,
        0x12 | 0xA4 | 0xA5 => Key::Alt,
        0x10 | 0xA0 | 0xA1 => Key::Shift,
        VK_B => Key::B,
        VK_X => Key::X,
        _ => Key::Other,
    }
}

unsafe extern "system" fn keyboard_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // HC_ACTION is 0; anything else must be passed on untouched.
    if code == 0 {
        // SAFETY: for WH_KEYBOARD_LL with HC_ACTION, lparam points to a KBDLLHOOKSTRUCT.
        let info = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        let message = u32::try_from(wparam.0).unwrap_or(0);
        let down = matches!(message, WM_KEYDOWN | WM_SYSKEYDOWN);
        let up = matches!(message, WM_KEYUP | WM_SYSKEYUP);
        if down || up {
            let injected = info.flags.0 & (LLKHF_INJECTED | LLKHF_LOWER_IL_INJECTED) != 0;
            STATE.with(|s| {
                if let Some(state) = s.borrow_mut().as_mut() {
                    state.gate.observe(KeyEvent {
                        key: classify(info.vkCode),
                        down,
                        injected,
                        at_ms: now_ms(state.started),
                    });
                }
            });
        }
    }
    // SAFETY: forwarding the unchanged arguments to the next hook.
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_HOTKEY {
        let which = match i32::try_from(wparam.0) {
            Ok(ID_RESTORE) => Some(HotkeyEvent::RestoreIndicators),
            Ok(ID_DISCONNECT) => Some(HotkeyEvent::EmergencyDisconnect),
            _ => None,
        };
        if let Some(which) = which {
            STATE.with(|s| {
                if let Some(state) = s.borrow_mut().as_mut() {
                    if state.gate.confirm(which, now_ms(state.started)) {
                        state.shared.events.lock().unwrap().push_back(which);
                    } else {
                        state.shared.rejected.fetch_add(1, Ordering::Relaxed);
                    }
                }
            });
        }
        return LRESULT(0);
    }
    // SAFETY: default handling with the arguments we were given.
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}

struct Started {
    registration: HotkeyRegistration,
    thread_id: u32,
}

fn backend(msg: &str) -> PlatformError {
    PlatformError::Backend(msg.to_owned())
}

/// Runs on the hotkey thread: window, hotkeys, hook, message loop, cleanup.
fn run(shared: Arc<Shared>, ready: mpsc::Sender<Result<Started, PlatformError>>) {
    // SAFETY: plain Win32 calls on this thread; every handle is released below.
    unsafe {
        let Ok(module) = GetModuleHandleW(None) else {
            let _ = ready.send(Err(backend("GetModuleHandleW failed")));
            return;
        };
        let instance = HINSTANCE(module.0);
        let class = w!("RemoteBridgeHotkeys");
        let class_def = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            lpszClassName: class,
            ..Default::default()
        };
        // 0 can mean "already registered", which is fine.
        let _ = RegisterClassW(&class_def);
        let hwnd = match CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class,
            w!("RemoteBridge hotkeys"),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            Some(instance),
            None,
        ) {
            Ok(h) => h,
            Err(_) => {
                let _ = ready.send(Err(backend("could not create the hotkey window")));
                return;
            }
        };

        STATE.with(|s| {
            *s.borrow_mut() = Some(ThreadState {
                gate: HotkeyGate::new(),
                started: Instant::now(),
                shared: shared.clone(),
            });
        });

        // The hook first: without it the shortcuts could not tell injected
        // input apart, so we must not register them.
        let hook: HHOOK =
            match SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_hook), Some(instance), 0) {
                Ok(h) => h,
                Err(_) => {
                    let _ = DestroyWindow(hwnd);
                    let _ = UnregisterClassW(class, Some(instance));
                    let _ = ready.send(Err(backend(
                        "keyboard hook unavailable; shortcuts cannot reject injected input",
                    )));
                    return;
                }
            };

        let modifiers: HOT_KEY_MODIFIERS = MOD_CONTROL | MOD_ALT | MOD_SHIFT | MOD_NOREPEAT;
        let restore = RegisterHotKey(Some(hwnd), ID_RESTORE, modifiers, VK_B).is_ok();
        let disconnect = RegisterHotKey(Some(hwnd), ID_DISCONNECT, modifiers, VK_X).is_ok();
        let _ = ready.send(Ok(Started {
            registration: HotkeyRegistration {
                restore,
                emergency_disconnect: disconnect,
            },
            thread_id: GetCurrentThreadId(),
        }));

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        if restore {
            let _ = UnregisterHotKey(Some(hwnd), ID_RESTORE);
        }
        if disconnect {
            let _ = UnregisterHotKey(Some(hwnd), ID_DISCONNECT);
        }
        let _ = UnhookWindowsHookEx(hook);
        let _ = DestroyWindow(hwnd);
        let _ = UnregisterClassW(class, Some(instance));
        STATE.with(|s| *s.borrow_mut() = None);
    }
}

struct Running {
    thread: JoinHandle<()>,
    thread_id: u32,
}

pub struct WindowsHotkeys {
    shared: Arc<Shared>,
    running: Option<Running>,
}

impl WindowsHotkeys {
    pub fn new() -> Self {
        Self {
            shared: Arc::new(Shared::default()),
            running: None,
        }
    }

    /// Hotkey notifications refused because no physical chord backed them
    /// (injected input). A count only.
    pub fn rejected_injected(&self) -> u32 {
        self.shared.rejected.load(Ordering::Relaxed)
    }
}

impl Default for WindowsHotkeys {
    fn default() -> Self {
        Self::new()
    }
}

impl Hotkeys for WindowsHotkeys {
    fn register(&mut self) -> PlatformResult<HotkeyRegistration> {
        self.unregister();
        let (tx, rx) = mpsc::channel();
        let shared = self.shared.clone();
        let thread = std::thread::Builder::new()
            .name("rb-hotkeys".into())
            .spawn(move || run(shared, tx))
            .map_err(|e| PlatformError::Backend(e.to_string()))?;
        let started = match rx.recv() {
            Ok(Ok(started)) => started,
            Ok(Err(e)) => {
                let _ = thread.join();
                return Err(e);
            }
            Err(_) => {
                let _ = thread.join();
                return Err(backend("hotkey thread ended unexpectedly"));
            }
        };
        self.running = Some(Running {
            thread,
            thread_id: started.thread_id,
        });
        Ok(started.registration)
    }

    fn unregister(&mut self) {
        if let Some(running) = self.running.take() {
            // SAFETY: posting WM_QUIT to a thread we started; failure means it is gone.
            let _ = unsafe { PostThreadMessageW(running.thread_id, WM_QUIT, WPARAM(0), LPARAM(0)) };
            let _ = running.thread.join();
        }
        self.shared.events.lock().unwrap().clear();
    }

    fn poll(&mut self) -> Option<HotkeyEvent> {
        self.shared.events.lock().unwrap().pop_front()
    }

    fn restore_label(&self) -> String {
        RESTORE_LABEL.into()
    }

    fn disconnect_label(&self) -> String {
        DISCONNECT_LABEL.into()
    }
}

impl Drop for WindowsHotkeys {
    fn drop(&mut self) {
        self.unregister();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;
    use std::time::Duration;
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_KEYUP, SendInput,
        VIRTUAL_KEY,
    };

    /// Global shortcuts are process-wide: tests must not run at the same time.
    static ONE_AT_A_TIME: StdMutex<()> = StdMutex::new(());

    #[test]
    fn classifies_left_right_and_generic_modifiers() {
        for vk in [0x11, 0xA2, 0xA3] {
            assert_eq!(classify(vk), Key::Ctrl);
        }
        for vk in [0x12, 0xA4, 0xA5] {
            assert_eq!(classify(vk), Key::Alt);
        }
        for vk in [0x10, 0xA0, 0xA1] {
            assert_eq!(classify(vk), Key::Shift);
        }
        assert_eq!(classify(VK_B), Key::B);
        assert_eq!(classify(VK_X), Key::X);
        assert_eq!(classify(0x41), Key::Other);
    }

    #[test]
    fn registers_reports_labels_and_unregisters_cleanly() {
        let _g = ONE_AT_A_TIME.lock().unwrap();
        let mut hk = WindowsHotkeys::new();
        assert_eq!(hk.restore_label(), "Ctrl+Alt+Shift+B");
        assert_eq!(hk.disconnect_label(), "Ctrl+Alt+Shift+X");
        let reg = hk.register().expect("hook and window must start");
        eprintln!("hotkey registration on this machine: {reg:?}");
        assert!(
            reg.restore && reg.emergency_disconnect,
            "shortcuts already taken by another app?"
        );
        assert!(!reg.collision());
        assert_eq!(hk.poll(), None);

        // Registering again replaces the old registration.
        let again = hk.register().unwrap();
        assert_eq!(again, reg);
        hk.unregister();
        hk.unregister();

        // After unregistering, the same shortcuts can be taken again.
        let mut other = WindowsHotkeys::new();
        assert!(!other.register().unwrap().collision());
    }

    #[test]
    fn a_second_host_instance_sees_a_collision_and_the_first_keeps_working() {
        let _g = ONE_AT_A_TIME.lock().unwrap();
        let mut first = WindowsHotkeys::new();
        assert!(!first.register().unwrap().collision());
        let mut second = WindowsHotkeys::new();
        let reg = second.register().unwrap();
        assert!(
            reg.collision(),
            "the OS refuses a shortcut that is already registered"
        );
        assert!(!reg.restore && !reg.emergency_disconnect);
    }

    fn key_input(vk: u16, up: bool) -> INPUT {
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VIRTUAL_KEY(vk),
                    wScan: 0,
                    dwFlags: if up {
                        KEYEVENTF_KEYUP
                    } else {
                        KEYBD_EVENT_FLAGS(0)
                    },
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        }
    }

    /// Types the emergency chord with `SendInput`, exactly what remote control
    /// does. This presses real keys in whatever window has focus, so it only
    /// runs on request:
    /// `cargo test -p rb-platform-windows injected -- --ignored --nocapture`
    #[test]
    #[ignore = "sends synthetic keystrokes to the focused window"]
    fn injected_shortcuts_never_reach_the_host() {
        let _g = ONE_AT_A_TIME.lock().unwrap();
        let mut hk = WindowsHotkeys::new();
        assert!(!hk.register().unwrap().collision());

        for letter in [VK_X as u16, VK_B as u16] {
            let sequence = [
                key_input(0x11, false),
                key_input(0x12, false),
                key_input(0x10, false),
                key_input(letter, false),
                key_input(letter, true),
                key_input(0x10, true),
                key_input(0x12, true),
                key_input(0x11, true),
            ];
            // SAFETY: a valid slice of INPUT structs of the right size.
            let sent = unsafe { SendInput(&sequence, std::mem::size_of::<INPUT>() as i32) };
            assert_eq!(sent as usize, sequence.len());
            std::thread::sleep(Duration::from_millis(400));
        }
        assert_eq!(
            hk.poll(),
            None,
            "injected input must not trigger the shortcuts"
        );
        assert!(
            hk.rejected_injected() >= 1,
            "Windows raised the hotkey; the gate refused it"
        );
    }
}

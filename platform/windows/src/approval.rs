//! The approval window on Windows: one checkbox per opt-in.
//!
//! Shown for an attended request. Viewing is always included; control, system
//! audio, microphone and clipboard each have a checkbox that starts unticked,
//! and a box is disabled when the viewer did not ask for that permission. Allow
//! grants exactly the ticked boxes; Deny, Esc, or closing the window denies.
//!
//! The window runs on its own thread with its own message loop, so `show`
//! returns at once. The answer goes back on a channel. What each control means
//! is decided in `prompt.rs`, which is tested on every OS; this file is only
//! the Win32 plumbing.

use std::cell::RefCell;
use std::sync::mpsc::Sender;
use std::thread;

use rb_core::Permissions;
use rb_core::traits::{ConsentAnswer, PromptId};
use rb_core::{PlatformError, PlatformResult};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{COLOR_WINDOW, DEFAULT_GUI_FONT, GetStockObject, HBRUSH};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    AdjustWindowRectEx, BM_GETCHECK, BS_AUTOCHECKBOX, BS_DEFPUSHBUTTON, BS_PUSHBUTTON,
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
    GetSystemMetrics, HMENU, IDCANCEL, IDOK, IsDialogMessageW, MSG, PostQuitMessage, SM_CXSCREEN,
    SM_CYSCREEN, SW_SHOW, SendMessageW, SetForegroundWindow, ShowWindow, TranslateMessage,
    WINDOW_EX_STYLE, WINDOW_STYLE, WM_CLOSE, WM_COMMAND, WM_DESTROY, WM_SETFONT, WS_CAPTION,
    WS_CHILD, WS_EX_DLGMODALFRAME, WS_EX_TOPMOST, WS_OVERLAPPED, WS_SYSMENU, WS_TABSTOP,
    WS_VISIBLE,
};
use windows::core::{HSTRING, w};

use crate::prompt::{OPT_IN_HINTS, OPT_IN_LABELS, approval_answer, approval_intro};
use crate::ui::register;

/// `BM_GETCHECK` result for a ticked box (`BST_CHECKED`; the constant lives in
/// a `windows` feature this crate does not otherwise need).
const CHECKED: LRESULT = LRESULT(1);

// Layout, in pixels at 100 % scaling.
const WIDTH: i32 = 460;
const MARGIN: i32 = 16;
const INTRO_H: i32 = 84;
const ROW_H: i32 = 56;
const BOX_H: i32 = 24;
const HINT_H: i32 = 18;
const BUTTON_W: i32 = 100;
const BUTTON_H: i32 = 30;
const ROWS_TOP: i32 = MARGIN + INTRO_H + 12;
const BUTTONS_TOP: i32 = ROWS_TOP + 4 * ROW_H;
const HEIGHT: i32 = BUTTONS_TOP + BUTTON_H + MARGIN;

/// Everything the window procedure needs, for the one window on this thread.
struct State {
    id: PromptId,
    requested: Permissions,
    boxes: [HWND; 4],
    answers: Sender<(PromptId, ConsentAnswer)>,
    answered: bool,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

/// Open the approval window on its own thread. The answer arrives on
/// `answers`; if the window cannot be built, the request is denied so the
/// viewer is not left waiting on a prompt nobody can see.
pub fn show(
    id: PromptId,
    caption: String,
    requester_name: String,
    requester_email: String,
    requester_verified: bool,
    requested: Permissions,
    answers: Sender<(PromptId, ConsentAnswer)>,
) -> PlatformResult<()> {
    thread::Builder::new()
        .name(format!("approval-{}", id.0))
        .spawn(move || {
            let intro = approval_intro(&requester_name, &requester_email, requester_verified);
            // SAFETY: Win32 setup and message loop on this thread only; the window
            // and its children are destroyed before the thread ends.
            let built = unsafe { build(id, &caption, &intro, requested, answers.clone()) };
            match built {
                Ok(hwnd) => unsafe { run_loop(hwnd) },
                Err(e) => {
                    log::warn!("approval window could not be shown: {e}; denying");
                    let _ = answers.send((id, ConsentAnswer::Deny));
                }
            }
            // The loop has ended, so nothing else can answer; deny if nothing did.
            let unanswered =
                STATE.with(|s| s.borrow_mut().take().filter(|st| !st.answered).is_some());
            if unanswered {
                let _ = answers.send((id, ConsentAnswer::Deny));
            }
        })
        .map_err(|e| PlatformError::Backend(format!("approval thread: {e}")))?;
    Ok(())
}

unsafe fn build(
    id: PromptId,
    caption: &str,
    intro: &str,
    requested: Permissions,
    answers: Sender<(PromptId, ConsentAnswer)>,
) -> PlatformResult<HWND> {
    let fail = |what: &str| PlatformError::Backend(format!("approval window: {what}"));
    // SAFETY: the caller promises this runs on the window's own thread.
    unsafe {
        let module = GetModuleHandleW(None).map_err(|_| fail("module handle"))?;
        let instance = HINSTANCE(module.0);
        // The standard window colour: `hbrBackground = COLOR_WINDOW + 1`.
        let background = HBRUSH((COLOR_WINDOW.0 as usize + 1) as *mut _);
        register(
            w!("RemoteBridgeApproval"),
            Some(approval_proc),
            instance,
            background,
        );

        let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU;
        let ex_style = WS_EX_TOPMOST | WS_EX_DLGMODALFRAME;
        let mut frame = RECT {
            left: 0,
            top: 0,
            right: WIDTH,
            bottom: HEIGHT,
        };
        let _ = AdjustWindowRectEx(&mut frame, style, false, ex_style);
        let outer_w = frame.right - frame.left;
        let outer_h = frame.bottom - frame.top;
        let x = (GetSystemMetrics(SM_CXSCREEN) - outer_w).max(0) / 2;
        let y = (GetSystemMetrics(SM_CYSCREEN) - outer_h).max(0) / 2;

        let hwnd = CreateWindowExW(
            ex_style,
            w!("RemoteBridgeApproval"),
            &HSTRING::from(caption),
            style,
            x,
            y,
            outer_w,
            outer_h,
            None,
            None,
            Some(instance),
            None,
        )
        .map_err(|_| fail("create"))?;

        let font = GetStockObject(DEFAULT_GUI_FONT);
        let set_font = |child: HWND| {
            SendMessageW(
                child,
                WM_SETFONT,
                Some(WPARAM(font.0 as usize)),
                Some(LPARAM(1)),
            );
        };
        let child = |class, text: &str, style: WINDOW_STYLE, x, y, w, h, menu: Option<HMENU>| {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                class,
                &HSTRING::from(text),
                WS_CHILD | WS_VISIBLE | style,
                x,
                y,
                w,
                h,
                Some(hwnd),
                menu,
                Some(instance),
                None,
            )
        };

        let intro_hwnd = child(
            w!("STATIC"),
            intro,
            WINDOW_STYLE(0),
            MARGIN,
            MARGIN,
            WIDTH - 2 * MARGIN,
            INTRO_H,
            None,
        )
        .map_err(|_| fail("intro text"))?;
        set_font(intro_hwnd);

        let enabled = crate::prompt::opt_in_enabled(&requested);
        let mut boxes = [HWND::default(); 4];
        for (i, slot) in boxes.iter_mut().enumerate() {
            let top = ROWS_TOP + i as i32 * ROW_H;
            let check = child(
                w!("BUTTON"),
                OPT_IN_LABELS[i],
                WS_TABSTOP | WINDOW_STYLE(BS_AUTOCHECKBOX as u32),
                MARGIN,
                top,
                WIDTH - 2 * MARGIN,
                BOX_H,
                None,
            )
            .map_err(|_| fail("checkbox"))?;
            set_font(check);
            let hint = if enabled[i] {
                OPT_IN_HINTS[i]
            } else {
                "Not requested by the viewer."
            };
            let hint_hwnd = child(
                w!("STATIC"),
                hint,
                WINDOW_STYLE(0),
                MARGIN + 20,
                top + BOX_H,
                WIDTH - 2 * MARGIN - 20,
                HINT_H,
                None,
            )
            .map_err(|_| fail("hint text"))?;
            set_font(hint_hwnd);
            if !enabled[i] {
                let _ = EnableWindow(check, false);
            }
            *slot = check;
        }

        let deny_x = WIDTH - MARGIN - BUTTON_W;
        let allow_x = deny_x - 8 - BUTTON_W;
        // Allow is the default button (Enter) with id IDOK; Deny has id IDCANCEL,
        // which `IsDialogMessageW` also sends for Esc.
        let allow = child(
            w!("BUTTON"),
            "Allow",
            WS_TABSTOP | WINDOW_STYLE(BS_DEFPUSHBUTTON as u32),
            allow_x,
            BUTTONS_TOP,
            BUTTON_W,
            BUTTON_H,
            Some(HMENU(IDOK.0 as usize as *mut _)),
        )
        .map_err(|_| fail("allow button"))?;
        set_font(allow);
        let deny = child(
            w!("BUTTON"),
            "Deny",
            WS_TABSTOP | WINDOW_STYLE(BS_PUSHBUTTON as u32),
            deny_x,
            BUTTONS_TOP,
            BUTTON_W,
            BUTTON_H,
            Some(HMENU(IDCANCEL.0 as usize as *mut _)),
        )
        .map_err(|_| fail("deny button"))?;
        set_font(deny);

        STATE.with(|s| {
            *s.borrow_mut() = Some(State {
                id,
                requested,
                boxes,
                answers,
                answered: false,
            })
        });

        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        Ok(hwnd)
    }
}

unsafe fn run_loop(hwnd: HWND) {
    let mut msg = MSG::default();
    // SAFETY: a standard message loop on the thread that owns `hwnd`.
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).into() {
            // Tab moves between the boxes and buttons; Enter and Esc press them.
            if !IsDialogMessageW(hwnd, &msg).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }
}

/// Send the answer once. The state is copied out before any Win32 call, so a
/// message arriving during the call cannot find the state borrowed.
fn answer(allow: bool) {
    let taken = STATE.with(|s| {
        let mut s = s.borrow_mut();
        match s.as_mut() {
            Some(st) if !st.answered => {
                st.answered = true;
                Some((st.id, st.requested, st.boxes, st.answers.clone()))
            }
            _ => None,
        }
    });
    let Some((id, requested, boxes, answers)) = taken else {
        return;
    };
    let answer = if allow {
        // SAFETY: querying child buttons of a window on this thread.
        let checked = boxes.map(|b| unsafe { SendMessageW(b, BM_GETCHECK, None, None) } == CHECKED);
        approval_answer(&requested, checked)
    } else {
        ConsentAnswer::Deny
    };
    let _ = answers.send((id, answer));
}

unsafe extern "system" fn approval_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_COMMAND => {
            let id = (wparam.0 & 0xffff) as i32;
            let decision = if id == IDOK.0 {
                Some(true)
            } else if id == IDCANCEL.0 {
                Some(false)
            } else {
                None
            };
            if let Some(allow) = decision {
                answer(allow);
                // SAFETY: destroying our own window on its thread.
                let _ = unsafe { DestroyWindow(hwnd) };
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            answer(false);
            // SAFETY: destroying our own window on its thread.
            let _ = unsafe { DestroyWindow(hwnd) };
            LRESULT(0)
        }
        WM_DESTROY => {
            // SAFETY: ends this thread's message loop.
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        // SAFETY: default handling.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

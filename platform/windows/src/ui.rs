//! The banner and the tray icon on Windows.
//!
//! * **Banner**: on every display, always on top, "<name> can see your screen"
//!   with what is granted, and two buttons. The text window is click-through
//!   (`WS_EX_TRANSPARENT`); only the small button window takes clicks. The
//!   windows are excluded from capture with `WDA_EXCLUDEFROMCAPTURE`, so the
//!   viewer's stream does not show a banner inside the banner. Windows has no
//!   per-application exclusion, so *any* capture tool skips these windows; the
//!   person at the machine still sees them on the physical screen, and the OS
//!   capture indicators are untouched.
//! * **Tray**: an icon that changes while a session is live, with a menu that
//!   names who is connected, shows time left on consent, and offers
//!   "Show indicators" and "Disconnect now" (the fallbacks for the shortcuts).
//!
//! Everything runs on one UI thread. Calls from the host are synchronous
//! (`SendMessageW`), so `show()` has taken effect when it returns.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;

use rb_core::traits::{
    Banner, BannerAction, BannerContent, HideChoice, Tray, TrayAction, TrayModel,
};
use rb_core::{PlatformError, PlatformResult};

use crate::prompt::permission_words;
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BACKGROUND_MODE, BeginPaint, CreateBitmap, CreateFontW, CreateSolidBrush, DEFAULT_GUI_FONT,
    DeleteObject, EndPaint, EnumDisplayMonitors, FillRect, GetStockObject, HBRUSH, HDC, HFONT,
    HGDIOBJ, HMONITOR, InvalidateRect, PAINTSTRUCT, SelectObject, SetBkMode, SetTextColor,
    TextOutW,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW,
    Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreateIconIndirect, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyIcon,
    DestroyMenu, DestroyWindow, DispatchMessageW, GetClientRect, GetCursorPos, GetMessageW, HICON,
    HMENU, ICONINFO, LWA_ALPHA, MF_DISABLED, MF_GRAYED, MF_SEPARATOR, MF_STRING, MSG, PostMessageW,
    PostQuitMessage, RegisterClassW, RegisterWindowMessageW, SW_SHOWNOACTIVATE, SendMessageW,
    SetForegroundWindow, SetLayeredWindowAttributes, SetWindowDisplayAffinity, ShowWindow,
    TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu, TranslateMessage,
    UnregisterClassW, WINDOW_DISPLAY_AFFINITY, WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WM_CLOSE,
    WM_COMMAND, WM_CONTEXTMENU, WM_DESTROY, WM_DISPLAYCHANGE, WM_LBUTTONUP, WM_PAINT, WM_RBUTTONUP,
    WM_SETFONT, WNDCLASSW, WS_CHILD, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP, WS_VISIBLE,
};
use windows::core::{BOOL, PCWSTR, w};

const WM_WAKE: u32 = WM_APP + 1;
const WM_TRAY: u32 = WM_APP + 2;

const ID_BTN_HIDE: usize = 1;
const ID_BTN_DISCONNECT: usize = 2;
const ID_HIDE_30: usize = 101;
const ID_HIDE_60: usize = 102;
const ID_HIDE_120: usize = 103;
const ID_HIDE_240: usize = 104;
const ID_HIDE_480: usize = 105;
const ID_HIDE_END: usize = 106;
const ID_TRAY_SHOW: usize = 201;
const ID_TRAY_DISCONNECT: usize = 202;

/// `WDA_EXCLUDEFROMCAPTURE` (Windows 10 2004 and later).
const WDA_EXCLUDEFROMCAPTURE: u32 = 0x11;

const TEXT_W: i32 = 500;
const BUTTONS_W: i32 = 214;
const BAR_H: i32 = 48;
const BAR_TOP: i32 = 6;

const BG: COLORREF = rgb(0x1f, 0x2d, 0x33);
const FG: COLORREF = rgb(0xf4, 0xf1, 0xea);
const SIGNAL: COLORREF = rgb(0xe0, 0x9f, 0x1f);

const fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    COLORREF((r as u32) | ((g as u32) << 8) | ((b as u32) << 16))
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

fn wide_z(s: &str) -> Vec<u16> {
    let mut v = wide(s);
    v.push(0);
    v
}

fn backend(msg: impl Into<String>) -> PlatformError {
    PlatformError::Backend(msg.into())
}

enum Command {
    ShowBanner(BannerContent),
    HideBanner,
    SetTray(TrayModel),
}

#[derive(Default)]
struct Shared {
    commands: Mutex<VecDeque<Command>>,
    banner_actions: Mutex<VecDeque<BannerAction>>,
    tray_actions: Mutex<VecDeque<TrayAction>>,
    banner_visible: AtomicBool,
    tray_icon_added: AtomicBool,
    main_hwnd: AtomicIsize,
    /// (text window, button window) per display, for tests.
    bars: Mutex<Vec<(isize, isize)>>,
}

impl Shared {
    fn main(&self) -> HWND {
        HWND(self.main_hwnd.load(Ordering::SeqCst) as *mut _)
    }

    /// Queue a command and wait until the UI thread has applied it.
    fn send(&self, command: Command) {
        self.commands.lock().unwrap().push_back(command);
        // SAFETY: SendMessageW to a window owned by the UI thread blocks until it has handled it.
        unsafe {
            SendMessageW(self.main(), WM_WAKE, Some(WPARAM(0)), Some(LPARAM(0)));
        }
    }
}

// ---- thread-local state ---------------------------------------------------

/// What the paint handler needs. Window procedures only touch this and
/// `SHARED`, never `UI`, so creating windows while `UI` is borrowed is safe.
struct PaintData {
    title: String,
    detail: String,
    title_font: HFONT,
    detail_font: HFONT,
    background: HBRUSH,
}

struct Ui {
    shared: Arc<Shared>,
    instance: HINSTANCE,
    main: HWND,
    bars: Vec<(HWND, HWND)>,
    banner: Option<BannerContent>,
    tray: Option<TrayModel>,
    tray_added: bool,
    icon_idle: HICON,
    icon_live: HICON,
    taskbar_created: u32,
}

thread_local! {
    static SHARED: RefCell<Option<Arc<Shared>>> = const { RefCell::new(None) };
    static PAINT: RefCell<Option<PaintData>> = const { RefCell::new(None) };
    static UI: RefCell<Option<Ui>> = const { RefCell::new(None) };
}

fn push_banner_action(action: BannerAction) {
    SHARED.with(|s| {
        if let Some(shared) = s.borrow().as_ref() {
            shared.banner_actions.lock().unwrap().push_back(action);
        }
    });
}

fn push_tray_action(action: TrayAction) {
    SHARED.with(|s| {
        if let Some(shared) = s.borrow().as_ref() {
            shared.tray_actions.lock().unwrap().push_back(action);
        }
    });
}

// ---- text -------------------------------------------------------------------

fn banner_title(content: &BannerContent) -> String {
    format!("{} can see your screen", content.requester_name)
}

// ---- window procedures ----------------------------------------------------------

unsafe extern "system" fn text_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_PAINT {
        let mut ps = PAINTSTRUCT::default();
        // SAFETY: standard paint sequence on our own window.
        unsafe {
            let hdc: HDC = BeginPaint(hwnd, &mut ps);
            let mut rc = RECT::default();
            let _ = GetClientRect(hwnd, &mut rc);
            PAINT.with(|p| {
                if let Some(paint) = p.borrow().as_ref() {
                    FillRect(hdc, &rc, paint.background);
                    SetBkMode(hdc, BACKGROUND_MODE(1)); // TRANSPARENT
                    SetTextColor(hdc, FG);
                    let old = SelectObject(hdc, HGDIOBJ(paint.title_font.0));
                    let title = wide(&paint.title);
                    let _ = TextOutW(hdc, 14, 6, &title);
                    SelectObject(hdc, HGDIOBJ(paint.detail_font.0));
                    SetTextColor(hdc, SIGNAL);
                    let detail = wide(&paint.detail);
                    let _ = TextOutW(hdc, 14, 27, &detail);
                    SelectObject(hdc, old);
                }
            });
            let _ = EndPaint(hwnd, &ps);
        }
        return LRESULT(0);
    }
    // SAFETY: default handling.
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

fn hide_choice_for(id: usize) -> Option<HideChoice> {
    Some(match id {
        ID_HIDE_30 => HideChoice::Minutes(30),
        ID_HIDE_60 => HideChoice::Minutes(60),
        ID_HIDE_120 => HideChoice::Minutes(120),
        ID_HIDE_240 => HideChoice::Minutes(240),
        ID_HIDE_480 => HideChoice::Minutes(480),
        ID_HIDE_END => HideChoice::UntilSessionEnd,
        _ => return None,
    })
}

fn append(
    menu: HMENU,
    flags: windows::Win32::UI::WindowsAndMessaging::MENU_ITEM_FLAGS,
    id: usize,
    text: &str,
) {
    let z = wide_z(text);
    // SAFETY: `z` is NUL-terminated and outlives the call.
    let _ = unsafe { AppendMenuW(menu, flags, id, PCWSTR(z.as_ptr())) };
}

fn choose_hide_length(owner: HWND) -> Option<HideChoice> {
    // SAFETY: a popup menu built, shown and destroyed on this thread.
    unsafe {
        let menu = CreatePopupMenu().ok()?;
        append(menu, MF_STRING, ID_HIDE_30, "Hide for 30 minutes");
        append(menu, MF_STRING, ID_HIDE_60, "Hide for 1 hour");
        append(menu, MF_STRING, ID_HIDE_120, "Hide for 2 hours");
        append(menu, MF_STRING, ID_HIDE_240, "Hide for 4 hours");
        append(menu, MF_STRING, ID_HIDE_480, "Hide for 8 hours");
        append(menu, MF_SEPARATOR, 0, "");
        append(menu, MF_STRING, ID_HIDE_END, "Hide until the session ends");
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let _ = SetForegroundWindow(owner);
        let picked = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON,
            pt.x,
            pt.y,
            None,
            owner,
            None,
        );
        let _ = DestroyMenu(menu);
        usize::try_from(picked.0).ok().and_then(hide_choice_for)
    }
}

unsafe extern "system" fn buttons_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_COMMAND {
        let id = wparam.0 & 0xffff;
        match id {
            ID_BTN_HIDE => {
                if let Some(choice) = choose_hide_length(hwnd) {
                    push_banner_action(BannerAction::Hide(choice));
                }
            }
            ID_BTN_DISCONNECT => push_banner_action(BannerAction::Disconnect),
            other => {
                // A menu choice (also how tests drive the menu).
                if let Some(choice) = hide_choice_for(other) {
                    push_banner_action(BannerAction::Hide(choice));
                }
            }
        }
        return LRESULT(0);
    }
    // SAFETY: default handling.
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

unsafe extern "system" fn main_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_WAKE => {
            // Take the state out while the commands run. Destroying banner windows
            // sends messages synchronously to this procedure, which borrows the
            // state again; holding the borrow across that call panicked ("already
            // mutably borrowed") inside a Windows callback, which aborts the host.
            // Any message that arrives meanwhile sees no state and does nothing.
            let taken = UI.with(|u| u.borrow_mut().take());
            if let Some(mut ui) = taken {
                ui.process_commands();
                UI.with(|u| *u.borrow_mut() = Some(ui));
            }
            LRESULT(0)
        }
        WM_TRAY => {
            let event = (lparam.0 & 0xffff) as u32;
            if matches!(event, WM_RBUTTONUP | WM_LBUTTONUP | WM_CONTEXTMENU) {
                // Copy what the menu needs and release the borrow first: the
                // menu runs its own message loop.
                let model = UI.with(|u| u.borrow().as_ref().and_then(|ui| ui.tray.clone()));
                if let Some(model) = model
                    && let Some(id) = show_tray_menu(hwnd, &model)
                {
                    handle_tray_command(id);
                }
            }
            LRESULT(0)
        }
        WM_COMMAND => {
            handle_tray_command(wparam.0 & 0xffff);
            LRESULT(0)
        }
        WM_DISPLAYCHANGE => {
            UI.with(|u| {
                if let Some(ui) = u.borrow_mut().as_mut() {
                    ui.rebuild_banner();
                }
            });
            LRESULT(0)
        }
        WM_CLOSE => {
            // SAFETY: destroying our own window.
            let _ = unsafe { DestroyWindow(hwnd) };
            LRESULT(0)
        }
        WM_DESTROY => {
            UI.with(|u| {
                if let Some(ui) = u.borrow_mut().as_mut() {
                    ui.teardown();
                }
            });
            // SAFETY: ends this thread's message loop.
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        _ => {
            let taskbar_created = UI.with(|u| u.borrow().as_ref().map(|ui| ui.taskbar_created));
            if Some(msg) == taskbar_created && msg != 0 {
                // Explorer restarted: the tray icon is gone, add it again.
                UI.with(|u| {
                    if let Some(ui) = u.borrow_mut().as_mut() {
                        ui.tray_added = false;
                        ui.apply_tray();
                    }
                });
                return LRESULT(0);
            }
            // SAFETY: default handling.
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
    }
}

fn handle_tray_command(id: usize) {
    match id {
        ID_TRAY_SHOW => push_tray_action(TrayAction::ShowIndicators),
        ID_TRAY_DISCONNECT => push_tray_action(TrayAction::DisconnectNow),
        _ => {}
    }
}

fn show_tray_menu(owner: HWND, model: &TrayModel) -> Option<usize> {
    // SAFETY: a popup menu built, shown and destroyed on this thread.
    unsafe {
        let menu = CreatePopupMenu().ok()?;
        let info = MF_STRING | MF_DISABLED | MF_GRAYED;
        match (&model.connected_name, model.session_live) {
            (Some(name), true) => append(menu, info, 0, &format!("Connected: {name}")),
            _ => append(menu, info, 0, "No one is connected"),
        }
        if let Some(secs) = model.consent_seconds_left {
            let minutes = secs.div_ceil(60);
            append(menu, info, 0, &format!("Consent: {minutes} min left"));
        }
        append(menu, MF_SEPARATOR, 0, "");
        append(menu, MF_STRING, ID_TRAY_SHOW, "Show indicators");
        let disconnect_flags = if model.session_live {
            MF_STRING
        } else {
            MF_STRING | MF_DISABLED | MF_GRAYED
        };
        append(menu, disconnect_flags, ID_TRAY_DISCONNECT, "Disconnect now");
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let _ = SetForegroundWindow(owner);
        let picked = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON,
            pt.x,
            pt.y,
            None,
            owner,
            None,
        );
        let _ = DestroyMenu(menu);
        usize::try_from(picked.0).ok().filter(|id| *id != 0)
    }
}

// ---- icons ----------------------------------------------------------------------

/// A 16x16 filled circle.
fn make_icon(color: (u8, u8, u8)) -> HICON {
    const N: i32 = 16;
    let mut pixels = vec![0u8; (N * N * 4) as usize];
    for y in 0..N {
        for x in 0..N {
            let dx = f64::from(x) - 7.5;
            let dy = f64::from(y) - 7.5;
            if dx * dx + dy * dy <= 7.0 * 7.0 {
                let i = ((y * N + x) * 4) as usize;
                pixels[i] = color.2; // B
                pixels[i + 1] = color.1; // G
                pixels[i + 2] = color.0; // R
                pixels[i + 3] = 255;
            }
        }
    }
    let mask = [0u8; 32]; // 16 rows x 2 bytes, all zero: the alpha channel decides
    // SAFETY: both buffers outlive the calls and match the stated bitmap sizes.
    unsafe {
        let color_bitmap = CreateBitmap(N, N, 1, 32, Some(pixels.as_ptr().cast()));
        let mask_bitmap = CreateBitmap(N, N, 1, 1, Some(mask.as_ptr().cast()));
        let info = ICONINFO {
            fIcon: BOOL(1),
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: mask_bitmap,
            hbmColor: color_bitmap,
        };
        let icon = CreateIconIndirect(&info).unwrap_or_default();
        let _ = DeleteObject(HGDIOBJ(color_bitmap.0));
        let _ = DeleteObject(HGDIOBJ(mask_bitmap.0));
        icon
    }
}

// ---- UI state ---------------------------------------------------------------------

unsafe extern "system" fn collect_monitor(
    _monitor: HMONITOR,
    _dc: HDC,
    rect: *mut RECT,
    data: LPARAM,
) -> BOOL {
    // SAFETY: `data` is the Vec<RECT> passed to EnumDisplayMonitors; `rect` is valid.
    unsafe {
        let list = &mut *(data.0 as *mut Vec<RECT>);
        list.push(*rect);
    }
    BOOL(1)
}

fn monitors() -> Vec<RECT> {
    let mut list: Vec<RECT> = Vec::new();
    // SAFETY: the callback only pushes into `list`, which outlives the call.
    unsafe {
        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(collect_monitor),
            LPARAM(&mut list as *mut _ as isize),
        );
    }
    list
}

impl Ui {
    fn process_commands(&mut self) {
        loop {
            let Some(command) = self.shared.commands.lock().unwrap().pop_front() else {
                break;
            };
            match command {
                Command::ShowBanner(content) => {
                    self.banner = Some(content);
                    self.rebuild_banner();
                }
                Command::HideBanner => {
                    self.banner = None;
                    self.destroy_banner();
                }
                Command::SetTray(model) => {
                    self.tray = Some(model);
                    self.apply_tray();
                }
            }
        }
    }

    fn destroy_banner(&mut self) {
        for (text, buttons) in self.bars.drain(..) {
            // SAFETY: windows we created on this thread.
            unsafe {
                let _ = DestroyWindow(buttons);
                let _ = DestroyWindow(text);
            }
        }
        self.shared.bars.lock().unwrap().clear();
        self.shared.banner_visible.store(false, Ordering::SeqCst);
    }

    /// (Re)create the banner on every display.
    fn rebuild_banner(&mut self) {
        self.destroy_banner();
        let Some(content) = self.banner.clone() else {
            return;
        };
        PAINT.with(|p| {
            if let Some(paint) = p.borrow_mut().as_mut() {
                paint.title = banner_title(&content);
                paint.detail = permission_words(&content.granted);
            }
        });
        let total = TEXT_W + BUTTONS_W;
        for rect in monitors() {
            let width = rect.right - rect.left;
            let x = rect.left + (width - total).max(0) / 2;
            let y = rect.top + BAR_TOP;
            if let Some(bar) = self.create_bar(x, y) {
                self.bars.push(bar);
            }
        }
        *self.shared.bars.lock().unwrap() = self
            .bars
            .iter()
            .map(|(t, b)| (t.0 as isize, b.0 as isize))
            .collect();
        self.shared
            .banner_visible
            .store(!self.bars.is_empty(), Ordering::SeqCst);
    }

    fn create_bar(&self, x: i32, y: i32) -> Option<(HWND, HWND)> {
        let topmost = WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE;
        // SAFETY: creating, configuring and showing our own windows on this thread.
        unsafe {
            // Text: click-through, slightly translucent, hidden from capture.
            let text = CreateWindowExW(
                topmost | WS_EX_LAYERED | WS_EX_TRANSPARENT,
                w!("RemoteBridgeBannerText"),
                w!("RemoteBridge banner"),
                WS_POPUP,
                x,
                y,
                TEXT_W,
                BAR_H,
                None,
                None,
                Some(self.instance),
                None,
            )
            .ok()?;
            let _ = SetLayeredWindowAttributes(text, COLORREF(0), 235, LWA_ALPHA);
            let _ = SetWindowDisplayAffinity(text, WINDOW_DISPLAY_AFFINITY(WDA_EXCLUDEFROMCAPTURE));

            // Buttons: the only part that takes clicks.
            let buttons = CreateWindowExW(
                topmost,
                w!("RemoteBridgeBannerButtons"),
                w!("RemoteBridge banner buttons"),
                WS_POPUP,
                x + TEXT_W,
                y,
                BUTTONS_W,
                BAR_H,
                None,
                None,
                Some(self.instance),
                None,
            )
            .ok()?;
            let _ =
                SetWindowDisplayAffinity(buttons, WINDOW_DISPLAY_AFFINITY(WDA_EXCLUDEFROMCAPTURE));
            let gui_font = GetStockObject(DEFAULT_GUI_FONT);
            for (label, id, left, width) in [
                (w!("Hide…"), ID_BTN_HIDE, 8, 92),
                (w!("Disconnect"), ID_BTN_DISCONNECT, 106, 100),
            ] {
                if let Ok(button) = CreateWindowExW(
                    WINDOW_EX_STYLE(0),
                    w!("BUTTON"),
                    label,
                    WS_CHILD | WS_VISIBLE,
                    left,
                    8,
                    width,
                    32,
                    Some(buttons),
                    Some(HMENU(id as *mut _)),
                    Some(self.instance),
                    None,
                ) {
                    SendMessageW(
                        button,
                        WM_SETFONT,
                        Some(WPARAM(gui_font.0 as usize)),
                        Some(LPARAM(1)),
                    );
                }
            }
            let _ = ShowWindow(text, SW_SHOWNOACTIVATE);
            let _ = ShowWindow(buttons, SW_SHOWNOACTIVATE);
            let _ = InvalidateRect(Some(text), None, true);
            Some((text, buttons))
        }
    }

    fn icon(&self) -> HICON {
        match &self.tray {
            Some(m) if m.session_live => self.icon_live,
            _ => self.icon_idle,
        }
    }

    fn tray_data(&self) -> NOTIFYICONDATAW {
        let tip = match &self.tray {
            Some(TrayModel {
                session_live: true,
                connected_name: Some(name),
                ..
            }) => format!("RemoteBridge - {name} is connected"),
            _ => "RemoteBridge".to_owned(),
        };
        let mut data = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.main,
            uID: 1,
            uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
            uCallbackMessage: WM_TRAY,
            hIcon: self.icon(),
            ..Default::default()
        };
        for (slot, unit) in data.szTip.iter_mut().zip(tip.encode_utf16().take(127)) {
            *slot = unit;
        }
        data
    }

    fn apply_tray(&mut self) {
        let visible = self.tray.as_ref().is_some_and(|m| m.visible);
        let data = self.tray_data();
        // SAFETY: `data` is a fully initialised NOTIFYICONDATAW.
        unsafe {
            if visible && !self.tray_added {
                self.tray_added = Shell_NotifyIconW(NIM_ADD, &data).as_bool();
            } else if visible {
                let _ = Shell_NotifyIconW(NIM_MODIFY, &data);
            } else if self.tray_added {
                let _ = Shell_NotifyIconW(NIM_DELETE, &data);
                self.tray_added = false;
            }
        }
        self.shared
            .tray_icon_added
            .store(self.tray_added, Ordering::SeqCst);
    }

    fn teardown(&mut self) {
        self.banner = None;
        self.destroy_banner();
        if self.tray_added {
            let data = self.tray_data();
            // SAFETY: removing the icon we added.
            let _ = unsafe { Shell_NotifyIconW(NIM_DELETE, &data) };
            self.tray_added = false;
            self.shared.tray_icon_added.store(false, Ordering::SeqCst);
        }
        // SAFETY: icons we created.
        unsafe {
            let _ = DestroyIcon(self.icon_idle);
            let _ = DestroyIcon(self.icon_live);
        }
    }
}

// ---- the thread ------------------------------------------------------------------------

fn register(
    class: PCWSTR,
    proc: windows::Win32::UI::WindowsAndMessaging::WNDPROC,
    instance: HINSTANCE,
    background: HBRUSH,
) {
    let def = WNDCLASSW {
        lpfnWndProc: proc,
        hInstance: instance,
        lpszClassName: class,
        hbrBackground: background,
        ..Default::default()
    };
    // SAFETY: a fully initialised class definition; 0 can mean "already registered".
    let _ = unsafe { RegisterClassW(&def) };
}

fn run(shared: Arc<Shared>, ready: mpsc::Sender<Result<(), PlatformError>>) {
    // SAFETY: Win32 setup on this thread; every handle is released in `teardown`/below.
    unsafe {
        let Ok(module) = GetModuleHandleW(None) else {
            let _ = ready.send(Err(backend("GetModuleHandleW failed")));
            return;
        };
        let instance = HINSTANCE(module.0);
        let background = CreateSolidBrush(BG);
        let make_font = |height: i32, weight: i32| {
            CreateFontW(
                height,
                0,
                0,
                0,
                weight,
                0,
                0,
                0,
                windows::Win32::Graphics::Gdi::DEFAULT_CHARSET,
                windows::Win32::Graphics::Gdi::OUT_DEFAULT_PRECIS,
                windows::Win32::Graphics::Gdi::CLIP_DEFAULT_PRECIS,
                windows::Win32::Graphics::Gdi::CLEARTYPE_QUALITY,
                0,
                w!("Segoe UI"),
            )
        };
        PAINT.with(|p| {
            *p.borrow_mut() = Some(PaintData {
                title: String::new(),
                detail: String::new(),
                title_font: make_font(-17, 700),
                detail_font: make_font(-14, 400),
                background,
            });
        });
        SHARED.with(|s| *s.borrow_mut() = Some(shared.clone()));

        register(
            w!("RemoteBridgeBannerText"),
            Some(text_proc),
            instance,
            background,
        );
        register(
            w!("RemoteBridgeBannerButtons"),
            Some(buttons_proc),
            instance,
            background,
        );
        register(
            w!("RemoteBridgeUiMain"),
            Some(main_proc),
            instance,
            HBRUSH::default(),
        );

        let main = match CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("RemoteBridgeUiMain"),
            w!("RemoteBridge"),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            None,
            None,
            Some(instance),
            None,
        ) {
            Ok(h) => h,
            Err(_) => {
                let _ = ready.send(Err(backend("could not create the UI window")));
                return;
            }
        };
        shared.main_hwnd.store(main.0 as isize, Ordering::SeqCst);

        UI.with(|u| {
            *u.borrow_mut() = Some(Ui {
                shared: shared.clone(),
                instance,
                main,
                bars: Vec::new(),
                banner: None,
                tray: None,
                tray_added: false,
                icon_idle: make_icon((0x2e, 0x7d, 0x7a)),
                icon_live: make_icon((0xe0, 0x9f, 0x1f)),
                taskbar_created: RegisterWindowMessageW(w!("TaskbarCreated")),
            });
        });
        let _ = ready.send(Ok(()));

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        PAINT.with(|p| {
            if let Some(paint) = p.borrow_mut().take() {
                let _ = DeleteObject(HGDIOBJ(paint.title_font.0));
                let _ = DeleteObject(HGDIOBJ(paint.detail_font.0));
            }
        });
        let _ = DeleteObject(HGDIOBJ(background.0));
        for class in [
            w!("RemoteBridgeBannerText"),
            w!("RemoteBridgeBannerButtons"),
            w!("RemoteBridgeUiMain"),
        ] {
            let _ = UnregisterClassW(class, Some(instance));
        }
        UI.with(|u| *u.borrow_mut() = None);
    }
}

/// Owns the UI thread. Hand out the banner and tray with [`WindowsUi::banner`]
/// and [`WindowsUi::tray`]; dropping it removes every window and the icon.
pub struct WindowsUi {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl WindowsUi {
    pub fn start() -> PlatformResult<Self> {
        let shared = Arc::new(Shared::default());
        let (tx, rx) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("rb-ui".into())
            .spawn({
                let shared = shared.clone();
                move || run(shared, tx)
            })
            .map_err(|e| backend(e.to_string()))?;
        match rx.recv() {
            Ok(Ok(())) => Ok(Self {
                shared,
                thread: Some(thread),
            }),
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(e)
            }
            Err(_) => {
                let _ = thread.join();
                Err(backend("UI thread ended unexpectedly"))
            }
        }
    }

    pub fn banner(&self) -> WindowsBanner {
        WindowsBanner {
            shared: self.shared.clone(),
            shown: None,
        }
    }

    pub fn tray(&self) -> WindowsTray {
        WindowsTray {
            shared: self.shared.clone(),
        }
    }
}

impl Drop for WindowsUi {
    fn drop(&mut self) {
        // SAFETY: asks the UI window to close itself, which ends the thread.
        let _ = unsafe { PostMessageW(Some(self.shared.main()), WM_CLOSE, WPARAM(0), LPARAM(0)) };
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

pub struct WindowsBanner {
    shared: Arc<Shared>,
    /// What is on screen now. The host calls `show` on every tick, so only a
    /// change (or a banner that vanished) rebuilds the windows; rebuilding on
    /// every tick made the banner flicker and its buttons disappear under the
    /// cursor.
    shown: Option<BannerContent>,
}

impl Banner for WindowsBanner {
    fn show(&mut self, content: &BannerContent) -> PlatformResult<()> {
        let visible = self.shared.banner_visible.load(Ordering::SeqCst);
        if visible && self.shown.as_ref() == Some(content) {
            return Ok(());
        }
        self.shared.send(Command::ShowBanner(content.clone()));
        if self.shared.banner_visible.load(Ordering::SeqCst) {
            self.shown = Some(content.clone());
            Ok(())
        } else {
            self.shown = None;
            Err(backend("no banner window could be created"))
        }
    }

    fn hide(&mut self) {
        // Only a banner we showed needs hiding; the host calls this every tick
        // while no session is shown.
        if self.shown.take().is_some() {
            self.shared.send(Command::HideBanner);
        }
    }

    fn is_visible(&self) -> bool {
        self.shared.banner_visible.load(Ordering::SeqCst)
    }

    fn poll_action(&mut self) -> Option<BannerAction> {
        self.shared.banner_actions.lock().unwrap().pop_front()
    }
}

pub struct WindowsTray {
    shared: Arc<Shared>,
}

impl Tray for WindowsTray {
    fn update(&mut self, model: &TrayModel) -> PlatformResult<()> {
        self.shared.send(Command::SetTray(model.clone()));
        Ok(())
    }

    fn poll_action(&mut self) -> Option<TrayAction> {
        self.shared.tray_actions.lock().unwrap().pop_front()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rb_core::Permissions;
    use std::time::Duration;
    use windows::Win32::UI::WindowsAndMessaging::{
        GWL_EXSTYLE, GetWindowDisplayAffinity, GetWindowLongPtrW, IsWindowVisible,
    };

    fn content() -> BannerContent {
        BannerContent {
            requester_name: "Sam Helper".into(),
            granted: Permissions {
                control: true,
                clipboard: true,
                ..Permissions::NONE
            },
        }
    }

    #[test]
    fn title_names_the_person() {
        assert_eq!(banner_title(&content()), "Sam Helper can see your screen");
    }

    #[test]
    fn menu_ids_map_to_the_allowed_durations() {
        assert_eq!(hide_choice_for(ID_HIDE_30), Some(HideChoice::Minutes(30)));
        assert_eq!(hide_choice_for(ID_HIDE_480), Some(HideChoice::Minutes(480)));
        assert_eq!(
            hide_choice_for(ID_HIDE_END),
            Some(HideChoice::UntilSessionEnd)
        );
        assert_eq!(hide_choice_for(0), None);
        assert_eq!(hide_choice_for(ID_BTN_HIDE), None);
    }

    fn settle() {
        std::thread::sleep(Duration::from_millis(150));
    }

    /// Opens real windows and a tray icon on the desktop for about a second,
    /// so it only runs on request:
    /// The host calls `show` on every tick. Repeating the same banner must not
    /// rebuild the windows (that flickered and hid the buttons under the cursor).
    /// `cargo test -p rb-platform-windows banner_is_not_rebuilt -- --ignored --nocapture --test-threads=1`
    #[test]
    #[ignore = "opens real windows on the desktop"]
    fn banner_is_not_rebuilt_on_every_tick() {
        let ui = WindowsUi::start().expect("UI thread");
        let mut banner = ui.banner();
        banner.show(&content()).unwrap();
        settle();
        let first = ui.shared.bars.lock().unwrap().clone();
        assert!(!first.is_empty());

        for _ in 0..20 {
            banner.show(&content()).unwrap();
        }
        settle();
        assert_eq!(
            ui.shared.bars.lock().unwrap().clone(),
            first,
            "same window handles after repeated shows"
        );

        banner.hide();
        settle();
        assert!(!banner.is_visible());
    }

    /// `cargo test -p rb-platform-windows windows_ui -- --ignored --nocapture --test-threads=1`
    #[test]
    #[ignore = "opens real windows on the desktop"]
    fn windows_ui_banner_and_tray_behave() {
        let ui = WindowsUi::start().expect("UI thread");
        let mut banner = ui.banner();
        let mut tray = ui.tray();

        // ---- banner
        assert!(!banner.is_visible());
        banner.show(&content()).unwrap();
        assert!(banner.is_visible());
        settle();
        let bars = ui.shared.bars.lock().unwrap().clone();
        let display_count = monitors().len();
        eprintln!("displays: {display_count}, banner bars: {}", bars.len());
        assert_eq!(bars.len(), display_count, "one banner on every display");
        for (text, buttons) in &bars {
            let text = HWND(*text as *mut _);
            let buttons = HWND(*buttons as *mut _);
            // SAFETY: querying windows that exist while `ui` lives.
            unsafe {
                assert!(IsWindowVisible(text).as_bool() && IsWindowVisible(buttons).as_bool());
                let ex_text = GetWindowLongPtrW(text, GWL_EXSTYLE) as u32;
                let ex_buttons = GetWindowLongPtrW(buttons, GWL_EXSTYLE) as u32;
                assert!(ex_text & WS_EX_TOPMOST.0 != 0, "always on top");
                assert!(ex_text & WS_EX_TRANSPARENT.0 != 0, "text is click-through");
                assert!(ex_buttons & WS_EX_TOPMOST.0 != 0);
                assert!(ex_buttons & WS_EX_TRANSPARENT.0 == 0, "buttons take clicks");
                assert!(ex_text & WS_EX_NOACTIVATE.0 != 0, "never steals focus");
                for (name, hwnd) in [("text", text), ("buttons", buttons)] {
                    let mut affinity = 0u32;
                    GetWindowDisplayAffinity(hwnd, &mut affinity).unwrap();
                    assert_eq!(
                        affinity, WDA_EXCLUDEFROMCAPTURE,
                        "{name} excluded from capture"
                    );
                }
            }
        }

        // Pressing the buttons (as the real buttons do) produces actions.
        let (_, buttons) = bars[0];
        let buttons = HWND(buttons as *mut _);
        // SAFETY: sending WM_COMMAND to our own window, as a button click would.
        unsafe {
            SendMessageW(
                buttons,
                WM_COMMAND,
                Some(WPARAM(ID_HIDE_60)),
                Some(LPARAM(0)),
            );
            SendMessageW(
                buttons,
                WM_COMMAND,
                Some(WPARAM(ID_HIDE_END)),
                Some(LPARAM(0)),
            );
            SendMessageW(
                buttons,
                WM_COMMAND,
                Some(WPARAM(ID_BTN_DISCONNECT)),
                Some(LPARAM(0)),
            );
        }
        assert_eq!(
            banner.poll_action(),
            Some(BannerAction::Hide(HideChoice::Minutes(60)))
        );
        assert_eq!(
            banner.poll_action(),
            Some(BannerAction::Hide(HideChoice::UntilSessionEnd))
        );
        assert_eq!(banner.poll_action(), Some(BannerAction::Disconnect));
        assert_eq!(banner.poll_action(), None);

        banner.hide();
        assert!(!banner.is_visible());
        assert!(ui.shared.bars.lock().unwrap().is_empty());
        // Showing again after hiding works.
        banner.show(&content()).unwrap();
        assert!(banner.is_visible());
        banner.hide();

        // ---- tray
        assert!(!ui.shared.tray_icon_added.load(Ordering::SeqCst));
        tray.update(&TrayModel {
            visible: true,
            session_live: true,
            connected_name: Some("Sam Helper".into()),
            consent_seconds_left: Some(1500),
        })
        .unwrap();
        settle();
        assert!(
            ui.shared.tray_icon_added.load(Ordering::SeqCst),
            "tray icon added"
        );
        // Menu choices arrive as WM_COMMAND on the UI window.
        // SAFETY: as above.
        unsafe {
            SendMessageW(
                ui.shared.main(),
                WM_COMMAND,
                Some(WPARAM(ID_TRAY_SHOW)),
                Some(LPARAM(0)),
            );
            SendMessageW(
                ui.shared.main(),
                WM_COMMAND,
                Some(WPARAM(ID_TRAY_DISCONNECT)),
                Some(LPARAM(0)),
            );
        }
        assert_eq!(tray.poll_action(), Some(TrayAction::ShowIndicators));
        assert_eq!(tray.poll_action(), Some(TrayAction::DisconnectNow));
        assert_eq!(tray.poll_action(), None);

        // An attended "hide everything" removes the tray icon; restore brings it back.
        tray.update(&TrayModel {
            visible: false,
            session_live: true,
            connected_name: Some("Sam Helper".into()),
            consent_seconds_left: Some(1500),
        })
        .unwrap();
        assert!(
            !ui.shared.tray_icon_added.load(Ordering::SeqCst),
            "tray icon removed"
        );
        tray.update(&TrayModel {
            visible: true,
            ..TrayModel::default()
        })
        .unwrap();
        assert!(ui.shared.tray_icon_added.load(Ordering::SeqCst));

        drop(ui);
    }
}

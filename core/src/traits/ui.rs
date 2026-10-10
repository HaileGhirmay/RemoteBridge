use crate::Permissions;
use crate::PlatformResult;

/// Handle for a prompt on screen, so it can be withdrawn if the server ends
/// the request while the prompt is still up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PromptId(pub u64);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Notice {
    /// "This device was removed from your account".
    DeviceRemoved,
    /// Shown the first time the user ever hides the indicators.
    FirstHide { restore_shortcut: String },
    /// A shortcut could not be registered; the tray items are the fallback.
    ShortcutCollision { shortcut: String },
    /// Screen Recording / Accessibility / Microphone missing; points to settings.
    PermissionsUnavailable { what: String },
    /// A fresh support code for the person at this machine to read out.
    SupportCode { code: String, minutes_left: u64 },
    /// `host/invite` failed; the reason is for the local user to see.
    SupportCodeUnavailable { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConsentPrompt {
    /// Names the requester. Every permission is its own checkbox, all off
    /// except view.
    AttendedRequest {
        requester_name: String,
        requester_email: String,
        requester_verified: bool,
        requested: Permissions,
    },
    /// "Keep sharing with <name>?"
    RenewConsent {
        requester_name: String,
    },
    /// The viewer asked for more access.
    PermissionEscalation {
        requester_name: String,
        added: Permissions,
    },
    Notice(Notice),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConsentAnswer {
    /// Attended request approved with a (possibly reduced) grant and window.
    Approve {
        grant: Permissions,
        approved_minutes: u32,
    },
    Deny,
    /// Renewal accepted for this many minutes.
    Renew {
        minutes: u32,
    },
    /// Escalation allowed exactly as requested.
    Allow,
    /// Notice acknowledged, or prompt closed without a choice. For approvals
    /// and renewals this counts as no.
    Dismissed,
}

/// Local consent prompts. Non-blocking: `show` returns at once and answers
/// arrive through `poll`. Only the local user can answer; remote input never
/// reaches these dialogs.
pub trait ConsentUi: Send {
    fn show(&mut self, prompt: ConsentPrompt) -> PlatformResult<PromptId>;
    fn withdraw(&mut self, id: PromptId);
    fn poll(&mut self) -> Option<(PromptId, ConsentAnswer)>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BannerContent {
    pub requester_name: String,
    pub granted: Permissions,
}

/// What the user picked from the banner's "Hide…" menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HideChoice {
    /// 30 / 60 / 120 / 240 / 480 minutes.
    Minutes(u32),
    UntilSessionEnd,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BannerAction {
    /// The user chose how long to hide the indicators from "Hide…".
    Hide(HideChoice),
    Disconnect,
}

/// The always-on-top "<name> can see your screen" banner, on every display.
/// Excluded from our own capture only, never from OS indicators.
pub trait Banner: Send {
    fn show(&mut self, content: &BannerContent) -> PlatformResult<()>;
    fn hide(&mut self);
    fn is_visible(&self) -> bool;
    fn poll_action(&mut self) -> Option<BannerAction>;
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TrayModel {
    pub visible: bool,
    pub session_live: bool,
    pub connected_name: Option<String>,
    pub consent_seconds_left: Option<u64>,
    /// The local half of unattended access is switched on (the owner's
    /// website toggle is the other half).
    pub unattended_opt_in: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayAction {
    /// Menu item "Show indicators" (fallback for the shortcut).
    ShowIndicators,
    /// Menu item "Disconnect now" (fallback for the shortcut).
    DisconnectNow,
    /// Menu item "Share this computer…": get a support code to read out.
    ShareThisComputer,
    /// Menu item "Allow unattended access on this PC", after the local person
    /// confirmed. `true` switches it on, `false` off.
    SetUnattendedOptIn(bool),
}

/// Tray icon (Windows) or menu-bar item (macOS).
pub trait Tray: Send {
    fn update(&mut self, model: &TrayModel) -> PlatformResult<()>;
    fn poll_action(&mut self) -> Option<TrayAction>;
}

//! Host rules on top of the pure timer.
//!
//! * **Attended** sessions: both the timed hide and "hide until the session
//!   ends" use scope `All` (banner and tray).
//! * **Unattended** sessions: hiding is allowed for the banner only. The tray
//!   icon is always visible.
//! * Only local input can hide or restore. Every method that changes
//!   visibility here is called by local UI code (banner button, tray menu,
//!   physical hotkey). There is deliberately no path from a viewer message to
//!   this module.
//! * Every dismissal and restore becomes an [`IndicatorEvent`] for
//!   `host/banner_report`: metadata only.

use std::sync::{Arc, Mutex};

use crate::Permissions;
use crate::protocol::types::SessionMode;
use crate::protocol::types::{
    BannerEvent, BannerEventType, BannerReason, BannerReportRequest, BannerScope,
};
use crate::session::SessionSnapshot;
use crate::traits::{
    Banner, BannerAction, BannerContent, Clock, HideChoice, HotkeyEvent, Tray, TrayAction,
    TrayModel,
};

use super::timer::{
    self, Context, DismissError, Dismissal, Evaluation, RestoreReason, Scope, persist, restore,
};

/// Where the dismissal and the "first time" flag survive restarts.
pub trait IndicatorStore: Send {
    fn load_dismissal(&self) -> Option<String>;
    fn save_dismissal(&mut self, raw: Option<&str>);
    /// Has the user already seen the one-time "banner and tray will be hidden" notice?
    fn first_hide_notice_seen(&self) -> bool;
    fn mark_first_hide_notice_seen(&mut self);
}

#[derive(Debug, Default)]
struct MemoryState {
    dismissal: Option<String>,
    notice_seen: bool,
}

/// In-memory store for tests, and for hosts that keep nothing on disk. Clones
/// share their contents, so a test can keep one and give another to a
/// controller, then build a second controller from the same store to model a
/// restart.
#[derive(Debug, Clone, Default)]
pub struct MemoryIndicatorStore {
    state: Arc<Mutex<MemoryState>>,
}

impl MemoryIndicatorStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// What is currently stored.
    pub fn stored(&self) -> Option<String> {
        self.state.lock().unwrap().dismissal.clone()
    }
}

impl IndicatorStore for MemoryIndicatorStore {
    fn load_dismissal(&self) -> Option<String> {
        self.stored()
    }

    fn save_dismissal(&mut self, raw: Option<&str>) {
        self.state.lock().unwrap().dismissal = raw.map(str::to_owned);
    }

    fn first_hide_notice_seen(&self) -> bool {
        self.state.lock().unwrap().notice_seen
    }

    fn mark_first_hide_notice_seen(&mut self) {
        self.state.lock().unwrap().notice_seen = true;
    }
}

/// Something for `host/banner_report`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IndicatorEvent {
    Dismissed {
        /// The session the report names (a real session id).
        session_id: String,
        duration_minutes: u32,
        scope: Scope,
    },
    Restored {
        session_id: Option<String>,
        scope: Scope,
        reason: ReportReason,
    },
}

/// A restore reason as reported to the server: the timer's reasons plus the
/// user asking to see the indicators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportReason {
    UserShowNow,
    Timer(RestoreReason),
}

impl ReportReason {
    fn to_protocol(self) -> Option<BannerReason> {
        Some(match self {
            Self::UserShowNow => BannerReason::UserShowNow,
            Self::Timer(RestoreReason::NoDismissal) => return None,
            Self::Timer(RestoreReason::NewSession) => BannerReason::NewSession,
            Self::Timer(RestoreReason::ParticipantChanged) => BannerReason::ParticipantChanged,
            Self::Timer(RestoreReason::PermissionIncrease) => BannerReason::PermissionIncrease,
            Self::Timer(RestoreReason::ConsentRenewal) => BannerReason::ConsentRenewal,
            Self::Timer(RestoreReason::CrashRecovery) => BannerReason::CrashRecovery,
            Self::Timer(RestoreReason::ClockAmbiguity) => BannerReason::ClockAmbiguity,
            Self::Timer(RestoreReason::Timeout) => BannerReason::Timeout,
            Self::Timer(RestoreReason::ResumeAfterExpiry) => BannerReason::ResumeAfterExpiry,
            Self::Timer(RestoreReason::SessionEnd) => BannerReason::SessionEnd,
        })
    }
}

impl IndicatorEvent {
    /// The protocol form, or `None` for an event that must not be reported.
    pub fn to_protocol(&self) -> Option<BannerEvent> {
        let scope = |s: Scope| match s {
            Scope::Banner => BannerScope::Banner,
            Scope::All => BannerScope::All,
        };
        match self {
            Self::Dismissed {
                session_id,
                duration_minutes,
                scope: s,
            } => Some(BannerEvent {
                kind: BannerEventType::Dismissed,
                session_id: Some(session_id.clone()),
                duration_minutes: Some(*duration_minutes),
                scope: Some(scope(*s)),
                reason: None,
            }),
            Self::Restored {
                session_id,
                scope: s,
                reason,
            } => Some(BannerEvent {
                kind: BannerEventType::Restored,
                session_id: session_id.clone(),
                duration_minutes: None,
                scope: Some(scope(*s)),
                reason: Some(reason.to_protocol()?),
            }),
        }
    }
}

/// What the local user's action asks the host to do beyond indicators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndicatorIntent {
    None,
    /// Emergency disconnect (shortcut or tray "Disconnect now").
    EmergencyDisconnect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HideError {
    /// No live session to hide indicators for.
    NoSession,
    /// Not one of 30 / 60 / 120 / 240 / 480 minutes.
    UnsupportedDuration,
    /// The session is already past its maximum end.
    SessionEnded,
}

impl From<DismissError> for HideError {
    fn from(e: DismissError) -> Self {
        match e {
            DismissError::NoSession => Self::NoSession,
            DismissError::UnsupportedDuration => Self::UnsupportedDuration,
            DismissError::SessionAlreadyEnded => Self::SessionEnded,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HideOutcome {
    /// First time ever: show "The banner and tray icon will be hidden. The
    /// person connected can still see your screen. Press <restore shortcut>
    /// to show them again."
    pub show_first_time_notice: bool,
    pub scope: Scope,
}

/// The live sessions reduced to what the timer cares about.
#[derive(Debug, Clone)]
struct Live {
    /// Combined key: changes when any session starts or ends.
    session_key: String,
    /// A real session id for reports.
    primary_id: String,
    participant_key: String,
    perms_key: String,
    consent_key: String,
    /// Every live session is attended (an unattended one keeps the tray).
    all_attended: bool,
    /// Server-side maximum end of the soonest-ending session, in ms from now.
    max_remaining_ms: Option<i64>,
    names: String,
    granted: Permissions,
    consent_seconds_left: Option<u64>,
}

fn perms_key(p: &Permissions) -> String {
    let mut parts = Vec::new();
    if p.control {
        parts.push("control");
    }
    if p.system_audio {
        parts.push("systemAudio");
    }
    if p.microphone {
        parts.push("microphone");
    }
    if p.clipboard {
        parts.push("clipboard");
    }
    parts.join(",")
}

fn summarize(sessions: &[SessionSnapshot]) -> Option<Live> {
    let mut live: Vec<&SessionSnapshot> = sessions.iter().filter(|s| s.running).collect();
    if live.is_empty() {
        return None;
    }
    live.sort_by(|a, b| a.id.cmp(&b.id));
    let ids: Vec<&str> = live.iter().map(|s| s.id.as_str()).collect();
    let mut emails: Vec<&str> = live.iter().map(|s| s.requester_email.as_str()).collect();
    emails.sort_unstable();
    emails.dedup();
    let mut names: Vec<&str> = live.iter().map(|s| s.requester_name.as_str()).collect();
    names.dedup();
    let granted = live
        .iter()
        .fold(Permissions::NONE, |acc, s| acc.union(&s.granted));
    let consent_key = live
        .iter()
        .map(|s| {
            s.consent_expires_at
                .map_or_else(|| "-".to_owned(), |t| t.to_string())
        })
        .collect::<Vec<_>>()
        .join("|");
    Some(Live {
        session_key: ids.join("+"),
        primary_id: live[0].id.clone(),
        participant_key: emails.join(","),
        perms_key: perms_key(&granted),
        consent_key,
        all_attended: live.iter().all(|s| s.mode == SessionMode::Attended),
        max_remaining_ms: live
            .iter()
            .filter_map(|s| s.max_remaining)
            .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
            .min(),
        names: names.join(", "),
        granted,
        consent_seconds_left: live
            .iter()
            .filter_map(|s| s.consent_remaining)
            .map(|d| d.as_secs())
            .min(),
    })
}

pub struct IndicatorController {
    store: Box<dyn IndicatorStore>,
    clock: Arc<dyn Clock>,
    dismissal: Option<Dismissal>,
    /// A real session id for the stored dismissal, for reports.
    dismissal_primary: Option<String>,
    live: Option<Live>,
    last: Option<Evaluation>,
    events: Vec<IndicatorEvent>,
}

impl IndicatorController {
    /// Loads any stored dismissal. It comes back without a monotonic deadline,
    /// so the first evaluation shows the indicators (`crash_recovery`).
    pub fn new(store: Box<dyn IndicatorStore>, clock: Arc<dyn Clock>) -> Self {
        let dismissal = restore(store.load_dismissal().as_deref());
        Self {
            store,
            clock,
            dismissal,
            dismissal_primary: None,
            live: None,
            last: None,
            events: Vec::new(),
        }
    }

    fn context(&self) -> Context {
        let live = self.live.as_ref();
        Context {
            session_id: live.map(|l| l.session_key.clone()),
            participant_key: live.map(|l| l.participant_key.clone()).unwrap_or_default(),
            perms_key: live.map(|l| l.perms_key.clone()).unwrap_or_default(),
            consent_key: live.map(|l| l.consent_key.clone()).unwrap_or_default(),
            now_wall_ms: self.clock.wall_ms(),
            now_mono_ms: i64::try_from(self.clock.mono().as_millis()).unwrap_or(i64::MAX),
        }
    }

    /// Feed the current sessions. Re-evaluates the dismissal and returns the
    /// events to report (restores caused by timers or changes).
    pub fn update(&mut self, sessions: &[SessionSnapshot]) -> Vec<IndicatorEvent> {
        self.live = summarize(sessions);
        let ctx = self.context();
        let eval = timer::evaluate(self.dismissal.as_ref(), &ctx);
        self.last = Some(eval);
        let mut out = Vec::new();
        if let Some(d) = &self.dismissal
            && let Some(reason) = eval.reason
            && reason != RestoreReason::NoDismissal
        {
            out.push(IndicatorEvent::Restored {
                session_id: self.dismissal_primary.clone(),
                scope: d.scope,
                reason: ReportReason::Timer(reason),
            });
            self.clear_dismissal();
            self.last = Some(timer::evaluate(None, &ctx));
        }
        self.events.extend(out.iter().cloned());
        out
    }

    fn clear_dismissal(&mut self) {
        self.dismissal = None;
        self.dismissal_primary = None;
        self.store.save_dismissal(None);
    }

    /// Events not yet taken (for the caller that reports them).
    pub fn take_events(&mut self) -> Vec<IndicatorEvent> {
        std::mem::take(&mut self.events)
    }

    // ---- local actions -------------------------------------------------------

    /// Hide for 30 / 60 / 120 / 240 / 480 minutes. Attended: banner and tray.
    /// Unattended: banner only.
    pub fn hide_for(&mut self, minutes: u32) -> Result<HideOutcome, HideError> {
        let live = self.live.clone().ok_or(HideError::NoSession)?;
        let scope = Self::scope_for(&live);
        let d = timer::dismiss(&self.context(), minutes, scope)?;
        Ok(self.apply_dismissal(d, &live))
    }

    /// Hide until the session ends. Attended: banner and tray. Unattended:
    /// banner only.
    pub fn hide_until_end(&mut self) -> Result<HideOutcome, HideError> {
        let live = self.live.clone().ok_or(HideError::NoSession)?;
        let ctx = self.context();
        let remaining = live.max_remaining_ms.ok_or(HideError::SessionEnded)?;
        let mut d = timer::dismiss_all(&ctx, ctx.now_wall_ms + remaining)?;
        d.scope = Self::scope_for(&live);
        Ok(self.apply_dismissal(d, &live))
    }

    fn scope_for(live: &Live) -> Scope {
        if live.all_attended {
            Scope::All
        } else {
            Scope::Banner
        }
    }

    fn apply_dismissal(&mut self, d: Dismissal, live: &Live) -> HideOutcome {
        let outcome = HideOutcome {
            show_first_time_notice: !self.store.first_hide_notice_seen(),
            scope: d.scope,
        };
        self.store.mark_first_hide_notice_seen();
        self.store.save_dismissal(Some(&persist(&d)));
        self.events.push(IndicatorEvent::Dismissed {
            session_id: live.primary_id.clone(),
            duration_minutes: d.duration_minutes,
            scope: d.scope,
        });
        self.dismissal_primary = Some(live.primary_id.clone());
        self.dismissal = Some(d);
        let ctx = self.context();
        self.last = Some(timer::evaluate(self.dismissal.as_ref(), &ctx));
        outcome
    }

    /// The local user asked to see the indicators now (shortcut or tray menu).
    pub fn restore_now(&mut self) -> Vec<IndicatorEvent> {
        let Some(d) = &self.dismissal else {
            return Vec::new();
        };
        let event = IndicatorEvent::Restored {
            session_id: self.dismissal_primary.clone(),
            scope: d.scope,
            reason: ReportReason::UserShowNow,
        };
        self.clear_dismissal();
        let ctx = self.context();
        self.last = Some(timer::evaluate(None, &ctx));
        self.events.push(event.clone());
        vec![event]
    }

    /// A button on the banner. "Hide…" carries the length the local user chose.
    /// Returns what happened for the "hide" case so the caller can show the
    /// one-time notice, and the intent for "Disconnect".
    pub fn handle_banner_action(
        &mut self,
        action: BannerAction,
    ) -> Result<(Option<HideOutcome>, IndicatorIntent), HideError> {
        match action {
            BannerAction::Hide(HideChoice::Minutes(m)) => {
                Ok((Some(self.hide_for(m)?), IndicatorIntent::None))
            }
            BannerAction::Hide(HideChoice::UntilSessionEnd) => {
                Ok((Some(self.hide_until_end()?), IndicatorIntent::None))
            }
            BannerAction::Disconnect => Ok((None, IndicatorIntent::EmergencyDisconnect)),
        }
    }

    /// A physical-keyboard shortcut. Only the hotkey layer calls this, and it
    /// only ever delivers local, non-injected presses.
    pub fn handle_hotkey(&mut self, event: HotkeyEvent) -> IndicatorIntent {
        match event {
            HotkeyEvent::RestoreIndicators => {
                self.restore_now();
                IndicatorIntent::None
            }
            HotkeyEvent::EmergencyDisconnect => IndicatorIntent::EmergencyDisconnect,
        }
    }

    /// A tray menu choice: the fallback when a shortcut cannot be registered.
    pub fn handle_tray_action(&mut self, action: TrayAction) -> IndicatorIntent {
        match action {
            TrayAction::ShowIndicators => {
                self.restore_now();
                IndicatorIntent::None
            }
            TrayAction::DisconnectNow => IndicatorIntent::EmergencyDisconnect,
            // Sharing is the runtime's job; nothing changes for the indicators.
            TrayAction::ShareThisComputer => IndicatorIntent::None,
        }
    }

    // ---- what to show ------------------------------------------------------------

    /// Is a dismissal currently in force?
    pub fn is_hidden(&self) -> bool {
        self.live.is_some() && self.last.is_some_and(|e| e.hidden)
    }

    /// The banner is shown while a session is live and not hidden.
    pub fn banner_visible(&self) -> bool {
        self.live.is_some() && !self.is_hidden()
    }

    /// The tray icon is visible unless an attended session's `All` dismissal
    /// hides it. Unattended sessions always keep it.
    pub fn tray_visible(&self) -> bool {
        match (&self.live, self.last) {
            (Some(live), Some(e)) => !(e.tray_hidden && live.all_attended),
            _ => true,
        }
    }

    pub fn banner_content(&self) -> Option<BannerContent> {
        self.live.as_ref().map(|l| BannerContent {
            requester_name: l.names.clone(),
            granted: l.granted,
        })
    }

    pub fn tray_model(&self) -> TrayModel {
        match &self.live {
            Some(l) => TrayModel {
                visible: self.tray_visible(),
                session_live: true,
                connected_name: Some(l.names.clone()),
                consent_seconds_left: l.consent_seconds_left,
            },
            None => TrayModel {
                visible: true,
                session_live: false,
                connected_name: None,
                consent_seconds_left: None,
            },
        }
    }

    /// Update `sessions`, then push the result to the banner and tray.
    /// Returns the events to report.
    pub fn sync(
        &mut self,
        sessions: &[SessionSnapshot],
        banner: &mut dyn Banner,
        tray: &mut dyn Tray,
    ) -> Vec<IndicatorEvent> {
        let events = self.update(sessions);
        match (self.banner_visible(), self.banner_content()) {
            (true, Some(content)) => {
                let _ = banner.show(&content);
            }
            _ => banner.hide(),
        }
        let _ = tray.update(&self.tray_model());
        events
    }

    /// The `host/banner_report` body for an event (or for just the settings).
    pub fn report(
        event: Option<&IndicatorEvent>,
        default_hide_minutes: u32,
        restore_label: &str,
        disconnect_label: &str,
        shortcut_collision: bool,
    ) -> BannerReportRequest {
        BannerReportRequest {
            default_hide_minutes,
            show_hide_shortcut: restore_label.to_owned(),
            disconnect_shortcut: disconnect_label.to_owned(),
            shortcut_collision,
            event: event.and_then(IndicatorEvent::to_protocol),
        }
    }
}

#[cfg(test)]
mod tests;

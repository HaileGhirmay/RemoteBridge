use std::time::Duration;

use super::*;
use crate::fakes::{FakeBanner, FakeClock, FakeTray};
use crate::protocol::types::SessionMode::{Attended, Unattended};

const T0: i64 = 1_791_462_818_897;

const CONTROL: Permissions = Permissions {
    control: true,
    ..Permissions::NONE
};
const CONTROL_AUDIO: Permissions = Permissions {
    control: true,
    system_audio: true,
    ..Permissions::NONE
};

fn snap(id: &str, mode: SessionMode, granted: Permissions) -> SessionSnapshot {
    SessionSnapshot {
        id: id.into(),
        mode,
        requester_name: "Sam Helper".into(),
        requester_email: "sam@example.com".into(),
        state: "active".into(),
        granted,
        pending: None,
        consent_expires_at: Some(T0 + 30 * 60_000),
        consent_remaining: Some(Duration::from_secs(30 * 60)),
        max_remaining: Some(Duration::from_secs(3 * 3600)),
        running: true,
    }
}

struct Rig {
    c: IndicatorController,
    clock: FakeClock,
    store: MemoryIndicatorStore,
}

impl Rig {
    fn new() -> Self {
        let clock = FakeClock::new(T0);
        let store = MemoryIndicatorStore::new();
        let c = IndicatorController::new(Box::new(store.clone()), Arc::new(clock.clone()));
        Self { c, clock, store }
    }

    fn with_session(mode: SessionMode) -> (Self, Vec<SessionSnapshot>) {
        let mut r = Self::new();
        let sessions = vec![snap("s1", mode, CONTROL)];
        r.c.update(&sessions);
        (r, sessions)
    }

    fn pass(&self, secs: u64) {
        self.clock.advance(Duration::from_secs(secs));
    }
}

fn restored_reasons(events: &[IndicatorEvent]) -> Vec<ReportReason> {
    events
        .iter()
        .filter_map(|e| match e {
            IndicatorEvent::Restored { reason, .. } => Some(*reason),
            IndicatorEvent::Dismissed { .. } => None,
        })
        .collect()
}

// ---- attended -------------------------------------------------------------------

#[test]
fn indicators_are_shown_by_default() {
    let (r, _) = Rig::with_session(Attended);
    assert!(r.c.banner_visible());
    assert!(r.c.tray_visible());
    assert!(!r.c.is_hidden());
}

#[test]
fn an_attended_timed_hide_hides_banner_and_tray_then_restores() {
    let (mut r, sessions) = Rig::with_session(Attended);
    let outcome = r.c.hide_for(30).unwrap();
    assert_eq!(outcome.scope, Scope::All);
    assert!(!r.c.banner_visible());
    assert!(!r.c.tray_visible());

    r.pass(30 * 60 - 1);
    r.c.update(&sessions);
    assert!(!r.c.banner_visible() && !r.c.tray_visible());

    r.pass(1);
    let events = r.c.update(&sessions);
    assert!(r.c.banner_visible() && r.c.tray_visible());
    assert_eq!(
        restored_reasons(&events),
        vec![ReportReason::Timer(RestoreReason::Timeout)]
    );
    assert_eq!(r.store.stored(), None, "a restored dismissal is forgotten");
}

#[test]
fn every_allowed_duration_works_and_others_are_refused() {
    for minutes in [30, 60, 120, 240, 480] {
        let (mut r, _) = Rig::with_session(Attended);
        assert!(r.c.hide_for(minutes).is_ok(), "{minutes}");
    }
    let (mut r, _) = Rig::with_session(Attended);
    assert_eq!(r.c.hide_for(45), Err(HideError::UnsupportedDuration));
    assert!(r.c.banner_visible(), "a refused request hides nothing");
}

#[test]
fn hide_until_the_session_ends_hides_banner_and_tray() {
    let (mut r, sessions) = Rig::with_session(Attended);
    let outcome = r.c.hide_until_end().unwrap();
    assert_eq!(outcome.scope, Scope::All);
    r.pass(2 * 3600);
    r.c.update(&sessions);
    assert!(!r.c.banner_visible() && !r.c.tray_visible());

    r.pass(3600);
    r.c.update(&sessions);
    assert!(r.c.banner_visible() && r.c.tray_visible());
}

#[test]
fn hide_needs_a_live_session() {
    let mut r = Rig::new();
    assert_eq!(r.c.hide_for(30), Err(HideError::NoSession));
    assert_eq!(r.c.hide_until_end().map(|_| ()), Err(HideError::NoSession));

    // A session still waiting for approval is not live.
    let mut pending = snap("s1", Attended, Permissions::NONE);
    pending.running = false;
    r.c.update(&[pending]);
    assert_eq!(r.c.hide_for(30), Err(HideError::NoSession));
}

// ---- unattended -------------------------------------------------------------------

#[test]
fn an_unattended_hide_never_hides_the_tray() {
    let (mut r, sessions) = Rig::with_session(Unattended);
    let outcome = r.c.hide_for(60).unwrap();
    assert_eq!(outcome.scope, Scope::Banner);
    assert!(!r.c.banner_visible());
    assert!(
        r.c.tray_visible(),
        "unattended sessions always keep the tray"
    );

    let outcome = r.c.hide_until_end().unwrap();
    assert_eq!(outcome.scope, Scope::Banner);
    r.pass(3600);
    r.c.update(&sessions);
    assert!(!r.c.banner_visible());
    assert!(r.c.tray_visible());
    assert!(r.c.tray_model().visible);
}

#[test]
fn one_unattended_session_among_attended_ones_keeps_the_tray() {
    let mut r = Rig::new();
    let sessions = vec![
        snap("s1", Attended, CONTROL),
        snap("s2", Unattended, CONTROL),
    ];
    r.c.update(&sessions);
    let outcome = r.c.hide_for(30).unwrap();
    assert_eq!(outcome.scope, Scope::Banner);
    assert!(r.c.tray_visible());
}

// ---- every way back ---------------------------------------------------------------------

fn hidden_attended() -> (Rig, Vec<SessionSnapshot>) {
    let (mut r, sessions) = Rig::with_session(Attended);
    r.c.hide_for(240).unwrap();
    r.c.take_events();
    (r, sessions)
}

fn assert_back(r: &Rig, why: RestoreReason, events: &[IndicatorEvent]) {
    assert!(
        r.c.banner_visible() && r.c.tray_visible(),
        "{why:?}: indicators must be back"
    );
    assert_eq!(
        restored_reasons(events),
        vec![ReportReason::Timer(why)],
        "{why:?}"
    );
    assert_eq!(r.store.stored(), None);
}

#[test]
fn a_new_session_brings_the_indicators_back() {
    let (mut r, mut sessions) = hidden_attended();
    sessions.push(snap("s2", Attended, CONTROL));
    let events = r.c.update(&sessions);
    assert_back(&r, RestoreReason::NewSession, &events);
}

#[test]
fn a_session_ending_brings_them_back_even_if_another_continues() {
    let mut r = Rig::new();
    let both = vec![snap("s1", Attended, CONTROL), snap("s2", Attended, CONTROL)];
    r.c.update(&both);
    r.c.hide_for(60).unwrap();
    let events = r.c.update(&both[..1]);
    assert_back(&r, RestoreReason::NewSession, &events);
}

#[test]
fn a_different_participant_brings_them_back() {
    let (mut r, mut sessions) = hidden_attended();
    sessions[0].requester_email = "someone.else@example.com".into();
    let events = r.c.update(&sessions);
    assert_back(&r, RestoreReason::ParticipantChanged, &events);
}

#[test]
fn more_permissions_bring_them_back_but_fewer_do_not() {
    let mut r = Rig::new();
    let mut sessions = vec![snap("s1", Attended, CONTROL_AUDIO)];
    r.c.update(&sessions);
    r.c.hide_for(60).unwrap();

    sessions[0].granted = CONTROL;
    assert!(r.c.update(&sessions).is_empty());
    assert!(!r.c.banner_visible(), "a reduction keeps the dismissal");

    sessions[0].granted = Permissions {
        control: true,
        microphone: true,
        ..Permissions::NONE
    };
    let events = r.c.update(&sessions);
    assert_back(&r, RestoreReason::PermissionIncrease, &events);
}

#[test]
fn consent_renewal_brings_them_back() {
    let (mut r, mut sessions) = hidden_attended();
    sessions[0].consent_expires_at = Some(T0 + 90 * 60_000); // renewed
    let events = r.c.update(&sessions);
    assert_back(&r, RestoreReason::ConsentRenewal, &events);
}

#[test]
fn a_wall_clock_jump_brings_them_back() {
    let (mut r, sessions) = hidden_attended();
    r.pass(60);
    r.clock.jump_wall(-3_600_000);
    let events = r.c.update(&sessions);
    assert_back(&r, RestoreReason::ClockAmbiguity, &events);
}

#[test]
fn the_session_ending_brings_them_back_and_reports_it() {
    let (mut r, _) = hidden_attended();
    let events = r.c.update(&[]);
    assert_eq!(
        restored_reasons(&events),
        vec![ReportReason::Timer(RestoreReason::SessionEnd)]
    );
    assert!(r.c.tray_visible(), "idle tray is always visible");
    assert!(!r.c.banner_visible());
    assert_eq!(r.store.stored(), None);
}

#[test]
fn sleeping_past_the_deadline_brings_them_back() {
    let (mut r, sessions) = hidden_attended();
    // A 4 h hide; the machine slept 5 h. Both clocks moved, so it is an overshoot.
    r.pass(5 * 3600);
    let mut sessions = sessions;
    sessions[0].max_remaining = Some(Duration::from_secs(8 * 3600));
    let events = r.c.update(&sessions);
    assert_back(&r, RestoreReason::ResumeAfterExpiry, &events);
}

#[test]
fn the_restore_shortcut_brings_them_back_and_reports_user_show_now() {
    let (mut r, sessions) = hidden_attended();
    let intent = r.c.handle_hotkey(HotkeyEvent::RestoreIndicators);
    assert_eq!(intent, IndicatorIntent::None);
    r.c.update(&sessions);
    assert!(r.c.banner_visible() && r.c.tray_visible());
    let events = r.c.take_events();
    assert_eq!(restored_reasons(&events), vec![ReportReason::UserShowNow]);
    assert_eq!(r.store.stored(), None);
}

#[test]
fn the_hide_menu_on_the_banner_maps_to_the_right_dismissal() {
    for (choice, expect_minutes) in [
        (HideChoice::Minutes(30), 30),
        (HideChoice::Minutes(60), 60),
        (HideChoice::Minutes(120), 120),
        (HideChoice::Minutes(240), 240),
        (HideChoice::Minutes(480), 480),
        (HideChoice::UntilSessionEnd, 180),
    ] {
        let (mut r, _) = Rig::with_session(Attended);
        let (outcome, intent) =
            r.c.handle_banner_action(BannerAction::Hide(choice))
                .unwrap();
        assert_eq!(intent, IndicatorIntent::None);
        assert_eq!(outcome.unwrap().scope, Scope::All);
        assert!(!r.c.banner_visible() && !r.c.tray_visible());
        let events = r.c.take_events();
        assert!(matches!(
            events.last(),
            Some(IndicatorEvent::Dismissed { duration_minutes, .. }) if *duration_minutes == expect_minutes
        ));
    }

    // An unsupported length from a buggy UI is refused, not guessed at.
    let (mut r, _) = Rig::with_session(Attended);
    assert_eq!(
        r.c.handle_banner_action(BannerAction::Hide(HideChoice::Minutes(45)))
            .map(|_| ()),
        Err(HideError::UnsupportedDuration)
    );
    assert!(r.c.banner_visible());
}

#[test]
fn the_banner_disconnect_button_is_an_emergency_disconnect() {
    let (mut r, _) = hidden_attended();
    let (outcome, intent) = r.c.handle_banner_action(BannerAction::Disconnect).unwrap();
    assert!(outcome.is_none());
    assert_eq!(intent, IndicatorIntent::EmergencyDisconnect);
}

#[test]
fn the_tray_menu_is_a_fallback_for_both_shortcuts() {
    let (mut r, _) = hidden_attended();
    assert_eq!(
        r.c.handle_tray_action(TrayAction::DisconnectNow),
        IndicatorIntent::EmergencyDisconnect
    );
    assert!(r.c.is_hidden(), "disconnect does not touch the dismissal");
    assert_eq!(
        r.c.handle_tray_action(TrayAction::ShowIndicators),
        IndicatorIntent::None
    );
    assert!(r.c.banner_visible());
}

#[test]
fn emergency_disconnect_works_while_indicators_are_hidden() {
    let (mut r, _) = hidden_attended();
    assert!(r.c.is_hidden());
    assert_eq!(
        r.c.handle_hotkey(HotkeyEvent::EmergencyDisconnect),
        IndicatorIntent::EmergencyDisconnect
    );
}

#[test]
fn restoring_when_nothing_is_hidden_does_nothing() {
    let (mut r, _) = Rig::with_session(Attended);
    assert!(r.c.restore_now().is_empty());
    assert!(r.c.take_events().is_empty());
}

// ---- restart ------------------------------------------------------------------------------

#[test]
fn after_a_crash_restart_the_indicators_are_back() {
    let (mut r, sessions) = Rig::with_session(Attended);
    r.c.hide_until_end().unwrap();
    let stored = r.store.stored().unwrap();
    assert!(stored.contains(r#""monoDeadline":null"#), "{stored}");

    // The host restarts: a new controller over the same store.
    let mut again = IndicatorController::new(Box::new(r.store.clone()), Arc::new(r.clock.clone()));
    let events = again.update(&sessions);
    assert!(again.banner_visible() && again.tray_visible());
    assert_eq!(
        restored_reasons(&events),
        vec![ReportReason::Timer(RestoreReason::CrashRecovery)]
    );
    assert_eq!(r.store.stored(), None);
}

#[test]
fn a_corrupt_store_just_means_nothing_is_hidden() {
    let store = MemoryIndicatorStore::new();
    let mut s = store.clone();
    s.save_dismissal(Some("{{{{"));
    let mut c = IndicatorController::new(Box::new(store), Arc::new(FakeClock::new(T0)));
    c.update(&[snap("s1", Attended, CONTROL)]);
    assert!(c.banner_visible() && c.tray_visible());
}

// ---- first-time notice --------------------------------------------------------------------

#[test]
fn the_first_hide_ever_shows_the_notice_and_later_ones_do_not() {
    let (mut r, sessions) = Rig::with_session(Attended);
    assert!(r.c.hide_for(30).unwrap().show_first_time_notice);
    r.c.restore_now();
    assert!(!r.c.hide_for(30).unwrap().show_first_time_notice);

    // Remembered across restarts too.
    r.c.restore_now();
    let mut again = IndicatorController::new(Box::new(r.store.clone()), Arc::new(r.clock.clone()));
    again.update(&sessions);
    assert!(!again.hide_for(30).unwrap().show_first_time_notice);
}

// ---- reporting -----------------------------------------------------------------------------

#[test]
fn dismissals_and_restores_become_metadata_only_reports() {
    let (mut r, sessions) = Rig::with_session(Attended);
    r.c.hide_for(60).unwrap();
    r.c.update(&sessions);
    r.c.restore_now();
    let events = r.c.take_events();
    assert_eq!(events.len(), 2);

    let dismissed = events[0].to_protocol().unwrap();
    assert_eq!(dismissed.kind, BannerEventType::Dismissed);
    assert_eq!(dismissed.session_id.as_deref(), Some("s1"));
    assert_eq!(dismissed.duration_minutes, Some(60));
    assert_eq!(dismissed.scope, Some(BannerScope::All));
    assert_eq!(dismissed.reason, None);

    let restored = events[1].to_protocol().unwrap();
    assert_eq!(restored.kind, BannerEventType::Restored);
    assert_eq!(restored.reason, Some(BannerReason::UserShowNow));
    assert_eq!(restored.scope, Some(BannerScope::All));
}

#[test]
fn no_dismissal_is_never_reported() {
    let event = IndicatorEvent::Restored {
        session_id: None,
        scope: Scope::Banner,
        reason: ReportReason::Timer(RestoreReason::NoDismissal),
    };
    assert_eq!(event.to_protocol(), None);
}

#[test]
fn every_timer_reason_maps_to_the_server_vocabulary() {
    use RestoreReason::*;
    for (reason, expected) in [
        (NewSession, BannerReason::NewSession),
        (ParticipantChanged, BannerReason::ParticipantChanged),
        (PermissionIncrease, BannerReason::PermissionIncrease),
        (ConsentRenewal, BannerReason::ConsentRenewal),
        (CrashRecovery, BannerReason::CrashRecovery),
        (ClockAmbiguity, BannerReason::ClockAmbiguity),
        (Timeout, BannerReason::Timeout),
        (ResumeAfterExpiry, BannerReason::ResumeAfterExpiry),
        (SessionEnd, BannerReason::SessionEnd),
    ] {
        assert_eq!(ReportReason::Timer(reason).to_protocol(), Some(expected));
    }
}

#[test]
fn the_report_body_carries_shortcuts_and_collision_state() {
    let body = IndicatorController::report(None, 30, "Ctrl+Alt+Shift+B", "Ctrl+Alt+Shift+X", true);
    assert_eq!(body.default_hide_minutes, 30);
    assert_eq!(body.show_hide_shortcut, "Ctrl+Alt+Shift+B");
    assert_eq!(body.disconnect_shortcut, "Ctrl+Alt+Shift+X");
    assert!(body.shortcut_collision);
    assert_eq!(body.event, None);
}

// ---- driving the banner and tray ----------------------------------------------------------------

#[test]
fn sync_drives_the_banner_and_the_tray() {
    let mut r = Rig::new();
    let banner_probe = FakeBanner::new();
    let tray_probe = FakeTray::new();
    let mut banner: Box<dyn Banner> = Box::new(banner_probe.clone());
    let mut tray: Box<dyn Tray> = Box::new(tray_probe.clone());

    // Idle: no banner, tray visible.
    r.c.sync(&[], banner.as_mut(), tray.as_mut());
    assert!(!banner.is_visible());
    assert_eq!(
        tray_probe.model(),
        TrayModel {
            visible: true,
            ..TrayModel::default()
        }
    );

    // A session starts.
    let sessions = vec![snap("s1", Attended, CONTROL_AUDIO)];
    r.c.sync(&sessions, banner.as_mut(), tray.as_mut());
    assert!(banner.is_visible());
    assert_eq!(
        banner_probe.content(),
        Some(BannerContent {
            requester_name: "Sam Helper".into(),
            granted: CONTROL_AUDIO,
        })
    );
    let model = tray_probe.model();
    assert!(model.visible && model.session_live);
    assert_eq!(model.connected_name.as_deref(), Some("Sam Helper"));
    assert_eq!(model.consent_seconds_left, Some(30 * 60));

    // The user hides everything.
    r.c.hide_for(30).unwrap();
    r.c.sync(&sessions, banner.as_mut(), tray.as_mut());
    assert!(!banner.is_visible());
    assert!(!tray_probe.model().visible);
    assert!(
        tray_probe.model().session_live,
        "the model still knows a session is live"
    );

    // The restore shortcut shows both again.
    r.c.handle_hotkey(HotkeyEvent::RestoreIndicators);
    r.c.sync(&sessions, banner.as_mut(), tray.as_mut());
    assert!(banner.is_visible());
    assert!(tray_probe.model().visible);
}

#[test]
fn a_banner_covers_everyone_connected() {
    let mut r = Rig::new();
    let mut other = snap("s2", Attended, CONTROL_AUDIO);
    other.requester_name = "Alex Admin".into();
    other.requester_email = "alex@example.com".into();
    r.c.update(&[snap("s1", Attended, CONTROL), other]);
    let content = r.c.banner_content().unwrap();
    assert_eq!(content.requester_name, "Sam Helper, Alex Admin");
    assert_eq!(
        content.granted, CONTROL_AUDIO,
        "the union of what is granted"
    );
}

#[test]
fn a_stopped_session_no_longer_counts_as_live() {
    let mut r = Rig::new();
    let mut s = snap("s1", Attended, CONTROL);
    r.c.update(std::slice::from_ref(&s));
    assert!(r.c.banner_visible());
    s.running = false; // stopped locally (consent ran out)
    r.c.update(&[s]);
    assert!(!r.c.banner_visible());
}

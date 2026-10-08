//! Pure, host-local dismissal logic. A port of the website's `bannerTimer`.
//!
//! Two clocks guard the deadline: a monotonic clock (immune to wall-clock
//! changes) and a persisted wall-clock deadline (survives restart and sleep).
//! Any ambiguity shows the indicators again. Nothing here touches session
//! authorization.

use serde::{Deserialize, Serialize};

/// Allowed hide durations, in minutes.
pub const DURATIONS: [u32; 5] = [30, 60, 120, 240, 480];

const CLOCK_TOLERANCE_MS: i64 = 120_000;
const OVERSHOOT_MS: i64 = 5_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    /// The large banner only; the tray icon stays.
    Banner,
    /// Banner **and** tray icon. Attended sessions only.
    All,
}

fn default_scope() -> Scope {
    Scope::Banner
}

/// A local request to hide indicators, plus what it was made under.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Dismissal {
    pub session_id: String,
    /// Identity of the remote participant.
    pub participant_key: String,
    /// Granted scopes at dismissal time, comma separated.
    pub perms_key: String,
    /// Consent epoch; changes on renewal.
    pub consent_key: String,
    #[serde(default = "default_scope")]
    pub scope: Scope,
    pub duration_minutes: u32,
    /// Persisted wall-clock deadline (ms since the epoch).
    pub wall_deadline: i64,
    /// Monotonic deadline (ms); `None` after a restart.
    pub mono_deadline: Option<i64>,
}

/// What is true right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Context {
    pub session_id: Option<String>,
    pub participant_key: String,
    pub perms_key: String,
    pub consent_key: String,
    pub now_wall_ms: i64,
    pub now_mono_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestoreReason {
    NoDismissal,
    NewSession,
    ParticipantChanged,
    PermissionIncrease,
    ConsentRenewal,
    CrashRecovery,
    ClockAmbiguity,
    Timeout,
    ResumeAfterExpiry,
    SessionEnd,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DismissError {
    UnsupportedDuration,
    NoSession,
    SessionAlreadyEnded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Evaluation {
    /// The large banner may stay hidden.
    pub hidden: bool,
    /// The tray icon may stay hidden too.
    pub tray_hidden: bool,
    /// Why the indicators must be shown; `None` while hidden.
    pub reason: Option<RestoreReason>,
    pub remaining_ms: i64,
}

fn show(reason: RestoreReason) -> Evaluation {
    Evaluation {
        hidden: false,
        tray_hidden: false,
        reason: Some(reason),
        remaining_ms: 0,
    }
}

/// True if `after` lists a scope that `before` does not.
pub fn perms_increased(before: &str, after: &str) -> bool {
    let before: Vec<&str> = before.split(',').filter(|s| !s.is_empty()).collect();
    after
        .split(',')
        .filter(|s| !s.is_empty())
        .any(|p| !before.contains(&p))
}

/// Hide for a fixed time. `scope` is `Banner` or `All`.
pub fn dismiss(ctx: &Context, minutes: u32, scope: Scope) -> Result<Dismissal, DismissError> {
    if !DURATIONS.contains(&minutes) {
        return Err(DismissError::UnsupportedDuration);
    }
    let session_id = ctx.session_id.clone().ok_or(DismissError::NoSession)?;
    let ms = i64::from(minutes) * 60_000;
    Ok(Dismissal {
        session_id,
        participant_key: ctx.participant_key.clone(),
        perms_key: ctx.perms_key.clone(),
        consent_key: ctx.consent_key.clone(),
        scope,
        duration_minutes: minutes,
        wall_deadline: ctx.now_wall_ms + ms,
        mono_deadline: Some(ctx.now_mono_ms + ms),
    })
}

/// Hide everything (banner and tray) until the session's maximum end. The
/// same restore rules apply.
pub fn dismiss_all(ctx: &Context, session_ends_at_wall_ms: i64) -> Result<Dismissal, DismissError> {
    let session_id = ctx.session_id.clone().ok_or(DismissError::NoSession)?;
    let ms = session_ends_at_wall_ms - ctx.now_wall_ms;
    if ms <= 0 {
        return Err(DismissError::SessionAlreadyEnded);
    }
    Ok(Dismissal {
        session_id,
        participant_key: ctx.participant_key.clone(),
        perms_key: ctx.perms_key.clone(),
        consent_key: ctx.consent_key.clone(),
        scope: Scope::All,
        duration_minutes: u32::try_from((ms + 59_999) / 60_000).unwrap_or(u32::MAX),
        wall_deadline: ctx.now_wall_ms + ms,
        mono_deadline: Some(ctx.now_mono_ms + ms),
    })
}

/// Must the indicators be shown? `hidden` is true only when every safeguard
/// agrees the dismissal is still valid. Checks run in this exact order.
pub fn evaluate(d: Option<&Dismissal>, ctx: &Context) -> Evaluation {
    let Some(session_id) = &ctx.session_id else {
        return show(RestoreReason::SessionEnd);
    };
    let Some(d) = d else {
        return show(RestoreReason::NoDismissal);
    };
    if &d.session_id != session_id {
        return show(RestoreReason::NewSession);
    }
    if d.participant_key != ctx.participant_key {
        return show(RestoreReason::ParticipantChanged);
    }
    if perms_increased(&d.perms_key, &ctx.perms_key) {
        return show(RestoreReason::PermissionIncrease);
    }
    if d.consent_key != ctx.consent_key {
        return show(RestoreReason::ConsentRenewal);
    }
    let Some(mono_deadline) = d.mono_deadline else {
        return show(RestoreReason::CrashRecovery);
    };
    let wall_left = d.wall_deadline - ctx.now_wall_ms;
    let mono_left = mono_deadline - ctx.now_mono_ms;
    if (wall_left - mono_left).abs() > CLOCK_TOLERANCE_MS {
        return show(RestoreReason::ClockAmbiguity);
    }
    if mono_left <= 0 || wall_left <= 0 {
        // A big overshoot means we were asleep past the deadline.
        return show(if mono_left.min(wall_left) < -OVERSHOOT_MS {
            RestoreReason::ResumeAfterExpiry
        } else {
            RestoreReason::Timeout
        });
    }
    Evaluation {
        hidden: true,
        tray_hidden: d.scope == Scope::All,
        reason: None,
        remaining_ms: wall_left.min(mono_left),
    }
}

/// Serialize for storage. The monotonic deadline is dropped on purpose, so
/// any restart shows the indicators.
pub fn persist(d: &Dismissal) -> String {
    let stored = Dismissal {
        mono_deadline: None,
        ..d.clone()
    };
    serde_json::to_string(&stored).expect("a dismissal always serializes")
}

/// Load a stored dismissal. Anything unreadable is "no dismissal".
pub fn restore(raw: Option<&str>) -> Option<Dismissal> {
    let raw = raw?;
    let mut d: Dismissal = serde_json::from_str(raw).ok()?;
    d.mono_deadline = None;
    Some(d)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Cases copied from the website's helpers/bannerTimer.spec.
    fn base() -> Context {
        Context {
            session_id: Some("s1".into()),
            participant_key: "viewer@example.com".into(),
            perms_key: "control".into(),
            consent_key: "c1".into(),
            now_wall_ms: 1_000_000_000_000,
            now_mono_ms: 5_000,
        }
    }

    fn at(ms: i64) -> Context {
        let b = base();
        Context {
            now_wall_ms: b.now_wall_ms + ms,
            now_mono_ms: b.now_mono_ms + ms,
            ..b
        }
    }

    fn with(ctx: Context, f: impl FnOnce(&mut Context)) -> Context {
        let mut c = ctx;
        f(&mut c);
        c
    }

    #[test]
    fn hides_for_exactly_the_chosen_minutes_then_restores() {
        for minutes in DURATIONS {
            let d = dismiss(&base(), minutes, Scope::Banner).unwrap();
            let ms = i64::from(minutes) * 60_000;
            assert!(evaluate(Some(&d), &at(ms - 1000)).hidden, "{minutes}");
            let r = evaluate(Some(&d), &at(ms));
            assert!(!r.hidden);
            assert_eq!(r.reason, Some(RestoreReason::Timeout));
        }
    }

    #[test]
    fn rejects_unsupported_durations() {
        for bad in [0, 1, 29, 45, 481, 1000] {
            assert_eq!(
                dismiss(&base(), bad, Scope::Banner),
                Err(DismissError::UnsupportedDuration)
            );
        }
    }

    #[test]
    fn cannot_dismiss_without_a_session() {
        let ctx = with(base(), |c| c.session_id = None);
        assert_eq!(
            dismiss(&ctx, 30, Scope::Banner),
            Err(DismissError::NoSession)
        );
        assert_eq!(dismiss_all(&ctx, i64::MAX), Err(DismissError::NoSession));
    }

    #[test]
    fn restores_on_a_new_session() {
        let d = dismiss(&base(), 60, Scope::Banner).unwrap();
        let ctx = with(at(1000), |c| c.session_id = Some("s2".into()));
        assert_eq!(
            evaluate(Some(&d), &ctx).reason,
            Some(RestoreReason::NewSession)
        );
    }

    #[test]
    fn restores_when_the_participant_changes() {
        let d = dismiss(&base(), 60, Scope::Banner).unwrap();
        let ctx = with(at(1000), |c| c.participant_key = "other@example.com".into());
        assert_eq!(
            evaluate(Some(&d), &ctx).reason,
            Some(RestoreReason::ParticipantChanged)
        );
    }

    #[test]
    fn restores_on_permission_increase_but_not_decrease() {
        let start = with(base(), |c| c.perms_key = "control,systemAudio".into());
        let d = dismiss(&start, 60, Scope::Banner).unwrap();
        let fewer = with(at(1000), |c| c.perms_key = "control".into());
        assert!(evaluate(Some(&d), &fewer).hidden);
        let more = with(at(1000), |c| {
            c.perms_key = "control,systemAudio,microphone".into();
        });
        assert_eq!(
            evaluate(Some(&d), &more).reason,
            Some(RestoreReason::PermissionIncrease)
        );
    }

    #[test]
    fn restores_on_consent_renewal() {
        let d = dismiss(&base(), 480, Scope::Banner).unwrap();
        let ctx = with(at(1000), |c| c.consent_key = "c2".into());
        assert_eq!(
            evaluate(Some(&d), &ctx).reason,
            Some(RestoreReason::ConsentRenewal)
        );
    }

    #[test]
    fn restores_after_a_restart_because_the_monotonic_deadline_is_lost() {
        let d = dismiss(&base(), 240, Scope::Banner).unwrap();
        let reloaded = restore(Some(&persist(&d))).unwrap();
        assert_eq!(reloaded.mono_deadline, None);
        assert_eq!(
            evaluate(Some(&reloaded), &at(1000)).reason,
            Some(RestoreReason::CrashRecovery)
        );
    }

    #[test]
    fn restores_when_the_wall_clock_jumps_backwards() {
        let d = dismiss(&base(), 240, Scope::Banner).unwrap();
        let ctx = with(at(60_000), |c| {
            c.now_wall_ms = base().now_wall_ms - 3_600_000
        });
        assert_eq!(
            evaluate(Some(&d), &ctx).reason,
            Some(RestoreReason::ClockAmbiguity)
        );
    }

    #[test]
    fn restores_when_the_wall_clock_jumps_forwards_too() {
        let d = dismiss(&base(), 240, Scope::Banner).unwrap();
        let ctx = with(at(60_000), |c| c.now_wall_ms += 3_600_000);
        assert_eq!(
            evaluate(Some(&d), &ctx).reason,
            Some(RestoreReason::ClockAmbiguity)
        );
    }

    #[test]
    fn small_clock_drift_is_tolerated() {
        let d = dismiss(&base(), 240, Scope::Banner).unwrap();
        let ctx = with(at(60_000), |c| c.now_wall_ms += 100_000);
        assert!(evaluate(Some(&d), &ctx).hidden);
    }

    #[test]
    fn restores_after_sleeping_past_the_deadline() {
        let d = dismiss(&base(), 30, Scope::Banner).unwrap();
        assert_eq!(
            evaluate(Some(&d), &at(31 * 60_000)).reason,
            Some(RestoreReason::ResumeAfterExpiry)
        );
        // A hair past the deadline is an ordinary timeout.
        assert_eq!(
            evaluate(Some(&d), &at(30 * 60_000 + 3_000)).reason,
            Some(RestoreReason::Timeout)
        );
    }

    #[test]
    fn shows_when_the_session_has_ended() {
        let d = dismiss(&base(), 30, Scope::Banner).unwrap();
        let ctx = with(at(1000), |c| c.session_id = None);
        assert_eq!(
            evaluate(Some(&d), &ctx).reason,
            Some(RestoreReason::SessionEnd)
        );
    }

    #[test]
    fn shows_when_there_is_no_dismissal() {
        let r = evaluate(None, &base());
        assert!(!r.hidden && !r.tray_hidden);
        assert_eq!(r.reason, Some(RestoreReason::NoDismissal));
    }

    #[test]
    fn session_end_wins_over_no_dismissal() {
        let ctx = with(base(), |c| c.session_id = None);
        assert_eq!(evaluate(None, &ctx).reason, Some(RestoreReason::SessionEnd));
    }

    #[test]
    fn a_dismissal_carries_no_authorization() {
        let d = dismiss(&base(), 480, Scope::Banner).unwrap();
        let json = persist(&d);
        assert!(!json.contains("consentExpires"));
        assert!(!json.contains("authoriz"));
    }

    #[test]
    fn hide_all_until_the_session_ends() {
        let end = base().now_wall_ms + 3 * 3_600_000;
        let d = dismiss_all(&base(), end).unwrap();
        assert_eq!(d.scope, Scope::All);
        assert_eq!(d.duration_minutes, 180);
        let r = evaluate(Some(&d), &at(2 * 3_600_000));
        assert!(r.hidden && r.tray_hidden);
        assert!(!evaluate(Some(&d), &at(3 * 3_600_000)).hidden);
    }

    #[test]
    fn hide_all_rejects_a_session_that_is_already_over() {
        assert_eq!(
            dismiss_all(&base(), base().now_wall_ms),
            Err(DismissError::SessionAlreadyEnded)
        );
    }

    #[test]
    fn a_banner_only_dismissal_never_hides_the_tray() {
        let d = dismiss(&base(), 60, Scope::Banner).unwrap();
        let r = evaluate(Some(&d), &at(1000));
        assert!(r.hidden);
        assert!(!r.tray_hidden);
    }

    #[test]
    fn a_timed_hide_with_scope_all_hides_banner_and_tray_for_exactly_that_time() {
        for m in DURATIONS {
            let d = dismiss(&base(), m, Scope::All).unwrap();
            let ms = i64::from(m) * 60_000;
            assert!(evaluate(Some(&d), &at(ms - 1000)).tray_hidden);
            let after = evaluate(Some(&d), &at(ms));
            assert!(!after.hidden && !after.tray_hidden);
        }
    }

    #[test]
    fn everything_comes_back_on_renewal_participant_change_permissions_and_restart() {
        let end = base().now_wall_ms + 3 * 3_600_000;
        let d = dismiss_all(&base(), end).unwrap();
        let renewal = with(at(1000), |c| c.consent_key = "c2".into());
        assert!(!evaluate(Some(&d), &renewal).tray_hidden);
        let person = with(at(1000), |c| c.participant_key = "x@y.z".into());
        assert!(!evaluate(Some(&d), &person).tray_hidden);
        let perms = with(at(1000), |c| c.perms_key = "control,microphone".into());
        assert!(!evaluate(Some(&d), &perms).tray_hidden);
        let reloaded = restore(Some(&persist(&d))).unwrap();
        assert!(!evaluate(Some(&reloaded), &at(1000)).tray_hidden);
    }

    #[test]
    fn remaining_time_is_the_smaller_of_the_two_clocks() {
        let d = dismiss(&base(), 30, Scope::Banner).unwrap();
        let ctx = with(at(60_000), |c| c.now_wall_ms += 30_000);
        let r = evaluate(Some(&d), &ctx);
        assert_eq!(r.remaining_ms, 30 * 60_000 - 90_000);
    }

    #[test]
    fn restore_handles_missing_and_corrupt_storage() {
        assert_eq!(restore(None), None);
        assert_eq!(restore(Some("")), None);
        assert_eq!(restore(Some("not json")), None);
        assert_eq!(restore(Some("{}")), None);
    }

    #[test]
    fn stored_dismissals_without_a_scope_default_to_banner_only() {
        let raw = r#"{"sessionId":"s1","participantKey":"p","permsKey":"control","consentKey":"c1",
            "durationMinutes":30,"wallDeadline":1,"monoDeadline":7}"#;
        let d = restore(Some(raw)).unwrap();
        assert_eq!(d.scope, Scope::Banner);
        assert_eq!(
            d.mono_deadline, None,
            "never trust a stored monotonic deadline"
        );
    }

    #[test]
    fn perms_increase_detection() {
        assert!(!perms_increased("control", "control"));
        assert!(!perms_increased("control,clipboard", "control"));
        assert!(perms_increased("control", "control,clipboard"));
        assert!(perms_increased("", "control"));
        assert!(!perms_increased("", ""));
    }
}

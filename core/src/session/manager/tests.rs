use super::*;
use crate::fakes::{
    FakeClock, FakeConsentUi, FakeEffects, FakeHostApi, FakeKeyStore, Recorded, Trace, test_session,
};
use crate::protocol::types::SessionMode::{Attended, Unattended};

const T0: i64 = 1_791_462_818_897;

const VIEW: Permissions = Permissions::NONE;
const CONTROL: Permissions = Permissions {
    control: true,
    ..Permissions::NONE
};
const CLIPBOARD: Permissions = Permissions {
    clipboard: true,
    ..Permissions::NONE
};
const CONTROL_CLIPBOARD: Permissions = Permissions {
    control: true,
    clipboard: true,
    ..Permissions::NONE
};
const ALL: Permissions = Permissions {
    control: true,
    system_audio: true,
    microphone: true,
    clipboard: true,
};

struct Rig {
    mgr: SessionManager<FakeHostApi>,
    api: FakeHostApi,
    ui: FakeConsentUi,
    effects: FakeEffects,
    keys: FakeKeyStore,
    /// The host's clock.
    clock: FakeClock,
    /// The server's clock: separate, so tests can jump only the host's wall clock.
    server_clock: FakeClock,
    trace: Trace,
}

impl Rig {
    fn new() -> Self {
        Self::with(HostSettings::default())
    }

    fn with(settings: HostSettings) -> Self {
        let trace = Trace::new();
        let clock = FakeClock::new(T0);
        let server_clock = FakeClock::new(T0);
        let api = FakeHostApi::with_trace(server_clock.clone(), trace.clone());
        let ui = FakeConsentUi::new();
        let effects = FakeEffects::with_trace(trace.clone());
        let keys = FakeKeyStore::new();
        let mgr = SessionManager::new(
            api.clone(),
            Arc::new(clock.clone()),
            Box::new(ui.clone()),
            Box::new(effects.clone()),
            Arc::new(keys.clone()),
            settings,
        );
        Self {
            mgr,
            api,
            ui,
            effects,
            keys,
            clock,
            server_clock,
            trace,
        }
    }

    fn unattended() -> Self {
        let rig = Self::with(HostSettings {
            unattended_opt_in: true,
            host_fingerprint: None,
        });
        rig.api.set_unattended_enabled(true);
        rig
    }

    fn advance(&self, d: Duration) {
        self.clock.advance(d);
        self.server_clock.advance(d);
    }

    async fn tick(&mut self) {
        self.mgr.tick().await;
    }

    /// Time passes in 5 s steps with a tick after each, like the real run loop.
    async fn run(&mut self, secs: u64) {
        for _ in 0..secs / 5 {
            self.advance(Duration::from_secs(5));
            self.tick().await;
        }
        let rest = secs % 5;
        if rest > 0 {
            self.advance(Duration::from_secs(rest));
            self.tick().await;
        }
    }

    fn prompts_where(&self, f: impl Fn(&ConsentPrompt) -> bool) -> Vec<(PromptId, ConsentPrompt)> {
        self.ui.shown().into_iter().filter(|(_, p)| f(p)).collect()
    }

    fn attended_prompts(&self) -> Vec<(PromptId, ConsentPrompt)> {
        self.prompts_where(|p| matches!(p, ConsentPrompt::AttendedRequest { .. }))
    }

    fn renewal_prompts(&self) -> Vec<(PromptId, ConsentPrompt)> {
        self.prompts_where(|p| matches!(p, ConsentPrompt::RenewConsent { .. }))
    }

    fn escalation_prompts(&self) -> Vec<(PromptId, ConsentPrompt)> {
        self.prompts_where(|p| matches!(p, ConsentPrompt::PermissionEscalation { .. }))
    }

    /// A viewer asks; the host sees the prompt and answers with `answer`.
    async fn request_and_answer(
        &mut self,
        id: &str,
        requested: Permissions,
        answer: ConsentAnswer,
    ) {
        self.api
            .add_request(test_session(id, Attended, requested), 5);
        // Time passes so the next poll is due (45 s when idle) and the host
        // sees the request.
        self.run(45).await;
        let (pid, _) = self.attended_prompts().pop().expect("an approval prompt");
        assert!(
            matches!(
                self.ui.shown().last().map(|(_, p)| p.clone()),
                Some(ConsentPrompt::AttendedRequest { .. })
            ),
            "the newest prompt is the one for {id}"
        );
        self.ui.answer(pid, answer);
        self.tick().await;
    }

    /// Request, approve with `grant` for 30 minutes; the session is live after.
    async fn live_attended(&mut self, id: &str, requested: Permissions, grant: Permissions) {
        self.request_and_answer(
            id,
            requested,
            ConsentAnswer::Approve {
                grant,
                approved_minutes: 30,
            },
        )
        .await;
        assert!(self.effects.is_running(id), "{id} should be running");
    }
}

fn approve(grant: Permissions, minutes: u32) -> ConsentAnswer {
    ConsentAnswer::Approve {
        grant,
        approved_minutes: minutes,
    }
}

// ---- attended approval ----------------------------------------------------

#[tokio::test]
async fn attended_request_names_the_requester_and_waits_for_the_user() {
    let mut r = Rig::new();
    r.api
        .add_request(test_session("s1", Attended, CONTROL_CLIPBOARD), 5);
    r.tick().await;

    let shown = r.attended_prompts();
    assert_eq!(shown.len(), 1);
    assert_eq!(
        shown[0].1,
        ConsentPrompt::AttendedRequest {
            requester_name: "Sam Helper".into(),
            requester_email: "sam@example.com".into(),
            requester_verified: true,
            requested: CONTROL_CLIPBOARD,
        }
    );
    assert!(
        r.api.decides().is_empty(),
        "nothing is decided without the user"
    );
    assert!(r.effects.running().is_empty());

    // Still one prompt after more polls.
    r.run(60).await;
    assert_eq!(r.attended_prompts().len(), 1);
}

#[tokio::test]
async fn approving_sends_decide_and_starts_the_session() {
    let mut r = Rig::with(HostSettings {
        unattended_opt_in: false,
        host_fingerprint: Some("sha-256 AB:CD".into()),
    });
    r.live_attended("s1", CONTROL_CLIPBOARD, CONTROL).await;

    let decides = r.api.decides();
    assert_eq!(decides.len(), 1);
    assert_eq!(decides[0].decision, Decision::Approve);
    assert_eq!(decides[0].grant, Some(CONTROL));
    assert_eq!(decides[0].approved_minutes, Some(30));
    assert_eq!(
        decides[0].host_fingerprint.as_deref(),
        Some("sha-256 AB:CD")
    );
    assert!(
        r.trace
            .entries()
            .contains(&"effects:start:s1:control".to_string())
    );
}

#[tokio::test]
async fn view_only_is_the_default_grant() {
    let mut r = Rig::new();
    r.live_attended("s1", ALL, VIEW).await;
    assert_eq!(r.api.decides()[0].grant, Some(VIEW));
    assert!(
        r.trace
            .entries()
            .contains(&"effects:start:s1:view".to_string())
    );
}

#[tokio::test]
async fn the_grant_never_exceeds_what_was_requested() {
    let mut r = Rig::new();
    // The user (or a buggy UI) ticks everything; only control was asked for.
    r.request_and_answer("s1", CONTROL, approve(ALL, 30)).await;
    assert_eq!(r.api.decides()[0].grant, Some(CONTROL));
    assert!(r.effects.is_running("s1"));
}

#[tokio::test]
async fn deny_and_a_closed_prompt_both_deny() {
    let mut r = Rig::new();
    r.request_and_answer("s1", CONTROL, ConsentAnswer::Deny)
        .await;
    r.request_and_answer("s2", CONTROL, ConsentAnswer::Dismissed)
        .await;
    let decides = r.api.decides();
    assert_eq!(decides.len(), 2);
    assert!(decides.iter().all(|d| d.decision == Decision::Deny));
    assert!(decides.iter().all(|d| d.grant.is_none()));
    assert!(r.effects.running().is_empty());
    assert!(
        !r.trace
            .entries()
            .iter()
            .any(|e| e.starts_with("effects:start"))
    );
}

#[tokio::test]
async fn the_consent_window_is_clamped_and_only_longer_by_explicit_choice() {
    for (asked, sent) in [(5, 30), (30, 30), (120, 120), (480, 480), (9999, 480)] {
        let mut r = Rig::new();
        r.request_and_answer("s1", VIEW, approve(VIEW, asked)).await;
        assert_eq!(
            r.api.decides()[0].approved_minutes,
            Some(sent),
            "asked {asked}"
        );
    }
    // The owner's policy caps it further.
    let mut r = Rig::new();
    r.api.set_policy(PolicyView {
        consent_renewal_minutes: 30,
        max_session_minutes: 90,
        allow_control_unattended: false,
        allow_audio_unattended: false,
    });
    r.request_and_answer("s1", VIEW, approve(VIEW, 480)).await;
    assert_eq!(r.api.decides()[0].approved_minutes, Some(90));
}

#[tokio::test]
async fn an_unanswered_approval_prompt_is_withdrawn_when_the_window_ends() {
    let mut r = Rig::new();
    r.api.add_request(test_session("s1", Attended, CONTROL), 5);
    r.tick().await;
    let pid = r.attended_prompts()[0].0;

    r.run(5 * 60).await;
    assert!(
        r.ui.withdrawn().contains(&pid),
        "stale prompt must disappear"
    );
    assert!(
        r.api.decides().is_empty(),
        "no consent was given, so nothing is sent"
    );

    // The server times the request out; the host forgets it.
    r.api.end_session_on_server("s1");
    r.run(15).await;
    assert!(r.mgr.sessions().is_empty());
    assert!(!r.mgr.is_busy());
}

// ---- unattended -------------------------------------------------------------

fn policy(control: bool, audio: bool) -> PolicyView {
    PolicyView {
        consent_renewal_minutes: 30,
        max_session_minutes: 480,
        allow_control_unattended: control,
        allow_audio_unattended: audio,
    }
}

#[tokio::test]
async fn unattended_is_approved_without_a_prompt_within_policy() {
    let mut r = Rig::unattended();
    r.api.set_policy(policy(true, false));
    r.api.add_request(test_session("u1", Unattended, ALL), 5);
    r.tick().await;

    assert!(r.ui.shown().is_empty(), "no one is there to ask");
    let d = &r.api.decides()[0];
    assert_eq!(d.decision, Decision::Approve);
    assert_eq!(
        d.grant,
        Some(CONTROL),
        "control only: audio policy is off, mic and clipboard never"
    );
    assert_eq!(d.approved_minutes, None);
    assert!(r.effects.is_running("u1"));
}

#[tokio::test]
async fn unattended_audio_follows_policy_but_the_microphone_never_does() {
    let mut r = Rig::unattended();
    r.api.set_policy(policy(false, true));
    r.api.add_request(test_session("u1", Unattended, ALL), 5);
    r.tick().await;
    assert_eq!(
        r.api.decides()[0].grant,
        Some(Permissions {
            system_audio: true,
            ..Permissions::NONE
        })
    );
}

#[tokio::test]
async fn unattended_is_denied_without_the_local_opt_in() {
    let mut r = Rig::new(); // opt-in off
    r.api.set_unattended_enabled(true);
    r.api.set_policy(policy(true, true));
    r.api
        .add_request(test_session("u1", Unattended, CONTROL), 5);
    r.tick().await;
    assert_eq!(r.api.decides()[0].decision, Decision::Deny);
    assert!(r.effects.running().is_empty());
}

#[tokio::test]
async fn unattended_is_denied_when_the_owner_has_not_enabled_it() {
    let mut r = Rig::with(HostSettings {
        unattended_opt_in: true, // local opt-in alone is not enough
        host_fingerprint: None,
    });
    r.api.set_policy(policy(true, true));
    r.api
        .add_request(test_session("u1", Unattended, CONTROL), 5);
    r.tick().await;
    assert_eq!(r.api.decides()[0].decision, Decision::Deny);
}

#[test]
fn unattended_grant_truth_table() {
    let enabled = DeviceView {
        id: "d".into(),
        name: "n".into(),
        unattended_enabled: true,
    };
    let disabled = DeviceView {
        unattended_enabled: false,
        ..enabled.clone()
    };
    let opt_in = HostSettings {
        unattended_opt_in: true,
        host_fingerprint: None,
    };
    let no_opt_in = HostSettings::default();
    for (ctl, aud) in [(false, false), (true, false), (false, true), (true, true)] {
        let p = policy(ctl, aud);
        let got = unattended_grant(&ALL, &enabled, &p, &opt_in).unwrap();
        assert_eq!(got.control, ctl);
        assert_eq!(got.system_audio, aud);
        assert!(!got.microphone, "microphone is never granted unattended");
        assert!(!got.clipboard, "clipboard is never granted unattended");
        assert!(got.is_subset_of(&ALL));
        assert_eq!(unattended_grant(&ALL, &disabled, &p, &opt_in), None);
        assert_eq!(unattended_grant(&ALL, &enabled, &p, &no_opt_in), None);
    }
    // Nothing requested, nothing granted.
    assert_eq!(
        unattended_grant(&VIEW, &enabled, &policy(true, true), &opt_in),
        Some(VIEW)
    );
}

// ---- consent renewal ----------------------------------------------------------

#[tokio::test]
async fn renewal_is_asked_two_minutes_early_once_and_again_each_window() {
    let mut r = Rig::new();
    r.live_attended("s1", CONTROL, CONTROL).await;

    r.run(27 * 60).await; // 3 minutes left
    assert!(r.renewal_prompts().is_empty());
    r.run(65).await; // 115 s left
    assert_eq!(r.renewal_prompts().len(), 1);
    assert_eq!(
        r.renewal_prompts()[0].1,
        ConsentPrompt::RenewConsent {
            requester_name: "Sam Helper".into()
        }
    );
    r.run(30).await;
    assert_eq!(
        r.renewal_prompts().len(),
        1,
        "asked once per consent window"
    );

    // The user keeps sharing for an hour.
    let pid = r.renewal_prompts()[0].0;
    r.ui.answer(pid, ConsentAnswer::Renew { minutes: 60 });
    r.tick().await;
    let sent = r.api.sessions_sent();
    assert_eq!(
        sent.last().unwrap().action,
        SessionAction::RenewConsent { minutes: Some(60) }
    );
    assert!(r.effects.is_running("s1"));
    assert_eq!(r.renewal_prompts().len(), 1);

    r.run(57 * 60).await;
    assert_eq!(
        r.renewal_prompts().len(),
        1,
        "a fresh window is not near its end yet"
    );
    r.run(75).await;
    assert_eq!(
        r.renewal_prompts().len(),
        2,
        "asked again near the end of the new window"
    );
    assert!(r.effects.is_running("s1"));
}

#[tokio::test]
async fn ignored_renewal_stops_capture_at_the_consent_deadline() {
    let mut r = Rig::new();
    r.live_attended("s1", CONTROL, CONTROL).await;

    r.run(30 * 60 - 5).await;
    assert!(r.effects.is_running("s1"), "still within consent");
    let pid = r.renewal_prompts()[0].0;

    r.run(5).await;
    assert!(
        !r.effects.is_running("s1"),
        "consent ran out: capture must stop now"
    );
    assert!(
        r.ui.withdrawn().contains(&pid),
        "the stale prompt is withdrawn"
    );

    // The server did not sweep it (the fake has no sweep): the host ends it.
    r.run(15).await;
    assert_eq!(
        r.api.sessions_sent().last().unwrap().action,
        SessionAction::End {
            reason: EndReason::HostStop
        }
    );
    assert_eq!(r.trace.count("effects:start"), 1, "never restarted");
}

#[tokio::test]
async fn no_cleanup_is_sent_when_the_server_already_expired_the_session() {
    let mut r = Rig::new();
    r.live_attended("s1", CONTROL, CONTROL).await;
    r.run(30 * 60 - 5).await;
    r.api.end_session_on_server("s1"); // server sweep: consent_not_renewed
    r.run(15).await;
    assert!(!r.effects.is_running("s1"));
    assert!(r.api.sessions_sent().is_empty());
    assert!(r.mgr.sessions().is_empty());
}

#[tokio::test]
async fn consent_cannot_be_extended_by_changing_the_wall_clock() {
    let mut r = Rig::new();
    r.live_attended("s1", CONTROL, CONTROL).await;

    // Someone sets the host's clock back two hours, hoping to keep the session.
    r.clock.jump_wall(-2 * 3600 * 1000);
    r.run(30 * 60 - 5).await;
    assert!(r.effects.is_running("s1"));
    r.run(5).await;
    assert!(
        !r.effects.is_running("s1"),
        "monotonic time decides, not the wall clock"
    );

    // And a forward jump does not end a healthy session early.
    let mut r = Rig::new();
    r.live_attended("s1", CONTROL, CONTROL).await;
    r.clock.jump_wall(2 * 3600 * 1000);
    r.run(10 * 60).await;
    assert!(r.effects.is_running("s1"));
}

#[tokio::test]
async fn unattended_sessions_get_no_renewal_prompt() {
    let mut r = Rig::unattended();
    r.api.set_policy(policy(true, false));
    r.api
        .add_request(test_session("u1", Unattended, CONTROL), 5);
    r.tick().await;
    r.run(60 * 60).await;
    assert!(r.renewal_prompts().is_empty());
    assert!(r.effects.is_running("u1"));
}

#[tokio::test]
async fn the_maximum_lifetime_is_enforced_locally() {
    let mut r = Rig::new();
    r.api.set_policy(PolicyView {
        consent_renewal_minutes: 30,
        max_session_minutes: 60,
        allow_control_unattended: false,
        allow_audio_unattended: false,
    });
    r.request_and_answer("s1", CONTROL, approve(CONTROL, 30))
        .await;
    // Renew twice so only the lifetime cap is left.
    r.run(28 * 60).await;
    let pid = r.renewal_prompts()[0].0;
    r.ui.answer(pid, ConsentAnswer::Renew { minutes: 480 });
    r.tick().await;
    r.run(31 * 60).await;
    assert!(r.effects.is_running("s1"), "just inside 60 minutes");
    r.run(2 * 60).await;
    assert!(!r.effects.is_running("s1"), "past the 60 minute lifetime");
}

// ---- escalation ----------------------------------------------------------------

#[tokio::test]
async fn escalation_prompts_once_and_approve_grants_exactly_the_request() {
    let mut r = Rig::new();
    r.live_attended("s1", CONTROL_CLIPBOARD, CONTROL).await;
    r.api.request_more("s1", CONTROL_CLIPBOARD);
    r.run(15).await;

    let prompts = r.escalation_prompts();
    assert_eq!(prompts.len(), 1);
    assert_eq!(
        prompts[0].1,
        ConsentPrompt::PermissionEscalation {
            requester_name: "Sam Helper".into(),
            added: CLIPBOARD,
        }
    );
    r.run(45).await;
    assert_eq!(
        r.escalation_prompts().len(),
        1,
        "no repeat while it is open"
    );
    assert!(
        r.api.sessions_sent().is_empty(),
        "nothing is granted by waiting"
    );

    r.ui.answer(prompts[0].0, ConsentAnswer::Allow);
    r.tick().await;
    assert_eq!(
        r.api.sessions_sent().last().unwrap().action,
        SessionAction::SetPermissions {
            grant: CONTROL_CLIPBOARD
        }
    );
    // The media layer is told the new grant.
    assert!(
        r.trace
            .entries()
            .contains(&"effects:grant:s1:control+clipboard".to_string())
    );
    assert_eq!(r.escalation_prompts().len(), 1, "answered, not re-asked");
}

#[tokio::test]
async fn escalation_declined_keeps_the_old_grant() {
    for answer in [ConsentAnswer::Deny, ConsentAnswer::Dismissed] {
        let mut r = Rig::new();
        r.live_attended("s1", ALL, CONTROL).await;
        r.api.request_more("s1", CONTROL_CLIPBOARD);
        r.run(15).await;
        let pid = r.escalation_prompts()[0].0;
        r.ui.answer(pid, answer);
        r.tick().await;
        assert_eq!(
            r.api.sessions_sent().last().unwrap().action,
            SessionAction::DeclinePermissions
        );
        assert_eq!(
            r.api.session_view("s1").unwrap().granted_permissions,
            CONTROL
        );
        assert!(
            !r.trace
                .entries()
                .iter()
                .any(|e| e.starts_with("effects:grant"))
        );
        assert_eq!(r.api.session_view("s1").unwrap().pending_permissions, None);
    }
}

#[tokio::test]
async fn a_changed_escalation_request_replaces_the_prompt() {
    let mut r = Rig::new();
    r.live_attended("s1", ALL, VIEW).await;
    r.api.request_more("s1", CLIPBOARD);
    r.run(15).await;
    let first = r.escalation_prompts()[0].0;

    r.api.request_more("s1", CONTROL_CLIPBOARD);
    r.run(15).await;
    assert!(r.ui.withdrawn().contains(&first), "old question withdrawn");
    assert_eq!(r.escalation_prompts().len(), 2);

    // The viewer gives up: the open prompt goes away.
    let second = r.escalation_prompts()[1].0;
    r.api.update_session("s1", |v| v.pending_permissions = None);
    r.run(15).await;
    assert!(r.ui.withdrawn().contains(&second));
}

#[tokio::test]
async fn escalation_that_adds_nothing_is_declined_without_asking() {
    let mut r = Rig::new();
    r.live_attended("s1", CONTROL, CONTROL).await;
    r.api.request_more("s1", CONTROL);
    r.run(15).await;
    assert!(r.escalation_prompts().is_empty());
    assert_eq!(
        r.api.sessions_sent().last().unwrap().action,
        SessionAction::DeclinePermissions
    );
}

#[tokio::test]
async fn unattended_escalation_is_automatic_but_only_within_policy() {
    let mut r = Rig::unattended();
    r.api.set_policy(policy(true, true));
    r.api.add_request(test_session("u1", Unattended, ALL), 5);
    r.tick().await;
    // Starts with control + audio. Now the viewer asks for the microphone and clipboard.
    r.api.request_more("u1", ALL);
    r.run(15).await;
    assert!(r.ui.shown().is_empty());
    assert_eq!(
        r.api.sessions_sent().last().unwrap().action,
        SessionAction::DeclinePermissions,
        "microphone and clipboard are never granted unattended, so nothing is added"
    );

    // Control only at first, then audio is requested and the policy allows it.
    let mut r = Rig::unattended();
    r.api.set_policy(policy(true, true));
    r.api
        .add_request(test_session("u1", Unattended, CONTROL), 5);
    r.tick().await;
    r.api.request_more(
        "u1",
        Permissions {
            system_audio: true,
            microphone: true,
            ..Permissions::NONE
        },
    );
    r.run(15).await;
    assert_eq!(
        r.api.sessions_sent().last().unwrap().action,
        SessionAction::SetPermissions {
            grant: Permissions {
                control: true,
                system_audio: true,
                ..Permissions::NONE
            }
        }
    );
}

#[test]
fn unattended_escalation_truth_table() {
    let device = DeviceView {
        id: "d".into(),
        name: "n".into(),
        unattended_enabled: true,
    };
    let opt_in = HostSettings {
        unattended_opt_in: true,
        host_fingerprint: None,
    };
    let p = policy(true, false);
    assert_eq!(
        unattended_escalation(&VIEW, &ALL, &device, &p, &opt_in),
        Some(CONTROL)
    );
    assert_eq!(
        unattended_escalation(&CONTROL, &ALL, &device, &p, &opt_in),
        None
    );
    assert_eq!(
        unattended_escalation(&VIEW, &CLIPBOARD, &device, &p, &opt_in),
        None
    );
    assert_eq!(
        unattended_escalation(&VIEW, &ALL, &device, &p, &HostSettings::default()),
        None
    );
}

// ---- what the server says wins ------------------------------------------------------

#[tokio::test]
async fn a_session_that_disappears_on_the_server_stops_at_once() {
    let mut r = Rig::new();
    r.live_attended("s1", CONTROL, CONTROL).await;
    r.api.end_session_on_server("s1");
    r.run(15).await;
    assert!(!r.effects.is_running("s1"));
    assert!(r.mgr.sessions().is_empty());
}

#[tokio::test]
async fn a_terminal_state_stops_capture_and_input() {
    for state in ["ended", "expired", "revoked", "denied", "failed"] {
        let mut r = Rig::new();
        r.live_attended("s1", CONTROL, CONTROL).await;
        r.api.set_state("s1", state);
        r.run(15).await;
        assert!(!r.effects.is_running("s1"), "{state}");
        assert!(r.mgr.sessions().is_empty(), "{state}");
        assert_eq!(
            r.trace.count("effects:start"),
            1,
            "{state}: never restarted"
        );
    }
}

#[tokio::test]
async fn terminal_state_withdraws_open_prompts() {
    let mut r = Rig::new();
    r.api.add_request(test_session("s1", Attended, CONTROL), 5);
    r.tick().await;
    let pid = r.attended_prompts()[0].0;
    r.api.end_session_on_server("s1"); // the viewer cancelled
    r.run(15).await;
    assert!(r.ui.withdrawn().contains(&pid));
}

#[tokio::test]
async fn polling_every_15_seconds_when_busy_and_45_when_idle() {
    let mut r = Rig::new();
    r.tick().await;
    assert_eq!(r.api.poll_count(), 1);
    r.advance(Duration::from_secs(44));
    r.tick().await;
    assert_eq!(r.api.poll_count(), 1, "idle: not yet");
    r.advance(Duration::from_secs(1));
    r.tick().await;
    assert_eq!(r.api.poll_count(), 2, "idle: 45 s");

    let mut r = Rig::new();
    r.api.add_request(test_session("s1", Attended, CONTROL), 5);
    r.tick().await;
    assert_eq!(r.api.poll_count(), 1);
    assert!(r.mgr.is_busy());
    r.advance(Duration::from_secs(14));
    r.tick().await;
    assert_eq!(r.api.poll_count(), 1, "busy: not yet");
    r.advance(Duration::from_secs(1));
    r.tick().await;
    assert_eq!(r.api.poll_count(), 2, "busy: 15 s");
    assert_eq!(r.mgr.next_poll_in(), Duration::from_secs(15));
}

#[tokio::test]
async fn a_failed_poll_is_retried_at_the_normal_cadence_and_keeps_state() {
    let mut r = Rig::new();
    r.live_attended("s1", CONTROL, CONTROL).await;
    r.api.fail_next_poll(HostError::Timeout);
    r.run(15).await;
    assert_eq!(r.mgr.last_error(), Some(&HostError::Timeout));
    assert!(
        r.effects.is_running("s1"),
        "one missed poll is not a reason to stop"
    );
    r.run(15).await;
    assert_eq!(r.mgr.last_error(), None);
}

// ---- lease -------------------------------------------------------------------------

#[tokio::test]
async fn losing_the_lease_stops_capture_and_reports_device_offline_on_return() {
    let mut r = Rig::new();
    r.live_attended("s1", CONTROL, CONTROL).await;
    for _ in 0..40 {
        r.api.fail_next_poll(HostError::Network("down".into()));
    }

    r.run(115).await;
    assert!(r.effects.is_running("s1"), "still inside the 120 s lease");
    r.run(10).await;
    assert!(!r.effects.is_running("s1"), "lease lapsed: stop locally");
    assert!(
        r.api.sessions_sent().is_empty(),
        "cannot tell the server while offline"
    );

    // Connectivity returns; the server still lists the session.
    r.api.clear_poll_errors();
    r.run(15).await;
    let sent = r.api.sessions_sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(
        sent[0].action,
        SessionAction::ReportFailure {
            reason: FailureReason::DeviceOffline
        }
    );
    r.run(60).await;
    assert_eq!(r.api.sessions_sent().len(), 1, "reported once");
    assert_eq!(r.trace.count("effects:start"), 1, "never restarted");
    assert!(r.mgr.sessions().is_empty());
}

// ---- emergency disconnect --------------------------------------------------------------

#[tokio::test]
async fn emergency_disconnect_stops_everything_locally_before_telling_the_server() {
    let mut r = Rig::new();
    r.live_attended("s1", CONTROL, CONTROL).await;
    r.live_attended("s2", CONTROL, VIEW).await;
    r.api.add_request(test_session("s3", Attended, CONTROL), 5);
    r.run(15).await;
    let pending_prompt = r.attended_prompts().pop().unwrap().0;

    let errors = r.mgr.emergency_disconnect().await;
    assert!(errors.is_empty());

    let stop_all = r.trace.position("effects:stop_all").expect("stop_all");
    for api_call in [
        "api:session:end:s1",
        "api:session:end:s2",
        "api:decide:deny:s3",
    ] {
        let at = r
            .trace
            .position(api_call)
            .unwrap_or_else(|| panic!("{api_call} missing"));
        assert!(stop_all < at, "local stop must come before {api_call}");
    }
    assert!(r.effects.running().is_empty());
    assert!(r.ui.withdrawn().contains(&pending_prompt));
    for s in r.api.sessions_sent() {
        assert_eq!(
            s.action,
            SessionAction::End {
                reason: EndReason::EmergencyDisconnect
            }
        );
    }
    assert_eq!(r.api.sessions_sent().len(), 2);
    assert_eq!(r.api.decides().last().unwrap().decision, Decision::Deny);
    assert!(r.mgr.sessions().is_empty());
}

#[tokio::test]
async fn emergency_disconnect_still_holds_when_the_server_cannot_be_reached() {
    let mut r = Rig::new();
    r.live_attended("s1", CONTROL, CONTROL).await;
    r.live_attended("s2", CONTROL, CONTROL).await;
    r.api.fail_next_action(HostError::Network("down".into()));
    r.api.fail_next_action(HostError::Network("down".into()));

    let errors = r.mgr.emergency_disconnect().await;
    assert_eq!(errors.len(), 2);
    assert!(r.effects.running().is_empty(), "stopped locally regardless");

    // The server still lists both. They must never come back to life locally,
    // and the host keeps telling the server until it agrees.
    r.run(15).await;
    assert!(r.effects.running().is_empty());
    assert_eq!(r.trace.count("effects:start"), 2, "no restart");
    assert!(r.attended_prompts().len() == 2, "no new prompts either");
    r.run(30).await;
    assert!(r.api.session_view("s1").is_none());
    assert!(r.api.session_view("s2").is_none());
}

#[tokio::test]
async fn ending_one_session_from_the_banner_stops_it_then_tells_the_server() {
    let mut r = Rig::new();
    r.live_attended("s1", CONTROL, CONTROL).await;
    r.live_attended("s2", CONTROL, CONTROL).await;
    r.mgr.end_session("s1", EndReason::HostStop).await.unwrap();

    assert!(!r.effects.is_running("s1"));
    assert!(r.effects.is_running("s2"));
    assert!(
        r.trace.position("effects:stop:s1").unwrap()
            < r.trace.position("api:session:end:s1").unwrap()
    );
    assert!(r.api.session_view("s1").is_none());
    r.run(30).await;
    assert!(r.effects.is_running("s2"));
}

#[tokio::test]
async fn shutdown_ends_sessions_with_host_shutdown() {
    let mut r = Rig::new();
    r.live_attended("s1", CONTROL, CONTROL).await;
    assert!(r.mgr.shutdown().await.is_empty());
    assert_eq!(
        r.api.sessions_sent().last().unwrap().action,
        SessionAction::End {
            reason: EndReason::HostShutdown
        }
    );
}

// ---- support code ----------------------------------------------------------------------

#[tokio::test]
async fn get_help_gives_a_12_digit_code_with_a_ten_minute_countdown() {
    let mut r = Rig::new();
    assert_eq!(r.mgr.invite_seconds_left(), None);
    let info = r.mgr.get_help().await.unwrap();
    assert_eq!(info.code.len(), 12);
    assert!(info.code.bytes().all(|b| b.is_ascii_digit()));
    assert_eq!(r.mgr.invite_seconds_left(), Some(600));

    r.advance(Duration::from_secs(300));
    assert_eq!(r.mgr.invite_seconds_left(), Some(300));
    r.advance(Duration::from_secs(299));
    assert_eq!(r.mgr.invite_seconds_left(), Some(1));
    r.advance(Duration::from_secs(1));
    assert_eq!(r.mgr.invite_seconds_left(), None);
    assert!(r.mgr.invite().is_none());
}

#[tokio::test]
async fn the_support_code_countdown_follows_server_time_not_the_host_wall_clock() {
    let mut r = Rig::new();
    r.clock.jump_wall(-3600 * 1000); // the host's wall clock is an hour off
    r.tick().await; // a poll teaches the manager the offset
    let info = r.mgr.get_help().await.unwrap();
    assert_eq!(info.code.len(), 12);
    assert_eq!(r.mgr.invite_seconds_left(), Some(600));
}

// ---- revoked ----------------------------------------------------------------------------

#[tokio::test]
async fn device_revoked_ends_everything_wipes_the_key_and_says_so() {
    let mut r = Rig::new();
    r.keys.create_key().unwrap();
    r.live_attended("s1", CONTROL, CONTROL).await;
    r.api.add_request(test_session("s2", Attended, CONTROL), 5);
    r.run(15).await;
    let open_prompt = r.attended_prompts().pop().unwrap().0;

    r.api.fail_next_poll(HostError::DeviceRevoked);
    r.run(15).await;

    assert_eq!(r.mgr.state(), ManagerState::Revoked);
    assert!(r.trace.position("effects:stop_all").is_some());
    assert!(r.effects.running().is_empty());
    assert_eq!(r.keys.public_key(), Ok(None), "key wiped");
    assert_eq!(r.keys.wipe_count(), 1);
    assert!(r.ui.withdrawn().contains(&open_prompt));
    assert_eq!(
        r.ui.shown().last().unwrap().1,
        ConsentPrompt::Notice(Notice::DeviceRemoved)
    );
    assert!(r.mgr.sessions().is_empty());

    // Nothing runs afterwards: no more polls.
    let polls = r.api.poll_count();
    r.run(120).await;
    assert_eq!(r.api.poll_count(), polls);
}

#[tokio::test]
async fn device_revoked_during_an_action_is_handled_the_same_way() {
    let mut r = Rig::new();
    r.api.add_request(test_session("s1", Attended, CONTROL), 5);
    r.tick().await;
    let pid = r.attended_prompts()[0].0;
    r.api.fail_next_action(HostError::DeviceRevoked);
    r.ui.answer(pid, approve(CONTROL, 30));
    r.tick().await;
    assert_eq!(r.mgr.state(), ManagerState::Revoked);
    assert_eq!(r.keys.wipe_count(), 1);
    assert!(r.effects.running().is_empty());
}

#[tokio::test]
async fn device_revoked_while_creating_a_support_code() {
    let mut r = Rig::new();
    r.api.fail_next_action(HostError::DeviceRevoked);
    assert_eq!(
        r.mgr.get_help().await.unwrap_err(),
        HostError::DeviceRevoked
    );
    assert_eq!(r.mgr.state(), ManagerState::Revoked);
    assert_eq!(r.keys.wipe_count(), 1);
}

// ---- outbound actions --------------------------------------------------------------------

#[tokio::test]
async fn a_retryable_failure_is_sent_again_on_the_next_tick() {
    let mut r = Rig::new();
    r.api.add_request(test_session("s1", Attended, CONTROL), 5);
    r.tick().await;
    let pid = r.attended_prompts()[0].0;
    r.api.fail_next_action(HostError::Timeout);
    r.ui.answer(pid, approve(CONTROL, 30));
    r.tick().await;
    assert_eq!(r.api.decides().len(), 1);
    assert!(!r.effects.is_running("s1"));

    // Not hammered: nothing is sent again until the retry delay has passed.
    r.tick().await;
    assert_eq!(r.api.decides().len(), 1);
    r.advance(Duration::from_secs(2));
    r.tick().await;
    assert_eq!(r.api.decides().len(), 2);
    assert!(r.effects.is_running("s1"));
}

#[tokio::test]
async fn a_rejected_action_is_not_repeated() {
    let mut r = Rig::new();
    r.api.add_request(test_session("s1", Attended, CONTROL), 5);
    r.tick().await;
    let pid = r.attended_prompts()[0].0;
    r.api.fail_next_action(HostError::Api {
        status: 409,
        code: Some("STATE_CONFLICT".into()),
        message: "Session is expired".into(),
    });
    r.ui.answer(pid, approve(CONTROL, 30));
    r.run(60).await;
    assert_eq!(r.api.decides().len(), 1, "409 is final");
}

#[tokio::test]
async fn retrying_gives_up_after_a_few_attempts() {
    let mut r = Rig::new();
    r.api.add_request(test_session("s1", Attended, CONTROL), 5);
    r.tick().await;
    let pid = r.attended_prompts()[0].0;
    for _ in 0..10 {
        r.api.fail_next_action(HostError::Timeout);
    }
    r.ui.answer(pid, approve(CONTROL, 30));
    r.run(60).await;
    assert_eq!(r.api.decides().len(), 3);
}

#[tokio::test]
async fn answers_to_withdrawn_prompts_are_ignored() {
    let mut r = Rig::new();
    r.api.add_request(test_session("s1", Attended, CONTROL), 5);
    r.tick().await;
    let pid = r.attended_prompts()[0].0;
    r.api.end_session_on_server("s1");
    r.run(15).await;
    r.ui.answer(pid, approve(ALL, 480)); // a late click on a gone prompt
    r.tick().await;
    assert!(r.api.decides().is_empty());
}

// ---- observation ----------------------------------------------------------------------------

#[tokio::test]
async fn snapshots_report_consent_time_left_for_the_banner_and_tray() {
    let mut r = Rig::new();
    r.live_attended("s1", CONTROL, CONTROL).await;
    r.run(10 * 60).await;
    let snap = r.mgr.sessions();
    assert_eq!(snap.len(), 1);
    assert_eq!(snap[0].requester_name, "Sam Helper");
    assert_eq!(snap[0].granted, CONTROL);
    assert!(snap[0].running);
    let left = snap[0].consent_remaining.unwrap();
    assert!(
        (Duration::from_secs(19 * 60)..=Duration::from_secs(20 * 60)).contains(&left),
        "{left:?}"
    );
}

#[tokio::test]
async fn update_required_is_exposed() {
    let mut r = Rig::new();
    assert!(!r.mgr.update_required());
    r.api.set_update_required(true);
    r.run(45).await;
    assert!(r.mgr.update_required());
}

#[tokio::test]
async fn every_request_the_manager_sends_is_acceptable_to_the_server_model() {
    // The fake server enforces the real server's rules (subset, unattended,
    // microphone). If the manager ever sent something the server would
    // refuse, the action would error and last_error would be set.
    let mut r = Rig::unattended();
    r.api.set_policy(policy(true, true));
    r.api.add_request(test_session("u1", Unattended, ALL), 5);
    r.api.add_request(test_session("s1", Attended, ALL), 5);
    r.tick().await;
    let pid = r.attended_prompts()[0].0;
    r.ui.answer(pid, approve(ALL, 480));
    r.run(60).await;
    assert_eq!(r.mgr.last_error(), None);
    assert!(r.effects.is_running("u1") && r.effects.is_running("s1"));
    assert!(matches!(r.api.requests().first(), Some(Recorded::Poll)));
}

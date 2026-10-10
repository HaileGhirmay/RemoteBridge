//! The whole host on one machine.
//!
//! Real: session manager, indicators, media supervisor, WebRTC, signaling
//! relay. Faked: the control plane (a model of the server), the screen, the
//! keyboard and mouse, the banner, the tray and the shortcuts.

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rb_core::Permissions;
use rb_core::fakes::{
    FakeAudioCapture, FakeBanner, FakeCapture, FakeClipboard, FakeClock, FakeConsentUi,
    FakeHostApi, FakeHotkeys, FakeInputInjector, FakeKeyStore, FakeMicCapture, FakeTray, InputCall,
    Recorded, Trace, test_session,
};
use rb_core::indicators::{IndicatorController, MemoryIndicatorStore};
use rb_core::protocol::types::{
    BannerEventType, BannerReason, EndReason, FailureReason, MediaCredentialsResponse,
    SessionAction, SessionMode,
};
use rb_core::session::{HostSettings, SessionManager};
use rb_core::traits::{
    Banner, BannerAction, ConsentAnswer, ConsentPrompt, HideChoice, HotkeyEvent, Notice,
    SystemClock, TrayAction,
};
use rb_core::types::DisplayId;
use rb_host::{HostRuntime, SharedClipboard, SharedInjector, SupervisorConfig, spawn_supervisor};
use rb_media::HostIdentity;
use rb_media::audio::SilentOpusEncoder;
use rb_media::testing::{LocalSignaling, ScriptedViewer};
use rb_media::video::OpenH264Encoder;
use serde_json::json;
use tokio::sync::oneshot;

const SID: &str = "6f1c3d7e-2b4a-4c8e-9d10-0a1b2c3d4e5f";
const CONTROL: Permissions = Permissions {
    control: true,
    ..Permissions::NONE
};

struct Rig {
    api: FakeHostApi,
    ui: FakeConsentUi,
    banner: FakeBanner,
    tray: FakeTray,
    hotkeys: FakeHotkeys,
    capture: FakeCapture,
    input: FakeInputInjector,
    clipboard: FakeClipboard,
    trace: Trace,
    viewer: ScriptedViewer,
    host_fingerprint: String,
    settings_seen: Arc<Mutex<Vec<bool>>>,
    _shutdown: oneshot::Sender<()>,
    _task: tokio::task::JoinHandle<()>,
}

struct Options {
    /// What the server answers to `host/media_credentials`.
    credentials: Credentials,
    capture_error: Option<rb_core::PlatformError>,
}

enum Credentials {
    Good,
    Unconfigured,
    ViewerNotBound,
}

impl Options {
    fn normal() -> Self {
        Self {
            credentials: Credentials::Good,
            capture_error: None,
        }
    }
}

async fn rig(options: Options) -> Rig {
    let signaling = LocalSignaling::start().await;
    let host_id = HostIdentity::generate().unwrap();
    let viewer_id = HostIdentity::generate().unwrap();
    let viewer = ScriptedViewer::connect(&signaling, SID, &viewer_id, host_id.fingerprint()).await;

    let trace = Trace::new();
    let api = FakeHostApi::with_trace(FakeClock::new(1_791_462_818_897), trace.clone());
    api.set_media_credentials(match options.credentials {
        Credentials::Unconfigured => None,
        Credentials::Good | Credentials::ViewerNotBound => Some(MediaCredentialsResponse {
            signaling_url: signaling.url.clone(),
            signaling_token: signaling.token(
                SID,
                "host",
                host_id.fingerprint(),
                viewer_id.fingerprint(),
            ),
            token_expires_at: 1_791_463_118_897,
            ice_servers: vec![],
            turn_available: false,
            relay_allowed: true,
            peer_fingerprint: matches!(options.credentials, Credentials::Good)
                .then(|| viewer_id.fingerprint().to_owned()),
        }),
    });
    api.add_request(
        test_session(
            SID,
            SessionMode::Attended,
            Permissions {
                control: true,
                clipboard: true,
                ..Permissions::NONE
            },
        ),
        5,
    );

    let capture = FakeCapture::with_trace(trace.clone());
    capture.set_auto_frames(Some((320, 180)));
    capture.fail_start_with(options.capture_error);
    let audio = FakeAudioCapture::new();
    let mic = FakeMicCapture::new();
    let input = FakeInputInjector::new();
    let clipboard = FakeClipboard::new();

    let (effects, events, _supervisor) = spawn_supervisor(SupervisorConfig {
        identity: host_id.clone(),
        input: SharedInjector::new(Box::new(input.clone())),
        clipboard: SharedClipboard::new(Box::new(clipboard.clone())),
        capture: Box::new(capture.clone()),
        system_audio: Some(Box::new(audio)),
        microphone: Some(Box::new(mic)),
        display: DisplayId(1),
        udp_ip: "127.0.0.1".into(),
        video_encoder: Arc::new(|| Box::new(OpenH264Encoder::default())),
        audio_encoder: Arc::new(|| Box::new(SilentOpusEncoder::default())),
        credential_attempts: 3,
        credential_retry: Duration::from_millis(200),
    });

    let ui = FakeConsentUi::new();
    let banner = FakeBanner::new();
    let tray = FakeTray::new();
    let hotkeys = FakeHotkeys::new();
    let clock = Arc::new(SystemClock::new());
    let manager = SessionManager::new(
        api.clone(),
        clock.clone(),
        Box::new(ui.clone()),
        Box::new(effects),
        Arc::new(FakeKeyStore::new()),
        HostSettings {
            unattended_opt_in: false,
            host_fingerprint: Some(host_id.fingerprint().to_owned()),
        },
    );
    let indicators = IndicatorController::new(Box::new(MemoryIndicatorStore::new()), clock);
    let runtime = HostRuntime::new(
        manager,
        indicators,
        Box::new(banner.clone()),
        Box::new(tray.clone()),
        Box::new(hotkeys.clone()),
        events,
    );
    let settings_seen = Arc::new(Mutex::new(Vec::new()));
    let seen = settings_seen.clone();
    let mut runtime = runtime.on_settings_changed(Box::new(move |s: &HostSettings| {
        seen.lock().unwrap().push(s.unattended_opt_in);
    }));
    let (shutdown, stop) = oneshot::channel::<()>();
    let task = tokio::spawn(async move {
        runtime.startup().await;
        runtime
            .run_until(async {
                let _ = stop.await;
            })
            .await;
    });

    Rig {
        api,
        ui,
        banner,
        tray,
        hotkeys,
        capture,
        input,
        clipboard,
        trace,
        viewer,
        host_fingerprint: host_id.fingerprint().to_owned(),
        settings_seen,
        _shutdown: shutdown,
        _task: task,
    }
}

async fn eventually(what: &str, check: impl Fn() -> bool) {
    for _ in 0..400 {
        if check() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("timed out waiting for: {what}");
}

impl Rig {
    /// Wait for the approval prompt and approve with `grant`.
    async fn approve(&self, grant: Permissions) {
        eventually("the approval prompt", || {
            self.ui
                .shown()
                .iter()
                .any(|(_, p)| matches!(p, ConsentPrompt::AttendedRequest { .. }))
        })
        .await;
        let (pid, _) = self
            .ui
            .shown()
            .into_iter()
            .find(|(_, p)| matches!(p, ConsentPrompt::AttendedRequest { .. }))
            .unwrap();
        self.ui.answer(
            pid,
            ConsentAnswer::Approve {
                grant,
                approved_minutes: 30,
            },
        );
    }

    async fn streaming(&self) {
        let packets = self.viewer.state.video_packets.clone();
        eventually("the viewer to receive video", || {
            packets.load(Ordering::SeqCst) >= 5
        })
        .await;
        eventually("the server to see the session active", || {
            self.api
                .session_view(SID)
                .is_some_and(|v| v.state == "active")
        })
        .await;
    }

    fn banner_reports(&self) -> Vec<rb_core::protocol::types::BannerReportRequest> {
        self.api
            .requests()
            .into_iter()
            .filter_map(|r| match r {
                Recorded::BannerReport(b) => Some(b),
                _ => None,
            })
            .collect()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_support_request_flows_all_the_way_to_streaming_video() {
    let r = rig(Options::normal()).await;
    // Nothing runs until the person at the machine says yes.
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert!(
        !r.capture.is_running(),
        "capture must not start before approval"
    );
    assert!(r.api.decides().is_empty());

    r.approve(CONTROL).await;
    r.streaming().await;

    // What the server was told: the grant, and the DTLS fingerprint made before approval.
    let decide = &r.api.decides()[0];
    assert_eq!(decide.grant, Some(CONTROL));
    assert_eq!(
        decide.host_fingerprint.as_deref(),
        Some(r.host_fingerprint.as_str())
    );
    assert!(r.capture.is_running());
    assert_eq!(r.capture.start_count(), 1);
    let connected = r
        .api
        .sessions_sent()
        .into_iter()
        .find_map(|s| match s.action {
            SessionAction::MediaConnected { relay_used } => Some(relay_used),
            _ => None,
        });
    assert_eq!(connected, Some(false), "direct connection on loopback");

    // The indicators are up while someone can see the screen.
    assert!(r.banner.is_visible());
    assert!(r.tray.model().session_live);

    // Control was granted, clipboard was not.
    r.viewer
        .send(
            "control",
            json!({"t": "key", "code": "KeyA", "key": "a", "down": true, "repeat": false}),
        )
        .await;
    r.viewer
        .send("control", json!({"t": "clip", "text": "not allowed"}))
        .await;
    eventually("the key to arrive", || {
        r.input.calls().contains(&InputCall::Key {
            code: "KeyA".into(),
            down: true,
        })
    })
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(r.clipboard.write_count(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_emergency_shortcut_stops_capture_before_the_server_is_told_and_injected_presses_do_nothing()
 {
    let r = rig(Options::normal()).await;
    r.approve(CONTROL).await;
    r.streaming().await;

    // A remote user "types" the shortcut: refused.
    r.hotkeys.press_injected(HotkeyEvent::EmergencyDisconnect);
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert!(
        r.capture.is_running(),
        "an injected shortcut must not disconnect"
    );
    assert!(r.api.session_view(SID).is_some());

    // The person at the keyboard presses it.
    r.hotkeys.press_physical(HotkeyEvent::EmergencyDisconnect);
    eventually("the server to end the session", || {
        r.api.session_view(SID).is_none()
    })
    .await;

    assert!(!r.capture.is_running(), "capture stopped");
    let stop = r.trace.position("capture:stop").expect("local stop");
    let told = r.trace.position("api:session:end").expect("server told");
    assert!(stop < told, "local stop first, then the server");
    let ended = r
        .api
        .sessions_sent()
        .into_iter()
        .find_map(|s| match s.action {
            SessionAction::End { reason } => Some(reason),
            _ => None,
        });
    assert_eq!(ended, Some(EndReason::EmergencyDisconnect));
    assert!(r.input.calls().contains(&InputCall::ReleaseAll));

    // No more video reaches the viewer.
    let packets = r.viewer.state.video_packets.clone();
    tokio::time::sleep(Duration::from_millis(500)).await;
    let before = packets.load(Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert!(
        packets.load(Ordering::SeqCst) <= before + 1,
        "video kept flowing after the disconnect"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hiding_and_restoring_the_indicators_is_local_and_reported() {
    let r = rig(Options::normal()).await;
    r.approve(CONTROL).await;
    r.streaming().await;
    assert!(r.banner.is_visible());

    // The person at the machine hides everything for 30 minutes.
    r.banner.click(BannerAction::Hide(HideChoice::Minutes(30)));
    eventually("the banner to hide", || !r.banner.is_visible()).await;
    assert!(
        !r.tray.model().visible,
        "an attended session hides the tray too"
    );
    assert!(
        r.ui.shown()
            .iter()
            .any(|(_, p)| matches!(p, ConsentPrompt::Notice(Notice::FirstHide { .. }))),
        "the first time ever, the user is told how to bring it back"
    );
    eventually("the dismissal to be reported", || {
        r.banner_reports().iter().any(|b| {
            b.event
                .as_ref()
                .is_some_and(|e| e.kind == BannerEventType::Dismissed)
        })
    })
    .await;
    // The media keeps running: hiding an indicator never touches authorization.
    assert!(r.capture.is_running());

    // The shortcut typed by software does nothing; the real one restores.
    r.hotkeys.press_injected(HotkeyEvent::RestoreIndicators);
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert!(!r.banner.is_visible());
    r.hotkeys.press_physical(HotkeyEvent::RestoreIndicators);
    eventually("the banner to return", || r.banner.is_visible()).await;
    assert!(r.tray.model().visible);
    eventually("the restore to be reported", || {
        r.banner_reports().iter().any(|b| {
            b.event.as_ref().is_some_and(|e| {
                e.kind == BannerEventType::Restored && e.reason == Some(BannerReason::UserShowNow)
            })
        })
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn missing_screen_permission_is_reported_not_worked_around() {
    let r = rig(Options {
        credentials: Credentials::Good,
        capture_error: Some(rb_core::PlatformError::PermissionDenied("screen recording")),
    })
    .await;
    r.approve(CONTROL).await;
    eventually("permissions_unavailable to be reported", || {
        r.api.sessions_sent().iter().any(|s| {
            s.action
                == SessionAction::ReportFailure {
                    reason: FailureReason::PermissionsUnavailable,
                }
        })
    })
    .await;
    assert!(!r.capture.is_running());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unconfigured_media_service_ends_in_a_reported_failure_without_streaming() {
    let r = rig(Options {
        credentials: Credentials::Unconfigured,
        capture_error: None,
    })
    .await;
    r.approve(CONTROL).await;
    eventually("a failure to be reported", || {
        r.api.sessions_sent().iter().any(|s| {
            s.action
                == SessionAction::ReportFailure {
                    reason: FailureReason::IceFailure,
                }
        })
    })
    .await;
    assert_eq!(r.viewer.state.video_packets.load(Ordering::SeqCst), 0);
    assert!(
        r.trace.count("api:media_credentials") >= 2,
        "it retried before giving up"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_host_will_not_connect_to_a_viewer_the_server_has_not_vouched_for() {
    let r = rig(Options {
        credentials: Credentials::ViewerNotBound,
        capture_error: None,
    })
    .await;
    r.approve(CONTROL).await;
    eventually("a failure to be reported", || {
        r.api
            .sessions_sent()
            .iter()
            .any(|s| matches!(s.action, SessionAction::ReportFailure { .. }))
    })
    .await;
    assert_eq!(r.viewer.state.video_packets.load(Ordering::SeqCst), 0);
    assert!(
        r.viewer.state.tracks.lock().await.is_empty(),
        "no media track was ever offered to an unverified viewer"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_tray_can_share_this_computer_and_shows_the_code() {
    let r = rig(Options::normal()).await;
    // The countdown follows server time, which the host learns from its first
    // poll; a real host has polled long before anyone opens the tray menu.
    eventually("the first poll", || r.trace.count("api:poll") >= 1).await;
    r.tray.click(TrayAction::ShareThisComputer);
    eventually("the support code to be shown", || {
        r.ui.shown()
            .iter()
            .any(|(_, p)| matches!(p, ConsentPrompt::Notice(Notice::SupportCode { .. })))
    })
    .await;
    let shown = r.ui.shown();
    let Some((_, ConsentPrompt::Notice(Notice::SupportCode { code, minutes_left }))) = shown
        .iter()
        .find(|(_, p)| matches!(p, ConsentPrompt::Notice(Notice::SupportCode { .. })))
    else {
        unreachable!("found above");
    };
    assert_eq!(code.len(), 12, "a 12-digit support code");
    assert!(code.bytes().all(|b| b.is_ascii_digit()));
    assert!(
        (9..=10).contains(minutes_left),
        "valid for ten minutes, got {minutes_left}"
    );
    // Nothing about sharing touches the indicators or any session.
    assert!(!r.banner.is_visible());
    assert!(r.tray.model().visible);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_tray_can_switch_unattended_opt_in_and_the_host_program_is_told() {
    let r = rig(Options::normal()).await;
    assert!(
        !r.tray.model().unattended_opt_in,
        "off until chosen locally"
    );

    r.tray.click(TrayAction::SetUnattendedOptIn(true));
    eventually("the tray to show unattended access allowed", || {
        r.tray.model().unattended_opt_in
    })
    .await;
    assert_eq!(*r.settings_seen.lock().unwrap(), vec![true]);

    // Repeating the same choice changes nothing and is not reported again.
    r.tray.click(TrayAction::SetUnattendedOptIn(true));
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(*r.settings_seen.lock().unwrap(), vec![true]);

    r.tray.click(TrayAction::SetUnattendedOptIn(false));
    eventually("the tray to show it off again", || {
        !r.tray.model().unattended_opt_in
    })
    .await;
    assert_eq!(*r.settings_seen.lock().unwrap(), vec![true, false]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn quit_from_the_tray_ends_every_session_and_stops_the_host() {
    let r = rig(Options::normal()).await;
    r.approve(CONTROL).await;
    r.streaming().await;

    r.tray.click(TrayAction::Quit);
    eventually("capture to stop", || !r.capture.is_running()).await;
    eventually("the host loop to end", || r._task.is_finished()).await;
    let sent = r.api.sessions_sent();
    let last = sent.last().expect("the server was told");
    assert_eq!(
        last.action,
        SessionAction::End {
            reason: EndReason::HostShutdown
        },
        "quitting is an orderly shutdown, not an emergency disconnect"
    );
}

//! End to end on one machine: the real signaling server, the real host media
//! session, and a scripted viewer built from a second WebRTC peer. No browser
//! and no network beyond loopback.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use rb_core::Permissions;
use rb_core::fakes::{FakeClipboard, FakeInputInjector, InputCall};
use rb_core::protocol::types::FailureReason;
use rb_core::types::{AudioChunk, FrameData, MouseButton, VideoFrame};
use rb_media::audio::SilentOpusEncoder;
use rb_media::testing::{LocalSignaling, ScriptedViewer};
use rb_media::video::OpenH264Encoder;
use rb_media::{
    ChannelInfo, HostIdentity, MediaCredentials, MediaEvent, MediaSession, MediaSessionConfig,
};
use serde_json::{Value, json};

const WAIT: Duration = Duration::from_secs(25);
const SID: &str = "6f1c3d7e-2b4a-4c8e-9d10-0a1b2c3d4e5f";

struct Host {
    session: MediaSession,
    input: FakeInputInjector,
    clipboard: FakeClipboard,
}

async fn start_host(
    signaling: &LocalSignaling,
    sid: &str,
    host: &HostIdentity,
    viewer_fp: &str,
    expected_viewer_fp: &str,
    grant: Permissions,
) -> Host {
    let input = FakeInputInjector::new();
    let clipboard = FakeClipboard::new();
    let session = MediaSession::start(MediaSessionConfig {
        credentials: MediaCredentials {
            signaling_url: signaling.url.clone(),
            signaling_token: signaling.token(sid, "host", host.fingerprint(), viewer_fp),
            ice_servers: vec![],
            peer_fingerprint: expected_viewer_fp.to_owned(),
        },
        identity: host.clone(),
        grant,
        udp_ip: "127.0.0.1".into(),
        input: Box::new(input.clone()),
        clipboard: Box::new(clipboard.clone()),
        video_encoder: Box::new(OpenH264Encoder::default()),
        audio_encoder: Arc::new(|| Box::new(SilentOpusEncoder::default())),
    })
    .await
    .expect("host session starts");
    Host {
        session,
        input,
        clipboard,
    }
}

async fn wait_for<F: Fn(&MediaEvent) -> bool>(session: &mut MediaSession, pred: F) -> MediaEvent {
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(left, session.next_event()).await {
            Ok(Some(event)) if pred(&event) => return event,
            Ok(Some(_)) => {}
            other => panic!("expected a media event, got {other:?}"),
        }
    }
}

fn frame(shade: u8) -> VideoFrame {
    let (w, h) = (320u32, 180u32);
    let mut px = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            px.extend_from_slice(&[
                shade.wrapping_add((x % 200) as u8),
                (y % 200) as u8,
                shade,
                255,
            ]);
        }
    }
    VideoFrame {
        width: w,
        height: h,
        timestamp_us: 0,
        data: FrameData::Bgra(px),
    }
}

fn pcm_20ms() -> AudioChunk {
    AudioChunk {
        sample_rate: 48_000,
        channels: 2,
        timestamp_us: 0,
        samples: vec![0; 960 * 2],
    }
}

/// Keep feeding video until `check` is true or time runs out.
async fn feed_video_until(host: &Host, check: impl Fn() -> bool) -> bool {
    let feed = host.session.feed();
    for i in 0..400u32 {
        if check() {
            return true;
        }
        let _ = feed.push_video(&frame((i % 250) as u8)).await;
        tokio::time::sleep(Duration::from_millis(45)).await;
    }
    check()
}

/// A connected pair, ready for tests.
async fn connected(grant: Permissions) -> (Host, ScriptedViewer, LocalSignaling) {
    let signaling = LocalSignaling::start().await;
    let host_id = HostIdentity::generate().unwrap();
    let viewer_id = HostIdentity::generate().unwrap();
    let mut host = start_host(
        &signaling,
        SID,
        &host_id,
        viewer_id.fingerprint(),
        viewer_id.fingerprint(),
        grant,
    )
    .await;
    let viewer = ScriptedViewer::connect(&signaling, SID, &viewer_id, host_id.fingerprint()).await;
    let event = wait_for(&mut host.session, |e| {
        matches!(e, MediaEvent::Connected { .. })
    })
    .await;
    assert_eq!(event, MediaEvent::Connected { relay_used: false });
    viewer.wait_open("control").await;
    viewer.wait_open("pointer").await;
    (host, viewer, signaling)
}

const CONTROL: Permissions = Permissions {
    control: true,
    ..Permissions::NONE
};
const CLIPBOARD: Permissions = Permissions {
    clipboard: true,
    ..Permissions::NONE
};

fn key(code: &str, down: bool) -> Value {
    json!({"t": "key", "code": code, "key": "x", "down": down, "repeat": false})
}

async fn settle() {
    tokio::time::sleep(Duration::from_millis(400)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn host_offers_connects_and_creates_both_channels_with_the_right_options() {
    let (host, viewer, _) = connected(Permissions::NONE).await;

    // What the host created. (The receiving side of this WebRTC library does
    // not report maxRetransmits for inbound channels, so the host's own view
    // is the one to check; browsers read it from the channel-open message.)
    let mut info = host.session.channel_info().await.unwrap();
    info.sort_by(|a, b| a.label.cmp(&b.label));
    assert_eq!(
        info,
        vec![
            ChannelInfo {
                label: "control".into(),
                ordered: true,
                max_retransmits: None
            },
            ChannelInfo {
                label: "pointer".into(),
                ordered: false,
                max_retransmits: Some(0)
            },
        ]
    );
    // And the ordering survives to the viewer.
    assert!(viewer.channel("control").await.ordered().await.unwrap());
    assert!(!viewer.channel("pointer").await.ordered().await.unwrap());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn video_reaches_the_viewer_as_one_h264_track() {
    let (host, viewer, _) = connected(Permissions::NONE).await;
    let received = viewer.state.video_packets.clone();
    assert!(
        feed_video_until(&host, || received.load(Ordering::SeqCst) >= 5).await,
        "the viewer received {} video packets",
        received.load(Ordering::SeqCst)
    );
    let tracks = viewer.state.tracks.lock().await.clone();
    assert_eq!(
        tracks.len(),
        1,
        "only video while nothing else is granted: {tracks:?}"
    );
    assert!(tracks[0].0.contains("Video"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn view_only_drops_everything_and_grants_take_effect_immediately() {
    let (host, viewer, _) = connected(Permissions::NONE).await;

    // View only: nothing reaches the machine.
    viewer.send("control", key("KeyA", true)).await;
    viewer
        .send("control", json!({"t": "btn", "b": 0, "down": true}))
        .await;
    viewer
        .send("control", json!({"t": "wheel", "dx": 0, "dy": -120}))
        .await;
    viewer
        .send("control", json!({"t": "clip", "text": "secret"}))
        .await;
    viewer
        .send("pointer", json!({"t": "move", "x": 0.5, "y": 0.5}))
        .await;
    settle().await;
    assert!(
        host.input.calls().is_empty(),
        "no input without control: {:?}",
        host.input.calls()
    );
    assert_eq!(host.clipboard.write_count(), 0);
    assert!(host.session.filter_stats().dropped_control >= 4);
    assert!(host.session.filter_stats().dropped_clipboard >= 1);

    // Control granted: keys, buttons, wheel and pointer work; clipboard still does not.
    host.session.set_grant(CONTROL).unwrap();
    viewer.send("control", key("KeyA", true)).await;
    viewer.send("control", key("KeyA", false)).await;
    viewer
        .send("control", json!({"t": "btn", "b": 2, "down": true}))
        .await;
    viewer
        .send("pointer", json!({"t": "move", "x": 0.25, "y": 0.75}))
        .await;
    viewer
        .send("control", json!({"t": "clip", "text": "still blocked"}))
        .await;
    settle().await;
    let calls = host.input.calls();
    assert!(
        calls.contains(&InputCall::Key {
            code: "KeyA".into(),
            down: true
        }),
        "{calls:?}"
    );
    assert!(calls.contains(&InputCall::Key {
        code: "KeyA".into(),
        down: false
    }));
    assert!(calls.contains(&InputCall::Button {
        button: MouseButton::Right,
        down: true
    }));
    assert!(calls.contains(&InputCall::Move { x: 0.25, y: 0.75 }));
    assert_eq!(
        host.clipboard.write_count(),
        0,
        "clipboard needs its own grant"
    );

    // Clipboard granted too.
    host.session
        .set_grant(Permissions {
            control: true,
            clipboard: true,
            ..Permissions::NONE
        })
        .unwrap();
    viewer
        .send("control", json!({"t": "clip", "text": "now allowed"}))
        .await;
    settle().await;
    assert_eq!(host.clipboard.text().as_deref(), Some("now allowed"));

    // Control revoked: held keys are released and the next key is dropped at once.
    let before = host.input.calls().len();
    host.session.set_grant(CLIPBOARD).unwrap();
    viewer.send("control", key("KeyB", true)).await;
    settle().await;
    let calls = host.input.calls();
    assert!(calls[before..].contains(&InputCall::ReleaseAll));
    assert!(!calls.contains(&InputCall::Key {
        code: "KeyB".into(),
        down: true
    }));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn each_message_must_arrive_on_its_own_channel() {
    let (host, viewer, _) = connected(CONTROL).await;
    // `move` belongs on pointer; key belongs on control.
    viewer
        .send("control", json!({"t": "move", "x": 0.1, "y": 0.1}))
        .await;
    viewer.send("pointer", key("KeyZ", true)).await;
    settle().await;
    assert!(host.input.calls().is_empty(), "{:?}", host.input.calls());
    // The right channel works.
    viewer
        .send("pointer", json!({"t": "move", "x": 0.1, "y": 0.1}))
        .await;
    settle().await;
    assert_eq!(host.input.calls(), vec![InputCall::Move { x: 0.1, y: 0.1 }]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn malformed_binary_and_oversize_messages_are_dropped() {
    let (host, viewer, _) = connected(Permissions {
        control: true,
        clipboard: true,
        ..Permissions::NONE
    })
    .await;
    let control = viewer.channel("control").await;
    control.send_text("not json").await.unwrap();
    control
        .send(bytes::BytesMut::from(&b"\x00\x01\x02"[..]))
        .await
        .unwrap();
    control
        .send_text(&json!({"t": "clip", "text": "a".repeat(70_000)}).to_string())
        .await
        .unwrap();
    viewer.send("control", key("Enter", true)).await; // proves the channel still works
    settle().await;
    let stats = host.session.filter_stats();
    assert!(stats.dropped_malformed >= 2, "{stats:?}");
    assert_eq!(
        host.clipboard.write_count(),
        0,
        "oversize clipboard never lands"
    );
    assert_eq!(
        host.input.calls(),
        vec![InputCall::Key {
            code: "Enter".into(),
            down: true
        }]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn host_to_viewer_clipboard_only_flows_while_granted() {
    let (host, viewer, _) = connected(Permissions::NONE).await;
    host.session.send_clipboard("must not leave").unwrap();
    settle().await;
    assert!(viewer.state.host_messages.lock().await.is_empty());

    host.session.set_grant(CLIPBOARD).unwrap();
    host.session.send_clipboard("hello viewer").unwrap();
    settle().await;
    let got = viewer.state.host_messages.lock().await.clone();
    assert_eq!(got.len(), 1, "{got:?}");
    let v: Value = serde_json::from_str(&got[0]).unwrap();
    assert_eq!(v, json!({"t": "clip", "text": "hello viewer"}));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn audio_tracks_exist_only_while_granted_and_stop_when_revoked() {
    let (host, viewer, _) = connected(Permissions::NONE).await;
    let feed = host.session.feed();
    // Pushing audio without a grant goes nowhere.
    feed.push_system_audio(&pcm_20ms()).await.unwrap();
    feed.push_microphone(&pcm_20ms()).await.unwrap();

    host.session
        .set_grant(Permissions {
            system_audio: true,
            ..Permissions::NONE
        })
        .unwrap();
    // A second track appears after renegotiation. (A remote track is announced
    // when its first packet arrives, so keep sending audio while waiting.)
    let mut system_track = None;
    for _ in 0..300 {
        let _ = feed.push_system_audio(&pcm_20ms()).await;
        let tracks = viewer.state.tracks.lock().await.clone();
        if let Some(t) = tracks.iter().find(|t| t.0.contains("Audio")) {
            system_track = Some(t.1.clone());
            break;
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
    let system_track = system_track.expect("an audio track after the grant");
    assert_eq!(system_track, "system-audio");

    let received = viewer.state.audio_packets.clone();
    let mut got = false;
    for _ in 0..200 {
        let _ = feed.push_system_audio(&pcm_20ms()).await;
        tokio::time::sleep(Duration::from_millis(20)).await;
        if received.get(&system_track) >= 5 {
            got = true;
            break;
        }
    }
    assert!(
        got,
        "system audio reached the viewer: {}",
        received.get(&system_track)
    );

    // The microphone is separate and still off.
    assert_eq!(received.get("microphone"), 0);

    // Revoke: the track stops at once.
    host.session.set_grant(Permissions::NONE).unwrap();
    settle().await;
    let after_revoke = received.get(&system_track);
    for _ in 0..30 {
        let _ = feed.push_system_audio(&pcm_20ms()).await;
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    settle().await;
    assert!(
        received.get(&system_track) <= after_revoke + 2,
        "audio kept flowing after the permission was removed ({} -> {})",
        after_revoke,
        received.get(&system_track)
    );

    // The microphone is its own opt-in with its own track.
    host.session
        .set_grant(Permissions {
            microphone: true,
            ..Permissions::NONE
        })
        .unwrap();
    let mut mic_packets = 0;
    for _ in 0..300 {
        let _ = feed.push_microphone(&pcm_20ms()).await;
        tokio::time::sleep(Duration::from_millis(40)).await;
        mic_packets = received.get("microphone");
        if mic_packets >= 3 {
            break;
        }
    }
    assert!(
        mic_packets >= 3,
        "microphone audio reached the viewer ({mic_packets})"
    );
    let tracks = viewer.state.tracks.lock().await.clone();
    assert!(tracks.iter().any(|t| t.1 == "microphone"), "{tracks:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_viewer_with_the_wrong_fingerprint_is_dropped() {
    let signaling = LocalSignaling::start().await;
    let sid = "11111111-2222-4333-8444-555555555555";
    let host_id = HostIdentity::generate().unwrap();
    let viewer_id = HostIdentity::generate().unwrap();
    let impostor_expected = HostIdentity::generate().unwrap();
    // The server vouched for a different viewer than the one that answers.
    let mut host = start_host(
        &signaling,
        sid,
        &host_id,
        viewer_id.fingerprint(),
        impostor_expected.fingerprint(),
        Permissions::NONE,
    )
    .await;
    let viewer = ScriptedViewer::connect(&signaling, sid, &viewer_id, host_id.fingerprint()).await;

    let event = wait_for(&mut host.session, |e| {
        matches!(e, MediaEvent::Failed(_) | MediaEvent::Connected { .. })
    })
    .await;
    assert_eq!(event, MediaEvent::Failed(FailureReason::IceFailure));
    // And no media ever flows.
    let received = viewer.state.video_packets.clone();
    let _ = host.session.feed().push_video(&frame(1)).await;
    settle().await;
    assert_eq!(received.load(Ordering::SeqCst), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stopping_the_session_stops_the_media_and_releases_input() {
    let (host, viewer, _) = connected(CONTROL).await;
    let received = viewer.state.video_packets.clone();
    assert!(feed_video_until(&host, || received.load(Ordering::SeqCst) >= 3).await);

    viewer.send("control", key("ShiftLeft", true)).await;
    settle().await;
    let input = host.input.clone();
    let feed = host.session.feed();
    host.session.stop().await;

    assert!(
        input.calls().contains(&InputCall::ReleaseAll),
        "held keys are released on stop"
    );
    let before = received.load(Ordering::SeqCst);
    for i in 0..10u8 {
        let _ = feed.push_video(&frame(i)).await;
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
    settle().await;
    assert!(
        received.load(Ordering::SeqCst) <= before + 1,
        "no frames after stop"
    );
}

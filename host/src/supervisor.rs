//! Media supervisor: starts and stops one media session per approved session
//! and runs the capture devices.
//!
//! The session manager decides *whether* a session may run; this decides
//! nothing about policy. It implements [`SessionEffects`], so the manager's
//! "stop this now" becomes real as follows: the synchronous part (capture
//! devices stopped, frame delivery cut) happens **inside** the `stop_*` call,
//! before it returns; the asynchronous part (closing the peer connections)
//! follows a moment later.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle as ThreadHandle;
use std::time::Duration;

use rb_core::Permissions;
use rb_core::PlatformError;
use rb_core::protocol::HostError;
use rb_core::protocol::types::{FailureReason, HostSessionView, MediaCredentialsResponse};
use rb_core::session::{MediaReport, SessionEffects};
use rb_core::traits::{AudioCapture, Capture, MicCapture};
use rb_core::types::DisplayId;
use rb_media::peer::{IceServerConfig, MediaCredentials, MediaSessionConfig};
use rb_media::video::VideoEncoder;
use rb_media::{AudioEncoderFactory, HostIdentity, MediaEvent, MediaFeed, MediaSession};
use tokio::runtime::Handle;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::shared::{SharedClipboard, SharedInjector};

pub type VideoEncoderFactory = Arc<dyn Fn() -> Box<dyn VideoEncoder> + Send + Sync>;

/// What the supervisor needs from the machine.
pub struct SupervisorConfig {
    pub identity: HostIdentity,
    pub input: SharedInjector,
    pub clipboard: SharedClipboard,
    pub capture: Box<dyn Capture>,
    pub system_audio: Option<Box<dyn AudioCapture>>,
    pub microphone: Option<Box<dyn MicCapture>>,
    pub display: DisplayId,
    /// Local address to gather ICE candidates on.
    pub udp_ip: String,
    pub video_encoder: VideoEncoderFactory,
    pub audio_encoder: AudioEncoderFactory,
    /// How many times to ask for media credentials before giving up, and how long to wait between asks.
    pub credential_attempts: u32,
    pub credential_retry: Duration,
}

/// Messages from the supervisor to the host runtime (which owns the session manager).
pub enum SupervisorEvent {
    /// Please fetch media credentials for this session and answer through `reply`.
    NeedCredentials {
        session_id: String,
        reply: oneshot::Sender<Result<MediaCredentialsResponse, HostError>>,
    },
    /// Tell the server something about this session's media.
    Report {
        session_id: String,
        report: MediaReport,
    },
    /// A capture device could not start or died. Applies to every session.
    CaptureFailed(FailureReason),
}

// ---- capture pumps ---------------------------------------------------------------------------

struct Pump {
    stop: Arc<AtomicBool>,
    thread: Option<ThreadHandle<()>>,
}

impl Pump {
    fn idle() -> Self {
        Self {
            stop: Arc::new(AtomicBool::new(true)),
            thread: None,
        }
    }

    fn running(&self) -> bool {
        self.thread.is_some()
    }

    /// Ask the thread to end and wait for it. The caller has already stopped
    /// the device, so the thread's next read returns quickly.
    fn finish(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct Pumps {
    video: Pump,
    system_audio: Pump,
    microphone: Pump,
}

/// State shared by [`MediaEffects`] (synchronous) and the actor (asynchronous).
struct Shared {
    /// Sessions currently allowed to run, with what each is granted.
    sessions: Mutex<HashMap<String, Permissions>>,
    /// Where captured frames go.
    feeds: Mutex<Vec<(String, MediaFeed)>>,
    capture: Arc<Mutex<Box<dyn Capture>>>,
    system_audio: Option<Arc<Mutex<Box<dyn AudioCapture>>>>,
    microphone: Option<Arc<Mutex<Box<dyn MicCapture>>>>,
    display: DisplayId,
    pumps: Mutex<Pumps>,
    events: mpsc::UnboundedSender<SupervisorEvent>,
    handle: Handle,
}

impl Shared {
    /// Make the running capture devices match what the sessions need. Stopping
    /// is immediate and synchronous.
    fn sync_pumps(self: &Arc<Self>) {
        let (video, sys, mic) = {
            let sessions = self.sessions.lock().unwrap();
            (
                !sessions.is_empty(),
                sessions.values().any(|g| g.system_audio),
                sessions.values().any(|g| g.microphone),
            )
        };
        let mut pumps = self.pumps.lock().unwrap();

        // ---- screen
        if video && !pumps.video.running() {
            pumps.video = self.start_video_pump();
        } else if !video && pumps.video.running() {
            // Stop the device first (the pump is likely blocked in a read),
            // then let its thread go.
            self.capture.lock().unwrap().stop();
            pumps.video.finish();
        }

        // ---- system audio
        if let Some(device) = &self.system_audio {
            if sys && !pumps.system_audio.running() {
                pumps.system_audio = self.start_system_audio_pump(device.clone());
            } else if !sys && pumps.system_audio.running() {
                device.lock().unwrap().stop();
                pumps.system_audio.finish();
            }
        }

        // ---- microphone
        if let Some(device) = &self.microphone {
            if mic && !pumps.microphone.running() {
                pumps.microphone = self.start_microphone_pump(device.clone());
            } else if !mic && pumps.microphone.running() {
                device.lock().unwrap().stop();
                pumps.microphone.finish();
            }
        }
    }

    fn feeds_snapshot(&self) -> Vec<MediaFeed> {
        self.feeds
            .lock()
            .unwrap()
            .iter()
            .map(|(_, feed)| feed.clone())
            .collect()
    }

    fn failure(&self, error: &PlatformError, what: &str) {
        log::warn!("{what} failed: {error}");
        let reason = match error {
            PlatformError::PermissionDenied(_) => FailureReason::PermissionsUnavailable,
            _ => FailureReason::CaptureFailed,
        };
        let _ = self.events.send(SupervisorEvent::CaptureFailed(reason));
    }

    fn start_video_pump(self: &Arc<Self>) -> Pump {
        let stop = Arc::new(AtomicBool::new(false));
        let shared = self.clone();
        let flag = stop.clone();
        let thread = std::thread::Builder::new()
            .name("rb-capture-video".into())
            .spawn(move || {
                if let Err(e) = shared.capture.lock().unwrap().start(shared.display) {
                    shared.failure(&e, "screen capture");
                    return;
                }
                while !flag.load(Ordering::SeqCst) {
                    let frame = shared
                        .capture
                        .lock()
                        .unwrap()
                        .next_frame(Duration::from_millis(50));
                    match frame {
                        Ok(Some(frame)) => {
                            for feed in shared.feeds_snapshot() {
                                let _ = shared.handle.block_on(feed.push_video(&frame));
                            }
                        }
                        Ok(None) => {}
                        // The device was stopped under us: that is the normal way out.
                        Err(PlatformError::NotStarted) => break,
                        // A secure desktop (UAC, lock screen) shows nothing we may capture.
                        Err(PlatformError::SecureDesktop) => {
                            std::thread::sleep(Duration::from_millis(100));
                        }
                        Err(e) => {
                            shared.failure(&e, "screen capture");
                            break;
                        }
                    }
                }
            })
            .expect("spawn capture thread");
        Pump {
            stop,
            thread: Some(thread),
        }
    }

    fn start_system_audio_pump(
        self: &Arc<Self>,
        device: Arc<Mutex<Box<dyn AudioCapture>>>,
    ) -> Pump {
        let stop = Arc::new(AtomicBool::new(false));
        let shared = self.clone();
        let flag = stop.clone();
        let thread = std::thread::Builder::new()
            .name("rb-capture-system-audio".into())
            .spawn(move || {
                if let Err(e) = device.lock().unwrap().start() {
                    shared.failure(&e, "system audio capture");
                    return;
                }
                while !flag.load(Ordering::SeqCst) {
                    let chunk = device.lock().unwrap().next_chunk(Duration::from_millis(50));
                    match chunk {
                        Ok(Some(chunk)) => {
                            for feed in shared.feeds_snapshot() {
                                let _ = shared.handle.block_on(feed.push_system_audio(&chunk));
                            }
                        }
                        Ok(None) => {}
                        Err(PlatformError::NotStarted) => break,
                        Err(e) => {
                            shared.failure(&e, "system audio capture");
                            break;
                        }
                    }
                }
            })
            .expect("spawn audio thread");
        Pump {
            stop,
            thread: Some(thread),
        }
    }

    fn start_microphone_pump(self: &Arc<Self>, device: Arc<Mutex<Box<dyn MicCapture>>>) -> Pump {
        let stop = Arc::new(AtomicBool::new(false));
        let shared = self.clone();
        let flag = stop.clone();
        let thread = std::thread::Builder::new()
            .name("rb-capture-microphone".into())
            .spawn(move || {
                if let Err(e) = device.lock().unwrap().start() {
                    shared.failure(&e, "microphone capture");
                    return;
                }
                while !flag.load(Ordering::SeqCst) {
                    let chunk = device.lock().unwrap().next_chunk(Duration::from_millis(50));
                    match chunk {
                        Ok(Some(chunk)) => {
                            for feed in shared.feeds_snapshot() {
                                let _ = shared.handle.block_on(feed.push_microphone(&chunk));
                            }
                        }
                        Ok(None) => {}
                        Err(PlatformError::NotStarted) => break,
                        Err(e) => {
                            shared.failure(&e, "microphone capture");
                            break;
                        }
                    }
                }
            })
            .expect("spawn microphone thread");
        Pump {
            stop,
            thread: Some(thread),
        }
    }

    fn stop_everything_now(self: &Arc<Self>) {
        self.sessions.lock().unwrap().clear();
        self.feeds.lock().unwrap().clear();
        self.sync_pumps();
    }
}

// ---- the effects handle ------------------------------------------------------------------------

enum Command {
    Start { id: String, grant: Permissions },
    Grant { id: String, grant: Permissions },
    Stop { id: String },
    StopAll,
}

/// Implements [`SessionEffects`] for the session manager.
pub struct MediaEffects {
    shared: Arc<Shared>,
    commands: mpsc::UnboundedSender<Command>,
}

impl SessionEffects for MediaEffects {
    fn start_session(&mut self, session: &HostSessionView) {
        self.shared
            .sessions
            .lock()
            .unwrap()
            .insert(session.id.clone(), session.granted_permissions);
        self.shared.sync_pumps();
        let _ = self.commands.send(Command::Start {
            id: session.id.clone(),
            grant: session.granted_permissions,
        });
    }

    fn update_grant(&mut self, session_id: &str, grant: &Permissions) {
        if let Some(slot) = self.shared.sessions.lock().unwrap().get_mut(session_id) {
            *slot = *grant;
        }
        // A removed permission stops its capture device before this returns.
        self.shared.sync_pumps();
        let _ = self.commands.send(Command::Grant {
            id: session_id.to_owned(),
            grant: *grant,
        });
    }

    fn stop_session(&mut self, session_id: &str) {
        self.shared.sessions.lock().unwrap().remove(session_id);
        self.shared
            .feeds
            .lock()
            .unwrap()
            .retain(|(id, _)| id != session_id);
        self.shared.sync_pumps();
        let _ = self.commands.send(Command::Stop {
            id: session_id.to_owned(),
        });
    }

    fn stop_all(&mut self) {
        self.shared.stop_everything_now();
        let _ = self.commands.send(Command::StopAll);
    }
}

// ---- the actor ------------------------------------------------------------------------------------

enum Internal {
    Started {
        id: String,
        result: Result<MediaSession, String>,
    },
    Media {
        id: String,
        event: MediaEvent,
    },
}

struct Active {
    media: Option<MediaSession>,
    starter: Option<JoinHandle<()>>,
    forwarder: Option<JoinHandle<()>>,
    relay_used: bool,
}

impl Active {
    fn teardown(&mut self) {
        if let Some(task) = self.starter.take() {
            task.abort();
        }
        if let Some(task) = self.forwarder.take() {
            task.abort();
        }
        // Dropping the session stops its media.
        self.media = None;
    }
}

struct Actor {
    shared: Arc<Shared>,
    identity: HostIdentity,
    input: SharedInjector,
    clipboard: SharedClipboard,
    udp_ip: String,
    video_encoder: VideoEncoderFactory,
    audio_encoder: AudioEncoderFactory,
    attempts: u32,
    retry: Duration,
    active: HashMap<String, Active>,
    internal_tx: mpsc::UnboundedSender<Internal>,
}

/// Start the supervisor. Returns the effects handle for the session manager,
/// the event stream for the runtime, and the actor's task.
pub fn spawn(
    config: SupervisorConfig,
) -> (
    MediaEffects,
    mpsc::UnboundedReceiver<SupervisorEvent>,
    JoinHandle<()>,
) {
    let (events_tx, events_rx) = mpsc::unbounded_channel();
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let (internal_tx, internal_rx) = mpsc::unbounded_channel();
    let shared = Arc::new(Shared {
        sessions: Mutex::new(HashMap::new()),
        feeds: Mutex::new(Vec::new()),
        capture: Arc::new(Mutex::new(config.capture)),
        system_audio: config.system_audio.map(|d| Arc::new(Mutex::new(d))),
        microphone: config.microphone.map(|d| Arc::new(Mutex::new(d))),
        display: config.display,
        pumps: Mutex::new(Pumps {
            video: Pump::idle(),
            system_audio: Pump::idle(),
            microphone: Pump::idle(),
        }),
        events: events_tx,
        handle: Handle::current(),
    });
    let actor = Actor {
        shared: shared.clone(),
        identity: config.identity,
        input: config.input,
        clipboard: config.clipboard,
        udp_ip: config.udp_ip,
        video_encoder: config.video_encoder,
        audio_encoder: config.audio_encoder,
        attempts: config.credential_attempts.max(1),
        retry: config.credential_retry,
        active: HashMap::new(),
        internal_tx,
    };
    let task = tokio::spawn(actor.run(cmd_rx, internal_rx));
    (
        MediaEffects {
            shared,
            commands: cmd_tx,
        },
        events_rx,
        task,
    )
}

impl Actor {
    async fn run(
        mut self,
        mut commands: mpsc::UnboundedReceiver<Command>,
        mut internal: mpsc::UnboundedReceiver<Internal>,
    ) {
        loop {
            tokio::select! {
                command = commands.recv() => match command {
                    Some(Command::Start { id, grant }) => self.start(id, grant),
                    Some(Command::Grant { id, grant }) => self.grant(&id, grant),
                    Some(Command::Stop { id }) => self.stop(&id),
                    Some(Command::StopAll) => self.stop_all(),
                    None => break,
                },
                Some(event) = internal.recv() => match event {
                    Internal::Started { id, result } => self.started(id, result),
                    Internal::Media { id, event } => self.media_event(&id, event),
                },
            }
        }
        self.stop_all();
        self.shared.stop_everything_now();
    }

    fn start(&mut self, id: String, grant: Permissions) {
        let events = self.shared.events.clone();
        let internal = self.internal_tx.clone();
        let identity = self.identity.clone();
        let input = self.input.clone();
        let clipboard = self.clipboard.clone();
        let udp_ip = self.udp_ip.clone();
        let video_encoder = self.video_encoder.clone();
        let audio_encoder = self.audio_encoder.clone();
        let (attempts, retry) = (self.attempts, self.retry);
        let session_id = id.clone();
        let starter = tokio::spawn(async move {
            let result = start_media(
                &session_id,
                grant,
                events,
                identity,
                input,
                clipboard,
                udp_ip,
                video_encoder,
                audio_encoder,
                attempts,
                retry,
            )
            .await;
            let _ = internal.send(Internal::Started {
                id: session_id,
                result,
            });
        });
        if let Some(mut old) = self.active.insert(
            id,
            Active {
                media: None,
                starter: Some(starter),
                forwarder: None,
                relay_used: false,
            },
        ) {
            old.teardown();
        }
    }

    fn started(&mut self, id: String, result: Result<MediaSession, String>) {
        // The session may have been stopped while we were still connecting.
        let still_wanted = self.shared.sessions.lock().unwrap().contains_key(&id);
        let Some(active) = self.active.get_mut(&id).filter(|_| still_wanted) else {
            return;
        };
        active.starter = None;
        match result {
            Ok(mut media) => {
                // The grant may have changed while credentials were fetched.
                if let Some(grant) = self.shared.sessions.lock().unwrap().get(&id).copied() {
                    let _ = media.set_grant(grant);
                }
                self.shared
                    .feeds
                    .lock()
                    .unwrap()
                    .push((id.clone(), media.feed()));
                let mut events = media.take_events();
                let internal = self.internal_tx.clone();
                let session_id = id.clone();
                active.forwarder = Some(tokio::spawn(async move {
                    while let Some(event) = events.recv().await {
                        let _ = internal.send(Internal::Media {
                            id: session_id.clone(),
                            event,
                        });
                    }
                }));
                active.media = Some(media);
            }
            Err(why) => {
                log::warn!("media for session {id} could not start: {why}");
                let _ = self.shared.events.send(SupervisorEvent::Report {
                    session_id: id,
                    report: MediaReport::Failed(FailureReason::IceFailure),
                });
            }
        }
    }

    fn media_event(&mut self, id: &str, event: MediaEvent) {
        let Some(active) = self.active.get_mut(id) else {
            return;
        };
        let report = match event {
            MediaEvent::Connected { relay_used } => {
                active.relay_used = relay_used;
                Some(MediaReport::Connected { relay_used })
            }
            MediaEvent::Resumed => Some(MediaReport::Connected {
                relay_used: active.relay_used,
            }),
            MediaEvent::Reconnecting => Some(MediaReport::Reconnecting),
            MediaEvent::Failed(reason) => {
                // This session's media is over.
                self.shared
                    .feeds
                    .lock()
                    .unwrap()
                    .retain(|(sid, _)| sid != id);
                active.teardown();
                Some(MediaReport::Failed(reason))
            }
            MediaEvent::ChannelOpen { .. }
            | MediaEvent::ViewerLeft
            | MediaEvent::SignalingClosed { .. } => None,
        };
        if let Some(report) = report {
            let _ = self.shared.events.send(SupervisorEvent::Report {
                session_id: id.to_owned(),
                report,
            });
        }
    }

    fn grant(&mut self, id: &str, grant: Permissions) {
        if let Some(Active {
            media: Some(media), ..
        }) = self.active.get(id)
        {
            let _ = media.set_grant(grant);
        }
    }

    fn stop(&mut self, id: &str) {
        if let Some(mut active) = self.active.remove(id) {
            active.teardown();
        }
    }

    fn stop_all(&mut self) {
        for (_, mut active) in self.active.drain() {
            active.teardown();
        }
    }
}

/// Fetch credentials (through the runtime, which owns the session manager) and
/// bring up the media session. Retries while the media service is not ready
/// or the viewer has not bound a fingerprint yet; refuses to connect to a
/// viewer the server has not vouched for.
#[allow(clippy::too_many_arguments)]
async fn start_media(
    session_id: &str,
    grant: Permissions,
    events: mpsc::UnboundedSender<SupervisorEvent>,
    identity: HostIdentity,
    input: SharedInjector,
    clipboard: SharedClipboard,
    udp_ip: String,
    video_encoder: VideoEncoderFactory,
    audio_encoder: AudioEncoderFactory,
    attempts: u32,
    retry: Duration,
) -> Result<MediaSession, String> {
    let mut last = String::from("no attempt made");
    for attempt in 0..attempts {
        if attempt > 0 {
            tokio::time::sleep(retry).await;
        }
        let (reply, answer) = oneshot::channel();
        if events
            .send(SupervisorEvent::NeedCredentials {
                session_id: session_id.to_owned(),
                reply,
            })
            .is_err()
        {
            return Err("host runtime is gone".into());
        }
        let credentials = match answer.await {
            Ok(Ok(credentials)) => credentials,
            Ok(Err(HostError::DeviceRevoked)) => return Err("device was revoked".into()),
            Ok(Err(e)) => {
                last = e.to_string();
                continue;
            }
            Err(_) => return Err("host runtime is gone".into()),
        };
        // No fingerprint, no connection: the viewer is not who the server vouched for.
        let Some(peer_fingerprint) = credentials.peer_fingerprint.clone() else {
            last = "the viewer has not bound a fingerprint yet".into();
            continue;
        };
        let config = MediaSessionConfig {
            credentials: MediaCredentials {
                signaling_url: credentials.signaling_url,
                signaling_token: credentials.signaling_token,
                ice_servers: credentials
                    .ice_servers
                    .into_iter()
                    .map(|s| IceServerConfig {
                        urls: s.urls,
                        username: s.username,
                        credential: s.credential,
                    })
                    .collect(),
                peer_fingerprint,
            },
            identity: identity.clone(),
            grant,
            udp_ip: udp_ip.clone(),
            input: Box::new(input.clone()),
            clipboard: Box::new(clipboard.clone()),
            video_encoder: video_encoder(),
            audio_encoder: audio_encoder.clone(),
        };
        match MediaSession::start(config).await {
            Ok(session) => return Ok(session),
            Err(e) => last = e.to_string(),
        }
    }
    Err(last)
}

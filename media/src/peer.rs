//! The host's WebRTC session.
//!
//! * The host is the **offerer** and creates both data channels:
//!   `control` (reliable, ordered) and `pointer` (`ordered: false`,
//!   `maxRetransmits: 0`).
//! * The DTLS certificate is generated *before* `host/decide` (see
//!   [`HostIdentity`]) and its fingerprint is sent as `hostFingerprint`. The
//!   same certificate is used for the peer connection.
//! * An answer whose fingerprint is not the one the server vouched for is
//!   dropped, and the session reports an ICE failure.
//! * Every data channel message goes through the permission filter before it
//!   can reach the input injector or the clipboard.
//! * Audio and microphone tracks exist only while granted. Removing a
//!   permission stops the track at once, then the session renegotiates.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use rb_core::Permissions;
use rb_core::protocol::types::FailureReason;
use rb_core::session::{PermissionFilter, ViewerMessage, allow_outgoing_clipboard};
use rb_core::traits::{Clipboard, InputInjector};
use rb_core::types::{AudioChunk, VideoFrame};
use rtc::crypto::SignatureScheme;
use rtc::data_channel::RTCDataChannelInit;
use rtc::media::Sample;
use rtc::media_stream::MediaStreamTrack;
use rtc::peer_connection::certificate::{CertificateParams, RTCCertificate};
use rtc::peer_connection::configuration::media_engine::{MIME_TYPE_H264, MIME_TYPE_OPUS};
use rtc::rtp_transceiver::PayloadType;
use rtc::rtp_transceiver::rtp_sender::{
    RTCRtpCodec, RTCRtpCodecParameters, RTCRtpCodingParameters, RTCRtpEncodingParameters,
    RtpCodecKind,
};
use tokio::sync::{Mutex, mpsc};
use tokio::task::JoinHandle;
use webrtc::data_channel::{DataChannel, DataChannelEvent};
use webrtc::media_stream::track_local::TrackLocal;
use webrtc::media_stream::track_local::static_sample::TrackLocalStaticSample;
use webrtc::peer_connection::{
    MediaEngine, PeerConnection, PeerConnectionBuilder, PeerConnectionEventHandler,
    RTCConfigurationBuilder, RTCIceCandidate, RTCIceCandidateInit, RTCIceCandidateType,
    RTCIceGatheringState, RTCIceServer, RTCPeerConnectionIceEvent, RTCPeerConnectionState,
    RTCSessionDescription, RTCStatsReportEntry, Registry, SettingEngineBuilder, StatsSelector,
    register_default_interceptors,
};
use webrtc::rtp_transceiver::RtpSender;

use crate::audio::AudioEncoder;
use crate::error::MediaError;
use crate::fingerprint;
use crate::signaling::{ClientMessage, ServerEvent, SignalingClient};
use crate::video::{BitrateController, NetworkReport, VideoEncoder};

/// Frame rate cap for video.
pub const MAX_FPS: u32 = 30;
const FRAME_GAP: Duration = Duration::from_millis(1000 / MAX_FPS as u64);
const H264_PAYLOAD_TYPE: PayloadType = 102;
const OPUS_PAYLOAD_TYPE: PayloadType = 111;

// ---- identity ---------------------------------------------------------------

/// The host's DTLS identity for one session. Generate it **before** `host/decide`.
#[derive(Clone)]
pub struct HostIdentity {
    certificate: RTCCertificate,
    fingerprint: String,
}

impl HostIdentity {
    pub fn generate() -> Result<Self, MediaError> {
        let provider =
            rtc::crypto::default_provider().map_err(|e| MediaError::Crypto(e.to_string()))?;
        let params = CertificateParams::new(vec!["remotebridge".to_owned()])
            .map_err(|e| MediaError::Crypto(e.to_string()))?;
        let certificate =
            RTCCertificate::generate(provider.crypto(), SignatureScheme::EcdsaP256Sha256, params)
                .map_err(|e| MediaError::Crypto(e.to_string()))?;
        let fingerprints = certificate
            .get_fingerprints(provider.crypto())
            .map_err(|e| MediaError::Crypto(e.to_string()))?;
        let first = fingerprints
            .first()
            .ok_or_else(|| MediaError::Crypto("certificate has no fingerprint".into()))?;
        let fingerprint =
            fingerprint::normalize(&format!("{} {}", first.algorithm, first.value))
                .ok_or_else(|| MediaError::Crypto("unexpected fingerprint format".into()))?;
        Ok(Self {
            certificate,
            fingerprint,
        })
    }

    /// `sha-256 AB:CD:…`, ready for `hostFingerprint`.
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    /// The certificate, for whoever builds the peer connection (also used by
    /// the test viewer, which needs an identity of its own).
    pub fn certificate(&self) -> &RTCCertificate {
        &self.certificate
    }
}

// ---- configuration ------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IceServerConfig {
    pub urls: Vec<String>,
    pub username: Option<String>,
    pub credential: Option<String>,
}

/// What `host/media_credentials` returns.
#[derive(Debug, Clone)]
pub struct MediaCredentials {
    pub signaling_url: String,
    pub signaling_token: String,
    pub ice_servers: Vec<IceServerConfig>,
    /// The viewer's fingerprint as vouched for by the server.
    pub peer_fingerprint: String,
}

pub type AudioEncoderFactory = Arc<dyn Fn() -> Box<dyn AudioEncoder> + Send + Sync>;

pub struct MediaSessionConfig {
    pub credentials: MediaCredentials,
    pub identity: HostIdentity,
    pub grant: Permissions,
    /// Local address to gather candidates on (see [`default_udp_ip`]).
    pub udp_ip: String,
    pub input: Box<dyn InputInjector>,
    pub clipboard: Box<dyn Clipboard>,
    pub video_encoder: Box<dyn VideoEncoder>,
    pub audio_encoder: AudioEncoderFactory,
}

/// The local IP to gather candidates on: the address of the default route.
/// Nothing is sent.
pub fn default_udp_ip() -> String {
    std::net::UdpSocket::bind("0.0.0.0:0")
        .and_then(|s| {
            s.connect("203.0.113.1:9")?;
            s.local_addr()
        })
        .map_or_else(|_| "127.0.0.1".to_owned(), |a| a.ip().to_string())
}

// ---- events ----------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaEvent {
    /// A data channel is open.
    ChannelOpen {
        label: String,
    },
    /// Media is flowing. `relay_used` is true if the selected candidate pair is a relay.
    Connected {
        relay_used: bool,
    },
    Reconnecting,
    Resumed,
    /// The session cannot continue.
    Failed(FailureReason),
    ViewerLeft,
    /// The signaling socket closed (for example the token expired, code 4001).
    /// Media keeps running; fresh credentials are needed to renegotiate.
    SignalingClosed {
        code: Option<u16>,
    },
}

// ---- shared state ------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Channel {
    Control,
    Pointer,
}

impl Channel {
    fn label(self) -> &'static str {
        match self {
            Self::Control => "control",
            Self::Pointer => "pointer",
        }
    }

    /// The contract: `move` goes on `pointer`, everything else on `control`.
    fn accepts(self, message: &ViewerMessage) -> bool {
        let is_move = matches!(message, ViewerMessage::Move { .. });
        match self {
            Self::Control => !is_move,
            Self::Pointer => is_move,
        }
    }
}

/// Used by the data channel readers.
struct Shared {
    filter: StdMutex<PermissionFilter>,
    input: StdMutex<Box<dyn InputInjector>>,
    clipboard: StdMutex<Box<dyn Clipboard>>,
}

impl Shared {
    /// One message from the viewer. Filter first; only then does anything
    /// happen. Nothing here is logged: not keys, not clipboard text.
    fn handle(&self, channel: Channel, text: Option<&str>) {
        let Some(text) = text else {
            self.filter.lock().unwrap().reject_binary();
            return;
        };
        let Some(message) = self.filter.lock().unwrap().check(text) else {
            return;
        };
        if !channel.accepts(&message) {
            self.filter.lock().unwrap().reject_binary();
            return;
        }
        match message {
            ViewerMessage::Key { code, down, .. } => {
                let _ = self.input.lock().unwrap().key(&code, down);
            }
            ViewerMessage::Button { button, down } => {
                let _ = self.input.lock().unwrap().button(button, down);
            }
            ViewerMessage::Wheel { dx, dy } => {
                let _ = self.input.lock().unwrap().wheel(dx, dy);
            }
            ViewerMessage::Move { x, y } => {
                let _ = self.input.lock().unwrap().move_pointer(x, y);
            }
            ViewerMessage::Clipboard { text } => {
                let _ = self.clipboard.lock().unwrap().set_text(&text);
            }
        }
    }
}

// ---- tracks ---------------------------------------------------------------------------

struct TrackWriter {
    track: Arc<TrackLocalStaticSample>,
    sender: Arc<dyn RtpSender>,
    ssrc: u32,
    payload_type: Option<PayloadType>,
}

impl TrackWriter {
    /// Known once the answer is applied.
    async fn payload_type(&mut self) -> Option<PayloadType> {
        if self.payload_type.is_none() {
            let params = self.sender.get_parameters().await.ok()?;
            self.payload_type = params
                .rtp_parameters
                .codecs
                .first()
                .map(|codec| codec.payload_type);
        }
        self.payload_type
    }

    /// Frames before negotiation completes are dropped.
    async fn write(&mut self, data: Vec<u8>, duration: Duration) -> Result<(), MediaError> {
        let Some(payload_type) = self.payload_type().await else {
            return Ok(());
        };
        self.track
            .sample_writer(self.ssrc, payload_type)
            .write_sample(&Sample {
                data: Bytes::from(data),
                duration,
                ..Sample::new(Instant::now())
            })
            .await
            .map_err(MediaError::from)
    }
}

struct VideoPipe {
    writer: Option<TrackWriter>,
    encoder: Box<dyn VideoEncoder>,
    last_frame: Option<Instant>,
}

struct AudioPipe {
    writer: TrackWriter,
    encoder: Box<dyn AudioEncoder>,
}

/// Where capture threads hand frames. Cheap to clone.
#[derive(Clone)]
pub struct MediaFeed {
    inner: Arc<Feed>,
}

struct Feed {
    connected: AtomicBool,
    video: Mutex<VideoPipe>,
    system_audio: Mutex<Option<AudioPipe>>,
    microphone: Mutex<Option<AudioPipe>>,
}

impl MediaFeed {
    /// Encode and send one screen frame. Dropped if not connected yet, or if
    /// it follows the previous frame by less than 1/30 s.
    pub async fn push_video(&self, frame: &VideoFrame) -> Result<(), MediaError> {
        if !self.inner.connected.load(Ordering::SeqCst) {
            return Ok(());
        }
        let mut video = self.inner.video.lock().await;
        let now = Instant::now();
        if video
            .last_frame
            .is_some_and(|t| now.duration_since(t) < FRAME_GAP)
        {
            return Ok(());
        }
        let duration = video.last_frame.map_or(FRAME_GAP, |t| {
            now.duration_since(t)
                .clamp(Duration::from_millis(1), Duration::from_millis(200))
        });
        video.last_frame = Some(now);
        let Some(encoded) = video.encoder.encode(frame)? else {
            return Ok(());
        };
        match video.writer.as_mut() {
            Some(writer) => writer.write(encoded.data, duration).await,
            None => Ok(()),
        }
    }

    /// System audio. Dropped unless `systemAudio` is granted.
    pub async fn push_system_audio(&self, chunk: &AudioChunk) -> Result<(), MediaError> {
        Self::push_audio(&self.inner, &self.inner.system_audio, chunk).await
    }

    /// Microphone. Dropped unless `microphone` is granted.
    pub async fn push_microphone(&self, chunk: &AudioChunk) -> Result<(), MediaError> {
        Self::push_audio(&self.inner, &self.inner.microphone, chunk).await
    }

    async fn push_audio(
        feed: &Feed,
        pipe: &Mutex<Option<AudioPipe>>,
        chunk: &AudioChunk,
    ) -> Result<(), MediaError> {
        if !feed.connected.load(Ordering::SeqCst) {
            return Ok(());
        }
        let mut guard = pipe.lock().await;
        let Some(pipe) = guard.as_mut() else {
            return Ok(());
        };
        for packet in pipe.encoder.encode(chunk)? {
            pipe.writer.write(packet.data, packet.duration).await?;
        }
        Ok(())
    }
}

// ---- peer connection events --------------------------------------------------------------

enum Internal {
    Candidate(RTCIceCandidate),
    GatheringComplete,
    State(RTCPeerConnectionState),
    NegotiationNeeded,
    ChannelOpen(Channel),
}

#[derive(Clone)]
struct Handler {
    tx: mpsc::UnboundedSender<Internal>,
}

#[async_trait::async_trait]
impl PeerConnectionEventHandler for Handler {
    async fn on_ice_candidate(&self, event: RTCPeerConnectionIceEvent) {
        let _ = self.tx.send(Internal::Candidate(event.candidate));
    }

    async fn on_ice_gathering_state_change(&self, state: RTCIceGatheringState) {
        if state == RTCIceGatheringState::Complete {
            let _ = self.tx.send(Internal::GatheringComplete);
        }
    }

    async fn on_connection_state_change(&self, state: RTCPeerConnectionState) {
        let _ = self.tx.send(Internal::State(state));
    }

    async fn on_negotiation_needed(&self) {
        let _ = self.tx.send(Internal::NegotiationNeeded);
    }
}

// ---- the session -----------------------------------------------------------------------------

enum Command {
    SetGrant(Permissions),
    SendClipboard(String),
    ChannelInfo(tokio::sync::oneshot::Sender<Vec<ChannelInfo>>),
    Stop,
}

/// How a data channel was created, as the host sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelInfo {
    pub label: String,
    pub ordered: bool,
    pub max_retransmits: Option<u16>,
}

/// A running media session. Dropping it stops everything.
pub struct MediaSession {
    commands: mpsc::UnboundedSender<Command>,
    events: mpsc::UnboundedReceiver<MediaEvent>,
    feed: MediaFeed,
    shared: Arc<Shared>,
    task: Option<JoinHandle<()>>,
}

impl MediaSession {
    /// Connect to signaling and wait for the viewer. The host offers as soon
    /// as the viewer is there.
    pub async fn start(config: MediaSessionConfig) -> Result<Self, MediaError> {
        if !fingerprint::is_valid_format(config.identity.fingerprint()) {
            return Err(MediaError::Crypto(
                "host fingerprint has the wrong format".into(),
            ));
        }
        let signaling = SignalingClient::connect(
            &config.credentials.signaling_url,
            &config.credentials.signaling_token,
        )
        .await
        .map_err(|e| MediaError::Signaling(e.to_string()))?;

        let shared = Arc::new(Shared {
            filter: StdMutex::new(PermissionFilter::new(config.grant)),
            input: StdMutex::new(config.input),
            clipboard: StdMutex::new(config.clipboard),
        });
        let feed = MediaFeed {
            inner: Arc::new(Feed {
                connected: AtomicBool::new(false),
                video: Mutex::new(VideoPipe {
                    writer: None,
                    encoder: config.video_encoder,
                    last_frame: None,
                }),
                system_audio: Mutex::new(None),
                microphone: Mutex::new(None),
            }),
        };
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let (internal_tx, internal_rx) = mpsc::unbounded_channel();

        let actor = Actor {
            signaling,
            signaling_open: true,
            identity: config.identity,
            credentials: config.credentials,
            grant: config.grant,
            udp_ip: config.udp_ip,
            audio_factory: config.audio_encoder,
            shared: shared.clone(),
            feed: feed.clone(),
            events: event_tx,
            internal_tx,
            pc: None,
            channels: Vec::new(),
            system_audio_sender: None,
            microphone_sender: None,
            answered: false,
            awaiting_answer: false,
            offer_pending: false,
            was_connected: false,
            state: RTCPeerConnectionState::New,
            bitrate: BitrateController::new(crate::video::OpenH264Encoder::DEFAULT_BITRATE),
            started: Instant::now(),
        };
        let task = tokio::spawn(actor.run(cmd_rx, internal_rx));
        Ok(Self {
            commands: cmd_tx,
            events: event_rx,
            feed,
            shared,
            task: Some(task),
        })
    }

    /// The handle capture threads push frames into.
    pub fn feed(&self) -> MediaFeed {
        self.feed.clone()
    }

    /// Next event. `None` after the session has ended.
    pub async fn next_event(&mut self) -> Option<MediaEvent> {
        self.events.recv().await
    }

    /// Hand the event stream to another task (the supervisor reads events
    /// there while keeping the session itself). After this, `next_event`
    /// returns `None`.
    pub fn take_events(&mut self) -> mpsc::UnboundedReceiver<MediaEvent> {
        std::mem::replace(&mut self.events, mpsc::unbounded_channel().1)
    }

    /// Change what is allowed. The filter changes immediately; tracks follow.
    pub fn set_grant(&self, grant: Permissions) -> Result<(), MediaError> {
        // The filter is updated here, before the actor even sees the command,
        // so the very next viewer message is judged by the new grant.
        self.shared.filter.lock().unwrap().set_grant(grant);
        if !grant.control {
            let _ = self.shared.input.lock().unwrap().release_all();
        }
        self.commands
            .send(Command::SetGrant(grant))
            .map_err(|_| MediaError::Stopped)
    }

    /// Host to viewer clipboard text; sent only while `clipboard` is granted.
    pub fn send_clipboard(&self, text: &str) -> Result<(), MediaError> {
        self.commands
            .send(Command::SendClipboard(text.to_owned()))
            .map_err(|_| MediaError::Stopped)
    }

    /// The data channels the host created and their reliability settings.
    pub async fn channel_info(&self) -> Result<Vec<ChannelInfo>, MediaError> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.commands
            .send(Command::ChannelInfo(tx))
            .map_err(|_| MediaError::Stopped)?;
        rx.await.map_err(|_| MediaError::Stopped)
    }

    /// How many messages the filter let through or dropped. Counts only.
    pub fn filter_stats(&self) -> rb_core::session::FilterStats {
        self.shared.filter.lock().unwrap().stats()
    }

    /// Stop all media and close the connection.
    pub async fn stop(mut self) {
        let _ = self.commands.send(Command::Stop);
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}

impl Drop for MediaSession {
    fn drop(&mut self) {
        // Immediately, before anything async: no more frames, no held keys.
        self.feed.inner.connected.store(false, Ordering::SeqCst);
        let _ = self.shared.input.lock().unwrap().release_all();
        // Then let the task close the peer connection in the background.
        let _ = self.commands.send(Command::Stop);
        self.task.take();
    }
}

struct Actor {
    signaling: SignalingClient,
    signaling_open: bool,
    identity: HostIdentity,
    credentials: MediaCredentials,
    grant: Permissions,
    udp_ip: String,
    audio_factory: AudioEncoderFactory,
    shared: Arc<Shared>,
    feed: MediaFeed,
    events: mpsc::UnboundedSender<MediaEvent>,
    internal_tx: mpsc::UnboundedSender<Internal>,
    pc: Option<Arc<dyn PeerConnection>>,
    channels: Vec<(Channel, Arc<dyn DataChannel>)>,
    system_audio_sender: Option<Arc<dyn RtpSender>>,
    microphone_sender: Option<Arc<dyn RtpSender>>,
    /// An answer has been applied for the current connection.
    answered: bool,
    /// An offer is out and its answer has not been applied yet. An answer
    /// that arrives when none is awaited (a duplicate, because an offer was
    /// re-sent) is ignored.
    awaiting_answer: bool,
    /// Something changed while an offer was out; send another once it is answered.
    offer_pending: bool,
    was_connected: bool,
    state: RTCPeerConnectionState,
    bitrate: BitrateController,
    started: Instant,
}

fn codec_video() -> RTCRtpCodec {
    RTCRtpCodec {
        mime_type: MIME_TYPE_H264.to_owned(),
        clock_rate: 90_000,
        channels: 0,
        sdp_fmtp_line: "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f"
            .to_owned(),
        rtcp_feedback: vec![],
    }
}

fn codec_audio() -> RTCRtpCodec {
    RTCRtpCodec {
        mime_type: MIME_TYPE_OPUS.to_owned(),
        clock_rate: 48_000,
        channels: 2,
        sdp_fmtp_line: "minptime=10;useinbandfec=1".to_owned(),
        rtcp_feedback: vec![],
    }
}

/// One stream for everything, as the contract says.
const STREAM_ID: &str = "remotebridge";

fn new_track(
    kind: RtpCodecKind,
    track_id: &str,
    codec: RTCRtpCodec,
) -> Result<(Arc<TrackLocalStaticSample>, u32), MediaError> {
    let mut random = [0u8; 4];
    getrandom_fill(&mut random);
    let ssrc = u32::from_le_bytes(random);
    let track = TrackLocalStaticSample::new(
        Instant::now(),
        MediaStreamTrack::new(
            STREAM_ID.to_owned(),
            track_id.to_owned(),
            track_id.to_owned(),
            kind,
            vec![RTCRtpEncodingParameters {
                rtp_coding_parameters: RTCRtpCodingParameters {
                    ssrc: Some(ssrc),
                    ..Default::default()
                },
                codec,
                ..Default::default()
            }],
        ),
    )?;
    Ok((Arc::new(track), ssrc))
}

fn getrandom_fill(buf: &mut [u8]) {
    // Not security sensitive (an RTP SSRC); the clock and a counter are plenty.
    use std::sync::atomic::AtomicU32;
    static COUNTER: AtomicU32 = AtomicU32::new(0x9e37_79b9);
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    let n = COUNTER.fetch_add(0x85eb_ca6b, Ordering::Relaxed) ^ t;
    for (i, b) in buf.iter_mut().enumerate() {
        *b = (n >> ((i % 4) * 8)) as u8;
    }
}

impl Actor {
    async fn run(
        mut self,
        mut commands: mpsc::UnboundedReceiver<Command>,
        mut internal: mpsc::UnboundedReceiver<Internal>,
    ) {
        let mut stats_tick = tokio::time::interval(Duration::from_secs(2));
        loop {
            tokio::select! {
                command = commands.recv() => match command {
                    Some(Command::SetGrant(grant)) => self.apply_grant(grant).await,
                    Some(Command::SendClipboard(text)) => self.send_clipboard(&text).await,
                    Some(Command::ChannelInfo(reply)) => {
                        let _ = reply.send(self.channel_info().await);
                    }
                    Some(Command::Stop) | None => break,
                },
                event = Self::next_signal(&mut self.signaling, self.signaling_open) => {
                    match event {
                        Some(event) => self.on_signal(event).await,
                        None => self.signaling_open = false,
                    }
                },
                Some(event) = internal.recv() => self.on_internal(event).await,
                _ = stats_tick.tick() => self.on_stats_tick().await,
            }
        }
        self.shutdown().await;
    }

    async fn channel_info(&self) -> Vec<ChannelInfo> {
        let mut out = Vec::new();
        for (kind, channel) in &self.channels {
            out.push(ChannelInfo {
                label: kind.label().to_owned(),
                ordered: channel.ordered().await.unwrap_or(true),
                max_retransmits: channel.max_retransmits().await.ok().flatten(),
            });
        }
        out
    }

    async fn next_signal(signaling: &mut SignalingClient, open: bool) -> Option<ServerEvent> {
        if open {
            signaling.next_event().await
        } else {
            std::future::pending().await
        }
    }

    fn emit(&self, event: MediaEvent) {
        let _ = self.events.send(event);
    }

    async fn shutdown(&mut self) {
        self.feed.inner.connected.store(false, Ordering::SeqCst);
        // Tracks and pipes go first, so no frame is written after a stop.
        self.feed.inner.video.lock().await.writer = None;
        *self.feed.inner.system_audio.lock().await = None;
        *self.feed.inner.microphone.lock().await = None;
        let _ = self.shared.input.lock().unwrap().release_all();
        let _ = self.signaling.send(ClientMessage::Bye);
        if let Some(pc) = self.pc.take() {
            let _ = pc.close().await;
        }
    }

    // ---- signaling --------------------------------------------------------------------------

    async fn on_signal(&mut self, event: ServerEvent) {
        match event {
            ServerEvent::Peer { present: true } | ServerEvent::Ready => {
                self.offer_to_viewer().await;
            }
            ServerEvent::Peer { present: false } | ServerEvent::Bye => {
                self.emit(MediaEvent::ViewerLeft);
            }
            ServerEvent::Answer { sdp } => self.on_answer(sdp).await,
            ServerEvent::Ice { candidate } => {
                if let (Some(pc), Some(value)) = (&self.pc, candidate)
                    && let Ok(init) = serde_json::from_value::<RTCIceCandidateInit>(value)
                {
                    let _ = pc.add_ice_candidate(init).await;
                }
            }
            ServerEvent::Error { message } => {
                log::warn!("signaling refused a message: {message}");
            }
            ServerEvent::Closed { code } => {
                self.emit(MediaEvent::SignalingClosed { code });
            }
        }
    }

    /// The viewer is there (or says it is ready): make an offer.
    async fn offer_to_viewer(&mut self) {
        let healthy = self.pc.is_some()
            && !matches!(
                self.state,
                RTCPeerConnectionState::Failed
                    | RTCPeerConnectionState::Closed
                    | RTCPeerConnectionState::Disconnected
            );
        if healthy && self.answered {
            // Already connected: a returning signaling socket changes nothing.
            return;
        }
        if healthy {
            // An offer is out and unanswered: say it again.
            if let Some(pc) = &self.pc
                && let Some(desc) = pc.local_description().await
            {
                let _ = self.signaling.send(ClientMessage::Offer { sdp: desc.sdp });
            }
            return;
        }
        if let Err(e) = self.build_connection().await {
            log::warn!("could not start the peer connection: {e}");
            self.emit(MediaEvent::Failed(FailureReason::IceFailure));
        }
    }

    async fn build_connection(&mut self) -> Result<(), MediaError> {
        // Start clean: a previous attempt may be half dead.
        if let Some(old) = self.pc.take() {
            let _ = old.close().await;
        }
        self.channels.clear();
        self.system_audio_sender = None;
        self.microphone_sender = None;
        self.answered = false;
        self.awaiting_answer = false;
        self.offer_pending = false;
        self.was_connected = false;
        self.state = RTCPeerConnectionState::New;

        let mut media = MediaEngine::default();
        media.register_codec(
            RTCRtpCodecParameters {
                rtp_codec: codec_video(),
                payload_type: H264_PAYLOAD_TYPE,
            },
            RtpCodecKind::Video,
        )?;
        media.register_codec(
            RTCRtpCodecParameters {
                rtp_codec: codec_audio(),
                payload_type: OPUS_PAYLOAD_TYPE,
            },
            RtpCodecKind::Audio,
        )?;
        let registry = register_default_interceptors(Registry::new(), &mut media)?;

        let ice_servers = self
            .credentials
            .ice_servers
            .iter()
            .map(|s| RTCIceServer {
                urls: s.urls.clone(),
                username: s.username.clone().unwrap_or_default(),
                credential: s.credential.clone().unwrap_or_default(),
            })
            .collect::<Vec<_>>();
        let configuration = RTCConfigurationBuilder::new()
            .with_ice_servers(ice_servers)
            .with_certificates(vec![self.identity.certificate.clone()])
            .build();
        let settings = SettingEngineBuilder::new()
            // Browsers hide their addresses behind mDNS names; resolve them.
            .with_multicast_dns_mode(rtc::ice::mdns::MulticastDnsMode::QueryOnly)
            .with_include_loopback_candidate(self.udp_ip.starts_with("127."))
            .build();

        let runtime = webrtc::runtime::default_runtime()
            .ok_or_else(|| MediaError::WebRtc("no async runtime available".into()))?;
        let pc = PeerConnectionBuilder::new()
            .with_configuration(configuration)
            .with_media_engine(media)
            .with_interceptor_registry(registry)
            .with_setting_engine(settings)
            .with_handler(Arc::new(Handler {
                tx: self.internal_tx.clone(),
            }))
            .with_runtime(runtime)
            .with_udp_addrs(vec![format!("{}:0", self.udp_ip)])
            .build()
            .await?;
        let pc: Arc<dyn PeerConnection> = Arc::new(pc);

        // Data channels: the host creates both.
        let control = pc
            .create_data_channel(
                "control",
                Some(RTCDataChannelInit {
                    ordered: true,
                    ..Default::default()
                }),
            )
            .await?;
        let pointer = pc
            .create_data_channel(
                "pointer",
                Some(RTCDataChannelInit {
                    ordered: false,
                    max_retransmits: Some(0),
                    ..Default::default()
                }),
            )
            .await?;
        for (kind, channel) in [(Channel::Control, control), (Channel::Pointer, pointer)] {
            self.spawn_reader(kind, channel.clone());
            self.channels.push((kind, channel));
        }

        // Video is always there (view is the default).
        let (track, ssrc) = new_track(RtpCodecKind::Video, "video", codec_video())?;
        let sender = pc.add_track(track.clone() as Arc<dyn TrackLocal>).await?;
        self.feed.inner.video.lock().await.writer = Some(TrackWriter {
            track,
            sender,
            ssrc,
            payload_type: None,
        });

        self.pc = Some(pc);
        self.sync_audio_tracks().await?;
        self.send_offer().await
    }

    fn spawn_reader(&self, kind: Channel, channel: Arc<dyn DataChannel>) {
        let shared = self.shared.clone();
        let internal = self.internal_tx.clone();
        tokio::spawn(async move {
            while let Some(event) = channel.poll().await {
                match event {
                    DataChannelEvent::OnOpen => {
                        let _ = internal.send(Internal::ChannelOpen(kind));
                    }
                    DataChannelEvent::OnMessage(message) => {
                        let text = if message.is_string {
                            std::str::from_utf8(&message.data).ok()
                        } else {
                            None
                        };
                        shared.handle(kind, text);
                    }
                    DataChannelEvent::OnClose => break,
                    _ => {}
                }
            }
        });
    }

    async fn send_offer(&mut self) -> Result<(), MediaError> {
        let Some(pc) = self.pc.clone() else {
            return Ok(());
        };
        let offer = pc.create_offer(None).await?;
        pc.set_local_description(offer).await?;
        if let Some(desc) = pc.local_description().await {
            let _ = self.signaling.send(ClientMessage::Offer { sdp: desc.sdp });
        }
        self.awaiting_answer = true;
        Ok(())
    }

    /// Renegotiate: one offer in flight at a time. A request that arrives
    /// while an offer is out is sent once that offer has been answered.
    async fn request_offer(&mut self) {
        if self.awaiting_answer {
            self.offer_pending = true;
        } else if let Err(e) = self.send_offer().await {
            log::warn!("renegotiation failed: {e}");
        }
    }

    async fn on_answer(&mut self, sdp: String) {
        let Some(pc) = self.pc.clone() else {
            return;
        };
        if !self.awaiting_answer {
            return; // a duplicate: we re-sent an offer and the viewer answered twice
        }
        // The viewer must be exactly who the server said.
        if !fingerprint::sdp_matches(&sdp, &self.credentials.peer_fingerprint) {
            log::warn!("viewer fingerprint mismatch: dropping the call");
            self.fail(FailureReason::IceFailure).await;
            return;
        }
        let Ok(answer) = RTCSessionDescription::answer(sdp) else {
            self.fail(FailureReason::IceFailure).await;
            return;
        };
        match pc.set_remote_description(answer).await {
            Ok(()) => {
                self.answered = true;
                self.awaiting_answer = false;
                if std::mem::take(&mut self.offer_pending) {
                    self.request_offer().await;
                }
            }
            Err(e) => {
                log::warn!("could not apply the answer: {e}");
                self.fail(FailureReason::IceFailure).await;
            }
        }
    }

    /// Drop the call and tell the host runtime.
    async fn fail(&mut self, reason: FailureReason) {
        self.feed.inner.connected.store(false, Ordering::SeqCst);
        self.feed.inner.video.lock().await.writer = None;
        *self.feed.inner.system_audio.lock().await = None;
        *self.feed.inner.microphone.lock().await = None;
        if let Some(pc) = self.pc.take() {
            let _ = pc.close().await;
        }
        self.emit(MediaEvent::Failed(reason));
    }

    // ---- grants and tracks -----------------------------------------------------------------------

    async fn apply_grant(&mut self, grant: Permissions) {
        self.grant = grant;
        self.shared.filter.lock().unwrap().set_grant(grant);
        match self.sync_audio_tracks().await {
            Ok(changed) => {
                if changed && self.pc.is_some() {
                    self.request_offer().await;
                }
            }
            Err(e) => log::warn!("could not update tracks: {e}"),
        }
    }

    /// Make the audio tracks match the grant. Removal is immediate. Returns
    /// whether anything changed (so the caller renegotiates).
    async fn sync_audio_tracks(&mut self) -> Result<bool, MediaError> {
        let Some(pc) = self.pc.clone() else {
            return Ok(false);
        };
        let mut changed = false;
        for (wanted, label, pipe, slot) in [
            (
                self.grant.system_audio,
                "system-audio",
                &self.feed.inner.system_audio,
                &mut self.system_audio_sender,
            ),
            (
                self.grant.microphone,
                "microphone",
                &self.feed.inner.microphone,
                &mut self.microphone_sender,
            ),
        ] {
            if wanted && slot.is_none() {
                let (track, ssrc) = new_track(RtpCodecKind::Audio, label, codec_audio())?;
                let sender = pc.add_track(track.clone() as Arc<dyn TrackLocal>).await?;
                *pipe.lock().await = Some(AudioPipe {
                    writer: TrackWriter {
                        track,
                        sender: sender.clone(),
                        ssrc,
                        payload_type: None,
                    },
                    encoder: (self.audio_factory)(),
                });
                *slot = Some(sender);
                changed = true;
            } else if !wanted && let Some(sender) = slot.take() {
                // Stop the audio first, then take the track out.
                *pipe.lock().await = None;
                let _ = pc.remove_track(&sender).await;
                changed = true;
            }
        }
        Ok(changed)
    }

    async fn send_clipboard(&self, text: &str) {
        if !allow_outgoing_clipboard(&self.grant, text) {
            return;
        }
        let Some((_, control)) = self.channels.iter().find(|(k, _)| *k == Channel::Control) else {
            return;
        };
        let payload = serde_json::json!({"t": "clip", "text": text}).to_string();
        let _ = control.send_text(&payload).await;
    }

    // ---- connection state ---------------------------------------------------------------------------

    async fn on_internal(&mut self, event: Internal) {
        match event {
            Internal::Candidate(candidate) => {
                if let Ok(init) = candidate.to_json()
                    && let Ok(value) = serde_json::to_value(init)
                {
                    let _ = self.signaling.send(ClientMessage::Ice {
                        candidate: Some(value),
                    });
                }
            }
            Internal::GatheringComplete => {
                let _ = self.signaling.send(ClientMessage::Ice { candidate: None });
            }
            Internal::NegotiationNeeded => {
                // Only once the first negotiation is done: the initial offer is made by hand.
                if self.answered {
                    self.request_offer().await;
                }
            }
            Internal::ChannelOpen(kind) => self.emit(MediaEvent::ChannelOpen {
                label: kind.label().to_owned(),
            }),
            Internal::State(state) => self.on_state(state).await,
        }
    }

    async fn on_state(&mut self, state: RTCPeerConnectionState) {
        self.state = state;
        match state {
            RTCPeerConnectionState::Connected => {
                self.feed.inner.connected.store(true, Ordering::SeqCst);
                if self.was_connected {
                    self.emit(MediaEvent::Resumed);
                } else {
                    self.was_connected = true;
                    let relay_used = self.relay_used().await;
                    self.emit(MediaEvent::Connected { relay_used });
                    // A viewer that just joined needs a keyframe to start decoding.
                    self.feed
                        .inner
                        .video
                        .lock()
                        .await
                        .encoder
                        .request_keyframe();
                }
            }
            RTCPeerConnectionState::Disconnected => {
                self.feed.inner.connected.store(false, Ordering::SeqCst);
                self.emit(MediaEvent::Reconnecting);
            }
            RTCPeerConnectionState::Failed => {
                self.fail(FailureReason::IceFailure).await;
            }
            RTCPeerConnectionState::Closed => {
                self.feed.inner.connected.store(false, Ordering::SeqCst);
            }
            _ => {}
        }
    }

    /// Is the selected candidate pair a relay?
    async fn relay_used(&self) -> bool {
        let Some(pc) = &self.pc else {
            return false;
        };
        let report = pc.get_stats(Instant::now(), StatsSelector::None).await;
        let selected_pair = report.iter().find_map(|entry| match entry {
            RTCStatsReportEntry::Transport(t) if !t.selected_candidate_pair_id.is_empty() => {
                Some(t.selected_candidate_pair_id.clone())
            }
            _ => None,
        });
        let Some(selected_pair) = selected_pair else {
            return false;
        };
        let local_id = report.iter().find_map(|entry| match entry {
            RTCStatsReportEntry::IceCandidatePair(p) if p.stats.id == selected_pair => {
                Some(p.local_candidate_id.clone())
            }
            _ => None,
        });
        let Some(local_id) = local_id else {
            return false;
        };
        report.iter().any(|entry| {
            matches!(
                entry,
                RTCStatsReportEntry::LocalCandidate(c)
                    if c.stats.id == local_id && c.candidate_type == RTCIceCandidateType::Relay
            )
        })
    }

    /// Every couple of seconds: look at what the viewer reports and adapt the bitrate.
    async fn on_stats_tick(&mut self) {
        if self.state != RTCPeerConnectionState::Connected {
            return;
        }
        let Some(pc) = self.pc.clone() else {
            return;
        };
        let report = pc.get_stats(Instant::now(), StatsSelector::None).await;
        let remote = report.iter().find_map(|entry| match entry {
            RTCStatsReportEntry::RemoteInboundRtp(r) => Some(NetworkReport {
                loss_fraction: r.fraction_lost,
                rtt_ms: r.round_trip_time * 1000.0,
            }),
            _ => None,
        });
        if let Some(report) = remote
            && let Some(next) = self.bitrate.update(self.started.elapsed(), report)
        {
            let _ = self.feed.inner.video.lock().await.encoder.set_bitrate(next);
        }
    }
}

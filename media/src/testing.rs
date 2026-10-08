//! Test support: a local signaling server and a scripted viewer.
//!
//! Enabled with the `testing` feature. The viewer is a second WebRTC peer
//! (the answerer) that talks to the host through the real signaling relay on
//! loopback, so tests exercise the same path a browser would, without one.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures::{SinkExt, StreamExt};
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::sync::{Mutex, mpsc};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use webrtc::data_channel::{DataChannel, DataChannelEvent};
use webrtc::media_stream::track_remote::{TrackRemote, TrackRemoteEvent};
use webrtc::peer_connection::{
    MediaEngine, PeerConnection, PeerConnectionBuilder, PeerConnectionEventHandler,
    RTCConfigurationBuilder, RTCIceCandidate, RTCIceCandidateInit, RTCIceGatheringState,
    RTCPeerConnectionIceEvent, RTCSessionDescription, Registry, SettingEngineBuilder,
    register_default_interceptors,
};

use crate::peer::HostIdentity;

const SECRET: &[u8] = b"loopback-secret-loopback-secret-xx";

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// A signaling server on a free loopback port, with a known secret.
pub struct LocalSignaling {
    pub url: String,
}

impl LocalSignaling {
    pub async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = rb_signaling::serve(listener, rb_signaling::Config::new(SECRET)).await;
        });
        Self {
            url: format!("ws://{addr}/ws"),
        }
    }

    /// A token the relay accepts, valid for five minutes.
    pub fn token(&self, sid: &str, role: &str, fp: &str, pfp: &str) -> String {
        encode(
            &Header::new(Algorithm::HS256),
            &json!({"iss": "remotebridge", "aud": "rb-signaling", "exp": now() + 300,
                    "sid": sid, "role": role, "fp": fp, "pfp": pfp}),
            &EncodingKey::from_secret(SECRET),
        )
        .unwrap()
    }
}

enum ViewerEvent {
    Candidate(RTCIceCandidate),
    GatheringDone,
    Channel(Arc<dyn DataChannel>),
    Track(Arc<dyn TrackRemote>),
}

struct ViewerHandler {
    tx: mpsc::UnboundedSender<ViewerEvent>,
}

#[async_trait::async_trait]
impl PeerConnectionEventHandler for ViewerHandler {
    async fn on_ice_candidate(&self, event: RTCPeerConnectionIceEvent) {
        let _ = self.tx.send(ViewerEvent::Candidate(event.candidate));
    }
    async fn on_ice_gathering_state_change(&self, state: RTCIceGatheringState) {
        if state == RTCIceGatheringState::Complete {
            let _ = self.tx.send(ViewerEvent::GatheringDone);
        }
    }
    async fn on_data_channel(&self, channel: Arc<dyn DataChannel>) {
        let _ = self.tx.send(ViewerEvent::Channel(channel));
    }
    async fn on_track(&self, track: Arc<dyn TrackRemote>) {
        let _ = self.tx.send(ViewerEvent::Track(track));
    }
}

/// Packets counted per audio track id.
#[derive(Default)]
pub struct PacketCounts(StdMutex<HashMap<String, usize>>);

impl PacketCounts {
    fn bump(&self, id: &str) {
        *self.0.lock().unwrap().entry(id.to_owned()).or_default() += 1;
    }

    pub fn get(&self, id: &str) -> usize {
        self.0.lock().unwrap().get(id).copied().unwrap_or(0)
    }
}

/// What the viewer has seen so far. Cheap to clone.
#[derive(Clone, Default)]
pub struct ViewerState {
    channels: Arc<Mutex<HashMap<String, Arc<dyn DataChannel>>>>,
    open: Arc<Mutex<Vec<String>>>,
    pub video_packets: Arc<AtomicUsize>,
    pub audio_packets: Arc<PacketCounts>,
    /// `(kind, track id)` in arrival order.
    pub tracks: Arc<Mutex<Vec<(String, String)>>>,
    /// Text the host sent on `control` (clipboard).
    pub host_messages: Arc<Mutex<Vec<String>>>,
}

pub struct ScriptedViewer {
    pub state: ViewerState,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for ScriptedViewer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl ScriptedViewer {
    /// Join the room as the viewer. `identity` is the viewer's own DTLS
    /// identity; the host expects its fingerprint.
    pub async fn connect(
        signaling: &LocalSignaling,
        sid: &str,
        identity: &HostIdentity,
        host_fingerprint: &str,
    ) -> Self {
        let token = signaling.token(sid, "viewer", identity.fingerprint(), host_fingerprint);
        let (socket, _) = connect_async(format!("{}?token={token}", signaling.url))
            .await
            .unwrap();
        let (mut sink, mut stream) = socket.split();
        let (tx, mut rx) = mpsc::unbounded_channel();

        let mut media = MediaEngine::default();
        media.register_default_codecs().unwrap();
        let registry = register_default_interceptors(Registry::new(), &mut media).unwrap();
        let pc = PeerConnectionBuilder::new()
            .with_configuration(
                RTCConfigurationBuilder::new()
                    .with_certificates(vec![identity.certificate().clone()])
                    .build(),
            )
            .with_media_engine(media)
            .with_interceptor_registry(registry)
            .with_setting_engine(
                SettingEngineBuilder::new()
                    .with_include_loopback_candidate(true)
                    .build(),
            )
            .with_handler(Arc::new(ViewerHandler { tx }))
            .with_runtime(webrtc::runtime::default_runtime().unwrap())
            .with_udp_addrs(vec!["127.0.0.1:0"])
            .build()
            .await
            .unwrap();

        let state = ViewerState::default();
        let st = state.clone();
        let task = tokio::spawn(async move {
            sink.send(Message::Text(json!({"type": "ready"}).to_string().into()))
                .await
                .unwrap();
            loop {
                tokio::select! {
                    incoming = stream.next() => {
                        let Some(Ok(Message::Text(text))) = incoming else {
                            if incoming.is_none() { break } else { continue }
                        };
                        let v: Value = serde_json::from_str(text.as_str()).unwrap();
                        match v["type"].as_str() {
                            Some("offer") => {
                                let offer = RTCSessionDescription::offer(
                                    v["sdp"].as_str().unwrap().to_owned(),
                                )
                                .unwrap();
                                pc.set_remote_description(offer).await.unwrap();
                                let answer = pc.create_answer(None).await.unwrap();
                                pc.set_local_description(answer).await.unwrap();
                                let sdp = pc.local_description().await.unwrap().sdp;
                                let msg = json!({"type": "answer", "sdp": sdp});
                                sink.send(Message::Text(msg.to_string().into())).await.unwrap();
                            }
                            Some("ice") => {
                                if !v["candidate"].is_null()
                                    && let Ok(init) = serde_json::from_value::<RTCIceCandidateInit>(
                                        v["candidate"].clone(),
                                    )
                                {
                                    let _ = pc.add_ice_candidate(init).await;
                                }
                            }
                            _ => {}
                        }
                    }
                    Some(event) = rx.recv() => match event {
                        ViewerEvent::Candidate(c) => {
                            if let Ok(init) = c.to_json() {
                                let msg = json!({"type": "ice",
                                    "candidate": serde_json::to_value(init).unwrap()});
                                let _ = sink.send(Message::Text(msg.to_string().into())).await;
                            }
                        }
                        ViewerEvent::GatheringDone => {
                            let msg = json!({"type": "ice", "candidate": null});
                            let _ = sink.send(Message::Text(msg.to_string().into())).await;
                        }
                        ViewerEvent::Channel(channel) => {
                            let label = channel.label().await.unwrap();
                            st.channels.lock().await.insert(label.clone(), channel.clone());
                            let st2 = st.clone();
                            tokio::spawn(async move {
                                while let Some(event) = channel.poll().await {
                                    match event {
                                        DataChannelEvent::OnOpen => {
                                            st2.open.lock().await.push(label.clone());
                                        }
                                        DataChannelEvent::OnMessage(m) if label == "control" => {
                                            st2.host_messages
                                                .lock()
                                                .await
                                                .push(String::from_utf8_lossy(&m.data).into_owned());
                                        }
                                        DataChannelEvent::OnClose => break,
                                        _ => {}
                                    }
                                }
                            });
                        }
                        ViewerEvent::Track(track) => {
                            let kind = format!("{:?}", track.kind().await);
                            let id = track.track_id().await;
                            st.tracks.lock().await.push((kind.clone(), id.clone()));
                            let st2 = st.clone();
                            tokio::spawn(async move {
                                while let Some(event) = track.poll().await {
                                    if let TrackRemoteEvent::OnRtpPacket(_) = event {
                                        if kind.contains("Video") {
                                            st2.video_packets.fetch_add(1, Ordering::SeqCst);
                                        } else {
                                            st2.audio_packets.bump(&id);
                                        }
                                    }
                                }
                            });
                        }
                    },
                }
            }
        });
        Self { state, task }
    }

    /// A data channel by label, once the host has announced it.
    pub async fn channel(&self, label: &str) -> Arc<dyn DataChannel> {
        for _ in 0..200 {
            if let Some(c) = self.state.channels.lock().await.get(label) {
                return c.clone();
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("viewer never saw the {label} channel");
    }

    /// Wait until `label` is open on the viewer side.
    pub async fn wait_open(&self, label: &str) {
        for _ in 0..400 {
            if self.state.open.lock().await.iter().any(|l| l == label) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("channel {label} never opened on the viewer side");
    }

    /// Send one JSON message on a channel.
    pub async fn send(&self, label: &str, message: Value) {
        self.channel(label)
            .await
            .send_text(&message.to_string())
            .await
            .unwrap();
    }
}

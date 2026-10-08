//! Host-side client for the signaling relay (`AGENTS.md`, "Media path contract").
//!
//! JSON text frames over `wss://…?token=<signalingToken>`.
//!
//! * client to server (host): `offer`, `ice`, `bye` (the relay refuses anything else from a host)
//! * server to client: the viewer's `ready`, `answer`, `ice`, `bye` relayed
//!   unchanged, plus `peer` (the other side joined or left) and `error`.

use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;

/// What the host sends.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ClientMessage {
    Offer {
        sdp: String,
    },
    /// `candidate: None` is the end-of-candidates marker (sent as JSON `null`).
    Ice {
        candidate: Option<Value>,
    },
    Bye,
}

/// What arrives from the relay.
#[derive(Debug, Clone, PartialEq)]
pub enum ServerEvent {
    /// The other side joined (`true`) or left (`false`).
    Peer {
        present: bool,
    },
    /// The viewer is ready for an offer.
    Ready,
    Answer {
        sdp: String,
    },
    Ice {
        candidate: Option<Value>,
    },
    Bye,
    /// The relay refused something we sent.
    Error {
        message: String,
    },
    /// The connection ended. `code` is the WebSocket close code if one came.
    Closed {
        code: Option<u16>,
    },
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum Wire {
    Peer { present: bool },
    Ready,
    Answer { sdp: String },
    Ice { candidate: Option<Value> },
    Bye,
    Error { message: String },
}

/// Decode one text frame from the relay. `None` for anything we do not know.
pub fn parse_server_message(text: &str) -> Option<ServerEvent> {
    Some(match serde_json::from_str::<Wire>(text).ok()? {
        Wire::Peer { present } => ServerEvent::Peer { present },
        Wire::Ready => ServerEvent::Ready,
        Wire::Answer { sdp } => ServerEvent::Answer { sdp },
        Wire::Ice { candidate } => ServerEvent::Ice { candidate },
        Wire::Bye => ServerEvent::Bye,
        Wire::Error { message } => ServerEvent::Error { message },
    })
}

/// The WebSocket URL with the token attached.
pub fn with_token(signaling_url: &str, token: &str) -> String {
    let sep = if signaling_url.contains('?') {
        '&'
    } else {
        '?'
    };
    format!("{signaling_url}{sep}token={token}")
}

#[derive(Debug)]
pub enum SignalingError {
    Connect(String),
    Closed,
}

impl std::fmt::Display for SignalingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Connect(why) => write!(f, "cannot connect to signaling: {why}"),
            Self::Closed => f.write_str("signaling connection is closed"),
        }
    }
}

impl std::error::Error for SignalingError {}

/// A live signaling connection.
pub struct SignalingClient {
    outgoing: mpsc::UnboundedSender<ClientMessage>,
    events: mpsc::UnboundedReceiver<ServerEvent>,
    task: JoinHandle<()>,
}

impl SignalingClient {
    /// Connect to `signaling_url` with the token. The token is never logged.
    pub async fn connect(signaling_url: &str, token: &str) -> Result<Self, SignalingError> {
        let url = with_token(signaling_url, token);
        let (socket, _) = connect_async(url)
            .await
            .map_err(|e| SignalingError::Connect(without_url(&e.to_string())))?;
        let (mut sink, mut stream) = socket.split();
        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<ClientMessage>();
        let (event_tx, event_rx) = mpsc::unbounded_channel::<ServerEvent>();

        let task = tokio::spawn(async move {
            let mut close_code = None;
            loop {
                tokio::select! {
                    message = out_rx.recv() => match message {
                        Some(m) => {
                            let Ok(text) = serde_json::to_string(&m) else { continue };
                            if sink.send(Message::Text(text.into())).await.is_err() {
                                break;
                            }
                        }
                        None => {
                            let _ = sink.send(Message::Close(None)).await;
                            break;
                        }
                    },
                    incoming = stream.next() => match incoming {
                        Some(Ok(Message::Text(text))) => {
                            if let Some(event) = parse_server_message(text.as_str())
                                && event_tx.send(event).is_err()
                            {
                                break;
                            }
                        }
                        Some(Ok(Message::Close(frame))) => {
                            close_code = frame.map(|f| u16::from(f.code));
                            break;
                        }
                        Some(Ok(_)) => {}
                        Some(Err(_)) | None => break,
                    },
                }
            }
            let _ = event_tx.send(ServerEvent::Closed { code: close_code });
        });

        Ok(Self {
            outgoing: out_tx,
            events: event_rx,
            task,
        })
    }

    pub fn send(&self, message: ClientMessage) -> Result<(), SignalingError> {
        self.outgoing
            .send(message)
            .map_err(|_| SignalingError::Closed)
    }

    /// Next event from the relay. After `Closed`, returns `None`.
    pub async fn next_event(&mut self) -> Option<ServerEvent> {
        self.events.recv().await
    }

    /// Say goodbye and drop the connection.
    pub fn close(self) {
        let _ = self.outgoing.send(ClientMessage::Bye);
        self.task.abort();
    }
}

impl Drop for SignalingClient {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Error text can include the request URL, which carries the token.
fn without_url(message: &str) -> String {
    message
        .split_whitespace()
        .filter(|w| !w.contains("token="))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn host_messages_serialize_to_the_contract() {
        let offer = serde_json::to_value(ClientMessage::Offer { sdp: "v=0".into() }).unwrap();
        assert_eq!(offer, json!({"type": "offer", "sdp": "v=0"}));
        let ice = serde_json::to_value(ClientMessage::Ice {
            candidate: Some(json!({"candidate": "candidate:1"})),
        })
        .unwrap();
        assert_eq!(
            ice,
            json!({"type": "ice", "candidate": {"candidate": "candidate:1"}})
        );
        let end = serde_json::to_value(ClientMessage::Ice { candidate: None }).unwrap();
        assert_eq!(end, json!({"type": "ice", "candidate": null}));
        assert_eq!(
            serde_json::to_value(ClientMessage::Bye).unwrap(),
            json!({"type": "bye"})
        );
    }

    #[test]
    fn relay_messages_parse() {
        assert_eq!(
            parse_server_message(r#"{"type":"peer","present":true}"#),
            Some(ServerEvent::Peer { present: true })
        );
        assert_eq!(
            parse_server_message(r#"{"type":"ready"}"#),
            Some(ServerEvent::Ready)
        );
        assert_eq!(
            parse_server_message(r#"{"type":"answer","sdp":"v=0"}"#),
            Some(ServerEvent::Answer { sdp: "v=0".into() })
        );
        assert_eq!(
            parse_server_message(r#"{"type":"ice","candidate":null}"#),
            Some(ServerEvent::Ice { candidate: None })
        );
        assert_eq!(
            parse_server_message(r#"{"type":"ice","candidate":{"candidate":"x"}}"#),
            Some(ServerEvent::Ice {
                candidate: Some(json!({"candidate": "x"}))
            })
        );
        assert_eq!(
            parse_server_message(r#"{"type":"bye"}"#),
            Some(ServerEvent::Bye)
        );
        assert_eq!(
            parse_server_message(r#"{"type":"error","message":"nope"}"#),
            Some(ServerEvent::Error {
                message: "nope".into()
            })
        );
    }

    #[test]
    fn unknown_or_broken_frames_are_ignored() {
        for junk in [
            "",
            "x",
            "[]",
            r#"{"type":"shell"}"#,
            r#"{"type":"peer"}"#,
            r#"{"nothing":1}"#,
        ] {
            assert_eq!(parse_server_message(junk), None, "{junk}");
        }
    }

    #[test]
    fn token_is_attached_to_the_url() {
        assert_eq!(
            with_token("wss://s.example/ws", "abc"),
            "wss://s.example/ws?token=abc"
        );
        assert_eq!(
            with_token("wss://s.example/ws?x=1", "abc"),
            "wss://s.example/ws?x=1&token=abc"
        );
    }

    #[test]
    fn error_text_never_carries_the_token() {
        let msg = "HTTP error: 401 for ws://127.0.0.1:1/ws?token=SECRET.TOKEN.VALUE failed";
        let cleaned = without_url(msg);
        assert!(!cleaned.contains("SECRET"), "{cleaned}");
        assert!(cleaned.contains("401"));
    }
}

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::sync::mpsc;
use tokio::time::Instant;

use crate::auth::{Claims, Role, verify_token};
use crate::rooms::{Outgoing, Rooms, error_message};

#[derive(Clone)]
pub struct Config {
    /// `SIGNALING_JWT_SECRET`: the HS256 key the website signs tokens with.
    pub jwt_secret: Vec<u8>,
    /// Largest accepted message.
    pub max_message_bytes: usize,
    /// Messages per second per socket.
    pub messages_per_second: u32,
}

impl Config {
    pub fn new(jwt_secret: impl Into<Vec<u8>>) -> Self {
        Self {
            jwt_secret: jwt_secret.into(),
            max_message_bytes: 64 * 1024,
            messages_per_second: 50,
        }
    }
}

#[derive(Clone)]
struct App {
    config: Arc<Config>,
    rooms: Arc<Rooms>,
}

#[derive(Deserialize)]
struct TokenQuery {
    token: Option<String>,
}

pub fn router(config: Config) -> Router {
    let app = App {
        config: Arc::new(config),
        rooms: Arc::new(Rooms::new()),
    };
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/", get(ws_handler))
        .route("/ws", get(ws_handler))
        .with_state(app)
}

/// Serve on an already bound listener until it fails.
pub async fn serve(listener: tokio::net::TcpListener, config: Config) -> std::io::Result<()> {
    axum::serve(listener, router(config)).await
}

async fn ws_handler(
    State(app): State<App>,
    Query(query): Query<TokenQuery>,
    ws: WebSocketUpgrade,
) -> Response {
    // Note: the request URI carries the token, so it is never logged.
    let Some(token) = query.token else {
        return (StatusCode::UNAUTHORIZED, "token required").into_response();
    };
    let claims = match verify_token(&token, &app.config.jwt_secret) {
        Ok(c) => c,
        Err(_) => return (StatusCode::UNAUTHORIZED, "invalid or expired token").into_response(),
    };
    let max = app.config.max_message_bytes;
    ws.max_message_size(max)
        .max_frame_size(max)
        .on_upgrade(move |socket| handle_socket(app, claims, socket))
}

/// What a role may send.
fn allowed(role: Role, kind: &str) -> bool {
    match role {
        Role::Host => matches!(kind, "offer" | "ice" | "bye"),
        Role::Viewer => matches!(kind, "ready" | "answer" | "ice" | "bye"),
    }
}

/// Check a client message. `Ok` means relay it unchanged.
fn check_message(role: Role, text: &str) -> Result<(), &'static str> {
    let value: serde_json::Value = serde_json::from_str(text).map_err(|_| "malformed message")?;
    let kind = value
        .get("type")
        .and_then(|t| t.as_str())
        .ok_or("malformed message")?;
    if !allowed(role, kind) {
        return Err("message type not allowed for this role");
    }
    let well_formed = match kind {
        "offer" | "answer" => value.get("sdp").is_some_and(serde_json::Value::is_string),
        // `candidate: null` marks the end of candidates.
        "ice" => value.get("candidate").is_some(),
        _ => true,
    };
    if well_formed {
        Ok(())
    } else {
        Err("malformed message")
    }
}

struct RateLimiter {
    capacity: f64,
    tokens: f64,
    per_second: f64,
    last: Instant,
}

impl RateLimiter {
    fn new(per_second: u32) -> Self {
        let rate = f64::from(per_second.max(1));
        Self {
            capacity: rate,
            tokens: rate,
            per_second: rate,
            last: Instant::now(),
        }
    }

    fn allow(&mut self) -> bool {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last).as_secs_f64();
        self.last = now;
        self.tokens = (self.tokens + elapsed * self.per_second).min(self.capacity);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

async fn handle_socket(app: App, claims: Claims, socket: WebSocket) {
    let connected = Instant::now();
    let Claims { sid, role, exp, .. } = claims;
    let (mut sink, mut stream) = socket.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<Outgoing>();

    let writer = tokio::spawn(async move {
        while let Some(out) = rx.recv().await {
            match out {
                Outgoing::Text(text) => {
                    if sink.send(Message::Text(text.into())).await.is_err() {
                        break;
                    }
                }
                Outgoing::Close { code, reason } => {
                    let _ = sink
                        .send(Message::Close(Some(CloseFrame {
                            code,
                            reason: reason.into(),
                        })))
                        .await;
                    break;
                }
            }
        }
    });

    let peer_id = app.rooms.join(&sid, role, tx.clone());
    tracing::info!(sid = %sid, role = role.as_str(), "connected");

    let expires_in = Duration::from_secs(exp.saturating_sub(now_unix()));
    let expiry = tokio::time::sleep(expires_in);
    tokio::pin!(expiry);
    let mut limiter = RateLimiter::new(app.config.messages_per_second);
    let mut over_limit = 0u32;

    let reason: &'static str = loop {
        tokio::select! {
            () = &mut expiry => {
                let _ = tx.send(Outgoing::Close { code: 4001, reason: "token expired" });
                break "token_expired";
            }
            incoming = stream.next() => match incoming {
                None => break "disconnected",
                Some(Err(_)) => break "protocol_error",
                Some(Ok(Message::Close(_))) => break "client_closed",
                Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
                Some(Ok(Message::Binary(_))) => {
                    let _ = tx.send(Outgoing::Text(error_message("binary frames are not supported")));
                }
                Some(Ok(Message::Text(text))) => {
                    if !limiter.allow() {
                        over_limit += 1;
                        if over_limit == 1 || over_limit.is_multiple_of(25) {
                            let _ = tx.send(Outgoing::Text(error_message("rate limit exceeded")));
                        }
                        if over_limit > 500 {
                            let _ = tx.send(Outgoing::Close { code: 1008, reason: "rate limit exceeded" });
                            break "rate_limited";
                        }
                        continue;
                    }
                    let text = text.as_str();
                    if text.len() > app.config.max_message_bytes {
                        let _ = tx.send(Outgoing::Text(error_message("message too large")));
                        continue;
                    }
                    match check_message(role, text) {
                        Ok(()) => {
                            app.rooms.relay(&sid, role, text.to_owned());
                        }
                        Err(why) => {
                            let _ = tx.send(Outgoing::Text(error_message(why)));
                        }
                    }
                }
            }
        }
    };

    app.rooms.leave(&sid, role, peer_id);
    drop(tx);
    let _ = writer.await;
    tracing::info!(
        sid = %sid,
        role = role.as_str(),
        reason,
        connected_ms = u64::try_from(connected.elapsed().as_millis()).unwrap_or(u64::MAX),
        "disconnected"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_role_may_only_send_its_own_message_types() {
        for kind in ["offer", "ice", "bye"] {
            assert!(allowed(Role::Host, kind), "host {kind}");
        }
        for kind in ["ready", "answer", "ice", "bye"] {
            assert!(allowed(Role::Viewer, kind), "viewer {kind}");
        }
        for kind in ["ready", "answer", "peer", "error", "shell", ""] {
            assert!(!allowed(Role::Host, kind), "host must not send {kind}");
        }
        for kind in ["offer", "peer", "error", "shell", ""] {
            assert!(!allowed(Role::Viewer, kind), "viewer must not send {kind}");
        }
    }

    #[test]
    fn messages_are_checked_for_shape() {
        assert!(check_message(Role::Host, r#"{"type":"offer","sdp":"v=0"}"#).is_ok());
        assert!(check_message(Role::Host, r#"{"type":"ice","candidate":null}"#).is_ok());
        assert!(
            check_message(
                Role::Host,
                r#"{"type":"ice","candidate":{"candidate":"x"}}"#
            )
            .is_ok()
        );
        assert!(check_message(Role::Viewer, r#"{"type":"ready"}"#).is_ok());
        assert!(check_message(Role::Viewer, r#"{"type":"answer","sdp":"v=0"}"#).is_ok());
        assert!(check_message(Role::Host, r#"{"type":"bye"}"#).is_ok());

        for bad in [
            "",
            "nope",
            "[]",
            r#"{"type":5}"#,
            r#"{"nothing":1}"#,
            r#"{"type":"offer"}"#,
            r#"{"type":"offer","sdp":5}"#,
            r#"{"type":"ice"}"#,
        ] {
            assert_eq!(
                check_message(Role::Host, bad),
                Err("malformed message"),
                "{bad}"
            );
        }
        assert_eq!(
            check_message(Role::Viewer, r#"{"type":"offer","sdp":"v=0"}"#),
            Err("message type not allowed for this role")
        );
    }

    #[tokio::test(start_paused = true)]
    async fn rate_limiter_allows_a_burst_then_refills() {
        let mut limiter = RateLimiter::new(50);
        let allowed_now = (0..100).filter(|_| limiter.allow()).count();
        assert_eq!(allowed_now, 50);
        tokio::time::advance(Duration::from_millis(100)).await;
        assert_eq!((0..100).filter(|_| limiter.allow()).count(), 5);
        tokio::time::advance(Duration::from_secs(10)).await;
        assert_eq!(
            (0..100).filter(|_| limiter.allow()).count(),
            50,
            "capped at one burst"
        );
    }
}

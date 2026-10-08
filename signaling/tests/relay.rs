//! Integration tests: a real server on a local port and two scripted clients.

use std::net::SocketAddr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures_util::{SinkExt, StreamExt};
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use rb_signaling::{Config, serve};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

const SECRET: &[u8] = b"integration-secret-integration-secret";
const SID: &str = "6f1c3d7e-2b4a-4c8e-9d10-0a1b2c3d4e5f";

type Client = WebSocketStream<MaybeTlsStream<TcpStream>>;

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

async fn start(config: Config) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = serve(listener, config).await;
    });
    addr
}

async fn start_default() -> SocketAddr {
    start(Config::new(SECRET)).await
}

fn claims(sid: &str, role: &str, ttl: i64) -> Value {
    json!({
        "iss": "remotebridge", "aud": "rb-signaling",
        "exp": (now() as i64 + ttl) as u64,
        "sid": sid, "role": role,
        "fp": "sha-256 AA:BB", "pfp": "sha-256 CC:DD",
    })
}

fn token(claims: &Value) -> String {
    encode(
        &Header::new(Algorithm::HS256),
        claims,
        &EncodingKey::from_secret(SECRET),
    )
    .unwrap()
}

// The tungstenite error is large; fine for a test helper.
#[allow(clippy::result_large_err)]
async fn connect_with(
    addr: SocketAddr,
    token: &str,
) -> Result<Client, tokio_tungstenite::tungstenite::Error> {
    connect_async(format!("ws://{addr}/ws?token={token}"))
        .await
        .map(|(c, _)| c)
}

async fn join(addr: SocketAddr, sid: &str, role: &str) -> Client {
    connect_with(addr, &token(&claims(sid, role, 300)))
        .await
        .expect("handshake")
}

async fn next_text(c: &mut Client) -> Value {
    loop {
        match tokio::time::timeout(Duration::from_secs(5), c.next()).await {
            Ok(Some(Ok(Message::Text(t)))) => return serde_json::from_str(t.as_str()).unwrap(),
            Ok(Some(Ok(Message::Ping(_) | Message::Pong(_)))) => {}
            other => panic!("expected a text message, got {other:?}"),
        }
    }
}

/// `None` if nothing arrives soon.
async fn maybe_text(c: &mut Client, wait_ms: u64) -> Option<Value> {
    match tokio::time::timeout(Duration::from_millis(wait_ms), c.next()).await {
        Ok(Some(Ok(Message::Text(t)))) => Some(serde_json::from_str(t.as_str()).unwrap()),
        _ => None,
    }
}

async fn send(c: &mut Client, v: Value) {
    c.send(Message::Text(v.to_string().into())).await.unwrap();
}

/// Both sides see the presence messages that a join produces.
async fn pair(addr: SocketAddr, sid: &str) -> (Client, Client) {
    let mut host = join(addr, sid, "host").await;
    let mut viewer = join(addr, sid, "viewer").await;
    assert_eq!(
        next_text(&mut host).await,
        json!({"type": "peer", "present": true})
    );
    assert_eq!(
        next_text(&mut viewer).await,
        json!({"type": "peer", "present": true})
    );
    (host, viewer)
}

#[tokio::test]
async fn health_check_answers_ok() {
    let addr = start_default().await;
    let mut s = TcpStream::connect(addr).await.unwrap();
    s.write_all(b"GET /healthz HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut body = String::new();
    s.read_to_string(&mut body).await.unwrap();
    assert!(body.starts_with("HTTP/1.1 200"), "{body}");
    assert!(body.trim_end().ends_with("ok"), "{body}");
}

#[tokio::test]
async fn offer_answer_and_ice_flow_between_host_and_viewer() {
    let addr = start_default().await;
    let (mut host, mut viewer) = pair(addr, SID).await;

    // The viewer says it is ready; the host offers; the viewer answers; both trickle ICE.
    send(&mut viewer, json!({"type": "ready"})).await;
    assert_eq!(next_text(&mut host).await, json!({"type": "ready"}));

    let offer = json!({"type": "offer", "sdp": "v=0\r\na=fingerprint:sha-256 AA:BB\r\n"});
    send(&mut host, offer.clone()).await;
    assert_eq!(next_text(&mut viewer).await, offer, "relayed unchanged");

    let answer = json!({"type": "answer", "sdp": "v=0\r\na=fingerprint:sha-256 CC:DD\r\n"});
    send(&mut viewer, answer.clone()).await;
    assert_eq!(next_text(&mut host).await, answer);

    let ice = json!({"type": "ice", "candidate": {"candidate": "candidate:1 1 udp 1 10.0.0.1 9 typ host", "sdpMid": "0"}});
    send(&mut host, ice.clone()).await;
    assert_eq!(next_text(&mut viewer).await, ice);
    let end_of_candidates = json!({"type": "ice", "candidate": null});
    send(&mut viewer, end_of_candidates.clone()).await;
    assert_eq!(next_text(&mut host).await, end_of_candidates);

    send(&mut host, json!({"type": "bye"})).await;
    assert_eq!(next_text(&mut viewer).await, json!({"type": "bye"}));
}

#[tokio::test]
async fn the_relay_does_not_reorder_messages() {
    let addr = start_default().await;
    let (mut host, mut viewer) = pair(addr, SID).await;
    for i in 0..30 {
        send(&mut host, json!({"type": "ice", "candidate": {"n": i}})).await;
    }
    for i in 0..30 {
        assert_eq!(next_text(&mut viewer).await["candidate"]["n"], i);
    }
}

#[tokio::test]
async fn peers_hear_when_the_other_side_leaves_and_returns() {
    let addr = start_default().await;
    let (mut host, mut viewer) = pair(addr, SID).await;
    viewer.close(None).await.unwrap();
    assert_eq!(
        next_text(&mut host).await,
        json!({"type": "peer", "present": false})
    );

    let _viewer2 = join(addr, SID, "viewer").await;
    assert_eq!(
        next_text(&mut host).await,
        json!({"type": "peer", "present": true})
    );
}

#[tokio::test]
async fn a_host_that_joins_after_the_viewer_is_told_the_viewer_is_there() {
    let addr = start_default().await;
    let mut viewer = join(addr, SID, "viewer").await;
    assert!(maybe_text(&mut viewer, 200).await.is_none(), "alone");
    let mut host = join(addr, SID, "host").await;
    assert_eq!(
        next_text(&mut host).await,
        json!({"type": "peer", "present": true})
    );
    assert_eq!(
        next_text(&mut viewer).await,
        json!({"type": "peer", "present": true})
    );
}

#[tokio::test]
async fn rooms_are_isolated_by_session() {
    let addr = start_default().await;
    let (mut host1, _viewer1) = pair(addr, "session-one").await;
    let (_host2, mut viewer2) = pair(addr, "session-two").await;
    send(
        &mut host1,
        json!({"type": "offer", "sdp": "for session one"}),
    )
    .await;
    assert!(
        maybe_text(&mut viewer2, 300).await.is_none(),
        "nothing crosses sessions"
    );
}

#[tokio::test]
async fn bad_tokens_are_refused_at_the_handshake() {
    let addr = start_default().await;
    let good = claims(SID, "host", 300);

    let mut cases: Vec<(&str, String)> = vec![
        ("no token", String::new()),
        ("garbage", "not-a-token".into()),
    ];
    let wrong_secret = encode(
        &Header::new(Algorithm::HS256),
        &good,
        &EncodingKey::from_secret(b"some-other-secret-some-other-secret"),
    )
    .unwrap();
    cases.push(("wrong secret", wrong_secret));
    for (name, patch) in [
        ("wrong issuer", json!({"iss": "evil"})),
        ("wrong audience", json!({"aud": "other"})),
        ("expired", json!({"exp": now() - 5})),
        ("bad role", json!({"role": "admin"})),
        ("bad session id", json!({"sid": "../x"})),
    ] {
        let mut c = good.clone();
        for (k, v) in patch.as_object().unwrap() {
            c[k] = v.clone();
        }
        cases.push((name, token(&c)));
    }
    let mut hs512 = Header::new(Algorithm::HS512);
    hs512.typ = Some("JWT".into());
    cases.push((
        "wrong algorithm",
        encode(&hs512, &good, &EncodingKey::from_secret(SECRET)).unwrap(),
    ));

    for (name, t) in cases {
        match connect_with(addr, &t).await {
            Err(tokio_tungstenite::tungstenite::Error::Http(resp)) => {
                assert_eq!(resp.status(), 401, "{name}");
            }
            other => panic!(
                "{name}: expected a 401, got {:?}",
                other.map(|_| "connected")
            ),
        }
    }
}

#[tokio::test]
async fn each_role_is_limited_to_its_own_messages() {
    let addr = start_default().await;
    let (mut host, mut viewer) = pair(addr, SID).await;

    // A viewer cannot offer; a host cannot say ready or answer.
    send(&mut viewer, json!({"type": "offer", "sdp": "v=0"})).await;
    assert_eq!(
        next_text(&mut viewer).await,
        json!({"type": "error", "message": "message type not allowed for this role"})
    );
    send(&mut host, json!({"type": "ready"})).await;
    assert_eq!(next_text(&mut host).await["type"], "error");
    send(&mut host, json!({"type": "answer", "sdp": "v=0"})).await;
    assert_eq!(next_text(&mut host).await["type"], "error");

    // Clients cannot forge server messages, and junk is refused.
    for forged in [
        json!({"type": "peer", "present": false}),
        json!({"type": "error", "message": "x"}),
    ] {
        send(&mut viewer, forged).await;
        assert_eq!(next_text(&mut viewer).await["type"], "error");
    }
    viewer.send(Message::Text("not json".into())).await.unwrap();
    assert_eq!(
        next_text(&mut viewer).await,
        json!({"type": "error", "message": "malformed message"})
    );
    viewer
        .send(Message::Binary(vec![1, 2, 3].into()))
        .await
        .unwrap();
    assert_eq!(next_text(&mut viewer).await["type"], "error");

    // None of it reached the other side.
    assert!(maybe_text(&mut host, 300).await.is_none());
    assert!(maybe_text(&mut viewer, 100).await.is_none());

    // And the connection is still good afterwards.
    send(&mut viewer, json!({"type": "ready"})).await;
    assert_eq!(next_text(&mut host).await, json!({"type": "ready"}));
}

#[tokio::test]
async fn a_second_socket_with_the_same_role_replaces_the_first() {
    let addr = start_default().await;
    let (mut host1, mut viewer) = pair(addr, SID).await;
    let mut host2 = join(addr, SID, "host").await;

    // The old host is closed with the "replaced" code.
    let closed = loop {
        match tokio::time::timeout(Duration::from_secs(5), host1.next()).await {
            Ok(Some(Ok(Message::Close(frame)))) => break frame,
            Ok(Some(Ok(_))) => {}
            other => panic!("expected a close frame, got {other:?}"),
        }
    };
    assert_eq!(u16::from(closed.expect("close frame").code), 4000);

    // The viewer is told a host is present; the new host sees the viewer.
    assert_eq!(
        next_text(&mut viewer).await,
        json!({"type": "peer", "present": true})
    );
    assert_eq!(
        next_text(&mut host2).await,
        json!({"type": "peer", "present": true})
    );
    send(
        &mut host2,
        json!({"type": "offer", "sdp": "from the new host"}),
    )
    .await;
    assert_eq!(next_text(&mut viewer).await["sdp"], "from the new host");
}

#[tokio::test]
async fn oversize_messages_never_reach_the_peer() {
    let addr = start(Config {
        max_message_bytes: 1024,
        ..Config::new(SECRET)
    })
    .await;
    let (mut host, mut viewer) = pair(addr, SID).await;
    let big = json!({"type": "offer", "sdp": "x".repeat(2048)}).to_string();
    let _ = host.send(Message::Text(big.into())).await;

    // The offender is disconnected, so the first (and only) thing the viewer
    // hears is the host leaving, never the oversize offer.
    assert_eq!(
        next_text(&mut viewer).await,
        json!({"type": "peer", "present": false})
    );
    assert!(
        maybe_text(&mut viewer, 400).await.is_none(),
        "oversize message must not be relayed"
    );
}

#[tokio::test]
async fn the_rate_limit_drops_excess_messages() {
    let addr = start(Config {
        messages_per_second: 10,
        ..Config::new(SECRET)
    })
    .await;
    let (mut host, mut viewer) = pair(addr, SID).await;
    for i in 0..200 {
        send(&mut host, json!({"type": "ice", "candidate": {"n": i}})).await;
    }
    let mut relayed = 0;
    while maybe_text(&mut viewer, 500).await.is_some() {
        relayed += 1;
    }
    assert!(
        (10..=40).contains(&relayed),
        "relayed {relayed} of 200 at 10/s"
    );

    let first_reply = next_text(&mut host).await;
    assert_eq!(
        first_reply,
        json!({"type": "error", "message": "rate limit exceeded"})
    );
}

#[tokio::test]
async fn an_expiring_token_closes_the_socket_and_the_peer_is_told() {
    let addr = start_default().await;
    let mut host = connect_with(addr, &token(&claims(SID, "host", 300)))
        .await
        .unwrap();
    let mut viewer = connect_with(addr, &token(&claims(SID, "viewer", 2)))
        .await
        .unwrap();
    assert_eq!(next_text(&mut host).await["present"], true);
    assert_eq!(next_text(&mut viewer).await["present"], true);

    let code = loop {
        match tokio::time::timeout(Duration::from_secs(8), viewer.next()).await {
            Ok(Some(Ok(Message::Close(Some(frame))))) => break u16::from(frame.code),
            Ok(Some(Ok(_))) => {}
            other => panic!("expected the token expiry close, got {other:?}"),
        }
    };
    assert_eq!(code, 4001);
    assert_eq!(
        next_text(&mut host).await,
        json!({"type": "peer", "present": false})
    );
}

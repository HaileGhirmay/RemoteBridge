use std::collections::{HashSet, VecDeque};
use std::sync::Mutex;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use p256::ecdsa::signature::{Signer, Verifier};
use p256::ecdsa::{Signature, SigningKey, VerifyingKey};
use serde_json::{Value, json};

use super::*;
use crate::fakes::FakeClock;
use crate::protocol::b64url;
use crate::protocol::nonce::OsNonceSource;
use crate::protocol::types::{Decision, EndReason, SessionAction};
use crate::types::{PublicKeyJwk, RawSignature, SignatureEncoding};
use crate::{PlatformError, PlatformResult};

const DEVICE: &str = "6f1c3d7e-2b4a-4c8e-9d10-0a1b2c3d4e5f";
const SESSION: &str = "a2b4c6d8-1111-4222-8333-444455556666";
const NOW_MS: i64 = 1_791_462_818_897;

/// A real P-256 key that answers in DER, like OpenSSL / Security.framework,
/// so the DER to P1363 path is exercised end to end.
struct SoftKey {
    signing: SigningKey,
    created: Mutex<bool>,
}

impl SoftKey {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            signing: SigningKey::from_slice(&[0x11; 32]).unwrap(),
            created: Mutex::new(false),
        })
    }

    fn verifying(&self) -> VerifyingKey {
        *self.signing.verifying_key()
    }

    fn jwk(&self) -> PublicKeyJwk {
        let point = self.verifying().to_sec1_point(false);
        PublicKeyJwk {
            x: b64url(point.x().unwrap().as_slice()),
            y: b64url(point.y().unwrap().as_slice()),
        }
    }
}

impl KeyStore for SoftKey {
    fn public_key(&self) -> PlatformResult<Option<PublicKeyJwk>> {
        Ok(self.created.lock().unwrap().then(|| self.jwk()))
    }

    fn create_key(&self) -> PlatformResult<PublicKeyJwk> {
        *self.created.lock().unwrap() = true;
        Ok(self.jwk())
    }

    fn sign(&self, message: &[u8]) -> PlatformResult<RawSignature> {
        if !*self.created.lock().unwrap() {
            return Err(PlatformError::KeyNotFound);
        }
        let sig: Signature = self.signing.sign(message);
        Ok(RawSignature {
            encoding: SignatureEncoding::Der,
            bytes: sig.to_der().as_bytes().to_vec(),
        })
    }

    fn wipe(&self) -> PlatformResult<()> {
        *self.created.lock().unwrap() = false;
        Ok(())
    }
}

#[derive(Clone, Default)]
struct FakeTransport {
    script: Arc<Mutex<VecDeque<Result<HttpResponse, TransportError>>>>,
    sent: Arc<Mutex<Vec<HttpRequest>>>,
}

impl FakeTransport {
    fn push(&self, r: Result<HttpResponse, TransportError>) {
        self.script.lock().unwrap().push_back(r);
    }

    fn sent(&self) -> Vec<HttpRequest> {
        self.sent.lock().unwrap().clone()
    }
}

impl Transport for FakeTransport {
    async fn post(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
        self.sent.lock().unwrap().push(request);
        self.script
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| Err(TransportError::Other("nothing scripted".into())))
    }
}

fn ok(body: &str) -> Result<HttpResponse, TransportError> {
    Ok(HttpResponse {
        status: 200,
        body: body.as_bytes().to_vec(),
    })
}

fn status(code: u16, body: &str) -> Result<HttpResponse, TransportError> {
    Ok(HttpResponse {
        status: code,
        body: body.as_bytes().to_vec(),
    })
}

fn poll_ok() -> Result<HttpResponse, TransportError> {
    ok(include_str!("../fixtures/poll_response.json"))
}

struct Rig {
    client: HostClient<FakeTransport>,
    transport: FakeTransport,
    key: Arc<SoftKey>,
}

fn rig_with(retry: RetryPolicy, enrolled: bool) -> Rig {
    let key = SoftKey::new();
    let transport = FakeTransport::default();
    let mut config = ClientConfig::new("0.1.0-test");
    config.base_url = "https://example.test/_api/".into();
    config.retry = retry;
    let mut client = HostClient::new(
        transport.clone(),
        config,
        key.clone(),
        Arc::new(FakeClock::new(NOW_MS)),
        Arc::new(OsNonceSource),
    );
    if enrolled {
        key.create_key().unwrap();
        client = client.with_device_id(DEVICE);
    }
    Rig {
        client,
        transport,
        key,
    }
}

fn rig() -> Rig {
    rig_with(
        RetryPolicy {
            max_attempts: 3,
            base_delay: Duration::ZERO,
            max_delay: Duration::ZERO,
        },
        true,
    )
}

/// What the server's `hostAuth` does, minus the database.
fn assert_server_accepts(req: &HttpRequest, route: &str, vk: &VerifyingKey) {
    assert_eq!(req.url, format!("https://example.test/_api/{route}"));
    assert_eq!(req.header("x-ra-device"), Some(DEVICE));
    let ts = req.header("x-ra-timestamp").unwrap();
    assert!(ts.len() >= 10 && ts.bytes().all(|b| b.is_ascii_digit()));
    assert_eq!(ts, NOW_MS.to_string());
    let nonce = req.header("x-ra-nonce").unwrap();
    assert!((16..=64).contains(&nonce.len()));
    assert!(
        nonce
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    );
    let raw_sig = URL_SAFE_NO_PAD
        .decode(req.header("x-ra-signature").unwrap())
        .unwrap();
    assert_eq!(raw_sig.len(), 64, "signature must be P1363, not DER");
    let sig = Signature::from_slice(&raw_sig).unwrap();
    let message = request_message(route, DEVICE, NOW_MS, nonce, &req.body);
    vk.verify(message.as_bytes(), &sig)
        .expect("server would reject this signature");
}

fn body_json(req: &HttpRequest) -> Value {
    let v: Value = serde_json::from_slice(&req.body).unwrap();
    v["json"].clone()
}

#[tokio::test]
async fn poll_is_signed_the_way_the_server_verifies_it() {
    let r = rig();
    r.transport.push(poll_ok());

    let poll = r.client.poll().await.unwrap();

    assert_eq!(poll.pending.len(), 1);
    let sent = r.transport.sent();
    assert_eq!(sent.len(), 1);
    assert_server_accepts(&sent[0], "host/poll", &r.key.verifying());
    assert_eq!(
        body_json(&sent[0]),
        json!({"hostVersion": "0.1.0-test", "protocolVersion": 1})
    );
}

#[tokio::test]
async fn every_signed_route_signs_its_own_route_name() {
    let r = rig();
    let session_ok = r#"{"json":{"state":"ended","consentExpiresAt":null}}"#;
    r.transport.push(ok(r#"{"json":{"code":"123456789012","expiresAt":"2026-10-08T12:43:38.897Z"},"meta":{"values":{"expiresAt":["Date"]}}}"#));
    r.transport.push(ok(r#"{"json":{"state":"connecting","failureReason":null,"consentExpiresAt":null,"maxEndsAt":null}}"#));
    r.transport.push(ok(session_ok));
    r.transport.push(ok(r#"{"json":{"ok":true}}"#));

    r.client.invite().await.unwrap();
    r.client
        .decide(&DecideRequest {
            session_id: SESSION.into(),
            decision: Decision::Approve,
            grant: Some(Default::default()),
            approved_minutes: Some(30),
            host_fingerprint: None,
        })
        .await
        .unwrap();
    r.client
        .session(&SessionRequest {
            session_id: SESSION.into(),
            action: SessionAction::End {
                reason: EndReason::HostStop,
            },
        })
        .await
        .unwrap();
    r.client
        .banner_report(&BannerReportRequest {
            default_hide_minutes: 30,
            show_hide_shortcut: "Ctrl+Alt+Shift+B".into(),
            disconnect_shortcut: "Ctrl+Alt+Shift+X".into(),
            shortcut_collision: false,
            event: None,
        })
        .await
        .unwrap();

    let sent = r.transport.sent();
    let routes = [
        "host/invite",
        "host/decide",
        "host/session",
        "host/banner_report",
    ];
    assert_eq!(sent.len(), routes.len());
    for (req, route) in sent.iter().zip(routes) {
        assert_server_accepts(req, route, &r.key.verifying());
    }
    assert_eq!(body_json(&sent[0]), json!({}));
}

#[tokio::test]
async fn a_retry_is_a_new_signed_request_with_a_new_nonce() {
    let r = rig();
    r.transport
        .push(status(503, r#"{"json":{"error":"busy"}}"#));
    r.transport
        .push(Err(TransportError::Other("connection reset".into())));
    r.transport.push(poll_ok());

    r.client.poll().await.unwrap();

    let sent = r.transport.sent();
    assert_eq!(sent.len(), 3);
    let nonces: HashSet<_> = sent
        .iter()
        .map(|s| s.header("x-ra-nonce").unwrap())
        .collect();
    assert_eq!(nonces.len(), 3, "a nonce was reused across attempts");
    let sigs: HashSet<_> = sent
        .iter()
        .map(|s| s.header("x-ra-signature").unwrap())
        .collect();
    assert_eq!(sigs.len(), 3);
    for req in &sent {
        assert_server_accepts(req, "host/poll", &r.key.verifying());
        assert_eq!(req.body, sent[0].body, "retry must carry the same body");
    }
}

#[tokio::test]
async fn timeouts_are_retried_then_reported() {
    let r = rig();
    for _ in 0..3 {
        r.transport.push(Err(TransportError::Timeout));
    }
    assert_eq!(r.client.poll().await.unwrap_err(), HostError::Timeout);
    assert_eq!(r.transport.sent().len(), 3);
}

#[tokio::test]
async fn client_errors_are_not_retried() {
    for (code, body) in [
        (400, r#"{"json":{"error":"Bad input"}}"#),
        (401, r#"{"json":{"error":"Replayed request"}}"#),
        (
            401,
            r#"{"json":{"error":"Device clock skew too large or request expired"}}"#,
        ),
        (
            403,
            r#"{"json":{"error":"Nope","code":"STEP_UP_REQUIRED"}}"#,
        ),
        (404, r#"{"json":{"error":"Not found"}}"#),
        (402, r#"{"json":{"error":"No plan"}}"#),
    ] {
        let r = rig();
        r.transport.push(status(code, body));
        let err = r.client.poll().await.unwrap_err();
        assert!(
            matches!(err, HostError::Api { status, .. } if status == code),
            "{err}"
        );
        assert_eq!(
            r.transport.sent().len(),
            1,
            "status {code} must not be retried"
        );
    }
}

#[tokio::test]
async fn error_code_and_message_are_surfaced() {
    let r = rig();
    r.transport.push(status(
        403,
        r#"{"json":{"error":"Confirm with your authenticator code","code":"STEP_UP_REQUIRED"}}"#,
    ));
    assert_eq!(
        r.client.poll().await.unwrap_err(),
        HostError::Api {
            status: 403,
            code: Some("STEP_UP_REQUIRED".into()),
            message: "Confirm with your authenticator code".into(),
        }
    );
}

#[tokio::test]
async fn non_json_error_body_still_gives_a_typed_error() {
    let r = rig();
    r.transport.push(status(502, "<html>Bad gateway</html>"));
    r.transport.push(status(502, "<html>Bad gateway</html>"));
    r.transport.push(status(502, "<html>Bad gateway</html>"));
    match r.client.poll().await.unwrap_err() {
        HostError::Api {
            status,
            code,
            message,
        } => {
            assert_eq!(status, 502);
            assert_eq!(code, None);
            assert!(message.contains("Bad gateway"));
        }
        other => panic!("unexpected {other}"),
    }
    assert_eq!(r.transport.sent().len(), 3, "5xx is retried");
}

#[tokio::test]
async fn device_revoked_is_typed_and_final() {
    let r = rig();
    r.transport.push(status(
        401,
        r#"{"json":{"error":"Device is not registered or has been revoked","code":"DEVICE_REVOKED"}}"#,
    ));
    assert_eq!(r.client.poll().await.unwrap_err(), HostError::DeviceRevoked);
    assert_eq!(r.transport.sent().len(), 1);
}

#[tokio::test]
async fn device_revoked_code_needs_a_401() {
    let r = rig();
    r.transport.push(status(
        400,
        r#"{"json":{"error":"x","code":"DEVICE_REVOKED"}}"#,
    ));
    assert!(matches!(
        r.client.poll().await.unwrap_err(),
        HostError::Api { status: 400, .. }
    ));
}

#[tokio::test]
async fn signed_routes_need_a_device_id() {
    let r = rig_with(RetryPolicy::none(), false);
    assert_eq!(r.client.poll().await.unwrap_err(), HostError::NotEnrolled);
    assert!(r.transport.sent().is_empty());
}

#[tokio::test]
async fn bad_success_body_is_a_codec_error_and_not_retried() {
    let r = rig();
    r.transport.push(ok(r#"{"json":{"unexpected":true}}"#));
    assert!(matches!(
        r.client.poll().await.unwrap_err(),
        HostError::Codec(_)
    ));
    assert_eq!(r.transport.sent().len(), 1);
}

#[tokio::test]
async fn base_url_without_trailing_slash_works() {
    let r = rig();
    let mut config = ClientConfig::new("v");
    config.base_url = "https://example.test/_api".into();
    let client = HostClient::new(
        r.transport.clone(),
        config,
        r.key.clone(),
        Arc::new(FakeClock::new(NOW_MS)),
        Arc::new(OsNonceSource),
    )
    .with_device_id(DEVICE);
    r.transport.push(poll_ok());
    client.poll().await.unwrap();
    assert_eq!(
        r.transport.sent()[0].url,
        "https://example.test/_api/host/poll"
    );
}

#[tokio::test]
async fn enroll_sends_an_unsigned_request_with_a_valid_proof() {
    let r = rig_with(RetryPolicy::default(), false);
    let fp = fingerprint(&r.key.jwk()).unwrap();
    r.transport.push(ok(&format!(
        r#"{{"json":{{"deviceId":"{DEVICE}","fingerprint":"{fp}"}}}}"#
    )));

    let enrolled = r
        .client
        .enroll("12345678", "  Office PC ", HostPlatform::Windows)
        .await
        .unwrap();
    assert_eq!(enrolled.device_id, DEVICE);

    let sent = r.transport.sent();
    assert_eq!(sent.len(), 1);
    let req = &sent[0];
    assert_eq!(req.url, "https://example.test/_api/host/enroll");
    assert!(
        req.header("x-ra-signature").is_none(),
        "enroll is not header-signed"
    );

    let body = body_json(req);
    assert_eq!(body["code"], "12345678");
    assert_eq!(body["name"], "Office PC");
    assert_eq!(body["platform"], "windows");
    assert_eq!(body["protocolVersion"], 1);
    assert_eq!(body["hostVersion"], "0.1.0-test");
    let jwk = r.key.jwk();
    assert_eq!(
        body["publicKeyJwk"],
        json!({"kty": "EC", "crv": "P-256", "x": jwk.x, "y": jwk.y})
    );
    let proof = URL_SAFE_NO_PAD
        .decode(body["proof"].as_str().unwrap())
        .unwrap();
    assert_eq!(proof.len(), 64);
    r.key
        .verifying()
        .verify(
            enroll_message("12345678", &fp).as_bytes(),
            &Signature::from_slice(&proof).unwrap(),
        )
        .expect("server would reject the proof");
}

#[tokio::test]
async fn enroll_reuses_an_existing_unenrolled_key() {
    let r = rig_with(RetryPolicy::default(), false);
    let created = r.key.create_key().unwrap();
    let fp = fingerprint(&created).unwrap();
    r.transport.push(ok(&format!(
        r#"{{"json":{{"deviceId":"{DEVICE}","fingerprint":"{fp}"}}}}"#
    )));
    r.client
        .enroll("00000000", "Home", HostPlatform::Macos)
        .await
        .unwrap();
    assert_eq!(
        body_json(&r.transport.sent()[0])["publicKeyJwk"]["x"],
        created.x
    );
}

#[tokio::test]
async fn enroll_is_never_retried() {
    let r = rig_with(RetryPolicy::default(), false);
    r.transport
        .push(status(500, r#"{"json":{"error":"boom"}}"#));
    assert!(
        r.client
            .enroll("12345678", "PC", HostPlatform::Windows)
            .await
            .is_err()
    );
    assert_eq!(r.transport.sent().len(), 1);
}

#[tokio::test]
async fn enroll_detects_a_fingerprint_mismatch() {
    let r = rig_with(RetryPolicy::none(), false);
    r.transport.push(ok(&format!(
        r#"{{"json":{{"deviceId":"{DEVICE}","fingerprint":"{}"}}}}"#,
        "0".repeat(64)
    )));
    assert_eq!(
        r.client
            .enroll("12345678", "PC", HostPlatform::Windows)
            .await
            .unwrap_err(),
        HostError::FingerprintMismatch
    );
}

#[tokio::test]
async fn enroll_rejects_bad_input_before_touching_the_network() {
    let r = rig_with(RetryPolicy::none(), false);
    for (code, name) in [
        ("1234567", "PC"),
        ("123456789", "PC"),
        ("1234567a", "PC"),
        ("12345678", ""),
        ("12345678", "   "),
        ("12345678", &"n".repeat(61)),
    ] {
        assert!(
            r.client
                .enroll(code, name, HostPlatform::Windows)
                .await
                .is_err(),
            "{code:?} {name:?}"
        );
    }
    assert!(r.transport.sent().is_empty());
    assert_eq!(
        r.key.public_key().unwrap(),
        None,
        "no key made for bad input"
    );
}

#[test]
fn retry_delays_stay_within_the_exponential_cap() {
    let p = RetryPolicy {
        max_attempts: 5,
        base_delay: Duration::from_millis(500),
        max_delay: Duration::from_secs(2),
    };
    for retry in 0..40 {
        let cap = Duration::from_millis(500)
            .saturating_mul(1u32.checked_shl(retry).unwrap_or(u32::MAX))
            .min(Duration::from_secs(2));
        for _ in 0..50 {
            assert!(p.delay_for(retry) <= cap);
        }
    }
    let zero = RetryPolicy {
        base_delay: Duration::ZERO,
        ..p
    };
    assert_eq!(zero.delay_for(3), Duration::ZERO);
    assert_eq!(RetryPolicy::none().max_attempts, 1);
    // Jitter is real: 50 draws of a 2 s cap are not all identical.
    let draws: HashSet<_> = (0..50).map(|_| p.delay_for(10)).collect();
    assert!(draws.len() > 1);
}

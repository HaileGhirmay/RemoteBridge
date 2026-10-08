use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::traits::{Clock, KeyStore};

use super::error::HostError;
use super::fingerprint::fingerprint;
use super::nonce::NonceSource;
use super::signing::{encode_signature, enroll_message, request_message};
use super::transport::{HttpRequest, HttpResponse, Transport, TransportError};
use super::types::{
    BannerReportRequest, BannerReportResponse, DecideRequest, DecideResponse, EnrollRequest,
    EnrollResponse, HostPlatform, InviteRequest, InviteResponse, JwkBody, MediaCredentialsRequest,
    MediaCredentialsResponse, PollRequest, PollResponse, SessionRequest, SessionResponse,
};
use super::{DEFAULT_BASE_URL, PROTOCOL_VERSION, superjson};

/// Exponential backoff with full jitter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Total tries including the first. `1` disables retries.
    pub max_attempts: u32,
    pub base_delay: Duration,
    pub max_delay: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 4,
            base_delay: Duration::from_millis(500),
            max_delay: Duration::from_secs(8),
        }
    }
}

impl RetryPolicy {
    pub fn none() -> Self {
        Self {
            max_attempts: 1,
            ..Self::default()
        }
    }

    /// Delay before retry number `retry` (0 = first retry): a random value in
    /// `0..=min(max_delay, base_delay * 2^retry)`.
    pub fn delay_for(&self, retry: u32) -> Duration {
        let factor = 1u32.checked_shl(retry).unwrap_or(u32::MAX);
        let cap = self.base_delay.saturating_mul(factor).min(self.max_delay);
        if cap.is_zero() {
            return Duration::ZERO;
        }
        let mut bytes = [0u8; 8];
        getrandom::fill(&mut bytes).expect("OS random source unavailable");
        let r = u64::from_le_bytes(bytes);
        let nanos = u128::from(u64::try_from(cap.as_nanos()).unwrap_or(u64::MAX));
        let scaled = nanos * u128::from(r) / u128::from(u64::MAX);
        Duration::from_nanos(u64::try_from(scaled).unwrap_or(u64::MAX))
    }
}

#[derive(Debug, Clone)]
pub struct ClientConfig {
    /// Ends with `/`, for example `https://remotebridge.floot.app/_api/`.
    pub base_url: String,
    pub host_version: String,
    pub retry: RetryPolicy,
}

impl ClientConfig {
    pub fn new(host_version: impl Into<String>) -> Self {
        Self {
            base_url: DEFAULT_BASE_URL.into(),
            host_version: host_version.into(),
            retry: RetryPolicy::default(),
        }
    }
}

/// Client for the host routes. Every request but `enroll` is signed with the
/// device key; every attempt, including a retry, gets a new timestamp, nonce
/// and signature.
pub struct HostClient<T: Transport> {
    transport: T,
    config: ClientConfig,
    keys: Arc<dyn KeyStore>,
    clock: Arc<dyn Clock>,
    nonces: Arc<dyn NonceSource>,
    device_id: Option<String>,
}

impl<T: Transport> HostClient<T> {
    pub fn new(
        transport: T,
        config: ClientConfig,
        keys: Arc<dyn KeyStore>,
        clock: Arc<dyn Clock>,
        nonces: Arc<dyn NonceSource>,
    ) -> Self {
        Self {
            transport,
            config,
            keys,
            clock,
            nonces,
            device_id: None,
        }
    }

    /// Set the enrolled device id. Required before any signed route.
    pub fn with_device_id(mut self, device_id: impl Into<String>) -> Self {
        self.device_id = Some(device_id.into());
        self
    }

    pub fn device_id(&self) -> Option<&str> {
        self.device_id.as_deref()
    }

    /// `host/enroll`: not header-signed. Uses the existing device key, or
    /// creates one, and proves possession by signing
    /// `RA-ENROLL-V1\n<code>\n<fingerprint>`.
    ///
    /// Sent once, never retried: the code is single use, so a repeat after a
    /// lost answer could only fail.
    pub async fn enroll(
        &self,
        code: &str,
        name: &str,
        platform: HostPlatform,
    ) -> Result<EnrollResponse, HostError> {
        if code.len() != 8 || !code.bytes().all(|b| b.is_ascii_digit()) {
            return Err(HostError::Codec("enrollment code must be 8 digits".into()));
        }
        let name = name.trim();
        if name.is_empty() || name.chars().count() > 60 {
            return Err(HostError::Codec(
                "device name must be 1-60 characters".into(),
            ));
        }
        let key = match self.keys.public_key()? {
            Some(key) => key,
            None => self.keys.create_key()?,
        };
        let local_fingerprint = fingerprint(&key)?;
        let proof = encode_signature(
            &self
                .keys
                .sign(enroll_message(code, &local_fingerprint).as_bytes())?,
        )?;
        let request = EnrollRequest {
            code: code.into(),
            name: name.into(),
            platform,
            host_version: self.config.host_version.clone(),
            protocol_version: PROTOCOL_VERSION,
            public_key_jwk: JwkBody {
                kty: "EC".into(),
                crv: "P-256".into(),
                x: key.x,
                y: key.y,
            },
            proof,
        };
        let body = superjson::encode(&request)?;
        let response = self
            .transport
            .post(HttpRequest {
                url: self.url("host/enroll"),
                headers: Vec::new(),
                body,
            })
            .await
            .map_err(map_transport_error)?;
        let enrolled: EnrollResponse = decode_response(&response)?;
        if enrolled.fingerprint != local_fingerprint {
            return Err(HostError::FingerprintMismatch);
        }
        Ok(enrolled)
    }

    /// `host/poll`: the heartbeat. Also renews the 120 s session lease.
    pub async fn poll(&self) -> Result<PollResponse, HostError> {
        self.signed(
            "host/poll",
            &PollRequest {
                host_version: Some(self.config.host_version.clone()),
                protocol_version: PROTOCOL_VERSION,
            },
        )
        .await
    }

    /// `host/invite`: a 12-digit support code, valid 10 minutes, single use.
    pub async fn invite(&self) -> Result<InviteResponse, HostError> {
        self.signed("host/invite", &InviteRequest {}).await
    }

    /// `host/decide`: approve or deny a pending session.
    pub async fn decide(&self, request: &DecideRequest) -> Result<DecideResponse, HostError> {
        self.signed("host/decide", request).await
    }

    /// `host/session`: every session action.
    pub async fn session(&self, request: &SessionRequest) -> Result<SessionResponse, HostError> {
        self.signed("host/session", request).await
    }

    /// `host/media_credentials`: signaling token and ICE servers for a live
    /// session. `503 MEDIA_UNCONFIGURED` until the website has the media services set up.
    pub async fn media_credentials(
        &self,
        session_id: &str,
    ) -> Result<MediaCredentialsResponse, HostError> {
        self.signed(
            "host/media_credentials",
            &MediaCredentialsRequest {
                session_id: session_id.to_owned(),
            },
        )
        .await
    }

    /// `host/banner_report`: metadata only.
    pub async fn banner_report(
        &self,
        request: &BannerReportRequest,
    ) -> Result<BannerReportResponse, HostError> {
        self.signed("host/banner_report", request).await
    }

    fn url(&self, route: &str) -> String {
        let base = self.config.base_url.trim_end_matches('/');
        format!("{base}/{route}")
    }

    async fn signed<Req, Resp>(&self, route: &'static str, request: &Req) -> Result<Resp, HostError>
    where
        Req: Serialize,
        Resp: DeserializeOwned,
    {
        let device_id = self.device_id.as_deref().ok_or(HostError::NotEnrolled)?;
        // The body is fixed; the timestamp, nonce and signature are not.
        let body = superjson::encode(request)?;
        let max_attempts = self.config.retry.max_attempts.max(1);
        let mut attempt = 1;
        loop {
            match self.attempt(route, device_id, &body).await {
                Err(e) if e.is_retryable() && attempt < max_attempts => {
                    let delay = self.config.retry.delay_for(attempt - 1);
                    if !delay.is_zero() {
                        tokio::time::sleep(delay).await;
                    }
                    attempt += 1;
                }
                other => return other,
            }
        }
    }

    /// One signed request with a fresh timestamp, nonce and signature.
    async fn attempt<Resp: DeserializeOwned>(
        &self,
        route: &str,
        device_id: &str,
        body: &[u8],
    ) -> Result<Resp, HostError> {
        let timestamp_ms = self.clock.wall_ms();
        let nonce = self.nonces.next();
        let message = request_message(route, device_id, timestamp_ms, &nonce, body);
        let signature = encode_signature(&self.keys.sign(message.as_bytes())?)?;
        let response = self
            .transport
            .post(HttpRequest {
                url: self.url(route),
                headers: vec![
                    ("x-ra-device".into(), device_id.into()),
                    ("x-ra-timestamp".into(), timestamp_ms.to_string()),
                    ("x-ra-nonce".into(), nonce),
                    ("x-ra-signature".into(), signature),
                ],
                body: body.to_vec(),
            })
            .await
            .map_err(map_transport_error)?;
        decode_response(&response)
    }
}

fn map_transport_error(e: TransportError) -> HostError {
    match e {
        TransportError::Timeout => HostError::Timeout,
        TransportError::Other(msg) => HostError::Network(msg),
    }
}

fn decode_response<Resp: DeserializeOwned>(response: &HttpResponse) -> Result<Resp, HostError> {
    if (200..300).contains(&response.status) {
        superjson::decode(&response.body)
    } else {
        Err(error_from_response(response))
    }
}

/// The server answers errors as `{"json": {"error": "...", "code": "..."}}`.
fn error_from_response(response: &HttpResponse) -> HostError {
    let parsed = superjson::decode_value(&response.body).ok();
    let field = |name: &str| {
        parsed
            .as_ref()
            .and_then(|v| v.get(name))
            .and_then(|v| v.as_str())
            .map(str::to_owned)
    };
    let code = field("code");
    if response.status == 401 && code.as_deref() == Some("DEVICE_REVOKED") {
        return HostError::DeviceRevoked;
    }
    let message = field("error").unwrap_or_else(|| {
        String::from_utf8_lossy(&response.body)
            .chars()
            .take(200)
            .collect()
    });
    HostError::Api {
        status: response.status,
        code,
        message,
    }
}

#[cfg(test)]
mod tests;

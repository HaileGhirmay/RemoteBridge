use std::future::Future;
use std::time::Duration;

/// Per-request timeout of the production transport.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpRequest {
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl HttpRequest {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportError {
    Timeout,
    Other(String),
}

/// One HTTP `POST`. Abstracted so the client can be tested without a network.
pub trait Transport: Send + Sync {
    fn post(
        &self,
        request: HttpRequest,
    ) -> impl Future<Output = Result<HttpResponse, TransportError>> + Send;
}

/// Production transport: reqwest with rustls (ring) and a 10 s timeout.
#[derive(Debug, Clone)]
pub struct ReqwestTransport {
    client: reqwest::Client,
}

impl ReqwestTransport {
    pub fn new() -> Result<Self, TransportError> {
        // Installing twice returns Err; that only means a provider is set.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|e| TransportError::Other(e.to_string()))?;
        Ok(Self { client })
    }
}

impl Transport for ReqwestTransport {
    async fn post(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
        let mut builder = self
            .client
            .post(&request.url)
            .header("content-type", "application/json");
        for (k, v) in &request.headers {
            builder = builder.header(k.as_str(), v.as_str());
        }
        let response = builder
            .body(request.body)
            .send()
            .await
            .map_err(map_reqwest_error)?;
        let status = response.status().as_u16();
        let body = response.bytes().await.map_err(map_reqwest_error)?;
        Ok(HttpResponse {
            status,
            body: body.to_vec(),
        })
    }
}

fn map_reqwest_error(e: reqwest::Error) -> TransportError {
    if e.is_timeout() {
        TransportError::Timeout
    } else {
        TransportError::Other(e.without_url().to_string())
    }
}

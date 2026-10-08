use std::future::Future;

use crate::protocol::types::{
    BannerReportRequest, BannerReportResponse, DecideRequest, DecideResponse, InviteResponse,
    MediaCredentialsResponse, PollResponse, SessionRequest, SessionResponse,
};
use crate::protocol::{HostClient, HostError, Transport};

/// The host routes the session manager needs. Implemented by [`HostClient`];
/// tests use `fakes::FakeHostApi`.
pub trait HostApi: Send + Sync {
    fn poll(&self) -> impl Future<Output = Result<PollResponse, HostError>> + Send;
    fn invite(&self) -> impl Future<Output = Result<InviteResponse, HostError>> + Send;
    fn decide(
        &self,
        request: &DecideRequest,
    ) -> impl Future<Output = Result<DecideResponse, HostError>> + Send;
    fn session(
        &self,
        request: &SessionRequest,
    ) -> impl Future<Output = Result<SessionResponse, HostError>> + Send;
    fn banner_report(
        &self,
        request: &BannerReportRequest,
    ) -> impl Future<Output = Result<BannerReportResponse, HostError>> + Send;
    fn media_credentials(
        &self,
        session_id: &str,
    ) -> impl Future<Output = Result<MediaCredentialsResponse, HostError>> + Send;
}

impl<T: Transport> HostApi for HostClient<T> {
    async fn poll(&self) -> Result<PollResponse, HostError> {
        HostClient::poll(self).await
    }

    async fn invite(&self) -> Result<InviteResponse, HostError> {
        HostClient::invite(self).await
    }

    async fn decide(&self, request: &DecideRequest) -> Result<DecideResponse, HostError> {
        HostClient::decide(self, request).await
    }

    async fn session(&self, request: &SessionRequest) -> Result<SessionResponse, HostError> {
        HostClient::session(self, request).await
    }

    async fn banner_report(
        &self,
        request: &BannerReportRequest,
    ) -> Result<BannerReportResponse, HostError> {
        HostClient::banner_report(self, request).await
    }

    async fn media_credentials(
        &self,
        session_id: &str,
    ) -> Result<MediaCredentialsResponse, HostError> {
        HostClient::media_credentials(self, session_id).await
    }
}

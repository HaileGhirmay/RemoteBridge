use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use crate::Permissions;
use crate::protocol::HostError;
use crate::protocol::types::{
    BannerReportRequest, BannerReportResponse, DecideRequest, DecideResponse, Decision, DeviceView,
    HostSessionView, InviteResponse, MediaCredentialsResponse, PolicyView, PollResponse, Requester,
    SessionAction, SessionMode, SessionRequest, SessionResponse,
};
use crate::session::HostApi;
use crate::traits::Clock;

use super::{FakeClock, Trace};

/// A request the fake server received.
#[derive(Debug, Clone, PartialEq)]
pub enum Recorded {
    Poll,
    Invite,
    Decide(DecideRequest),
    Session(SessionRequest),
    BannerReport(BannerReportRequest),
}

struct State {
    device: DeviceView,
    policy: PolicyView,
    update_required: bool,
    /// Every non-terminal session. `awaiting_approval` ones are served as
    /// `pending`, the rest as `live`.
    sessions: Vec<HostSessionView>,
    poll_errors: VecDeque<HostError>,
    action_errors: VecDeque<HostError>,
    requests: Vec<Recorded>,
    /// What `host/media_credentials` answers; `None` means 503 MEDIA_UNCONFIGURED.
    media: Option<MediaCredentialsResponse>,
}

/// A small model of the control plane, enough to drive the session manager:
/// it applies `decide` and `session` actions the way the real server does
/// (subset checks, unattended rules, state changes) and records every request.
#[derive(Clone)]
pub struct FakeHostApi {
    state: Arc<Mutex<State>>,
    clock: FakeClock,
    trace: Trace,
}

fn api_error(status: u16, message: &str) -> HostError {
    HostError::Api {
        status,
        code: None,
        message: message.into(),
    }
}

/// A freshly requested session, awaiting approval, requester "Sam Helper".
pub fn test_session(id: &str, mode: SessionMode, requested: Permissions) -> HostSessionView {
    HostSessionView {
        id: id.into(),
        mode,
        state: "awaiting_approval".into(),
        requester: Requester {
            email: "sam@example.com".into(),
            display_name: "Sam Helper".into(),
            verified: true,
        },
        requested_permissions: requested,
        granted_permissions: Permissions::NONE,
        pending_permissions: None,
        approval_expires_at: None,
        consent_expires_at: None,
        max_ends_at: None,
        lease_expires_at: None,
        started_at: None,
        viewer_fingerprint: None,
    }
}

impl FakeHostApi {
    pub fn new(clock: FakeClock) -> Self {
        Self::with_trace(clock, Trace::new())
    }

    pub fn with_trace(clock: FakeClock, trace: Trace) -> Self {
        Self {
            state: Arc::new(Mutex::new(State {
                device: DeviceView {
                    id: "6f1c3d7e-2b4a-4c8e-9d10-0a1b2c3d4e5f".into(),
                    name: "Test PC".into(),
                    unattended_enabled: false,
                },
                policy: PolicyView {
                    consent_renewal_minutes: 30,
                    max_session_minutes: 480,
                    allow_control_unattended: false,
                    allow_audio_unattended: false,
                },
                update_required: false,
                sessions: Vec::new(),
                poll_errors: VecDeque::new(),
                action_errors: VecDeque::new(),
                requests: Vec::new(),
                media: None,
            })),
            clock,
            trace,
        }
    }

    fn now(&self) -> i64 {
        self.clock.wall_ms()
    }

    // ---- scripting ---------------------------------------------------

    pub fn set_unattended_enabled(&self, on: bool) {
        self.state.lock().unwrap().device.unattended_enabled = on;
    }

    pub fn set_policy(&self, policy: PolicyView) {
        self.state.lock().unwrap().policy = policy;
    }

    /// What `host/media_credentials` should answer.
    pub fn set_media_credentials(&self, credentials: Option<MediaCredentialsResponse>) {
        self.state.lock().unwrap().media = credentials;
    }

    pub fn set_update_required(&self, on: bool) {
        self.state.lock().unwrap().update_required = on;
    }

    /// A viewer asks for access. Approval expires `approval_minutes` from now.
    pub fn add_request(&self, mut view: HostSessionView, approval_minutes: i64) {
        view.approval_expires_at = Some(self.now() + approval_minutes * 60_000);
        self.state.lock().unwrap().sessions.push(view);
    }

    pub fn update_session(&self, id: &str, f: impl FnOnce(&mut HostSessionView)) {
        let mut s = self.state.lock().unwrap();
        let view = s
            .sessions
            .iter_mut()
            .find(|v| v.id == id)
            .expect("no such session");
        f(view);
    }

    /// The viewer asks for more access.
    pub fn request_more(&self, id: &str, pending: Permissions) {
        self.update_session(id, |v| v.pending_permissions = Some(pending));
    }

    /// The server ends the session (expiry sweep, viewer left, revoked, ...).
    pub fn end_session_on_server(&self, id: &str) {
        self.state.lock().unwrap().sessions.retain(|v| v.id != id);
    }

    /// Keep a session in the list but with a terminal state (a server should
    /// not do this; the host must cope).
    pub fn set_state(&self, id: &str, state: &str) {
        self.update_session(id, |v| v.state = state.into());
    }

    pub fn fail_next_poll(&self, error: HostError) {
        self.state.lock().unwrap().poll_errors.push_back(error);
    }

    pub fn clear_poll_errors(&self) {
        self.state.lock().unwrap().poll_errors.clear();
    }

    /// Fail the next decide / session / invite call.
    pub fn fail_next_action(&self, error: HostError) {
        self.state.lock().unwrap().action_errors.push_back(error);
    }

    // ---- inspection --------------------------------------------------

    pub fn requests(&self) -> Vec<Recorded> {
        self.state.lock().unwrap().requests.clone()
    }

    pub fn decides(&self) -> Vec<DecideRequest> {
        self.requests()
            .into_iter()
            .filter_map(|r| match r {
                Recorded::Decide(d) => Some(d),
                _ => None,
            })
            .collect()
    }

    pub fn sessions_sent(&self) -> Vec<SessionRequest> {
        self.requests()
            .into_iter()
            .filter_map(|r| match r {
                Recorded::Session(s) => Some(s),
                _ => None,
            })
            .collect()
    }

    pub fn poll_count(&self) -> usize {
        self.requests()
            .iter()
            .filter(|r| **r == Recorded::Poll)
            .count()
    }

    pub fn session_view(&self, id: &str) -> Option<HostSessionView> {
        self.state
            .lock()
            .unwrap()
            .sessions
            .iter()
            .find(|v| v.id == id)
            .cloned()
    }

    fn next_action_error(&self) -> Option<HostError> {
        self.state.lock().unwrap().action_errors.pop_front()
    }
}

impl HostApi for FakeHostApi {
    async fn poll(&self) -> Result<PollResponse, HostError> {
        let mut s = self.state.lock().unwrap();
        s.requests.push(Recorded::Poll);
        self.trace.push("api:poll");
        if let Some(e) = s.poll_errors.pop_front() {
            return Err(e);
        }
        let (pending, live): (Vec<_>, Vec<_>) = s
            .sessions
            .iter()
            .cloned()
            .partition(|v| v.state == "awaiting_approval");
        Ok(PollResponse {
            server_time: self.now(),
            update_required: s.update_required,
            min_protocol_version: 1,
            device: s.device.clone(),
            policy: s.policy.clone(),
            pending,
            live,
        })
    }

    async fn invite(&self) -> Result<crate::protocol::types::InviteResponse, HostError> {
        self.state.lock().unwrap().requests.push(Recorded::Invite);
        self.trace.push("api:invite");
        if let Some(e) = self.next_action_error() {
            return Err(e);
        }
        Ok(InviteResponse {
            code: "123456789012".into(),
            expires_at: self.now() + 10 * 60_000,
        })
    }

    async fn decide(&self, request: &DecideRequest) -> Result<DecideResponse, HostError> {
        let decision = match request.decision {
            Decision::Approve => "approve",
            Decision::Deny => "deny",
        };
        self.trace
            .push(format!("api:decide:{decision}:{}", request.session_id));
        if let Some(e) = self.next_action_error() {
            self.state
                .lock()
                .unwrap()
                .requests
                .push(Recorded::Decide(request.clone()));
            return Err(e);
        }
        let now = self.now();
        let mut s = self.state.lock().unwrap();
        s.requests.push(Recorded::Decide(request.clone()));
        let policy = s.policy.clone();
        let unattended_enabled = s.device.unattended_enabled;
        let Some(idx) = s.sessions.iter().position(|v| v.id == request.session_id) else {
            return Err(api_error(404, "Session not found for this device"));
        };
        if s.sessions[idx].state != "awaiting_approval" {
            return Err(api_error(409, "Session is not awaiting approval"));
        }
        if request.decision == Decision::Deny {
            s.sessions.remove(idx);
            return Ok(DecideResponse {
                state: "denied".into(),
                failure_reason: None,
                consent_expires_at: None,
                max_ends_at: None,
            });
        }
        let grant = request.grant.unwrap_or(Permissions::NONE);
        let view = &mut s.sessions[idx];
        if !grant.is_subset_of(&view.requested_permissions) {
            return Err(api_error(
                400,
                "Host cannot grant scopes the requester did not ask for",
            ));
        }
        if view.mode == SessionMode::Unattended {
            if !unattended_enabled {
                return Err(api_error(
                    403,
                    "Unattended access is not enabled for this device",
                ));
            }
            if grant.control && !policy.allow_control_unattended {
                return Err(api_error(403, "Policy forbids unattended control"));
            }
            if (grant.system_audio || grant.microphone) && !policy.allow_audio_unattended {
                return Err(api_error(403, "Policy forbids unattended audio"));
            }
            if grant.microphone {
                return Err(api_error(
                    403,
                    "Microphone is never granted without a person at the host",
                ));
            }
        }
        let max_ends = now + i64::from(policy.max_session_minutes) * 60_000;
        let consent = (view.mode == SessionMode::Attended).then(|| {
            let minutes = request
                .approved_minutes
                .unwrap_or(policy.consent_renewal_minutes);
            (now + i64::from(minutes) * 60_000).min(max_ends)
        });
        view.state = "connecting".into();
        view.granted_permissions = grant;
        view.consent_expires_at = consent;
        view.max_ends_at = Some(max_ends);
        view.approval_expires_at = None;
        Ok(DecideResponse {
            state: "connecting".into(),
            failure_reason: None,
            consent_expires_at: consent,
            max_ends_at: Some(max_ends),
        })
    }

    async fn session(&self, request: &SessionRequest) -> Result<SessionResponse, HostError> {
        let name = match &request.action {
            SessionAction::MediaConnected { .. } => "media_connected",
            SessionAction::Pause => "pause",
            SessionAction::Resume => "resume",
            SessionAction::Reconnecting => "reconnecting",
            SessionAction::End { .. } => "end",
            SessionAction::RenewConsent { .. } => "renew_consent",
            SessionAction::SetPermissions { .. } => "set_permissions",
            SessionAction::DeclinePermissions => "decline_permissions",
            SessionAction::ReportFailure { .. } => "report_failure",
        };
        self.trace
            .push(format!("api:session:{name}:{}", request.session_id));
        let now = self.now();
        let injected = self.next_action_error();
        let mut s = self.state.lock().unwrap();
        s.requests.push(Recorded::Session(request.clone()));
        if let Some(e) = injected {
            return Err(e);
        }
        let Some(idx) = s.sessions.iter().position(|v| v.id == request.session_id) else {
            return Err(api_error(404, "Session not found for this device"));
        };
        match &request.action {
            SessionAction::End { .. } | SessionAction::ReportFailure { .. } => {
                let removed = s.sessions.remove(idx);
                let state = if matches!(request.action, SessionAction::End { .. }) {
                    "ended"
                } else {
                    "failed"
                };
                Ok(SessionResponse {
                    state: state.into(),
                    consent_expires_at: removed.consent_expires_at,
                })
            }
            action => {
                let view = &mut s.sessions[idx];
                match action {
                    SessionAction::MediaConnected { .. } | SessionAction::Resume => {
                        view.state = "active".into();
                    }
                    SessionAction::Pause => view.state = "paused".into(),
                    SessionAction::Reconnecting => view.state = "reconnecting".into(),
                    SessionAction::RenewConsent { minutes } => {
                        if view.mode != SessionMode::Attended {
                            return Err(api_error(
                                400,
                                "Only attended sessions use consent renewal",
                            ));
                        }
                        let until = now + i64::from(minutes.unwrap_or(30)) * 60_000;
                        view.consent_expires_at =
                            Some(until.min(view.max_ends_at.unwrap_or(i64::MAX)));
                    }
                    SessionAction::SetPermissions { grant } => {
                        let allowed = view
                            .granted_permissions
                            .union(&view.pending_permissions.unwrap_or(Permissions::NONE));
                        if !grant.is_subset_of(&allowed) {
                            return Err(api_error(
                                400,
                                "Cannot grant scopes that were not requested",
                            ));
                        }
                        if view.mode == SessionMode::Unattended && grant.microphone {
                            return Err(api_error(403, "Microphone requires an attended session"));
                        }
                        view.granted_permissions = *grant;
                        view.pending_permissions = None;
                    }
                    SessionAction::DeclinePermissions => view.pending_permissions = None,
                    SessionAction::End { .. } | SessionAction::ReportFailure { .. } => {}
                }
                Ok(SessionResponse {
                    state: view.state.clone(),
                    consent_expires_at: view.consent_expires_at,
                })
            }
        }
    }

    async fn media_credentials(
        &self,
        session_id: &str,
    ) -> Result<MediaCredentialsResponse, HostError> {
        self.trace
            .push(format!("api:media_credentials:{session_id}"));
        if let Some(e) = self.next_action_error() {
            return Err(e);
        }
        let s = self.state.lock().unwrap();
        let Some(view) = s.sessions.iter().find(|v| v.id == session_id) else {
            return Err(api_error(404, "Session not found for this device"));
        };
        if view.state == "awaiting_approval" {
            return Err(HostError::Api {
                status: 409,
                code: Some("SESSION_NOT_LIVE".into()),
                message: format!("Session is {}", view.state),
            });
        }
        match &s.media {
            Some(creds) => Ok(creds.clone()),
            None => Err(HostError::Api {
                status: 503,
                code: Some("MEDIA_UNCONFIGURED".into()),
                message: "The media service isn't connected yet".into(),
            }),
        }
    }

    async fn banner_report(
        &self,
        request: &BannerReportRequest,
    ) -> Result<BannerReportResponse, HostError> {
        self.state
            .lock()
            .unwrap()
            .requests
            .push(Recorded::BannerReport(request.clone()));
        self.trace.push("api:banner_report");
        Ok(BannerReportResponse { ok: true })
    }
}

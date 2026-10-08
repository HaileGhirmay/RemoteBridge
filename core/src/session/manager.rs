//! The host session manager.
//!
//! Driven by [`SessionManager::tick`], which the host calls often (every few
//! hundred milliseconds is plenty). Each tick: handle answers from the local
//! user, send queued actions, poll the server if due, then enforce timers.
//!
//! Rules this module upholds (AGENTS.md):
//! * The server is the source of truth. A session that disappears or turns
//!   terminal is stopped at once.
//! * Scheduling uses the monotonic clock only. Server deadlines are converted
//!   to monotonic deadlines when a poll arrives (`expiresAt - serverTime`), so
//!   a wrong or jumping wall clock can never extend consent.
//! * Consent and the lease are enforced locally too: when consent, the maximum
//!   lifetime or the 120 s lease runs out, capture stops without waiting for
//!   the server to say so.
//! * More access always needs a new local approval (attended), or is limited
//!   by the owner's unattended policy plus the local opt-in.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use crate::Permissions;
use crate::protocol::types::{
    BannerReportRequest, DecideRequest, Decision, DeviceView, EndReason, FailureReason,
    HostSessionView, MediaCredentialsResponse, PolicyView, PollResponse, SessionAction,
    SessionMode, SessionRequest, SessionState,
};
use crate::protocol::{HostError, UnixMs};
use crate::traits::{Clock, ConsentAnswer, ConsentPrompt, ConsentUi, KeyStore, Notice, PromptId};

use super::api::HostApi;
use super::effects::SessionEffects;

/// Poll interval while any session is pending or live.
pub const POLL_BUSY: Duration = Duration::from_secs(15);
/// Poll interval when idle.
pub const POLL_IDLE: Duration = Duration::from_secs(45);
/// The server expires a session if the host has not polled for this long.
pub const LEASE: Duration = Duration::from_secs(120);
/// The "Keep sharing?" prompt appears this long before consent runs out.
pub const RENEWAL_LEAD: Duration = Duration::from_secs(120);

const MIN_CONSENT_MINUTES: u32 = 30;
const MAX_CONSENT_MINUTES: u32 = 480;
const MAX_ACTION_ATTEMPTS: u8 = 3;
/// Pause before an action that failed (network trouble) is sent again.
const ACTION_RETRY_DELAY: Duration = Duration::from_secs(2);

/// Local settings, set by a local administrator on this machine.
#[derive(Debug, Clone, Default)]
pub struct HostSettings {
    /// Unattended sessions are only ever approved if this is set locally, in
    /// addition to the owner enabling unattended access on the website.
    pub unattended_opt_in: bool,
    /// DTLS fingerprint (`sha-256 AB:CD:...`) sent with `decide`. `None` until
    /// the media layer exists (Prompt 08).
    pub host_fingerprint: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagerState {
    Running,
    /// The server said `DEVICE_REVOKED`. The key is wiped; nothing runs.
    Revoked,
}

/// What the media layer tells the manager to pass on to the server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaReport {
    /// Media is flowing (again). `relay_used` is true if the selected path is a relay.
    Connected {
        relay_used: bool,
    },
    Reconnecting,
    Failed(FailureReason),
}

/// A support code and when it stops working.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InviteInfo {
    /// 12 digits.
    pub code: String,
    deadline: Duration,
}

/// What the banner and tray need to know about a session.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionSnapshot {
    pub id: String,
    pub mode: SessionMode,
    pub requester_name: String,
    pub requester_email: String,
    pub state: String,
    pub granted: Permissions,
    pub pending: Option<Permissions>,
    /// Server time consent ends. Changes on every renewal, so it identifies
    /// the consent epoch (the indicators come back when it changes).
    pub consent_expires_at: Option<UnixMs>,
    /// Time left on consent (attended), on the monotonic clock.
    pub consent_remaining: Option<Duration>,
    pub max_remaining: Option<Duration>,
    /// Media is currently allowed to run.
    pub running: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StopCause {
    LeaseLost,
    ConsentExpired,
    MaxLifetime,
}

#[derive(Debug)]
struct Tracked {
    view: HostSessionView,
    in_pending: bool,
    approval_deadline: Option<Duration>,
    consent_deadline: Option<Duration>,
    max_deadline: Option<Duration>,
    started: bool,
    stopped: Option<StopCause>,
    cleanup_sent: bool,
    decided: bool,
    approval_prompt: Option<PromptId>,
    renewal_prompt: Option<PromptId>,
    renewal_prompted_for: Option<UnixMs>,
    escalation_prompt: Option<PromptId>,
    escalation_answered: Option<Permissions>,
    last_grant: Permissions,
}

impl Tracked {
    fn new(view: HostSessionView) -> Self {
        let last_grant = view.granted_permissions;
        Self {
            view,
            in_pending: false,
            approval_deadline: None,
            consent_deadline: None,
            max_deadline: None,
            started: false,
            stopped: None,
            cleanup_sent: false,
            decided: false,
            approval_prompt: None,
            renewal_prompt: None,
            renewal_prompted_for: None,
            escalation_prompt: None,
            escalation_answered: None,
            last_grant,
        }
    }

    fn prompt_ids(&self) -> impl Iterator<Item = PromptId> {
        [
            self.approval_prompt,
            self.renewal_prompt,
            self.escalation_prompt,
        ]
        .into_iter()
        .flatten()
    }
}

#[derive(Debug, Clone)]
enum PromptKind {
    Approval(String),
    Renewal(String),
    Escalation(String, Permissions),
    Notice,
}

#[derive(Debug, Clone)]
enum Action {
    Decide(DecideRequest),
    Session(SessionRequest),
}

struct Outbound {
    action: Action,
    attempts: u8,
    /// A failed attempt is not repeated before this monotonic time.
    not_before: Option<Duration>,
}

/// Grant for an unattended request, or `None` to deny it.
///
/// Only with the owner's website setting **and** the local opt-in. Control
/// and audio follow the owner's policy; the microphone is never granted
/// without a person at the machine; clipboard is never granted unattended
/// because no policy flag exists that could have opted in to it.
pub fn unattended_grant(
    requested: &Permissions,
    device: &DeviceView,
    policy: &PolicyView,
    settings: &HostSettings,
) -> Option<Permissions> {
    if !device.unattended_enabled || !settings.unattended_opt_in {
        return None;
    }
    Some(Permissions {
        control: requested.control && policy.allow_control_unattended,
        system_audio: requested.system_audio && policy.allow_audio_unattended,
        microphone: false,
        clipboard: false,
    })
}

/// New grant for an unattended escalation, or `None` to decline it: the
/// current grant plus whatever the unattended rules allow of the request.
pub fn unattended_escalation(
    current: &Permissions,
    pending: &Permissions,
    device: &DeviceView,
    policy: &PolicyView,
    settings: &HostSettings,
) -> Option<Permissions> {
    let allowed = unattended_grant(pending, device, policy, settings)?;
    let next = current.union(&allowed);
    (next != *current).then_some(next)
}

/// `expires_at - server_time` as a monotonic deadline.
fn deadline(now: Duration, expires_at: UnixMs, server_time: UnixMs) -> Duration {
    let remaining = u64::try_from(expires_at.saturating_sub(server_time)).unwrap_or(0);
    now + Duration::from_millis(remaining)
}

pub struct SessionManager<A: HostApi> {
    api: A,
    clock: Arc<dyn Clock>,
    ui: Box<dyn ConsentUi>,
    effects: Box<dyn SessionEffects>,
    keys: Arc<dyn KeyStore>,
    settings: HostSettings,
    state: ManagerState,
    sessions: Vec<Tracked>,
    prompts: HashMap<PromptId, PromptKind>,
    /// Sessions we ended locally. If the server still lists one, end it again;
    /// never restart or prompt for it.
    ended_locally: HashSet<String>,
    outbox: VecDeque<Outbound>,
    device: Option<DeviceView>,
    policy: Option<PolicyView>,
    update_required: bool,
    last_poll: Option<Duration>,
    last_poll_ok: Option<Duration>,
    force_poll: bool,
    server_offset_ms: Option<i64>,
    invite: Option<InviteInfo>,
    last_error: Option<HostError>,
}

impl<A: HostApi> SessionManager<A> {
    pub fn new(
        api: A,
        clock: Arc<dyn Clock>,
        ui: Box<dyn ConsentUi>,
        effects: Box<dyn SessionEffects>,
        keys: Arc<dyn KeyStore>,
        settings: HostSettings,
    ) -> Self {
        Self {
            api,
            clock,
            ui,
            effects,
            keys,
            settings,
            state: ManagerState::Running,
            sessions: Vec::new(),
            prompts: HashMap::new(),
            ended_locally: HashSet::new(),
            outbox: VecDeque::new(),
            device: None,
            policy: None,
            update_required: false,
            last_poll: None,
            last_poll_ok: None,
            force_poll: false,
            server_offset_ms: None,
            invite: None,
            last_error: None,
        }
    }

    // ---- observation -----------------------------------------------------

    pub fn state(&self) -> ManagerState {
        self.state
    }

    pub fn settings(&self) -> &HostSettings {
        &self.settings
    }

    pub fn set_settings(&mut self, settings: HostSettings) {
        self.settings = settings;
    }

    /// The server asked this host to update (`updateRequired`). Acting on it
    /// belongs to the updater (Prompt 12).
    pub fn update_required(&self) -> bool {
        self.update_required
    }

    pub fn last_error(&self) -> Option<&HostError> {
        self.last_error.as_ref()
    }

    /// Any session pending or live.
    pub fn is_busy(&self) -> bool {
        !self.sessions.is_empty()
    }

    /// How long until the next poll is due. Zero means now.
    pub fn next_poll_in(&self) -> Duration {
        if self.force_poll {
            return Duration::ZERO;
        }
        match self.last_poll {
            None => Duration::ZERO,
            Some(t) => {
                let due = t + self.poll_interval();
                due.saturating_sub(self.clock.mono())
            }
        }
    }

    pub fn sessions(&self) -> Vec<SessionSnapshot> {
        let now = self.clock.mono();
        self.sessions
            .iter()
            .map(|t| SessionSnapshot {
                id: t.view.id.clone(),
                mode: t.view.mode,
                requester_name: t.view.requester.display_name.clone(),
                requester_email: t.view.requester.email.clone(),
                state: t.view.state.clone(),
                granted: t.view.granted_permissions,
                pending: t.view.pending_permissions,
                consent_expires_at: t.view.consent_expires_at,
                consent_remaining: t.consent_deadline.map(|d| d.saturating_sub(now)),
                max_remaining: t.max_deadline.map(|d| d.saturating_sub(now)),
                running: t.started && t.stopped.is_none(),
            })
            .collect()
    }

    /// Whole seconds left on the support code, or `None` if there is none or
    /// it has expired.
    pub fn invite_seconds_left(&self) -> Option<u64> {
        let invite = self.invite.as_ref()?;
        let left = invite.deadline.checked_sub(self.clock.mono())?;
        (!left.is_zero()).then_some(left.as_secs())
    }

    pub fn invite(&self) -> Option<&InviteInfo> {
        self.invite
            .as_ref()
            .filter(|_| self.invite_seconds_left().is_some())
    }

    // ---- the loop ----------------------------------------------------------

    pub async fn tick(&mut self) {
        if self.state == ManagerState::Revoked {
            return;
        }
        self.process_answers();
        self.flush_outbox().await;
        if self.state == ManagerState::Revoked {
            return;
        }
        if self.poll_due() {
            self.do_poll().await;
            if self.state == ManagerState::Revoked {
                return;
            }
        }
        self.run_timers();
        self.flush_outbox().await;
        // A successful action wants an immediate look at what the server
        // decided (for example to start media right after an approval).
        if self.force_poll && self.state == ManagerState::Running {
            self.do_poll().await;
            self.run_timers();
        }
    }

    fn poll_interval(&self) -> Duration {
        if self.is_busy() { POLL_BUSY } else { POLL_IDLE }
    }

    fn poll_due(&self) -> bool {
        self.next_poll_in().is_zero()
    }

    async fn do_poll(&mut self) {
        self.force_poll = false;
        let result = self.api.poll().await;
        let now = self.clock.mono();
        self.last_poll = Some(now);
        match result {
            Ok(poll) => {
                self.last_poll_ok = Some(now);
                self.last_error = None;
                self.reconcile(poll);
            }
            Err(HostError::DeviceRevoked) => self.handle_revoked(),
            Err(e) => self.last_error = Some(e),
        }
    }

    // ---- reconciling with the server --------------------------------------

    fn reconcile(&mut self, poll: PollResponse) {
        let now = self.clock.mono();
        let server_time = poll.server_time;
        self.server_offset_ms = Some(server_time - self.clock.wall_ms());
        self.update_required = poll.update_required;
        let device = poll.device;
        let policy = poll.policy;
        self.device = Some(device.clone());
        self.policy = Some(policy.clone());

        let mut old = std::mem::take(&mut self.sessions);
        let mut next = Vec::new();
        let mut listed = HashSet::new();

        let items = poll
            .pending
            .into_iter()
            .map(|v| (v, true))
            .chain(poll.live.into_iter().map(|v| (v, false)));
        for (view, in_pending) in items {
            if view.parsed_state().is_some_and(SessionState::is_terminal) {
                continue; // treated as gone: any tracked copy is dropped below
            }
            listed.insert(view.id.clone());
            if self.ended_locally.contains(&view.id) {
                // We ended this one; the server has not caught up. Say so again.
                self.queue_end_again(&view.id);
                continue;
            }
            let mut tracked = match old.iter().position(|t| t.view.id == view.id) {
                Some(i) => old.swap_remove(i),
                None => Tracked::new(view.clone()),
            };
            tracked.in_pending = in_pending;
            tracked.approval_deadline = view
                .approval_expires_at
                .map(|t| deadline(now, t, server_time));
            tracked.consent_deadline = view
                .consent_expires_at
                .map(|t| deadline(now, t, server_time));
            tracked.max_deadline = view.max_ends_at.map(|t| deadline(now, t, server_time));
            tracked.view = view;
            if in_pending {
                self.handle_pending(&mut tracked, &device, &policy, now);
            } else {
                self.handle_live(&mut tracked, &device, &policy);
            }
            next.push(tracked);
        }
        for gone in old {
            self.drop_session(gone);
        }
        self.ended_locally.retain(|id| listed.contains(id));
        self.sessions = next;
    }

    fn handle_pending(
        &mut self,
        t: &mut Tracked,
        device: &DeviceView,
        policy: &PolicyView,
        now: Duration,
    ) {
        if t.decided || t.approval_prompt.is_some() {
            return;
        }
        if t.approval_deadline.is_some_and(|d| now >= d) {
            return; // approval window over; the server will time it out
        }
        let id = t.view.id.clone();
        match t.view.mode {
            SessionMode::Attended => {
                let prompt = ConsentPrompt::AttendedRequest {
                    requester_name: t.view.requester.display_name.clone(),
                    requester_email: t.view.requester.email.clone(),
                    requester_verified: t.view.requester.verified,
                    requested: t.view.requested_permissions,
                };
                match self.show_prompt(prompt, PromptKind::Approval(id.clone())) {
                    Some(pid) => t.approval_prompt = Some(pid),
                    None => {
                        // No way to ask means no consent.
                        t.decided = true;
                        self.enqueue_deny(&id);
                    }
                }
            }
            SessionMode::Unattended => {
                t.decided = true;
                match unattended_grant(
                    &t.view.requested_permissions,
                    device,
                    policy,
                    &self.settings,
                ) {
                    Some(grant) => self.enqueue(Action::Decide(DecideRequest {
                        session_id: id,
                        decision: Decision::Approve,
                        grant: Some(grant),
                        approved_minutes: None,
                        host_fingerprint: self.settings.host_fingerprint.clone(),
                    })),
                    None => self.enqueue_deny(&id),
                }
            }
        }
    }

    fn handle_live(&mut self, t: &mut Tracked, device: &DeviceView, policy: &PolicyView) {
        if let Some(pid) = t.approval_prompt.take() {
            self.withdraw(pid);
        }
        if t.stopped.is_some() {
            self.queue_cleanup(t);
            return;
        }
        if !t.started {
            self.effects.start_session(&t.view);
            t.started = true;
            t.last_grant = t.view.granted_permissions;
        } else if t.view.granted_permissions != t.last_grant {
            self.effects
                .update_grant(&t.view.id, &t.view.granted_permissions);
            t.last_grant = t.view.granted_permissions;
        }

        match t.view.pending_permissions {
            Some(pending) => self.handle_escalation(t, pending, device, policy),
            None => {
                t.escalation_answered = None;
                if let Some(pid) = t.escalation_prompt.take() {
                    self.withdraw(pid);
                }
            }
        }
    }

    fn handle_escalation(
        &mut self,
        t: &mut Tracked,
        pending: Permissions,
        device: &DeviceView,
        policy: &PolicyView,
    ) {
        if t.escalation_answered == Some(pending) {
            return;
        }
        let id = t.view.id.clone();
        let current = t.view.granted_permissions;
        let added = pending.added_over(&current);
        if !added.any() {
            t.escalation_answered = Some(pending);
            self.enqueue_session(&id, SessionAction::DeclinePermissions);
            return;
        }
        match t.view.mode {
            SessionMode::Unattended => {
                t.escalation_answered = Some(pending);
                match unattended_escalation(&current, &pending, device, policy, &self.settings) {
                    Some(grant) => {
                        self.enqueue_session(&id, SessionAction::SetPermissions { grant });
                    }
                    None => self.enqueue_session(&id, SessionAction::DeclinePermissions),
                }
            }
            SessionMode::Attended => {
                if let Some(pid) = t.escalation_prompt {
                    let same_request = matches!(
                        self.prompts.get(&pid),
                        Some(PromptKind::Escalation(_, asked)) if *asked == pending
                    );
                    if same_request {
                        return; // already asking about exactly this
                    }
                    self.withdraw(pid); // the request changed; ask again
                    t.escalation_prompt = None;
                }
                let prompt = ConsentPrompt::PermissionEscalation {
                    requester_name: t.view.requester.display_name.clone(),
                    added,
                };
                match self.show_prompt(prompt, PromptKind::Escalation(id.clone(), pending)) {
                    Some(pid) => t.escalation_prompt = Some(pid),
                    None => {
                        t.escalation_answered = Some(pending);
                        self.enqueue_session(&id, SessionAction::DeclinePermissions);
                    }
                }
            }
        }
    }

    /// The session is gone (or terminal) on the server.
    fn drop_session(&mut self, gone: Tracked) {
        for pid in gone.prompt_ids() {
            self.withdraw(pid);
        }
        if gone.started && gone.stopped.is_none() {
            self.effects.stop_session(&gone.view.id);
        }
    }

    /// We stopped a session locally but the server still lists it: make the
    /// server agree.
    fn queue_cleanup(&mut self, t: &mut Tracked) {
        if t.cleanup_sent {
            return;
        }
        t.cleanup_sent = true;
        let action = match t.stopped {
            Some(StopCause::LeaseLost) => SessionAction::ReportFailure {
                reason: FailureReason::DeviceOffline,
            },
            _ => SessionAction::End {
                reason: EndReason::HostStop,
            },
        };
        self.enqueue_session(&t.view.id.clone(), action);
    }

    fn queue_end_again(&mut self, id: &str) {
        self.enqueue_session(
            id,
            SessionAction::End {
                reason: EndReason::EmergencyDisconnect,
            },
        );
    }

    // ---- answers from the local user ---------------------------------------

    fn process_answers(&mut self) {
        while let Some((pid, answer)) = self.ui.poll() {
            let Some(kind) = self.prompts.remove(&pid) else {
                continue; // withdrawn or unknown
            };
            match kind {
                PromptKind::Notice => {}
                PromptKind::Approval(id) => self.answer_approval(&id, pid, answer),
                PromptKind::Renewal(id) => self.answer_renewal(&id, pid, answer),
                PromptKind::Escalation(id, pending) => {
                    self.answer_escalation(&id, pid, pending, answer);
                }
            }
        }
    }

    fn answer_approval(&mut self, id: &str, pid: PromptId, answer: ConsentAnswer) {
        let Some(t) = self.sessions.iter_mut().find(|t| t.view.id == id) else {
            return;
        };
        if t.approval_prompt != Some(pid) {
            return;
        }
        t.approval_prompt = None;
        t.decided = true;
        let requested = t.view.requested_permissions;
        let request = match answer {
            ConsentAnswer::Approve {
                grant,
                approved_minutes,
            } => {
                let cap = self
                    .policy
                    .as_ref()
                    .map_or(MAX_CONSENT_MINUTES, |p| p.max_session_minutes)
                    .clamp(MIN_CONSENT_MINUTES, MAX_CONSENT_MINUTES);
                DecideRequest {
                    session_id: id.into(),
                    decision: Decision::Approve,
                    // Never more than was asked for.
                    grant: Some(grant.intersect(&requested)),
                    approved_minutes: Some(approved_minutes.clamp(MIN_CONSENT_MINUTES, cap)),
                    host_fingerprint: self.settings.host_fingerprint.clone(),
                }
            }
            // Deny, or the prompt was closed without a choice: no.
            _ => deny_request(id),
        };
        self.enqueue(Action::Decide(request));
    }

    fn answer_renewal(&mut self, id: &str, pid: PromptId, answer: ConsentAnswer) {
        let Some(t) = self.sessions.iter_mut().find(|t| t.view.id == id) else {
            return;
        };
        if t.renewal_prompt != Some(pid) {
            return;
        }
        t.renewal_prompt = None;
        if let ConsentAnswer::Renew { minutes } = answer {
            let minutes = minutes.clamp(MIN_CONSENT_MINUTES, MAX_CONSENT_MINUTES);
            self.enqueue_session(
                id,
                SessionAction::RenewConsent {
                    minutes: Some(minutes),
                },
            );
        }
        // Anything else: let consent run out.
    }

    fn answer_escalation(
        &mut self,
        id: &str,
        pid: PromptId,
        pending: Permissions,
        answer: ConsentAnswer,
    ) {
        let Some(t) = self.sessions.iter_mut().find(|t| t.view.id == id) else {
            return;
        };
        if t.escalation_prompt != Some(pid) {
            return;
        }
        t.escalation_prompt = None;
        t.escalation_answered = Some(pending);
        let current = t.view.granted_permissions;
        let action = if matches!(answer, ConsentAnswer::Allow) {
            SessionAction::SetPermissions {
                grant: current.union(&pending),
            }
        } else {
            SessionAction::DeclinePermissions
        };
        self.enqueue_session(id, action);
    }

    // ---- timers ----------------------------------------------------------

    fn run_timers(&mut self) {
        let now = self.clock.mono();
        let lease_lost = self
            .last_poll_ok
            .is_some_and(|t| now.saturating_sub(t) > LEASE);
        let mut sessions = std::mem::take(&mut self.sessions);
        for t in &mut sessions {
            if t.approval_deadline.is_some_and(|d| now >= d)
                && let Some(pid) = t.approval_prompt.take()
            {
                self.withdraw(pid);
            }
            if t.consent_deadline.is_some_and(|d| now >= d)
                && let Some(pid) = t.renewal_prompt.take()
            {
                self.withdraw(pid);
            }

            if t.started && t.stopped.is_none() {
                let cause = if lease_lost {
                    Some(StopCause::LeaseLost)
                } else if t.consent_deadline.is_some_and(|d| now >= d) {
                    Some(StopCause::ConsentExpired)
                } else if t.max_deadline.is_some_and(|d| now >= d) {
                    Some(StopCause::MaxLifetime)
                } else {
                    None
                };
                if let Some(cause) = cause {
                    self.effects.stop_session(&t.view.id);
                    t.stopped = Some(cause);
                    for pid in [t.renewal_prompt.take(), t.escalation_prompt.take()]
                        .into_iter()
                        .flatten()
                    {
                        self.withdraw(pid);
                    }
                    continue;
                }
            }

            self.maybe_prompt_renewal(t, now);
        }
        self.sessions = sessions;
    }

    fn maybe_prompt_renewal(&mut self, t: &mut Tracked, now: Duration) {
        if t.view.mode != SessionMode::Attended
            || !t.started
            || t.stopped.is_some()
            || t.renewal_prompt.is_some()
        {
            return;
        }
        let Some(deadline) = t.consent_deadline else {
            return;
        };
        let remaining = deadline.saturating_sub(now);
        if remaining.is_zero() || remaining > RENEWAL_LEAD {
            return;
        }
        // Once per consent window.
        if t.renewal_prompted_for == t.view.consent_expires_at {
            return;
        }
        t.renewal_prompted_for = t.view.consent_expires_at;
        let prompt = ConsentPrompt::RenewConsent {
            requester_name: t.view.requester.display_name.clone(),
        };
        if let Some(pid) = self.show_prompt(prompt, PromptKind::Renewal(t.view.id.clone())) {
            t.renewal_prompt = Some(pid);
        }
    }

    // ---- outbound actions ----------------------------------------------------

    fn enqueue(&mut self, action: Action) {
        self.outbox.push_back(Outbound {
            action,
            attempts: 0,
            not_before: None,
        });
    }

    fn enqueue_deny(&mut self, id: &str) {
        self.enqueue(Action::Decide(deny_request(id)));
    }

    fn enqueue_session(&mut self, id: &str, action: SessionAction) {
        self.enqueue(Action::Session(SessionRequest {
            session_id: id.into(),
            action,
        }));
    }

    async fn send(&mut self, action: &Action) -> Result<(), HostError> {
        match action {
            Action::Decide(r) => self.api.decide(r).await.map(|_| ()),
            Action::Session(r) => self.api.session(r).await.map(|_| ()),
        }
    }

    async fn flush_outbox(&mut self) {
        let queued = std::mem::take(&mut self.outbox);
        for mut item in queued {
            let now = self.clock.mono();
            if item.not_before.is_some_and(|t| now < t) {
                self.outbox.push_back(item);
                continue;
            }
            match self.send(&item.action).await {
                Ok(()) => self.force_poll = true,
                Err(HostError::DeviceRevoked) => {
                    self.handle_revoked();
                    return;
                }
                Err(e) if e.is_retryable() && item.attempts + 1 < MAX_ACTION_ATTEMPTS => {
                    item.attempts += 1;
                    item.not_before = Some(now + ACTION_RETRY_DELAY);
                    self.last_error = Some(e);
                    self.outbox.push_back(item);
                }
                // Not worth repeating (409, 404, 400, ...): the next poll
                // shows what the server decided.
                Err(e) => {
                    self.last_error = Some(e);
                    self.force_poll = true;
                }
            }
        }
    }

    // ---- prompts ---------------------------------------------------------------

    fn show_prompt(&mut self, prompt: ConsentPrompt, kind: PromptKind) -> Option<PromptId> {
        match self.ui.show(prompt) {
            Ok(id) => {
                self.prompts.insert(id, kind);
                Some(id)
            }
            Err(_) => None,
        }
    }

    fn withdraw(&mut self, id: PromptId) {
        self.ui.withdraw(id);
        self.prompts.remove(&id);
    }

    // ---- teardown ---------------------------------------------------------------

    fn handle_revoked(&mut self) {
        self.effects.stop_all();
        let ids: Vec<PromptId> = self.prompts.keys().copied().collect();
        for id in ids {
            self.withdraw(id);
        }
        self.sessions.clear();
        self.outbox.clear();
        self.invite = None;
        self.ended_locally.clear();
        if let Err(e) = self.keys.wipe() {
            self.last_error = Some(HostError::Key(e));
        }
        if let Ok(id) = self.ui.show(ConsentPrompt::Notice(Notice::DeviceRemoved)) {
            self.prompts.insert(id, PromptKind::Notice);
        }
        self.state = ManagerState::Revoked;
    }

    /// Emergency disconnect (Ctrl+Alt+Shift+X, tray, banner): stop all
    /// capture and input locally **first**, then tell the server. Pending
    /// requests are denied. Returns the errors from telling the server; the
    /// local stop has happened regardless.
    pub async fn emergency_disconnect(&mut self) -> Vec<HostError> {
        self.end_everything(EndReason::EmergencyDisconnect).await
    }

    /// Orderly end of everything (for example the host app is quitting).
    pub async fn shutdown(&mut self) -> Vec<HostError> {
        self.end_everything(EndReason::HostShutdown).await
    }

    async fn end_everything(&mut self, reason: EndReason) -> Vec<HostError> {
        // 1. Local first. Nothing below may delay this.
        self.effects.stop_all();
        self.outbox.clear();
        let sessions = std::mem::take(&mut self.sessions);
        for t in &sessions {
            for pid in t.prompt_ids() {
                self.withdraw(pid);
            }
            self.ended_locally.insert(t.view.id.clone());
        }
        // 2. Then the server.
        let mut errors = Vec::new();
        for t in sessions {
            let id = t.view.id.clone();
            let result = if t.in_pending && !t.started {
                self.api.decide(&deny_request(&id)).await.map(|_| ())
            } else {
                self.api
                    .session(&SessionRequest {
                        session_id: id,
                        action: SessionAction::End { reason },
                    })
                    .await
                    .map(|_| ())
            };
            match result {
                Ok(()) => {}
                Err(HostError::DeviceRevoked) => {
                    errors.push(HostError::DeviceRevoked);
                    self.handle_revoked();
                    return errors;
                }
                Err(e) => errors.push(e),
            }
        }
        self.force_poll = true;
        errors
    }

    /// End one session at the local user's request (banner "Disconnect").
    pub async fn end_session(&mut self, id: &str, reason: EndReason) -> Result<(), HostError> {
        self.effects.stop_session(id);
        self.ended_locally.insert(id.to_owned());
        if let Some(i) = self.sessions.iter().position(|t| t.view.id == id) {
            let t = self.sessions.remove(i);
            for pid in t.prompt_ids() {
                self.withdraw(pid);
            }
        }
        let result = self
            .api
            .session(&SessionRequest {
                session_id: id.into(),
                action: SessionAction::End { reason },
            })
            .await
            .map(|_| ());
        match &result {
            Err(HostError::DeviceRevoked) => self.handle_revoked(),
            _ => self.force_poll = true,
        }
        result
    }

    /// Show a notice (shortcut collision, the first-time hide message, missing
    /// permissions) through the same local UI as the consent prompts.
    pub fn notify(&mut self, notice: Notice) {
        if let Ok(id) = self.ui.show(ConsentPrompt::Notice(notice)) {
            self.prompts.insert(id, PromptKind::Notice);
        }
    }

    /// `host/banner_report`, with revocation handled like every other call.
    pub async fn banner_report(&mut self, request: &BannerReportRequest) -> Result<(), HostError> {
        match self.api.banner_report(request).await {
            Err(HostError::DeviceRevoked) => {
                self.handle_revoked();
                Err(HostError::DeviceRevoked)
            }
            other => other.map(|_| ()),
        }
    }

    // ---- media ----------------------------------------------------------------

    /// Credentials for a live session's media path (signaling token, ICE
    /// servers, and the viewer fingerprint the server vouches for).
    pub async fn media_credentials(
        &mut self,
        session_id: &str,
    ) -> Result<MediaCredentialsResponse, HostError> {
        match self.api.media_credentials(session_id).await {
            Err(HostError::DeviceRevoked) => {
                self.handle_revoked();
                Err(HostError::DeviceRevoked)
            }
            other => other,
        }
    }

    /// The media layer says something happened to a session. Reported to the
    /// server on the next tick: `media_connected` (with whether a relay is
    /// used), `reconnecting`, or `report_failure`.
    pub fn report_media(&mut self, session_id: &str, report: MediaReport) {
        // Only for sessions we still run: a late report for one that ended
        // locally would only earn a 409.
        if !self.sessions.iter().any(|t| t.view.id == session_id) {
            return;
        }
        let action = match report {
            MediaReport::Connected { relay_used } => SessionAction::MediaConnected { relay_used },
            MediaReport::Reconnecting => SessionAction::Reconnecting,
            MediaReport::Failed(reason) => SessionAction::ReportFailure { reason },
        };
        self.enqueue_session(session_id, action);
    }

    // ---- support code ---------------------------------------------------------

    /// "Get help": create a 12-digit support code valid for 10 minutes.
    pub async fn get_help(&mut self) -> Result<InviteInfo, HostError> {
        match self.api.invite().await {
            Ok(response) => {
                let wall = self.clock.wall_ms() + self.server_offset_ms.unwrap_or(0);
                let remaining =
                    u64::try_from(response.expires_at.saturating_sub(wall)).unwrap_or(0);
                let info = InviteInfo {
                    code: response.code,
                    deadline: self.clock.mono() + Duration::from_millis(remaining),
                };
                self.invite = Some(info.clone());
                Ok(info)
            }
            Err(HostError::DeviceRevoked) => {
                self.handle_revoked();
                Err(HostError::DeviceRevoked)
            }
            Err(e) => Err(e),
        }
    }
}

fn deny_request(id: &str) -> DecideRequest {
    DecideRequest {
        session_id: id.into(),
        decision: Decision::Deny,
        grant: None,
        approved_minutes: None,
        host_fingerprint: None,
    }
}

#[cfg(test)]
mod tests;

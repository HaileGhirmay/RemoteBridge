//! Typed bodies for every host route, matching the server's schemas exactly.
//!
//! Field names go over the wire in camelCase. Server `Date` values arrive as
//! Unix milliseconds ([`UnixMs`]); see `superjson`.

use serde::{Deserialize, Serialize};

use crate::Permissions;

use super::UnixMs;

// ---- host/enroll ---------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HostPlatform {
    Windows,
    Macos,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JwkBody {
    pub kty: String,
    pub crv: String,
    pub x: String,
    pub y: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnrollRequest {
    /// 8 digits, made by the owner on the website.
    pub code: String,
    /// 1-60 characters.
    pub name: String,
    pub platform: HostPlatform,
    pub host_version: String,
    pub protocol_version: u32,
    pub public_key_jwk: JwkBody,
    /// base64url P1363 signature over `RA-ENROLL-V1\n<code>\n<fingerprint>`.
    pub proof: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnrollResponse {
    pub device_id: String,
    pub fingerprint: String,
}

// ---- host/poll -----------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PollRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host_version: Option<String>,
    pub protocol_version: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionMode {
    Attended,
    Unattended,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Requester {
    pub email: String,
    pub display_name: String,
    pub verified: bool,
}

/// An entry of `pending[]` or `live[]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostSessionView {
    pub id: String,
    pub mode: SessionMode,
    /// Server state name; see [`SessionState`].
    pub state: String,
    pub requester: Requester,
    pub requested_permissions: Permissions,
    pub granted_permissions: Permissions,
    pub pending_permissions: Option<Permissions>,
    pub approval_expires_at: Option<UnixMs>,
    pub consent_expires_at: Option<UnixMs>,
    pub max_ends_at: Option<UnixMs>,
    pub lease_expires_at: Option<UnixMs>,
    pub started_at: Option<UnixMs>,
    pub viewer_fingerprint: Option<String>,
}

impl HostSessionView {
    pub fn parsed_state(&self) -> Option<SessionState> {
        SessionState::parse(&self.state)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceView {
    pub id: String,
    pub name: String,
    pub unattended_enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PolicyView {
    pub consent_renewal_minutes: u32,
    pub max_session_minutes: u32,
    pub allow_control_unattended: bool,
    pub allow_audio_unattended: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PollResponse {
    pub server_time: UnixMs,
    pub update_required: bool,
    pub min_protocol_version: u32,
    pub device: DeviceView,
    pub policy: PolicyView,
    pub pending: Vec<HostSessionView>,
    pub live: Vec<HostSessionView>,
}

/// Server session states.
///
/// `requested → awaiting_approval → connecting → active ↔ paused`,
/// `active/paused ↔ reconnecting`; terminal: `ended`, `expired`, `revoked`,
/// `denied`, `failed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    Requested,
    AwaitingApproval,
    Connecting,
    Active,
    Paused,
    Reconnecting,
    Ended,
    Expired,
    Revoked,
    Denied,
    Failed,
}

impl SessionState {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "requested" => Self::Requested,
            "awaiting_approval" => Self::AwaitingApproval,
            "connecting" => Self::Connecting,
            "active" => Self::Active,
            "paused" => Self::Paused,
            "reconnecting" => Self::Reconnecting,
            "ended" => Self::Ended,
            "expired" => Self::Expired,
            "revoked" => Self::Revoked,
            "denied" => Self::Denied,
            "failed" => Self::Failed,
            _ => return None,
        })
    }

    /// The host must stop capture and input as soon as it sees one of these.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Ended | Self::Expired | Self::Revoked | Self::Denied | Self::Failed
        )
    }

    /// States in which media credentials are issued.
    pub fn is_live(self) -> bool {
        matches!(
            self,
            Self::Connecting | Self::Active | Self::Paused | Self::Reconnecting
        )
    }
}

// ---- host/invite ---------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InviteRequest {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InviteResponse {
    /// 12 digits, valid 10 minutes, single use.
    pub code: String,
    pub expires_at: UnixMs,
}

// ---- host/decide ---------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Decision {
    Approve,
    Deny,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DecideRequest {
    pub session_id: String,
    pub decision: Decision,
    /// Must be a subset of what was requested.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grant: Option<Permissions>,
    /// Attended only; 30-480.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approved_minutes: Option<u32>,
    /// DTLS fingerprint, `sha-256 AB:CD:...`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host_fingerprint: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DecideResponse {
    pub state: String,
    pub failure_reason: Option<String>,
    pub consent_expires_at: Option<UnixMs>,
    pub max_ends_at: Option<UnixMs>,
}

// ---- host/session --------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndReason {
    HostStop,
    EmergencyDisconnect,
    LocalUiLost,
    HostShutdown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureReason {
    IceFailure,
    PermissionsUnavailable,
    UpdateRequired,
    CaptureFailed,
    DeviceOffline,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "action",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum SessionAction {
    MediaConnected {
        relay_used: bool,
    },
    Pause,
    Resume,
    Reconnecting,
    End {
        reason: EndReason,
    },
    RenewConsent {
        /// 30-480; omitted means the policy default.
        #[serde(skip_serializing_if = "Option::is_none")]
        minutes: Option<u32>,
    },
    SetPermissions {
        grant: Permissions,
    },
    DeclinePermissions,
    ReportFailure {
        reason: FailureReason,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionRequest {
    pub session_id: String,
    #[serde(flatten)]
    pub action: SessionAction,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionResponse {
    pub state: String,
    pub consent_expires_at: Option<UnixMs>,
}

// ---- host/banner_report --------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BannerEventType {
    Dismissed,
    Restored,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BannerScope {
    Banner,
    All,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BannerReason {
    UserShowNow,
    Timeout,
    NewSession,
    ParticipantChanged,
    PermissionIncrease,
    ConsentRenewal,
    CrashRecovery,
    ClockAmbiguity,
    ResumeAfterExpiry,
    SessionEnd,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BannerEvent {
    #[serde(rename = "type")]
    pub kind: BannerEventType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_minutes: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<BannerScope>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<BannerReason>,
}

/// Metadata only. The server never changes local banner state in response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BannerReportRequest {
    /// One of 30, 60, 120, 240, 480.
    pub default_hide_minutes: u32,
    pub show_hide_shortcut: String,
    pub disconnect_shortcut: String,
    pub shortcut_collision: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event: Option<BannerEvent>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BannerReportResponse {
    pub ok: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::superjson;
    use serde_json::{Value, json};

    fn wire<T: Serialize>(v: &T) -> Value {
        serde_json::to_value(v).unwrap()
    }

    #[test]
    fn session_actions_serialize_like_the_server_schema() {
        let id = "11111111-1111-4111-8111-111111111111";
        let req = |action| SessionRequest {
            session_id: id.into(),
            action,
        };
        assert_eq!(
            wire(&req(SessionAction::MediaConnected { relay_used: true })),
            json!({"sessionId": id, "action": "media_connected", "relayUsed": true})
        );
        assert_eq!(
            wire(&req(SessionAction::Pause)),
            json!({"sessionId": id, "action": "pause"})
        );
        assert_eq!(
            wire(&req(SessionAction::End {
                reason: EndReason::EmergencyDisconnect
            })),
            json!({"sessionId": id, "action": "end", "reason": "emergency_disconnect"})
        );
        assert_eq!(
            wire(&req(SessionAction::RenewConsent { minutes: None })),
            json!({"sessionId": id, "action": "renew_consent"})
        );
        assert_eq!(
            wire(&req(SessionAction::RenewConsent { minutes: Some(60) })),
            json!({"sessionId": id, "action": "renew_consent", "minutes": 60})
        );
        assert_eq!(
            wire(&req(SessionAction::SetPermissions {
                grant: Permissions {
                    control: true,
                    ..Permissions::NONE
                }
            })),
            json!({"sessionId": id, "action": "set_permissions", "grant":
                {"control": true, "systemAudio": false, "microphone": false, "clipboard": false}})
        );
        assert_eq!(
            wire(&req(SessionAction::DeclinePermissions)),
            json!({"sessionId": id, "action": "decline_permissions"})
        );
        assert_eq!(
            wire(&req(SessionAction::ReportFailure {
                reason: FailureReason::PermissionsUnavailable
            })),
            json!({"sessionId": id, "action": "report_failure", "reason": "permissions_unavailable"})
        );
    }

    #[test]
    fn every_end_and_failure_reason_uses_the_server_spelling() {
        for (r, s) in [
            (EndReason::HostStop, "host_stop"),
            (EndReason::EmergencyDisconnect, "emergency_disconnect"),
            (EndReason::LocalUiLost, "local_ui_lost"),
            (EndReason::HostShutdown, "host_shutdown"),
        ] {
            assert_eq!(wire(&r), json!(s));
        }
        for (r, s) in [
            (FailureReason::IceFailure, "ice_failure"),
            (
                FailureReason::PermissionsUnavailable,
                "permissions_unavailable",
            ),
            (FailureReason::UpdateRequired, "update_required"),
            (FailureReason::CaptureFailed, "capture_failed"),
            (FailureReason::DeviceOffline, "device_offline"),
        ] {
            assert_eq!(wire(&r), json!(s));
        }
    }

    #[test]
    fn decide_omits_unset_optionals() {
        let d = DecideRequest {
            session_id: "s".into(),
            decision: Decision::Deny,
            grant: None,
            approved_minutes: None,
            host_fingerprint: None,
        };
        assert_eq!(wire(&d), json!({"sessionId": "s", "decision": "deny"}));
        let a = DecideRequest {
            session_id: "s".into(),
            decision: Decision::Approve,
            grant: Some(Permissions::NONE),
            approved_minutes: Some(30),
            host_fingerprint: Some("sha-256 AB:CD".into()),
        };
        let v = wire(&a);
        assert_eq!(v["approvedMinutes"], json!(30));
        assert_eq!(v["hostFingerprint"], json!("sha-256 AB:CD"));
        assert_eq!(v["grant"]["systemAudio"], json!(false));
    }

    #[test]
    fn banner_report_uses_type_key_and_snake_reasons() {
        let r = BannerReportRequest {
            default_hide_minutes: 30,
            show_hide_shortcut: "Ctrl+Alt+Shift+B".into(),
            disconnect_shortcut: "Ctrl+Alt+Shift+X".into(),
            shortcut_collision: false,
            event: Some(BannerEvent {
                kind: BannerEventType::Restored,
                session_id: None,
                duration_minutes: None,
                scope: Some(BannerScope::All),
                reason: Some(BannerReason::ClockAmbiguity),
            }),
        };
        assert_eq!(
            wire(&r),
            json!({
                "defaultHideMinutes": 30,
                "showHideShortcut": "Ctrl+Alt+Shift+B",
                "disconnectShortcut": "Ctrl+Alt+Shift+X",
                "shortcutCollision": false,
                "event": {"type": "restored", "scope": "all", "reason": "clock_ambiguity"}
            })
        );
    }

    #[test]
    fn enroll_and_poll_requests_match_schema() {
        let p = PollRequest {
            host_version: None,
            protocol_version: 1,
        };
        assert_eq!(wire(&p), json!({"protocolVersion": 1}));
        assert_eq!(wire(&HostPlatform::Macos), json!("macos"));
        assert_eq!(wire(&InviteRequest {}), json!({}));
    }

    #[test]
    fn session_state_helpers() {
        assert!(SessionState::parse("ended").unwrap().is_terminal());
        assert!(SessionState::parse("denied").unwrap().is_terminal());
        assert!(!SessionState::parse("active").unwrap().is_terminal());
        assert!(SessionState::Paused.is_live());
        assert!(!SessionState::AwaitingApproval.is_live());
        assert_eq!(SessionState::parse("bogus"), None);
    }

    /// A poll answer written by hand from the AGENTS.md route table, in the
    /// shape superjson produces (dates as ISO strings plus a meta map).
    #[test]
    fn decodes_a_poll_response_fixture() {
        let fixture = include_str!("fixtures/poll_response.json");
        let poll: PollResponse = superjson::decode(fixture.as_bytes()).unwrap();

        assert_eq!(poll.server_time, 1_791_462_818_897);
        assert!(!poll.update_required);
        assert_eq!(poll.min_protocol_version, 1);
        assert_eq!(poll.device.name, "Office PC");
        assert!(!poll.device.unattended_enabled);
        assert_eq!(poll.policy.consent_renewal_minutes, 30);
        assert_eq!(poll.policy.max_session_minutes, 480);

        assert!(poll.live.is_empty());
        assert_eq!(poll.pending.len(), 1);
        let s = &poll.pending[0];
        assert_eq!(s.mode, SessionMode::Attended);
        assert_eq!(s.parsed_state(), Some(SessionState::AwaitingApproval));
        assert_eq!(s.requester.display_name, "Sam Helper");
        assert!(s.requester.verified);
        assert!(s.requested_permissions.control);
        assert!(!s.granted_permissions.any());
        assert_eq!(s.pending_permissions, None);
        assert_eq!(s.approval_expires_at, Some(1_791_463_118_897));
        assert_eq!(s.consent_expires_at, None);
        assert_eq!(s.viewer_fingerprint, None);
    }

    #[test]
    fn decodes_invite_and_decide_responses() {
        let invite: InviteResponse = superjson::decode(
            br#"{"json":{"code":"123456789012","expiresAt":"2026-10-08T12:43:38.897Z"},
                 "meta":{"values":{"expiresAt":["Date"]}}}"#,
        )
        .unwrap();
        assert_eq!(invite.code.len(), 12);
        assert_eq!(invite.expires_at, 1_791_463_418_897);

        let decide: DecideResponse = superjson::decode(
            br#"{"json":{"state":"connecting","failureReason":null,
                 "consentExpiresAt":"2026-10-08T13:03:38.897Z","maxEndsAt":"2026-10-08T20:33:38.897Z"},
                 "meta":{"values":{"consentExpiresAt":["Date"],"maxEndsAt":["Date"]}}}"#,
        )
        .unwrap();
        assert_eq!(decide.state, "connecting");
        assert_eq!(decide.failure_reason, None);
        assert_eq!(decide.consent_expires_at, Some(1_791_464_618_897));
        assert_eq!(decide.max_ends_at, Some(1_791_491_618_897));
    }
}

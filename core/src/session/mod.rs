//! Host session management: poll loop, consent, permissions and teardown.
//!
//! The server is the source of truth. The manager mirrors `pending[]` and
//! `live[]` from `host/poll`, asks the local user for consent, and stops
//! capture and input the moment the server (or a local timer) says a session
//! is over.

pub mod api;
pub mod effects;
pub mod filter;
pub mod manager;

pub use api::HostApi;
pub use effects::SessionEffects;
pub use filter::{
    DropReason, FilterStats, PermissionFilter, ViewerMessage, allow_outgoing_clipboard,
};
pub use manager::{
    HostSettings, InviteInfo, ManagerState, SessionManager, SessionSnapshot, unattended_escalation,
    unattended_grant,
};

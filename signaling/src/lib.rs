//! RemoteBridge signaling relay.
//!
//! A tiny WebSocket server for the WebRTC handshake. It follows
//! `AGENTS.md`, "Media path contract", exactly:
//!
//! * A client connects with `?token=<signalingToken>`, an HS256 JWT
//!   (`iss` = `remotebridge`, `aud` = `rb-signaling`, `exp`, `sid`, `role`,
//!   `fp`, `pfp`).
//! * One room per `sid`: at most one host and one viewer. A second socket
//!   with the same role replaces the first.
//! * Relay only: from the host `offer`, `ice`, `bye`; from the viewer
//!   `ready`, `answer`, `ice`, `bye`. Everything else is refused with
//!   `{type:"error",message}`. Messages are relayed unchanged.
//! * `{type:"peer",present}` tells each side when the other joins or leaves.
//! * 64 KB per message, 50 messages per second per socket.
//! * Nothing about message content is ever logged: only session id, role,
//!   connect and disconnect times and the close reason.

mod auth;
mod rooms;
mod server;

pub use auth::{AuthError, Claims, Role, verify_token};
pub use server::{Config, router, serve};

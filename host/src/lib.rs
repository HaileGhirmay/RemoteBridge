//! RemoteBridge host runtime.
//!
//! * [`supervisor`]: one media session per approved session, plus the capture
//!   devices; implements `SessionEffects` so "stop" is immediate.
//! * [`runtime`]: the loop that ties the session manager, the indicators, the
//!   local controls and the media together.
//! * [`shared`]: one input injector and one clipboard for every session.

pub mod runtime;
pub mod shared;
pub mod supervisor;

pub use runtime::HostRuntime;
pub use shared::{SharedClipboard, SharedInjector};
pub use supervisor::{MediaEffects, SupervisorConfig, SupervisorEvent, spawn as spawn_supervisor};

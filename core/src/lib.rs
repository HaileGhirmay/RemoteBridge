//! RemoteBridge host core.
//!
//! Shared, platform-independent logic for the native host. Everything that
//! touches the operating system sits behind the traits in [`traits`], so the
//! protocol client, timers and permission filter can be unit-tested with the
//! fakes in [`fakes`] and a fake clock.

pub mod error;
pub mod traits;
pub mod types;

#[cfg(any(test, feature = "fakes"))]
pub mod fakes;

pub use error::{PlatformError, PlatformResult};
pub use types::Permissions;

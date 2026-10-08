use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Two time sources. Wall time is what the server and users see; monotonic
/// time is used for scheduling and for noticing wall-clock jumps.
pub trait Clock: Send + Sync {
    /// Unix time in milliseconds. Can jump backwards or forwards.
    fn wall_ms(&self) -> i64;
    /// Time since an arbitrary fixed origin. Never goes backwards.
    fn mono(&self) -> Duration;
}

/// The real clock.
#[derive(Debug, Clone, Copy)]
pub struct SystemClock {
    origin: Instant,
}

impl SystemClock {
    pub fn new() -> Self {
        Self {
            origin: Instant::now(),
        }
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for SystemClock {
    fn wall_ms(&self) -> i64 {
        match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(d) => i64::try_from(d.as_millis()).unwrap_or(i64::MAX),
            Err(e) => -i64::try_from(e.duration().as_millis()).unwrap_or(i64::MAX),
        }
    }

    fn mono(&self) -> Duration {
        self.origin.elapsed()
    }
}

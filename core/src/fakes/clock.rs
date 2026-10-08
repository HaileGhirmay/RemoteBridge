use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::traits::Clock;

#[derive(Debug)]
struct State {
    wall_ms: i64,
    mono: Duration,
}

/// A clock that only moves when the test says so.
#[derive(Debug, Clone)]
pub struct FakeClock {
    state: Arc<Mutex<State>>,
}

impl FakeClock {
    pub fn new(start_wall_ms: i64) -> Self {
        Self {
            state: Arc::new(Mutex::new(State {
                wall_ms: start_wall_ms,
                mono: Duration::ZERO,
            })),
        }
    }

    /// Time passes normally: both clocks move.
    pub fn advance(&self, d: Duration) {
        let mut s = self.state.lock().unwrap();
        s.mono += d;
        s.wall_ms += i64::try_from(d.as_millis()).unwrap();
    }

    /// The wall clock jumps (NTP step, user change, sleep) while the
    /// monotonic clock does not.
    pub fn jump_wall(&self, delta_ms: i64) {
        self.state.lock().unwrap().wall_ms += delta_ms;
    }
}

impl Clock for FakeClock {
    fn wall_ms(&self) -> i64 {
        self.state.lock().unwrap().wall_ms
    }

    fn mono(&self) -> Duration {
        self.state.lock().unwrap().mono
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advance_moves_both_clocks() {
        let c = FakeClock::new(1_000);
        c.advance(Duration::from_secs(30));
        assert_eq!(c.wall_ms(), 31_000);
        assert_eq!(c.mono(), Duration::from_secs(30));
    }

    #[test]
    fn wall_jump_leaves_monotonic_alone() {
        let c = FakeClock::new(0);
        let shared = c.clone();
        c.jump_wall(-5_000);
        assert_eq!(shared.wall_ms(), -5_000);
        assert_eq!(shared.mono(), Duration::ZERO);
    }

    #[test]
    fn system_clock_is_monotonic() {
        let c = crate::traits::SystemClock::new();
        let a = c.mono();
        let b = c.mono();
        assert!(b >= a);
        assert!(c.wall_ms() > 1_700_000_000_000);
    }
}

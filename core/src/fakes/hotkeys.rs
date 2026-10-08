use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use crate::PlatformResult;
use crate::traits::{HotkeyEvent, HotkeyRegistration, Hotkeys};

#[derive(Debug)]
struct State {
    restore_ok: bool,
    disconnect_ok: bool,
    registered: bool,
    events: VecDeque<HotkeyEvent>,
    rejected_injected: u32,
}

/// Fake global shortcuts. Models the contract that only physical keyboard
/// input produces events: [`FakeHotkeys::press_injected`] is counted and dropped.
#[derive(Debug, Clone)]
pub struct FakeHotkeys {
    state: Arc<Mutex<State>>,
}

impl Default for FakeHotkeys {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeHotkeys {
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(State {
                restore_ok: true,
                disconnect_ok: true,
                registered: false,
                events: VecDeque::new(),
                rejected_injected: 0,
            })),
        }
    }

    /// Simulate the OS refusing one or both shortcuts.
    pub fn set_collision(&self, restore: bool, disconnect: bool) {
        let mut s = self.state.lock().unwrap();
        s.restore_ok = !restore;
        s.disconnect_ok = !disconnect;
    }

    /// The user pressed the shortcut on the local keyboard.
    pub fn press_physical(&self, event: HotkeyEvent) {
        let mut s = self.state.lock().unwrap();
        let usable = s.registered
            && match event {
                HotkeyEvent::RestoreIndicators => s.restore_ok,
                HotkeyEvent::EmergencyDisconnect => s.disconnect_ok,
            };
        if usable {
            s.events.push_back(event);
        }
    }

    /// Software (for example remote control) generated the shortcut. Rejected.
    pub fn press_injected(&self, _event: HotkeyEvent) {
        self.state.lock().unwrap().rejected_injected += 1;
    }

    pub fn rejected_injected_count(&self) -> u32 {
        self.state.lock().unwrap().rejected_injected
    }

    pub fn is_registered(&self) -> bool {
        self.state.lock().unwrap().registered
    }
}

impl Hotkeys for FakeHotkeys {
    fn register(&mut self) -> PlatformResult<HotkeyRegistration> {
        let mut s = self.state.lock().unwrap();
        s.registered = true;
        Ok(HotkeyRegistration {
            restore: s.restore_ok,
            emergency_disconnect: s.disconnect_ok,
        })
    }

    fn unregister(&mut self) {
        let mut s = self.state.lock().unwrap();
        s.registered = false;
        s.events.clear();
    }

    fn poll(&mut self) -> Option<HotkeyEvent> {
        self.state.lock().unwrap().events.pop_front()
    }

    fn restore_label(&self) -> String {
        "Ctrl+Alt+Shift+B".into()
    }

    fn disconnect_label(&self) -> String {
        "Ctrl+Alt+Shift+X".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_physical_presses_are_delivered() {
        let probe = FakeHotkeys::new();
        let mut hk: Box<dyn Hotkeys> = Box::new(probe.clone());
        let reg = hk.register().unwrap();
        assert!(!reg.collision());

        probe.press_injected(HotkeyEvent::EmergencyDisconnect);
        probe.press_injected(HotkeyEvent::RestoreIndicators);
        assert_eq!(hk.poll(), None);
        assert_eq!(probe.rejected_injected_count(), 2);

        probe.press_physical(HotkeyEvent::RestoreIndicators);
        assert_eq!(hk.poll(), Some(HotkeyEvent::RestoreIndicators));
        assert_eq!(hk.poll(), None);
    }

    #[test]
    fn collision_is_reported_and_suppresses_that_shortcut() {
        let probe = FakeHotkeys::new();
        probe.set_collision(true, false);
        let mut hk = probe.clone();
        let reg = hk.register().unwrap();
        assert!(reg.collision());
        assert!(!reg.restore && reg.emergency_disconnect);

        probe.press_physical(HotkeyEvent::RestoreIndicators);
        probe.press_physical(HotkeyEvent::EmergencyDisconnect);
        assert_eq!(hk.poll(), Some(HotkeyEvent::EmergencyDisconnect));
        assert_eq!(hk.poll(), None);
    }

    #[test]
    fn nothing_fires_before_register_or_after_unregister() {
        let probe = FakeHotkeys::new();
        let mut hk = probe.clone();
        probe.press_physical(HotkeyEvent::EmergencyDisconnect);
        assert_eq!(hk.poll(), None);

        hk.register().unwrap();
        probe.press_physical(HotkeyEvent::EmergencyDisconnect);
        hk.unregister();
        assert!(!probe.is_registered());
        assert_eq!(hk.poll(), None);
    }
}

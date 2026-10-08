//! Decides whether a shortcut was typed on the **physical** keyboard.
//!
//! Remote control injects keystrokes, and the OS reports an injected
//! `Ctrl+Alt+Shift+B` or `+X` exactly like a real one. Restoring the
//! indicators and the emergency disconnect are local controls, so injected
//! input must never trigger them.
//!
//! Windows feeds this from a low-level keyboard hook (which sees the
//! `LLKHF_INJECTED` flag) and asks it to confirm each `WM_HOTKEY` from
//! `RegisterHotKey`. The rules are identical on any platform that can tell
//! injected events apart.

use crate::traits::HotkeyEvent;

/// How long after the physical chord a hotkey notification is still accepted.
pub const CONFIRM_WINDOW_MS: u64 = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Ctrl,
    Alt,
    Shift,
    /// The restore key.
    B,
    /// The emergency-disconnect key.
    X,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyEvent {
    pub key: Key,
    pub down: bool,
    /// Posted by software (SendInput, CGEventPost, remote control), not by the keyboard.
    pub injected: bool,
    /// Monotonic milliseconds.
    pub at_ms: u64,
}

#[derive(Debug, Default)]
pub struct HotkeyGate {
    ctrl: bool,
    alt: bool,
    shift: bool,
    chord: Option<(HotkeyEvent, u64)>,
    rejected: u32,
}

impl HotkeyGate {
    pub fn new() -> Self {
        Self::default()
    }

    /// Every key event the system hook sees, injected or not.
    ///
    /// Injected events are ignored completely: they neither count towards a
    /// chord nor release a physically held modifier.
    pub fn observe(&mut self, event: KeyEvent) {
        if event.injected {
            return;
        }
        match event.key {
            Key::Ctrl => self.ctrl = event.down,
            Key::Alt => self.alt = event.down,
            Key::Shift => self.shift = event.down,
            Key::B | Key::X if event.down && self.ctrl && self.alt && self.shift => {
                let which = if event.key == Key::B {
                    HotkeyEvent::RestoreIndicators
                } else {
                    HotkeyEvent::EmergencyDisconnect
                };
                self.chord = Some((which, event.at_ms));
            }
            _ => {}
        }
    }

    /// The OS says a registered hotkey fired. `true` only if the physical
    /// keyboard just typed that chord; otherwise it is counted and refused.
    pub fn confirm(&mut self, which: HotkeyEvent, now_ms: u64) -> bool {
        match self.chord.take() {
            Some((typed, at))
                if typed == which && now_ms.saturating_sub(at) <= CONFIRM_WINDOW_MS =>
            {
                true
            }
            _ => {
                self.rejected += 1;
                false
            }
        }
    }

    /// Hotkey notifications refused because no physical chord backed them.
    /// A count only.
    pub fn rejected(&self) -> u32 {
        self.rejected
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(key: Key, down: bool, injected: bool, at_ms: u64) -> KeyEvent {
        KeyEvent {
            key,
            down,
            injected,
            at_ms,
        }
    }

    fn press_modifiers(g: &mut HotkeyGate, injected: bool, at: u64) {
        for k in [Key::Ctrl, Key::Alt, Key::Shift] {
            g.observe(ev(k, true, injected, at));
        }
    }

    #[test]
    fn a_physical_chord_is_confirmed_once() {
        for (key, which) in [
            (Key::B, HotkeyEvent::RestoreIndicators),
            (Key::X, HotkeyEvent::EmergencyDisconnect),
        ] {
            let mut g = HotkeyGate::new();
            press_modifiers(&mut g, false, 1000);
            g.observe(ev(key, true, false, 1010));
            assert!(g.confirm(which, 1012));
            assert!(!g.confirm(which, 1013), "a chord confirms one notification");
            assert_eq!(g.rejected(), 1);
        }
    }

    #[test]
    fn an_injected_chord_is_refused() {
        let mut g = HotkeyGate::new();
        press_modifiers(&mut g, true, 1000);
        g.observe(ev(Key::X, true, true, 1010));
        // The OS still raises the hotkey for injected input; the gate says no.
        assert!(!g.confirm(HotkeyEvent::EmergencyDisconnect, 1012));
        assert!(!g.confirm(HotkeyEvent::RestoreIndicators, 1012));
        assert_eq!(g.rejected(), 2);
    }

    #[test]
    fn an_injected_letter_with_physical_modifiers_is_refused() {
        let mut g = HotkeyGate::new();
        press_modifiers(&mut g, false, 1000);
        g.observe(ev(Key::B, true, true, 1010));
        assert!(!g.confirm(HotkeyEvent::RestoreIndicators, 1012));
    }

    #[test]
    fn a_physical_letter_with_injected_modifiers_is_refused() {
        let mut g = HotkeyGate::new();
        press_modifiers(&mut g, true, 1000);
        g.observe(ev(Key::B, true, false, 1010));
        assert!(!g.confirm(HotkeyEvent::RestoreIndicators, 1012));
    }

    #[test]
    fn a_partly_injected_chord_is_refused() {
        for injected_key in [Key::Ctrl, Key::Alt, Key::Shift] {
            let mut g = HotkeyGate::new();
            for k in [Key::Ctrl, Key::Alt, Key::Shift] {
                g.observe(ev(k, true, k == injected_key, 1000));
            }
            g.observe(ev(Key::X, true, false, 1010));
            assert!(
                !g.confirm(HotkeyEvent::EmergencyDisconnect, 1012),
                "{injected_key:?}"
            );
        }
    }

    #[test]
    fn injected_events_cannot_release_a_held_physical_modifier() {
        let mut g = HotkeyGate::new();
        press_modifiers(&mut g, false, 1000);
        // Software "releases" Ctrl; the physical key is still down.
        g.observe(ev(Key::Ctrl, false, true, 1005));
        g.observe(ev(Key::B, true, false, 1010));
        assert!(g.confirm(HotkeyEvent::RestoreIndicators, 1012));
    }

    #[test]
    fn injected_keys_cannot_block_the_physical_chord() {
        // A remote user spams keystrokes to try to jam the emergency shortcut.
        let mut g = HotkeyGate::new();
        for t in 0..50 {
            g.observe(ev(Key::Other, true, true, 900 + t));
            g.observe(ev(Key::X, true, true, 900 + t));
            g.observe(ev(Key::Ctrl, false, true, 900 + t));
        }
        press_modifiers(&mut g, false, 1000);
        g.observe(ev(Key::X, true, false, 1010));
        assert!(g.confirm(HotkeyEvent::EmergencyDisconnect, 1012));
    }

    #[test]
    fn the_wrong_key_or_a_stale_chord_is_refused() {
        let mut g = HotkeyGate::new();
        press_modifiers(&mut g, false, 1000);
        g.observe(ev(Key::B, true, false, 1010));
        assert!(
            !g.confirm(HotkeyEvent::EmergencyDisconnect, 1012),
            "typed B, not X"
        );

        let mut g = HotkeyGate::new();
        press_modifiers(&mut g, false, 1000);
        g.observe(ev(Key::B, true, false, 1010));
        assert!(!g.confirm(HotkeyEvent::RestoreIndicators, 1010 + CONFIRM_WINDOW_MS + 1));

        let mut g = HotkeyGate::new();
        press_modifiers(&mut g, false, 1000);
        g.observe(ev(Key::B, true, false, 1010));
        assert!(g.confirm(HotkeyEvent::RestoreIndicators, 1010 + CONFIRM_WINDOW_MS));
    }

    #[test]
    fn the_letter_alone_or_without_all_modifiers_is_no_chord() {
        let mut g = HotkeyGate::new();
        g.observe(ev(Key::B, true, false, 1000));
        assert!(!g.confirm(HotkeyEvent::RestoreIndicators, 1001));

        let mut g = HotkeyGate::new();
        g.observe(ev(Key::Ctrl, true, false, 1000));
        g.observe(ev(Key::Alt, true, false, 1000));
        g.observe(ev(Key::X, true, false, 1010));
        assert!(!g.confirm(HotkeyEvent::EmergencyDisconnect, 1011));
    }

    #[test]
    fn releasing_a_modifier_ends_the_chord_possibility() {
        let mut g = HotkeyGate::new();
        press_modifiers(&mut g, false, 1000);
        g.observe(ev(Key::Shift, false, false, 1005));
        g.observe(ev(Key::B, true, false, 1010));
        assert!(!g.confirm(HotkeyEvent::RestoreIndicators, 1011));
    }

    #[test]
    fn key_up_of_the_letter_is_not_a_chord() {
        let mut g = HotkeyGate::new();
        press_modifiers(&mut g, false, 1000);
        g.observe(ev(Key::B, false, false, 1010));
        assert!(!g.confirm(HotkeyEvent::RestoreIndicators, 1011));
    }
}

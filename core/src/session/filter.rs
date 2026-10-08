//! Permission filter for viewer messages.
//!
//! Every message from the viewer passes through [`decide`] before it can reach
//! the input injector or the clipboard. Anything the current grant does not
//! allow is dropped and counted. Counters hold numbers only: never message
//! content, keystrokes or clipboard text.

use serde::Deserialize;

use crate::Permissions;
use crate::types::MouseButton;

/// Clipboard text limit, both directions.
pub const MAX_CLIPBOARD_BYTES: usize = 64 * 1024;
/// Anything bigger than this is not a valid message at all.
const MAX_MESSAGE_BYTES: usize = MAX_CLIPBOARD_BYTES + 1024;
const MAX_FIELD_CHARS: usize = 32;
const MAX_WHEEL_DELTA: f64 = 10_000.0;

/// A validated viewer message.
#[derive(Debug, Clone, PartialEq)]
pub enum ViewerMessage {
    Key {
        /// DOM `KeyboardEvent.code`.
        code: String,
        key: String,
        down: bool,
        repeat: bool,
    },
    Button {
        button: MouseButton,
        down: bool,
    },
    Wheel {
        dx: f32,
        dy: f32,
    },
    /// `x` and `y` in 0..=1 of the captured display.
    Move {
        x: f32,
        y: f32,
    },
    Clipboard {
        text: String,
    },
}

/// Why a message was dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropReason {
    /// Not valid JSON, unknown type, or a field out of range.
    Malformed,
    /// Larger than allowed.
    Oversize,
    /// Keyboard, button, wheel or pointer input without the control grant.
    ControlNotGranted,
    /// Clipboard text without the clipboard grant.
    ClipboardNotGranted,
}

#[derive(Deserialize)]
#[serde(tag = "t", rename_all = "lowercase")]
enum Wire {
    Key {
        code: String,
        key: String,
        down: bool,
        #[serde(default)]
        repeat: bool,
    },
    Btn {
        b: u8,
        down: bool,
    },
    Wheel {
        dx: f64,
        dy: f64,
    },
    Move {
        x: f64,
        y: f64,
    },
    Clip {
        text: String,
    },
}

/// Pure decision: parse `raw` and allow it only if `grant` permits it.
pub fn decide(grant: &Permissions, raw: &str) -> Result<ViewerMessage, DropReason> {
    if raw.len() > MAX_MESSAGE_BYTES {
        return Err(DropReason::Oversize);
    }
    let wire: Wire = serde_json::from_str(raw).map_err(|_| DropReason::Malformed)?;
    match wire {
        Wire::Key {
            code,
            key,
            down,
            repeat,
        } => {
            let code_ok = !code.is_empty()
                && code.chars().count() <= MAX_FIELD_CHARS
                && code.chars().all(|c| c.is_ascii_alphanumeric());
            let key_ok = !key.is_empty() && key.chars().count() <= MAX_FIELD_CHARS;
            if !code_ok || !key_ok {
                return Err(DropReason::Malformed);
            }
            require_control(grant)?;
            Ok(ViewerMessage::Key {
                code,
                key,
                down,
                repeat,
            })
        }
        Wire::Btn { b, down } => {
            let button = match b {
                0 => MouseButton::Left,
                1 => MouseButton::Middle,
                2 => MouseButton::Right,
                _ => return Err(DropReason::Malformed),
            };
            require_control(grant)?;
            Ok(ViewerMessage::Button { button, down })
        }
        Wire::Wheel { dx, dy } => {
            let in_range = |v: f64| v.is_finite() && v.abs() <= MAX_WHEEL_DELTA;
            if !in_range(dx) || !in_range(dy) {
                return Err(DropReason::Malformed);
            }
            require_control(grant)?;
            Ok(ViewerMessage::Wheel {
                dx: dx as f32,
                dy: dy as f32,
            })
        }
        Wire::Move { x, y } => {
            if !x.is_finite() || !y.is_finite() {
                return Err(DropReason::Malformed);
            }
            require_control(grant)?;
            Ok(ViewerMessage::Move {
                x: x.clamp(0.0, 1.0) as f32,
                y: y.clamp(0.0, 1.0) as f32,
            })
        }
        Wire::Clip { text } => {
            if text.len() > MAX_CLIPBOARD_BYTES {
                return Err(DropReason::Oversize);
            }
            if !grant.clipboard {
                return Err(DropReason::ClipboardNotGranted);
            }
            Ok(ViewerMessage::Clipboard { text })
        }
    }
}

fn require_control(grant: &Permissions) -> Result<(), DropReason> {
    if grant.control {
        Ok(())
    } else {
        Err(DropReason::ControlNotGranted)
    }
}

/// Host-to-viewer clipboard text is only sent while clipboard is granted, and
/// never above the size limit.
pub fn allow_outgoing_clipboard(grant: &Permissions, text: &str) -> bool {
    grant.clipboard && text.len() <= MAX_CLIPBOARD_BYTES
}

/// Counts only. Never content.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FilterStats {
    pub allowed: u64,
    pub dropped_malformed: u64,
    pub dropped_oversize: u64,
    pub dropped_control: u64,
    pub dropped_clipboard: u64,
}

impl FilterStats {
    pub fn dropped_total(&self) -> u64 {
        self.dropped_malformed
            + self.dropped_oversize
            + self.dropped_control
            + self.dropped_clipboard
    }
}

/// The filter for one session: the current grant plus drop counters.
#[derive(Debug, Clone)]
pub struct PermissionFilter {
    grant: Permissions,
    stats: FilterStats,
}

impl PermissionFilter {
    pub fn new(grant: Permissions) -> Self {
        Self {
            grant,
            stats: FilterStats::default(),
        }
    }

    pub fn grant(&self) -> &Permissions {
        &self.grant
    }

    /// Takes effect for the very next message.
    pub fn set_grant(&mut self, grant: Permissions) {
        self.grant = grant;
    }

    pub fn stats(&self) -> FilterStats {
        self.stats
    }

    /// A binary frame arrived. The protocol is text JSON only, so it is
    /// dropped and counted as malformed.
    pub fn reject_binary(&mut self) {
        self.stats.dropped_malformed += 1;
    }

    /// `Some(message)` if allowed; `None` if dropped (and counted).
    pub fn check(&mut self, raw: &str) -> Option<ViewerMessage> {
        match decide(&self.grant, raw) {
            Ok(message) => {
                self.stats.allowed += 1;
                Some(message)
            }
            Err(reason) => {
                match reason {
                    DropReason::Malformed => self.stats.dropped_malformed += 1,
                    DropReason::Oversize => self.stats.dropped_oversize += 1,
                    DropReason::ControlNotGranted => self.stats.dropped_control += 1,
                    DropReason::ClipboardNotGranted => self.stats.dropped_clipboard += 1,
                }
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VIEW_ONLY: Permissions = Permissions::NONE;
    const CONTROL: Permissions = Permissions {
        control: true,
        ..Permissions::NONE
    };
    const CLIPBOARD: Permissions = Permissions {
        clipboard: true,
        ..Permissions::NONE
    };
    const AUDIO_AND_MIC: Permissions = Permissions {
        system_audio: true,
        microphone: true,
        ..Permissions::NONE
    };

    const KEY: &str = r#"{"t":"key","code":"KeyA","key":"a","down":true,"repeat":false}"#;
    const BTN: &str = r#"{"t":"btn","b":0,"down":true}"#;
    const WHEEL: &str = r#"{"t":"wheel","dx":0,"dy":-120}"#;
    const MOVE: &str = r#"{"t":"move","x":0.5,"y":0.25}"#;
    const CLIP: &str = r#"{"t":"clip","text":"hello"}"#;

    #[test]
    fn view_only_drops_all_input_and_clipboard() {
        for raw in [KEY, BTN, WHEEL, MOVE] {
            assert_eq!(
                decide(&VIEW_ONLY, raw),
                Err(DropReason::ControlNotGranted),
                "{raw}"
            );
        }
        assert_eq!(
            decide(&VIEW_ONLY, CLIP),
            Err(DropReason::ClipboardNotGranted)
        );
    }

    #[test]
    fn control_allows_input_but_not_clipboard() {
        assert!(decide(&CONTROL, KEY).is_ok());
        assert!(decide(&CONTROL, BTN).is_ok());
        assert!(decide(&CONTROL, WHEEL).is_ok());
        assert!(decide(&CONTROL, MOVE).is_ok());
        assert_eq!(decide(&CONTROL, CLIP), Err(DropReason::ClipboardNotGranted));
    }

    #[test]
    fn clipboard_allows_text_but_not_input() {
        assert_eq!(
            decide(&CLIPBOARD, CLIP),
            Ok(ViewerMessage::Clipboard {
                text: "hello".into()
            })
        );
        assert_eq!(decide(&CLIPBOARD, KEY), Err(DropReason::ControlNotGranted));
    }

    #[test]
    fn audio_and_microphone_grants_allow_no_input_at_all() {
        for raw in [KEY, BTN, WHEEL, MOVE, CLIP] {
            assert!(decide(&AUDIO_AND_MIC, raw).is_err(), "{raw}");
        }
    }

    #[test]
    fn parsed_values_are_exact() {
        assert_eq!(
            decide(&CONTROL, KEY),
            Ok(ViewerMessage::Key {
                code: "KeyA".into(),
                key: "a".into(),
                down: true,
                repeat: false
            })
        );
        assert_eq!(
            decide(&CONTROL, r#"{"t":"btn","b":2,"down":false}"#),
            Ok(ViewerMessage::Button {
                button: MouseButton::Right,
                down: false
            })
        );
        assert_eq!(
            decide(&CONTROL, WHEEL),
            Ok(ViewerMessage::Wheel {
                dx: 0.0,
                dy: -120.0
            })
        );
    }

    #[test]
    fn repeat_defaults_to_false() {
        let raw = r#"{"t":"key","code":"Enter","key":"Enter","down":false}"#;
        assert!(matches!(
            decide(&CONTROL, raw),
            Ok(ViewerMessage::Key { repeat: false, .. })
        ));
    }

    #[test]
    fn pointer_is_clamped_into_the_screen() {
        assert_eq!(
            decide(&CONTROL, r#"{"t":"move","x":1.7,"y":-3}"#),
            Ok(ViewerMessage::Move { x: 1.0, y: 0.0 })
        );
    }

    #[test]
    fn malformed_messages_are_dropped_even_with_every_grant() {
        let all = Permissions {
            control: true,
            system_audio: true,
            microphone: true,
            clipboard: true,
        };
        let long_code = format!(
            r#"{{"t":"key","code":"{}","key":"a","down":true}}"#,
            "A".repeat(33)
        );
        for raw in [
            "",
            "not json",
            "[]",
            r#"{"t":"shell","cmd":"calc"}"#,
            r#"{"t":"key"}"#,
            r#"{"t":"key","code":"","key":"a","down":true}"#,
            r#"{"t":"key","code":"Key A","key":"a","down":true}"#,
            r#"{"t":"key","code":"KeyA","key":"","down":true}"#,
            r#"{"t":"btn","b":3,"down":true}"#,
            r#"{"t":"btn","b":-1,"down":true}"#,
            r#"{"t":"wheel","dx":1e9,"dy":0}"#,
            r#"{"t":"wheel","dx":"x","dy":0}"#,
            r#"{"t":"move","x":null,"y":0}"#,
            r#"{"t":"clip"}"#,
            long_code.as_str(),
        ] {
            assert_eq!(decide(&all, raw), Err(DropReason::Malformed), "{raw}");
        }
    }

    #[test]
    fn clipboard_size_limit() {
        let all = Permissions {
            clipboard: true,
            ..Permissions::NONE
        };
        let at_limit = format!(
            r#"{{"t":"clip","text":"{}"}}"#,
            "a".repeat(MAX_CLIPBOARD_BYTES)
        );
        assert!(decide(&all, &at_limit).is_ok());
        let over = format!(
            r#"{{"t":"clip","text":"{}"}}"#,
            "a".repeat(MAX_CLIPBOARD_BYTES + 1)
        );
        assert_eq!(decide(&all, &over), Err(DropReason::Oversize));
        let huge = "x".repeat(MAX_MESSAGE_BYTES + 1);
        assert_eq!(decide(&all, &huge), Err(DropReason::Oversize));
    }

    #[test]
    fn outgoing_clipboard_needs_the_grant_and_respects_the_limit() {
        assert!(!allow_outgoing_clipboard(&VIEW_ONLY, "x"));
        assert!(!allow_outgoing_clipboard(&CONTROL, "x"));
        assert!(allow_outgoing_clipboard(&CLIPBOARD, "x"));
        assert!(!allow_outgoing_clipboard(
            &CLIPBOARD,
            &"a".repeat(MAX_CLIPBOARD_BYTES + 1)
        ));
    }

    #[test]
    fn filter_counts_drops_by_kind_and_never_keeps_content() {
        let mut f = PermissionFilter::new(VIEW_ONLY);
        assert_eq!(f.check(KEY), None);
        assert_eq!(f.check(MOVE), None);
        assert_eq!(f.check(CLIP), None);
        assert_eq!(f.check("garbage"), None);
        assert_eq!(
            f.stats(),
            FilterStats {
                allowed: 0,
                dropped_malformed: 1,
                dropped_oversize: 0,
                dropped_control: 2,
                dropped_clipboard: 1,
            }
        );
        assert_eq!(f.stats().dropped_total(), 4);
    }

    #[test]
    fn grant_changes_apply_to_the_next_message() {
        let mut f = PermissionFilter::new(VIEW_ONLY);
        assert_eq!(f.check(KEY), None);
        f.set_grant(CONTROL);
        assert!(f.check(KEY).is_some());
        // The host revokes control: keys stop at once.
        f.set_grant(VIEW_ONLY);
        assert_eq!(f.check(KEY), None);
        assert_eq!(f.stats().allowed, 1);
        assert_eq!(f.stats().dropped_control, 2);
        assert_eq!(f.grant(), &VIEW_ONLY);
    }
}

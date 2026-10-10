//! What the local consent dialogs say, and what each button means.
//!
//! This is plain logic with no Win32 calls, so it is tested on every OS. The
//! dialogs themselves live in `consent.rs` (Yes/No message boxes) and
//! `approval.rs` (the approval window with one checkbox per opt-in).
//!
//! Viewing is always included. Control, system audio, microphone and clipboard
//! are separate opt-ins: each is off unless the person ticks it, and a box is
//! only available when the viewer asked for that permission. Escalation requests
//! are still declined, because they are not offered in this version.

use rb_core::Permissions;
use rb_core::traits::{ConsentAnswer, ConsentPrompt, Notice};

/// Length of an attended approval, and of a renewal, offered by this version.
pub const APPROVAL_MINUTES: u32 = 30;

/// The opt-ins the approval window offers, in the order shown:
/// control, system audio, microphone, clipboard.
pub const OPT_IN_LABELS: [&str; 4] = [
    "Mouse and keyboard control",
    "System audio",
    "Microphone",
    "Clipboard text",
];

/// One line under each opt-in, saying what it lets the viewer do.
pub const OPT_IN_HINTS: [&str; 4] = [
    "The viewer can move the mouse and type on this PC.",
    "Sound playing on this PC, including meetings.",
    "This PC's microphone. Separate opt-in, off by default.",
    "Text only, size-limited. No files.",
];

/// Asked before unattended access is switched on from the tray menu.
pub const UNATTENDED_ON_PROMPT: &str = "Allow unattended access on this PC?\n\n\
    Someone the owner allowed on the website could then connect to this PC without anyone here approving it. \
    This is one half of the switch: the owner must also turn it on for this device on the website.\n\n\
    The tray icon always stays visible during an unattended session, and Ctrl+Alt+Shift+X disconnects at any time. \
    You can turn this off again from the tray icon.";

/// One dialog: its text and whether it asks a question (Yes/No) or only
/// informs (OK).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dialog {
    pub body: String,
    pub question: bool,
}

/// "Allowed: view, mouse and keyboard, ..." for the permissions that are on.
pub fn permission_words(p: &Permissions) -> String {
    let mut parts = vec!["view"];
    if p.control {
        parts.push("mouse and keyboard");
    }
    if p.system_audio {
        parts.push("system audio");
    }
    if p.microphone {
        parts.push("microphone");
    }
    if p.clipboard {
        parts.push("clipboard text");
    }
    format!("Allowed: {}", parts.join(", "))
}

/// "123456789012" as "1234 5678 9012", the way the website shows it.
pub fn support_code_groups(code: &str) -> String {
    code.as_bytes()
        .chunks(4)
        .map(|c| String::from_utf8_lossy(c).into_owned())
        .collect::<Vec<_>>()
        .join(" ")
}

/// The opening line of the approval window.
pub fn approval_intro(requester_name: &str, requester_email: &str, verified: bool) -> String {
    let verified = if verified { "verified" } else { "not verified" };
    format!(
        "{requester_name} ({requester_email}, {verified}) asks to view this screen.\n\n\
         Viewing is always included. Tick only what you want to allow, for \
         {APPROVAL_MINUTES} minutes. Anything left unticked stays off."
    )
}

/// Which opt-in boxes can be ticked: only the permissions the viewer asked for.
pub fn opt_in_enabled(requested: &Permissions) -> [bool; 4] {
    [
        requested.control,
        requested.system_audio,
        requested.microphone,
        requested.clipboard,
    ]
}

/// The grant for the boxes that are ticked. A box can only grant something the
/// viewer requested, so a tick on an unrequested permission does nothing.
pub fn grant_from_checks(requested: &Permissions, checked: [bool; 4]) -> Permissions {
    let enabled = opt_in_enabled(requested);
    Permissions {
        control: enabled[0] && checked[0],
        system_audio: enabled[1] && checked[1],
        microphone: enabled[2] && checked[2],
        clipboard: enabled[3] && checked[3],
    }
}

/// The answer for Allow in the approval window.
pub fn approval_answer(requested: &Permissions, checked: [bool; 4]) -> ConsentAnswer {
    ConsentAnswer::Approve {
        grant: grant_from_checks(requested, checked),
        approved_minutes: APPROVAL_MINUTES,
    }
}

/// The dialog to show for `prompt` as a Yes/No or OK message box. Attended
/// requests are shown in the approval window instead, so the text here is the
/// same summary without the checkboxes.
pub fn dialog_for(prompt: &ConsentPrompt) -> Dialog {
    match prompt {
        ConsentPrompt::AttendedRequest {
            requester_name,
            requester_email,
            requester_verified,
            requested,
        } => Dialog {
            body: format!(
                "{}\n\nRequested: {}",
                approval_intro(requester_name, requester_email, *requester_verified),
                permission_words(requested)
            ),
            question: true,
        },
        ConsentPrompt::RenewConsent { requester_name } => Dialog {
            body: format!(
                "Keep sharing your screen with {requester_name} for another {APPROVAL_MINUTES} minutes?"
            ),
            question: true,
        },
        ConsentPrompt::PermissionEscalation {
            requester_name,
            added,
        } => Dialog {
            body: format!(
                "{requester_name} asked for more access ({}). This version cannot grant that, so the session stays view only.",
                permission_words(added)
            ),
            question: false,
        },
        ConsentPrompt::Notice(notice) => Dialog {
            body: notice_text(notice),
            question: false,
        },
    }
}

/// The answer for a Yes/No or OK message box. Attended requests never come
/// through here (they use the approval window), so closing one is a denial.
pub fn answer_for(prompt: &ConsentPrompt, yes: bool) -> ConsentAnswer {
    match prompt {
        ConsentPrompt::AttendedRequest { .. } => ConsentAnswer::Deny,
        ConsentPrompt::RenewConsent { .. } => {
            if yes {
                ConsentAnswer::Renew {
                    minutes: APPROVAL_MINUTES,
                }
            } else {
                ConsentAnswer::Dismissed
            }
        }
        ConsentPrompt::PermissionEscalation { .. } | ConsentPrompt::Notice(_) => {
            ConsentAnswer::Dismissed
        }
    }
}

fn notice_text(notice: &Notice) -> String {
    match notice {
        Notice::DeviceRemoved => {
            "This device was removed from your account. RemoteBridge has stopped and its key was deleted."
                .to_owned()
        }
        Notice::FirstHide { restore_shortcut } => format!(
            "The indicators are hidden. They come back when the timer ends, when a new session starts, or with {restore_shortcut}."
        ),
        Notice::ShortcutCollision { shortcut } => format!(
            "Could not register {shortcut}. Use the tray icon menu instead."
        ),
        Notice::SupportCode { code, minutes_left } => format!(
            "Your support code is {}.\n\nRead it to the person helping you. It works once and expires in {minutes_left} minutes. They can see your screen only after you approve their request here.",
            support_code_groups(code)
        ),
        Notice::SupportCodeUnavailable { reason } => format!(
            "Could not get a support code: {reason}. Check the internet connection and try again from the tray icon."
        ),
        Notice::PermissionsUnavailable { what } => format!(
            "RemoteBridge needs {what} permission for this to work. Open the system settings to allow it."
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn requested_everything() -> Permissions {
        Permissions {
            control: true,
            system_audio: true,
            microphone: true,
            clipboard: true,
        }
    }

    fn attended(requested: Permissions) -> ConsentPrompt {
        ConsentPrompt::AttendedRequest {
            requester_name: "Haile".into(),
            requester_email: "someone@example.com".into(),
            requester_verified: true,
            requested,
        }
    }

    #[test]
    fn permission_words_list_exactly_what_is_granted() {
        assert_eq!(permission_words(&Permissions::NONE), "Allowed: view");
        assert_eq!(
            permission_words(&requested_everything()),
            "Allowed: view, mouse and keyboard, system audio, microphone, clipboard text"
        );
    }

    #[test]
    fn the_approval_intro_names_the_requester_and_says_view_is_included() {
        let text = approval_intro("Haile", "someone@example.com", true);
        assert!(text.starts_with("Haile (someone@example.com, verified) asks"));
        assert!(text.contains("Viewing is always included"));
        assert!(text.contains("for 30 minutes"));
        assert!(approval_intro("Alex", "alex@example.com", false).contains("not verified"));
    }

    #[test]
    fn only_requested_permissions_have_a_box() {
        let requested = Permissions {
            control: true,
            clipboard: true,
            ..Permissions::NONE
        };
        assert_eq!(opt_in_enabled(&requested), [true, false, false, true]);
        assert_eq!(opt_in_enabled(&Permissions::NONE), [false; 4]);
    }

    #[test]
    fn nothing_is_granted_unless_ticked() {
        assert_eq!(
            grant_from_checks(&requested_everything(), [false; 4]),
            Permissions::NONE
        );
    }

    #[test]
    fn ticks_grant_exactly_the_ticked_requested_permissions() {
        let grant = grant_from_checks(&requested_everything(), [true, false, true, false]);
        assert_eq!(
            grant,
            Permissions {
                control: true,
                microphone: true,
                ..Permissions::NONE
            }
        );
    }

    #[test]
    fn a_tick_on_an_unrequested_permission_grants_nothing() {
        let requested = Permissions {
            control: true,
            ..Permissions::NONE
        };
        let grant = grant_from_checks(&requested, [false, true, true, true]);
        assert_eq!(grant, Permissions::NONE);
    }

    #[test]
    fn allow_approves_the_ticked_grant_for_the_approval_window() {
        assert_eq!(
            approval_answer(&requested_everything(), [false, true, false, false]),
            ConsentAnswer::Approve {
                grant: Permissions {
                    system_audio: true,
                    ..Permissions::NONE
                },
                approved_minutes: APPROVAL_MINUTES,
            }
        );
    }

    #[test]
    fn closing_an_attended_request_in_a_message_box_is_a_denial() {
        assert_eq!(
            answer_for(&attended(requested_everything()), true),
            ConsentAnswer::Deny
        );
        assert_eq!(
            answer_for(&attended(Permissions::NONE), false),
            ConsentAnswer::Deny
        );
    }

    #[test]
    fn the_attended_summary_lists_what_was_asked() {
        let dialog = dialog_for(&attended(requested_everything()));
        assert!(dialog.question);
        assert!(
            dialog
                .body
                .contains("Requested: Allowed: view, mouse and keyboard")
        );
    }

    #[test]
    fn renewal_yes_renews_and_no_is_treated_as_no() {
        let prompt = ConsentPrompt::RenewConsent {
            requester_name: "Haile".into(),
        };
        assert_eq!(
            answer_for(&prompt, true),
            ConsentAnswer::Renew {
                minutes: APPROVAL_MINUTES
            }
        );
        assert_eq!(answer_for(&prompt, false), ConsentAnswer::Dismissed);
    }

    #[test]
    fn escalation_is_never_granted_in_this_version() {
        let prompt = ConsentPrompt::PermissionEscalation {
            requester_name: "Haile".into(),
            added: Permissions {
                control: true,
                ..Permissions::NONE
            },
        };
        assert_eq!(answer_for(&prompt, true), ConsentAnswer::Dismissed);
        assert_eq!(answer_for(&prompt, false), ConsentAnswer::Dismissed);
        assert!(!dialog_for(&prompt).question, "one OK button, not a choice");
    }

    #[test]
    fn notices_are_information_only() {
        let prompt = ConsentPrompt::Notice(Notice::DeviceRemoved);
        let dialog = dialog_for(&prompt);
        assert!(!dialog.question);
        assert_eq!(answer_for(&prompt, true), ConsentAnswer::Dismissed);
    }

    #[test]
    fn first_hide_notice_names_the_restore_shortcut() {
        let dialog = dialog_for(&ConsentPrompt::Notice(Notice::FirstHide {
            restore_shortcut: "Ctrl+Alt+Shift+B".into(),
        }));
        assert!(dialog.body.contains("Ctrl+Alt+Shift+B"));
    }

    #[test]
    fn support_codes_are_shown_in_groups_of_four() {
        assert_eq!(support_code_groups("123456789012"), "1234 5678 9012");
        assert_eq!(support_code_groups("12345"), "1234 5");
        assert_eq!(support_code_groups(""), "");
    }

    #[test]
    fn the_support_code_notice_shows_the_grouped_code_and_the_time_left() {
        let dialog = dialog_for(&ConsentPrompt::Notice(Notice::SupportCode {
            code: "844661833066".into(),
            minutes_left: 10,
        }));
        assert!(!dialog.question);
        assert!(dialog.body.contains("8446 6183 3066"));
        assert!(dialog.body.contains("10 minutes"));
        let failed = dialog_for(&ConsentPrompt::Notice(Notice::SupportCodeUnavailable {
            reason: "timed out".into(),
        }));
        assert!(failed.body.contains("timed out"));
    }

    #[test]
    fn the_unattended_prompt_names_both_halves_and_the_disconnect_shortcut() {
        assert!(UNATTENDED_ON_PROMPT.contains("on the website"));
        assert!(UNATTENDED_ON_PROMPT.contains("Ctrl+Alt+Shift+X"));
        assert!(UNATTENDED_ON_PROMPT.contains("tray icon always stays visible"));
    }

    #[test]
    fn there_is_one_label_and_hint_per_opt_in() {
        assert_eq!(OPT_IN_LABELS.len(), 4);
        assert_eq!(OPT_IN_HINTS.len(), 4);
    }
}

//! What the local consent dialogs say, and what each button means.
//!
//! This is plain logic with no Win32 calls, so it is tested on every OS. The
//! dialogs themselves live in `consent.rs`.
//!
//! Version one approves **view only**. Control, system audio, microphone and
//! clipboard need their own checkboxes before they can be granted, so until
//! those exist, a request for them is answered with view only and the dialog
//! says so. Escalation requests are declined for the same reason.

use rb_core::Permissions;
use rb_core::traits::{ConsentAnswer, ConsentPrompt, Notice};

/// Length of an attended approval, and of a renewal, offered by this version.
pub const APPROVAL_MINUTES: u32 = 30;

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

/// The dialog to show for `prompt`.
pub fn dialog_for(prompt: &ConsentPrompt) -> Dialog {
    match prompt {
        ConsentPrompt::AttendedRequest {
            requester_name,
            requester_email,
            requester_verified,
            requested,
        } => {
            let verified = if *requester_verified {
                "verified"
            } else {
                "not verified"
            };
            Dialog {
                body: format!(
                    "{requester_name} ({requester_email}, {verified}) asks to view this screen.\n\n\
                     Requested: {}\n\n\
                     This version allows view only for {APPROVAL_MINUTES} minutes. \
                     Control, audio, microphone and clipboard stay off.\n\n\
                     Allow view only?",
                    permission_words(requested)
                ),
                question: true,
            }
        }
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

/// The answer for the button pressed. `yes` is true for Yes, false for No or
/// for a dialog closed without a choice. OK-only dialogs always report
/// `Dismissed`, whatever the button.
pub fn answer_for(prompt: &ConsentPrompt, yes: bool) -> ConsentAnswer {
    match prompt {
        ConsentPrompt::AttendedRequest { .. } => {
            if yes {
                ConsentAnswer::Approve {
                    grant: Permissions::NONE,
                    approved_minutes: APPROVAL_MINUTES,
                }
            } else {
                ConsentAnswer::Deny
            }
        }
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
    fn attended_dialog_names_the_requester_and_what_was_asked() {
        let dialog = dialog_for(&attended(requested_everything()));
        assert!(dialog.question);
        assert!(
            dialog
                .body
                .starts_with("Haile (someone@example.com, verified) asks")
        );
        assert!(
            dialog
                .body
                .contains("Requested: Allowed: view, mouse and keyboard")
        );
        assert!(dialog.body.contains("view only for 30 minutes"));
    }

    #[test]
    fn unverified_requesters_are_labelled_as_such() {
        let prompt = ConsentPrompt::AttendedRequest {
            requester_name: "Alex".into(),
            requester_email: "alex@example.com".into(),
            requester_verified: false,
            requested: Permissions::NONE,
        };
        assert!(dialog_for(&prompt).body.contains("not verified"));
    }

    #[test]
    fn approving_grants_view_only_even_when_more_was_requested() {
        assert_eq!(
            answer_for(&attended(requested_everything()), true),
            ConsentAnswer::Approve {
                grant: Permissions::NONE,
                approved_minutes: APPROVAL_MINUTES,
            }
        );
    }

    #[test]
    fn denying_or_closing_an_attended_request_is_a_denial() {
        assert_eq!(
            answer_for(&attended(Permissions::NONE), false),
            ConsentAnswer::Deny
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
}

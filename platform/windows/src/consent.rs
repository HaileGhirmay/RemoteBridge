//! Local consent dialogs on Windows.
//!
//! Each prompt is a native message box on its own thread, so `show` returns at
//! once and the host loop keeps running. The button pressed is sent back on a
//! channel and read by `poll`. Withdrawing a prompt closes its box; an answer
//! that still arrives afterwards is dropped here, and the session manager
//! ignores answers to withdrawn prompts anyway.
//!
//! The dialogs are plain message boxes on purpose: they are on the local
//! desktop only, and nothing from the remote viewer can reach them.

use std::collections::HashSet;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use rb_core::traits::{ConsentAnswer, ConsentPrompt, ConsentUi, PromptId};
use rb_core::{PlatformError, PlatformResult};
use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowW, IDYES, MB_ICONINFORMATION, MB_ICONQUESTION, MB_OK, MB_SETFOREGROUND, MB_TOPMOST,
    MB_YESNO, MessageBoxW, PostMessageW, WM_CLOSE,
};
use windows::core::{HSTRING, PCWSTR};

use crate::prompt::{answer_for, dialog_for};

pub struct WindowsConsent {
    next_id: u64,
    answers_tx: Sender<(PromptId, ConsentAnswer)>,
    answers_rx: Receiver<(PromptId, ConsentAnswer)>,
    withdrawn: HashSet<PromptId>,
}

impl WindowsConsent {
    pub fn new() -> Self {
        let (answers_tx, answers_rx) = mpsc::channel();
        Self {
            next_id: 1,
            answers_tx,
            answers_rx,
            withdrawn: HashSet::new(),
        }
    }

    /// The window caption carries the prompt number so it can be found and
    /// closed by `withdraw`.
    fn caption(id: PromptId) -> String {
        format!("RemoteBridge request {}", id.0)
    }
}

impl Default for WindowsConsent {
    fn default() -> Self {
        Self::new()
    }
}

impl ConsentUi for WindowsConsent {
    fn show(&mut self, prompt: ConsentPrompt) -> PlatformResult<PromptId> {
        let id = PromptId(self.next_id);
        self.next_id += 1;
        let caption = Self::caption(id);
        let answers = self.answers_tx.clone();

        // An attended request gets the approval window with one checkbox per
        // opt-in. Everything else is a Yes/No or OK message box.
        if let ConsentPrompt::AttendedRequest {
            requester_name,
            requester_email,
            requester_verified,
            requested,
        } = prompt
        {
            crate::approval::show(
                id,
                caption,
                requester_name,
                requester_email,
                requester_verified,
                requested,
                answers,
            )?;
            return Ok(id);
        }

        let dialog = dialog_for(&prompt);
        thread::Builder::new()
            .name(format!("consent-{}", id.0))
            .spawn(move || {
                let style = if dialog.question {
                    MB_YESNO | MB_ICONQUESTION
                } else {
                    MB_OK | MB_ICONINFORMATION
                };
                // SAFETY: both strings outlive the call; no owner window is given,
                // so the box is not tied to any of our windows.
                let result = unsafe {
                    MessageBoxW(
                        None,
                        &HSTRING::from(dialog.body.as_str()),
                        &HSTRING::from(caption.as_str()),
                        style | MB_TOPMOST | MB_SETFOREGROUND,
                    )
                };
                let yes = result == IDYES;
                // If the receiver is gone the host is shutting down; nothing to do.
                let _ = answers.send((id, answer_for(&prompt, yes)));
            })
            .map_err(|e| PlatformError::Backend(format!("consent thread: {e}")))?;

        Ok(id)
    }

    fn withdraw(&mut self, id: PromptId) {
        self.withdrawn.insert(id);
        let caption = HSTRING::from(Self::caption(id).as_str());
        // SAFETY: a null class name matches any window; the caption is unique to
        // this prompt. Closing a message box sends it the cancel result.
        unsafe {
            if let Ok(hwnd) = FindWindowW(PCWSTR::null(), &caption) {
                let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
            }
        }
    }

    fn poll(&mut self) -> Option<(PromptId, ConsentAnswer)> {
        loop {
            let (id, answer) = self.answers_rx.try_recv().ok()?;
            if self.withdrawn.remove(&id) {
                continue;
            }
            return Some((id, answer));
        }
    }
}

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use crate::PlatformResult;
use crate::traits::{
    Banner, BannerAction, BannerContent, ConsentAnswer, ConsentPrompt, ConsentUi, PromptId, Tray,
    TrayAction, TrayModel,
};

#[derive(Debug, Default)]
struct ConsentState {
    next_id: u64,
    shown: Vec<(PromptId, ConsentPrompt)>,
    withdrawn: Vec<PromptId>,
    /// Answers the "user" will give, one per prompt, in order of `show`.
    scripted: VecDeque<ConsentAnswer>,
    ready: VecDeque<(PromptId, ConsentAnswer)>,
}

/// Consent prompts answered from a script. If no answer is scripted for a
/// prompt, it stays unanswered (the user is "ignoring" it).
#[derive(Debug, Clone, Default)]
pub struct FakeConsentUi {
    state: Arc<Mutex<ConsentState>>,
}

impl FakeConsentUi {
    pub fn new() -> Self {
        Self::default()
    }

    /// Queue the answer for the next prompt that gets shown.
    pub fn script_answer(&self, answer: ConsentAnswer) {
        self.state.lock().unwrap().scripted.push_back(answer);
    }

    /// Answer an already-shown prompt now (a late click).
    pub fn answer(&self, id: PromptId, answer: ConsentAnswer) {
        self.state.lock().unwrap().ready.push_back((id, answer));
    }

    pub fn shown(&self) -> Vec<(PromptId, ConsentPrompt)> {
        self.state.lock().unwrap().shown.clone()
    }

    pub fn withdrawn(&self) -> Vec<PromptId> {
        self.state.lock().unwrap().withdrawn.clone()
    }
}

impl ConsentUi for FakeConsentUi {
    fn show(&mut self, prompt: ConsentPrompt) -> PlatformResult<PromptId> {
        let mut s = self.state.lock().unwrap();
        s.next_id += 1;
        let id = PromptId(s.next_id);
        s.shown.push((id, prompt));
        if let Some(answer) = s.scripted.pop_front() {
            s.ready.push_back((id, answer));
        }
        Ok(id)
    }

    fn withdraw(&mut self, id: PromptId) {
        let mut s = self.state.lock().unwrap();
        s.withdrawn.push(id);
        s.ready.retain(|(ready_id, _)| *ready_id != id);
    }

    fn poll(&mut self) -> Option<(PromptId, ConsentAnswer)> {
        self.state.lock().unwrap().ready.pop_front()
    }
}

#[derive(Debug, Default)]
struct BannerState {
    visible: bool,
    content: Option<BannerContent>,
    actions: VecDeque<BannerAction>,
}

#[derive(Debug, Clone, Default)]
pub struct FakeBanner {
    state: Arc<Mutex<BannerState>>,
}

impl FakeBanner {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn content(&self) -> Option<BannerContent> {
        self.state.lock().unwrap().content.clone()
    }

    /// The user clicked a button on the banner.
    pub fn click(&self, action: BannerAction) {
        self.state.lock().unwrap().actions.push_back(action);
    }
}

impl Banner for FakeBanner {
    fn show(&mut self, content: &BannerContent) -> PlatformResult<()> {
        let mut s = self.state.lock().unwrap();
        s.visible = true;
        s.content = Some(content.clone());
        Ok(())
    }

    fn hide(&mut self) {
        self.state.lock().unwrap().visible = false;
    }

    fn is_visible(&self) -> bool {
        self.state.lock().unwrap().visible
    }

    fn poll_action(&mut self) -> Option<BannerAction> {
        self.state.lock().unwrap().actions.pop_front()
    }
}

#[derive(Debug, Default)]
struct TrayState {
    model: TrayModel,
    updates: u32,
    actions: VecDeque<TrayAction>,
}

#[derive(Debug, Clone, Default)]
pub struct FakeTray {
    state: Arc<Mutex<TrayState>>,
}

impl FakeTray {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn model(&self) -> TrayModel {
        self.state.lock().unwrap().model.clone()
    }

    pub fn update_count(&self) -> u32 {
        self.state.lock().unwrap().updates
    }

    /// The user picked a menu item.
    pub fn click(&self, action: TrayAction) {
        self.state.lock().unwrap().actions.push_back(action);
    }
}

impl Tray for FakeTray {
    fn update(&mut self, model: &TrayModel) -> PlatformResult<()> {
        let mut s = self.state.lock().unwrap();
        s.model = model.clone();
        s.updates += 1;
        Ok(())
    }

    fn poll_action(&mut self) -> Option<TrayAction> {
        self.state.lock().unwrap().actions.pop_front()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Permissions;
    use crate::traits::{HideChoice, Notice};

    fn attended() -> ConsentPrompt {
        ConsentPrompt::AttendedRequest {
            requester_name: "Sam".into(),
            requester_email: "sam@example.com".into(),
            requester_verified: true,
            requested: Permissions {
                control: true,
                ..Permissions::NONE
            },
        }
    }

    #[test]
    fn scripted_answers_follow_show_order_and_unscripted_prompts_stay_open() {
        let probe = FakeConsentUi::new();
        let mut ui: Box<dyn ConsentUi> = Box::new(probe.clone());
        probe.script_answer(ConsentAnswer::Deny);

        let first = ui.show(attended()).unwrap();
        let second = ui
            .show(ConsentPrompt::RenewConsent {
                requester_name: "Sam".into(),
            })
            .unwrap();
        assert_ne!(first, second);
        assert_eq!(ui.poll(), Some((first, ConsentAnswer::Deny)));
        assert_eq!(ui.poll(), None, "ignored prompt must stay unanswered");
        assert_eq!(probe.shown().len(), 2);
    }

    #[test]
    fn withdrawn_prompt_cannot_be_answered() {
        let probe = FakeConsentUi::new();
        let mut ui = probe.clone();
        let id = ui
            .show(ConsentPrompt::Notice(Notice::DeviceRemoved))
            .unwrap();
        probe.answer(id, ConsentAnswer::Dismissed);
        ui.withdraw(id);
        assert_eq!(ui.poll(), None);
        assert_eq!(probe.withdrawn(), vec![id]);
    }

    #[test]
    fn banner_and_tray_track_state_and_clicks() {
        let banner_probe = FakeBanner::new();
        let mut banner: Box<dyn Banner> = Box::new(banner_probe.clone());
        let content = BannerContent {
            requester_name: "Sam".into(),
            granted: Permissions::NONE,
        };
        banner.show(&content).unwrap();
        assert!(banner.is_visible());
        assert_eq!(banner_probe.content(), Some(content));
        banner_probe.click(BannerAction::Hide(HideChoice::Minutes(60)));
        assert_eq!(
            banner.poll_action(),
            Some(BannerAction::Hide(HideChoice::Minutes(60)))
        );
        banner.hide();
        assert!(!banner.is_visible());

        let tray_probe = FakeTray::new();
        let mut tray: Box<dyn Tray> = Box::new(tray_probe.clone());
        let model = TrayModel {
            visible: true,
            session_live: true,
            connected_name: Some("Sam".into()),
            consent_seconds_left: Some(1800),
            unattended_opt_in: false,
        };
        tray.update(&model).unwrap();
        assert_eq!(tray_probe.model(), model);
        tray_probe.click(TrayAction::DisconnectNow);
        assert_eq!(tray.poll_action(), Some(TrayAction::DisconnectNow));
        assert_eq!(tray.poll_action(), None);
    }
}

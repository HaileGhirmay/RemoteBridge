//! One input injector and one clipboard serve every session.
//!
//! The machine has a single keyboard, mouse and clipboard, but each media
//! session wants its own `Box<dyn InputInjector>` / `Box<dyn Clipboard>`.
//! These wrappers share the real thing behind a lock. Policy is never decided
//! here: the permission filter in each session has already said yes.

use std::sync::{Arc, Mutex};

use rb_core::PlatformResult;
use rb_core::traits::{Clipboard, InputInjector};
use rb_core::types::MouseButton;

#[derive(Clone)]
pub struct SharedInjector(Arc<Mutex<Box<dyn InputInjector>>>);

impl SharedInjector {
    pub fn new(inner: Box<dyn InputInjector>) -> Self {
        Self(Arc::new(Mutex::new(inner)))
    }

    fn with<T>(&self, f: impl FnOnce(&mut dyn InputInjector) -> T) -> T {
        let mut guard = self.0.lock().unwrap();
        f(guard.as_mut())
    }
}

impl InputInjector for SharedInjector {
    fn key(&mut self, code: &str, down: bool) -> PlatformResult<()> {
        self.with(|i| i.key(code, down))
    }

    fn button(&mut self, button: MouseButton, down: bool) -> PlatformResult<()> {
        self.with(|i| i.button(button, down))
    }

    fn wheel(&mut self, dx: f32, dy: f32) -> PlatformResult<()> {
        self.with(|i| i.wheel(dx, dy))
    }

    fn move_pointer(&mut self, x: f32, y: f32) -> PlatformResult<()> {
        self.with(|i| i.move_pointer(x, y))
    }

    fn release_all(&mut self) -> PlatformResult<()> {
        self.with(|i| i.release_all())
    }
}

#[derive(Clone)]
pub struct SharedClipboard(Arc<Mutex<Box<dyn Clipboard>>>);

impl SharedClipboard {
    pub fn new(inner: Box<dyn Clipboard>) -> Self {
        Self(Arc::new(Mutex::new(inner)))
    }
}

impl Clipboard for SharedClipboard {
    fn get_text(&mut self) -> PlatformResult<Option<String>> {
        self.0.lock().unwrap().get_text()
    }

    fn set_text(&mut self, text: &str) -> PlatformResult<()> {
        self.0.lock().unwrap().set_text(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rb_core::fakes::{FakeClipboard, FakeInputInjector, InputCall};

    #[test]
    fn clones_share_one_injector() {
        let probe = FakeInputInjector::new();
        let shared = SharedInjector::new(Box::new(probe.clone()));
        let mut a = shared.clone();
        let mut b = shared;
        a.key("KeyA", true).unwrap();
        b.release_all().unwrap();
        assert_eq!(
            probe.calls(),
            vec![
                InputCall::Key {
                    code: "KeyA".into(),
                    down: true
                },
                InputCall::ReleaseAll
            ]
        );
    }

    #[test]
    fn clones_share_one_clipboard() {
        let probe = FakeClipboard::new();
        let shared = SharedClipboard::new(Box::new(probe.clone()));
        let mut a = shared.clone();
        let mut b = shared;
        a.set_text("hello").unwrap();
        assert_eq!(b.get_text().unwrap().as_deref(), Some("hello"));
        assert_eq!(probe.write_count(), 1);
    }
}

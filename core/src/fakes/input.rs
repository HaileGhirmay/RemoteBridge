use std::sync::{Arc, Mutex};

use crate::PlatformResult;
use crate::traits::InputInjector;
use crate::types::MouseButton;

#[derive(Debug, Clone, PartialEq)]
pub enum InputCall {
    Key { code: String, down: bool },
    Button { button: MouseButton, down: bool },
    Wheel { dx: f32, dy: f32 },
    Move { x: f32, y: f32 },
    ReleaseAll,
}

/// Records everything it is asked to inject. Used to prove that the
/// permission filter drops disallowed input before it reaches the injector.
#[derive(Debug, Clone, Default)]
pub struct FakeInputInjector {
    calls: Arc<Mutex<Vec<InputCall>>>,
}

impl FakeInputInjector {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn calls(&self) -> Vec<InputCall> {
        self.calls.lock().unwrap().clone()
    }

    fn record(&self, call: InputCall) -> PlatformResult<()> {
        self.calls.lock().unwrap().push(call);
        Ok(())
    }
}

impl InputInjector for FakeInputInjector {
    fn key(&mut self, code: &str, down: bool) -> PlatformResult<()> {
        self.record(InputCall::Key {
            code: code.into(),
            down,
        })
    }

    fn button(&mut self, button: MouseButton, down: bool) -> PlatformResult<()> {
        self.record(InputCall::Button { button, down })
    }

    fn wheel(&mut self, dx: f32, dy: f32) -> PlatformResult<()> {
        self.record(InputCall::Wheel { dx, dy })
    }

    fn move_pointer(&mut self, x: f32, y: f32) -> PlatformResult<()> {
        self.record(InputCall::Move { x, y })
    }

    fn release_all(&mut self) -> PlatformResult<()> {
        self.record(InputCall::ReleaseAll)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_calls_in_order() {
        let probe = FakeInputInjector::new();
        let mut inj: Box<dyn InputInjector> = Box::new(probe.clone());
        inj.key("KeyA", true).unwrap();
        inj.button(MouseButton::Left, true).unwrap();
        inj.move_pointer(0.5, 0.25).unwrap();
        inj.release_all().unwrap();
        assert_eq!(
            probe.calls(),
            vec![
                InputCall::Key {
                    code: "KeyA".into(),
                    down: true
                },
                InputCall::Button {
                    button: MouseButton::Left,
                    down: true
                },
                InputCall::Move { x: 0.5, y: 0.25 },
                InputCall::ReleaseAll,
            ]
        );
    }
}

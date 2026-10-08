use std::sync::{Arc, Mutex};

use crate::PlatformResult;
use crate::traits::Clipboard;

#[derive(Debug, Default)]
struct State {
    text: Option<String>,
    writes: u32,
}

/// An in-memory clipboard.
#[derive(Debug, Clone, Default)]
pub struct FakeClipboard {
    state: Arc<Mutex<State>>,
}

impl FakeClipboard {
    pub fn new() -> Self {
        Self::default()
    }

    /// Put text there as if the local user had copied it.
    pub fn copy(&self, text: &str) {
        self.state.lock().unwrap().text = Some(text.to_owned());
    }

    pub fn text(&self) -> Option<String> {
        self.state.lock().unwrap().text.clone()
    }

    /// How many times `set_text` was called.
    pub fn write_count(&self) -> u32 {
        self.state.lock().unwrap().writes
    }
}

impl Clipboard for FakeClipboard {
    fn get_text(&mut self) -> PlatformResult<Option<String>> {
        Ok(self.text())
    }

    fn set_text(&mut self, text: &str) -> PlatformResult<()> {
        let mut s = self.state.lock().unwrap();
        s.text = Some(text.to_owned());
        s.writes += 1;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_text_and_counts_writes() {
        let probe = FakeClipboard::new();
        let mut cb: Box<dyn Clipboard> = Box::new(probe.clone());
        assert_eq!(cb.get_text().unwrap(), None);
        cb.set_text("hello").unwrap();
        assert_eq!(probe.text().as_deref(), Some("hello"));
        assert_eq!(probe.write_count(), 1);
        probe.copy("local");
        assert_eq!(cb.get_text().unwrap().as_deref(), Some("local"));
        assert_eq!(
            probe.write_count(),
            1,
            "copy is the local user, not a write"
        );
    }
}

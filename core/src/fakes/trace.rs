use std::sync::{Arc, Mutex};

/// A shared, ordered log of what happened. Fakes that share one `Trace` let a
/// test assert the *order* of effects across components, for example that all
/// capture stopped locally before the server was told.
#[derive(Debug, Clone, Default)]
pub struct Trace(Arc<Mutex<Vec<String>>>);

impl Trace {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&self, entry: impl Into<String>) {
        self.0.lock().unwrap().push(entry.into());
    }

    pub fn entries(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }

    /// Index of the first entry starting with `prefix`.
    pub fn position(&self, prefix: &str) -> Option<usize> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .position(|e| e.starts_with(prefix))
    }

    pub fn count(&self, prefix: &str) -> usize {
        self.0
            .lock()
            .unwrap()
            .iter()
            .filter(|e| e.starts_with(prefix))
            .count()
    }

    pub fn clear(&self) {
        self.0.lock().unwrap().clear();
    }
}

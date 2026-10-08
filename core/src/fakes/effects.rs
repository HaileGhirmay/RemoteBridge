use std::sync::{Arc, Mutex};

use crate::Permissions;
use crate::protocol::types::HostSessionView;
use crate::session::SessionEffects;

use super::Trace;

#[derive(Debug, Default)]
struct State {
    /// Sessions for which media is currently allowed to run.
    running: Vec<String>,
}

/// Records session effects and tracks which sessions may still run.
#[derive(Debug, Clone, Default)]
pub struct FakeEffects {
    state: Arc<Mutex<State>>,
    trace: Trace,
}

impl FakeEffects {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_trace(trace: Trace) -> Self {
        Self {
            state: Arc::default(),
            trace,
        }
    }

    pub fn trace(&self) -> &Trace {
        &self.trace
    }

    pub fn running(&self) -> Vec<String> {
        self.state.lock().unwrap().running.clone()
    }

    pub fn is_running(&self, session_id: &str) -> bool {
        self.state
            .lock()
            .unwrap()
            .running
            .iter()
            .any(|s| s == session_id)
    }
}

fn describe(grant: &Permissions) -> String {
    let mut parts = Vec::new();
    for (on, name) in [
        (grant.control, "control"),
        (grant.system_audio, "audio"),
        (grant.microphone, "mic"),
        (grant.clipboard, "clipboard"),
    ] {
        if on {
            parts.push(name);
        }
    }
    if parts.is_empty() {
        "view".into()
    } else {
        parts.join("+")
    }
}

impl SessionEffects for FakeEffects {
    fn start_session(&mut self, session: &HostSessionView) {
        self.trace.push(format!(
            "effects:start:{}:{}",
            session.id,
            describe(&session.granted_permissions)
        ));
        let mut s = self.state.lock().unwrap();
        if !s.running.contains(&session.id) {
            s.running.push(session.id.clone());
        }
    }

    fn update_grant(&mut self, session_id: &str, grant: &Permissions) {
        self.trace
            .push(format!("effects:grant:{session_id}:{}", describe(grant)));
    }

    fn stop_session(&mut self, session_id: &str) {
        self.trace.push(format!("effects:stop:{session_id}"));
        self.state
            .lock()
            .unwrap()
            .running
            .retain(|s| s != session_id);
    }

    fn stop_all(&mut self) {
        self.trace.push("effects:stop_all");
        self.state.lock().unwrap().running.clear();
    }
}

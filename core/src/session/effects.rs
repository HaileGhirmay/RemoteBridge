use crate::Permissions;
use crate::protocol::types::HostSessionView;

/// What the session manager does to the machine. The media layer (Prompt 08)
/// implements this to start and stop capture, audio, input and the data
/// channels; the manager never touches them directly.
pub trait SessionEffects: Send {
    /// The session was approved (state `connecting` or later): media may begin,
    /// restricted to `session.granted_permissions`.
    fn start_session(&mut self, session: &HostSessionView);

    /// The grant changed (escalation approved, or reduced by the host).
    /// Tracks and channels no longer allowed must stop at once.
    fn update_grant(&mut self, session_id: &str, grant: &Permissions);

    /// Stop capture, audio, input and clipboard for this session immediately.
    fn stop_session(&mut self, session_id: &str);

    /// Stop everything for every session immediately.
    fn stop_all(&mut self);
}

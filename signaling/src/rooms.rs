//! Rooms: at most one host and one viewer per session id.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::sync::mpsc::UnboundedSender;

use crate::auth::Role;

/// What a connection's writer task should do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outgoing {
    Text(String),
    /// Send a close frame and stop.
    Close {
        code: u16,
        reason: &'static str,
    },
}

#[derive(Debug, Clone)]
struct Peer {
    id: u64,
    tx: UnboundedSender<Outgoing>,
}

#[derive(Debug, Default)]
struct Room {
    host: Option<Peer>,
    viewer: Option<Peer>,
}

impl Room {
    fn slot(&mut self, role: Role) -> &mut Option<Peer> {
        match role {
            Role::Host => &mut self.host,
            Role::Viewer => &mut self.viewer,
        }
    }

    fn get(&self, role: Role) -> Option<&Peer> {
        match role {
            Role::Host => self.host.as_ref(),
            Role::Viewer => self.viewer.as_ref(),
        }
    }
}

pub fn peer_message(present: bool) -> String {
    format!(r#"{{"type":"peer","present":{present}}}"#)
}

pub fn error_message(message: &str) -> String {
    serde_json::json!({"type": "error", "message": message}).to_string()
}

#[derive(Debug, Default)]
pub struct Rooms {
    inner: Mutex<HashMap<String, Room>>,
    next_id: AtomicU64,
}

impl Rooms {
    pub fn new() -> Self {
        Self::default()
    }

    /// Put a connection into its room and return its id. A previous
    /// connection with the same role is told to close. Presence is announced:
    /// the other side learns that this peer is here, and the newcomer learns
    /// whether the other side already is.
    pub fn join(&self, sid: &str, role: Role, tx: UnboundedSender<Outgoing>) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        let mut rooms = self.inner.lock().unwrap();
        let room = rooms.entry(sid.to_owned()).or_default();
        if let Some(old) = room.slot(role).replace(Peer { id, tx: tx.clone() }) {
            let _ = old.tx.send(Outgoing::Close {
                code: 4000,
                reason: "replaced by a newer connection",
            });
        }
        if let Some(other) = room.get(role.other()) {
            let _ = other.tx.send(Outgoing::Text(peer_message(true)));
            let _ = tx.send(Outgoing::Text(peer_message(true)));
        }
        id
    }

    /// Remove a connection (only if it is still the current one for its role)
    /// and tell the other side. Empty rooms are dropped.
    pub fn leave(&self, sid: &str, role: Role, id: u64) {
        let mut rooms = self.inner.lock().unwrap();
        let Some(room) = rooms.get_mut(sid) else {
            return;
        };
        if room.get(role).is_some_and(|p| p.id == id) {
            *room.slot(role) = None;
            if let Some(other) = room.get(role.other()) {
                let _ = other.tx.send(Outgoing::Text(peer_message(false)));
            }
        }
        if room.host.is_none() && room.viewer.is_none() {
            rooms.remove(sid);
        }
    }

    /// Pass `text` unchanged to the other side. `false` if nobody is there.
    pub fn relay(&self, sid: &str, from: Role, text: String) -> bool {
        let rooms = self.inner.lock().unwrap();
        rooms
            .get(sid)
            .and_then(|room| room.get(from.other()))
            .is_some_and(|peer| peer.tx.send(Outgoing::Text(text)).is_ok())
    }

    #[cfg(test)]
    pub fn room_count(&self) -> usize {
        self.inner.lock().unwrap().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

    fn chan() -> (UnboundedSender<Outgoing>, UnboundedReceiver<Outgoing>) {
        unbounded_channel()
    }

    fn drain(rx: &mut UnboundedReceiver<Outgoing>) -> Vec<Outgoing> {
        let mut out = Vec::new();
        while let Ok(m) = rx.try_recv() {
            out.push(m);
        }
        out
    }

    fn text(s: &str) -> Outgoing {
        Outgoing::Text(s.to_owned())
    }

    #[test]
    fn presence_is_announced_to_both_sides() {
        let rooms = Rooms::new();
        let (htx, mut hrx) = chan();
        let (vtx, mut vrx) = chan();
        rooms.join("s", Role::Host, htx);
        assert!(drain(&mut hrx).is_empty(), "alone: nothing to announce");

        let vid = rooms.join("s", Role::Viewer, vtx);
        assert_eq!(drain(&mut hrx), vec![text(&peer_message(true))]);
        assert_eq!(drain(&mut vrx), vec![text(&peer_message(true))]);

        rooms.leave("s", Role::Viewer, vid);
        assert_eq!(drain(&mut hrx), vec![text(&peer_message(false))]);
    }

    #[test]
    fn a_host_joining_a_waiting_viewer_hears_about_it() {
        let rooms = Rooms::new();
        let (vtx, mut vrx) = chan();
        let (htx, mut hrx) = chan();
        rooms.join("s", Role::Viewer, vtx);
        rooms.join("s", Role::Host, htx);
        assert_eq!(drain(&mut hrx), vec![text(&peer_message(true))]);
        assert_eq!(drain(&mut vrx), vec![text(&peer_message(true))]);
    }

    #[test]
    fn relay_goes_to_the_other_side_only_and_unchanged() {
        let rooms = Rooms::new();
        let (htx, mut hrx) = chan();
        let (vtx, mut vrx) = chan();
        rooms.join("s", Role::Host, htx);
        rooms.join("s", Role::Viewer, vtx);
        drain(&mut hrx);
        drain(&mut vrx);

        let raw = r#"{"type":"offer",  "sdp":"v=0\r\n"}"#; // odd spacing must survive
        assert!(rooms.relay("s", Role::Host, raw.to_owned()));
        assert_eq!(drain(&mut vrx), vec![text(raw)]);
        assert!(drain(&mut hrx).is_empty());
    }

    #[test]
    fn relay_to_an_empty_seat_reports_false() {
        let rooms = Rooms::new();
        let (htx, _hrx) = chan();
        rooms.join("s", Role::Host, htx);
        assert!(!rooms.relay("s", Role::Host, "x".into()));
        assert!(!rooms.relay("nobody", Role::Viewer, "x".into()));
    }

    #[test]
    fn rooms_are_separate_by_session_id() {
        let rooms = Rooms::new();
        let (h1, mut h1rx) = chan();
        let (v2, mut v2rx) = chan();
        rooms.join("one", Role::Host, h1);
        rooms.join("two", Role::Viewer, v2);
        assert!(!rooms.relay("one", Role::Host, "x".into()));
        assert!(drain(&mut v2rx).is_empty() && drain(&mut h1rx).is_empty());
        assert_eq!(rooms.room_count(), 2);
    }

    #[test]
    fn a_second_socket_with_the_same_role_replaces_the_first() {
        let rooms = Rooms::new();
        let (h1, mut h1rx) = chan();
        let (h2, mut h2rx) = chan();
        let (vtx, mut vrx) = chan();
        let first = rooms.join("s", Role::Host, h1);
        rooms.join("s", Role::Viewer, vtx);
        drain(&mut h1rx);
        drain(&mut vrx);

        let second = rooms.join("s", Role::Host, h2);
        assert_ne!(first, second);
        assert_eq!(
            drain(&mut h1rx),
            vec![Outgoing::Close {
                code: 4000,
                reason: "replaced by a newer connection"
            }]
        );
        // The viewer is told the (new) host is present; the new host sees the viewer.
        assert_eq!(drain(&mut vrx), vec![text(&peer_message(true))]);
        assert_eq!(drain(&mut h2rx), vec![text(&peer_message(true))]);

        // The old connection's late "leave" must not evict the new one.
        rooms.leave("s", Role::Host, first);
        assert!(drain(&mut vrx).is_empty(), "no false 'peer left'");
        assert!(rooms.relay("s", Role::Viewer, "hello".into()));
        assert_eq!(drain(&mut h2rx), vec![text("hello")]);
    }

    #[test]
    fn empty_rooms_are_dropped() {
        let rooms = Rooms::new();
        let (htx, _hrx) = chan();
        let id = rooms.join("s", Role::Host, htx);
        assert_eq!(rooms.room_count(), 1);
        rooms.leave("s", Role::Host, id);
        assert_eq!(rooms.room_count(), 0);
        rooms.leave("s", Role::Host, id); // idempotent
    }
}

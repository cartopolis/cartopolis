//! In-process chat routing (ADR-028). Keyed by `avatar_id` (one per connection).
//!
//! Global and room delivery reuse one encoded frame; proximity (`Local`) encodes
//! per recipient because each carries its own `distance_m`. This is the concrete
//! form of the ADR's `ChatBus` seam — a trait can wrap it when multi-sim lands.
//!
//! It carries **voice as well as text**, and needed no generalising to do so: it
//! deals in already-encoded frames (`Vec<u8>`), so the payload was always opaque
//! to it. Proximity voice is the text path with an audio frame in place of the
//! string — including the per-recipient encode, which distance attenuation needs
//! for exactly the reason `distance_m` already needed it.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use glam::Vec3;
use tokio::sync::mpsc;

/// Proximity radii in metres (sim-space ≈ metres). `say` is default; `shout`
/// widens it.
pub const SAY_RADIUS_M: f32 = 50.0;
pub const SHOUT_RADIUS_M: f32 = 300.0;

/// Recipients of a proximity send, each with its own distance to the sender.
///
/// Text and voice share this: both fan out by distance and both encode per
/// recipient, differing only in what they put in the frame. `sender` is not
/// filtered here — text echoes to the sender and voice must not, so that call
/// belongs to the caller.
#[must_use]
pub fn within_radius(origin: Vec3, candidates: &[(u64, Vec3)], radius: f32) -> Vec<(u64, f32)> {
    candidates
        .iter()
        .filter_map(|(id, p)| {
            let d = (*p - origin).length();
            (d <= radius).then_some((*id, d))
        })
        .collect()
}

struct Client {
    tx: mpsc::UnboundedSender<Vec<u8>>,
    rooms: HashSet<String>,
}

#[derive(Default)]
pub struct ChatHub {
    clients: Mutex<HashMap<u64, Client>>,
}

impl ChatHub {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a connection's personal chat outbox.
    pub fn register(&self, avatar_id: u64, tx: mpsc::UnboundedSender<Vec<u8>>) {
        self.clients.lock().unwrap().insert(
            avatar_id,
            Client {
                tx,
                rooms: HashSet::new(),
            },
        );
    }

    pub fn unregister(&self, avatar_id: u64) {
        self.clients.lock().unwrap().remove(&avatar_id);
    }

    /// Join (`join = true`) or leave a room for a connection.
    pub fn set_room(&self, avatar_id: u64, room: &str, join: bool) {
        if let Some(c) = self.clients.lock().unwrap().get_mut(&avatar_id) {
            if join {
                c.rooms.insert(room.to_string());
            } else {
                c.rooms.remove(room);
            }
        }
    }

    /// Deliver one encoded frame to every connected client (Global).
    pub fn deliver_all(&self, frame: &[u8]) {
        for c in self.clients.lock().unwrap().values() {
            let _ = c.tx.send(frame.to_vec());
        }
    }

    /// Deliver one encoded frame to members of a room.
    pub fn deliver_room(&self, room: &str, frame: &[u8]) {
        for c in self.clients.lock().unwrap().values() {
            if c.rooms.contains(room) {
                let _ = c.tx.send(frame.to_vec());
            }
        }
    }

    /// Deliver one encoded frame to members of a room, skipping `except`.
    ///
    /// Voice needs this and text does not: seeing your own message appear is
    /// confirmation it was sent, but hearing your own voice come back is an echo.
    pub fn deliver_room_except(&self, room: &str, except: u64, frame: &[u8]) {
        for (id, c) in self.clients.lock().unwrap().iter() {
            if *id != except && c.rooms.contains(room) {
                let _ = c.tx.send(frame.to_vec());
            }
        }
    }

    /// Deliver a per-recipient frame to a specific avatar (proximity).
    pub fn deliver_to(&self, avatar_id: u64, frame: Vec<u8>) {
        if let Some(c) = self.clients.lock().unwrap().get(&avatar_id) {
            let _ = c.tx.send(frame);
        }
    }

    /// Connected avatar ids — the candidate set for a proximity send.
    pub fn connected_ids(&self) -> Vec<u64> {
        self.clients.lock().unwrap().keys().copied().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(x: f32) -> Vec3 {
        Vec3::new(x, 0.0, 0.0)
    }

    #[test]
    fn say_radius_excludes_beyond_and_keeps_distance() {
        let candidates = [(1, at(10.0)), (2, at(49.0)), (3, at(51.0))];
        let got = within_radius(Vec3::ZERO, &candidates, SAY_RADIUS_M);

        let ids: Vec<u64> = got.iter().map(|(id, _)| *id).collect();
        assert_eq!(ids, vec![1, 2], "3 is beyond say radius");
        // Each recipient carries its own distance — what attenuation reads.
        assert!((got[0].1 - 10.0).abs() < 0.001);
        assert!((got[1].1 - 49.0).abs() < 0.001);
    }

    #[test]
    fn shout_reaches_what_say_cannot() {
        let candidates = [(1, at(200.0))];
        assert!(within_radius(Vec3::ZERO, &candidates, SAY_RADIUS_M).is_empty());
        assert_eq!(within_radius(Vec3::ZERO, &candidates, SHOUT_RADIUS_M).len(), 1);
    }

    #[test]
    fn radius_is_a_sphere_not_an_axis() {
        // A listener directly above is as far as one to the side.
        let candidates = [(1, Vec3::new(0.0, 60.0, 0.0)), (2, Vec3::new(30.0, 0.0, 30.0))];
        let ids: Vec<u64> = within_radius(Vec3::ZERO, &candidates, SAY_RADIUS_M)
            .iter()
            .map(|(id, _)| *id)
            .collect();
        assert_eq!(ids, vec![2], "60 m up is out of say range; 42 m diagonal is in");
    }
}

//! In-process chat routing (ADR-028). Keyed by `avatar_id` (one per connection).
//!
//! Global and room delivery reuse one encoded frame; proximity (`Local`) encodes
//! per recipient because each carries its own `distance_m`. This is the concrete
//! form of the ADR's `ChatBus` seam — a trait can wrap it when multi-sim lands.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use tokio::sync::mpsc;

/// Proximity radii in metres (sim-space ≈ metres). `say` is default; `shout`
/// widens it.
pub const SAY_RADIUS_M: f32 = 50.0;
pub const SHOUT_RADIUS_M: f32 = 300.0;

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

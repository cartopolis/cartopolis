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
//!
//! # Why every outbox is bounded, and why there are two of them
//!
//! A connection's outbox is written by *other* connections' tasks and drained by
//! its own. When the two rates differ, something has to give, and an unbounded
//! queue answers "the server's memory" — one client whose TCP window has closed
//! stalls its `select!` on a `framed.send`, while nearby speakers keep pushing
//! ~100 KB/s of voice into a queue nobody is reading, and `send` on an unbounded
//! channel cannot fail, so nothing anywhere reports it. The snapshot path had the
//! answer already: a bounded `broadcast::channel(256)` whose `Lagged` is handled
//! and shrugged off. These are the same shape, with [`Lane`] deciding what
//! "handled" means for a frame that does not fit.

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

/// Which of a connection's two outboxes a frame belongs in.
///
/// The split exists because the two have opposite failure policies, not because
/// they carry different bytes — the hub still deals in opaque encoded frames. A
/// dropped voice frame is 20 ms of audio nobody will miss; a dropped chat line is
/// something a person typed and believes was delivered. One queue could only have
/// one policy, and whichever it was would be wrong for the other half.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lane {
    Text,
    Voice,
}

/// Chat frames a connection may be behind before lines are dropped.
///
/// Text is rate limited *per sender* (`net::MIN_LOCAL_INTERVAL` and friends), so
/// filling 256 slots takes a crowd, not a flood — a client this far behind is not
/// reading its socket at all.
const TEXT_BACKLOG: usize = 256;

/// Voice frames a connection may be behind. Deliberately about a second of audio
/// at the 20 ms frame cadence: an unbounded queue was the bug, but a *large*
/// bounded one is barely better, because everything in it is stale by the time it
/// is sent. Keeping the queue shorter than human patience is what makes dropping
/// the newest frame equivalent, in effect, to the client's own drop-the-oldest
/// backlog policy (`platform::audio::MAX_BACKLOG`).
const VOICE_BACKLOG: usize = 64;

struct Client {
    text: mpsc::Sender<Vec<u8>>,
    voice: mpsc::Sender<Vec<u8>>,
    rooms: HashSet<String>,
    /// Usernames this connection refuses to hear, lowercased. Loaded from
    /// `blocks` at register time and updated in place by [`ChatHub::set_blocked`].
    ///
    /// It lives here, on the *recipient*, because that is where the decision is:
    /// a block is one person's choice about their own client, not a sanction on
    /// the speaker. Keeping it beside the outbox is also what makes enforcement
    /// free — the fan-out already walks every connection, and skipping one costs
    /// a set lookup rather than a second encode or a database round trip on a
    /// path that runs at conversation speed.
    blocked: HashSet<String>,
}

impl Client {
    /// Hand a frame to one of this connection's outboxes, never blocking.
    ///
    /// `try_send` rather than `send().await` is the whole point: this is called
    /// from *other* clients' tasks, while the hub's mutex is held. An `.await`
    /// here would park a speaker's connection task on a listener's socket.
    fn enqueue(&self, avatar_id: u64, lane: Lane, frame: Vec<u8>) {
        let tx = match lane {
            Lane::Text => &self.text,
            Lane::Voice => &self.voice,
        };
        match tx.try_send(frame) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(_)) => match lane {
                // Worth a line: someone's words went missing, and the operator is
                // the only one who can see that it happened.
                Lane::Text => tracing::warn!(
                    avatar_id,
                    "chat backlog full; dropped a message for this client"
                ),
                Lane::Voice => tracing::trace!(avatar_id, "voice backlog full; dropped a frame"),
            },
            // The connection is on its way out and has not called `unregister`
            // yet. Nothing to say and nothing to do.
            Err(mpsc::error::TrySendError::Closed(_)) => {}
        }
    }
}

#[derive(Default)]
pub struct ChatHub {
    clients: Mutex<HashMap<u64, Client>>,
}

impl ChatHub {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a connection's personal chat outboxes and return the receiving
    /// ends, which the connection task drains.
    ///
    /// The hub creates them rather than taking them, so the capacities — and the
    /// fact that there are two — live next to the policy that enforces them.
    /// `blocked` is the connection's persisted block list (see the `blocks`
    /// table); it is passed in at register time rather than fetched here so the
    /// hub keeps no database handle.
    pub fn register(
        &self,
        avatar_id: u64,
        blocked: impl IntoIterator<Item = String>,
    ) -> (mpsc::Receiver<Vec<u8>>, mpsc::Receiver<Vec<u8>>) {
        let (text, text_rx) = mpsc::channel(TEXT_BACKLOG);
        let (voice, voice_rx) = mpsc::channel(VOICE_BACKLOG);
        self.clients.lock().unwrap().insert(
            avatar_id,
            Client {
                text,
                voice,
                rooms: HashSet::new(),
                blocked: blocked.into_iter().map(|u| u.to_lowercase()).collect(),
            },
        );
        (text_rx, voice_rx)
    }

    /// Add or remove a block for a live connection, so it takes effect on the
    /// next message rather than the next login.
    pub fn set_blocked(&self, avatar_id: u64, username: &str, blocked: bool) {
        if let Some(c) = self.clients.lock().unwrap().get_mut(&avatar_id) {
            let key = username.to_lowercase();
            if blocked {
                c.blocked.insert(key);
            } else {
                c.blocked.remove(&key);
            }
        }
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

    /// Whether a connection has joined a room.
    ///
    /// Membership used to be consulted for *recipients* only, so a client could
    /// address any room it could name without ever joining it — a broadcast into
    /// a conversation it is not part of, and (for text) a row in that room's
    /// history. A sender is a member first.
    #[must_use]
    pub fn in_room(&self, avatar_id: u64, room: &str) -> bool {
        self.clients
            .lock()
            .unwrap()
            .get(&avatar_id)
            .is_some_and(|c| c.rooms.contains(room))
    }

    /// Deliver one encoded frame to every connected client (Global).
    ///
    /// Text only: global voice is deliberately not carried (see `net`'s
    /// `VoiceFrame` arm), so there is no lane to choose.
    ///
    /// `from` is the *speaker's* username, and every delivery entry point takes
    /// it for one reason: it is what a recipient's block list is checked against.
    /// Passing it rather than deriving it from the frame keeps the hub's "opaque
    /// encoded bytes" property — it never has to decode a message to route it.
    pub fn deliver_all(&self, from: &str, frame: &[u8]) {
        let from = from.to_lowercase();
        for (id, c) in self.clients.lock().unwrap().iter() {
            if c.blocked.contains(&from) {
                continue;
            }
            c.enqueue(*id, Lane::Text, frame.to_vec());
        }
    }

    /// Deliver one encoded frame to members of a room.
    pub fn deliver_room(&self, room: &str, from: &str, lane: Lane, frame: &[u8]) {
        let from = from.to_lowercase();
        for (id, c) in self.clients.lock().unwrap().iter() {
            if c.rooms.contains(room) && !c.blocked.contains(&from) {
                c.enqueue(*id, lane, frame.to_vec());
            }
        }
    }

    /// Deliver one encoded frame to members of a room, skipping `except`.
    ///
    /// Voice needs this and text does not: seeing your own message appear is
    /// confirmation it was sent, but hearing your own voice come back is an echo.
    pub fn deliver_room_except(
        &self,
        room: &str,
        except: u64,
        from: &str,
        lane: Lane,
        frame: &[u8],
    ) {
        let from = from.to_lowercase();
        for (id, c) in self.clients.lock().unwrap().iter() {
            if *id != except && c.rooms.contains(room) && !c.blocked.contains(&from) {
                c.enqueue(*id, lane, frame.to_vec());
            }
        }
    }

    /// Deliver a per-recipient frame to a specific avatar (proximity).
    ///
    /// The block is applied here too, and it must be: proximity is the tier where
    /// somebody you have blocked can still walk up to you.
    pub fn deliver_to(&self, avatar_id: u64, from: &str, lane: Lane, frame: Vec<u8>) {
        if let Some(c) = self.clients.lock().unwrap().get(&avatar_id) {
            if c.blocked.contains(&from.to_lowercase()) {
                return;
            }
            c.enqueue(avatar_id, lane, frame);
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
        assert_eq!(
            within_radius(Vec3::ZERO, &candidates, SHOUT_RADIUS_M).len(),
            1
        );
    }

    /// A client that never reads costs a bounded amount of memory, and says so.
    ///
    /// The unbounded version of this test could not exist: there was no number to
    /// assert against, which is exactly why the problem was invisible.
    #[test]
    fn a_client_that_never_reads_bounds_the_queue_instead_of_the_heap() {
        let hub = ChatHub::new();
        let (_text_rx, mut voice_rx) = hub.register(1, []);

        // Ten times the backlog, from a "speaker" that is not this connection.
        for _ in 0..VOICE_BACKLOG * 10 {
            hub.deliver_to(1, "speaker", Lane::Voice, vec![0u8; 1500]);
        }
        let mut queued = 0;
        while voice_rx.try_recv().is_ok() {
            queued += 1;
        }
        assert_eq!(queued, VOICE_BACKLOG, "the voice queue grew past its bound");
    }

    /// The lanes are independent: a client drowning in voice still gets its chat.
    ///
    /// This is the reason for two channels rather than one bounded channel. With a
    /// shared queue, a burst of proximity voice is what decides whether a typed
    /// line survives, and the person who typed it is told nothing.
    #[test]
    fn a_voice_flood_does_not_drop_text() {
        let hub = ChatHub::new();
        let (mut text_rx, _voice_rx) = hub.register(1, []);

        for _ in 0..VOICE_BACKLOG * 4 {
            hub.deliver_to(1, "speaker", Lane::Voice, vec![0u8; 1500]);
        }
        hub.deliver_to(1, "speaker", Lane::Text, b"hello".to_vec());

        assert_eq!(
            text_rx.try_recv().ok(),
            Some(b"hello".to_vec()),
            "the text lane was consumed by voice"
        );
    }

    /// A sender must be in a room to address it — the check the hub only ever
    /// applied to recipients.
    #[test]
    fn room_membership_is_visible_to_the_sender_side() {
        let hub = ChatHub::new();
        let (_t, _v) = hub.register(1, []);
        assert!(!hub.in_room(1, "room:secret"), "not joined");
        hub.set_room(1, "room:secret", true);
        assert!(hub.in_room(1, "room:secret"));
        hub.set_room(1, "room:secret", false);
        assert!(!hub.in_room(1, "room:secret"), "left");
        // An unknown avatar is in no room, rather than a lookup failure.
        assert!(!hub.in_room(99, "room:secret"));
    }

    /// The block has to hold on every tier a person can be heard on, which is the
    /// whole reason `from` threads through all four delivery entry points.
    #[test]
    fn a_blocked_speaker_reaches_none_of_the_three_channels() {
        let hub = ChatHub::new();
        let (mut rx, _v) = hub.register(1, ["loud".to_string()]);
        hub.set_room(1, "room:x", true);

        hub.deliver_all("loud", b"global".as_ref());
        hub.deliver_room("room:x", "loud", Lane::Text, b"room".as_ref());
        hub.deliver_to(1, "loud", Lane::Text, b"local".to_vec());
        hub.deliver_room_except("room:x", 99, "loud", Lane::Voice, b"voice".as_ref());
        assert!(rx.try_recv().is_err(), "a blocked speaker got through");

        // …and everyone else still arrives.
        hub.deliver_all("quiet", b"global".as_ref());
        assert_eq!(rx.try_recv().ok(), Some(b"global".to_vec()));
    }

    /// A block is stored and compared lowercased, so it cannot be walked around
    /// by the shift key.
    #[test]
    fn blocking_ignores_case_on_both_sides() {
        let hub = ChatHub::new();
        let (mut rx, _v) = hub.register(1, ["LoUd".to_string()]);
        hub.deliver_all("lOuD", b"x".as_ref());
        assert!(rx.try_recv().is_err());
    }

    /// Unblocking takes effect on the next message, not the next login: the hub
    /// entry is edited in place beside the database write.
    #[test]
    fn unblocking_lands_on_the_live_connection() {
        let hub = ChatHub::new();
        let (mut rx, _v) = hub.register(1, ["loud".to_string()]);
        hub.deliver_all("loud", b"before".as_ref());
        assert!(rx.try_recv().is_err());

        hub.set_blocked(1, "Loud", false);
        hub.deliver_all("loud", b"after".as_ref());
        assert_eq!(rx.try_recv().ok(), Some(b"after".to_vec()));

        hub.set_blocked(1, "loud", true);
        hub.deliver_all("loud", b"again".as_ref());
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn radius_is_a_sphere_not_an_axis() {
        // A listener directly above is as far as one to the side.
        let candidates = [
            (1, Vec3::new(0.0, 60.0, 0.0)),
            (2, Vec3::new(30.0, 0.0, 30.0)),
        ];
        let ids: Vec<u64> = within_radius(Vec3::ZERO, &candidates, SAY_RADIUS_M)
            .iter()
            .map(|(id, _)| *id)
            .collect();
        assert_eq!(
            ids,
            vec![2],
            "60 m up is out of say range; 42 m diagonal is in"
        );
    }
}

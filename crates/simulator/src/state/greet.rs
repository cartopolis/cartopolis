//! Greetings between two avatars (protocol 49, `docs/notes/paired-gestures.md`):
//! one offers, the other accepts, and the world starts both at once.
//!
//! Everything a client could lie about is the world's to check here: that the
//! two are where *the sim* holds them, within reach of each other, on their
//! feet, and that the offer is still open. Blocks are the connection's
//! (`ChatHub::blocked_between`), checked by `net::greet` before it gets here.

use cartopolis_core::greet::{
    pair_up, Greeting, GREET_HOLD_SECS, GREET_REACH_M, GREET_STEP_M, OFFER_SECS,
};
use cartopolis_core::{EmoteState, GeoAnchor};

use super::{ground_distance_m, SimWorld};

/// An open offer.
pub(super) struct Offer {
    from: u64,
    to: u64,
    greeting: Greeting,
    /// Seconds since it was made.
    age: f32,
}

/// Why an offer or an acceptance was refused. Each is a `ServerError` to the
/// client that asked, with [`Self::code`] and [`Self::message`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GreetRefusal {
    Yourself,
    /// The asker is not standing anywhere yet.
    Nowhere,
    /// There is no such avatar, or it is not standing anywhere.
    NoSuchAvatar,
    Seated,
    OutOfReach,
    /// No open offer from that avatar: it lapsed, was withdrawn, or never was.
    NoOffer,
}

impl GreetRefusal {
    /// The HTTP-style code the refusal travels with.
    #[must_use]
    pub fn code(self) -> u32 {
        match self {
            Self::NoSuchAvatar | Self::NoOffer => 404,
            Self::Yourself | Self::Nowhere | Self::Seated | Self::OutOfReach => 422,
        }
    }

    #[must_use]
    pub fn message(self) -> &'static str {
        match self {
            Self::Yourself => "you cannot greet yourself",
            Self::Nowhere => "your avatar is not in the world yet",
            Self::NoSuchAvatar => "they are not here",
            Self::Seated => "stand up first",
            Self::OutOfReach => "walk up to them first",
            Self::NoOffer => "that offer has lapsed",
        }
    }
}

/// A greeting the world has started: who, and which way each now faces.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GreetingStart {
    pub greeting: Greeting,
    /// The one who offered, and the one who accepted.
    pub from: u64,
    pub to: u64,
    pub yaw_from: f32,
    pub yaw_to: f32,
}

/// `b` in metres east and south of `a`.
fn metres_from(a: GeoAnchor, b: GeoAnchor) -> [f32; 2] {
    let (per_lat, per_lng) = cartopolis_core::degree_scale_m(a.latitude);
    [
        ((b.longitude - a.longitude) * per_lng) as f32,
        ((a.latitude - b.latitude) * per_lat) as f32,
    ]
}

/// The place `m` metres east and south of `a`.
fn geo_at(a: GeoAnchor, m: [f32; 2]) -> GeoAnchor {
    let (per_lat, per_lng) = cartopolis_core::degree_scale_m(a.latitude);
    GeoAnchor {
        latitude: a.latitude - f64::from(m[1]) / per_lat,
        longitude: a.longitude + f64::from(m[0]) / per_lng,
    }
}

impl SimWorld {
    /// Whether `a` and `b` could greet now: both standing somewhere, neither
    /// seated, within [`GREET_REACH_M`] of each other and on one level.
    fn greet_ready(&self, a: u64, b: u64) -> Result<(GeoAnchor, GeoAnchor), GreetRefusal> {
        if a == b {
            return Err(GreetRefusal::Yourself);
        }
        let me = self.avatars.get(&a).ok_or(GreetRefusal::Nowhere)?;
        let here = me.place.ok_or(GreetRefusal::Nowhere)?;
        let them = self.avatars.get(&b).ok_or(GreetRefusal::NoSuchAvatar)?;
        let there = them.place.ok_or(GreetRefusal::NoSuchAvatar)?;
        if me.seated.is_some() || them.seated.is_some() {
            return Err(GreetRefusal::Seated);
        }
        if ground_distance_m(here, there) > GREET_REACH_M
            || (me.alt - them.alt).abs() > GREET_STEP_M
        {
            return Err(GreetRefusal::OutOfReach);
        }
        Ok((here, there))
    }

    /// `from` offers `greeting` to `to` (`NetMessage::OfferGreeting`). An
    /// offer again to the same avatar replaces the first, so asking twice is
    /// harmless and the latest greeting is the one on offer.
    pub fn offer_greeting(
        &mut self,
        from: u64,
        to: u64,
        greeting: Greeting,
    ) -> Result<(), GreetRefusal> {
        self.greet_ready(from, to)?;
        self.greetings.retain(|o| !(o.from == from && o.to == to));
        self.greetings.push(Offer {
            from,
            to,
            greeting,
            age: 0.0,
        });
        Ok(())
    }

    /// `to` takes the greeting `from` offered (`NetMessage::AcceptGreeting`):
    /// the two are stood where the clip needs them (`greet::pair_up`, about
    /// the middle between them, so neither is carried off), turned, held
    /// facing that way for [`GREET_HOLD_SECS`], and given the greeting as
    /// their emote with a fresh sequence each, in this one call — so the next
    /// snapshot shows both starting together to everyone.
    pub fn accept_greeting(&mut self, to: u64, from: u64) -> Result<GreetingStart, GreetRefusal> {
        let at = self
            .greetings
            .iter()
            .position(|o| o.from == from && o.to == to)
            .ok_or(GreetRefusal::NoOffer)?;
        let greeting = self.greetings[at].greeting;
        let (here, there) = self.greet_ready(from, to)?;
        self.greetings.remove(at);
        // Neither is greeting anyone else now.
        self.greetings
            .retain(|o| ![o.from, o.to].iter().any(|id| *id == from || *id == to));
        let pairing = pair_up([0.0, 0.0], metres_from(here, there), greeting);
        // One sequence for the pair, past both avatars' own: fresh to every
        // receiver of either, and the mark that the two belong together.
        let sequence = [from, to]
            .iter()
            .filter_map(|id| self.avatars.get(id))
            .map(|a| a.emote_sequence)
            .max()
            .unwrap_or(0)
            .wrapping_add(1);
        for (id, place, yaw) in [
            (from, geo_at(here, pairing.a), pairing.yaw_a),
            (to, geo_at(here, pairing.b), pairing.yaw_b),
        ] {
            if let Some(av) = self.avatars.get_mut(&id) {
                av.place = Some(place);
                av.yaw = yaw;
                av.velocity.x = 0.0;
                av.velocity.z = 0.0;
                av.greet_hold = GREET_HOLD_SECS;
                av.emote_sequence = sequence;
                av.emote = Some(EmoteState {
                    emote: greeting.emote(),
                    sequence,
                });
                av.emote_age = 0.0;
            }
        }
        Ok(GreetingStart {
            greeting,
            from,
            to,
            yaw_from: pairing.yaw_a,
            yaw_to: pairing.yaw_b,
        })
    }

    /// Drop every offer `avatar` made or was made: it moved or sat down. The
    /// ones it made are noted for its connection to tell the one offered.
    pub(super) fn withdraw_greetings_of(&mut self, avatar: u64) {
        if self.greetings.is_empty() {
            return;
        }
        let withdrawn = &mut self.withdrawn;
        self.greetings.retain(|o| {
            if o.from == avatar {
                withdrawn.push((o.to, o.from));
            }
            o.from != avatar && o.to != avatar
        });
    }

    /// The offers withdrawn since the last call, `(to, from)`.
    pub fn take_withdrawn(&mut self) -> Vec<(u64, u64)> {
        std::mem::take(&mut self.withdrawn)
    }

    /// Age the offers and let lapse those past [`OFFER_SECS`] and those whose
    /// avatars have left. A lapsed offer is not announced: both clients time
    /// it out on the same clock.
    pub(super) fn age_greetings(&mut self, dt: f32) {
        if self.greetings.is_empty() {
            return;
        }
        let avatars = &self.avatars;
        self.greetings.retain_mut(|o| {
            o.age += dt;
            o.age <= OFFER_SECS && avatars.contains_key(&o.from) && avatars.contains_key(&o.to)
        });
    }

    /// The open offer from `from` to `to`, if there is one.
    #[must_use]
    pub fn greeting_offered(&self, from: u64, to: u64) -> Option<Greeting> {
        self.greetings
            .iter()
            .find(|o| o.from == from && o.to == to)
            .map(|o| o.greeting)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::WorldCaps;
    use std::collections::{HashMap, HashSet};

    const LAT: f64 = 53.218;
    const LNG: f64 = 6.567;

    fn world() -> SimWorld {
        SimWorld::new(
            Vec::new(),
            WorldCaps {
                max_prims: 10,
                max_scripts: 10,
            },
            HashSet::new(),
            HashMap::new(),
        )
    }

    /// An avatar standing `east` metres east of the origin, facing north.
    fn stand(w: &mut SimWorld, east: f32) -> u64 {
        let id = w.spawn_avatar();
        let geo = geo_at(
            GeoAnchor {
                latitude: LAT,
                longitude: LNG,
            },
            [east, 0.0],
        );
        w.apply_intent(
            id,
            0.0,
            0.0,
            std::f32::consts::PI,
            false,
            None,
            None,
            Some(geo),
        );
        id
    }

    fn state(w: &SimWorld, id: u64) -> cartopolis_core::AvatarStateDto {
        w.placed_avatars()
            .into_iter()
            .find(|(i, ..)| *i == id)
            .map(|(.., s)| s)
            .unwrap()
    }

    /// Accepted within reach: both play the greeting from the same tick, stood
    /// at the clip's spacing about their old middle, turned point-symmetric.
    #[test]
    fn an_accepted_greeting_stands_both_and_starts_both() {
        let mut w = world();
        let (a, b) = (stand(&mut w, 0.0), stand(&mut w, 2.0));
        w.offer_greeting(a, b, Greeting::Handshake).unwrap();
        let start = w.accept_greeting(b, a).unwrap();
        let (sa, sb) = (state(&w, a), state(&w, b));
        assert_eq!(sa.emote.unwrap().emote, cartopolis_core::Emote::Handshake);
        assert_eq!(sb.emote.unwrap().emote, cartopolis_core::Emote::Handshake);
        assert_eq!(sa.emote.unwrap().sequence, sb.emote.unwrap().sequence);
        assert_eq!(sa.yaw, start.yaw_from);
        assert_eq!(sb.yaw, start.yaw_to);
        let apart = ground_distance_m(sa.geo, sb.geo);
        let [x, z] = Greeting::Handshake.partner_at();
        assert!((apart - x.hypot(z)).abs() < 0.01, "{apart} m apart");
        let origin = GeoAnchor {
            latitude: LAT,
            longitude: LNG,
        };
        let mid = (metres_from(origin, sa.geo)[0] + metres_from(origin, sb.geo)[0]) * 0.5;
        assert!((mid - 1.0).abs() < 0.01, "the middle stays at 1 m: {mid}");
        // The offer is spent.
        assert_eq!(w.accept_greeting(b, a), Err(GreetRefusal::NoOffer));
    }

    /// The facing is held against an intent already on its way, and let go
    /// the moment the avatar walks.
    #[test]
    fn the_facing_is_held_until_the_avatar_walks() {
        let mut w = world();
        let (a, b) = (stand(&mut w, 0.0), stand(&mut w, 1.5));
        w.offer_greeting(a, b, Greeting::HighFive).unwrap();
        let start = w.accept_greeting(b, a).unwrap();
        w.apply_intent(a, 0.0, 0.0, 2.0, false, None, None, None);
        assert_eq!(state(&w, a).yaw, start.yaw_from, "held");
        w.apply_intent(a, 0.5, 0.0, 2.0, false, None, None, None);
        assert!((state(&w, a).yaw - 2.0).abs() < 1e-4, "let go walking");
    }

    /// Out of reach, seated, to oneself, to nobody, after the offer lapsed,
    /// and after the offerer walked off: refused.
    #[test]
    fn greetings_are_refused_when_they_cannot_happen() {
        let mut w = world();
        let (a, b, far) = (stand(&mut w, 0.0), stand(&mut w, 2.0), stand(&mut w, 4.0));
        assert_eq!(
            w.offer_greeting(a, a, Greeting::Handshake),
            Err(GreetRefusal::Yourself)
        );
        assert_eq!(
            w.offer_greeting(a, far, Greeting::Handshake),
            Err(GreetRefusal::OutOfReach)
        );
        assert_eq!(
            w.offer_greeting(a, 999, Greeting::Handshake),
            Err(GreetRefusal::NoSuchAvatar)
        );
        w.apply_intent(b, 0.0, 0.0, 0.0, false, None, Some([0.3, 0.0]), None);
        assert_eq!(
            w.offer_greeting(a, b, Greeting::Handshake),
            Err(GreetRefusal::Seated)
        );
        w.apply_intent(b, 0.0, 0.0, 0.0, false, None, None, None);
        w.offer_greeting(a, b, Greeting::Handshake).unwrap();
        for _ in 0..((OFFER_SECS + 0.5) / 0.05) as usize {
            w.step(0.05);
        }
        assert_eq!(
            w.accept_greeting(b, a),
            Err(GreetRefusal::NoOffer),
            "lapsed"
        );
        w.offer_greeting(a, b, Greeting::Handshake).unwrap();
        w.apply_intent(a, 0.0, 1.0, 0.0, false, None, None, None);
        assert_eq!(w.take_withdrawn(), vec![(b, a)]);
        assert_eq!(
            w.accept_greeting(b, a),
            Err(GreetRefusal::NoOffer),
            "walked off"
        );
    }

    /// A greeting cannot be started from an intent: only the world starts one.
    #[test]
    fn an_intent_cannot_start_a_greeting() {
        let mut w = world();
        let a = stand(&mut w, 0.0);
        w.apply_intent(
            a,
            0.0,
            0.0,
            0.0,
            false,
            Some(cartopolis_core::Emote::HighFive),
            None,
            None,
        );
        assert_eq!(state(&w, a).emote, None);
        w.apply_intent(
            a,
            0.0,
            0.0,
            0.0,
            false,
            Some(cartopolis_core::Emote::Throw),
            None,
            None,
        );
        assert_eq!(
            state(&w, a).emote.map(|e| e.emote),
            Some(cartopolis_core::Emote::Throw)
        );
    }
}

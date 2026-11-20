//! Greeting another avatar: a handshake or a high five (protocol 49,
//! `docs/notes/paired-gestures.md`).
//!
//! Offered from the person card a tap on somebody's avatar opens
//! (`contacts::render_person_card`); shown to the one offered as the
//! interaction prompt's first button (`interact::render_interaction_prompt`),
//! which E or a tap accepts. The sim decides the rest and starts both bodies
//! at once, so this module only keeps the open offers, sends the two
//! requests, and turns the local avatar when the sim says it has started.

use bevy::prelude::*;
use cartopolis_core::greet::{Greeting, OFFER_SECS};
use cartopolis_core::msg;
use cartopolis_core::NetMessage;

use crate::components::RemoteAvatar;
use crate::resources::{AvatarState, OnlineSession};
use crate::systems::interact::InteractState;

/// The request id both greeting requests travel under, so a refusal finds
/// its way here and not to the prim-edit arm (`GreetInbox::wants`). "GREE".
pub const GREET_REQUEST: u32 = 0x4752_4545;

/// How long a refusal or a "waiting" line stays under the prompt, seconds.
const NOTICE_SECS: f32 = 4.0;

/// Greeting traffic the network system sorted out of the mailbox for
/// [`drive_greetings`], as it does for contacts.
#[derive(Resource, Default)]
pub struct GreetInbox(pub Vec<NetMessage>);

impl GreetInbox {
    /// Whether a message from the server is greeting traffic.
    #[must_use]
    pub fn wants(msg: &NetMessage) -> bool {
        match msg {
            NetMessage::GreetingOffered { .. } | NetMessage::GreetingStarted { .. } => true,
            NetMessage::ServerError { request_id, .. } => *request_id == GREET_REQUEST,
            _ => false,
        }
    }
}

/// A greeting somebody offered this avatar.
#[derive(Clone, Debug, PartialEq)]
pub struct Offer {
    pub from: u64,
    pub name: String,
    pub greeting: Greeting,
    /// Seconds before it lapses (the sim's [`OFFER_SECS`], on this clock).
    pub left: f32,
}

impl Offer {
    /// The prompt's line: "Shake hands with ada", "High five with ada".
    #[must_use]
    pub fn line(&self) -> String {
        format!("{} with {}", self.greeting.label(), self.name)
    }
}

/// The open offers, both ways.
#[derive(Resource, Default)]
pub struct Greetings {
    /// Offered to this avatar, newest last.
    pub incoming: Vec<Offer>,
    /// The one this avatar made and is waiting on: to whom, and how long left.
    pub outgoing: Option<(u64, f32)>,
    /// Which incoming offer the prompt shows: the nearest offerer's.
    pub shown: Option<u64>,
}

/// What a message from the server asks of the local avatar.
#[derive(Debug, PartialEq)]
pub enum Heard {
    Nothing,
    /// The sim has started `greeting`: turn to `yaw` and play it.
    Started {
        greeting: Greeting,
        yaw: f32,
    },
    /// A line to show the person: a refusal, an offer withdrawn.
    Notice(String),
}

impl Greetings {
    /// Take in one message from the server.
    pub fn receive(&mut self, msg: NetMessage) -> Heard {
        match msg {
            NetMessage::GreetingOffered {
                from,
                name,
                greeting,
            } => {
                self.incoming.retain(|o| o.from != from);
                match greeting {
                    Some(greeting) => {
                        self.incoming.push(Offer {
                            from,
                            name,
                            greeting,
                            left: OFFER_SECS,
                        });
                        Heard::Nothing
                    }
                    None => Heard::Notice(format!("{name} walked off")),
                }
            }
            NetMessage::GreetingStarted {
                greeting,
                partner,
                yaw,
            } => {
                self.incoming.retain(|o| o.from != partner);
                if self.outgoing.is_some_and(|(to, _)| to == partner) {
                    self.outgoing = None;
                }
                Heard::Started { greeting, yaw }
            }
            NetMessage::ServerError { message, .. } => {
                self.outgoing = None;
                Heard::Notice(capitalise(&message))
            }
            _ => Heard::Nothing,
        }
    }

    /// Advance the clocks by `dt` and let lapse what has run out. `moving`:
    /// the avatar is walking, which withdraws everything both ways (the sim
    /// does the same on its side).
    pub fn tick(&mut self, dt: f32, moving: bool) {
        if moving {
            self.incoming.clear();
            self.outgoing = None;
            return;
        }
        for o in &mut self.incoming {
            o.left -= dt;
        }
        self.incoming.retain(|o| o.left > 0.0);
        if let Some((_, left)) = &mut self.outgoing {
            *left -= dt;
            if *left <= 0.0 {
                self.outgoing = None;
            }
        }
    }

    /// Choose the offer the prompt shows: the one whose offerer stands
    /// nearest `here`, by `at` (where each avatar is, if it is drawn).
    pub fn choose(&mut self, here: Vec2, at: impl Fn(u64) -> Option<Vec2>) {
        let nearest = self
            .incoming
            .iter()
            .map(|o| (o.from, at(o.from).map_or(f32::MAX, |p| p.distance(here))))
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(id, _)| id);
        if self.shown != nearest {
            self.shown = nearest;
        }
    }

    /// The offer the prompt shows.
    #[must_use]
    pub fn prompt(&self) -> Option<&Offer> {
        let id = self.shown?;
        self.incoming.iter().find(|o| o.from == id)
    }

    /// Offer `greeting` to the avatar `to`.
    pub fn offer(&mut self, online: &OnlineSession, to: u64, greeting: Greeting) {
        if online.send(
            "offer a greeting",
            NetMessage::OfferGreeting(msg::OfferGreeting {
                request_id: GREET_REQUEST,
                to,
                greeting,
            }),
        ) {
            self.outgoing = Some((to, OFFER_SECS));
        }
    }

    /// Take the offer the prompt shows. The sim answers with
    /// `GreetingStarted`, or a refusal.
    pub fn accept(&mut self, online: &OnlineSession) {
        let Some(from) = self.prompt().map(|o| o.from) else {
            return;
        };
        self.incoming.retain(|o| o.from != from);
        online.send(
            "accept a greeting",
            NetMessage::AcceptGreeting(msg::AcceptGreeting {
                request_id: GREET_REQUEST,
                from,
            }),
        );
    }

    /// Let the offer the prompt shows go unanswered. Nothing is sent: an
    /// offer ignored simply lapses.
    pub fn dismiss(&mut self) {
        if let Some(id) = self.shown.take() {
            self.incoming.retain(|o| o.from != id);
        }
    }
}

fn capitalise(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map(|f| f.to_uppercase().chain(c).collect())
        .unwrap_or_default()
}

/// Take the server's greeting traffic, age the offers, choose the one the
/// prompt shows, and turn and start the local avatar when the sim has.
pub fn drive_greetings(
    time: Res<Time>,
    online: Option<Res<OnlineSession>>,
    mut inbox: ResMut<GreetInbox>,
    mut greetings: ResMut<Greetings>,
    mut avatar_state: ResMut<AvatarState>,
    mut interact: ResMut<InteractState>,
    remotes: Query<(&RemoteAvatar, &Transform)>,
) {
    if online.is_none() {
        inbox.0.clear();
        if !greetings.incoming.is_empty() || greetings.outgoing.is_some() {
            *greetings = Greetings::default();
        }
        return;
    }
    for msg in inbox.0.drain(..) {
        match greetings.receive(msg) {
            Heard::Nothing => {}
            Heard::Started { greeting, yaw } => {
                // The sim has turned this avatar and holds it there while the
                // gesture plays; it has also stood it at the clip's spacing,
                // which the reconciliation eases the body onto.
                avatar_state.facing_yaw = yaw;
                avatar_state.pending_emote = Some(greeting.emote());
                interact.last = None;
                interact.last_ttl = 0.0;
            }
            Heard::Notice(line) => {
                interact.last = Some(line);
                interact.last_ttl = NOTICE_SECS;
            }
        }
    }
    let moving = avatar_state.stride_speed() >= crate::systems::locomotion::MOVING_EPS;
    if moving || !greetings.incoming.is_empty() || greetings.outgoing.is_some() {
        greetings.tick(time.delta_secs(), moving);
    }
    if greetings.incoming.is_empty() {
        if greetings.shown.is_some() {
            greetings.shown = None;
        }
        return;
    }
    let here = Vec2::new(avatar_state.position.x, avatar_state.position.z);
    greetings.choose(here, |id| {
        remotes
            .iter()
            .find(|(r, _)| r.sim_id == id)
            .map(|(_, tf)| Vec2::new(tf.translation.x, tf.translation.z))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn offered(from: u64, greeting: Option<Greeting>) -> NetMessage {
        NetMessage::GreetingOffered {
            from,
            name: format!("p{from}"),
            greeting,
        }
    }

    /// An offer is kept until it lapses, replaced by the same person's next,
    /// and taken back when they walk off.
    #[test]
    fn offers_are_kept_replaced_withdrawn_and_lapse() {
        let mut g = Greetings::default();
        assert_eq!(
            g.receive(offered(2, Some(Greeting::Handshake))),
            Heard::Nothing
        );
        g.receive(offered(2, Some(Greeting::HighFive)));
        assert_eq!(g.incoming.len(), 1);
        assert_eq!(g.incoming[0].greeting, Greeting::HighFive);
        assert_eq!(g.incoming[0].line(), "High five with p2");
        assert_eq!(
            g.receive(offered(2, None)),
            Heard::Notice("p2 walked off".into())
        );
        assert!(g.incoming.is_empty());

        g.receive(offered(3, Some(Greeting::Handshake)));
        g.tick(OFFER_SECS - 0.5, false);
        assert_eq!(g.incoming.len(), 1, "not yet");
        g.tick(1.0, false);
        assert!(g.incoming.is_empty(), "lapsed");
    }

    /// The prompt shows the nearest offerer's, and accepting or walking off
    /// clears it.
    #[test]
    fn the_prompt_shows_the_nearest_offer() {
        let mut g = Greetings::default();
        g.receive(offered(2, Some(Greeting::Handshake)));
        g.receive(offered(3, Some(Greeting::HighFive)));
        let at = |id: u64| Some(Vec2::new(if id == 2 { 3.0 } else { 1.0 }, 0.0));
        g.choose(Vec2::ZERO, at);
        assert_eq!(g.prompt().map(|o| o.from), Some(3));
        g.dismiss();
        g.choose(Vec2::ZERO, at);
        assert_eq!(g.prompt().map(|o| o.from), Some(2));
        g.tick(0.1, true);
        assert!(g.incoming.is_empty() && g.outgoing.is_none(), "walking off");
    }

    /// The start turns the avatar and ends the offers with that partner; a
    /// refusal is a line for the person.
    #[test]
    fn a_start_turns_and_a_refusal_is_said() {
        let mut g = Greetings {
            outgoing: Some((4, 5.0)),
            ..Greetings::default()
        };
        g.receive(offered(4, Some(Greeting::Handshake)));
        assert_eq!(
            g.receive(NetMessage::GreetingStarted {
                greeting: Greeting::Handshake,
                partner: 4,
                yaw: 1.5,
            }),
            Heard::Started {
                greeting: Greeting::Handshake,
                yaw: 1.5
            }
        );
        assert!(g.incoming.is_empty() && g.outgoing.is_none());
        assert_eq!(
            g.receive(NetMessage::ServerError {
                request_id: GREET_REQUEST,
                code: 422,
                message: "walk up to them first".into(),
            }),
            Heard::Notice("Walk up to them first".into())
        );
        assert!(GreetInbox::wants(&NetMessage::ServerError {
            request_id: GREET_REQUEST,
            code: 404,
            message: String::new(),
        }));
        assert!(!GreetInbox::wants(&NetMessage::ServerError {
            request_id: 7,
            code: 403,
            message: String::new(),
        }));
    }
}

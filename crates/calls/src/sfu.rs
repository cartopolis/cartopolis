//! The forwarder the simulator runs: every call's media arrives here over one
//! UDP port and leaves again for the call's other participants, unopened.
//!
//! Sans-IO, like str0m under it: the simulator owns the socket and the clock,
//! hands datagrams in with [`Sfu::receive`], and sends what [`Sfu::poll`]
//! returns. Signalling (SDP) travels over the simulator's existing connection,
//! which is also where it decides who may join which call — this module takes a
//! join it is given and does not know what a chat or an account is.
//!
//! What it takes from Galène (`docs/notes/calls.md`):
//!
//! - **One direction per connection, and the sender always offers.** Each
//!   participant has an *up* connection (it offers its microphone and camera;
//!   we answer) and a *down* connection (we offer everybody else's tracks; it
//!   answers). Neither side ever has to answer an offer while its own is
//!   outstanding, which is the renegotiation race that makes a single
//!   bidirectional connection hard.
//! - **One port for everything**, demultiplexed here by asking each connection
//!   whether a datagram is its own (`Rtc::accepts`).
//! - **Repair is the forwarder's job.** A gap in a sender's video asks it for a
//!   keyframe; a receiver's keyframe request is passed to the right sender; a
//!   receiver that has just subscribed gets one too, so a new face appears
//!   without waiting for the sender's next scheduled keyframe. All of it
//!   throttled to one per second per track, or a lossy receiver would make a
//!   sender spend its whole bitrate on keyframes.
//!
//! Nothing here can open a frame: frames are SFrame-sealed by their sender, and
//! only the start of a VP8 or VP9 payload is readable (`crate::media`), which is
//! what str0m reads to tell a keyframe. A browser sends VP9 with one spatial
//! layer (`webcall.js`): str0m would join a picture's spatial layers into one
//! frame, and each is sealed on its own.

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use str0m::change::{SdpAnswer, SdpOffer, SdpPendingOffer};
use str0m::media::{Direction, KeyframeRequestKind, MediaData, MediaKind, Mid};
use str0m::net::{Protocol, Receive};
use str0m::{Candidate, Event, IceConnectionState, Input, Output, Rtc};

pub use crate::media::{Kind, ParticipantId, TrackInfo, RECONNECT_GRACE};

/// A call. The simulator uses the private chat's group id.
pub type RoomId = Vec<u8>;

/// Beyond this a call is refused at join. Everyone uploads once and downloads
/// everyone else, so the forwarder's bandwidth grows with the square of this;
/// Galène measures ~20 per meeting per core.
pub const MAX_PARTICIPANTS: usize = 16;

/// The shortest gap between two keyframe requests for one track.
const KEYFRAME_THROTTLE: Duration = Duration::from_secs(1);

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("unreadable SDP: {0}")]
    Sdp(String),
    #[error("refused: {0}")]
    Rtc(String),
    #[error("no such participant")]
    Unknown,
    #[error("the call is full")]
    Full,
    #[error("no offer is waiting for an answer")]
    NoPendingOffer,
}

fn rtc_err(e: impl std::fmt::Display) -> Error {
    Error::Rtc(e.to_string())
}

impl From<MediaKind> for Kind {
    fn from(k: MediaKind) -> Self {
        match k {
            MediaKind::Audio => Kind::Audio,
            MediaKind::Video => Kind::Video,
        }
    }
}

impl From<Kind> for MediaKind {
    fn from(k: Kind) -> Self {
        match k {
            Kind::Audio => MediaKind::Audio,
            Kind::Video | Kind::Screen => MediaKind::Video,
        }
    }
}

/// What the simulator must tell a participant over its signalling channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Signal {
    /// A new offer for `to`'s down connection. It answers with [`Sfu::answer`].
    Offer {
        to: ParticipantId,
        sdp: String,
        tracks: Vec<TrackInfo>,
    },
    /// `who`'s connection failed or was closed; it is no longer in the call.
    Left { who: ParticipantId },
}

/// One datagram to send.
#[derive(Debug, Clone)]
pub struct Transmit {
    pub destination: SocketAddr,
    pub contents: Vec<u8>,
}

/// A track some participant sends.
#[derive(Debug)]
struct TrackIn {
    mid: Mid,
    kind: Kind,
    last_keyframe_request: Option<Instant>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutState {
    /// Not yet in an offer.
    ToOpen,
    /// In an offer that has not been answered.
    Negotiating(Mid),
    Open(Mid),
    /// Its sender left; to be removed in the next offer.
    ToStop(Mid),
    Stopping(Mid),
}

/// A track on a participant's down connection: somebody else's track in.
#[derive(Debug)]
struct TrackOut {
    from: ParticipantId,
    from_mid: Mid,
    kind: Kind,
    state: OutState,
}

impl TrackOut {
    fn mid(&self) -> Option<Mid> {
        match self.state {
            OutState::ToOpen => None,
            OutState::Negotiating(m)
            | OutState::Open(m)
            | OutState::ToStop(m)
            | OutState::Stopping(m) => Some(m),
        }
    }
}

struct Participant {
    id: ParticipantId,
    room: RoomId,
    up: Rtc,
    down: Rtc,
    tracks_in: Vec<TrackIn>,
    tracks_out: Vec<TrackOut>,
    /// Senders this participant does not receive ([`Sfu::ignore`]).
    ignores: Vec<ParticipantId>,
    /// The up media id its shared screen arrives on, named at the join.
    screen_mid: Option<Mid>,
    /// Its sound is dropped here, not forwarded ([`Sfu::hold`]).
    held: bool,
    /// Its shared screen is dropped here ([`Sfu::hold_screen`]).
    screen_held: bool,
    /// Whose cameras it draws; `None` is everybody ([`Sfu::watch`]).
    watching: Option<Vec<ParticipantId>>,
    pending: Option<SdpPendingOffer>,
    alive: bool,
    /// When either connection last went `Disconnected` without coming back.
    lost_since: Option<Instant>,
    /// Whether an offer has been made on the down connection. It is not
    /// driven before: alone in a call there is nothing to negotiate, and its
    /// handshake timer would otherwise end the participant at ~30 s.
    down_started: bool,
}

/// Media that arrived on somebody's up connection, waiting to be forwarded.
struct Forward {
    from: ParticipantId,
    data: MediaData,
}

pub struct Sfu {
    /// The address clients reach the forwarder's UDP port at. Advertised as the
    /// one ICE candidate, and given as every datagram's destination: behind
    /// Docker's NAT the socket's own address is not one any client can use.
    public: SocketAddr,
    participants: Vec<Participant>,
    next_id: u64,
    signals: VecDeque<Signal>,
}

impl Sfu {
    #[must_use]
    pub fn new(public: SocketAddr) -> Self {
        crate::install_crypto();
        Self {
            public,
            participants: Vec::new(),
            next_id: 1,
            signals: VecDeque::new(),
        }
    }

    fn rtc(&self, now: Instant) -> Result<Rtc, Error> {
        fresh_rtc(self.public, now)
    }

    /// Admit a participant to `room` with the offer for its up connection.
    /// Returns its id and our answer. Everybody already in the call is offered
    /// to it on its down connection at the next [`Sfu::poll`].
    pub fn join(
        &mut self,
        now: Instant,
        room: RoomId,
        offer: &str,
        screen: Option<&str>,
    ) -> Result<(ParticipantId, String), Error> {
        if self
            .participants
            .iter()
            .filter(|p| p.room == room && p.alive)
            .count()
            >= MAX_PARTICIPANTS
        {
            return Err(Error::Full);
        }
        let offer = SdpOffer::from_sdp_string(offer).map_err(|e| Error::Sdp(e.to_string()))?;
        let mut up = self.rtc(now)?;
        let answer = up.sdp_api().accept_offer(offer).map_err(rtc_err)?;
        let down = self.rtc(now)?;
        let id = ParticipantId(self.next_id);
        self.next_id += 1;

        let tracks_out = self
            .participants
            .iter()
            .filter(|p| p.room == room && p.alive)
            .flat_map(|p| {
                p.tracks_in.iter().map(|t| TrackOut {
                    from: p.id,
                    from_mid: t.mid,
                    kind: t.kind,
                    state: OutState::ToOpen,
                })
            })
            .collect();
        self.participants.push(Participant {
            id,
            room,
            up,
            down,
            tracks_in: Vec::new(),
            tracks_out,
            ignores: Vec::new(),
            screen_mid: screen.map(Mid::from),
            held: false,
            screen_held: false,
            watching: None,
            pending: None,
            alive: true,
            lost_since: None,
            down_started: false,
        });
        Ok((id, answer.to_sdp_string()))
    }

    /// A participant's answer to the last [`Signal::Offer`] it was sent.
    pub fn answer(&mut self, now: Instant, who: ParticipantId, sdp: &str) -> Result<(), Error> {
        let answer = SdpAnswer::from_sdp_string(sdp).map_err(|e| Error::Sdp(e.to_string()))?;
        let p = self.participant_mut(who).ok_or(Error::Unknown)?;
        let pending = p.pending.take().ok_or(Error::NoPendingOffer)?;
        p.down
            .sdp_api()
            .accept_answer(pending, answer)
            .map_err(rtc_err)?;
        let mut opened = Vec::new();
        for t in &mut p.tracks_out {
            match t.state {
                OutState::Negotiating(m) => {
                    t.state = OutState::Open(m);
                    if t.kind.is_video() {
                        opened.push((t.from, t.from_mid));
                    }
                }
                _ => {}
            }
        }
        p.tracks_out
            .retain(|t| !matches!(t.state, OutState::Stopping(_)));
        // A new subscriber needs a keyframe now, not at the sender's next one.
        for (from, mid) in opened {
            self.request_keyframe(from, mid, KeyframeRequestKind::Pli, now);
        }
        Ok(())
    }

    /// Stop forwarding `sender`'s media to `receiver`: a block. Its tracks
    /// are withdrawn from `receiver`'s down connection at the next offer and
    /// never offered again, so the refusal is here in the forwarder rather than
    /// in the receiver's app, which could be rewritten to ignore it.
    pub fn ignore(&mut self, receiver: ParticipantId, sender: ParticipantId) {
        let Some(p) = self.participant_mut(receiver) else {
            return;
        };
        if !p.ignores.contains(&sender) {
            p.ignores.push(sender);
        }
        p.tracks_out.retain_mut(|t| {
            if t.from != sender {
                return true;
            }
            match t.state {
                OutState::ToOpen => false,
                OutState::Negotiating(m) | OutState::Open(m) => {
                    t.state = OutState::ToStop(m);
                    true
                }
                OutState::ToStop(_) | OutState::Stopping(_) => true,
            }
        });
    }

    /// Drop (or pass again) everything `who` says, at the forwarder: a host's
    /// mute that a rewritten app cannot undo.
    pub fn hold(&mut self, who: ParticipantId, held: bool) {
        if let Some(p) = self.participant_mut(who) {
            p.held = held;
        }
    }

    /// Drop (or pass again) `who`'s shared screen. Passing it again asks for a
    /// keyframe, so its receivers get a whole picture at once.
    pub fn hold_screen(&mut self, now: Instant, who: ParticipantId, held: bool) {
        let Some(p) = self.participant_mut(who) else {
            return;
        };
        let was = std::mem::replace(&mut p.screen_held, held);
        let screen = p.screen_mid;
        if was && !held {
            if let Some(mid) = screen {
                self.request_keyframe(who, mid, KeyframeRequestKind::Pli, now);
            }
        }
    }

    /// Whose cameras `receiver` draws (`None`: everybody's). A camera it
    /// starts drawing gets a keyframe request, since what it skipped is what
    /// the next picture would build on.
    pub fn watch(
        &mut self,
        now: Instant,
        receiver: ParticipantId,
        cameras: Option<Vec<ParticipantId>>,
    ) {
        let Some(p) = self.participant_mut(receiver) else {
            return;
        };
        let before = std::mem::replace(&mut p.watching, cameras.clone());
        let room = p.room.clone();
        let newly: Vec<ParticipantId> = self
            .participants
            .iter()
            .filter(|s| s.alive && s.room == room && s.id != receiver)
            .map(|s| s.id)
            .filter(|s| {
                let now_in = cameras.as_ref().is_none_or(|w| w.contains(s));
                let was_in = before.as_ref().is_none_or(|w| w.contains(s));
                now_in && !was_in
            })
            .collect();
        for s in newly {
            let mids: Vec<Mid> = self
                .participant(s)
                .map(|p| {
                    p.tracks_in
                        .iter()
                        .filter(|t| t.kind == Kind::Video)
                        .map(|t| t.mid)
                        .collect()
                })
                .unwrap_or_default();
            for mid in mids {
                self.request_keyframe(s, mid, KeyframeRequestKind::Pli, now);
            }
        }
    }

    /// Take a participant out of its call.
    pub fn leave(&mut self, who: ParticipantId) {
        if let Some(p) = self.participant_mut(who) {
            p.alive = false;
            p.up.disconnect();
            p.down.disconnect();
        }
    }

    /// Who is in a call, in join order.
    #[must_use]
    pub fn members(&self, room: &[u8]) -> Vec<ParticipantId> {
        self.participants
            .iter()
            .filter(|p| p.room == room && p.alive)
            .map(|p| p.id)
            .collect()
    }

    /// The call a participant is in.
    #[must_use]
    pub fn room_of(&self, who: ParticipantId) -> Option<&[u8]> {
        self.participant(who).map(|p| p.room.as_slice())
    }

    /// Hand in a datagram that arrived on the forwarder's socket. Returns
    /// whether any connection claimed it; one nobody claims is dropped, which
    /// is normal for the first STUN of a join still being processed.
    pub fn receive(&mut self, now: Instant, source: SocketAddr, contents: &[u8]) -> bool {
        let Ok(contents) = contents.try_into() else {
            return false;
        };
        let input = Input::Receive(
            now,
            Receive {
                proto: Protocol::Udp,
                source,
                destination: self.public,
                contents,
            },
        );
        for p in self.participants.iter_mut().filter(|p| p.alive) {
            let rtc = if p.up.accepts(&input) {
                &mut p.up
            } else if p.down.accepts(&input) {
                &mut p.down
            } else {
                continue;
            };
            if rtc.handle_input(input).is_err() {
                p.alive = false;
            }
            return true;
        }
        false
    }

    /// Advance every connection to `now`: forward media, answer keyframe
    /// requests, start renegotiations. Returns the datagrams to send and when to
    /// call again at the latest.
    pub fn poll(&mut self, now: Instant) -> (Vec<Transmit>, Instant) {
        let mut out = Vec::new();
        let mut next = now + Duration::from_millis(100);
        let mut forwards = Vec::new();
        let mut keyframes = Vec::new();
        let mut added = Vec::new();

        for p in self.participants.iter_mut().filter(|p| p.alive) {
            for up in [true, false] {
                if !up && !p.down_started {
                    continue;
                }
                let rtc = if up { &mut p.up } else { &mut p.down };
                if rtc.handle_input(Input::Timeout(now)).is_err() {
                    p.alive = false;
                    break;
                }
                loop {
                    match rtc.poll_output() {
                        Ok(Output::Transmit(t)) => out.push(Transmit {
                            destination: t.destination,
                            contents: t.contents.to_vec(),
                        }),
                        Ok(Output::Timeout(t)) => {
                            next = next.min(t);
                            break;
                        }
                        Ok(Output::Event(e)) => match e {
                            Event::IceConnectionStateChange(IceConnectionState::Disconnected) => {
                                p.lost_since.get_or_insert(now);
                            }
                            Event::IceConnectionStateChange(
                                IceConnectionState::Connected | IceConnectionState::Completed,
                            ) => p.lost_since = None,
                            Event::Closed => {
                                p.alive = false;
                                break;
                            }
                            Event::MediaAdded(m) if up => {
                                let kind = if p.screen_mid == Some(m.mid) {
                                    Kind::Screen
                                } else {
                                    m.kind.into()
                                };
                                p.tracks_in.push(TrackIn {
                                    mid: m.mid,
                                    kind,
                                    last_keyframe_request: None,
                                });
                                added.push((p.room.clone(), p.id, m.mid, kind));
                            }
                            Event::MediaData(data) if up => {
                                // Held sound and a forbidden screen stop here,
                                // at the source, for every receiver at once.
                                let kind = p
                                    .tracks_in
                                    .iter()
                                    .find(|t| t.mid == data.mid)
                                    .map(|t| t.kind);
                                if (p.held && kind == Some(Kind::Audio))
                                    || (p.screen_held && kind == Some(Kind::Screen))
                                {
                                    continue;
                                }
                                if !data.contiguous && data.params.spec().codec.is_video() {
                                    keyframes.push((p.id, data.mid, KeyframeRequestKind::Fir));
                                }
                                forwards.push(Forward { from: p.id, data });
                            }
                            Event::KeyframeRequest(req) if !up => {
                                if let Some(t) =
                                    p.tracks_out.iter().find(|t| t.mid() == Some(req.mid))
                                {
                                    keyframes.push((t.from, t.from_mid, req.kind));
                                }
                            }
                            _ => {}
                        },
                        Err(_) => {
                            p.alive = false;
                            break;
                        }
                    }
                }
                if !p.alive {
                    break;
                }
            }
        }

        // A connection gone for longer than the grace is gone.
        for p in &mut self.participants {
            if p.lost_since
                .is_some_and(|t| now.duration_since(t) > RECONNECT_GRACE)
            {
                p.alive = false;
            }
        }

        // A participant that just started sending: offer its track to the rest.
        for (room, from, mid, kind) in added {
            for q in self
                .participants
                .iter_mut()
                .filter(|q| q.alive && q.room == room && q.id != from && !q.ignores.contains(&from))
            {
                q.tracks_out.push(TrackOut {
                    from,
                    from_mid: mid,
                    kind,
                    state: OutState::ToOpen,
                });
            }
        }

        for f in forwards {
            self.forward(&f);
        }
        for (from, mid, kind) in keyframes {
            self.request_keyframe(from, mid, kind, now);
        }
        self.retire_dead();
        self.negotiate(now);

        // Forwarding and negotiating queued output on the down connections.
        for p in self.participants.iter_mut().filter(|p| p.alive) {
            while let Ok(o) = p.down.poll_output() {
                match o {
                    Output::Transmit(t) => out.push(Transmit {
                        destination: t.destination,
                        contents: t.contents.to_vec(),
                    }),
                    Output::Timeout(t) => {
                        next = next.min(t);
                        break;
                    }
                    Output::Event(_) => {}
                }
            }
        }
        (out, next)
    }

    /// Signalling the simulator must deliver, oldest first.
    pub fn signals(&mut self) -> impl Iterator<Item = Signal> + '_ {
        self.signals.drain(..)
    }

    fn forward(&mut self, f: &Forward) {
        let Some((room, kind)) = self.participant(f.from).map(|p| {
            let kind = p
                .tracks_in
                .iter()
                .find(|t| t.mid == f.data.mid)
                .map(|t| t.kind);
            (p.room.clone(), kind)
        }) else {
            return;
        };
        for q in self
            .participants
            .iter_mut()
            .filter(|q| q.alive && q.room == room && q.id != f.from && !q.ignores.contains(&f.from))
        {
            // A camera nobody here draws is not sent (shared screens always are).
            if kind == Some(Kind::Video)
                && q.watching.as_ref().is_some_and(|w| !w.contains(&f.from))
            {
                continue;
            }
            let Some(mid) = q
                .tracks_out
                .iter()
                .find(|t| t.from == f.from && t.from_mid == f.data.mid)
                .and_then(|t| match t.state {
                    OutState::Open(m) => Some(m),
                    _ => None,
                })
            else {
                continue;
            };
            let Some(writer) = q.down.writer(mid) else {
                continue;
            };
            let Some(pt) = writer.match_params(f.data.params) else {
                continue;
            };
            if writer
                .write(pt, f.data.network_time, f.data.time, f.data.data.clone())
                .is_err()
            {
                q.alive = false;
            }
        }
    }

    fn request_keyframe(
        &mut self,
        from: ParticipantId,
        mid: Mid,
        kind: KeyframeRequestKind,
        now: Instant,
    ) {
        let Some(p) = self.participant_mut(from) else {
            return;
        };
        let Some(t) = p.tracks_in.iter_mut().find(|t| t.mid == mid) else {
            return;
        };
        if !t.kind.is_video()
            || t.last_keyframe_request
                .is_some_and(|at| now.duration_since(at) < KEYFRAME_THROTTLE)
        {
            return;
        }
        if let Some(mut w) = p.up.writer(mid) {
            if w.request_keyframe(None, kind).is_ok() {
                t.last_keyframe_request = Some(now);
            }
        }
    }

    /// Drop participants whose connections died, and stop their tracks on
    /// everybody else's down connection.
    fn retire_dead(&mut self) {
        let dead: Vec<ParticipantId> = self
            .participants
            .iter()
            .filter(|p| !p.alive)
            .map(|p| p.id)
            .collect();
        if dead.is_empty() {
            return;
        }
        self.participants.retain(|p| p.alive);
        for who in dead {
            self.signals.push_back(Signal::Left { who });
            for q in &mut self.participants {
                q.tracks_out.retain_mut(|t| {
                    if t.from != who {
                        return true;
                    }
                    match t.state {
                        OutState::ToOpen => false,
                        OutState::Negotiating(m) | OutState::Open(m) => {
                            t.state = OutState::ToStop(m);
                            true
                        }
                        OutState::ToStop(_) | OutState::Stopping(_) => true,
                    }
                });
            }
        }
    }

    /// Start an offer on every down connection that has track changes and no
    /// offer already outstanding. The one-offer-at-a-time rule is what keeps a
    /// burst of joins from producing offers that cross.
    fn negotiate(&mut self, now: Instant) {
        let public = self.public;
        for p in self
            .participants
            .iter_mut()
            .filter(|p| p.alive && p.pending.is_none())
        {
            // The down connection is made at its first offer, not at the join:
            // its handshake timer starts when it is created, and somebody alone
            // in a call can wait a minute for the first person to pick up.
            if !p.down_started
                && p.tracks_out
                    .iter()
                    .any(|t| matches!(t.state, OutState::ToOpen))
            {
                match fresh_rtc(public, now) {
                    Ok(rtc) => p.down = rtc,
                    Err(_) => continue,
                }
            }
            // Never renegotiate down to nothing. An offer that stops every
            // track carries no BUNDLE group, and the receiving side refuses it
            // ("a=group missing"), which ended the call for the last person
            // left in it. The departed tracks carry nothing anyway; they are
            // stopped with the next offer, when somebody joins.
            if !p.tracks_out.iter().any(|t| {
                matches!(
                    t.state,
                    OutState::ToOpen | OutState::Negotiating(_) | OutState::Open(_)
                )
            }) {
                continue;
            }
            let mut change = p.down.sdp_api();
            for t in &mut p.tracks_out {
                match t.state {
                    OutState::ToOpen => {
                        let mid = change.add_media(
                            t.kind.into(),
                            Direction::SendOnly,
                            Some(t.from.0.to_string()),
                            None,
                            None,
                        );
                        t.state = OutState::Negotiating(mid);
                    }
                    OutState::ToStop(m) => {
                        change.stop_media(m);
                        t.state = OutState::Stopping(m);
                    }
                    _ => {}
                }
            }
            if !change.has_changes() {
                continue;
            }
            let Some((offer, pending)) = change.apply() else {
                continue;
            };
            p.pending = Some(pending);
            p.down_started = true;
            let tracks = p
                .tracks_out
                .iter()
                .filter(|t| matches!(t.state, OutState::Negotiating(_) | OutState::Open(_)))
                .filter_map(|t| {
                    Some(TrackInfo {
                        mid: t.mid()?.to_string(),
                        from: t.from,
                        kind: t.kind,
                    })
                })
                .collect();
            self.signals.push_back(Signal::Offer {
                to: p.id,
                sdp: offer.to_sdp_string(),
                tracks,
            });
        }
    }

    fn participant(&self, who: ParticipantId) -> Option<&Participant> {
        self.participants.iter().find(|p| p.id == who)
    }

    fn participant_mut(&mut self, who: ParticipantId) -> Option<&mut Participant> {
        self.participants.iter_mut().find(|p| p.id == who)
    }
}

/// An ICE-lite connection with the forwarder's one public candidate.
fn fresh_rtc(public: SocketAddr, now: Instant) -> Result<Rtc, Error> {
    let mut rtc = Rtc::builder().set_ice_lite(true).build(now);
    let candidate = Candidate::host(public, "udp").map_err(rtc_err)?;
    rtc.add_local_candidate(candidate)
        .ok_or_else(|| Error::Rtc("candidate refused".into()))?;
    Ok(rtc)
}

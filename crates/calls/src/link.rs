//! A native device's connection to the forwarder ([`crate::sfu`]): the *up*
//! connection it sends on and the *down* connection it receives on.
//!
//! Sans-IO, like the forwarder. The two connections need **two local sockets**:
//! the forwarder tells its connections apart by the datagram's source address
//! once ICE has settled, and two connections behind one port would be one
//! address to it. The caller binds both and says which one a datagram arrived on.
//!
//! Frames go in and come out *sealed*: this module moves bytes, and the
//! caller seals with [`crate::keys::CallKeys`] before [`Link::send`] and opens
//! after [`Output::Media`]. That keeps it testable without keys and keeps one
//! place — `keys` — that ever sees plaintext.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use str0m::change::{SdpAnswer, SdpOffer, SdpPendingOffer};
use str0m::format::Codec as RtpCodec;
use str0m::media::{Direction, Frequency, MediaKind, MediaTime, Mid};
use str0m::net::{Protocol, Receive};
use str0m::{Candidate, Event, IceConnectionState, Input, Output as RtcOutput, Rtc};

use crate::media::Codec;
use crate::media::{Kind, ParticipantId, TrackInfo};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("unreadable SDP: {0}")]
    Sdp(String),
    #[error("refused: {0}")]
    Rtc(String),
    #[error("no answer was expected")]
    NotWaiting,
    #[error("this call sends no {0:?}")]
    NotSending(Codec),
}

fn rtc_err(e: impl std::fmt::Display) -> Error {
    Error::Rtc(e.to_string())
}

fn rtp_codec(codec: Codec) -> RtpCodec {
    match codec {
        Codec::Opus => RtpCodec::Opus,
        Codec::Vp8 => RtpCodec::Vp8,
        Codec::Vp9 => RtpCodec::Vp9,
    }
}

/// Which of the two connections a datagram belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Leg {
    Up,
    Down,
}

#[derive(Debug)]
pub enum Output {
    Transmit {
        leg: Leg,
        destination: SocketAddr,
        contents: Vec<u8>,
    },
    /// A sealed frame from another participant's microphone, camera or screen.
    Media {
        from: ParticipantId,
        kind: Kind,
        codec: Codec,
        sealed: Vec<u8>,
    },
    /// The up connection is established and [`Link::send`] will be carried.
    Connected,
    /// A connection stopped answering. Not over yet: it has
    /// [`crate::media::RECONNECT_GRACE`] to come back, and a person should be
    /// told it is trying rather than shown a timer running over silence.
    Lost,
    /// Every connection that was lost is answering again.
    Restored,
    /// Somebody cannot decode this device's video and wants a full picture:
    /// the next frame of the camera, or of the shared screen when `screen`,
    /// should be a keyframe.
    KeyframeWanted { screen: bool },
    /// Either connection failed, and why. The call is over for this device;
    /// to carry on, join again (with fresh [`crate::keys::CallKeys`]).
    Failed(String),
}

pub struct Link {
    up: Rtc,
    down: Rtc,
    up_local: SocketAddr,
    down_local: SocketAddr,
    pending_up: Option<SdpPendingOffer>,
    audio: Mid,
    video: Option<Mid>,
    /// A shared screen's track, offered from the start like the camera.
    screen: Option<Mid>,
    /// Down-connection tracks by mid, as the last offer named them.
    tracks: HashMap<Mid, TrackInfo>,
    connected: bool,
    /// Why a connection failed, once one has.
    failed: Option<String>,
    /// When a connection last went `Disconnected` and has not come back. Not
    /// failed yet: see `sfu::RECONNECT_GRACE`, which this mirrors.
    lost_since: Option<Instant>,
    /// Whether the down connection has been offered anything yet. Until it
    /// has it is not driven at all: nobody else is in the call, nothing is
    /// negotiated on it, and its handshake timer would otherwise run out at
    /// ~30 s and end a call that is only waiting to be picked up.
    down_started: bool,
}

impl Link {
    /// Prepare both connections; returns the offer for the up connection, to
    /// send to the forwarder with the join request.
    pub fn new(
        now: Instant,
        up_local: SocketAddr,
        down_local: SocketAddr,
        video: bool,
        screen: bool,
    ) -> Result<(Self, String), Error> {
        crate::install_crypto();
        let mut up = Rtc::new(now);
        up.add_local_candidate(Candidate::host(up_local, "udp").map_err(rtc_err)?)
            .ok_or_else(|| Error::Rtc("candidate refused".into()))?;
        let mut down = Rtc::new(now);
        down.add_local_candidate(Candidate::host(down_local, "udp").map_err(rtc_err)?)
            .ok_or_else(|| Error::Rtc("candidate refused".into()))?;

        let mut change = up.sdp_api();
        let audio = change.add_media(MediaKind::Audio, Direction::SendOnly, None, None, None);
        let video = video
            .then(|| change.add_media(MediaKind::Video, Direction::SendOnly, None, None, None));
        let screen = screen
            .then(|| change.add_media(MediaKind::Video, Direction::SendOnly, None, None, None));
        let (offer, pending) = change
            .apply()
            .ok_or_else(|| Error::Rtc("empty offer".into()))?;
        Ok((
            Self {
                up,
                down,
                up_local,
                down_local,
                pending_up: Some(pending),
                audio,
                video,
                screen,
                tracks: HashMap::new(),
                connected: false,
                failed: None,
                lost_since: None,
                down_started: false,
            },
            offer.to_sdp_string(),
        ))
    }

    /// The forwarder's answer to the join.
    pub fn accept_answer(&mut self, sdp: &str) -> Result<(), Error> {
        let answer = SdpAnswer::from_sdp_string(sdp).map_err(|e| Error::Sdp(e.to_string()))?;
        let pending = self.pending_up.take().ok_or(Error::NotWaiting)?;
        self.up
            .sdp_api()
            .accept_answer(pending, answer)
            .map_err(rtc_err)
    }

    /// The forwarder offers (a new set of) other participants' tracks. Returns
    /// the answer to send back.
    pub fn accept_offer(
        &mut self,
        now: Instant,
        sdp: &str,
        tracks: &[TrackInfo],
    ) -> Result<String, Error> {
        let offer = SdpOffer::from_sdp_string(sdp).map_err(|e| Error::Sdp(e.to_string()))?;
        if !self.down_started {
            // Made now rather than at the join: its handshake timer starts when
            // it is created, and the first offer can come a minute later.
            self.down = Rtc::new(now);
            self.down
                .add_local_candidate(Candidate::host(self.down_local, "udp").map_err(rtc_err)?)
                .ok_or_else(|| Error::Rtc("candidate refused".into()))?;
            self.down_started = true;
        }
        let answer = self.down.sdp_api().accept_offer(offer).map_err(rtc_err)?;
        self.tracks = tracks
            .iter()
            .map(|t| (Mid::from(t.mid.as_str()), t.clone()))
            .collect();
        Ok(answer.to_sdp_string())
    }

    /// Whether the up connection carries media yet.
    #[must_use]
    pub fn is_connected(&self) -> bool {
        self.connected
    }

    /// The media id of the screen track in the join's offer, for
    /// `CallJoin.screen`, so the forwarder can tell it from the camera.
    #[must_use]
    pub fn screen_mid(&self) -> Option<String> {
        self.screen.map(|m| m.to_string())
    }

    /// Send one sealed frame of the microphone or the camera. `time` is the
    /// frame's capture time; RTP timestamps are derived from it at the codec's
    /// clock rate.
    pub fn send(
        &mut self,
        now: Instant,
        codec: Codec,
        time: Duration,
        sealed: Vec<u8>,
    ) -> Result<(), Error> {
        self.send_on(now, codec, false, time, sealed)
    }

    /// [`Link::send`] for a shared screen's picture.
    pub fn send_screen(
        &mut self,
        now: Instant,
        codec: Codec,
        time: Duration,
        sealed: Vec<u8>,
    ) -> Result<(), Error> {
        self.send_on(now, codec, true, time, sealed)
    }

    fn send_on(
        &mut self,
        now: Instant,
        codec: Codec,
        screen: bool,
        time: Duration,
        sealed: Vec<u8>,
    ) -> Result<(), Error> {
        let picture = if screen { self.screen } else { self.video };
        let (mid, want, rate) = match codec {
            Codec::Opus => (self.audio, RtpCodec::Opus, Frequency::FORTY_EIGHT_KHZ),
            Codec::Vp8 | Codec::Vp9 => (
                picture.ok_or(Error::NotSending(codec))?,
                rtp_codec(codec),
                Frequency::NINETY_KHZ,
            ),
        };
        let writer = self.up.writer(mid).ok_or(Error::NotSending(codec))?;
        // VP9 is offered as profile 0 (8-bit 4:2:0, what `video` encodes) and
        // profile 2; a profile-2 payload type would tell the receiver to
        // expect ten bits.
        let pt = writer
            .payload_params()
            .find(|p| p.spec().codec == want && p.spec().format.profile_id.unwrap_or(0) == 0)
            .map(|p| p.pt())
            .ok_or(Error::NotSending(codec))?;
        let ticks = time.as_micros() as u64 * u64::from(rate.get()) / 1_000_000;
        writer
            .write(pt, now, MediaTime::new(ticks, rate), sealed)
            .map_err(rtc_err)
    }

    /// Ask `from` for a full picture of its camera or screen (`kind`): it
    /// stopped decoding (a lost packet took a reference frame with it). Goes to
    /// the forwarder, which passes it on, throttled.
    pub fn request_keyframe(&mut self, from: ParticipantId, kind: Kind) {
        let mids: Vec<Mid> = self
            .tracks
            .iter()
            .filter(|(_, t)| t.from == from && t.kind == kind)
            .map(|(mid, _)| *mid)
            .collect();
        for mid in mids {
            if let Some(mut w) = self.down.writer(mid) {
                let _ = w.request_keyframe(None, str0m::media::KeyframeRequestKind::Pli);
            }
        }
    }

    /// Hand in a datagram that arrived on `leg`'s socket.
    pub fn receive(&mut self, now: Instant, leg: Leg, source: SocketAddr, contents: &[u8]) {
        let Ok(contents) = contents.try_into() else {
            return;
        };
        let (rtc, destination) = match leg {
            Leg::Up => (&mut self.up, self.up_local),
            Leg::Down => (&mut self.down, self.down_local),
        };
        let input = Input::Receive(
            now,
            Receive {
                proto: Protocol::Udp,
                source,
                destination,
                contents,
            },
        );
        if let Err(e) = rtc.handle_input(input) {
            self.failed
                .get_or_insert(format!("{leg:?} connection refused a datagram: {e}"));
        }
    }

    /// Advance both connections to `now`. Returns what to send and deliver, and
    /// when to call again at the latest.
    pub fn poll(&mut self, now: Instant) -> (Vec<Output>, Instant) {
        let mut out = Vec::new();
        let mut next = now + Duration::from_millis(100);
        for leg in [Leg::Up, Leg::Down] {
            if leg == Leg::Down && !self.down_started {
                continue;
            }
            let rtc = match leg {
                Leg::Up => &mut self.up,
                Leg::Down => &mut self.down,
            };
            if let Err(e) = rtc.handle_input(Input::Timeout(now)) {
                self.failed
                    .get_or_insert(format!("{leg:?} connection: {e}"));
            }
            loop {
                match rtc.poll_output() {
                    Ok(RtcOutput::Transmit(t)) => out.push(Output::Transmit {
                        leg,
                        destination: t.destination,
                        contents: t.contents.to_vec(),
                    }),
                    Ok(RtcOutput::Timeout(t)) => {
                        next = next.min(t);
                        break;
                    }
                    Ok(RtcOutput::Event(e)) => match e {
                        Event::KeyframeRequest(r) if leg == Leg::Up => {
                            let screen = self.screen == Some(r.mid);
                            out.push(Output::KeyframeWanted { screen });
                        }
                        Event::Connected if leg == Leg::Up => {
                            self.connected = true;
                            out.push(Output::Connected);
                        }
                        Event::IceConnectionStateChange(IceConnectionState::Disconnected) => {
                            if self.lost_since.is_none() {
                                self.lost_since = Some(now);
                                out.push(Output::Lost);
                            }
                        }
                        Event::IceConnectionStateChange(
                            IceConnectionState::Connected | IceConnectionState::Completed,
                        ) => {
                            if self.lost_since.take().is_some() {
                                out.push(Output::Restored);
                            }
                        }
                        Event::Closed => {
                            self.failed
                                .get_or_insert(format!("{leg:?} connection closed"));
                        }
                        Event::MediaData(d) if leg == Leg::Down => {
                            let codec = match d.params.spec().codec {
                                RtpCodec::Opus => Codec::Opus,
                                RtpCodec::Vp8 => Codec::Vp8,
                                RtpCodec::Vp9 => Codec::Vp9,
                                _ => continue,
                            };
                            let Some((from, kind)) =
                                self.tracks.get(&d.mid).map(|t| (t.from, t.kind))
                            else {
                                continue;
                            };
                            out.push(Output::Media {
                                from,
                                kind,
                                codec,
                                sealed: d.data.to_vec(),
                            });
                        }
                        _ => {}
                    },
                    Err(e) => {
                        self.failed
                            .get_or_insert(format!("{leg:?} connection: {e}"));
                        break;
                    }
                }
            }
        }
        if self
            .lost_since
            .is_some_and(|t| now.duration_since(t) > crate::media::RECONNECT_GRACE)
        {
            self.failed
                .get_or_insert("no connection to the server for 30 s".into());
        }
        if let Some(why) = &self.failed {
            out.push(Output::Failed(why.clone()));
        }
        (out, next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::{CallKeys, BASE_KEY_LEN};
    use crate::sfu::{Sfu, Signal};
    use cartopolis_e2ee::Device;
    use std::net::UdpSocket;
    use uuid::Uuid;

    struct Phone {
        link: Link,
        keys: CallKeys,
        id: ParticipantId,
        up: UdpSocket,
        down: UdpSocket,
        heard: Vec<(ParticipantId, u32, Vec<u8>)>,
    }

    fn socket() -> UdpSocket {
        let s = UdpSocket::bind("127.0.0.1:0").unwrap();
        s.set_nonblocking(true).unwrap();
        s
    }

    /// Three members of one private chat in one call, over real loopback UDP,
    /// through the forwarder with keys from their real MLS group.
    #[test]
    fn three_members_hear_each_other_through_a_forwarder_that_cannot() {
        call_of_three(Duration::ZERO);
    }

    /// The forwarder has been waiting 20 s — past str0m's 15 s, after which it
    /// calls a connection that has seen no check `Disconnected` — before any
    /// client's first check arrives: a phone that answered on a slow frame.
    /// They must still get in (`media::RECONNECT_GRACE`). The forwarder's clock
    /// is simply set ahead; sans-IO is what makes that a test and not a sleep.
    #[test]
    fn a_member_whose_first_check_arrives_late_still_gets_in() {
        call_of_three(Duration::from_secs(20));
    }

    /// The caller is alone while it rings, and a phone can take a while to be
    /// picked up — longer still when it is woken from closed (`push`). Nothing
    /// is negotiated on anybody's receiving connection until a second person
    /// joins, and that must not end the call: it used to, at ~30 s, when the
    /// idle receiving connection's handshake timer ran out on both sides.
    #[test]
    fn a_caller_alone_for_a_minute_is_still_in_the_call() {
        struct Leg2 {
            link: Link,
            id: ParticipantId,
            up: UdpSocket,
            down: UdpSocket,
            heard: usize,
        }
        let sfu_sock = socket();
        let mut sfu = Sfu::new(sfu_sock.local_addr().unwrap());
        let start = Instant::now();
        let room = vec![7u8];
        let join = |sfu: &mut Sfu, now: Instant| {
            let (up, down) = (socket(), socket());
            let (mut link, offer) = Link::new(
                now,
                up.local_addr().unwrap(),
                down.local_addr().unwrap(),
                true,
                false,
            )
            .unwrap();
            let (id, answer) = sfu.join(now, room.clone(), &offer, None).unwrap();
            link.accept_answer(&answer).unwrap();
            Leg2 {
                link,
                id,
                up,
                down,
                heard: 0,
            }
        };
        let mut people = vec![join(&mut sfu, start)];
        let mut buf = vec![0u8; 2000];
        let mut sent = 0u64;
        // Simulated time in 50 ms steps; the datagrams still go over loopback,
        // and each step waits for them so nothing is outrun by the clock.
        let mut heard_while_both = Vec::new();
        for step in 0..1900u64 {
            let now = start + Duration::from_millis(50 * step);
            // A minute in, somebody picks up.
            if step == 1200 {
                people.push(join(&mut sfu, now));
            }
            // Ten seconds later they hang up; the caller stays, alone again.
            if step == 1400 {
                heard_while_both = people.iter().map(|p| p.heard).collect();
                let gone = people.pop().unwrap();
                sfu.leave(gone.id);
            }
            // And somebody else joins: the departed tracks are cleared then.
            if step == 1600 {
                people.push(join(&mut sfu, now));
            }
            let (tx, _) = sfu.poll(now);
            // The waits are for datagrams in flight, so a step that sent none
            // skips them: alone, the caller is quiet for most of the minute.
            let mut sent_any = !tx.is_empty();
            for t in tx {
                let _ = sfu_sock.send_to(&t.contents, t.destination);
            }
            let signals: Vec<Signal> = sfu.signals().collect();
            for s in signals {
                if let Signal::Offer { to, sdp, tracks } = s {
                    let p = people.iter_mut().find(|p| p.id == to).unwrap();
                    let answer = p.link.accept_offer(now, &sdp, &tracks).unwrap();
                    sfu.answer(now, to, &answer).unwrap();
                    sent_any = true;
                }
            }
            if sent_any {
                std::thread::sleep(Duration::from_micros(300));
            }
            sent_any = false;
            for p in &mut people {
                for (leg, sock) in [(Leg::Up, &p.up), (Leg::Down, &p.down)] {
                    while let Ok((n, from)) = sock.recv_from(&mut buf) {
                        p.link.receive(now, leg, from, &buf[..n]);
                    }
                }
                let (outs, _) = p.link.poll(now);
                for o in outs {
                    match o {
                        Output::Transmit {
                            leg,
                            destination,
                            contents,
                        } => {
                            let s = if leg == Leg::Up { &p.up } else { &p.down };
                            let _ = s.send_to(&contents, destination);
                            sent_any = true;
                        }
                        Output::Media { .. } => p.heard += 1,
                        Output::Failed(why) => panic!("failed after {} s: {why}", step / 20),
                        _ => {}
                    }
                }
            }
            // Once both are in, each talks every step.
            if people.len() == 2 && people.iter().all(|p| p.link.is_connected()) {
                for p in &mut people {
                    let _ = p.link.send(
                        now,
                        Codec::Opus,
                        Duration::from_millis(50 * sent),
                        b"frame".to_vec(),
                    );
                }
                sent += 1;
                sent_any = true;
            }
            if sent_any {
                std::thread::sleep(Duration::from_micros(300));
            }
            while let Ok((n, from)) = sfu_sock.recv_from(&mut buf) {
                sfu.receive(now, from, &buf[..n]);
            }
        }
        for heard in heard_while_both {
            assert!(heard > 50, "heard {heard} of {sent}");
        }
        assert_eq!(
            sfu.members(&room).len(),
            2,
            "the caller stayed, and the newcomer got in"
        );
        assert!(
            people[1].heard > 50,
            "the newcomer heard {}",
            people[1].heard
        );
    }

    fn call_of_three(late: Duration) {
        let mut devs: Vec<Device> = [
            "alice@test.example",
            "bob@test.example",
            "carol@test.example",
        ]
        .iter()
        .map(|n| Device::new(n, Uuid::new_v4()).unwrap())
        .collect();
        let g = devs[0].create_group(b"test.example/call-test").unwrap();
        let mut kps = devs[1].key_packages(1).unwrap();
        kps.extend(devs[2].key_packages(1).unwrap());
        let add = devs[0].add(&g, &kps).unwrap();
        devs[0].confirm_pending(&g).unwrap();
        devs[1].join(add.welcome.as_ref().unwrap()).unwrap();
        devs[2].join(add.welcome.as_ref().unwrap()).unwrap();

        let sfu_sock = socket();
        let mut sfu = Sfu::new(sfu_sock.local_addr().unwrap());
        let now = Instant::now();

        let mut phones: Vec<Phone> = devs
            .iter()
            .map(|d| {
                let (up, down) = (socket(), socket());
                let (mut link, offer) = Link::new(
                    now,
                    up.local_addr().unwrap(),
                    down.local_addr().unwrap(),
                    true,
                    false,
                )
                .unwrap();
                let (id, answer) = sfu.join(now, g.clone(), &offer, None).unwrap();
                link.accept_answer(&answer).unwrap();
                let e = d.call_epoch(&g, BASE_KEY_LEN).unwrap();
                let mut keys = CallKeys::new();
                keys.set_epoch(
                    e.epoch,
                    e.base_key,
                    e.own_index,
                    e.members.iter().map(|m| m.0).collect(),
                );
                Phone {
                    link,
                    keys,
                    id,
                    up,
                    down,
                    heard: Vec::new(),
                }
            })
            .collect();
        // The forwarder lives `late` ahead: step it through the gap first, so
        // its connections have timed out waiting before anyone checks in.
        if !late.is_zero() {
            for s in 0..=late.as_secs() {
                let _ = sfu.poll(now + Duration::from_secs(s));
            }
        }
        let alice_index = devs[0].call_epoch(&g, BASE_KEY_LEN).unwrap().own_index;
        let voice = |n: u32| format!("alice's frame {n}").into_bytes();
        let mut plaintext_at_forwarder = false;

        let start = Instant::now();
        let mut sent = 0u32;
        let mut buf = vec![0u8; 2000];
        while start.elapsed() < Duration::from_secs(8) {
            let now = Instant::now();
            // The forwarder's clock; the phones keep real time.
            let fwd = now + late;
            // Forwarder: read, poll, send, deliver signalling.
            while let Ok((n, from)) = sfu_sock.recv_from(&mut buf) {
                if buf[..n].windows(13).any(|w| w == b"alice's frame") {
                    plaintext_at_forwarder = true;
                }
                sfu.receive(fwd, from, &buf[..n]);
            }
            let (tx, _) = sfu.poll(fwd);
            for t in tx {
                let _ = sfu_sock.send_to(&t.contents, t.destination);
            }
            let signals: Vec<Signal> = sfu.signals().collect();
            for s in signals {
                if let Signal::Offer { to, sdp, tracks } = s {
                    let p = phones.iter_mut().find(|p| p.id == to).unwrap();
                    let answer = p.link.accept_offer(now, &sdp, &tracks).unwrap();
                    sfu.answer(fwd, to, &answer).unwrap();
                }
            }
            // Phones.
            for p in &mut phones {
                for (leg, sock) in [(Leg::Up, &p.up), (Leg::Down, &p.down)] {
                    while let Ok((n, from)) = sock.recv_from(&mut buf) {
                        p.link.receive(now, leg, from, &buf[..n]);
                    }
                }
                let (outs, _) = p.link.poll(now);
                for o in outs {
                    match o {
                        Output::Transmit {
                            leg,
                            destination,
                            contents,
                        } => {
                            let s = if leg == Leg::Up { &p.up } else { &p.down };
                            let _ = s.send_to(&contents, destination);
                        }
                        Output::Media {
                            from,
                            codec,
                            sealed,
                            ..
                        } => {
                            let opened = p.keys.open(codec, &sealed).expect("a member opens it");
                            p.heard.push((from, opened.sender, opened.frame));
                        }
                        Output::Failed(why) => panic!("a link failed: {why}"),
                        Output::Connected
                        | Output::KeyframeWanted { .. }
                        | Output::Lost
                        | Output::Restored => {}
                    }
                }
            }
            // Alice talks every 20 ms once everyone is connected.
            if phones.iter().all(|p| p.link.is_connected()) && sent < 100 {
                let a = &mut phones[0];
                let sealed = a.keys.seal(Codec::Opus, &voice(sent)).unwrap();
                a.link
                    .send(
                        now,
                        Codec::Opus,
                        Duration::from_millis(20 * u64::from(sent)),
                        sealed,
                    )
                    .unwrap();
                sent += 1;
            }
            if phones[1..].iter().all(|p| p.heard.len() >= 90) {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }

        assert!(sent > 0, "never connected");
        for p in &phones[1..] {
            assert!(p.heard.len() >= 90, "heard {} of {sent}", p.heard.len());
            for (from, sender, frame) in &p.heard {
                assert_eq!(*from, phones[0].id, "the forwarder names the sender");
                assert_eq!(*sender, alice_index, "and the frame itself agrees");
                assert!(frame.starts_with(b"alice's frame "));
            }
        }
        assert!(phones[0].heard.is_empty(), "nobody hears themselves");
        assert!(
            !plaintext_at_forwarder,
            "the forwarder never saw a plaintext frame"
        );
    }
}

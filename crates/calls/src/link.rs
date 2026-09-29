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
use crate::media::{ParticipantId, TrackInfo};

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
    /// A sealed frame from another participant.
    Media {
        from: ParticipantId,
        codec: Codec,
        sealed: Vec<u8>,
    },
    /// The up connection is established and [`Link::send`] will be carried.
    Connected,
    /// Either connection failed. The call is over for this device; to carry
    /// on, join again (with fresh [`crate::keys::CallKeys`]).
    Failed,
}

pub struct Link {
    up: Rtc,
    down: Rtc,
    up_local: SocketAddr,
    down_local: SocketAddr,
    pending_up: Option<SdpPendingOffer>,
    audio: Mid,
    video: Option<Mid>,
    /// Down-connection tracks by mid, as the last offer named them.
    tracks: HashMap<String, ParticipantId>,
    connected: bool,
    failed: bool,
}

impl Link {
    /// Prepare both connections; returns the offer for the up connection, to
    /// send to the forwarder with the join request.
    pub fn new(
        now: Instant,
        up_local: SocketAddr,
        down_local: SocketAddr,
        video: bool,
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
                tracks: HashMap::new(),
                connected: false,
                failed: false,
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
    pub fn accept_offer(&mut self, sdp: &str, tracks: &[TrackInfo]) -> Result<String, Error> {
        let offer = SdpOffer::from_sdp_string(sdp).map_err(|e| Error::Sdp(e.to_string()))?;
        let answer = self.down.sdp_api().accept_offer(offer).map_err(rtc_err)?;
        self.tracks = tracks.iter().map(|t| (t.mid.clone(), t.from)).collect();
        Ok(answer.to_sdp_string())
    }

    /// Whether the up connection carries media yet.
    #[must_use]
    pub fn is_connected(&self) -> bool {
        self.connected
    }

    /// Send one sealed frame. `time` is the frame's capture time; RTP
    /// timestamps are derived from it at the codec's clock rate.
    pub fn send(
        &mut self,
        now: Instant,
        codec: Codec,
        time: Duration,
        sealed: Vec<u8>,
    ) -> Result<(), Error> {
        let (mid, want, rate) = match codec {
            Codec::Opus => (self.audio, RtpCodec::Opus, Frequency::FORTY_EIGHT_KHZ),
            Codec::Vp8 => (
                self.video.ok_or(Error::NotSending(codec))?,
                RtpCodec::Vp8,
                Frequency::NINETY_KHZ,
            ),
        };
        let writer = self.up.writer(mid).ok_or(Error::NotSending(codec))?;
        let pt = writer
            .payload_params()
            .find(|p| p.spec().codec == want)
            .map(|p| p.pt())
            .ok_or(Error::NotSending(codec))?;
        let ticks = time.as_micros() as u64 * u64::from(rate.get()) / 1_000_000;
        writer
            .write(pt, now, MediaTime::new(ticks, rate), sealed)
            .map_err(rtc_err)
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
        if rtc.handle_input(input).is_err() {
            self.failed = true;
        }
    }

    /// Advance both connections to `now`. Returns what to send and deliver, and
    /// when to call again at the latest.
    pub fn poll(&mut self, now: Instant) -> (Vec<Output>, Instant) {
        let mut out = Vec::new();
        let mut next = now + Duration::from_millis(100);
        for leg in [Leg::Up, Leg::Down] {
            let rtc = match leg {
                Leg::Up => &mut self.up,
                Leg::Down => &mut self.down,
            };
            if rtc.handle_input(Input::Timeout(now)).is_err() {
                self.failed = true;
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
                        Event::Connected if leg == Leg::Up => {
                            self.connected = true;
                            out.push(Output::Connected);
                        }
                        Event::IceConnectionStateChange(IceConnectionState::Disconnected)
                        | Event::Closed => {
                            self.failed = true;
                        }
                        Event::MediaData(d) if leg == Leg::Down => {
                            let codec = match d.params.spec().codec {
                                RtpCodec::Opus => Codec::Opus,
                                RtpCodec::Vp8 => Codec::Vp8,
                                _ => continue,
                            };
                            let Some(&from) = self.tracks.get(&d.mid.to_string()) else {
                                continue;
                            };
                            out.push(Output::Media {
                                from,
                                codec,
                                sealed: d.data.to_vec(),
                            });
                        }
                        _ => {}
                    },
                    Err(_) => {
                        self.failed = true;
                        break;
                    }
                }
            }
        }
        if self.failed {
            out.push(Output::Failed);
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
        let mut devs: Vec<Device> = ["alice", "bob", "carol"]
            .iter()
            .map(|n| Device::new(n, Uuid::new_v4()).unwrap())
            .collect();
        let g = devs[0].create_group().unwrap();
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
                )
                .unwrap();
                let (id, answer) = sfu.join(now, g.clone(), &offer).unwrap();
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
        let alice_index = devs[0].call_epoch(&g, BASE_KEY_LEN).unwrap().own_index;
        let voice = |n: u32| format!("alice's frame {n}").into_bytes();
        let mut plaintext_at_forwarder = false;

        let start = Instant::now();
        let mut sent = 0u32;
        let mut buf = vec![0u8; 2000];
        while start.elapsed() < Duration::from_secs(8) {
            let now = Instant::now();
            // Forwarder: read, poll, send, deliver signalling.
            while let Ok((n, from)) = sfu_sock.recv_from(&mut buf) {
                if buf[..n].windows(13).any(|w| w == b"alice's frame") {
                    plaintext_at_forwarder = true;
                }
                sfu.receive(now, from, &buf[..n]);
            }
            let (tx, _) = sfu.poll(now);
            for t in tx {
                let _ = sfu_sock.send_to(&t.contents, t.destination);
            }
            let signals: Vec<Signal> = sfu.signals().collect();
            for s in signals {
                if let Signal::Offer { to, sdp, tracks } = s {
                    let p = phones.iter_mut().find(|p| p.id == to).unwrap();
                    let answer = p.link.accept_offer(&sdp, &tracks).unwrap();
                    sfu.answer(now, to, &answer).unwrap();
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
                        } => {
                            let opened = p.keys.open(codec, &sealed).expect("a member opens it");
                            p.heard.push((from, opened.sender, opened.frame));
                        }
                        Output::Failed => panic!("a link failed"),
                        Output::Connected => {}
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

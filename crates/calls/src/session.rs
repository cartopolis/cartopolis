//! One device's side of a call, on a thread of its own.
//!
//! [`crate::link::Link`] is sans-IO; something has to own its two sockets and
//! call it every few milliseconds whatever the app's frame rate is doing — a
//! phone rendering at 20 fps must not be the thing that paces DTLS retransmits
//! and a 50 Hz audio stream. That is this thread. It also owns everything a
//! frame passes through on its way in and out: the Opus codec, the SFrame keys,
//! a decoder per person heard. The app hands it microphone frames and chat
//! epochs and takes back decoded audio and the signalling to send; plaintext
//! media never leaves this thread except as PCM.
//!
//! Signalling is the app's to carry (it goes over the simulator connection the
//! app already has): [`Event::Offer`] is what to send in `CallJoin`, and the
//! forwarder's replies come back in through [`Session::joined`] and
//! [`Session::offered`].

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, ToSocketAddrs, UdpSocket};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::keys::CallKeys;
use crate::link::{Leg, Link, Output};
use crate::media::{Codec, ParticipantId, TrackInfo};
use crate::opus;
use crate::video;

/// How long the thread sleeps between turns when nothing arrived. Well under a
/// 20 ms frame, and cheap: a turn with nothing to do is two failed `recv`s.
const IDLE: Duration = Duration::from_millis(2);

/// What the app tells the thread.
enum Cmd {
    Joined(String),
    Offered(String, Vec<TrackInfo>),
    Epoch {
        epoch: u64,
        base_key: Vec<u8>,
        own_index: u32,
        members: Vec<u32>,
    },
    Pcm(Vec<f32>),
    Picture(video::I420),
}

/// What the thread tells the app.
#[derive(Debug)]
pub enum Event {
    /// The offer for this device's sending connection: send it as `CallJoin`.
    Offer(String),
    /// The answer to the forwarder's last offer: send it as `CallAnswer`.
    Answer(String),
    /// Media is flowing out.
    Connected,
    /// 20 ms of somebody's voice, decoded, at the session's rate. `sender` is
    /// their leaf index in the chat, from the frame itself.
    Audio {
        from: ParticipantId,
        sender: u32,
        pcm: Vec<f32>,
    },
    /// One decoded video frame from somebody.
    Video {
        from: ParticipantId,
        frame: video::I420,
    },
    /// The call is over for this device. The text is for a person.
    Failed(String),
}

/// A running call. Dropping it ends the thread and closes both sockets.
///
/// `Sync`, so an app can keep it in shared state (a Bevy resource): the
/// receiving end sits behind a mutex nobody else ever contends.
pub struct Session {
    tx: Option<mpsc::Sender<Cmd>>,
    rx: std::sync::Mutex<mpsc::Receiver<Event>>,
    thread: Option<JoinHandle<()>>,
}

impl Session {
    /// Start a call session towards the simulator at `server` (the host the
    /// app is already connected to; a name is resolved on the thread). `rate`
    /// is the PCM rate frames go in and come out at, one of [`opus::RATES`].
    /// The first event is the [`Event::Offer`] to join with. With `video`
    /// this device sends a camera too ([`Session::send_picture`]); every
    /// device receives video whether it sends any or not.
    #[must_use]
    pub fn start(server: String, rate: u32, video: bool) -> Self {
        let (tx, cmd_rx) = mpsc::channel();
        let (ev_tx, rx) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("call".into())
            .spawn(move || {
                if let Err(e) = run(&server, rate, video, &cmd_rx, &ev_tx) {
                    let _ = ev_tx.send(Event::Failed(e));
                }
            })
            .ok();
        Self {
            tx: Some(tx),
            rx: std::sync::Mutex::new(rx),
            thread,
        }
    }

    fn send(&self, cmd: Cmd) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(cmd);
        }
    }

    /// The forwarder's answer to the join.
    pub fn joined(&self, answer: String) {
        self.send(Cmd::Joined(answer));
    }

    /// The forwarder's offer for the receiving connection.
    pub fn offered(&self, sdp: String, tracks: Vec<TrackInfo>) {
        self.send(Cmd::Offered(sdp, tracks));
    }

    /// The chat's current epoch, from `cartopolis_e2ee::Device::call_epoch`.
    /// Send it before the first frame and again whenever the chat changes.
    pub fn epoch(&self, epoch: u64, base_key: Vec<u8>, own_index: u32, members: Vec<u32>) {
        self.send(Cmd::Epoch {
            epoch,
            base_key,
            own_index,
            members,
        });
    }

    /// One 20 ms frame of microphone audio at the session's rate.
    pub fn send_pcm(&self, pcm: Vec<f32>) {
        self.send(Cmd::Pcm(pcm));
    }

    /// One camera picture, any size (it is scaled to [`video::WIDTH`] ×
    /// [`video::HEIGHT`]). Send at [`video::FPS`]; a picture that arrives while
    /// the encoder is still busy with the last one is dropped, not queued.
    pub fn send_picture(&self, frame: video::I420) {
        self.send(Cmd::Picture(frame));
    }

    /// Whatever the thread has said since last asked.
    pub fn events(&self) -> Vec<Event> {
        self.rx
            .lock()
            .map(|rx| rx.try_iter().collect())
            .unwrap_or_default()
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // Closing the channel is the stop signal; the thread notices on its next
        // turn, a couple of milliseconds away.
        self.tx = None;
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// The address this machine would use to reach `server`: a host candidate has
/// to be a real interface address, and asking the routing table is the one way
/// to know which. `connect` on UDP sends nothing.
fn local_ip_towards(server: IpAddr) -> IpAddr {
    let probe = match server {
        IpAddr::V4(_) => UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)),
        IpAddr::V6(_) => UdpSocket::bind((std::net::Ipv6Addr::UNSPECIFIED, 0)),
    };
    probe
        .and_then(|s| {
            s.connect((server, 9))?;
            s.local_addr()
        })
        .map_or(server, |a| a.ip())
}

fn bind(ip: IpAddr) -> Result<UdpSocket, String> {
    let s = UdpSocket::bind(SocketAddr::new(ip, 0))
        .map_err(|e| format!("no socket for the call: {e}"))?;
    s.set_nonblocking(true).map_err(|e| e.to_string())?;
    Ok(s)
}

/// The video encoder, on a thread of its own: a 320×240 frame is ~40 ms of
/// CPU, and on the session thread that would hold up two voice frames every
/// time. It takes the newest picture (dropping any that queued behind a slow
/// encode — stale video is worth nothing) and hands back VP8 bytes.
struct VideoEncoder {
    tx: Option<mpsc::SyncSender<(video::I420, bool)>>,
    rx: mpsc::Receiver<Vec<u8>>,
    thread: Option<JoinHandle<()>>,
}

impl VideoEncoder {
    fn start() -> Self {
        let (tx, frames) = mpsc::sync_channel::<(video::I420, bool)>(1);
        let (out, rx) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("call-video".into())
            .spawn(move || {
                let mut enc = video::Encoder::new();
                while let Ok((frame, key)) = frames.recv() {
                    if let Ok(bytes) = enc.encode(&frame, key) {
                        if out.send(bytes).is_err() {
                            return;
                        }
                    }
                }
            })
            .ok();
        Self {
            tx: Some(tx),
            rx,
            thread,
        }
    }

    /// Hand a picture over if the encoder is free; drop it if not.
    fn offer(&self, frame: video::I420, key: bool) -> bool {
        self.tx
            .as_ref()
            .is_some_and(|tx| tx.try_send((frame, key)).is_ok())
    }
}

impl Drop for VideoEncoder {
    fn drop(&mut self) {
        self.tx = None;
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn run(
    server: &str,
    rate: u32,
    send_video: bool,
    cmds: &mpsc::Receiver<Cmd>,
    events: &mpsc::Sender<Event>,
) -> Result<(), String> {
    let server_ip = (server, 0)
        .to_socket_addrs()
        .ok()
        .and_then(|mut a| a.next())
        .map(|a| a.ip())
        .ok_or_else(|| format!("cannot find {server}"))?;
    let ip = local_ip_towards(server_ip);
    let (up, down) = (bind(ip)?, bind(ip)?);
    let (mut link, offer) = Link::new(
        Instant::now(),
        up.local_addr().map_err(|e| e.to_string())?,
        down.local_addr().map_err(|e| e.to_string())?,
        send_video,
    )
    .map_err(|e| e.to_string())?;
    events.send(Event::Offer(offer)).map_err(|_| "app gone")?;

    let video_encoder = send_video.then(VideoEncoder::start);
    // A receiver asked for a full picture; the next one sent is a keyframe.
    let mut keyframe_wanted = false;
    let mut sent_pictures: u64 = 0;
    let mut pictures: HashMap<ParticipantId, video::Decoder> = HashMap::new();

    let mut keys = CallKeys::new();
    let mut encoder = opus::Encoder::new(rate).map_err(|e| e.to_string())?;
    let mut decoders: HashMap<ParticipantId, opus::Decoder> = HashMap::new();
    let mut sent_frames: u64 = 0;
    let mut buf = [0u8; 2048];
    loop {
        let now = Instant::now();
        loop {
            match cmds.try_recv() {
                Ok(Cmd::Joined(answer)) => {
                    link.accept_answer(&answer).map_err(|e| e.to_string())?
                }
                Ok(Cmd::Offered(sdp, tracks)) => {
                    let answer = link
                        .accept_offer(&sdp, &tracks)
                        .map_err(|e| e.to_string())?;
                    events.send(Event::Answer(answer)).map_err(|_| "app gone")?;
                }
                Ok(Cmd::Epoch {
                    epoch,
                    base_key,
                    own_index,
                    members,
                }) => keys.set_epoch(epoch, base_key, own_index, members),
                Ok(Cmd::Pcm(pcm)) => {
                    // Nothing goes out before the connection is up: the frame
                    // would be dropped by str0m anyway, and the counter would
                    // run ahead of the stream's clock.
                    if !link.is_connected() {
                        continue;
                    }
                    let Ok(packet) = encoder.encode(&pcm) else {
                        continue;
                    };
                    let Ok(sealed) = keys.seal(Codec::Opus, &packet) else {
                        continue;
                    };
                    let time = Duration::from_millis(20 * sent_frames);
                    sent_frames += 1;
                    let _ = link.send(now, Codec::Opus, time, sealed);
                }
                Ok(Cmd::Picture(frame)) => {
                    if let Some(enc) = &video_encoder {
                        if link.is_connected() && enc.offer(frame, keyframe_wanted) {
                            keyframe_wanted = false;
                        }
                    }
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => return Ok(()),
            }
        }

        // Encoded pictures back from the encoder thread: seal and send.
        if let Some(enc) = &video_encoder {
            for bytes in enc.rx.try_iter() {
                let Ok(sealed) = keys.seal(Codec::Vp8, &bytes) else {
                    continue;
                };
                let time = Duration::from_micros(sent_pictures * 1_000_000 / u64::from(video::FPS));
                sent_pictures += 1;
                let _ = link.send(now, Codec::Vp8, time, sealed);
            }
        }

        let mut busy = false;
        for (leg, sock) in [(Leg::Up, &up), (Leg::Down, &down)] {
            while let Ok((n, from)) = sock.recv_from(&mut buf) {
                link.receive(now, leg, from, &buf[..n]);
                busy = true;
            }
        }
        let (outs, _) = link.poll(now);
        for o in outs {
            match o {
                Output::Transmit {
                    leg,
                    destination,
                    contents,
                } => {
                    let s = if leg == Leg::Up { &up } else { &down };
                    let _ = s.send_to(&contents, destination);
                }
                Output::Media {
                    from,
                    codec,
                    sealed,
                } => {
                    // A frame that does not open is dropped: a member who has
                    // not caught up with the chat's latest epoch, or noise.
                    let Ok(opened) = keys.open(codec, &sealed) else {
                        continue;
                    };
                    if codec == Codec::Vp8 {
                        let dec = pictures.entry(from).or_default();
                        match dec.decode(&opened.frame) {
                            Ok(frame) => {
                                let _ = events.send(Event::Video { from, frame });
                            }
                            // A lost reference frame: nothing decodes until
                            // the next keyframe, so ask for one now.
                            Err(_) => link.request_keyframe(from),
                        }
                        continue;
                    }
                    let dec = match decoders.entry(from) {
                        std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
                        std::collections::hash_map::Entry::Vacant(v) => {
                            v.insert(opus::Decoder::new(rate).map_err(|e| e.to_string())?)
                        }
                    };
                    if let Ok(pcm) = dec.decode(&opened.frame) {
                        let _ = events.send(Event::Audio {
                            from,
                            sender: opened.sender,
                            pcm,
                        });
                    }
                }
                Output::Connected => {
                    let _ = events.send(Event::Connected);
                }
                Output::KeyframeWanted => keyframe_wanted = true,
                Output::Failed(why) => return Err(format!("the call's connection failed: {why}")),
            }
        }
        if !busy {
            std::thread::sleep(IDLE);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::BASE_KEY_LEN;
    use crate::sfu::{Sfu, Signal};
    use cartopolis_e2ee::Device;
    use std::sync::{Arc, Mutex};
    use uuid::Uuid;

    /// Two members' sessions, each on its own thread, through a forwarder on a
    /// third — the shape a real call has, minus the simulator connection, whose
    /// part (carrying SDP) this test plays by hand.
    #[test]
    fn a_tone_spoken_into_one_session_comes_out_of_the_other() {
        let mut alice = Device::new("alice", Uuid::new_v4()).unwrap();
        let mut bob = Device::new("bob", Uuid::new_v4()).unwrap();
        let g = alice.create_group().unwrap();
        let add = alice.add(&g, &bob.key_packages(1).unwrap()).unwrap();
        alice.confirm_pending(&g).unwrap();
        bob.join(add.welcome.as_ref().unwrap()).unwrap();

        let sfu_sock = UdpSocket::bind("127.0.0.1:0").unwrap();
        sfu_sock.set_nonblocking(true).unwrap();
        let sfu = Arc::new(Mutex::new(Sfu::new(sfu_sock.local_addr().unwrap())));
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let signals: Arc<Mutex<Vec<Signal>>> = Arc::default();
        let forwarder = {
            let (sfu, stop, signals) = (sfu.clone(), stop.clone(), signals.clone());
            std::thread::spawn(move || {
                let mut buf = [0u8; 2048];
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    let now = Instant::now();
                    let mut s = sfu.lock().unwrap();
                    while let Ok((n, from)) = sfu_sock.recv_from(&mut buf) {
                        s.receive(now, from, &buf[..n]);
                    }
                    let (out, _) = s.poll(now);
                    for t in out {
                        let _ = sfu_sock.send_to(&t.contents, t.destination);
                    }
                    signals.lock().unwrap().extend(s.signals());
                    drop(s);
                    std::thread::sleep(Duration::from_millis(1));
                }
            })
        };

        let rate = 16_000;
        let sa = Session::start("127.0.0.1".into(), rate, true);
        let sb = Session::start("127.0.0.1".into(), rate, false);
        let mut ids = Vec::new();
        for (session, dev) in [(&sa, &alice), (&sb, &bob)] {
            let offer = loop {
                if let Some(Event::Offer(o)) = session.events().into_iter().next() {
                    break o;
                }
                std::thread::sleep(Duration::from_millis(5));
            };
            let (id, answer) = sfu
                .lock()
                .unwrap()
                .join(Instant::now(), g.clone(), &offer)
                .unwrap();
            ids.push(id);
            session.joined(answer);
            let e = dev.call_epoch(&g, BASE_KEY_LEN).unwrap();
            session.epoch(
                e.epoch,
                e.base_key,
                e.own_index,
                e.members.iter().map(|m| m.0).collect(),
            );
        }

        let frame = opus::frame_samples(rate);
        let mut heard: Vec<f32> = Vec::new();
        let mut seen: Vec<u64> = Vec::new();
        let mut connected = false;
        let mut n = 0usize;
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline && (heard.len() < frame * 60 || seen.len() < 10) {
            for s in signals.lock().unwrap().drain(..) {
                if let Signal::Offer { to, sdp, tracks } = s {
                    let session = if to == ids[0] { &sa } else { &sb };
                    session.offered(sdp, tracks);
                }
            }
            for (i, session) in [&sa, &sb].into_iter().enumerate() {
                for ev in session.events() {
                    match ev {
                        Event::Answer(sdp) => sfu
                            .lock()
                            .unwrap()
                            .answer(Instant::now(), ids[i], &sdp)
                            .unwrap(),
                        Event::Connected if i == 0 => connected = true,
                        Event::Audio { from, pcm, .. } if i == 1 => {
                            assert_eq!(from, ids[0]);
                            heard.extend(pcm);
                        }
                        Event::Video { from, frame } if i == 1 => {
                            assert_eq!(from, ids[0]);
                            seen.extend(video::test_pattern_number(&frame));
                        }
                        Event::Failed(e) => panic!("session {i} failed: {e}"),
                        _ => {}
                    }
                }
            }
            if connected {
                let pcm: Vec<f32> = (0..frame)
                    .map(|k| {
                        let t = (n * frame + k) as f32 / rate as f32;
                        0.5 * (t * 440.0 * std::f32::consts::TAU).sin()
                    })
                    .collect();
                sa.send_pcm(pcm);
                // A picture every other voice frame is 25 fps; the encoder
                // takes what it can and drops the rest, as with a camera.
                if n % 2 == 0 {
                    sa.send_picture(video::test_pattern(n as u64 / 2));
                }
                n += 1;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        forwarder.join().unwrap();

        assert!(
            heard.len() >= frame * 60,
            "bob heard {} samples",
            heard.len()
        );
        let tail = &heard[heard.len() - frame * 20..];
        let rms = (tail.iter().map(|s| s * s).sum::<f32>() / tail.len() as f32).sqrt();
        assert!(
            (rms - 0.354).abs() < 0.06,
            "the tone came through at rms {rms}"
        );
        // Video: pictures arrive, decoded, and in the order they were taken.
        assert!(seen.len() >= 10, "bob saw {} pictures", seen.len());
        assert!(seen.windows(2).all(|w| w[0] < w[1]), "in order: {seen:?}");
    }
}

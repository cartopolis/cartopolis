//! One device's side of a call, on a thread of its own.
//!
//! [`crate::link::Link`] is sans-IO; something has to own its two sockets and
//! call it every few milliseconds whatever the app's frame rate is doing — a
//! phone rendering at 20 fps must not be the thing that paces DTLS retransmits
//! and a 50 Hz audio stream. That is this thread. It also owns everything a
//! frame passes through on its way in and out: the Opus codec, the SFrame keys,
//! a decoder per person heard. The app hands it microphone frames and chat
//! epochs and takes back decoded audio and the signalling to send; plaintext
//! media never leaves this thread except as PCM and pictures ready to draw.
//!
//! Video is the expensive part, and none of it runs here or on the app's main
//! thread: encoding has a thread ([`VideoEncoder`]), and so does decoding
//! ([`VideoDecoder`]), which also converts each picture to RGBA and keeps only
//! the newest one per person ([`Session::take_pictures`]). A queue of decoded
//! pictures was what this used to hand the app, unbounded: an app that
//! stalled (a phone in the background) came back to seconds of 115 KB
//! pictures, every one of them converted on the main thread to draw only the
//! last.
//!
//! Signalling is the app's to carry (it goes over the simulator connection the
//! app already has): [`Event::Offer`] is what to send in `CallJoin`, and the
//! forwarder's replies come back in through [`Session::joined`] and
//! [`Session::offered`].

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, ToSocketAddrs, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::keys::CallKeys;
use crate::link::{Leg, Link, Output};
use crate::media::{Codec, ParticipantId, TrackInfo};
use crate::opus;
use crate::video;

/// The longest the thread sleeps between turns when nothing arrived: under a
/// quarter of a 20 ms voice frame. It sleeps less when str0m has a timer due
/// sooner.
const IDLE: Duration = Duration::from_millis(4);

/// The shortest gap between two keyframe requests this device makes of one
/// sender. Every delta after a lost picture fails to decode until the
/// keyframe lands, which at 15 fps was fifteen requests a second; the
/// forwarder throttles to one a second anyway.
const KEYFRAME_ASK_EVERY: Duration = Duration::from_millis(500);

/// Sealed video frames waiting for the decoder. Every frame must be decoded in
/// order (each refers to the last), so these are queued, not replaced; past
/// this the decoder has fallen behind, the frame is dropped, and the sender is
/// asked for a keyframe to start again from.
const DECODE_QUEUE: usize = 16;

/// Makes a call's video encoder, on the encoder's own thread.
pub type EncoderFactory = Box<dyn FnOnce() -> Box<dyn video::VideoEncode> + Send>;

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
    /// A camera picture and when it was taken.
    Picture(video::I420, Instant),
    /// End the call. Said, not implied by the channel closing: a camera
    /// thread's [`PictureSink`] holds a sender of its own, so the channel stays
    /// open for as long as the camera does.
    Stop,
}

/// What the thread tells the app. Pictures are not events: see
/// [`Session::take_pictures`].
#[derive(Debug)]
pub enum Event {
    /// The offer for this device's sending connection: send it as `CallJoin`.
    Offer(String),
    /// The answer to the forwarder's last offer: send it as `CallAnswer`.
    Answer(String),
    /// Media is flowing out.
    Connected,
    /// The connection stopped answering and is being given a chance to come
    /// back ([`crate::media::RECONNECT_GRACE`]).
    Reconnecting,
    /// It came back.
    Reconnected,
    /// 20 ms of somebody's voice, decoded, at the session's rate. `sender` is
    /// their leaf index in the chat, from the frame itself.
    Audio {
        from: ParticipantId,
        sender: u32,
        pcm: Vec<f32>,
    },
    /// The call is over for this device. The text is for a person.
    Failed(String),
}

/// The newest picture from somebody, decoded and converted, ready to draw.
#[derive(Debug, Clone)]
pub struct Picture {
    pub width: u32,
    pub height: u32,
    /// Opaque RGBA, so premultiplied as it stands.
    pub rgba: Vec<u8>,
    /// Pictures decoded from this person so far, including the ones replaced
    /// before the app took them.
    pub decoded: u64,
    /// The stand-in camera's frame number, when it is that
    /// ([`video::test_pattern_number`]): what a headless test reads.
    pub test_number: Option<u64>,
}

/// Shared between the session and its decoder thread.
#[derive(Default)]
struct Shared {
    /// The newest picture per person; replaced, never queued.
    pictures: Mutex<HashMap<ParticipantId, Picture>>,
    /// Whether anybody is looking: while the call screen is folded away
    /// nothing is decoded at all.
    video_wanted: AtomicBool,
    /// Senders whose stream has a hole in it (a frame dropped before it was
    /// decoded): the decoder skips them until their next keyframe.
    resync: Mutex<HashSet<ParticipantId>>,
}

/// A running call. Dropping it ends the thread and closes both sockets.
///
/// `Sync`, so an app can keep it in shared state (a Bevy resource): the
/// receiving end sits behind a mutex nobody else ever contends.
pub struct Session {
    tx: Option<mpsc::Sender<Cmd>>,
    rx: Mutex<mpsc::Receiver<Event>>,
    shared: Arc<Shared>,
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
        Self::start_with_encoder(
            server,
            rate,
            video,
            Box::new(|| Box::new(video::Encoder::new())),
        )
    }

    /// [`Session::start`], with the video encoder made by `make` (on the
    /// encoder's thread) rather than this crate's software one: a phone's
    /// system codec, behind [`video::WithFallback`].
    #[must_use]
    pub fn start_with_encoder(
        server: String,
        rate: u32,
        video: bool,
        make_encoder: EncoderFactory,
    ) -> Self {
        let (tx, cmd_rx) = mpsc::channel();
        let (ev_tx, rx) = mpsc::channel();
        let shared = Arc::new(Shared {
            video_wanted: AtomicBool::new(true),
            ..Shared::default()
        });
        let thread_shared = Arc::clone(&shared);
        let thread = std::thread::Builder::new()
            .name("call".into())
            .spawn(move || {
                let r = run(
                    &server,
                    rate,
                    video,
                    make_encoder,
                    &cmd_rx,
                    &ev_tx,
                    &thread_shared,
                );
                if let Err(e) = r {
                    let _ = ev_tx.send(Event::Failed(e));
                }
            })
            .ok();
        Self {
            tx: Some(tx),
            rx: Mutex::new(rx),
            shared,
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

    /// One camera picture, any size (it is scaled to the call's size,
    /// [`video::call_size`]). Send at [`video::FPS`]; a picture that arrives
    /// while the encoder is still busy with the last one is dropped, not
    /// queued.
    pub fn send_picture(&self, frame: video::I420) {
        self.send(Cmd::Picture(frame, Instant::now()));
    }

    /// A handle a camera thread can send pictures through directly, so video
    /// runs at the camera's pace rather than the app's frame rate — which on a
    /// slow frame (or a software renderer) would be the video's frame rate.
    #[must_use]
    pub fn picture_sink(&self) -> PictureSink {
        PictureSink(self.tx.clone())
    }

    /// Whatever the thread has said since last asked.
    pub fn events(&self) -> Vec<Event> {
        self.rx
            .lock()
            .map(|rx| rx.try_iter().collect())
            .unwrap_or_default()
    }

    /// The newest picture from each person since last asked.
    pub fn take_pictures(&self) -> Vec<(ParticipantId, Picture)> {
        self.shared
            .pictures
            .lock()
            .map(|mut p| p.drain().collect())
            .unwrap_or_default()
    }

    /// Whether anybody is looking at the call's video. Off, nothing is
    /// decoded; back on, every sender is asked for a keyframe, since what was
    /// skipped is what the next delta refers to.
    pub fn set_video_wanted(&self, wanted: bool) {
        self.shared.video_wanted.store(wanted, Ordering::Relaxed);
    }
}

/// See [`Session::picture_sink`]. Sending after the call ended does nothing.
#[derive(Clone)]
pub struct PictureSink(Option<mpsc::Sender<Cmd>>);

impl PictureSink {
    pub fn send(&self, frame: video::I420) {
        if let Some(tx) = &self.0 {
            let _ = tx.send(Cmd::Picture(frame, Instant::now()));
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // Told to stop, then left to do it: the thread notices on its next turn,
        // a couple of milliseconds away, and closes its sockets as it returns.
        //
        // Closing the channel used to be the stop signal, with a join after it.
        // But a camera's `PictureSink` is a sender too, and it lives on the
        // camera's thread until that thread notices *its* stop flag — so with
        // the camera on, hanging up waited on a channel that never closed and
        // the app froze on the button. Not joining also keeps a hang-up from
        // waiting out a video encode (~200 ms on a phone).
        if let Some(tx) = self.tx.take() {
            let _ = tx.send(Cmd::Stop);
        }
        drop(self.thread.take());
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

/// The video encoder, on a thread of its own: a 320×240 frame is tens of ms
/// of CPU, and on the session thread that would hold up voice frames every
/// time. It takes the newest picture (dropping any that queued behind a slow
/// encode — stale video is worth nothing) and hands back the coded bytes with
/// their codec and capture time.
struct VideoEncoder {
    tx: Option<mpsc::SyncSender<(video::I420, Instant, bool)>>,
    rx: mpsc::Receiver<(Codec, Instant, Vec<u8>)>,
    thread: Option<JoinHandle<()>>,
}

impl VideoEncoder {
    fn start(make: EncoderFactory) -> Self {
        let (tx, frames) = mpsc::sync_channel::<(video::I420, Instant, bool)>(1);
        let (out, rx) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("call-video".into())
            .spawn(move || {
                // Made here, on the thread that uses it: a platform codec is
                // seconds to open on some phones, and that is the call's
                // first picture late, not the call.
                let mut enc = make();
                while let Ok((frame, taken, key)) = frames.recv() {
                    let Ok(coded) = enc.encode(&frame, key) else {
                        continue;
                    };
                    for bytes in coded {
                        if out.send((enc.codec(), taken, bytes)).is_err() {
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
    fn offer(&self, frame: video::I420, taken: Instant, key: bool) -> bool {
        self.tx
            .as_ref()
            .is_some_and(|tx| tx.try_send((frame, taken, key)).is_ok())
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

/// The video decoders, one per sender, on a thread of their own: a keyframe
/// is several ms to decode, and on the session thread it held up the voice
/// frames queued behind it. Converts each picture for display as it goes, so
/// the app's main thread only uploads.
struct VideoDecoder {
    tx: Option<mpsc::SyncSender<(ParticipantId, Codec, Vec<u8>)>>,
    /// Senders whose stream would not decode: ask them for a keyframe.
    broken: mpsc::Receiver<ParticipantId>,
    thread: Option<JoinHandle<()>>,
}

impl VideoDecoder {
    fn start(shared: Arc<Shared>) -> Self {
        let (tx, frames) = mpsc::sync_channel::<(ParticipantId, Codec, Vec<u8>)>(DECODE_QUEUE);
        let (broken_tx, broken) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("call-video-in".into())
            .spawn(move || {
                let mut decoders: HashMap<ParticipantId, (video::Decoder, u64)> = HashMap::new();
                // Waiting for a keyframe: a delta refers to a picture this
                // decoder never saw, and would decode to garbage or fail.
                let mut waiting: HashSet<ParticipantId> = HashSet::new();
                let mut was_wanted = true;
                while let Ok((from, codec, frame)) = frames.recv() {
                    let wanted = shared.video_wanted.load(Ordering::Relaxed);
                    if !wanted {
                        was_wanted = false;
                        continue;
                    }
                    if !was_wanted {
                        // Back from not decoding: every stream has a hole.
                        was_wanted = true;
                        waiting.extend(decoders.keys().copied());
                        for who in decoders.keys() {
                            let _ = broken_tx.send(*who);
                        }
                    }
                    if let Ok(mut gaps) = shared.resync.lock() {
                        waiting.extend(gaps.drain());
                    }
                    let key = codec.is_keyframe(&frame);
                    if waiting.contains(&from) && !key {
                        continue;
                    }
                    waiting.remove(&from);
                    let (dec, decoded) = decoders
                        .entry(from)
                        .or_insert_with(|| (video::Decoder::with_codec(codec), 0));
                    match dec.decode_as(codec, &frame) {
                        Ok(picture) => {
                            *decoded += 1;
                            let p = Picture {
                                width: picture.width,
                                height: picture.height,
                                rgba: picture.to_rgba(),
                                decoded: *decoded,
                                test_number: video::test_pattern_number(&picture),
                            };
                            if let Ok(mut all) = shared.pictures.lock() {
                                all.insert(from, p);
                            }
                        }
                        // A lost reference frame: nothing decodes until the
                        // next keyframe, so skip to it and ask for one.
                        Err(_) => {
                            waiting.insert(from);
                            if broken_tx.send(from).is_err() {
                                return;
                            }
                        }
                    }
                }
            })
            .ok();
        Self {
            tx: Some(tx),
            broken,
            thread,
        }
    }

    /// Queue a frame for decoding. `false` if the queue was full and it was
    /// dropped: the stream now has a hole, and needs a keyframe.
    fn offer(&self, from: ParticipantId, codec: Codec, frame: Vec<u8>) -> bool {
        self.tx
            .as_ref()
            .is_some_and(|tx| tx.try_send((from, codec, frame)).is_ok())
    }
}

impl Drop for VideoDecoder {
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
    make_encoder: EncoderFactory,
    cmds: &mpsc::Receiver<Cmd>,
    events: &mpsc::Sender<Event>,
    shared: &Arc<Shared>,
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

    let video_encoder = send_video.then(|| VideoEncoder::start(make_encoder));
    let video_decoder = VideoDecoder::start(Arc::clone(shared));
    // A receiver asked for a full picture; the next one sent is a keyframe.
    let mut keyframe_wanted = false;
    // The capture time of the first picture sent: video's RTP clock runs from
    // it, so a picture the busy encoder dropped leaves a gap in time rather
    // than pulling every later picture earlier.
    let mut first_picture: Option<Instant> = None;
    let mut asked_keyframe: HashMap<ParticipantId, Instant> = HashMap::new();

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
                        .accept_offer(now, &sdp, &tracks)
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
                Ok(Cmd::Picture(frame, taken)) => {
                    if let Some(enc) = &video_encoder {
                        if link.is_connected() && enc.offer(frame, taken, keyframe_wanted) {
                            keyframe_wanted = false;
                        }
                    }
                }
                Ok(Cmd::Stop) | Err(mpsc::TryRecvError::Disconnected) => return Ok(()),
                Err(mpsc::TryRecvError::Empty) => break,
            }
        }

        // Encoded pictures back from the encoder thread: seal and send.
        if let Some(enc) = &video_encoder {
            for (codec, taken, bytes) in enc.rx.try_iter() {
                let Ok(sealed) = keys.seal(codec, &bytes) else {
                    continue;
                };
                let first = *first_picture.get_or_insert(taken);
                let time = taken.saturating_duration_since(first);
                let _ = link.send(now, codec, time, sealed);
            }
        }

        // Streams the decoder could not follow: ask for a fresh start, though
        // not on every failed delta.
        for from in video_decoder.broken.try_iter() {
            let due = asked_keyframe
                .get(&from)
                .is_none_or(|at| now.duration_since(*at) >= KEYFRAME_ASK_EVERY);
            if due {
                asked_keyframe.insert(from, now);
                link.request_keyframe(from);
            }
        }

        let mut busy = false;
        for (leg, sock) in [(Leg::Up, &up), (Leg::Down, &down)] {
            while let Ok((n, from)) = sock.recv_from(&mut buf) {
                link.receive(now, leg, from, &buf[..n]);
                busy = true;
            }
        }
        let (outs, next) = link.poll(now);
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
                    if codec.is_video() {
                        if !video_decoder.offer(from, codec, opened.frame) {
                            if let Ok(mut gaps) = shared.resync.lock() {
                                gaps.insert(from);
                            }
                            if asked_keyframe
                                .get(&from)
                                .is_none_or(|at| now.duration_since(*at) >= KEYFRAME_ASK_EVERY)
                            {
                                asked_keyframe.insert(from, now);
                                link.request_keyframe(from);
                            }
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
                Output::Lost => {
                    let _ = events.send(Event::Reconnecting);
                }
                Output::Restored => {
                    let _ = events.send(Event::Reconnected);
                }
                Output::KeyframeWanted => keyframe_wanted = true,
                Output::Failed(why) => return Err(format!("the call's connection failed: {why}")),
            }
        }
        if !busy {
            let wait = next.saturating_duration_since(Instant::now());
            std::thread::sleep(wait.clamp(Duration::from_millis(1), IDLE));
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

    /// Hanging up with the camera on: the camera thread still holds a sender
    /// when the session is dropped, and the session thread must end anyway.
    /// It used to wait for the channel to close, and the app waited on it.
    #[test]
    fn a_session_ends_while_a_camera_still_holds_its_sink() {
        let mut s = Session::start("127.0.0.1".into(), 16_000, true);
        let sink = s.picture_sink();
        let thread = s.thread.take().expect("the call thread started");
        let (done, ended) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = thread.join();
            let _ = done.send(());
        });
        drop(s);
        assert!(
            ended.recv_timeout(Duration::from_secs(5)).is_ok(),
            "the call thread outlived its session"
        );
        drop(sink);
    }

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
                        Event::Failed(e) => panic!("session {i} failed: {e}"),
                        _ => {}
                    }
                }
            }
            for (from, picture) in sb.take_pictures() {
                assert_eq!(from, ids[0]);
                assert_eq!(
                    picture.rgba.len(),
                    (picture.width * picture.height * 4) as usize
                );
                seen.extend(picture.test_number);
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

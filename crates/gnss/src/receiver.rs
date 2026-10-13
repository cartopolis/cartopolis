//! The receiver loop: read the port, assemble solutions, and write the caster's
//! corrections back into it — on two threads, so that neither a quiet caster
//! nor an unplugged cable stalls the other.
//!
//! **The port thread owns the device.** It opens the port through the caller's
//! [`Opener`] (a tty on the desktop, the USB host API on Android — this crate
//! does not know which), asks the receiver for the sentences it needs, and
//! then reads until the port fails, at which point it opens it again: a cable
//! pulled out and pushed back in is the normal life of a handheld receiver,
//! and nobody should have to toggle a setting afterwards.
//!
//! **The caster thread owns the corrections.** It is started only when a
//! caster is configured, connects (choosing the nearest mountpoint once there
//! is a position to measure from), and hands the bytes to the port thread over
//! a bounded channel. While the port is down they are dropped: corrections are
//! only worth anything fresh, and a backlog replayed into a receiver that has
//! just come back would be seconds-old observations it has to discard.
//!
//! Everything either thread learns goes into one [`Status`], which the client
//! clones out for its fix and its settings readout.
//!
//! **Nothing can reach the receiver that could change it for good.** A Unicore
//! receiver takes commands on the port it reports on, including `FRESET`,
//! `SAVECONFIG` and baud and mode changes, so every write goes through one of
//! two gates in this file, each checking every byte it is handed:
//!
//! * [`allowed_command`] — the three output-rate commands of
//!   [`configure_commands`] at four rates, and nothing else. None is saved:
//!   unplugged, the receiver is as it was.
//! * [`rtcm::checked_frame`] — one whole CRC-checked frame of a standard RTCM
//!   message. Whatever else a caster sends is dropped at the framer and refused
//!   again here.
//!
//! And neither gate opens until the receiver has said something checksummed at
//! this baud: at the wrong rate a command arrives as noise, and noise is not
//! something to send a device on purpose.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, Receiver as Rx, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::nmea::{self, FixQuality, Sentence};
use crate::ntrip::{self, Caster, Session, GGA_INTERVAL};
use crate::rtcm;
use crate::solution::{Assembler, Solution, Tracking};

/// A byte stream to a receiver.
pub trait Port: Send {
    /// Read what has arrived, waiting briefly. `Ok(0)` means nothing yet;
    /// `Err` means the port is gone and should be opened again.
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, String>;
    /// Write all of `data`.
    fn write(&mut self, data: &[u8]) -> Result<(), String>;
}

/// Opens the port: the port, and how to name it to a person.
pub type Opener = Box<dyn FnMut() -> Result<(Box<dyn Port>, String), String> + Send>;

/// How the receiver should be run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    /// Ask the receiver for GGA, RMC and GST at [`Self::rate_hz`] on this port
    /// ([`configure_commands`]). Not saved into the receiver: unplugged, it
    /// goes back to whatever it was configured for.
    pub configure: bool,
    pub rate_hz: u32,
    pub caster: Option<Caster>,
}

/// The port side, as a person would want it described.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Link {
    #[default]
    Opening,
    /// Open, named.
    Open(String),
    /// Could not open or lost — why. Retried on its own.
    Failed(String),
}

/// The caster side.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum CasterState {
    /// No caster configured.
    #[default]
    Off,
    /// The mountpoint is to be chosen by distance, and there is no position yet.
    WaitingForPosition,
    Connecting,
    Streaming,
    /// Why the last attempt failed. Retried on its own, backing off.
    Failed(String),
}

/// What the corrections are doing.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Corrections {
    pub state: CasterState,
    pub mountpoint: Option<String>,
    /// To the chosen base, when it was chosen by distance.
    pub distance_km: Option<f64>,
    /// The base was chosen from the caller's position hint rather than the
    /// receiver's own fix (see [`Receiver::set_position_hint`]).
    pub chosen_from_hint: bool,
    pub bytes: u64,
    /// Valid RTCM frames, and the message numbers seen with their counts.
    pub frames: u64,
    /// Valid frames not passed to the receiver: a proprietary message number
    /// (`rtcm::forwardable`).
    pub held_back: u64,
    pub messages: Vec<(u16, u64)>,
    pub last_at: Option<Instant>,
}

/// Everything known about the receiver right now.
#[derive(Clone, Debug, Default)]
pub struct Status {
    pub link: Link,
    pub solution: Option<Solution>,
    /// When [`Self::solution`] arrived, by this machine's clock.
    pub solution_at: Option<Instant>,
    /// The last GGA's satellites and quality, with or without a position.
    pub tracking: Tracking,
    /// Seconds between the last two solutions by the receiver's own clock —
    /// the rate it is really delivering, whatever it was asked for.
    pub epoch_s: Option<f64>,
    pub bytes_in: u64,
    pub sentences: u64,
    pub bad_checksums: u64,
    pub last_byte_at: Option<Instant>,
    /// The receiver's last answer to a command.
    pub last_reply: Option<String>,
    pub corrections: Corrections,
}

impl Status {
    /// The solution, if it arrived within `max_age`.
    #[must_use]
    pub fn fresh_solution(&self, max_age: Duration) -> Option<&Solution> {
        let at = self.solution_at?;
        (at.elapsed() <= max_age).then_some(self.solution.as_ref()?)
    }
}

/// A running receiver. Dropping it stops both threads (they notice within a
/// read timeout; nothing is joined, so dropping never blocks the caller).
pub struct Receiver {
    cancel: Arc<AtomicBool>,
    status: Arc<Mutex<Status>>,
    hint: Arc<Mutex<Option<(f64, f64)>>>,
}

/// A base further than this is not chosen automatically: single-base RTK
/// degrades with distance and is past fixing reliably well before here, and a
/// caster's "nearest" from outside its country can be hundreds of kilometres
/// away — connecting to it would look like corrections and fix nothing.
pub const MAX_STATION_KM: f64 = 100.0;

/// How a caster session ended without an error.
enum End {
    /// Cancelled, or the receiver side went away.
    Stopped,
    /// The receiver's own fix names a nearer base than the hint did.
    Rechoose,
}

impl Receiver {
    #[must_use]
    pub fn start(opener: Opener, config: Config) -> Self {
        let cancel = Arc::new(AtomicBool::new(false));
        let status = Arc::new(Mutex::new(Status::default()));
        // The caster's position, written by the port thread: the newest
        // solution, and its GGA rebuilt for upload.
        let position: Arc<Mutex<Option<(Solution, String)>>> = Arc::new(Mutex::new(None));
        let hint: Arc<Mutex<Option<(f64, f64)>>> = Arc::new(Mutex::new(None));
        // ~64 reads' worth of corrections; more than that is a port that is
        // not draining, and dropping is right (see the module note).
        let (tx, rx) = sync_channel::<Vec<u8>>(64);

        if let Some(caster) = config.caster.clone() {
            let (cancel, status, position, hint) = (
                cancel.clone(),
                status.clone(),
                position.clone(),
                hint.clone(),
            );
            spawn("gnss-caster", move || {
                caster_thread(&caster, &tx, &position, &hint, &status, &cancel);
            });
        } else {
            drop(tx);
        }
        {
            let (cancel, status) = (cancel.clone(), status.clone());
            spawn("gnss-port", move || {
                port_thread(opener, &config, &rx, &position, &status, &cancel);
            });
        }
        Self {
            cancel,
            status,
            hint,
        }
    }

    /// Roughly where the receiver is, from somewhere else — the map, the
    /// phone's own GPS. Used only to choose the nearest base when the receiver
    /// has no fix of its own yet: indoors that can be never, and the base's
    /// corrections are worth having flowing before the sky is.
    ///
    /// Never sent to the caster: the GGA uploaded is always the receiver's.
    pub fn set_position_hint(&self, hint: Option<(f64, f64)>) {
        if let Ok(mut h) = self.hint.lock() {
            *h = hint;
        }
    }

    /// A copy of the current status.
    #[must_use]
    pub fn status(&self) -> Status {
        self.status.lock().map(|s| s.clone()).unwrap_or_default()
    }
}

impl Drop for Receiver {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

fn spawn(name: &str, f: impl FnOnce() + Send + 'static) {
    if let Err(e) = std::thread::Builder::new().name(name.into()).spawn(f) {
        tracing::error!(error = %e, thread = name, "could not start a GNSS thread");
    }
}

/// Sleep up to `total`, returning early (with `true`) once cancelled.
fn nap(cancel: &AtomicBool, total: Duration) -> bool {
    let until = Instant::now() + total;
    while Instant::now() < until {
        if cancel.load(Ordering::Relaxed) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    cancel.load(Ordering::Relaxed)
}

fn update(status: &Mutex<Status>, f: impl FnOnce(&mut Status)) {
    if let Ok(mut s) = status.lock() {
        f(&mut s);
    }
}

/// The commands that make a Unicore receiver (UM980, UM982, UM960) write what
/// a [`Solution`] needs on the port they arrive on. The port name is left out
/// on purpose: Unicore applies a log command without one to the port it came
/// in on, which is the only port this client knows — the board's USB bridge
/// may be wired to COM1, COM2 or COM3 and nothing on this side can tell which.
#[must_use]
///
/// The rate is clamped to the five Settings offers, 20 Hz at most — the
/// UM980's own position rate — because `user.json` can be edited by hand.
/// [`rate_for_baud`] is what keeps it inside what the line carries.
pub fn configure_commands(rate_hz: u32) -> Vec<String> {
    let period = match rate_hz {
        0 | 1 => "1",
        2 | 3 => "0.5",
        4..=7 => "0.2",
        8..=14 => "0.1",
        _ => "0.05",
    };
    COMMAND_LOGS
        .iter()
        .map(|log| format!("{log} {period}\r\n"))
        .collect()
}

/// The fastest of the offered rates that `baud` carries, at most `rate_hz`.
///
/// Nothing a baud rate can do harms the receiver — it is this side's setting,
/// never sent there — but a rate the line cannot carry makes the receiver drop
/// sentences. Budgeted at 80 % of the line: each epoch's GGA + RMC + GST is
/// ~250 bytes, and ~1.5 kB/s is left for what the receiver already sends on
/// its own (the UM980 writes GSV, GSA and VTG at 1 Hz out of the box). So
/// 115200 carries 20 Hz, 57600 10, 38400 5 and 9600 only 1.
#[must_use]
pub fn rate_for_baud(rate_hz: u32, baud: u32) -> u32 {
    let budget = (baud / 10) * 8 / 10;
    let room = budget.saturating_sub(1_500) / 250;
    let fits = [20, 10, 5, 2, 1]
        .into_iter()
        .find(|&r| r <= room)
        .unwrap_or(1);
    rate_hz.clamp(1, 20).min(fits)
}

/// The only logs this client asks a receiver for, and the only periods.
const COMMAND_LOGS: [&str; 3] = ["GPGGA", "GPRMC", "GPGST"];
const COMMAND_PERIODS: [&str; 5] = ["1", "0.5", "0.2", "0.1", "0.05"];

/// Whether `command` is one this client may send a receiver: exactly one of
/// [`COMMAND_LOGS`] at one of [`COMMAND_PERIODS`], one line. The gate every
/// command passes on its way to the port (module note).
#[must_use]
pub fn allowed_command(command: &str) -> bool {
    let Some(line) = command.strip_suffix("\r\n") else {
        return false;
    };
    let Some((log, period)) = line.split_once(' ') else {
        return false;
    };
    COMMAND_LOGS.contains(&log) && COMMAND_PERIODS.contains(&period)
}

/// Send a command, if it passes [`allowed_command`].
fn send_command(port: &mut dyn Port, command: &str) -> Result<(), String> {
    if !allowed_command(command) {
        tracing::error!(command = %command.escape_debug(), "refused to send a receiver command");
        return Ok(());
    }
    port.write(command.as_bytes())
}

/// Send a correction frame, if it passes [`rtcm::checked_frame`].
fn send_frame(port: &mut dyn Port, frame: &[u8]) -> Result<(), String> {
    if rtcm::checked_frame(frame).is_none() {
        tracing::error!(
            len = frame.len(),
            "refused to send a correction that is not one valid frame"
        );
        return Ok(());
    }
    port.write(frame)
}

fn port_thread(
    mut opener: Opener,
    config: &Config,
    corrections: &Rx<Vec<u8>>,
    position: &Mutex<Option<(Solution, String)>>,
    status: &Mutex<Status>,
    cancel: &AtomicBool,
) {
    let mut logged_failure: Option<String> = None;
    while !cancel.load(Ordering::Relaxed) {
        // A failure stands until an open succeeds: flipping it back to
        // "opening" on every retry would make an unplugged receiver blink
        // between an error and a search in every readout.
        update(status, |s| {
            if !matches!(s.link, Link::Failed(_)) {
                s.link = Link::Opening;
            }
        });
        let (mut port, name) = match opener() {
            Ok(p) => p,
            Err(e) => {
                if logged_failure.as_ref() != Some(&e) {
                    tracing::warn!(error = %e, "GNSS receiver not available");
                    logged_failure = Some(e.clone());
                }
                update(status, |s| s.link = Link::Failed(e));
                if nap(cancel, Duration::from_secs(2)) {
                    return;
                }
                continue;
            }
        };
        logged_failure = None;
        tracing::info!(port = %name, "GNSS receiver open");
        update(status, |s| s.link = Link::Open(name));
        let result = read_port(port.as_mut(), config, corrections, position, status, cancel);
        drop(port);
        match result {
            Ok(()) => return,
            Err(e) => {
                tracing::warn!(error = %e, "GNSS receiver lost");
                update(status, |s| s.link = Link::Failed(e));
                if nap(cancel, Duration::from_secs(1)) {
                    return;
                }
            }
        }
    }
}

/// Read until cancelled (`Ok`) or the port fails (`Err`).
fn read_port(
    port: &mut dyn Port,
    config: &Config,
    corrections: &Rx<Vec<u8>>,
    position: &Mutex<Option<(Solution, String)>>,
    status: &Mutex<Status>,
    cancel: &AtomicBool,
) -> Result<(), String> {
    // Corrections queued while the port was down are stale (module note).
    while corrections.try_recv().is_ok() {}

    let mut framer = nmea::Framer::default();
    let mut assembler = Assembler::default();
    let mut buf = [0u8; 4096];
    // Whether the receiver has said anything checksummed at this baud yet.
    // Until it has, nothing is written to it (module note).
    let mut heard = false;
    while !cancel.load(Ordering::Relaxed) {
        if heard {
            while let Ok(frame) = corrections.try_recv() {
                send_frame(port, &frame)?;
            }
        } else {
            while corrections.try_recv().is_ok() {}
        }
        let n = port.read(&mut buf)?;
        if n == 0 {
            continue;
        }
        let mut sentences = 0u64;
        let mut reply = None;
        let mut solution = None;
        framer.push(&buf[..n], |body| {
            sentences += 1;
            let sentence = nmea::parse(body);
            if let Sentence::Reply(r) = &sentence {
                reply = Some(r.clone());
            }
            if let Some(s) = assembler.push(sentence, body) {
                solution = Some(s);
            }
        });
        if let Some(r) = &reply {
            tracing::info!(reply = %r, "GNSS receiver answered");
        }
        if !heard && sentences > 0 {
            heard = true;
            if config.configure {
                for command in configure_commands(config.rate_hz) {
                    send_command(port, &command)?;
                    // Unicore parses one command at a time; give it the gap a
                    // person typing them would.
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
        }
        if let Some(s) = &solution {
            if let Ok(mut p) = position.lock() {
                *p = Some((s.clone(), gga_for_caster(s)));
            }
        }
        let now = Instant::now();
        update(status, |st| {
            st.bytes_in += n as u64;
            st.last_byte_at = Some(now);
            st.sentences += sentences;
            st.bad_checksums = framer.bad_checksums;
            st.tracking = assembler.tracking.clone();
            if reply.is_some() {
                st.last_reply = reply;
            }
            if let Some(s) = solution {
                let prev = st.solution.as_ref().and_then(|p| p.utc_s);
                if let (Some(a), Some(b)) = (prev, s.utc_s) {
                    let d = (b - a).rem_euclid(86_400.0);
                    if d > 0.0 && d < 5.0 {
                        st.epoch_s = Some(d);
                    }
                }
                st.solution = Some(s);
                st.solution_at = Some(now);
            }
        });
    }
    Ok(())
}

/// A compact `$GPGGA` for the caster, rebuilt from the solution.
///
/// Not the receiver's own sentence passed on: the UM980 writes eight decimals
/// of minutes and a `GN` talker, which takes the line past NMEA's 82 bytes, and
/// older network casters refuse both. Seven decimals is a centimetre — far
/// finer than a caster needs to place a virtual base.
#[must_use]
pub fn gga_for_caster(s: &Solution) -> String {
    let time = s.utc_s.map_or(String::new(), |t| {
        let t = t.rem_euclid(86_400.0);
        let h = (t / 3600.0).floor();
        let m = ((t - h * 3600.0) / 60.0).floor();
        let sec = t - h * 3600.0 - m * 60.0;
        format!("{:02}{:02}{:05.2}", h as u32, m as u32, sec)
    });
    let q = match s.quality {
        FixQuality::None => 0,
        FixQuality::Single => 1,
        FixQuality::Dgps => 2,
        FixQuality::RtkFixed => 4,
        FixQuality::RtkFloat => 5,
        FixQuality::Estimated => 6,
    };
    let opt = |v: Option<f64>, digits: usize| v.map_or(String::new(), |v| format!("{v:.digits$}"));
    let body = format!(
        "GPGGA,{time},{},{},{q},{:02},{},{},M,{},M,,",
        nmea::format_coordinate(s.lat, 2, 'N', 'S'),
        nmea::format_coordinate(s.lng, 3, 'E', 'W'),
        s.satellites.unwrap_or(0).min(99),
        opt(s.hdop, 1),
        opt(s.altitude_m, 2),
        opt(s.geoid_separation_m, 2),
    );
    nmea::sentence(&body)
}

/// How long a stream may go without a byte before it is reconnected.
const CASTER_SILENCE: Duration = Duration::from_secs(30);

fn caster_thread(
    caster: &Caster,
    tx: &SyncSender<Vec<u8>>,
    position: &Mutex<Option<(Solution, String)>>,
    hint: &Mutex<Option<(f64, f64)>>,
    status: &Mutex<Status>,
    cancel: &AtomicBool,
) {
    let mut backoff = Duration::from_secs(3);
    while !cancel.load(Ordering::Relaxed) {
        let started = Instant::now();
        let e = match caster_session(caster, tx, position, hint, status, cancel) {
            Ok(End::Stopped) => return,
            Ok(End::Rechoose) => {
                backoff = Duration::from_secs(3);
                continue;
            }
            Err(e) => e,
        };
        tracing::warn!(error = %e, caster = %caster.host, "NTRIP caster");
        update(status, |s| s.corrections.state = CasterState::Failed(e));
        // A session that streamed for a while was a working configuration that
        // dropped; start the backoff over.
        if started.elapsed() > Duration::from_secs(60) {
            backoff = Duration::from_secs(3);
        }
        if nap(cancel, backoff) {
            return;
        }
        backoff = (backoff * 2).min(Duration::from_secs(60));
    }
}

/// One connection, from choosing the mountpoint to the stream ending.
fn caster_session(
    caster: &Caster,
    tx: &SyncSender<Vec<u8>>,
    position: &Mutex<Option<(Solution, String)>>,
    hint: &Mutex<Option<(f64, f64)>>,
    status: &Mutex<Status>,
    cancel: &AtomicBool,
) -> Result<End, String> {
    let current = || position.lock().ok().and_then(|p| p.clone());
    let hinted = || hint.lock().ok().and_then(|h| *h);
    let mut send_position = caster.send_position;
    let logged_in = !caster.username.is_empty();
    // Set while the base was chosen from the hint: the receiver's first fix
    // then gets a say (see the stream loop).
    let mut table: Option<(Vec<ntrip::Mount>, String)> = None;
    let mount = if caster.mountpoint.trim().is_empty() {
        update(status, |s| {
            s.corrections.state = CasterState::WaitingForPosition
        });
        // The receiver's own fix when there is one, the hint otherwise.
        let (lat, lng, from_hint) = loop {
            if let Some((s, _)) = current() {
                break (s.lat, s.lng, false);
            }
            if let Some((lat, lng)) = hinted() {
                break (lat, lng, true);
            }
            if nap(cancel, Duration::from_millis(500)) {
                return Ok(End::Stopped);
            }
        };
        update(status, |s| s.corrections.state = CasterState::Connecting);
        let mounts = ntrip::sourcetable(caster)?;
        let (m, km) = ntrip::nearest(&mounts, lat, lng, logged_in).ok_or_else(|| {
            if logged_in {
                format!(
                    "the caster lists no RTCM 3 station ({} streams)",
                    mounts.len()
                )
            } else {
                format!(
                    "the caster lists no RTCM 3 station open without a login ({} streams)",
                    mounts.len()
                )
            }
        })?;
        if km > MAX_STATION_KM {
            return Err(format!(
                "the nearest station ({}) is {km:.0} km away — too far for RTK",
                m.name
            ));
        }
        tracing::info!(mountpoint = %m.name, km, from_hint, "nearest NTRIP station");
        // A network stream cannot work without the rover's position, so asking
        // for one is consent enough to send it there.
        send_position |= m.wants_position;
        let (name, km) = (m.name.clone(), km);
        update(status, |s| {
            s.corrections.mountpoint = Some(name.clone());
            s.corrections.distance_km = Some(km);
            s.corrections.chosen_from_hint = from_hint;
        });
        if from_hint {
            table = Some((mounts, name.clone()));
        }
        name
    } else {
        let name = caster.mountpoint.trim().to_string();
        update(status, |s| {
            s.corrections.mountpoint = Some(name.clone());
            s.corrections.distance_km = None;
            s.corrections.chosen_from_hint = false;
        });
        name
    };

    update(status, |s| s.corrections.state = CasterState::Connecting);
    let mut session = Session::connect(caster, &mount)?;
    tracing::info!(mountpoint = %mount, "NTRIP stream open");
    let mut last_gga: Option<Instant> = None;
    let mut last_data = Instant::now();
    let mut framer = rtcm::Framer::default();
    let mut buf = [0u8; 4096];
    let mut streaming = false;
    while !cancel.load(Ordering::Relaxed) {
        // The base came from the hint and the receiver now knows better: keep
        // it if it is still the nearest, move if it is not.
        if let (Some((mounts, chosen)), Some((fix, _))) = (&table, current()) {
            let better = ntrip::nearest(mounts, fix.lat, fix.lng, logged_in)
                .filter(|(m, km)| m.name != *chosen && *km <= MAX_STATION_KM);
            if let Some((m, km)) = better {
                tracing::info!(from = %chosen, to = %m.name, km, "a nearer NTRIP station for the receiver's fix");
                return Ok(End::Rechoose);
            }
            table = None;
            update(status, |s| s.corrections.chosen_from_hint = false);
        }
        if send_position && last_gga.is_none_or(|t| t.elapsed() >= GGA_INTERVAL) {
            if let Some((_, gga)) = current() {
                session.send_gga(&gga)?;
                last_gga = Some(Instant::now());
            }
        }
        let n = session.read(&mut buf)?;
        if n == 0 {
            if last_data.elapsed() > CASTER_SILENCE {
                return Err(format!(
                    "no corrections from {mount} for {} s",
                    CASTER_SILENCE.as_secs()
                ));
            }
            continue;
        }
        last_data = Instant::now();
        // Whole valid frames only, one per message, and only standard ones
        // (`rtcm::forwardable`): see the module note for why the raw bytes are
        // never passed on.
        let mut seen: Vec<u16> = Vec::new();
        let mut frames: Vec<Vec<u8>> = Vec::new();
        let mut held_back = 0u64;
        framer.push(&buf[..n], |m, frame| {
            seen.push(m);
            if rtcm::forwardable(m) {
                frames.push(frame.to_vec());
            } else {
                held_back += 1;
            }
        });
        for frame in frames {
            match tx.try_send(frame) {
                Ok(()) | Err(TrySendError::Full(_)) => {}
                Err(TrySendError::Disconnected(_)) => return Ok(End::Stopped),
            }
        }
        let first = !streaming && !seen.is_empty();
        streaming |= !seen.is_empty();
        if first {
            tracing::info!(mountpoint = %mount, "RTCM corrections arriving");
        }
        update(status, |s| {
            let c = &mut s.corrections;
            c.bytes += n as u64;
            c.last_at = Some(last_data);
            c.frames += seen.len() as u64;
            c.held_back += held_back;
            for m in seen {
                match c.messages.iter_mut().find(|(k, _)| *k == m) {
                    Some((_, count)) => *count += 1,
                    None => {
                        c.messages.push((m, 1));
                        c.messages.sort_unstable();
                    }
                }
            }
            // Bytes that frame as RTCM, not merely bytes: a caster that answered
            // with a web page is "connected" and useless.
            c.state = if streaming {
                CasterState::Streaming
            } else {
                CasterState::Connecting
            };
        });
    }
    Ok(End::Stopped)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    #[test]
    fn rates_map_to_unicore_periods() {
        assert_eq!(configure_commands(5)[0], "GPGGA 0.2\r\n");
        assert_eq!(configure_commands(1)[2], "GPGST 1\r\n");
        assert_eq!(configure_commands(10)[1], "GPRMC 0.1\r\n");
        assert_eq!(configure_commands(20)[0], "GPGGA 0.05\r\n");
        // A hand-edited user.json cannot ask for more than 20 Hz.
        assert_eq!(configure_commands(u32::MAX)[0], "GPGGA 0.05\r\n");
    }

    #[test]
    fn the_rate_follows_what_the_line_carries() {
        assert_eq!(rate_for_baud(20, 115_200), 20);
        assert_eq!(rate_for_baud(20, 57_600), 10);
        assert_eq!(rate_for_baud(20, 38_400), 5);
        assert_eq!(rate_for_baud(20, 9_600), 1);
        assert_eq!(rate_for_baud(2, 921_600), 2, "never raised");
        assert_eq!(rate_for_baud(1_000, 921_600), 20, "never past 20");
        assert_eq!(rate_for_baud(0, 0), 1);
    }

    /// Every command the client can produce passes the gate, at every rate;
    /// nothing that could change the receiver for good does.
    #[test]
    fn the_command_gate_admits_only_output_rates() {
        for rate in [0, 1, 2, 3, 5, 7, 10, 11, 1000, u32::MAX] {
            for c in configure_commands(rate) {
                assert!(allowed_command(&c), "{c:?}");
            }
        }
        for bad in [
            "SAVECONFIG\r\n",
            "FRESET\r\n",
            "RESET\r\n",
            "CONFIG COM1 9600\r\n",
            "MODE BASE\r\n",
            "UNLOG\r\n",
            "GPGGA COM1 0.2\r\n",
            "GPGGA 0.2\r\nSAVECONFIG\r\n",
            "GPGGA 0.2",
            "GPGGA 0.01\r\n",
            "GPGGA 0.02\r\n",
            "",
        ] {
            assert!(!allowed_command(bad), "{bad:?}");
        }
    }

    #[test]
    fn the_caster_gga_is_short_and_reads_back() {
        let s = Solution {
            lat: 53.219_4,
            lng: 6.566_5,
            quality: FixQuality::RtkFloat,
            altitude_m: Some(8.12),
            geoid_separation_m: Some(40.5),
            satellites: Some(31),
            hdop: Some(0.6),
            correction_age_s: None,
            utc_s: Some(37_230.5),
            sigma_m: None,
            speed_ms: None,
            course_deg: None,
        };
        let line = gga_for_caster(&s);
        assert!(line.len() <= 82, "{} bytes: {line}", line.len());
        assert!(line.starts_with("$GPGGA,102030.50,"), "{line}");
        let mut f = nmea::Framer::default();
        let mut parsed = None;
        f.push(line.as_bytes(), |b| parsed = Some(nmea::parse(b)));
        let Some(Sentence::Gga(g)) = parsed else {
            panic!("{line}")
        };
        assert_eq!(g.quality, FixQuality::RtkFloat);
        assert!((g.lat.unwrap() - s.lat).abs() < 1e-8);
        assert!((g.lng.unwrap() - s.lng).abs() < 1e-8);
    }

    /// A port that plays back a script and records what was written to it.
    struct Script {
        reads: VecDeque<Vec<u8>>,
        written: Arc<Mutex<Vec<u8>>>,
    }

    impl Port for Script {
        fn read(&mut self, buf: &mut [u8]) -> Result<usize, String> {
            std::thread::sleep(Duration::from_millis(5));
            match self.reads.pop_front() {
                Some(r) => {
                    buf[..r.len()].copy_from_slice(&r);
                    Ok(r.len())
                }
                None => Err("unplugged".into()),
            }
        }
        fn write(&mut self, data: &[u8]) -> Result<(), String> {
            self.written.lock().unwrap().extend_from_slice(data);
            Ok(())
        }
    }

    #[test]
    fn the_loop_configures_reads_a_fix_and_reopens_a_lost_port() {
        let written = Arc::new(Mutex::new(Vec::new()));
        let opens = Arc::new(Mutex::new(0));
        let (w, o) = (written.clone(), opens.clone());
        let opener: Opener = Box::new(move || {
            *o.lock().unwrap() += 1;
            let gga =
                nmea::sentence("GNGGA,101530.00,5313.1,N,00633.9,E,4,30,0.6,8.1,M,40.5,M,1.0,0000");
            let reads = VecDeque::from(vec![
                nmea::sentence("command,GPGGA 0.2,response: OK").into_bytes(),
                gga.as_bytes()[..20].to_vec(),
                gga.as_bytes()[20..].to_vec(),
            ]);
            Ok((
                Box::new(Script {
                    reads,
                    written: w.clone(),
                }) as Box<dyn Port>,
                "script".to_string(),
            ))
        });
        let receiver = Receiver::start(
            opener,
            Config {
                configure: true,
                rate_hz: 5,
                caster: None,
            },
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        while *opens.lock().unwrap() < 2 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        let status = receiver.status();
        let s = status.solution.expect("a fix was read");
        assert_eq!(s.quality, FixQuality::RtkFixed);
        assert_eq!(status.last_reply.as_deref(), Some("GPGGA 0.2,response: OK"));
        assert!(*opens.lock().unwrap() >= 2, "the lost port was reopened");
        let w = String::from_utf8(written.lock().unwrap().clone()).unwrap();
        assert!(
            w.starts_with("GPGGA 0.2\r\nGPRMC 0.2\r\nGPGST 0.2\r\n"),
            "{w}"
        );
    }

    /// A receiver that reports the same fix forever.
    struct Steady {
        written: Arc<Mutex<Vec<u8>>>,
    }

    impl Port for Steady {
        fn read(&mut self, buf: &mut [u8]) -> Result<usize, String> {
            std::thread::sleep(Duration::from_millis(50));
            let gga = nmea::sentence("GNGGA,101530.00,5313.1,N,00633.9,E,1,12,0.9,8.1,M,40.5,M,,");
            buf[..gga.len()].copy_from_slice(gga.as_bytes());
            Ok(gga.len())
        }
        fn write(&mut self, data: &[u8]) -> Result<(), String> {
            self.written.lock().unwrap().extend_from_slice(data);
            Ok(())
        }
    }

    /// The whole correction path against a caster on loopback: no mountpoint
    /// set, so the sourcetable is fetched once there is a fix and the nearest
    /// station chosen; its RTCM reaches the receiver, and the rover's position
    /// reaches the caster because the chosen stream asks for it.
    /// A caster on loopback: a two-station sourcetable (NEAR in Groningen,
    /// FAR in Utrecht) and an endless 1005 stream from any mountpoint.
    /// Returns its port and whether it has heard a GGA.
    fn fake_caster() -> (u16, Arc<Mutex<bool>>) {
        fake_caster_sending(rtcm::frame(1005, &[0; 17]))
    }

    /// [`fake_caster`], streaming `chunk` over and over.
    fn fake_caster_sending(chunk: Vec<u8>) -> (u16, Arc<Mutex<bool>>) {
        use std::io::{BufRead, BufReader, Write as _};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let heard_gga = Arc::new(Mutex::new(false));
        let heard = heard_gga.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let mut stream = stream.unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut request = String::new();
                reader.read_line(&mut request).unwrap();
                let mut line = String::new();
                while reader.read_line(&mut line).unwrap() > 2 {
                    line.clear();
                }
                if request.starts_with("GET / ") {
                    let table = "SOURCETABLE 200 OK\r\n\
                        STR;FAR;Utrecht;RTCM 3.2;;2;GPS;NET;NLD;52.09;5.12;0;0;x;none;N;N;0;\r\n\
                        STR;NEAR;Groningen;RTCM 3.2;;2;GPS;NET;NLD;53.24;6.53;1;0;x;none;N;N;0;\r\n\
                        ENDSOURCETABLE\r\n";
                    stream.write_all(table.as_bytes()).unwrap();
                    continue;
                }
                stream.write_all(b"ICY 200 OK\r\n").unwrap();
                let heard = heard.clone();
                std::thread::spawn(move || {
                    let mut l = String::new();
                    while reader.read_line(&mut l).is_ok_and(|n| n > 0) {
                        if l.starts_with("$GPGGA,") {
                            *heard.lock().unwrap() = true;
                        }
                        l.clear();
                    }
                });
                for _ in 0..100 {
                    if stream.write_all(&chunk).is_err() {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
        });

        (port, heard_gga)
    }

    /// A receiver indoors: talking, but never with a position.
    struct NoFix;

    impl Port for NoFix {
        fn read(&mut self, buf: &mut [u8]) -> Result<usize, String> {
            std::thread::sleep(Duration::from_millis(50));
            let gga = nmea::sentence("GNGGA,171018.80,,,,,0,00,9999.0,,,,,,");
            buf[..gga.len()].copy_from_slice(gga.as_bytes());
            Ok(gga.len())
        }
        fn write(&mut self, _: &[u8]) -> Result<(), String> {
            Ok(())
        }
    }

    fn indoors(port: u16) -> Receiver {
        Receiver::start(
            Box::new(|| Ok((Box::new(NoFix) as Box<dyn Port>, "indoors".to_string()))),
            Config {
                configure: false,
                rate_hz: 1,
                caster: Some(Caster {
                    host: "127.0.0.1".into(),
                    port,
                    send_position: true,
                    ..Caster::default()
                }),
            },
        )
    }

    fn wait_for(receiver: &Receiver, done: impl Fn(&Status) -> bool) -> Status {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let status = receiver.status();
            if done(&status) {
                return status;
            }
            assert!(Instant::now() < deadline, "{:?}", status.corrections);
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// With no fix, the corrections wait for nothing more than a hint: the
    /// station is chosen from it and the stream flows. The rover's position is
    /// never invented for the caster — there is none to send.
    #[test]
    fn indoors_the_hint_chooses_the_station() {
        let (port, heard_gga) = fake_caster();
        let receiver = indoors(port);
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(
            receiver.status().corrections.state,
            CasterState::WaitingForPosition
        );
        receiver.set_position_hint(Some((53.2194, 6.5665)));
        let s = wait_for(&receiver, |s| s.corrections.frames >= 3);
        assert_eq!(s.corrections.state, CasterState::Streaming);
        assert_eq!(s.corrections.mountpoint.as_deref(), Some("NEAR"));
        assert!(s.corrections.chosen_from_hint);
        assert!(!*heard_gga.lock().unwrap(), "no fix, so no GGA");
    }

    /// From Madrid every station in the table is over a thousand kilometres
    /// away: refused, rather than "streaming" corrections that fix nothing.
    #[test]
    fn a_station_too_far_away_is_refused() {
        let (port, _) = fake_caster();
        let receiver = indoors(port);
        receiver.set_position_hint(Some((40.4168, -3.7038)));
        let s = wait_for(&receiver, |s| {
            matches!(s.corrections.state, CasterState::Failed(_))
        });
        let CasterState::Failed(why) = s.corrections.state else {
            unreachable!()
        };
        assert!(why.contains("too far"), "{why}");
    }

    #[test]
    fn corrections_flow_from_the_nearest_station_into_the_receiver() {
        let (port, heard_gga) = fake_caster();
        let written = Arc::new(Mutex::new(Vec::new()));
        let w = written.clone();
        let receiver = Receiver::start(
            Box::new(move || {
                Ok((
                    Box::new(Steady { written: w.clone() }) as Box<dyn Port>,
                    "steady".to_string(),
                ))
            }),
            Config {
                configure: false,
                rate_hz: 1,
                caster: Some(Caster {
                    host: "127.0.0.1".into(),
                    port,
                    mountpoint: String::new(),
                    username: String::new(),
                    password: String::new(),
                    send_position: false,
                    tls: false,
                }),
            },
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let status = receiver.status();
            let reached = written.lock().unwrap().len() >= 3 * 25;
            if reached && *heard_gga.lock().unwrap() {
                assert_eq!(status.corrections.state, CasterState::Streaming);
                assert_eq!(status.corrections.mountpoint.as_deref(), Some("NEAR"));
                assert_eq!(status.corrections.messages.first().map(|m| m.0), Some(1005));
                break;
            }
            assert!(Instant::now() < deadline, "{:?}", status.corrections);
            std::thread::sleep(Duration::from_millis(50));
        }
        let w = written.lock().unwrap();
        assert_eq!(&w[..3], &rtcm::frame(1005, &[0; 17])[..3]);
    }

    fn receiver_on(
        port: u16,
        read: fn(&mut [u8]) -> usize,
        written: Arc<Mutex<Vec<u8>>>,
    ) -> Receiver {
        struct Scripted {
            read: fn(&mut [u8]) -> usize,
            written: Arc<Mutex<Vec<u8>>>,
        }
        impl Port for Scripted {
            fn read(&mut self, buf: &mut [u8]) -> Result<usize, String> {
                std::thread::sleep(Duration::from_millis(50));
                Ok((self.read)(buf))
            }
            fn write(&mut self, data: &[u8]) -> Result<(), String> {
                self.written.lock().unwrap().extend_from_slice(data);
                Ok(())
            }
        }
        Receiver::start(
            Box::new(move || {
                Ok((
                    Box::new(Scripted {
                        read,
                        written: written.clone(),
                    }) as Box<dyn Port>,
                    "scripted".to_string(),
                ))
            }),
            Config {
                configure: true,
                rate_hz: 5,
                caster: Some(Caster {
                    host: "127.0.0.1".into(),
                    port,
                    mountpoint: "NEAR".into(),
                    ..Caster::default()
                }),
            },
        )
    }

    fn talking(buf: &mut [u8]) -> usize {
        let gga = nmea::sentence("GNGGA,101530.00,5313.1,N,00633.9,E,1,12,0.9,8.1,M,40.5,M,,");
        buf[..gga.len()].copy_from_slice(gga.as_bytes());
        gga.len()
    }

    /// A caster that types commands at the receiver — a factory reset, a save,
    /// a baud change — between a vendor frame, junk and one real correction.
    /// Only the real correction arrives, whole; the commands never do.
    #[test]
    fn a_hostile_caster_cannot_type_at_the_receiver() {
        let good = rtcm::frame(1077, &[5; 30]);
        let mut chunk = b"FRESET\r\nSAVECONFIG\r\nCONFIG COM1 9600\r\n<html>".to_vec();
        chunk.extend(rtcm::frame(4094, &[1; 12]));
        chunk.extend([0xD3, 0x00, 0x05, 1, 2]);
        chunk.extend(&good);
        chunk.extend(b"MODE BASE\r\n");
        let (port, _) = fake_caster_sending(chunk);
        let written = Arc::new(Mutex::new(Vec::new()));
        let receiver = receiver_on(port, talking, written.clone());
        let s = wait_for(&receiver, |s| s.corrections.frames >= 6);
        assert!(s.corrections.held_back >= 3, "{:?}", s.corrections);
        std::thread::sleep(Duration::from_millis(200));

        let w = written.lock().unwrap().clone();
        let text = String::from_utf8_lossy(&w);
        for bad in ["FRESET", "SAVECONFIG", "CONFIG", "MODE", "<html>"] {
            assert!(!text.contains(bad), "{bad} reached the receiver");
        }
        // What was written is the three output commands, then whole copies of
        // the one good frame and nothing between them.
        let commands: String = configure_commands(5).concat();
        assert!(w.starts_with(commands.as_bytes()));
        let rest = &w[commands.len()..];
        assert!(!rest.is_empty());
        assert_eq!(rest.len() % good.len(), 0);
        for f in rest.chunks(good.len()) {
            assert_eq!(f, &good[..]);
        }
    }

    /// At the wrong baud the receiver's output is noise: nothing checksummed,
    /// so nothing is sent to it — no commands, no corrections — however long
    /// the caster streams.
    #[test]
    fn nothing_is_sent_to_a_receiver_that_has_not_been_heard() {
        fn noise(buf: &mut [u8]) -> usize {
            for (i, b) in buf[..200].iter_mut().enumerate() {
                *b = (i as u8).wrapping_mul(37) | 0x80;
            }
            200
        }
        let (port, _) = fake_caster();
        let written = Arc::new(Mutex::new(Vec::new()));
        let receiver = receiver_on(port, noise, written.clone());
        let s = wait_for(&receiver, |s| {
            s.corrections.frames >= 5 && s.bytes_in > 1000
        });
        assert_eq!(s.sentences, 0);
        assert!(
            written.lock().unwrap().is_empty(),
            "wrote to an unheard receiver"
        );
    }
}

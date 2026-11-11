//! The Midas device link: this app paired to a developer's Midas hub, so their
//! agent sessions can ask it for a screenshot, a camera frame, the precision
//! log or a sensor recording while it is open, and so the developer can send
//! those to a session from the phone (Developer tools ▸ Midas). The design is
//! midas's `docs/mockups/device-link/design.md`; the hub's half is its
//! `src/device.rs`.
//!
//! **The phone dials out.** It is behind NAT, so a worker thread holds a
//! WebSocket to the hub (`wss://<hub>/v1/device/link`) while the link is on,
//! announces its tools, and answers each call the hub relays. A call is
//! answered here in the ECS ([`serve_calls`]), because what it reads — the
//! window, precision mode's camera — lives there; one that waits for a frame
//! or a recording is kept in [`MidasLink`] until it has its answer.
//!
//! **Pairing** trades a one-time code from the hub's page for a device token,
//! kept in `platform::storage` (`midas_link.json`). The token is sent as
//! `X-Midas-Device`, the one header the hub takes it in; Forget on the page
//! voids it.
//!
//! **Not a second keeper of camera frames.** Precision mode keeps nothing the
//! camera sees; a frame or a recording exists here only because a session
//! asked for one, and goes only to the developer's own hub. A call that needs
//! the camera switches precision mode on for itself and off again after.
//!
//! **Two sets of tools.** The sensors — screenshot, camera frame, precision
//! log, recording, GNSS — are this module's own, answered in [`serve_calls`].
//! Beside them the link offers every tool of the client's MCP server
//! (`cartopolis_mcp`: `view`, `walk_to`, `me`, `read_chat`, …), which the
//! worker relays to an in-process session of that server
//! (`cartopolis_mcp::local`) and answers itself. So a session drives the phone
//! exactly as a bot drives a headless client, as the account signed in here,
//! through the same job channel and the same checks; the phone never opens the
//! desktop's socket.

use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::systems::high_precision::{HighPrecision, HighPrecisionLog, Recording};
use crate::systems::theme;

/// The hub this workspace runs, as a phone reaches it.
pub const DEFAULT_HUB: &str = "midas.cartopolis.org";
const STORE_KEY: &str = "midas_link.json";
/// How long a call that needs the camera waits for a frame: opening it takes
/// a second or two, getting it back from another app longer.
const FRAME_WAIT_S: f32 = 15.0;
/// The longest recording a call may ask for.
const RECORD_MAX_S: u64 = 120;
/// A screenshot's longest side: enough to read the UI, small enough to send.
/// A colour still is shrunk to the same.
const SHOT_MAX_PX: u32 = 1600;
/// How long a colour still's camera runs before its newest picture is taken:
/// the first pictures off Camera2 come before auto-exposure has settled, dark
/// or blown out.
const STILL_SETTLE_S: f32 = 1.0;
/// How long `gnss` and `device_state` wait for a first fix when none is in. A
/// fresh watch on Android answers with the system's last known location within
/// a second or two; a cold GNSS start can take minutes, which no call waits for.
const FIX_WAIT_S: f32 = 10.0;
/// How long a fresh compass is given for its first reading.
const COMPASS_WAIT_S: f32 = 1.0;
/// How long a screenshot or a colour still may take from the call to its
/// answer. Neither had a limit, and one whose capture never came (the app
/// suspended under it) waited for good while the hub gave up.
const PICTURE_WAIT_S: f32 = 30.0;
/// How long the link keeps its own location watch and compass after the last
/// call that asked, so a session asking again finds them warm.
const SENSE_HOLD_S: f32 = 120.0;

/// The slot a job off the frame loop answers into.
type Slot<T> = Arc<Mutex<Option<T>>>;

/// What is kept between runs.
#[derive(Serialize, Deserialize, Clone, Default, PartialEq, Debug)]
pub struct LinkConfig {
    /// The hub's host, or `http://host:port` for a plain one on a private network.
    pub hub: String,
    pub token: Option<String>,
    pub id: Option<String>,
    /// The name the hub knows this device by.
    pub name: String,
    /// The link is wanted: held open, and opened again after a restart.
    pub on: bool,
    /// The session Send goes to.
    pub send_to: Option<String>,
}

impl LinkConfig {
    #[must_use]
    pub fn paired(&self) -> bool {
        self.token.is_some()
    }
}

/// A call the hub relayed: one MCP `tools/call`.
#[derive(Debug, Clone)]
pub struct Call {
    pub id: u64,
    pub session: String,
    pub tool: String,
    pub args: Value,
    /// The JSON-RPC id the session's request had, for the response.
    pub rpc: Value,
}

/// What the worker thread and the panel share.
#[derive(Default)]
pub struct Shared {
    /// The link, in words for the panel.
    pub state: String,
    pub linked: bool,
    pub calls: Vec<Call>,
    /// The newest call: its tool, its session and when, for the map's button.
    pub last_call: Option<(String, String, web_time::Instant)>,
    /// The open sessions Send may go to: (id, title).
    pub sessions: Vec<(String, String)>,
    /// The answer to the last Pair, Send or refresh, for the panel.
    pub message: Option<String>,
    /// A pairing that came back: (token, id, name).
    pub paired: Option<(String, String, String)>,
    /// The hub refused the token: pair again.
    pub unpaired: bool,
}

/// A call still waiting for what it asked for.
enum Waiting {
    Frame {
        call: Call,
        until: web_time::Instant,
        started_mode: bool,
    },
    Recording {
        call: Call,
        started_mode: bool,
    },
    Shot {
        call: Call,
        out: Slot<Result<Value, String>>,
        until: web_time::Instant,
    },
    /// `gnss` or `device_state`, waiting for a first fix and compass reading.
    Fix {
        call: Call,
        since: web_time::Instant,
    },
    /// A colour still: the back camera, opened for this call only, until its
    /// exposure has settled.
    Still {
        call: Call,
        camera: crate::platform::camera::Camera,
        until: web_time::Instant,
        first_at: Option<web_time::Instant>,
        picture: Option<cartopolis_calls::video::I420>,
    },
    /// A colour still being encoded off the frame loop, into its answer.
    Encoding {
        call: Call,
        out: Slot<Result<Value, String>>,
        until: web_time::Instant,
    },
}

/// The link's own watch on the phone's location and its compass, opened by a
/// call that asks where the phone is and kept [`SENSE_HOLD_S`] after the last
/// one. The location watch is the one subscription every holder shares
/// (`platform::location`), so while the locate button or precision mode runs
/// this only joins it; the compass is a subscription of its own.
#[derive(Default)]
struct Sense {
    watch: Option<crate::platform::location::LocationWatch>,
    compass: Option<crate::platform::orientation::OrientationWatch>,
    until: Option<web_time::Instant>,
    /// The fix last seen, when it changed, and whether that change was seen
    /// happen (else its age is only a lower bound: `GpsFix` carries no
    /// timestamp).
    last: Option<crate::platform::location::GpsFix>,
    changed_at: Option<web_time::Instant>,
    seen_arrive: bool,
}

impl Sense {
    /// Open the watches if they are not, and keep them for a while.
    fn hold(&mut self, now: web_time::Instant) {
        use crate::platform::{location::LocationWatch, orientation::OrientationWatch};
        if self.watch.is_none() && LocationWatch::available() {
            self.watch = Some(LocationWatch::start());
            self.last = None;
            self.changed_at = None;
            self.seen_arrive = false;
        }
        if self.compass.is_none() && OrientationWatch::available() {
            self.compass = Some(OrientationWatch::start());
        }
        self.until = Some(now + std::time::Duration::from_secs_f32(SENSE_HOLD_S));
    }

    /// Note when the fix changes; let go once the hold is over and nothing waits.
    fn tick(&mut self, now: web_time::Instant, waiting: bool) {
        if !waiting && self.until.is_some_and(|u| now >= u) {
            *self = Self::default();
            return;
        }
        let Some(watch) = &self.watch else { return };
        let fix = watch.latest();
        if fix != self.last {
            if fix.is_some() {
                self.changed_at = Some(now);
                // Only a change from one fix to the next is seen arriving: the
                // first may have been in for long (a joined watch), or be the
                // system's last known location (a fresh one on Android).
                self.seen_arrive = self.last.is_some();
            }
            self.last = fix;
        }
    }
}

/// A message a session put beside the Midas button with `tell`.
#[derive(Debug, Clone)]
pub struct Told {
    pub session: String,
    pub text: String,
    /// When it was told, so the map repaints for a new one.
    pub at: web_time::Instant,
    /// When it goes on its own; `None` keeps it until tapped or replaced.
    pub until: Option<web_time::Instant>,
}

impl Told {
    /// The message a `tell` call asks for, or `None` for an empty text, which
    /// clears it.
    fn from_call(call: &Call, now: web_time::Instant) -> Option<Self> {
        let text = call.args["text"].as_str().unwrap_or("").trim();
        if text.is_empty() {
            return None;
        }
        let text: String = text.chars().take(TELL_MAX_CHARS).collect();
        let until = call.args["seconds"]
            .as_u64()
            .filter(|s| *s > 0)
            .map(|s| now + std::time::Duration::from_secs(s.min(TELL_MAX_S)));
        Some(Self {
            session: call.session.clone(),
            text,
            at: now,
            until,
        })
    }
}

/// The longest `tell` the bubble holds: a few lines on a phone, not a page.
const TELL_MAX_CHARS: usize = 400;
/// The longest a timed `tell` stays.
const TELL_MAX_S: u64 = 3600;

#[derive(Resource)]
pub struct MidasLink {
    pub config: LinkConfig,
    saved: LinkConfig,
    loaded: bool,
    pub shared: Arc<Mutex<Shared>>,
    /// Typed into the panel.
    pub code_draft: String,
    pub hub_draft: String,
    pub text_draft: String,
    waiting: Vec<Waiting>,
    sense: Sense,
    worker: Option<worker::Worker>,
    /// What the worker was last told to link to, so a change re-links once.
    linked_to: Option<(String, String)>,
    /// A push asked for from the panel: (what, needs a screenshot first).
    push_shot: Option<Arc<Mutex<Option<Result<Vec<u8>, String>>>>>,
    /// The card beside the map's Midas button is open.
    pub card_open: bool,
    /// What a session asked to tell the person (`tell`), shown beside the
    /// button until it is tapped away, replaced, or its time is up.
    pub told: Option<Told>,
    /// The MCP server's job channel, for the worker's in-process session.
    #[cfg(mcp)]
    jobs: Option<McpJobs>,
}

#[cfg(mcp)]
type McpJobs = futures_channel::mpsc::UnboundedSender<cartopolis_mcp::protocol::Job>;

impl Default for MidasLink {
    fn default() -> Self {
        Self {
            config: LinkConfig::default(),
            saved: LinkConfig::default(),
            loaded: false,
            shared: Arc::default(),
            code_draft: String::new(),
            hub_draft: DEFAULT_HUB.to_string(),
            text_draft: String::new(),
            waiting: Vec::new(),
            sense: Sense::default(),
            worker: None,
            linked_to: None,
            push_shot: None,
            card_open: false,
            told: None,
            #[cfg(mcp)]
            jobs: None,
        }
    }
}

impl MidasLink {
    fn shared(&self) -> std::sync::MutexGuard<'_, Shared> {
        self.shared
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Whether the link is up now, for the map's button.
    #[must_use]
    pub fn linked(&self) -> bool {
        self.shared().linked
    }

    /// The newest call, if it came in the last few seconds: (tool, session).
    #[must_use]
    pub fn recent_call(&self, within_s: f32) -> Option<(String, String)> {
        let s = self.shared();
        let (tool, session, at) = s.last_call.as_ref()?;
        (at.elapsed().as_secs_f32() < within_s).then(|| (tool.clone(), session.clone()))
    }

    /// The link as it looks up, for `--ui midaslink`, `midascard` and
    /// `midastell` (a session's message beside the button): a
    /// scripted run is paired to no hub. Nothing is written, since `saved`
    /// follows `config`, and with no token no worker starts.
    pub fn stage_for_capture(&mut self, card: bool, tell: bool) {
        self.loaded = true;
        self.told = tell.then(|| Told {
            session: "s669".to_string(),
            text: "I am walking you to the Vismarkt. Tap the camera button when you are there, and I will take it from there.".to_string(),
            at: web_time::Instant::now(),
            until: None,
        });
        self.config.on = true;
        self.saved = self.config.clone();
        self.card_open = card;
        let mut s = self.shared();
        s.linked = true;
        // Seconds old: past the glow, still "just now".
        s.last_call = web_time::Instant::now()
            .checked_sub(std::time::Duration::from_secs(5))
            .map(|at| ("screenshot".to_string(), "s579".to_string(), at));
    }

    fn worker(&mut self) -> &worker::Worker {
        let shared = self.shared.clone();
        #[cfg(mcp)]
        let tools = self
            .jobs
            .clone()
            .map(|jobs| Arc::new(cartopolis_mcp::local::LocalTools::new(jobs)));
        #[cfg(not(mcp))]
        let tools: Option<worker::Tools> = None;
        self.worker
            .get_or_insert_with(|| worker::Worker::start(shared, tools))
    }
}

/// This module's own tools, as MCP lists them; the link offers the MCP
/// server's beside them. `device_state` rather than `state`, which is the
/// server's (where the camera is, whether the map has settled).
#[must_use]
pub fn tools() -> Value {
    let none = json!({"type": "object", "properties": {}});
    json!([
        {"name": "device_state", "description": "What the app is doing: its build (version and commit), the platform, precision mode and what it says, and where the phone is — the newest location fix (precision mode's, or the phone's ordinary location when that is off), its accuracy, age, speed and course, and the compass. Waits a few seconds for a first fix when none is in.", "inputSchema": none},
        {"name": "screenshot", "description": "The app's window as it is now, UI and all, as a PNG.", "inputSchema": none},
        {"name": "camera_frame", "description": "One frame from the back camera. By default greyscale as the tracker sees it, as a PNG: switches precision mode on for the call when it is off (the camera takes a second or two to open) and off again after. With colour, a colour still from the back camera as a JPEG instead (at most 1600 px), the camera opened for the call and closed after; refused while precision mode or the camera screen has the camera.",
         "inputSchema": {"type": "object", "properties": {"colour": {"type": "boolean", "description": "a colour still instead of the tracker's grey frame"}}}},
        {"name": "precision_log", "description": "The precision mode's log (Developer tools ▸ Precision): the camera, motion, magnetometer and GNSS lines the indoor walk test writes.", "inputSchema": none},
        {"name": "record", "description": "Record the motion sensors (accelerometer, gyroscope, magnetometer; device axes, on CLOCK_BOOTTIME) and each new GNSS fix for a while: two CSV files. Runs precision mode for the recording.",
         "inputSchema": {"type": "object", "properties": {"seconds": {"type": "integer", "description": format!("how long, at most {RECORD_MAX_S}")}}, "required": ["seconds"]}},
        {"name": "gnss", "description": "The newest location fix — precision mode's, or the phone's ordinary location when that is off, saying which — with its accuracy, age, speed, course and the compass, and the external RTK receiver's report (its settings, state and events).", "inputSchema": none},
        {"name": "tell", "description": "Show the person holding the phone a short message in a bubble beside the Midas button on the map, signed with this session's name, so they can follow along without leaving the app: what you are about to do, what you saw, or what they should tap or do next. It stays until they tap it away, the next tell replaces it, or `seconds` pass; an empty text takes it down. Shown on the map only, not over an open panel. This is not chat: nobody else sees it (`say` speaks in the world).",
         "inputSchema": {"type": "object", "properties": {
             "text": {"type": "string", "description": format!("the message, a few short sentences at most ({TELL_MAX_CHARS} characters); empty to take the bubble down")},
             "seconds": {"type": "integer", "description": "take it down after this long; leave out to keep it until tapped or replaced"}
         }, "required": ["text"]}},
    ])
}

/// The build, for the hub's device list and the bridge's "this build has no such tool".
#[must_use]
pub fn build() -> Value {
    json!({"version": env!("CARGO_PKG_VERSION"), "commit": option_env!("CARTO_BUILD_SHA")})
}

fn text(t: impl Into<String>) -> Value {
    json!({"content": [{"type": "text", "text": t.into()}]})
}

fn refusal(t: impl Into<String>) -> Value {
    json!({"content": [{"type": "text", "text": t.into()}], "isError": true})
}

fn png_result(png: &[u8], caption: &str) -> Value {
    picture_result(png, "image/png", caption)
}

fn picture_result(bytes: &[u8], mime: &str, caption: &str) -> Value {
    use base64::Engine as _;
    json!({"content": [
        {"type": "image", "mimeType": mime, "data": base64::engine::general_purpose::STANDARD.encode(bytes)},
        {"type": "text", "text": caption},
    ]})
}

/// A camera picture as packed RGB, three bytes a pixel.
fn rgb_from_i420(picture: &cartopolis_calls::video::I420) -> Vec<u8> {
    picture
        .to_rgba()
        .chunks_exact(4)
        .flat_map(|px| [px[0], px[1], px[2]])
        .collect()
}

/// A colour still, shrunk to [`SHOT_MAX_PX`], as a JPEG and its caption. Run
/// off the frame loop: a 1920×1440 picture is a tenth of a second or more.
fn still_jpeg(picture: &cartopolis_calls::video::I420) -> Result<(Vec<u8>, String), String> {
    let rgb = image::RgbImage::from_raw(picture.width, picture.height, rgb_from_i420(picture))
        .ok_or("a picture of the wrong size")?;
    let dynamic = image::DynamicImage::ImageRgb8(rgb);
    let dynamic = if dynamic.width().max(dynamic.height()) > SHOT_MAX_PX {
        dynamic.resize(
            SHOT_MAX_PX,
            SHOT_MAX_PX,
            image::imageops::FilterType::Triangle,
        )
    } else {
        dynamic
    };
    let mut jpeg = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 85)
        .encode_image(&dynamic.to_rgb8())
        .map_err(|e| e.to_string())?;
    let caption = format!(
        "back camera, {}×{} colour (taken at {}×{})",
        dynamic.width(),
        dynamic.height(),
        picture.width,
        picture.height
    );
    Ok((jpeg, caption))
}

/// Why a colour still cannot be taken now, if it cannot. Only one camera can
/// be open on the phone, and this does not take one from its holder.
fn colour_refusal(
    precision_on: bool,
    screen_has_camera: bool,
    still_pending: bool,
) -> Option<&'static str> {
    if still_pending {
        Some("A colour picture is being taken already: ask again in a moment.")
    } else if precision_on {
        Some("Precision mode has the back camera, so no colour picture now: switch it off (the hand button), or ask without colour for the tracker's grey frame.")
    } else if screen_has_camera {
        Some("The camera screen has the camera open, so no colour picture now: close it first.")
    } else {
        None
    }
}

/// A greyscale frame as a PNG.
fn frame_png(f: &crate::platform::precise_sensors::Frame) -> Result<Vec<u8>, String> {
    let img = image::GrayImage::from_raw(f.width, f.height, f.luma.clone())
        .ok_or("a frame of the wrong size")?;
    let mut png = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .map_err(|e| e.to_string())?;
    Ok(png)
}

/// A captured window, shrunk to [`SHOT_MAX_PX`], as a PNG. Run off the frame loop.
fn shot_png(image: &bevy::image::Image) -> Result<Vec<u8>, String> {
    let dynamic = image
        .clone()
        .try_into_dynamic()
        .map_err(|e| e.to_string())?;
    let dynamic = if dynamic.width().max(dynamic.height()) > SHOT_MAX_PX {
        dynamic.resize(
            SHOT_MAX_PX,
            SHOT_MAX_PX,
            image::imageops::FilterType::Triangle,
        )
    } else {
        dynamic
    };
    let mut png = Vec::new();
    dynamic
        .to_rgb8()
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .map_err(|e| e.to_string())?;
    Ok(png)
}

/// Ask for the window as it is; what `done` makes of its PNG arrives in the
/// slot returned, off the frame loop.
fn capture<T: Send + 'static>(
    commands: &mut Commands,
    done: impl Fn(Vec<u8>) -> T + Send + Sync + 'static,
) -> Slot<Result<T, String>> {
    use bevy::render::view::screenshot::{Screenshot, ScreenshotCaptured};
    let out: Slot<Result<T, String>> = Arc::default();
    let done = Arc::new(done);
    let slot = out.clone();
    commands.spawn(Screenshot::primary_window()).observe(
        move |captured: On<ScreenshotCaptured>| {
            let image = captured.image.clone();
            let (slot, done) = (slot.clone(), done.clone());
            // Encoding a phone's screen is a quarter second, and its base64 a
            // megabyte more: not on the frame.
            crate::platform::workers::submit(
                "midas_link_shot",
                Box::new(move || {
                    let r = shot_png(&image).map(|png| done(png));
                    *slot
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(r);
                }),
            );
        },
    );
    out
}

fn take<T>(slot: &Arc<Mutex<Option<T>>>) -> Option<T> {
    slot.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take()
}

/// Whether the link is wanted (paired and on), for the background service
/// (`systems::background`): while it is, Android must not freeze the process,
/// or the link dies each time the person leaves the app to read what a session
/// made of it — on the phone they follow Midas on, which is every time.
static HELD: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[must_use]
pub fn holds_process() -> bool {
    HELD.load(std::sync::atomic::Ordering::Relaxed)
}

/// Tell the hub when the app leaves the screen and comes back. Off screen
/// the link stays, but nothing is answered — Bevy runs no update while the
/// app is suspended — so the hub tells the person a call waits for them.
pub fn follow_lifecycle(
    mut lifecycle: MessageReader<bevy::window::AppLifecycle>,
    link: Res<MidasLink>,
) {
    use bevy::window::AppLifecycle;
    for event in lifecycle.read() {
        let away = match event {
            AppLifecycle::WillSuspend | AppLifecycle::Suspended => true,
            AppLifecycle::WillResume | AppLifecycle::Running => false,
            AppLifecycle::Idle => continue,
        };
        if let Some(w) = &link.worker {
            w.away(away);
        }
    }
}

/// Load the saved link once, keep it saved, and keep the worker linked to what it says.
pub fn drive_link(
    mut link: ResMut<MidasLink>,
    #[cfg(mcp)] jobs: Option<Res<crate::systems::mcp::JobSender>>,
) {
    let link = &mut *link;
    // Before anything can start the worker: it opens its session with these.
    // None in a run with no MCP server (a scripted capture, `--no-mcp`).
    #[cfg(mcp)]
    if link.jobs.is_none() {
        link.jobs = jobs.map(|j| j.0.clone());
    }
    if !link.loaded {
        if let crate::platform::storage::Cached::Ready(bytes) =
            crate::platform::storage::read_now(crate::platform::storage::Namespace::Data, STORE_KEY)
        {
            if let Ok(c) = serde_json::from_slice::<LinkConfig>(&bytes) {
                link.hub_draft = if c.hub.is_empty() {
                    DEFAULT_HUB.to_string()
                } else {
                    c.hub.clone()
                };
                link.config = c.clone();
                link.saved = c;
            }
        }
        link.loaded = true;
    }
    {
        let mut s = link.shared();
        if let Some((token, id, name)) = s.paired.take() {
            drop(s);
            link.config.token = Some(token);
            link.config.id = Some(id);
            link.config.name = name;
            link.config.hub = link.hub_draft.trim().to_string();
            link.config.on = true;
            link.code_draft.clear();
        } else if std::mem::take(&mut s.unpaired) {
            drop(s);
            link.config.token = None;
            link.config.id = None;
            link.config.on = false;
        }
    }
    if link.config != link.saved {
        if let Ok(bytes) = serde_json::to_vec(&link.config) {
            let _ = crate::platform::storage::write_now(
                crate::platform::storage::Namespace::Data,
                STORE_KEY,
                &bytes,
            );
        }
        link.saved = link.config.clone();
    }
    let want = link
        .config
        .token
        .clone()
        .filter(|_| link.config.on)
        .map(|t| (link.config.hub.clone(), t));
    HELD.store(want.is_some(), std::sync::atomic::Ordering::Relaxed);
    if want != link.linked_to {
        match &want {
            Some((hub, token)) => link.worker().link(hub, token),
            None => {
                if let Some(w) = &link.worker {
                    w.unlink();
                }
            }
        }
        link.linked_to = want;
    }
}

/// Answer the calls the hub relayed, and those that waited for a frame, a
/// recording, a screenshot, a fix or a colour still once they have it.
pub fn serve_calls(
    mut commands: Commands,
    mut link: ResMut<MidasLink>,
    mut high: ResMut<HighPrecision>,
    log: Res<HighPrecisionLog>,
    store: Res<crate::systems::user_store::UserStore>,
    locate: Option<Res<crate::systems::locate::LocateTracking>>,
    screen: Option<Res<crate::systems::camera_screen::CameraScreen>>,
    right_panel: Res<crate::resources::RightPanel>,
    driver: Res<crate::systems::driver::DriverState>,
) {
    let link = &mut *link;
    let now = web_time::Instant::now();
    let calls = std::mem::take(&mut link.shared().calls);
    let mut answers: Vec<(Call, Value)> = Vec::new();
    for call in calls {
        let still_pending = link
            .waiting
            .iter()
            .any(|w| matches!(w, Waiting::Still { .. } | Waiting::Encoding { .. }));
        let answer = match call.tool.as_str() {
            // Answered below, once there is a fix (or plainly none) to report.
            "device_state" | "gnss" => {
                link.sense.hold(now);
                link.waiting.push(Waiting::Fix { call: call.clone(), since: now });
                None
            }
            "tell" => {
                link.told = Told::from_call(&call, now);
                let hidden = !right_panel.is(crate::resources::RightPanel::None) || driver.active;
                Some(text(match (&link.told, hidden) {
                    (None, _) => "Taken down.",
                    (Some(_), false) => "Shown beside the Midas button.",
                    (Some(_), true) => "Kept, but not shown yet: a panel or driver mode covers the map, and the bubble appears beside the Midas button once the person is back on the map.",
                }))
            }
            "precision_log" => Some(if log.is_empty() {
                text("The log is empty: precision mode has not run since the app started (Developer tools ▸ Precision ▸ Start, or camera_frame / record).")
            } else {
                text(log.text())
            }),
            "screenshot" => {
                let out = capture(&mut commands, |png| png_result(&png, "the app's window"));
                let until = now + std::time::Duration::from_secs_f32(PICTURE_WAIT_S);
                link.waiting.push(Waiting::Shot { call: call.clone(), out, until });
                None
            }
            "camera_frame" | "record" if !HighPrecision::available() => {
                Some(refusal("This device has no back camera and motion sensors the app can use: that takes the Android app on a phone."))
            }
            "camera_frame" if call.args["colour"].as_bool() == Some(true) => {
                let screen_has_camera = screen.as_ref().is_some_and(|s| s.phone_hold().is_some());
                match colour_refusal(high.on, screen_has_camera, still_pending) {
                    Some(why) => Some(refusal(why)),
                    None => {
                        // Opened for this call only, and dropped (closed) as
                        // soon as it has its picture or gives up.
                        let camera = crate::platform::camera::Camera::open_still(false);
                        let until = now + std::time::Duration::from_secs_f32(FRAME_WAIT_S);
                        link.waiting.push(Waiting::Still { call: call.clone(), camera, until, first_at: None, picture: None });
                        None
                    }
                }
            }
            "camera_frame" | "record" if still_pending => {
                Some(refusal("The back camera is taking a colour picture for another call: ask again in a moment."))
            }
            "camera_frame" => {
                let started_mode = !high.on;
                high.on = true;
                high.tap.want_frame = true;
                high.tap.frame = None;
                let until = web_time::Instant::now() + std::time::Duration::from_secs_f32(FRAME_WAIT_S);
                link.waiting.push(Waiting::Frame { call: call.clone(), until, started_mode });
                None
            }
            "record" => {
                if high.tap.recording.is_some() {
                    Some(refusal("A recording is running already."))
                } else {
                    let secs = call.args["seconds"].as_u64().unwrap_or(10).clamp(1, RECORD_MAX_S);
                    let started_mode = !high.on;
                    high.on = true;
                    high.tap.recording = Some(Recording::new(web_time::Instant::now() + std::time::Duration::from_secs(secs)));
                    link.waiting.push(Waiting::Recording { call: call.clone(), started_mode });
                    None
                }
            }
            other => Some(refusal(format!("This build has no tool {other}: run the app from the session's own code on a build node, or install a newer build."))),
        };
        if let Some(a) = answer {
            answers.push((call, a));
        }
    }

    // Those waiting: done, or still waiting.
    let fix_waiting = link
        .waiting
        .iter()
        .any(|w| matches!(w, Waiting::Fix { .. }));
    link.sense.tick(now, fix_waiting);
    let mut still = Vec::new();
    for w in std::mem::take(&mut link.waiting) {
        match w {
            Waiting::Fix { call, since } => {
                let waited_s = (now - since).as_secs_f32();
                let w = whereabouts(&high, &link.sense, locate.as_deref(), now, waited_s);
                if w.settled(waited_s) {
                    let t = if call.tool == "gnss" {
                        gnss(&w, &store.data.gnss_receiver)
                    } else {
                        state(&high, &w)
                    };
                    answers.push((call, text(t)));
                } else {
                    still.push(Waiting::Fix { call, since });
                }
            }
            Waiting::Still {
                call,
                camera,
                until,
                mut first_at,
                mut picture,
            } => {
                if let Some(p) = camera.take_preview() {
                    picture = Some(p);
                    first_at.get_or_insert(now);
                }
                let settled = first_at.is_some_and(|t| (now - t).as_secs_f32() >= STILL_SETTLE_S);
                if let Some(p) = picture.take_if(|_| settled) {
                    // The camera closes here, before the encode: nothing more
                    // of what it sees is wanted.
                    drop(camera);
                    let out: Slot<Result<Value, String>> = Arc::default();
                    let slot = out.clone();
                    crate::platform::workers::submit(
                        "midas_link_still",
                        Box::new(move || {
                            let r = still_jpeg(&p).map(|(jpeg, caption)| {
                                picture_result(&jpeg, "image/jpeg", &caption)
                            });
                            *slot
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(r);
                        }),
                    );
                    let until = now + std::time::Duration::from_secs_f32(PICTURE_WAIT_S);
                    still.push(Waiting::Encoding { call, out, until });
                } else if let Some(e) = camera.error() {
                    answers.push((
                        call,
                        refusal(format!(
                            "The back camera would not give a colour picture: {e}"
                        )),
                    ));
                } else if now >= until {
                    answers.push((call, refusal(format!("No colour picture within {FRAME_WAIT_S:.0} s: no picture came from the back camera. Is the app in front, and the camera not taken by another app?"))));
                } else {
                    still.push(Waiting::Still {
                        call,
                        camera,
                        until,
                        first_at,
                        picture,
                    });
                }
            }
            Waiting::Encoding { call, out, until } => match take(&out) {
                Some(Ok(answer)) => answers.push((call, answer)),
                Some(Err(e)) => answers.push((
                    call,
                    refusal(format!("the colour picture could not be encoded: {e}")),
                )),
                None if now >= until => answers.push((call, refusal(format!("The colour picture was not encoded within {PICTURE_WAIT_S:.0} s.")))),
                None => still.push(Waiting::Encoding { call, out, until }),
            },
            Waiting::Shot { call, out, until } => match take(&out) {
                Some(Ok(answer)) => answers.push((call, answer)),
                Some(Err(e)) => answers.push((
                    call,
                    refusal(format!("the window could not be captured: {e}")),
                )),
                None if now >= until => answers.push((call, refusal(format!("No screenshot within {PICTURE_WAIT_S:.0} s: the window was not drawn. Is the app on screen?")))),
                None => still.push(Waiting::Shot { call, out, until }),
            },
            Waiting::Frame {
                call,
                until,
                started_mode,
            } => {
                if let Some(frame) = high.tap.frame.take() {
                    if started_mode {
                        high.on = false;
                    }
                    let a = match frame_png(&frame) {
                        Ok(png) => png_result(
                            &png,
                            &format!(
                                "back camera, {}×{} grey, t_ns {}",
                                frame.width, frame.height, frame.t_ns
                            ),
                        ),
                        Err(e) => refusal(e),
                    };
                    answers.push((call, a));
                } else if now >= until {
                    high.tap.want_frame = false;
                    if started_mode {
                        high.on = false;
                    }
                    let why = high
                        .problem()
                        .unwrap_or_else(|| "no frame came from the camera".to_string());
                    answers.push((call, refusal(format!("No camera frame within {FRAME_WAIT_S:.0} s: {why}. Is the app in front, and the camera not taken by another app?"))));
                } else {
                    still.push(Waiting::Frame {
                        call,
                        until,
                        started_mode,
                    });
                }
            }
            Waiting::Recording { call, started_mode } => {
                if high.tap.recording.as_ref().is_some_and(|r| now >= r.until) {
                    let r = high.tap.recording.take().expect("checked above");
                    if started_mode {
                        high.on = false;
                    }
                    let rows = r.motion.lines().count().saturating_sub(1);
                    let fixes = r.fixes.lines().count().saturating_sub(1);
                    answers.push((call, json!({"content": [
                        {"type": "text", "text": format!("{rows} motion samples and {fixes} fixes")},
                        {"type": "resource", "resource": {"uri": "cartopolis://record/motion.csv", "mimeType": "text/csv", "text": r.motion}},
                        {"type": "resource", "resource": {"uri": "cartopolis://record/fixes.csv", "mimeType": "text/csv", "text": r.fixes}},
                    ]})));
                } else {
                    still.push(Waiting::Recording { call, started_mode });
                }
            }
        }
    }
    link.waiting = still;

    for (call, result) in answers {
        let response = json!({"jsonrpc": "2.0", "id": call.rpc, "result": result});
        if let Some(w) = &link.worker {
            w.reply(call.id, response);
        }
    }

    // A push the panel asked for, once its screenshot is in.
    if let Some(slot) = &link.push_shot {
        if let Some(r) = take(slot) {
            link.push_shot = None;
            match r {
                Ok(png) => {
                    use base64::Engine as _;
                    let file = json!({"name": "screenshot.png", "data": base64::engine::general_purpose::STANDARD.encode(png)});
                    push(link, "A screenshot.", vec![file]);
                }
                Err(e) => {
                    link.shared().message = Some(format!("Could not capture the window: {e}"))
                }
            }
        }
    }
}

fn state(high: &HighPrecision, w: &Whereabouts) -> String {
    format!(
        "{}\nplatform {}\ndevice {}\nprecision mode {}{}\n{}",
        build_line(env!("CARGO_PKG_VERSION"), option_env!("CARTO_BUILD_SHA")),
        std::env::consts::OS,
        crate::platform::precise_sensors::device_name(),
        if high.on { "on" } else { "off" },
        high.hand_word()
            .map(|w| format!(" · {}", w.text()))
            .unwrap_or_default(),
        w.text(),
    )
}

/// The build in words: the version and the commit. `CARTO_BUILD_SHA` is what
/// [`build`] sends the hub, and the APK's is `version+commit`
/// (`build-apk.sh`), so the commit is what follows the `+`.
fn build_line(version: &str, sha: Option<&str>) -> String {
    match sha {
        Some(sha) => {
            let commit = sha.rsplit_once('+').map_or(sha, |(_, c)| c);
            format!("build {version} · commit {commit}")
        }
        None => format!("build {version} · commit unknown (built without CARTO_BUILD_SHA)"),
    }
}

fn gnss(w: &Whereabouts, prefs: &crate::systems::gnss_receiver::ReceiverPrefs) -> String {
    let mut out = w.text();
    out.push('\n');
    out.push_str(&crate::systems::gnss_receiver::report(prefs));
    out
}

/// Whose watch the fix came through. One provider answers them all
/// (`platform::location`), so this says who is holding the receiver open,
/// and whether the fix is precision mode's or the phone's ordinary location.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FixSource {
    /// An external receiver switched on in Settings, whose fresh fix wins.
    Receiver,
    Precision,
    Locate,
    /// The link's own watch, opened for a call.
    Link,
}

impl FixSource {
    const fn words(self) -> &'static str {
        match self {
            Self::Receiver => "the external receiver",
            Self::Precision => "precision mode's watch",
            Self::Locate => "ordinary location, the locate button's watch",
            Self::Link => "ordinary location, opened for this call",
        }
    }
}

/// A compass reading: the bearing of the screen's top, its stated accuracy,
/// and where the back camera looks (heading, pitch).
#[derive(Clone, Copy, Debug, PartialEq)]
struct CompassReading {
    bearing_deg: f64,
    accuracy_deg: Option<f64>,
    camera: (f64, f64),
    /// Whose compass: the locate button's, or the link's own.
    locate: bool,
}

/// Where the phone is, as `gnss` and `device_state` say it: gathered from the
/// ECS, written by [`Whereabouts::text`], which is pure.
#[derive(Clone, Debug, PartialEq)]
struct Whereabouts {
    /// This build has a location provider at all.
    provider: bool,
    fix: Option<crate::platform::location::GpsFix>,
    source: FixSource,
    /// Seconds since the fix last changed, and whether the change was seen
    /// (else it is only a lower bound).
    age: Option<(f32, bool)>,
    error: Option<String>,
    compass_available: bool,
    compass: Option<CompassReading>,
    compass_error: Option<String>,
    waited_s: f32,
}

fn whereabouts(
    high: &HighPrecision,
    sense: &Sense,
    locate: Option<&crate::systems::locate::LocateTracking>,
    now: web_time::Instant,
    waited_s: f32,
) -> Whereabouts {
    use crate::platform::{location::LocationWatch, orientation::OrientationWatch};
    let locate_on = locate.is_some_and(|l| l.active);
    let precision = high.fix();
    let fix = sense
        .watch
        .as_ref()
        .and_then(LocationWatch::latest)
        .or(precision);
    let source = if crate::platform::gnss_receiver::fresh_fix().is_some() {
        FixSource::Receiver
    } else if precision.is_some() {
        FixSource::Precision
    } else if locate_on {
        FixSource::Locate
    } else {
        FixSource::Link
    };
    let read = |a: crate::platform::orientation::DeviceAttitude, locate: bool| {
        let (heading, pitch, _) = a.view_angles(false);
        CompassReading {
            bearing_deg: a.bearing_deg(),
            accuracy_deg: a.accuracy_deg,
            camera: (heading, pitch),
            locate,
        }
    };
    let compass = locate
        .filter(|_| locate_on)
        .and_then(|l| l.heading)
        .map(|a| read(a, true))
        .or_else(|| {
            sense
                .compass
                .as_ref()
                .and_then(OrientationWatch::latest)
                .map(|a| read(a, false))
        });
    Whereabouts {
        provider: LocationWatch::available(),
        fix,
        source,
        age: sense
            .changed_at
            .filter(|_| fix.is_some())
            .map(|t| ((now - t).as_secs_f32(), sense.seen_arrive)),
        error: sense.watch.as_ref().and_then(LocationWatch::error),
        compass_available: OrientationWatch::available(),
        compass,
        compass_error: sense.compass.as_ref().and_then(OrientationWatch::error),
        waited_s,
    }
}

impl Whereabouts {
    /// Whether there is enough to answer with after `waited_s`: a fix, or
    /// plainly none (no provider, an error, waited long enough), and a compass
    /// reading or a second for one.
    fn settled(&self, waited_s: f32) -> bool {
        let fix =
            !self.provider || self.fix.is_some() || self.error.is_some() || waited_s >= FIX_WAIT_S;
        let compass = !self.compass_available
            || self.compass.is_some()
            || self.compass_error.is_some()
            || waited_s >= COMPASS_WAIT_S;
        fix && compass
    }

    fn text(&self) -> String {
        let fix = match (&self.fix, &self.error) {
            (Some(f), _) => {
                let age = self.age.map(|(s, seen)| {
                    if seen {
                        format!("{s:.0} s old")
                    } else if s < 1.0 {
                        "age unknown (the first fix seen: it may be the phone's last known one)"
                            .to_string()
                    } else {
                        format!("at least {s:.0} s old")
                    }
                });
                let quality = crate::systems::driver::gps_quality(
                    true,
                    false,
                    Some(f.accuracy_m),
                    self.age.map(|(s, _)| s),
                );
                let quality = match quality {
                    crate::systems::driver::GpsQuality::Stale => "stale: no new fix lately",
                    crate::systems::driver::GpsQuality::Poor(_) => "poor",
                    _ => "good",
                };
                let speed = match (f.speed_ms, f.heading_deg) {
                    (None, None) => "speed and course not reported (standing still, or not moved far enough yet)".to_string(),
                    (s, c) => format!(
                        "speed {} · course {}",
                        s.map_or_else(|| "not reported".to_string(), |s| format!("{s:.1} m/s")),
                        c.map_or_else(|| "not reported".to_string(), |c| format!("{c:.0}°")),
                    ),
                };
                format!(
                    "fix {:.7},{:.7} ±{:.1} m ({}) · {} · {quality}\n{speed}",
                    f.lat,
                    f.lng,
                    f.accuracy_m,
                    self.source.words(),
                    age.unwrap_or_else(|| "age unknown".to_string()),
                )
            }
            (None, Some(e)) => format!("no fix: {e}"),
            (None, None) if !self.provider => "no fix: this build has no location provider (that takes a phone, a browser, or an external receiver switched on in Settings)".to_string(),
            (None, None) => format!("no fix yet: none came within {:.0} s (a cold GNSS start can take a minute or more, indoors longer)", self.waited_s),
        };
        let compass = match (&self.compass, &self.compass_error) {
            (Some(c), _) => format!(
                "compass {:.0}°{} at the screen's top · back camera looks {:.0}°, pitch {:+.0}° ({})",
                c.bearing_deg,
                c.accuracy_deg.map(|a| format!(" ±{a:.0}°")).unwrap_or_default(),
                c.camera.0,
                c.camera.1,
                if c.locate { "the locate button's compass" } else { "a compass opened for this call" },
            ),
            (None, Some(e)) => format!("compass: {e}"),
            (None, None) if !self.compass_available => "compass: none in this build".to_string(),
            (None, None) => "compass: no reading yet".to_string(),
        };
        format!("{fix}\n{compass}")
    }
}

/// Send words and files to the chosen session.
fn push(link: &mut MidasLink, text: &str, files: Vec<Value>) {
    let (Some(token), Some(session)) = (link.config.token.clone(), link.config.send_to.clone())
    else {
        link.shared().message = Some("Pair first, and choose a session to send to.".into());
        return;
    };
    let words = if link.text_draft.trim().is_empty() {
        text.to_string()
    } else {
        format!("{}\n\n{text}", link.text_draft.trim())
    };
    let body = json!({"session": session, "text": words, "files": files});
    let hub = link.config.hub.clone();
    link.worker().push(&hub, &token, body);
    link.text_draft.clear();
}

/// Developer tools ▸ Midas.
pub fn midas_tab(
    ui: &mut egui::Ui,
    link: &mut MidasLink,
    log: &HighPrecisionLog,
    commands: &mut Commands,
) {
    theme::section(ui, "Midas");
    let message = link.shared().message.clone();
    if !link.config.paired() {
        theme::note(ui, "Pair this app with your Midas hub, and your sessions can ask it for screenshots, camera frames, the precision log and sensor recordings while it is open.");
        theme::note(ui, "On the Midas page: Settings > This device > Paired apps > Pair a device. Type the code it shows here.");
        ui.horizontal(|ui| {
            ui.label("hub");
            ui.text_edit_singleline(&mut link.hub_draft);
        });
        ui.horizontal(|ui| {
            ui.label("code");
            ui.text_edit_singleline(&mut link.code_draft);
        });
        if theme::primary_button_sized(ui, "Pair", 140.0).clicked()
            && !link.code_draft.trim().is_empty()
        {
            let (hub, code) = (
                link.hub_draft.trim().to_string(),
                link.code_draft.trim().to_string(),
            );
            link.worker().pair(
                &hub,
                &code,
                &crate::platform::precise_sensors::device_name(),
            );
        }
        if let Some(m) = message {
            theme::note(ui, &m);
        }
        return;
    }
    let state = link.shared().state.clone();
    ui.label(format!("{} · {}", link.config.name, link.config.hub));
    theme::checkbox(ui, &mut link.config.on, "Link on");
    theme::note(
        ui,
        if link.config.on {
            &state
        } else {
            "Off: no session can reach this app."
        },
    );
    if let Some((tool, session)) = link.recent_call(30.0) {
        theme::note(ui, &format!("{session} asked for {tool}"));
    }
    ui.separator();
    theme::section(ui, "Send to a session");
    let sessions = link.shared().sessions.clone();
    let chosen = link.config.send_to.clone();
    let label = chosen.as_deref().map_or_else(
        || "choose…".to_string(),
        |id| {
            sessions
                .iter()
                .find(|(s, _)| s == id)
                .map_or_else(|| id.to_string(), |(s, t)| format!("{s} · {t}"))
        },
    );
    ui.horizontal(|ui| {
        egui::ComboBox::from_id_salt("midas_send_to")
            .selected_text(label)
            .width(200.0)
            .show_ui(ui, |ui| {
                for (id, title) in &sessions {
                    ui.selectable_value(
                        &mut link.config.send_to,
                        Some(id.clone()),
                        format!("{id} · {title}"),
                    );
                }
            });
        if ui.button("Refresh").clicked() {
            let (hub, token) = (
                link.config.hub.clone(),
                link.config.token.clone().unwrap_or_default(),
            );
            link.worker().sessions(&hub, &token);
        }
    });
    ui.add(
        egui::TextEdit::multiline(&mut link.text_draft)
            .hint_text("a few words with it (optional)")
            .desired_rows(2),
    );
    ui.horizontal_wrapped(|ui| {
        if ui.button("Precision log").clicked() {
            use base64::Engine as _;
            let file = json!({"name": "precision.log", "data": base64::engine::general_purpose::STANDARD.encode(log.text())});
            push(link, "The precision log.", vec![file]);
        }
        if ui.button("Screenshot").clicked() && link.push_shot.is_none() {
            link.push_shot = Some(capture(commands, |png| png));
        }
        if ui.button("Words only").clicked() && !link.text_draft.trim().is_empty() {
            push(link, "", vec![]);
        }
    });
    if let Some(m) = message {
        theme::note(ui, &m);
    }
    ui.separator();
    if ui.button("Unpair").clicked() {
        link.config = LinkConfig {
            hub: link.config.hub.clone(),
            ..LinkConfig::default()
        };
        link.shared().message = Some("Unpaired here. Forget it on the Midas page too.".into());
    }
}

/// How long the button glows after a call comes in.
const GLOW_S: f32 = 1.5;
/// The card beside the button: fixed, because an anchored size-to-content
/// `Area` grows a little every frame (`map-chrome.md`).
const CARD_WIDTH: f32 = 230.0;

/// The Midas disc in the top-right corner, there exactly while the link is up
/// — not greyed when it is down, gone — because a session can then drive this
/// app as its account: walk, talk, move the camera. A green dot says linked; a
/// call coming in makes it glow rather than spell the call out, and a tap
/// opens a card with which session asked for what, and as whom.
///
/// It replaced a text chip at the top centre and, on the phone, the editor's
/// activity box (`mcp::overlay`), which between them covered the top of the
/// map with a socket path and a tool name wrapped one syllable a line.
#[allow(clippy::too_many_arguments)]
pub fn draw_link_button(
    egui: crate::systems::egui_manager::EguiRef,
    mut link: ResMut<MidasLink>,
    identity: Option<Res<crate::resources::AuthIdentity>>,
    right_panel: Res<crate::resources::RightPanel>,
    driver: Res<crate::systems::driver::DriverState>,
    mut seen: Local<(bool, Option<web_time::Instant>, Option<web_time::Instant>)>,
) {
    let up = link.config.on && link.linked();
    let last = link.shared().last_call.clone();
    // Timed out: gone before it is drawn.
    if link
        .told
        .as_ref()
        .and_then(|t| t.until)
        .is_some_and(|u| web_time::Instant::now() >= u)
    {
        link.told = None;
    }
    // The link and the call change on the worker's thread, and a `tell` in
    // `serve_calls`, which egui never hears of: without a repaint the button
    // would appear, glow, or speak only at the next touch
    // (`egui-quiet-frame-staleness.md`).
    let now_seen = (
        up,
        last.as_ref().map(|(_, _, at)| *at),
        link.told.as_ref().map(|t| t.at),
    );
    if *seen != now_seen {
        *seen = now_seen;
        egui.ctx().request_repaint();
    }
    // Under an open panel, or in driver mode, whose trip strip has this corner.
    if !up || !right_panel.is(crate::resources::RightPanel::None) || driver.active {
        link.card_open = false;
        return;
    }
    let age = last.as_ref().map(|(_, _, at)| at.elapsed().as_secs_f32());
    let glow = age.filter(|a| *a < GLOW_S).map(|a| 1.0 - a / GLOW_S);
    if glow.is_some() {
        egui.animating();
    }
    let ctx = egui.ctx();
    let size = theme::ROUND_BUTTON_SIZE;
    let mut toggle = false;
    // A second column, left of the map's round buttons under the search bar
    // (`dock::right_column`): the top right corner is your face now.
    let left_of_column = theme::OSD_MARGIN + size + theme::OSD_GAP;
    egui::Area::new(egui::Id::new("midas_link_button"))
        .anchor(
            egui::Align2::RIGHT_TOP,
            egui::vec2(-left_of_column, crate::systems::dock::reserved_top()),
        )
        .show(ctx, |ui| {
            let (resp, face) = theme::map_disc(
                ui,
                size,
                false,
                theme::p().accent_ring,
                egui::Sense::click(),
            );
            let resp = theme::named(ui, resp, "Midas");
            let p = ui.painter();
            if let Some(g) = glow {
                p.circle_stroke(
                    face.center,
                    face.radius + 2.0 + 6.0 * (1.0 - g),
                    egui::Stroke::new(3.0 * g, theme::midas_gold().gamma_multiply(g)),
                );
            }
            theme::paint_midas_mark(p, face.center, face.radius);
            let dot = face.center + egui::vec2(1.0, -1.0) * face.radius * 0.74;
            p.circle_filled(dot, 5.0, theme::CALL_ACCEPT_GREEN);
            p.circle_stroke(dot, 5.0, egui::Stroke::new(1.5, theme::p().ink));
            toggle = resp.on_hover_text("Midas").clicked();
        });
    // The card and the bubble share the place left of the button; with both
    // up, the bubble stands under the card.
    let beside = -(left_of_column + size + theme::OSD_GAP);
    let top = crate::systems::dock::reserved_top();
    let bubble_top = if link.card_open {
        draw_link_card(
            ctx,
            &mut link,
            identity.as_deref(),
            last.as_ref(),
            age,
            beside,
            top,
            toggle,
        ) + theme::OSD_GAP
    } else {
        link.card_open = toggle;
        top
    };
    draw_told(ctx, &mut link, beside, bubble_top, bubble_top == top, size);
}

/// The card a tap on the button opens: which session asked for what last,
/// and as whom. Returns its bottom edge.
#[allow(clippy::too_many_arguments)]
fn draw_link_card(
    ctx: &egui::Context,
    link: &mut MidasLink,
    identity: Option<&crate::resources::AuthIdentity>,
    last: Option<&(String, String, web_time::Instant)>,
    age: Option<f32>,
    beside: f32,
    top: f32,
    toggle: bool,
) -> f32 {
    let who = identity.and_then(|i| i.username.clone());
    let card = egui::Area::new(egui::Id::new("midas_link_card"))
        .anchor(egui::Align2::RIGHT_TOP, egui::vec2(beside, top))
        .show(ctx, |ui| {
            theme::overlay_frame().show(ui, |ui| {
                ui.set_width(CARD_WIDTH);
                ui.spacing_mut().item_spacing.y = 3.0;
                ui.label(
                    egui::RichText::new("Midas linked")
                        .size(theme::SIZE_SUB)
                        .color(theme::p().cream)
                        .strong(),
                );
                let call = match (last, age) {
                    (Some((tool, session, _)), Some(a)) => {
                        format!("{session} · {tool} · {}", ago(a))
                    }
                    _ => "No requests yet".to_string(),
                };
                ui.label(
                    egui::RichText::new(call)
                        .size(theme::SIZE_CHIP)
                        .color(theme::midas_gold()),
                );
                let acts = match who {
                    Some(name) => format!("Sessions can act as {name}"),
                    None => "Signed out: sessions act as nobody".to_string(),
                };
                ui.label(
                    egui::RichText::new(acts)
                        .size(theme::SIZE_TINY)
                        .color(theme::p().text_dim),
                );
            });
        });
    if age.is_some() {
        // "just now" turns into minutes on its own.
        ctx.request_repaint_after(std::time::Duration::from_secs(30));
    }
    // A second tap closes it, on the button or on the card.
    let tap = card.response.interact(egui::Sense::click());
    tap.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Close"));
    if toggle || tap.clicked() {
        link.card_open = false;
    }
    card.response.rect.bottom()
}

/// What a session said with `tell`, in a bubble beside the button — with a
/// tail pointing at it when nothing stands between — so the person can follow
/// a session driving the phone without leaving the app for the Midas page. A
/// tap takes it down.
fn draw_told(
    ctx: &egui::Context,
    link: &mut MidasLink,
    beside: f32,
    top: f32,
    by_the_button: bool,
    button: f32,
) {
    let Some(told) = &link.told else {
        return;
    };
    if let Some(until) = told.until {
        ctx.request_repaint_after(until.saturating_duration_since(web_time::Instant::now()));
    }
    let bubble = egui::Area::new(egui::Id::new("midas_link_told"))
        .anchor(egui::Align2::RIGHT_TOP, egui::vec2(beside, top))
        .show(ctx, |ui| {
            theme::overlay_frame()
                .stroke(egui::Stroke::new(
                    1.0,
                    theme::midas_gold().gamma_multiply(0.6),
                ))
                .show(ui, |ui| {
                    ui.set_width(CARD_WIDTH);
                    ui.spacing_mut().item_spacing.y = 4.0;
                    ui.label(
                        egui::RichText::new(&told.session)
                            .size(theme::SIZE_CHIP)
                            .color(theme::midas_gold())
                            .strong(),
                    );
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(&told.text)
                                .size(theme::SIZE_BODY)
                                .color(theme::p().cream),
                        )
                        .wrap(),
                    );
                });
        });
    let rect = bubble.response.rect;
    if by_the_button {
        // Level with the disc's centre, across the gap to it.
        let y = rect.top() + button / 2.0;
        let tail = vec![
            egui::pos2(rect.right() - 1.0, y - 7.0),
            egui::pos2(rect.right() + theme::OSD_GAP - 1.0, y),
            egui::pos2(rect.right() - 1.0, y + 7.0),
        ];
        ctx.layer_painter(bubble.response.layer_id)
            .add(egui::Shape::convex_polygon(
                tail,
                theme::p().overlay,
                egui::Stroke::NONE,
            ));
    }
    let tap = bubble.response.interact(egui::Sense::click());
    tap.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Dismiss"));
    if tap.clicked() {
        link.told = None;
    }
}

/// How long ago, in the card's words.
fn ago(secs: f32) -> String {
    match secs as u64 {
        0..60 => "just now".to_string(),
        s @ 60..3600 => format!("{} min ago", s / 60),
        s => format!("{} h ago", s / 3600),
    }
}

mod worker {
    //! The link's own thread: one socket and one loop on a current-thread
    //! runtime, as the simulator's link has (`network.rs`), and plain HTTP for
    //! pairing, the session list and a push on threads of their own.

    use std::sync::{Arc, Mutex};

    use futures_util::{SinkExt, StreamExt};
    use serde_json::{json, Value};
    use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
    use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Message};

    use super::{Call, Shared};

    /// The MCP server's tools, in-process; nothing where the client has none.
    #[cfg(mcp)]
    pub type Tools = Arc<cartopolis_mcp::local::LocalTools>;
    #[cfg(not(mcp))]
    pub type Tools = std::convert::Infallible;

    enum Cmd {
        Link(String, String),
        Unlink,
        /// A call's JSON-RPC response, made into its message on this thread:
        /// a screenshot's is a megabyte of base64, not for the frame loop.
        Answer(u64, Value),
        /// The app left the screen (`true`) or came back.
        Away(bool),
    }

    /// The hub pings every 25 s: a link that has heard nothing for this long
    /// is dead, however it looks. A network change leaves a socket that reads
    /// nothing until the OS gives up on it, minutes on, with the button green.
    const SILENT_S: u64 = 70;
    /// Coming back to the screen, a link silent this long is opened again at
    /// once rather than trusted: the phone may have changed networks meanwhile.
    const RESUME_SILENT_S: u64 = 30;
    /// The longest wait between tries. The hub waits 90 s for a phone that a
    /// session wants; this was 60 s, which one unlucky wait used up.
    const BACKOFF_MAX_S: f32 = 10.0;

    /// The next wait, doubled up to [`BACKOFF_MAX_S`], and spread by up to a
    /// quarter so phones that lost one network do not all come back at once.
    fn next_backoff(backoff: f32) -> f32 {
        let nanos = web_time::SystemTime::now()
            .duration_since(web_time::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
        let spread = 0.75 + 0.5 * (nanos % 1000) as f32 / 1000.0;
        (backoff * 2.0).min(BACKOFF_MAX_S) * spread
    }

    pub struct Worker {
        tx: UnboundedSender<Cmd>,
        shared: Arc<Mutex<Shared>>,
    }

    fn lock(s: &Arc<Mutex<Shared>>) -> std::sync::MutexGuard<'_, Shared> {
        s.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// `https://host` for a bare host; a URL as it is.
    fn http_base(hub: &str) -> String {
        if hub.starts_with("http://") || hub.starts_with("https://") {
            hub.trim_end_matches('/').to_string()
        } else {
            format!("https://{}", hub.trim_end_matches('/'))
        }
    }

    fn ws_url(hub: &str) -> String {
        let base = http_base(hub);
        let ws = base.strip_prefix("https://").map_or_else(
            || format!("ws://{}", base.trim_start_matches("http://")),
            |h| format!("wss://{h}"),
        );
        format!("{ws}/v1/device/link")
    }

    /// A request to the hub's device paths, with the token if there is one; the
    /// status says the hub's refusal (`platform::http` keeps no body with one).
    fn ask(
        hub: &str,
        method: &str,
        path: &str,
        token: Option<&str>,
        body: Option<&Value>,
    ) -> Result<Value, String> {
        use crate::platform::http::Request;
        let url = format!("{}{path}", http_base(hub));
        let mut req = if method == "POST" {
            Request::post(url)
        } else {
            Request::get(url)
        };
        req = req
            .header("Content-Type", "application/json")
            .timeout(std::time::Duration::from_secs(60));
        if let Some(t) = token {
            req = req.header("X-Midas-Device", t);
        }
        if let Some(b) = body {
            req = req.body(b.to_string().into_bytes());
        }
        let bytes = futures_lite::future::block_on(crate::platform::http::fetch(req))?;
        serde_json::from_slice(&bytes).map_err(|e| e.to_string())
    }

    impl Worker {
        pub fn start(shared: Arc<Mutex<Shared>>, tools: Option<Tools>) -> Self {
            let (tx, rx) = unbounded_channel();
            let s = shared.clone();
            let answers = tx.clone();
            std::thread::Builder::new()
                .name("midas-link".into())
                .spawn(move || {
                    let Ok(rt) = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                    else {
                        lock(&s).state = "could not start the link's runtime".into();
                        return;
                    };
                    rt.block_on(run(rx, answers, s, tools));
                })
                .ok();
            Self { tx, shared }
        }

        pub fn link(&self, hub: &str, token: &str) {
            let _ = self.tx.send(Cmd::Link(hub.to_string(), token.to_string()));
        }

        pub fn unlink(&self) {
            let _ = self.tx.send(Cmd::Unlink);
        }

        pub fn reply(&self, id: u64, response: Value) {
            let _ = self.tx.send(Cmd::Answer(id, response));
        }

        /// The app left the screen or came back: the hub is told, and a link
        /// that may have died meanwhile is opened again.
        pub fn away(&self, away: bool) {
            let _ = self.tx.send(Cmd::Away(away));
        }

        pub fn pair(&self, hub: &str, code: &str, name: &str) {
            let (shared, hub, code, name) = (
                self.shared.clone(),
                hub.to_string(),
                code.to_string(),
                name.to_string(),
            );
            lock(&shared).message = Some("Pairing…".into());
            std::thread::spawn(move || {
                let body = json!({"code": code, "name": name, "app": "cartopolis", "version": env!("CARGO_PKG_VERSION")});
                let r = ask(&hub, "POST", "/v1/device/pair", None, Some(&body));
                let mut s = lock(&shared);
                match r {
                    Ok(v) => match (v["token"].as_str(), v["id"].as_str()) {
                        (Some(t), Some(id)) => {
                            s.paired = Some((t.to_string(), id.to_string(), v["name"].as_str().unwrap_or(&name).to_string()));
                            s.message = Some("Paired.".into());
                        }
                        _ => s.message = Some("The hub answered, but with no token.".into()),
                    },
                    Err(e) if e.ends_with("status code 400") => s.message = Some("That code is wrong, spent or more than ten minutes old: make a new one on the page.".into()),
                    Err(e) => s.message = Some(format!("Could not reach the hub: {e}")),
                }
            });
        }

        pub fn sessions(&self, hub: &str, token: &str) {
            let (shared, hub, token) = (self.shared.clone(), hub.to_string(), token.to_string());
            std::thread::spawn(move || {
                let r = ask(&hub, "GET", "/v1/device/sessions", Some(&token), None);
                let mut s = lock(&shared);
                match r {
                    Ok(v) => {
                        s.sessions = v["sessions"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(|x| {
                                Some((
                                    x["id"].as_str()?.to_string(),
                                    x["title"].as_str().unwrap_or("").to_string(),
                                ))
                            })
                            .collect();
                        s.message = None;
                    }
                    Err(e) if e.ends_with("status code 401") => s.unpaired = true,
                    Err(e) => s.message = Some(format!("Could not list the sessions: {e}")),
                }
            });
        }

        pub fn push(&self, hub: &str, token: &str, body: Value) {
            let (shared, hub, token) = (self.shared.clone(), hub.to_string(), token.to_string());
            lock(&shared).message = Some("Sending…".into());
            std::thread::spawn(move || {
                let r = ask(&hub, "POST", "/v1/device/push", Some(&token), Some(&body));
                let mut s = lock(&shared);
                s.message = Some(match r {
                    Ok(_) => format!(
                        "Sent to {}.",
                        body["session"].as_str().unwrap_or("the session")
                    ),
                    Err(e) if e.ends_with("status code 401") => {
                        s.unpaired = true;
                        "The hub no longer knows this app: pair it again.".to_string()
                    }
                    Err(e) => format!("Not sent: {e}"),
                });
            });
        }
    }

    type Socket = tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >;

    async fn connect(hub: &str, token: &str) -> Result<Socket, String> {
        crate::systems::network::install_crypto_provider();
        let mut req = ws_url(hub)
            .into_client_request()
            .map_err(|e| e.to_string())?;
        req.headers_mut().insert(
            "X-Midas-Device",
            token
                .parse()
                .map_err(|_| "a token that is not a header".to_string())?,
        );
        match tokio_tungstenite::connect_async(req).await {
            Ok((socket, _)) => Ok(socket),
            Err(tokio_tungstenite::tungstenite::Error::Http(r)) if r.status() == 401 => {
                Err("unpaired".into())
            }
            Err(e) => Err(e.to_string()),
        }
    }

    /// The hub's message for a call's answer: its JSON-RPC response.
    fn result_message(id: u64, response: &Value) -> String {
        json!({"type": "result", "id": id, "response": response}).to_string()
    }

    /// What the hello offers: this module's tools, then every MCP tool whose
    /// name they do not already take.
    fn offered(own: &Value, mcp: &[Value]) -> Value {
        let mut all = own.as_array().cloned().unwrap_or_default();
        for t in mcp {
            if !all.iter().any(|o| o["name"] == t["name"]) {
                all.push(t.clone());
            }
        }
        Value::Array(all)
    }

    /// The MCP server's tools as the hello lists them; none if it will not say.
    #[cfg(mcp)]
    async fn mcp_tools(tools: Option<&Tools>) -> Vec<Value> {
        let Some(tools) = tools else {
            return Vec::new();
        };
        tools.list().await.unwrap_or_else(|e| {
            tracing::warn!(error = %e, "midas link: the MCP tools are not offered");
            Vec::new()
        })
    }
    #[cfg(not(mcp))]
    async fn mcp_tools(_: Option<&Tools>) -> Vec<Value> {
        Vec::new()
    }

    /// Answer a call to an MCP tool from the in-process session, on a task of
    /// its own: a `read_chat` or a walk may wait minutes, and must hold up
    /// neither the socket nor the next call.
    #[cfg(mcp)]
    fn relay(tools: &Tools, call: Call, answers: &UnboundedSender<Cmd>) {
        let (tools, answers) = (tools.clone(), answers.clone());
        tokio::spawn(async move {
            let result = tools
                .call(call.tool.clone(), call.args.clone())
                .await
                .unwrap_or_else(
                    |e| json!({"content": [{"type": "text", "text": e}], "isError": true}),
                );
            let response = json!({"jsonrpc": "2.0", "id": call.rpc, "result": result});
            let _ = answers.send(Cmd::Answer(call.id, response));
        });
    }

    /// Hold the link the panel asks for, until told otherwise.
    async fn run(
        mut cmds: UnboundedReceiver<Cmd>,
        answers: UnboundedSender<Cmd>,
        shared: Arc<Mutex<Shared>>,
        tools: Option<Tools>,
    ) {
        #[cfg(not(mcp))]
        let _ = &answers;
        let own = super::tools();
        // Listed once: the server's tools are fixed for the life of the build.
        let mcp = mcp_tools(tools.as_ref()).await;
        let offer = offered(&own, &mcp);
        let mut target: Option<(String, String)> = None;
        let mut backoff = 1.0f32;
        let mut away = false;
        loop {
            let Some((hub, token)) = target.clone() else {
                {
                    let mut s = lock(&shared);
                    s.linked = false;
                    s.state = "off".into();
                }
                match cmds.recv().await {
                    Some(Cmd::Link(h, t)) => target = Some((h, t)),
                    Some(Cmd::Away(a)) => away = a,
                    Some(_) => {}
                    None => return,
                }
                continue;
            };
            lock(&shared).state = format!("connecting to {hub}…");
            match connect(&hub, &token).await {
                Ok(socket) => {
                    backoff = 1.0;
                    let (mut sink, mut stream) = socket.split();
                    // `ack`: each call is acknowledged as it arrives, so the
                    // hub can tell a dead socket in seconds rather than at the
                    // call's timeout. `away` follows, if the app is off screen.
                    let hello = json!({"type": "hello", "tools": offer, "build": super::build(), "ack": true});
                    let mut greeting = vec![hello];
                    if away {
                        greeting.push(json!({"type": "away", "away": true}));
                    }
                    let mut sent = true;
                    for m in greeting {
                        sent = sent && sink.send(Message::Text(m.to_string().into())).await.is_ok();
                    }
                    if sent {
                        {
                            let mut s = lock(&shared);
                            s.linked = true;
                            s.state = "linked: your sessions can reach this app".into();
                        }
                        hold(
                            &mut sink,
                            &mut stream,
                            &mut cmds,
                            &mut target,
                            &mut away,
                            &own,
                            tools.as_ref(),
                            &answers,
                            &shared,
                        )
                        .await;
                        if target
                            .as_ref()
                            .is_some_and(|t| *t != (hub.clone(), token.clone()))
                        {
                            // Told to link elsewhere: at once.
                            lock(&shared).linked = false;
                            continue;
                        }
                    }
                    lock(&shared).linked = false;
                    if lock(&shared).state.starts_with("linked") {
                        lock(&shared).state = "the link dropped; linking again".into();
                    }
                }
                Err(e) if e == "unpaired" => {
                    let mut s = lock(&shared);
                    s.unpaired = true;
                    s.state = "the hub no longer knows this app: pair it again".into();
                    target = None;
                    continue;
                }
                Err(e) => {
                    lock(&shared).state =
                        format!("not linked ({e}); trying again in {backoff:.0} s");
                }
            }
            if target.is_none() {
                continue;
            }
            // Wait before trying again, still listening: Off, a new hub or the
            // app coming back to the screen must not wait.
            let sleep = tokio::time::sleep(std::time::Duration::from_secs_f32(backoff));
            tokio::pin!(sleep);
            tokio::select! {
                () = &mut sleep => backoff = next_backoff(backoff),
                cmd = cmds.recv() => match cmd {
                    Some(Cmd::Link(h, t)) => target = Some((h, t)),
                    Some(Cmd::Unlink) => target = None,
                    Some(Cmd::Away(a)) => { away = a; backoff = 1.0; }
                    // Answers to calls of a link that is gone: the hub has
                    // failed them already.
                    Some(Cmd::Answer(..)) => {}
                    None => return,
                },
            }
        }
    }

    /// How long the hub holds a call for a phone that is away.
    const HELD_S: u64 = 90;

    /// A call unanswered this long while the app is off screen is put in a
    /// notice. Sooner, and a tool that needs no frame (`me`, `read_chat`,
    /// `scripting_guide`) would flash one over the Midas page as it answers.
    const NOTICE_AFTER_MS: u128 = 2000;

    /// A call that came while the app was off screen.
    struct Held {
        id: u64,
        // Read only by the Android notice.
        #[cfg_attr(not(target_os = "android"), allow(dead_code))]
        session: String,
        #[cfg_attr(not(target_os = "android"), allow(dead_code))]
        tool: String,
        at: web_time::Instant,
    }

    /// The calls that came while the app was off screen, and the notice
    /// showing them on the phone (`MidasCall.java`): the person is usually
    /// reading the session on the Midas page, with the app behind it, and
    /// nothing answers there until they bring it back.
    #[derive(Default)]
    struct Waiting {
        calls: Vec<Held>,
        /// The newest call and the count the notice shows, if one is up.
        shown: Option<(u64, usize)>,
    }

    impl Waiting {
        fn add(&mut self, id: u64, session: &str, tool: &str) {
            self.calls.push(Held {
                id,
                session: session.to_string(),
                tool: tool.to_string(),
                at: web_time::Instant::now(),
            });
        }

        fn answered(&mut self, id: u64) {
            self.calls.retain(|c| c.id != id);
            self.update();
        }

        /// Back on screen: the activity took the notice down as it resumed.
        fn back(&mut self) {
            self.calls.clear();
            self.shown = None;
        }

        /// What the notice should show: every call the hub still holds, once
        /// one of them has waited [`NOTICE_AFTER_MS`].
        fn wanted(&mut self) -> Option<(u64, usize)> {
            self.calls.retain(|c| c.at.elapsed().as_secs() < HELD_S);
            let due = self
                .calls
                .iter()
                .any(|c| c.at.elapsed().as_millis() >= NOTICE_AFTER_MS);
            let newest = self.calls.last()?;
            due.then_some((newest.id, self.calls.len()))
        }

        fn update(&mut self) {
            let wanted = self.wanted();
            if wanted == self.shown {
                return;
            }
            self.shown = wanted;
            #[cfg(target_os = "android")]
            {
                let show = self
                    .calls
                    .last()
                    .filter(|_| wanted.is_some())
                    .map(|c| (c.session.as_str(), c.tool.as_str(), self.calls.len()));
                if let Err(e) = crate::platform::android::midas_waiting(show) {
                    tracing::warn!(error = %e, "midas link: could not show the waiting call");
                }
            }
        }
    }

    /// Carry calls on one open link until it ends: closed, silent for
    /// [`SILENT_S`], or left for another target.
    #[allow(clippy::too_many_arguments)]
    async fn hold(
        sink: &mut futures_util::stream::SplitSink<Socket, Message>,
        stream: &mut futures_util::stream::SplitStream<Socket>,
        cmds: &mut UnboundedReceiver<Cmd>,
        target: &mut Option<(String, String)>,
        away: &mut bool,
        own: &Value,
        tools: Option<&Tools>,
        answers: &UnboundedSender<Cmd>,
        shared: &Arc<Mutex<Shared>>,
    ) {
        #[cfg(not(mcp))]
        let _ = (tools, answers);
        let mut heard = web_time::Instant::now();
        // Each second: the notice wants its first, a silent link its last.
        let mut check = tokio::time::interval(std::time::Duration::from_secs(1));
        let mut waiting = Waiting::default();
        loop {
            tokio::select! {
                _ = check.tick() => {
                    if heard.elapsed().as_secs() >= SILENT_S {
                        lock(shared).state = format!("the hub went quiet for {SILENT_S} s; linking again");
                        return;
                    }
                    waiting.update();
                },
                msg = stream.next() => match msg {
                    Some(Ok(Message::Text(t))) => {
                        heard = web_time::Instant::now();
                        let v: Value = serde_json::from_str(t.as_str()).unwrap_or(Value::Null);
                        if v["type"] == "call" {
                            // Before anything else, so a call that waits for
                            // the screen still reads as received.
                            let ack = json!({"type": "ack", "id": v["id"]});
                            if sink.send(Message::Text(ack.to_string().into())).await.is_err() { return }
                            let req = &v["request"];
                            let call = Call {
                                id: v["id"].as_u64().unwrap_or(0),
                                session: v["session"].as_str().unwrap_or("").to_string(),
                                tool: req["params"]["name"].as_str().unwrap_or("").to_string(),
                                args: req["params"]["arguments"].clone(),
                                rpc: req["id"].clone(),
                            };
                            if *away {
                                waiting.add(call.id, &call.session, &call.tool);
                            }
                            let mut s = lock(shared);
                            s.last_call = Some((call.tool.clone(), call.session.clone(), web_time::Instant::now()));
                            let ours = own.as_array().is_some_and(|o| o.iter().any(|t| t["name"] == call.tool.as_str()));
                            #[cfg(mcp)]
                            if let (false, Some(tools)) = (ours, tools) {
                                drop(s);
                                relay(tools, call, answers);
                                continue;
                            }
                            #[cfg(not(mcp))]
                            let _ = ours;
                            s.calls.push(call);
                        }
                    }
                    Some(Ok(Message::Ping(p))) => {
                        heard = web_time::Instant::now();
                        let _ = sink.send(Message::Pong(p)).await;
                    }
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => return,
                    Some(Ok(_)) => heard = web_time::Instant::now(),
                },
                cmd = cmds.recv() => match cmd {
                    Some(Cmd::Answer(id, response)) => {
                        // A tool that needs no frame answers off screen too.
                        waiting.answered(id);
                        if sink.send(Message::Text(result_message(id, &response).into())).await.is_err() { return }
                    }
                    Some(Cmd::Away(a)) => {
                        *away = a;
                        if !a {
                            waiting.back();
                        }
                        // Back on screen after a while: the phone may be on
                        // another network, so a quiet link is not trusted.
                        if !a && heard.elapsed().as_secs() >= RESUME_SILENT_S {
                            return;
                        }
                        let m = json!({"type": "away", "away": a});
                        if sink.send(Message::Text(m.to_string().into())).await.is_err() { return }
                    }
                    Some(Cmd::Link(h, t)) => { *target = Some((h, t)); return }
                    Some(Cmd::Unlink) => { *target = None; let _ = sink.send(Message::Close(None)).await; return }
                    None => { *target = None; return }
                },
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn a_bare_host_is_the_secure_one() {
            assert_eq!(
                ws_url("midas.cartopolis.org"),
                "wss://midas.cartopolis.org/v1/device/link"
            );
            assert_eq!(
                ws_url("http://10.44.0.1:4791/"),
                "ws://10.44.0.1:4791/v1/device/link"
            );
            assert_eq!(
                http_base("midas.cartopolis.org"),
                "https://midas.cartopolis.org"
            );
        }

        #[test]
        fn a_call_held_off_screen_is_shown_once_it_waits() {
            let ago = |ms| web_time::Instant::now() - std::time::Duration::from_millis(ms);
            let mut w = Waiting::default();
            w.add(1, "s1", "screenshot");
            assert_eq!(w.wanted(), None, "a call answered at once flashes nothing");
            w.calls[0].at = ago(2500);
            w.add(2, "s1", "gnss");
            assert_eq!(w.wanted(), Some((2, 2)), "the newest, and how many wait");
            w.calls[0].at = ago(HELD_S * 1000 + 1);
            assert_eq!(w.wanted(), None, "the hub gave up on the old one");
            w.answered(2);
            assert!(w.calls.is_empty());
        }

        #[test]
        fn the_backoff_stays_short_and_spread() {
            let mut b = 1.0;
            for _ in 0..20 {
                b = next_backoff(b);
                assert!((0.75..=BACKOFF_MAX_S * 1.25).contains(&b), "{b}");
            }
        }

        /// The next text message the stand-in hub gets, as JSON.
        async fn heard(
            ws: &mut tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
        ) -> Value {
            loop {
                if let Message::Text(t) = ws.next().await.unwrap().unwrap() {
                    return serde_json::from_str(t.as_str()).unwrap();
                }
            }
        }

        /// The worker against a stand-in hub: its hello offers acks, it
        /// acknowledges a call before anything answers it, and it says when
        /// the app leaves the screen, on the link it keeps.
        #[test]
        fn a_call_is_acknowledged_and_leaving_the_screen_is_said() {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async {
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let hub = format!("http://{}", listener.local_addr().unwrap());
                let shared: Arc<Mutex<Shared>> = Arc::default();
                let worker = Worker::start(shared.clone(), None);
                worker.link(&hub, "a-token-of-the-right-length-000000");
                let (tcp, _) = listener.accept().await.unwrap();
                let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
                let hello = heard(&mut ws).await;
                assert_eq!((hello["type"].as_str(), hello["ack"].as_bool()), (Some("hello"), Some(true)));

                let call = json!({"type": "call", "id": 7, "session": "s1", "request":
                    {"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "gnss", "arguments": {}}}});
                ws.send(Message::Text(call.to_string().into())).await.unwrap();
                assert_eq!(heard(&mut ws).await, json!({"type": "ack", "id": 7}));

                worker.away(true);
                assert_eq!(heard(&mut ws).await, json!({"type": "away", "away": true}));
                // After the ack, and on the link's one thread after the call: handed on by now.
                assert_eq!(lock(&shared).calls.len(), 1, "handed on to be answered");
                worker.away(false);
                assert_eq!(heard(&mut ws).await, json!({"type": "away", "away": false}), "a fresh link is kept");

                worker.reply(7, json!({"jsonrpc": "2.0", "id": 1, "result": {"content": []}}));
                let answer = heard(&mut ws).await;
                assert_eq!((answer["type"].as_str(), answer["id"].as_u64()), (Some("result"), Some(7)));
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tool_has_a_name_and_a_schema() {
        for t in tools().as_array().unwrap() {
            assert!(t["name"].as_str().is_some_and(|n| !n.is_empty()));
            assert_eq!(t["inputSchema"]["type"], "object", "{t}");
        }
    }

    #[test]
    fn a_tell_is_signed_and_bounded_and_an_empty_one_clears() {
        let call = |args: Value| Call {
            id: 1,
            session: "s669".into(),
            tool: "tell".into(),
            args,
            rpc: json!(1),
        };
        let now = web_time::Instant::now();
        let kept = Told::from_call(&call(json!({"text": "  Tap the camera.  "})), now).unwrap();
        assert_eq!(kept.session, "s669");
        assert_eq!(kept.text, "Tap the camera.");
        assert!(kept.until.is_none(), "no seconds keeps it until tapped");
        let timed = Told::from_call(&call(json!({"text": "hi", "seconds": 5})), now).unwrap();
        assert_eq!(timed.until, Some(now + std::time::Duration::from_secs(5)));
        let long = Told::from_call(&call(json!({"text": "x".repeat(1000)})), now).unwrap();
        assert_eq!(long.text.chars().count(), TELL_MAX_CHARS);
        assert!(Told::from_call(&call(json!({"text": ""})), now).is_none());
        assert!(Told::from_call(&call(json!({})), now).is_none());
    }

    #[test]
    fn the_card_says_how_long_ago_in_whole_units() {
        assert_eq!(ago(0.0), "just now");
        assert_eq!(ago(59.9), "just now");
        assert_eq!(ago(60.0), "1 min ago");
        assert_eq!(ago(3599.0), "59 min ago");
        assert_eq!(ago(7300.0), "2 h ago");
    }

    #[test]
    fn camera_frame_takes_an_optional_colour() {
        let tools = tools();
        let frame = tools
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["name"] == "camera_frame")
            .unwrap();
        assert_eq!(
            frame["inputSchema"]["properties"]["colour"]["type"],
            "boolean"
        );
        assert!(frame["inputSchema"]["required"].is_null());
    }

    fn picture(width: u32, height: u32, y: u8, u: u8, v: u8) -> cartopolis_calls::video::I420 {
        let c = (width.div_ceil(2) * height.div_ceil(2)) as usize;
        cartopolis_calls::video::I420 {
            width,
            height,
            y: vec![y; (width * height) as usize],
            u: vec![u; c],
            v: vec![v; c],
        }
    }

    #[test]
    fn a_camera_picture_turns_into_rgb() {
        let mut p = picture(2, 2, 128, 128, 128);
        p.y = vec![16, 235, 128, 128];
        let rgb = rgb_from_i420(&p);
        assert_eq!(rgb.len(), 2 * 2 * 3);
        assert_eq!(&rgb[0..3], &[0, 0, 0], "video black");
        assert_eq!(&rgb[3..6], &[255, 255, 255], "video white");
        assert!(rgb[6] == rgb[7] && rgb[7] == rgb[8], "no chroma is grey");
        // BT.601 red.
        let red = rgb_from_i420(&picture(2, 2, 81, 90, 240));
        assert!(red[0] > 240 && red[1] < 20 && red[2] < 20, "{red:?}");
    }

    #[test]
    fn a_colour_still_is_a_jpeg() {
        let (jpeg, caption) = still_jpeg(&picture(4, 2, 120, 100, 160)).unwrap();
        assert_eq!(&jpeg[..2], &[0xFF, 0xD8]);
        assert_eq!(caption, "back camera, 4×2 colour (taken at 4×2)");
    }

    #[test]
    fn colour_is_refused_while_another_holds_the_camera() {
        assert!(colour_refusal(false, false, false).is_none());
        assert!(colour_refusal(true, false, false)
            .unwrap()
            .starts_with("Precision mode has the back camera"));
        assert!(colour_refusal(false, true, false)
            .unwrap()
            .starts_with("The camera screen"));
        assert!(colour_refusal(true, true, true)
            .unwrap()
            .contains("being taken already"));
    }

    #[test]
    fn the_build_names_its_commit() {
        assert_eq!(
            build_line("1.0.0", Some("1.0.0+fc24fd3d")),
            "build 1.0.0 · commit fc24fd3d"
        );
        assert_eq!(
            build_line("1.0.0", Some("fc24fd3d")),
            "build 1.0.0 · commit fc24fd3d"
        );
        assert!(build_line("1.0.0", None).contains("commit unknown"));
    }

    fn somewhere() -> Whereabouts {
        Whereabouts {
            provider: true,
            fix: Some(crate::platform::location::GpsFix {
                lat: 53.219_383_3,
                lng: 6.566_5,
                accuracy_m: 4.0,
                speed_ms: Some(1.5),
                heading_deg: None,
                height_m: None,
            }),
            source: FixSource::Link,
            age: Some((3.2, true)),
            error: None,
            compass_available: true,
            compass: Some(CompassReading {
                bearing_deg: 92.4,
                accuracy_deg: Some(10.0),
                camera: (88.0, 4.0),
                locate: true,
            }),
            compass_error: None,
            waited_s: 0.0,
        }
    }

    #[test]
    fn the_ordinary_location_is_said_as_such() {
        let t = somewhere().text();
        assert_eq!(
            t,
            "fix 53.2193833,6.5665000 ±4.0 m (ordinary location, opened for this call) · 3 s old · good\n\
             speed 1.5 m/s · course not reported\n\
             compass 92° ±10° at the screen's top · back camera looks 88°, pitch +4° (the locate button's compass)"
        );
        let precise = Whereabouts {
            source: FixSource::Precision,
            ..somewhere()
        };
        assert!(precise.text().contains("(precision mode's watch)"));
    }

    #[test]
    fn a_fix_of_unknown_age_says_so() {
        let first = Whereabouts {
            age: Some((0.0, false)),
            ..somewhere()
        };
        assert!(first.text().contains("age unknown"));
        let old = Whereabouts {
            age: Some((40.0, false)),
            ..somewhere()
        };
        assert!(
            old.text().contains("at least 40 s old · stale"),
            "{}",
            old.text()
        );
    }

    #[test]
    fn no_fix_says_why() {
        let none = Whereabouts {
            fix: None,
            age: None,
            waited_s: 10.0,
            compass: None,
            ..somewhere()
        };
        assert!(none.text().starts_with("no fix yet: none came within 10 s"));
        assert!(none.text().ends_with("compass: no reading yet"));
        let refused = Whereabouts {
            error: Some("Location access was refused.".into()),
            ..none.clone()
        };
        assert!(refused
            .text()
            .starts_with("no fix: Location access was refused."));
        let desktop = Whereabouts {
            provider: false,
            compass_available: false,
            ..none
        };
        assert!(desktop.text().contains("no location provider"));
        assert!(desktop.text().ends_with("compass: none in this build"));
        assert!(desktop.settled(0.0), "nothing to wait for");
    }

    #[test]
    fn a_call_waits_a_little_for_a_fix_and_a_compass() {
        let waiting = Whereabouts {
            fix: None,
            age: None,
            compass: None,
            ..somewhere()
        };
        assert!(!waiting.settled(0.5));
        assert!(!waiting.settled(COMPASS_WAIT_S + 0.1), "still no fix");
        assert!(waiting.settled(FIX_WAIT_S));
        let fix_only = Whereabouts {
            compass: None,
            ..somewhere()
        };
        assert!(!fix_only.settled(0.0));
        assert!(fix_only.settled(COMPASS_WAIT_S));
        assert!(somewhere().settled(0.0));
    }

    #[test]
    fn a_frame_is_a_png() {
        let f = crate::platform::precise_sensors::Frame {
            t_ns: 1,
            width: 4,
            height: 2,
            luma: vec![0, 50, 100, 150, 200, 250, 255, 10],
        };
        let png = frame_png(&f).unwrap();
        assert_eq!(&png[1..4], b"PNG");
        assert!(
            frame_png(&crate::platform::precise_sensors::Frame {
                luma: vec![0; 3],
                ..f
            })
            .is_err(),
            "a frame of the wrong size"
        );
    }
}

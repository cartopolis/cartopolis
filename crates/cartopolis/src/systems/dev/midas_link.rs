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
//! The tools are this module's own rather than the desktop MCP server's
//! (`cartopolis_mcp`, Unix desktops only): a phone needs a different handful —
//! sensors, not editing — and a Bevy-free crate's job channel for six tools
//! would be more seam than tool.

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
const SHOT_MAX_PX: u32 = 1600;

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
    /// The newest call: its tool, its session and when, for the chip.
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
        out: Arc<Mutex<Option<Result<Vec<u8>, String>>>>,
    },
}

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
    worker: Option<worker::Worker>,
    /// What the worker was last told to link to, so a change re-links once.
    linked_to: Option<(String, String)>,
    /// A push asked for from the panel: (what, needs a screenshot first).
    push_shot: Option<Arc<Mutex<Option<Result<Vec<u8>, String>>>>>,
}

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
            worker: None,
            linked_to: None,
            push_shot: None,
        }
    }
}

impl MidasLink {
    fn shared(&self) -> std::sync::MutexGuard<'_, Shared> {
        self.shared
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Whether the link is up now, for the map's chip.
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

    fn worker(&mut self) -> &worker::Worker {
        let shared = self.shared.clone();
        self.worker
            .get_or_insert_with(|| worker::Worker::start(shared))
    }
}

/// The tools this app offers a session, as MCP lists them.
#[must_use]
pub fn tools() -> Value {
    let none = json!({"type": "object", "properties": {}});
    json!([
        {"name": "state", "description": "What the app is doing: its build, the platform, precision mode and what it says, the newest GNSS fix and the external receiver.", "inputSchema": none},
        {"name": "screenshot", "description": "The app's window as it is now, UI and all, as a PNG.", "inputSchema": none},
        {"name": "camera_frame", "description": "One frame from the back camera, greyscale as the tracker sees it, as a PNG. Switches precision mode on for the call when it is off (the camera takes a second or two to open) and off again after.", "inputSchema": none},
        {"name": "precision_log", "description": "The precision mode's log (Developer tools ▸ Precision): the camera, motion, magnetometer and GNSS lines the indoor walk test writes.", "inputSchema": none},
        {"name": "record", "description": "Record the motion sensors (accelerometer, gyroscope, magnetometer; device axes, on CLOCK_BOOTTIME) and each new GNSS fix for a while: two CSV files. Runs precision mode for the recording.",
         "inputSchema": {"type": "object", "properties": {"seconds": {"type": "integer", "description": format!("how long, at most {RECORD_MAX_S}")}}, "required": ["seconds"]}},
        {"name": "gnss", "description": "The newest GNSS fix and the external RTK receiver's report (its settings, state and events).", "inputSchema": none},
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
    use base64::Engine as _;
    json!({"content": [
        {"type": "image", "mimeType": "image/png", "data": base64::engine::general_purpose::STANDARD.encode(png)},
        {"type": "text", "text": caption},
    ]})
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

/// Ask for the window as it is; the PNG arrives in the slot returned.
fn capture(commands: &mut Commands) -> Arc<Mutex<Option<Result<Vec<u8>, String>>>> {
    use bevy::render::view::screenshot::{Screenshot, ScreenshotCaptured};
    let out: Arc<Mutex<Option<Result<Vec<u8>, String>>>> = Arc::default();
    let slot = out.clone();
    commands.spawn(Screenshot::primary_window()).observe(
        move |captured: On<ScreenshotCaptured>| {
            let image = captured.image.clone();
            let slot = slot.clone();
            // Encoding a phone's screen is a quarter second: not on the frame.
            crate::platform::workers::submit(
                "midas_link_shot",
                Box::new(move || {
                    let r = shot_png(&image);
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

/// Load the saved link once, keep it saved, and keep the worker linked to what it says.
pub fn drive_link(mut link: ResMut<MidasLink>) {
    let link = &mut *link;
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
/// recording or a screenshot once they have it.
pub fn serve_calls(
    mut commands: Commands,
    mut link: ResMut<MidasLink>,
    mut high: ResMut<HighPrecision>,
    log: Res<HighPrecisionLog>,
    store: Res<crate::systems::user_store::UserStore>,
) {
    let link = &mut *link;
    let calls = std::mem::take(&mut link.shared().calls);
    let mut answers: Vec<(Call, Value)> = Vec::new();
    for call in calls {
        let answer = match call.tool.as_str() {
            "state" => Some(text(state(&high))),
            "precision_log" => Some(if log.is_empty() {
                text("The log is empty: precision mode has not run since the app started (Developer tools ▸ Precision ▸ Start, or camera_frame / record).")
            } else {
                text(log.text())
            }),
            "gnss" => Some(text(gnss(&high, &store.data.gnss_receiver))),
            "screenshot" => {
                let out = capture(&mut commands);
                link.waiting.push(Waiting::Shot { call: call.clone(), out });
                None
            }
            "camera_frame" | "record" if !HighPrecision::available() => {
                Some(refusal("This device has no back camera and motion sensors the app can use: that takes the Android app on a phone."))
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
    let now = web_time::Instant::now();
    let mut still = Vec::new();
    for w in std::mem::take(&mut link.waiting) {
        match w {
            Waiting::Shot { call, out } => match take(&out) {
                Some(Ok(png)) => answers.push((call, png_result(&png, "the app's window"))),
                Some(Err(e)) => answers.push((
                    call,
                    refusal(format!("the window could not be captured: {e}")),
                )),
                None => still.push(Waiting::Shot { call, out }),
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
            w.reply(call.id, &response);
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

fn state(high: &HighPrecision) -> String {
    let fix = high.fix().map_or_else(
        || "none (precision mode is off, or no fix yet)".to_string(),
        |f| format!("{:.7},{:.7} ±{:.1} m", f.lat, f.lng, f.accuracy_m),
    );
    format!(
        "build {} ({})\nplatform {}\ndevice {}\nprecision mode {}{}\ngnss {fix}",
        env!("CARGO_PKG_VERSION"),
        option_env!("CARTO_BUILD_SHA").unwrap_or("no commit"),
        std::env::consts::OS,
        crate::platform::precise_sensors::device_name(),
        if high.on { "on" } else { "off" },
        high.hand_word()
            .map(|w| format!(" · {}", w.text()))
            .unwrap_or_default(),
    )
}

fn gnss(high: &HighPrecision, prefs: &crate::systems::gnss_receiver::ReceiverPrefs) -> String {
    let mut out = match high.fix() {
        Some(f) => format!(
            "fix {:.7},{:.7} ±{:.1} m (precision mode's own watch)\n",
            f.lat, f.lng, f.accuracy_m
        ),
        None => "no fix: precision mode is off, or has none yet\n".to_string(),
    };
    out.push_str(&crate::systems::gnss_receiver::report(prefs));
    out
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
            link.push_shot = Some(capture(commands));
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

/// The chip on the map while linked: says the link is open, and which session
/// just asked for what.
pub fn draw_link_chip(mut egui: crate::systems::egui_manager::EguiMut, link: Res<MidasLink>) {
    if !link.config.on || !link.linked() {
        return;
    }
    let recent = link.recent_call(4.0);
    let text = match &recent {
        Some((tool, session)) => format!("Midas · {session}: {tool}"),
        None => "Midas linked".to_string(),
    };
    if recent.is_some() {
        egui.animating();
    }
    let ctx = egui.ctx_mut();
    egui::Area::new(egui::Id::new("midas_link_chip"))
        .anchor(egui::Align2::CENTER_TOP, [0.0, theme::OSD_MARGIN])
        .interactable(false)
        .show(ctx, |ui| {
            theme::chip_frame().show(ui, |ui| {
                ui.label(egui::RichText::new(text).size(theme::SIZE_CHIP).color(
                    if recent.is_some() {
                        theme::ACCENT_BRIGHT
                    } else {
                        theme::TEXT_DIM
                    },
                ));
            });
        });
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

    enum Cmd {
        Link(String, String),
        Unlink,
        Send(String),
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
        pub fn start(shared: Arc<Mutex<Shared>>) -> Self {
            let (tx, rx) = unbounded_channel();
            let s = shared.clone();
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
                    rt.block_on(run(rx, s));
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

        pub fn reply(&self, id: u64, response: &Value) {
            let _ = self.tx.send(Cmd::Send(
                json!({"type": "result", "id": id, "response": response}).to_string(),
            ));
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

    /// Hold the link the panel asks for, until told otherwise.
    async fn run(mut cmds: UnboundedReceiver<Cmd>, shared: Arc<Mutex<Shared>>) {
        let mut target: Option<(String, String)> = None;
        let mut backoff = 2u64;
        loop {
            let Some((hub, token)) = target.clone() else {
                {
                    let mut s = lock(&shared);
                    s.linked = false;
                    s.state = "off".into();
                }
                match cmds.recv().await {
                    Some(Cmd::Link(h, t)) => target = Some((h, t)),
                    Some(_) => {}
                    None => return,
                }
                continue;
            };
            lock(&shared).state = format!("connecting to {hub}…");
            match connect(&hub, &token).await {
                Ok(socket) => {
                    backoff = 2;
                    let (mut sink, mut stream) = socket.split();
                    let hello =
                        json!({"type": "hello", "tools": super::tools(), "build": super::build()});
                    if sink
                        .send(Message::Text(hello.to_string().into()))
                        .await
                        .is_err()
                    {
                        continue;
                    }
                    {
                        let mut s = lock(&shared);
                        s.linked = true;
                        s.state = "linked: your sessions can reach this app".into();
                    }
                    loop {
                        tokio::select! {
                            msg = stream.next() => match msg {
                                Some(Ok(Message::Text(t))) => {
                                    let v: Value = serde_json::from_str(t.as_str()).unwrap_or(Value::Null);
                                    if v["type"] == "call" {
                                        let req = &v["request"];
                                        let call = Call {
                                            id: v["id"].as_u64().unwrap_or(0),
                                            session: v["session"].as_str().unwrap_or("").to_string(),
                                            tool: req["params"]["name"].as_str().unwrap_or("").to_string(),
                                            args: req["params"]["arguments"].clone(),
                                            rpc: req["id"].clone(),
                                        };
                                        let mut s = lock(&shared);
                                        s.last_call = Some((call.tool.clone(), call.session.clone(), web_time::Instant::now()));
                                        s.calls.push(call);
                                    }
                                }
                                Some(Ok(Message::Ping(p))) => { let _ = sink.send(Message::Pong(p)).await; }
                                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                                Some(Ok(_)) => {}
                            },
                            cmd = cmds.recv() => match cmd {
                                Some(Cmd::Send(t)) => if sink.send(Message::Text(t.into())).await.is_err() { break },
                                Some(Cmd::Link(h, t)) => { target = Some((h, t)); break }
                                Some(Cmd::Unlink) => { target = None; let _ = sink.send(Message::Close(None)).await; break }
                                None => return,
                            },
                        }
                    }
                    lock(&shared).linked = false;
                }
                Err(e) if e == "unpaired" => {
                    let mut s = lock(&shared);
                    s.unpaired = true;
                    s.state = "the hub no longer knows this app: pair it again".into();
                    target = None;
                    continue;
                }
                Err(e) => {
                    lock(&shared).state = format!("not linked ({e}); trying again in {backoff} s");
                }
            }
            if target.is_none() {
                continue;
            }
            // Wait before trying again, still listening: Off or a new hub must not wait.
            let sleep = tokio::time::sleep(std::time::Duration::from_secs(backoff));
            tokio::pin!(sleep);
            tokio::select! {
                () = &mut sleep => {}
                cmd = cmds.recv() => match cmd {
                    Some(Cmd::Link(h, t)) => target = Some((h, t)),
                    Some(Cmd::Unlink) => target = None,
                    Some(Cmd::Send(_)) => {}
                    None => return,
                },
            }
            backoff = (backoff * 2).min(60);
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

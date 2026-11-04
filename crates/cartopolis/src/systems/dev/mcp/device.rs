//! The app answering about itself: which build it is, its log, its last crash,
//! what it costs, its settings and map layers, a route planned without moving,
//! and the camera flown somewhere the way a search flies it.
//!
//! **Why these exist.** The phone's first day on the Midas link (2026-11-04)
//! was spent working round their absence: a session parsed a 54.7 KB device
//! list to learn the phone ran a build behind, the person copied the precision
//! log off the phone by hand, a hand-tracking bug went undiagnosed because its
//! log never arrived, and a request to go "slowly back to Oosterpark" was six
//! `view` jumps. None of them needs an avatar, so all of them answer offline.
//!
//! Requests arrive on the bridge's `device` queue (the pump sorts them); the
//! two about the screen go to [`super::screen`] instead. A request that takes
//! time — a perf window, a route, a flight, the diagnostics report — waits in
//! [`DevicePending`] and is answered by the frame it is ready on.

use bevy::diagnostic::DiagnosticsStore;
use bevy::ecs::system::SystemParam;
use bevy::prelude::*;
use futures_channel::oneshot;
use serde_json::{json, Value};

use super::{DeviceRequest, McpBridge, Outcome, Reply};
use crate::resources::{AppPrefs, CameraMode, CameraState, FrameStats, OsmTileUrlTemplate};
use crate::systems::data_layers::DataLayers;
use crate::systems::geo_nav::{GeoNavState, Teleport};
use crate::systems::navigation::DetachedSolve;
use crate::systems::routing::TravelMode;
use cartopolis_mcp::args::AgentReply;
use cartopolis_mcp::device::{AppLogArgs, FlyToArgs, RouteArgs};

/// The settings an agent may read and change, with what each is.
///
/// **A list, not every field of `AppPrefs`.** What is left out is left out on
/// purpose: the editor-access switch and allow-list (what the agent may read),
/// holding the connection open in the background (a permanent notification and
/// the battery), the debug switches (startup only), and the microphone's gain.
/// Those are the person's to change.
const SETTINGS: &[(&str, &str)] = &[
    ("quality", "the render budget: auto, low, medium or high"),
    (
        "render_scale",
        "fraction of the screen the world is drawn at, 0.25-1, or null for the device's own",
    ),
    (
        "frame_rate",
        "frames per second to aim for, 0 for no cap, or null for the device's own",
    ),
    (
        "shadows",
        "sun and moon shadows: true, false, or null for the device's own",
    ),
    ("bloom", "glare around bright light: true, false or null"),
    ("anti_aliasing", "smoothed edges: true, false or null"),
    (
        "light_shafts",
        "sunlight picked out as shafts in the air (the costliest effect): true, false or null",
    ),
    (
        "sun_rays",
        "sunbeams through gaps in the clouds: true, false or null",
    ),
    (
        "sky_lighting",
        "light from the sky filling shadowed faces: true, false or null",
    ),
    ("sky", "which sky to draw: auto, full or dome"),
    (
        "effects_volume",
        "level of footsteps, wind and cues, 0-2 (1 as authored)",
    ),
    ("voice_volume", "level of other people's voices, 0-2"),
    (
        "voice_guidance",
        "the turn chime while navigating: true or false",
    ),
    (
        "keep_screen_on",
        "hold the screen awake while the app is in front: true or false",
    ),
    ("units", "metric or imperial"),
];

/// Lines `app_log` hands back unless asked for another number, and at most.
const LOG_DEFAULT: usize = 80;
const LOG_MAX: usize = 400;

/// What waits on a later frame.
#[derive(Resource, Default)]
pub struct DevicePending {
    perf: Vec<PerfWindow>,
    routes: Vec<PendingRoute>,
    flights: Vec<PendingFlight>,
    /// Answered by [`answer_diagnostics`], whose parameter is too wide to share
    /// a system with the settings this one writes.
    pub(super) diagnostics: Vec<oneshot::Sender<Reply>>,
}

impl DevicePending {
    /// Something here keeps the frame rate up (`mcp::pace_idle`).
    pub fn is_working(&self) -> bool {
        !(self.perf.is_empty() && self.routes.is_empty() && self.flights.is_empty())
    }
}

/// A `perf` measuring over a window: the counters at its start, and every
/// frame time since.
struct PerfWindow {
    until: f64,
    seconds: f32,
    stalls: (u64, u64),
    #[cfg(feature = "perf")]
    timings: std::collections::HashMap<Box<str>, crate::systems::dev::system_timings::Cost>,
    frames_ms: Vec<f32>,
    reply: oneshot::Sender<Reply>,
}

struct PendingRoute {
    solve: DetachedSolve,
    mode: TravelMode,
    show: Option<(f64, f64)>,
    reply: oneshot::Sender<Reply>,
}

struct PendingFlight {
    until: f64,
    to: (f64, f64),
    reply: oneshot::Sender<Reply>,
}

/// What `serve` reads and writes.
#[derive(SystemParam)]
pub struct DeviceWorld<'w> {
    frames: Res<'w, FrameStats>,
    prefs: ResMut<'w, AppPrefs>,
    layers: ResMut<'w, DataLayers>,
    nav: ResMut<'w, GeoNavState>,
    camera: ResMut<'w, CameraState>,
    session: Res<'w, crate::systems::session::SessionState>,
    tile_url: Res<'w, OsmTileUrlTemplate>,
    adapter: Option<Res<'w, bevy::render::renderer::RenderAdapterInfo>>,
    store: Option<Res<'w, DiagnosticsStore>>,
    uploads: Option<Res<'w, crate::systems::upload_meter::UploadMeter>>,
    anchor: Res<'w, crate::resources::MapAnchor>,
    avatar: Res<'w, crate::resources::AvatarState>,
}

/// Answer every question about the app that arrived this frame, and every one
/// whose wait is over.
pub fn serve(
    mut bridge: ResMut<McpBridge>,
    mut pending: ResMut<DevicePending>,
    mut w: DeviceWorld,
    real: Res<Time<Real>>,
) {
    let now = real.elapsed_secs_f64();
    for window in &mut pending.perf {
        window.frames_ms.push(real.delta_secs() * 1000.0);
    }
    let jobs = std::mem::take(&mut bridge.device);
    for (request, reply) in jobs {
        let label = request.label();
        match handle(request, &mut pending, &mut w, now, reply) {
            Some((reply, answer)) => answer_now(&mut bridge, label, reply, answer, now),
            None => bridge.note(label.into(), "working…".into(), Outcome::Working, now),
        }
    }
    land(&mut bridge, &mut pending, &mut w, now);
}

fn answer_now(
    bridge: &mut McpBridge,
    label: &str,
    reply: oneshot::Sender<Reply>,
    answer: AgentReply,
    now: f64,
) {
    let outcome = if answer.error {
        Outcome::Failed
    } else {
        Outcome::Done
    };
    let first = answer.text.lines().next().unwrap_or_default().to_string();
    bridge.note(label.into(), first, outcome, now);
    let _ = reply.send(Reply::Agent(Box::new(answer)));
}

/// One request: its answer now, or `None` when it waits in `pending`.
fn handle(
    request: DeviceRequest,
    pending: &mut DevicePending,
    w: &mut DeviceWorld,
    now: f64,
    reply: oneshot::Sender<Reply>,
) -> Option<(oneshot::Sender<Reply>, AgentReply)> {
    let answer = match request {
        DeviceRequest::AppInfo => app_info(w, now),
        DeviceRequest::AppLog(args) => app_log(&args),
        DeviceRequest::LastCrash => last_crash(),
        DeviceRequest::Perf(args) => match args.seconds {
            Some(s) => {
                let seconds = s.clamp(1.0, 30.0);
                pending.perf.push(PerfWindow {
                    until: now + f64::from(seconds),
                    seconds,
                    stalls: crate::platform::watchdog::stall_totals(),
                    #[cfg(feature = "perf")]
                    timings: crate::systems::dev::system_timings::snapshot(),
                    frames_ms: Vec::new(),
                    reply,
                });
                return None;
            }
            None => perf_now(w),
        },
        DeviceRequest::Diagnostics => {
            pending.diagnostics.push(reply);
            return None;
        }
        DeviceRequest::Settings => settings(&w.prefs),
        DeviceRequest::SetSetting(args) => set_setting(&mut w.prefs, &args.name, args.value),
        DeviceRequest::Layers => layers(&w.layers),
        DeviceRequest::SetLayer(args) => set_layer(&mut w.layers, &args.name, args.on),
        DeviceRequest::Route(args) => match start_route(w, &args) {
            Ok(route) => {
                pending.routes.push(PendingRoute { reply, ..route });
                return None;
            }
            Err(why) => AgentReply::refuse(why),
        },
        DeviceRequest::FlyTo(args) => {
            let answer = fly_to(w, &args);
            match args.wait_s {
                Some(s) if !answer.error && s > 0.0 => {
                    pending.flights.push(PendingFlight {
                        until: now + f64::from(s.min(60.0)),
                        to: (args.lat, args.lng),
                        reply,
                    });
                    return None;
                }
                _ => answer,
            }
        }
        // The pump sends these to `screen`; one here is a routing mistake.
        DeviceRequest::UiTree(_) | DeviceRequest::Press(_) => {
            AgentReply::refuse("The screen tools are not answered here.")
        }
    };
    Some((reply, answer))
}

/// Answer what has finished waiting.
fn land(bridge: &mut McpBridge, pending: &mut DevicePending, w: &mut DeviceWorld, now: f64) {
    let (done, still): (Vec<_>, Vec<_>) = std::mem::take(&mut pending.perf)
        .into_iter()
        .partition(|p| now >= p.until || p.reply.is_canceled());
    pending.perf = still;
    for window in done {
        if window.reply.is_canceled() {
            continue;
        }
        let answer = perf_window(w, &window);
        answer_now(bridge, "perf", window.reply, answer, now);
    }

    let mut routes = Vec::new();
    for route in std::mem::take(&mut pending.routes) {
        if route.reply.is_canceled() {
            continue;
        }
        let landed = route.solve.lock().ok().and_then(|mut g| g.take());
        match landed {
            Some(result) => {
                let answer = route_answer(result, route.mode);
                if let (Some((lat, lng)), false) = (route.show, answer.error) {
                    w.nav.route_to("The agent's route", lat, lng);
                }
                answer_now(bridge, "route", route.reply, answer, now);
            }
            None => routes.push(route),
        }
    }
    pending.routes = routes;

    let mut flights = Vec::new();
    for flight in std::mem::take(&mut pending.flights) {
        if flight.reply.is_canceled() {
            continue;
        }
        let landed = w.camera.flight.is_none() && w.nav.pending_teleport.is_none();
        if landed || now >= flight.until {
            let text = if landed {
                format!("Landed over {:.5}, {:.5}.", flight.to.0, flight.to.1)
            } else {
                "Still flying; it lands on its own.".to_string()
            };
            answer_now(
                bridge,
                "fly to",
                flight.reply,
                AgentReply::ok(text, json!({"landed": landed})),
                now,
            );
        } else {
            flights.push(flight);
        }
    }
    pending.flights = flights;
}

// ---------------------------------------------------------------------------
// The build, the log, the crash
// ---------------------------------------------------------------------------

fn app_info(w: &DeviceWorld, now: f64) -> AgentReply {
    let version = env!("CARGO_PKG_VERSION");
    let commit = option_env!("CARTO_BUILD_SHA");
    let adapter = w.adapter.as_deref().map(
        |a| json!({"name": a.name, "backend": format!("{:?}", a.backend), "driver": a.driver}),
    );
    let budget = crate::quality::budget();
    let online = w.session.is_online();
    let text = format!(
        "Cartopolis {version} ({}), on {} {}. Graphics: {}. Quality tier {:?}. Running {}; {}.",
        commit.unwrap_or("no commit recorded"),
        std::env::consts::OS,
        std::env::consts::ARCH,
        w.adapter
            .as_deref()
            .map_or("none (headless)".to_string(), |a| a.name.clone()),
        budget.tier,
        duration_words(now),
        if online { "online" } else { "offline" },
    );
    AgentReply::ok(
        text,
        json!({
            "version": version,
            "commit": commit,
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
            "adapter": adapter,
            "tier": format!("{:?}", budget.tier),
            "uptime_s": now.round(),
            "online": online,
        }),
    )
}

fn duration_words(s: f64) -> String {
    let s = s.max(0.0) as u64;
    match s {
        0..=89 => format!("{s} s"),
        90..=5399 => format!("{} min", s / 60),
        _ => format!("{:.1} h", s as f64 / 3600.0),
    }
}

/// A line of the log, wherever it came from.
struct LogLine {
    seq: u64,
    /// 0 error, 1 warn, 2 info, 3 debug and below.
    rank: u8,
    text: String,
}

fn level_rank(word: &str) -> Option<u8> {
    match word.to_ascii_lowercase().as_str() {
        "error" | "e" | "f" => Some(0),
        "warn" | "warning" | "w" => Some(1),
        "info" | "i" => Some(2),
        "debug" | "trace" | "d" | "v" => Some(3),
        _ => None,
    }
}

fn app_log(args: &AppLogArgs) -> AgentReply {
    let max_rank = match args.level.as_deref() {
        None => 2,
        Some(word) => match level_rank(word) {
            Some(r) => r,
            None => {
                return AgentReply::refuse(format!(
                    "level wants 'error', 'warn', 'info' or 'debug', not {word:?}"
                ))
            }
        },
    };
    let lines = match read_log() {
        Ok(lines) => lines,
        Err(why) => return AgentReply::refuse(why),
    };
    let limit = args.limit.unwrap_or(LOG_DEFAULT).clamp(1, LOG_MAX);
    let picked = pick_lines(lines, max_rank, args.contains.as_deref(), args.after, limit);
    let last = picked.last().map(|l| l.seq);
    let body: Vec<String> = picked
        .iter()
        .map(|l| format!("[{}] {}", l.seq, l.text))
        .collect();
    let text = if body.is_empty() {
        "Nothing in the log matches.".to_string()
    } else {
        format!(
            "{} lines, oldest first; `after: {}` reads on from here.\n{}",
            body.len(),
            last.unwrap_or_default(),
            body.join("\n")
        )
    };
    AgentReply::ok(text, json!({"last": last, "count": body.len()}))
}

/// The lines a call asked for: at the level or worse, containing the text,
/// after the number, the newest `limit` of them.
fn pick_lines(
    lines: Vec<LogLine>,
    max_rank: u8,
    contains: Option<&str>,
    after: Option<u64>,
    limit: usize,
) -> Vec<LogLine> {
    let needle = contains.map(str::to_lowercase);
    let mut kept: Vec<LogLine> = lines
        .into_iter()
        .filter(|l| l.rank <= max_rank)
        .filter(|l| after.is_none_or(|a| l.seq > a))
        .filter(|l| {
            needle
                .as_deref()
                .is_none_or(|n| l.text.to_lowercase().contains(n))
        })
        .collect();
    if kept.len() > limit {
        kept.drain(..kept.len() - limit);
    }
    kept
}

#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
fn read_log() -> Result<Vec<LogLine>, String> {
    Ok(crate::log_ring::since(0)
        .into_iter()
        .map(|l| LogLine {
            seq: l.seq,
            rank: level_rank(l.level.as_str()).unwrap_or(3),
            text: l.text,
        })
        .collect())
}

/// Logcat's own buffer, numbered by line. A line's number is its place in
/// this read, so `after` holds only while logcat has not rotated past it —
/// near enough for "what came since", which is what it is for.
#[cfg(target_os = "android")]
fn read_log() -> Result<Vec<LogLine>, String> {
    let log = crate::platform::android::own_logcat()
        .ok_or_else(|| "The log is empty, or logcat refused to answer.".to_string())?;
    Ok(log
        .lines()
        .enumerate()
        .map(|(i, line)| LogLine {
            seq: i as u64 + 1,
            rank: logcat_rank(line),
            text: line.to_string(),
        })
        .collect())
}

/// The level letter of a `threadtime` logcat line
/// (`MM-DD HH:MM:SS.mmm  PID  TID L TAG: message`).
#[cfg(any(target_os = "android", test))]
fn logcat_rank(line: &str) -> u8 {
    line.split_whitespace()
        .nth(4)
        .and_then(level_rank)
        .unwrap_or(2)
}

#[cfg(target_arch = "wasm32")]
fn read_log() -> Result<Vec<LogLine>, String> {
    Err("On the web the log is in the browser's console.".into())
}

#[cfg(target_os = "android")]
fn last_crash() -> AgentReply {
    match std::fs::read_to_string(crate::crash::shared_crash_path()) {
        Ok(report) if !report.trim().is_empty() => AgentReply::ok(
            format!("The last crash report, as it was offered to share:\n{report}"),
            json!({"found": true}),
        ),
        _ => AgentReply::ok(
            "No crash report: the app has not crashed since this was added.",
            json!({"found": false}),
        ),
    }
}

#[cfg(not(target_os = "android"))]
fn last_crash() -> AgentReply {
    AgentReply::ok(
        "Crash reports are kept only on Android. On a desktop a panic is in the terminal \
         the app was started from; `app_log` has what was logged before it.",
        json!({"found": false}),
    )
}

// ---------------------------------------------------------------------------
// What it costs
// ---------------------------------------------------------------------------

fn perf_now(w: &DeviceWorld) -> AgentReply {
    let f = &w.frames;
    let mut text = format!(
        "{} fps; frames {:.1} ms on average, the worst {:.1} ms, jitter {:.1} ms (the last second).",
        f.fps, f.mean_ms, f.worst_ms, f.jitter_ms
    );
    let mut data = json!({
        "fps": f.fps, "mean_ms": f.mean_ms, "worst_ms": f.worst_ms, "jitter_ms": f.jitter_ms,
    });
    common_perf(w, &mut text, &mut data);
    AgentReply::ok(text, data)
}

fn perf_window(w: &DeviceWorld, p: &PerfWindow) -> AgentReply {
    let mut ms = p.frames_ms.clone();
    ms.sort_by(f32::total_cmp);
    let n = ms.len();
    let mean = if n == 0 {
        0.0
    } else {
        ms.iter().sum::<f32>() / n as f32
    };
    let pct = |q: f32| {
        ms.get(((n as f32 * q) as usize).min(n.saturating_sub(1)))
            .copied()
    };
    let (count, stall_ms) = crate::platform::watchdog::stall_totals();
    let stalls = (count - p.stalls.0, stall_ms - p.stalls.1);
    let mut text = format!(
        "Over {:.0} s: {n} frames ({:.1} fps), {:.1} ms on average, half under {:.1} ms, \
         95 % under {:.1} ms, the worst {:.1} ms. Main loop stalled {} times ({} ms).",
        p.seconds,
        n as f32 / p.seconds,
        mean,
        pct(0.5).unwrap_or(0.0),
        pct(0.95).unwrap_or(0.0),
        ms.last().copied().unwrap_or(0.0),
        stalls.0,
        stalls.1,
    );
    let mut data = json!({
        "seconds": p.seconds, "frames": n, "mean_ms": mean,
        "p50_ms": pct(0.5), "p95_ms": pct(0.95), "worst_ms": ms.last(),
        "stalls": stalls.0, "stall_ms": stalls.1,
    });
    #[cfg(feature = "perf")]
    {
        let rows = crate::systems::dev::system_timings::delta(
            &p.timings,
            &crate::systems::dev::system_timings::snapshot(),
        );
        if rows.is_empty() {
            text.push_str(" No per-system times: this build was made without the `perf` feature.");
        } else {
            let top: Vec<Value> = rows
                .iter()
                .take(10)
                .map(|(name, c)| {
                    json!({"system": name, "ms_per_frame": c.nanos as f64 / 1e6 / n.max(1) as f64, "calls": c.calls})
                })
                .collect();
            text.push_str(" The costliest systems per frame: ");
            text.push_str(
                &rows
                    .iter()
                    .take(5)
                    .map(|(name, c)| {
                        format!("{name} {:.2} ms", c.nanos as f64 / 1e6 / n.max(1) as f64)
                    })
                    .collect::<Vec<_>>()
                    .join(", "),
            );
            text.push('.');
            data["systems"] = Value::Array(top);
        }
    }
    #[cfg(not(feature = "perf"))]
    text.push_str(" No per-system times: this build was made without the `perf` feature.");
    common_perf(w, &mut text, &mut data);
    AgentReply::ok(text, data)
}

/// What both kinds of `perf` add: GPU passes, uploads, pipelines.
fn common_perf(w: &DeviceWorld, text: &mut String, data: &mut Value) {
    if let Some(store) = w.store.as_deref() {
        let passes = crate::systems::perf::gpu_passes(store);
        if !passes.is_empty() {
            let mut sorted: Vec<_> = passes.iter().filter(|p| p.gpu_ms.is_some()).collect();
            sorted.sort_by(|a, b| b.gpu_ms.unwrap_or(0.0).total_cmp(&a.gpu_ms.unwrap_or(0.0)));
            text.push_str(" Render passes: ");
            text.push_str(
                &sorted
                    .iter()
                    .take(5)
                    .map(|p| format!("{} {:.2} ms", p.path, p.gpu_ms.unwrap_or(0.0)))
                    .collect::<Vec<_>>()
                    .join(", "),
            );
            text.push('.');
            data["passes"] = Value::Array(
                sorted
                    .iter()
                    .map(|p| json!({"pass": p.path, "gpu_ms": p.gpu_ms, "cpu_ms": p.cpu_ms}))
                    .collect(),
            );
        }
    }
    if let Some(m) = w.uploads.as_deref() {
        text.push_str(&format!(
            " Uploads: {} kB this frame, {} kB at the worst frame; {} MB resident.",
            m.frame_kb(),
            m.peak_kb(),
            m.resident_kb() / 1024
        ));
        data["upload_kb"] = json!(m.frame_kb());
        data["upload_peak_kb"] = json!(m.peak_kb());
        data["resident_kb"] = json!(m.resident_kb());
    }
    if let Some(h) = crate::systems::pipeline_health::snapshot() {
        text.push_str(&format!(
            " Render pipelines: {} ready, {} building, {} failed.",
            h.ready,
            h.building,
            h.errored.len()
        ));
        if !h.errored.is_empty() {
            text.push_str(&format!(" Failed: {}.", h.errored.join("; ")));
        }
        data["pipelines"] = json!({"ready": h.ready, "building": h.building, "failed": h.errored});
    }
}

/// The diagnostics report, for every `diagnostics` waiting. A system of its
/// own because [`DiagnosticsSources`] reads the camera `serve` writes.
pub(crate) fn answer_diagnostics(
    mut pending: ResMut<DevicePending>,
    sources: crate::systems::diagnostics::DiagnosticsSources,
) {
    if pending.diagnostics.is_empty() {
        return;
    }
    let report = crate::systems::diagnostics::compose(&sources);
    for reply in pending.diagnostics.drain(..) {
        let _ = reply.send(Reply::Agent(Box::new(AgentReply::ok(
            report.clone(),
            Value::Null,
        ))));
    }
}

// ---------------------------------------------------------------------------
// Settings and layers
// ---------------------------------------------------------------------------

fn settings(prefs: &AppPrefs) -> AgentReply {
    let all = serde_json::to_value(prefs).unwrap_or_default();
    let mut lines = Vec::new();
    let mut data = serde_json::Map::new();
    for (name, what) in SETTINGS {
        let value = all.get(*name).cloned().unwrap_or(Value::Null);
        lines.push(format!("{name} = {value} — {what}"));
        data.insert((*name).to_string(), value);
    }
    AgentReply::ok(lines.join("\n"), Value::Object(data))
}

fn set_setting(prefs: &mut AppPrefs, name: &str, value: Value) -> AgentReply {
    match with_setting(prefs, name, value) {
        Ok(next) => {
            let was = serde_json::to_value(*prefs)
                .ok()
                .and_then(|v| v.get(name).cloned())
                .unwrap_or(Value::Null);
            let now = serde_json::to_value(next)
                .ok()
                .and_then(|v| v.get(name).cloned())
                .unwrap_or(Value::Null);
            // Through change detection only when it changed: every system that
            // reconciles the scene to the settings runs on it.
            if next != *prefs {
                *prefs = next;
            }
            AgentReply::ok(
                format!("{name} is {now} (it was {was}), and saved."),
                json!({"name": name, "value": now, "was": was}),
            )
        }
        Err(why) => AgentReply::refuse(why),
    }
}

/// The settings with one changed, or why not. Through serde, so a value is
/// checked exactly as a saved settings file is.
fn with_setting(prefs: &AppPrefs, name: &str, value: Value) -> Result<AppPrefs, String> {
    if !SETTINGS.iter().any(|(n, _)| *n == name) {
        return Err(format!(
            "There is no setting {name:?} an agent may change. `settings` lists them."
        ));
    }
    let mut all = serde_json::to_value(prefs).map_err(|e| e.to_string())?;
    all[name] = value.clone();
    serde_json::from_value(all).map_err(|_| {
        let what = SETTINGS
            .iter()
            .find(|(n, _)| *n == name)
            .map_or("", |(_, w)| *w);
        format!("{value} is not a value for {name}: {what}.")
    })
}

/// Each layer `set_layer` can switch, and whether it shows. The saved layer
/// preferences' switches, flattened: a nested table (the underground layers)
/// gives `underground.<key>`.
fn layer_switches(layers: &DataLayers) -> Vec<(String, bool)> {
    let prefs = serde_json::to_value(layers.prefs()).unwrap_or_default();
    let mut out = Vec::new();
    if let Value::Object(top) = prefs {
        for (key, value) in top {
            match value {
                Value::Bool(on) => out.push((key, on)),
                Value::Object(inner) => {
                    for (k, v) in inner {
                        if let Value::Bool(on) = v {
                            out.push((format!("{key}.{k}"), on));
                        }
                    }
                }
                _ => {}
            }
        }
    }
    out
}

fn layers(layers: &DataLayers) -> AgentReply {
    let all = layer_switches(layers);
    let on: Vec<&str> = all
        .iter()
        .filter(|(_, v)| *v)
        .map(|(k, _)| k.as_str())
        .collect();
    let off: Vec<&str> = all
        .iter()
        .filter(|(_, v)| !*v)
        .map(|(k, _)| k.as_str())
        .collect();
    AgentReply::ok(
        format!("Showing: {}.\nHidden: {}.", on.join(", "), off.join(", ")),
        Value::Object(
            all.into_iter()
                .map(|(k, on)| (k, Value::Bool(on)))
                .collect(),
        ),
    )
}

fn set_layer(layers: &mut DataLayers, name: &str, on: bool) -> AgentReply {
    let mut prefs = serde_json::to_value(layers.prefs()).unwrap_or_default();
    let slot = match name.split_once('.') {
        Some((table, key)) => prefs.get_mut(table).and_then(|t| t.get_mut(key)),
        None => prefs.get_mut(name),
    };
    match slot {
        Some(v @ Value::Bool(_)) => *v = Value::Bool(on),
        _ => {
            return AgentReply::refuse(format!("There is no layer {name:?}. `layers` lists them."))
        }
    }
    match serde_json::from_value(prefs) {
        Ok(p) => {
            layers.apply_prefs(&p);
            AgentReply::ok(
                format!("{name} is {}.", if on { "showing" } else { "hidden" }),
                json!({"name": name, "on": on}),
            )
        }
        Err(e) => AgentReply::refuse(format!("Could not change {name}: {e}")),
    }
}

// ---------------------------------------------------------------------------
// Routes and flights
// ---------------------------------------------------------------------------

fn start_route(w: &DeviceWorld, args: &RouteArgs) -> Result<PendingRoute, String> {
    let mode = match args.mode.as_deref().unwrap_or("foot") {
        "foot" | "walk" => TravelMode::Foot,
        "bike" | "bicycle" => TravelMode::Bike,
        "car" | "drive" => TravelMode::Car,
        other => return Err(format!("mode wants 'foot', 'bike' or 'car', not {other:?}")),
    };
    let from = match (args.from_lat, args.from_lng) {
        (Some(lat), Some(lng)) => (lat, lng),
        (None, None) => here(w).ok_or_else(|| {
            "There is nowhere to start from yet: give `from_lat` and `from_lng`.".to_string()
        })?,
        _ => return Err("Give both `from_lat` and `from_lng`, or neither.".into()),
    };
    let template = w.tile_url.0.lock().map(|g| g.clone()).unwrap_or_default();
    let solve =
        crate::systems::navigation::solve_detached(from, (args.lat, args.lng), mode, template);
    // A placeholder sender; `handle` puts the call's own in.
    let (reply, _) = oneshot::channel();
    Ok(PendingRoute {
        solve,
        mode,
        show: args.show.unwrap_or(false).then_some((args.lat, args.lng)),
        reply,
    })
}

/// Where a route starts by default: the avatar online, the map's anchor
/// offline (the middle of what was last flown to).
fn here(w: &DeviceWorld) -> Option<(f64, f64)> {
    let anchor = w.anchor.geo?;
    if w.session.is_online() {
        let p = w.avatar.position;
        return Some(crate::systems::geo_nav::world_to_lat_lng(
            anchor,
            f64::from(p.x),
            f64::from(p.z),
        ));
    }
    Some((anchor.0, anchor.1))
}

fn route_answer(
    result: Result<crate::systems::routing::Route, crate::systems::routing::RouteError>,
    mode: TravelMode,
) -> AgentReply {
    let route = match result {
        Ok(r) => r,
        Err(e) => return AgentReply::refuse(format!("No {} route: {e}", mode.label())),
    };
    let steps: Vec<String> = route
        .steps
        .iter()
        .map(|s| format!("{} ({})", s.instruction(), metres(s.distance_m)))
        .collect();
    // Every point would be thousands of tokens for a long route; one every
    // ~25 m keeps its shape.
    let stride = (route.points.len() / 200).max(1);
    let points: Vec<[f64; 2]> = route.points.iter().step_by(stride).copied().collect();
    AgentReply::ok(
        format!(
            "{} {}, about {} min.\n{}",
            metres(route.distance_m),
            mode.label().to_lowercase(),
            (route.duration_s / 60.0).round(),
            steps.join("\n")
        ),
        json!({
            "mode": mode.label(),
            "distance_m": route.distance_m.round(),
            "duration_s": route.duration_s.round(),
            "steps": route.steps.iter().map(|s| json!({
                "do": s.instruction(), "distance_m": s.distance_m.round(), "at": s.at,
            })).collect::<Vec<_>>(),
            "points": points,
        }),
    )
}

fn metres(m: f64) -> String {
    if m >= 1000.0 {
        format!("{:.1} km", m / 1000.0)
    } else {
        format!("{m:.0} m")
    }
}

fn fly_to(w: &mut DeviceWorld, args: &FlyToArgs) -> AgentReply {
    if !(-90.0..=90.0).contains(&args.lat) || !(-180.0..=180.0).contains(&args.lng) {
        return AgentReply::refuse(format!(
            "lat/lng out of range: {:.5}, {:.5}",
            args.lat, args.lng
        ));
    }
    // The flight is Earth mode's (`geo_nav`'s `Flight`): up, across, down.
    if w.camera.mode != CameraMode::Orbit {
        crate::systems::free_camera::enter_mode(&mut w.camera, CameraMode::Orbit);
        w.camera.mode = CameraMode::Orbit;
    }
    // A look at the place, not a move of the body: signed in, the avatar
    // stays where it stands, as a search pick leaves it.
    w.nav.pending_label = None;
    w.nav.pending_teleport = Some(Teleport {
        look: true,
        ..Teleport::geo(args.lat, args.lng, args.altitude_m)
    });
    AgentReply::ok(
        format!(
            "Flying to {:.5}, {:.5}. A short hop jumps instead; anything further climbs, \
             crosses and comes down, over a few seconds.",
            args.lat, args.lng
        ),
        json!({"lat": args.lat, "lng": args.lng}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(seq: u64, rank: u8, text: &str) -> LogLine {
        LogLine {
            seq,
            rank,
            text: text.into(),
        }
    }

    #[test]
    fn a_log_read_filters_by_level_text_and_place() {
        let lines = || {
            vec![
                line(1, 2, "INFO cartopolis: started"),
                line(2, 1, "WARN wgpu: slow"),
                line(3, 0, "ERROR naga: shader failed"),
                line(4, 2, "INFO cartopolis: shader cache warm"),
            ]
        };
        let warn: Vec<u64> = pick_lines(lines(), 1, None, None, 10)
            .iter()
            .map(|l| l.seq)
            .collect();
        assert_eq!(warn, [2, 3]);
        let shader: Vec<u64> = pick_lines(lines(), 2, Some("SHADER"), None, 10)
            .iter()
            .map(|l| l.seq)
            .collect();
        assert_eq!(shader, [3, 4]);
        let since: Vec<u64> = pick_lines(lines(), 3, None, Some(2), 10)
            .iter()
            .map(|l| l.seq)
            .collect();
        assert_eq!(since, [3, 4]);
        let newest: Vec<u64> = pick_lines(lines(), 3, None, None, 1)
            .iter()
            .map(|l| l.seq)
            .collect();
        assert_eq!(newest, [4], "the limit keeps the newest");
    }

    #[test]
    fn a_logcat_line_names_its_level() {
        assert_eq!(
            logcat_rank("11-04 10:01:02.345  1234  1250 W cartopolis: slow frame"),
            1
        );
        assert_eq!(
            logcat_rank("11-04 10:01:02.345  1234  1250 E cartopolis: PANIC"),
            0
        );
        assert_eq!(logcat_rank("--------- beginning of main"), 2);
    }

    #[test]
    fn a_setting_is_checked_as_a_saved_file_is() {
        let prefs = AppPrefs::default();
        let shadows = with_setting(&prefs, "shadows", json!(false)).unwrap();
        assert_eq!(shadows.shadows, Some(false));
        let quality = with_setting(&prefs, "quality", json!("low")).unwrap();
        assert_eq!(quality.quality, crate::resources::QualityPreset::Low);
        assert!(with_setting(&prefs, "quality", json!("ultra")).is_err());
        assert!(
            with_setting(&prefs, "background_connection", json!(true)).is_err(),
            "the person's settings stay theirs"
        );
    }

    #[test]
    fn every_listed_setting_is_a_field() {
        let all = serde_json::to_value(AppPrefs::default()).unwrap();
        for (name, _) in SETTINGS {
            assert!(all.get(*name).is_some(), "{name} is not an AppPrefs field");
        }
    }
}

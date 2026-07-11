//! Live departure boards for the transit data layer (ADR-024).
//!
//! Dutch stops in the transit layer carry an IFOPT quay code (`NL:Q:…` →
//! [`crate::systems::data_layers::PoiInstance::live_id`]). This module polls
//! OVapi (`http://v0.ovapi.nl` — the open, key-free real-time feed covering all
//! Dutch public transport) for the stops around the camera and mounts a
//! departure-board display on each stop's pin, like the real sign at a Dutch
//! bus stop: next departures with line, destination, and minutes-to-go,
//! computed against the real wall clock.
//!
//! The board is a textured quad in the world (not a floating dialog): the text
//! is CPU-rasterised with `fontdue` into a small RGBA texture using the `Hack`
//! font egui embeds, and repainted only when the data or the minute changes.
//! Quads yaw-billboard toward the camera so they stay readable.
//!
//! Politeness: one batched request (`/tpc/{code,code,…}`) covers every stop in
//! range, at most once per [`POLL_INTERVAL`], on a background thread.
//!
//! Time model: OVapi timestamps are naive local time (no offset), except
//! `LastUpdateTimeStamp`, which carries one (`+0200`). Each batch converts the
//! naive times to unix with that self-reported offset — DST handled by the
//! feed itself, no timezone database needed — and teaches [`WorldClock`] the
//! local offset so clocks can show local wall time.

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bevy::pbr::{NotShadowCaster, NotShadowReceiver};
use bevy::prelude::*;
use bevy::render::render_asset::RenderAssetUsages;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use big_space::prelude::{BigSpace, GridCell};

use crate::systems::data_layers::{DataLayers, PoiInstance, LAYER_DEFS, TRANSIT_LAYER};
use crate::systems::free_camera::{FreeCamera, WORLD_CELL_EDGE};

/// Identifying User-Agent (OVapi is a free community service, like Nominatim).
const OVAPI_USER_AGENT: &str =
    "viberfox/0.1 (transit boards; https://github.com/garage44/viberfox)";
/// Refresh cadence for the departure data.
const POLL_INTERVAL: Duration = Duration::from_secs(30);
/// Early poll when stops enter range that have no data yet.
const WARMUP_INTERVAL: Duration = Duration::from_secs(8);
/// Stops within this range of the camera are polled (metres).
const FETCH_RANGE: f32 = 800.0;
/// Boards are mounted on stops within this range (metres).
const SHOW_RANGE: f32 = 500.0;
/// At most this many boards in the world (nearest first).
const MAX_BOARDS: usize = 8;
/// At most this many stops per batched request (URL length + politeness).
const BATCH_MAX: usize = 40;
/// Drop cached boards not refreshed for this long (stops left behind).
const BOARD_TTL: Duration = Duration::from_secs(300);
/// Rows shown per board.
const MAX_ROWS: usize = 4;

/// Board texture size in pixels (2:1, matching the quad's aspect).
const BOARD_PX_W: usize = 512;
const BOARD_PX_H: usize = 256;

/// Real-world wall clock, updated every frame. `now_unix` is seconds since the
/// Unix epoch; `local_offset_secs` is the UTC offset of the *transit region*
/// (learned from the OVapi feed), used to print wall-clock times.
#[derive(Resource, Default)]
pub struct WorldClock {
    pub now_unix: f64,
    pub local_offset_secs: Option<i32>,
}

impl WorldClock {
    /// Time of day as (h, m, s) plus whether it is real local time (offset
    /// known) or a UTC fallback.
    pub fn hms(&self) -> ((u32, u32, u32), bool) {
        let off = self.local_offset_secs.unwrap_or(0) as i64;
        let secs = (self.now_unix as i64 + off).rem_euclid(86_400);
        (
            ((secs / 3600) as u32, (secs / 60 % 60) as u32, (secs % 60) as u32),
            self.local_offset_secs.is_some(),
        )
    }
}

/// Advance the wall clock (cheap; runs every frame).
pub fn update_world_clock(mut clock: ResMut<WorldClock>) {
    if let Ok(d) = SystemTime::now().duration_since(UNIX_EPOCH) {
        clock.now_unix = d.as_secs_f64();
    }
}

/// One upcoming pass at a stop.
#[derive(Clone)]
pub struct Departure {
    pub line: String,
    pub destination: String,
    pub transport: String,
    pub target_unix: i64,
    pub expected_unix: i64,
    pub cancelled: bool,
    pub passed: bool,
}

/// Cached departures for one stop (keyed by OVapi timing-point code).
pub struct StopBoard {
    pub departures: Vec<Departure>,
    pub fetched: Instant,
}

/// A finished batch fetch: boards per code + the feed's UTC offset.
type FetchResult = Result<(HashMap<String, StopBoard>, Option<i32>), String>;

/// A spawned in-world board: its entity, its assets (freed on despawn), and
/// what state its texture was last painted from.
struct BoardEntry {
    entity: Entity,
    cell: GridCell,
    image: Handle<Image>,
    material: Handle<StandardMaterial>,
    mesh: Handle<Mesh>,
    drawn_fetch: Instant,
    drawn_minute: i64,
}

/// Live transit polling state, cached departures, and spawned boards.
#[derive(Resource, Default)]
pub struct TransitLive {
    jobs: Option<Sender<Vec<String>>>,
    results: Arc<Mutex<Vec<FetchResult>>>,
    in_flight: bool,
    last_poll: Option<Instant>,
    pub boards: HashMap<String, StopBoard>,
    entries: HashMap<String, BoardEntry>,
    painter: Option<BoardPainter>,
}

/// Marker for in-world departure-board quads.
#[derive(Component)]
pub struct BoardQuad;

/// Camera-relative position of a POI anchor, exact under the floating origin:
/// grid rebasing is a pure translation, so (poi − camera) computed from grid
/// cells + local translations equals the render-space delta.
fn poi_delta(poi: &PoiInstance, cam_cell: &GridCell, cam_tf: &Transform) -> Vec3 {
    Vec3::new(
        (poi.cell.x - cam_cell.x) as f32 * WORLD_CELL_EDGE + poi.anchor.x
            - cam_tf.translation.x,
        (poi.cell.y - cam_cell.y) as f32 * WORLD_CELL_EDGE + poi.anchor.y
            - cam_tf.translation.y,
        (poi.cell.z - cam_cell.z) as f32 * WORLD_CELL_EDGE + poi.anchor.z
            - cam_tf.translation.z,
    )
}

/// Poll OVapi for the live-capable stops around the camera: one batched
/// request per interval on a worker thread, results folded into `boards`.
pub fn poll_transit_live(
    mut live: ResMut<TransitLive>,
    mut clock: ResMut<WorldClock>,
    dl: Res<DataLayers>,
    camera: Query<(&GridCell, &Transform), With<FreeCamera>>,
) {
    // Start the fetch worker once.
    if live.jobs.is_none() {
        let (tx, rx) = std::sync::mpsc::channel::<Vec<String>>();
        live.jobs = Some(tx);
        let results = live.results.clone();
        std::thread::spawn(move || {
            while let Ok(codes) = rx.recv() {
                let fetched = fetch_departures(&codes);
                if let Ok(mut g) = results.lock() {
                    g.push(fetched);
                }
            }
        });
    }

    // Fold in completed fetches.
    let done: Vec<FetchResult> = live
        .results
        .lock()
        .map(|mut g| g.drain(..).collect())
        .unwrap_or_default();
    for result in done {
        live.in_flight = false;
        match result {
            Ok((boards, offset)) => {
                if let Some(off) = offset {
                    clock.local_offset_secs = Some(off);
                }
                live.boards.extend(boards);
            }
            Err(e) => tracing::warn!(error = %e, "OVapi fetch failed"),
        }
    }
    live.boards.retain(|_, b| b.fetched.elapsed() < BOARD_TTL);

    // Decide whether to poll now.
    if live.in_flight || !dl.layers[TRANSIT_LAYER].enabled {
        return;
    }
    let Ok((cam_cell, cam_tf)) = camera.single() else {
        return;
    };
    let mut codes: Vec<(f32, String)> = Vec::new();
    let mut seen = HashSet::new();
    let mut any_unfetched = false;
    for poi in dl.layers[TRANSIT_LAYER].pois.values().flatten() {
        let Some(code) = &poi.live_id else {
            continue;
        };
        let d = poi_delta(poi, cam_cell, cam_tf);
        let dist = Vec2::new(d.x, d.z).length();
        if dist <= FETCH_RANGE && seen.insert(code.clone()) {
            any_unfetched |= !live.boards.contains_key(code);
            codes.push((dist, code.clone()));
        }
    }
    if codes.is_empty() {
        return;
    }
    let interval = if any_unfetched { WARMUP_INTERVAL } else { POLL_INTERVAL };
    if live.last_poll.is_some_and(|t| t.elapsed() < interval) {
        return;
    }
    codes.sort_by(|a, b| a.0.total_cmp(&b.0));
    codes.truncate(BATCH_MAX);
    let batch: Vec<String> = codes.into_iter().map(|(_, c)| c).collect();
    live.last_poll = Some(Instant::now());
    live.in_flight = true;
    if let Some(jobs) = &live.jobs {
        let _ = jobs.send(batch);
    }
}

/// Mount, repaint, and billboard the in-world departure boards: one textured
/// quad per nearby live stop, repainted when its data or the minute changes.
#[allow(clippy::too_many_arguments)]
pub fn update_transit_boards(
    mut commands: Commands,
    mut live: ResMut<TransitLive>,
    clock: Res<WorldClock>,
    dl: Res<DataLayers>,
    mut images: ResMut<Assets<Image>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    camera: Query<(&GridCell, &Transform), With<FreeCamera>>,
    bigspace: Query<Entity, With<BigSpace>>,
    mut quads: Query<&mut Transform, (With<BoardQuad>, Without<FreeCamera>)>,
) {
    let live = live.as_mut();
    let enabled = dl.layers[TRANSIT_LAYER].enabled;
    let Ok((cam_cell, cam_tf)) = camera.single() else {
        return;
    };

    // Which stops should carry a board right now?
    let mut target: HashMap<&String, &PoiInstance> = HashMap::new();
    if enabled {
        let mut nearby: Vec<(f32, &String, &PoiInstance)> = Vec::new();
        let mut seen = HashSet::new();
        for poi in dl.layers[TRANSIT_LAYER].pois.values().flatten() {
            let Some(code) = &poi.live_id else {
                continue;
            };
            if !live.boards.contains_key(code) || !seen.insert(code) {
                continue;
            }
            let d = poi_delta(poi, cam_cell, cam_tf);
            let dist = Vec2::new(d.x, d.z).length();
            if dist <= SHOW_RANGE {
                nearby.push((dist, code, poi));
            }
        }
        nearby.sort_by(|a, b| a.0.total_cmp(&b.0));
        nearby.truncate(MAX_BOARDS);
        target.extend(nearby.into_iter().map(|(_, c, p)| (c, p)));
    }

    // Retire boards that fell out of the target set (or the layer went off).
    let stale: Vec<String> = live
        .entries
        .keys()
        .filter(|c| !target.contains_key(c))
        .cloned()
        .collect();
    for code in stale {
        if let Some(entry) = live.entries.remove(&code) {
            commands.entity(entry.entity).despawn();
            images.remove(&entry.image);
            materials.remove(&entry.material);
            meshes.remove(&entry.mesh);
        }
    }
    if target.is_empty() {
        return;
    }

    // Lazy painter init; without a usable font there are no boards.
    if live.painter.is_none() {
        live.painter = BoardPainter::new();
        if live.painter.is_none() {
            tracing::warn!("no rasterizable font for transit boards");
            return;
        }
    }
    let painter = live.painter.as_ref().unwrap();

    let Ok(root) = bigspace.single() else {
        return;
    };
    let now = clock.now_unix as i64;
    // Repaint on the *local* minute tick, so the board clock and the countdown
    // flip at the same instant as the HUD clock.
    let minute = (now + clock.local_offset_secs.unwrap_or(0) as i64) / 60;
    let (hm, _) = clock.hms();

    for (code, poi) in &target {
        let board = &live.boards[*code];
        if let Some(entry) = live.entries.get_mut(*code) {
            // Repaint when the data or the minute changed. The texture is
            // swapped as a fresh asset (mutating Image data in place does not
            // reliably reach the GPU) and the material repointed at it.
            if entry.drawn_fetch != board.fetched || entry.drawn_minute != minute {
                entry.drawn_fetch = board.fetched;
                entry.drawn_minute = minute;
                let image = images.add(board_image(paint_board(painter, poi, board, now, hm)));
                if let Some(mat) = materials.get_mut(&entry.material) {
                    mat.base_color_texture = Some(image.clone());
                }
                images.remove(&entry.image);
                entry.image = image;
            }
        } else {
            // New board: paint and mount the panel on the pole like a real
            // stop sign — top edge just under the category finial, the pole
            // running down behind the panel.
            let image = images.add(board_image(paint_board(painter, poi, board, now, hm)));
            let material = materials.add(StandardMaterial {
                base_color_texture: Some(image.clone()),
                unlit: true,
                ..default()
            });
            // Board size scales with the pin (bus ≈ 11×5.5 m, rail ≈ 22×11 m).
            let style = &LAYER_DEFS[TRANSIT_LAYER].categories[poi.category].1;
            let w = style.pole_height * 0.55;
            let h = w * 0.5;
            let mesh = meshes.add(Rectangle::new(w, h));
            let translation = Vec3::new(
                poi.anchor.x,
                style.pole_height - 0.2 - h * 0.5,
                poi.anchor.z,
            );
            let entity = commands
                .spawn((
                    Mesh3d(mesh.clone()),
                    MeshMaterial3d(material.clone()),
                    Transform::from_translation(translation),
                    poi.cell,
                    ChildOf(root),
                    BoardQuad,
                    NotShadowCaster,
                    NotShadowReceiver,
                ))
                .id();
            live.entries.insert(
                (*code).clone(),
                BoardEntry {
                    entity,
                    cell: poi.cell,
                    image,
                    material,
                    mesh,
                    drawn_fetch: board.fetched,
                    drawn_minute: minute,
                },
            );
        }
    }

    // Yaw-billboard every board toward the camera (upright, like a sign that
    // happens to always face you).
    for entry in live.entries.values() {
        let Ok(mut tf) = quads.get_mut(entry.entity) else {
            continue;
        };
        let dx = (cam_cell.x - entry.cell.x) as f32 * WORLD_CELL_EDGE
            + cam_tf.translation.x
            - tf.translation.x;
        let dz = (cam_cell.z - entry.cell.z) as f32 * WORLD_CELL_EDGE
            + cam_tf.translation.z
            - tf.translation.z;
        tf.rotation = Quat::from_rotation_y(dx.atan2(dz));
    }
}

/// Wrap painted pixels in a GPU-only texture asset (repaints replace the whole
/// asset, so no CPU-side copy is kept).
fn board_image(px: Vec<u8>) -> Image {
    Image::new(
        Extent3d {
            width: BOARD_PX_W as u32,
            height: BOARD_PX_H as u32,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        px,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD,
    )
}

// --- Board texture painting (fontdue over egui's embedded Hack font) ---

/// Palette: dark blue sign, amber LED text — the look of a Dutch stop display.
const BG: [u8; 4] = [8, 14, 28, 255];
const BORDER: [u8; 4] = [40, 84, 158, 255];
const HEADER_BAND: [u8; 4] = [21, 62, 120, 255];
const HEADER_TEXT: [u8; 3] = [235, 240, 248];
const CLOCK_TEXT: [u8; 3] = [168, 190, 215];
const AMBER: [u8; 3] = [255, 182, 46];
const ON_TIME: [u8; 3] = [124, 220, 140];
const LATE: [u8; 3] = [255, 200, 90];
const VERY_LATE: [u8; 3] = [255, 140, 70];
const CANCELLED: [u8; 3] = [255, 90, 90];

struct BoardPainter {
    font: fontdue::Font,
}

impl BoardPainter {
    /// Rasterizer over the `Hack` mono font egui embeds (no extra assets).
    fn new() -> Option<Self> {
        let defs = egui::FontDefinitions::default();
        let data = defs.font_data.get("Hack")?;
        let font =
            fontdue::Font::from_bytes(data.font.as_ref(), fontdue::FontSettings::default())
                .ok()?;
        Some(Self { font })
    }

    fn measure(&self, size: f32, text: &str) -> f32 {
        text.chars()
            .map(|c| self.font.metrics(c, size).advance_width)
            .sum()
    }

    /// Draw `text` with its baseline at (`x`, `baseline`); returns the end x.
    fn text(&self, px: &mut [u8], x: f32, baseline: f32, size: f32, color: [u8; 3], text: &str) -> f32 {
        let mut cx = x;
        for ch in text.chars() {
            let (m, cov) = self.font.rasterize(ch, size);
            let gx = cx as i32 + m.xmin;
            let gy = baseline as i32 - m.height as i32 - m.ymin;
            for row in 0..m.height {
                let py = gy + row as i32;
                if py < 0 || py >= BOARD_PX_H as i32 {
                    continue;
                }
                for col in 0..m.width {
                    let pxx = gx + col as i32;
                    if pxx < 0 || pxx >= BOARD_PX_W as i32 {
                        continue;
                    }
                    let a = cov[row * m.width + col] as u32;
                    if a == 0 {
                        continue;
                    }
                    let i = (py as usize * BOARD_PX_W + pxx as usize) * 4;
                    for k in 0..3 {
                        let dst = px[i + k] as u32;
                        px[i + k] = ((color[k] as u32 * a + dst * (255 - a)) / 255) as u8;
                    }
                }
            }
            cx += m.advance_width;
        }
        cx
    }
}

fn fill_rect(px: &mut [u8], x0: usize, y0: usize, x1: usize, y1: usize, c: [u8; 4]) {
    for y in y0..y1.min(BOARD_PX_H) {
        for x in x0..x1.min(BOARD_PX_W) {
            let i = (y * BOARD_PX_W + x) * 4;
            px[i..i + 4].copy_from_slice(&c);
        }
    }
}

/// Paint one board texture: header band with the stop name and wall clock,
/// then up to [`MAX_ROWS`] departures (line, destination, minutes-to-go).
fn paint_board(
    painter: &BoardPainter,
    poi: &PoiInstance,
    board: &StopBoard,
    now: i64,
    (hh, mm, _): (u32, u32, u32),
) -> Vec<u8> {
    let (w, h) = (BOARD_PX_W, BOARD_PX_H);
    let mut px = vec![0u8; w * h * 4];
    fill_rect(&mut px, 0, 0, w, h, BORDER);
    fill_rect(&mut px, 3, 3, w - 3, h - 3, BG);
    fill_rect(&mut px, 3, 3, w - 3, 50, HEADER_BAND);

    // Header: stop name left, clock right.
    let name = poi.name.as_deref().unwrap_or("Halte");
    painter.text(&mut px, 16.0, 37.0, 26.0, HEADER_TEXT, &fit(painter, name, 26.0, 360.0));
    let clock = format!("{hh:02}:{mm:02}");
    let cw = painter.measure(22.0, &clock);
    painter.text(&mut px, w as f32 - 16.0 - cw, 36.0, 22.0, CLOCK_TEXT, &clock);

    let rows: Vec<&Departure> = board
        .departures
        .iter()
        .filter(|d| !d.passed && d.expected_unix >= now - 30)
        .take(MAX_ROWS)
        .collect();

    if rows.is_empty() {
        let msg = "Geen vertrekken";
        let mw = painter.measure(26.0, msg);
        painter.text(&mut px, (w as f32 - mw) / 2.0, 150.0, 26.0, AMBER, msg);
        return px;
    }

    for (i, dep) in rows.iter().enumerate() {
        let baseline = 96.0 + i as f32 * 38.0;

        // Line number, right-aligned in its column, tinted by transport type
        // (buses amber, trams green, trains blue) so mixed boards read at a glance.
        let line_color = match dep.transport.as_str() {
            "TRAM" => [110, 220, 140],
            "TRAIN" => [120, 170, 255],
            "METRO" => [255, 120, 120],
            _ => AMBER,
        };
        let lw = painter.measure(27.0, &dep.line);
        painter.text(&mut px, 70.0 - lw, baseline, 27.0, line_color, &dep.line);

        // Destination (struck through when cancelled).
        let dest = fit(painter, &dep.destination, 27.0, 300.0);
        let end = painter.text(&mut px, 86.0, baseline, 27.0, AMBER, &dest);
        if dep.cancelled {
            fill_rect(
                &mut px,
                84,
                baseline as usize - 9,
                end as usize + 4,
                baseline as usize - 6,
                [CANCELLED[0], CANCELLED[1], CANCELLED[2], 255],
            );
        }

        // Minutes-to-go, right-aligned, tinted by delay.
        let delay = dep.expected_unix - dep.target_unix;
        let color = if dep.cancelled {
            CANCELLED
        } else if delay >= 180 {
            VERY_LATE
        } else if delay >= 60 {
            LATE
        } else {
            ON_TIME
        };
        let rel = dep.expected_unix - now;
        let time = if dep.cancelled {
            "rijdt niet".to_string()
        } else if rel < 60 {
            "nu".to_string()
        } else {
            format!("{} min", rel / 60)
        };
        let tw = painter.measure(27.0, &time);
        painter.text(&mut px, w as f32 - 18.0 - tw, baseline, 27.0, color, &time);
    }
    px
}

/// Truncate `text` (with an ellipsis) to fit `max_w` pixels at `size`.
fn fit(painter: &BoardPainter, text: &str, size: f32, max_w: f32) -> String {
    if painter.measure(size, text) <= max_w {
        return text.to_string();
    }
    let mut out: String = text.to_string();
    while !out.is_empty() && painter.measure(size, &out) + painter.measure(size, "…") > max_w {
        out.pop();
    }
    format!("{out}…")
}

// --- Worker-thread fetch + parse ---

/// Fetch one batched `/tpc/` request and parse it into per-stop boards.
fn fetch_departures(codes: &[String]) -> FetchResult {
    let url = format!("http://v0.ovapi.nl/tpc/{}", codes.join(","));
    let body = ureq::get(&url)
        .set("User-Agent", OVAPI_USER_AGENT)
        .timeout(Duration::from_secs(15))
        .call()
        .map_err(|e| e.to_string())?
        .into_string()
        .map_err(|e| e.to_string())?;
    parse_departures(&body)
}

/// Parse an OVapi `/tpc/` response. Returns boards keyed by timing-point code
/// plus the UTC offset the feed reported (from `LastUpdateTimeStamp`).
fn parse_departures(text: &str) -> FetchResult {
    let json: serde_json::Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let obj = json.as_object().ok_or("OVapi response is not an object")?;

    let mut offset: Option<i32> = None;
    let mut boards = HashMap::new();
    for (code, stop) in obj {
        let mut departures: Vec<Departure> = Vec::new();
        let passes = stop.get("Passes").and_then(|p| p.as_object());
        for pass in passes.into_iter().flat_map(|p| p.values()) {
            let s = |k: &str| pass.get(k).and_then(|v| v.as_str());
            // Learn the UTC offset from the one field that carries it.
            if offset.is_none() {
                offset = s("LastUpdateTimeStamp").and_then(parse_utc_offset);
            }
            let off = offset.unwrap_or(0) as i64;
            let target = s("TargetDepartureTime").and_then(parse_naive_secs);
            let expected = s("ExpectedDepartureTime").and_then(parse_naive_secs);
            let (Some(t), Some(e)) = (target, expected) else {
                continue;
            };
            let status = s("TripStopStatus").unwrap_or("");
            departures.push(Departure {
                line: s("LinePublicNumber").unwrap_or("?").to_string(),
                destination: s("DestinationName50").unwrap_or("").to_string(),
                transport: s("TransportType").unwrap_or("BUS").to_string(),
                target_unix: t - off,
                expected_unix: e - off,
                cancelled: matches!(status, "CANCEL" | "CANCELLED"),
                passed: status == "PASSED",
            });
        }
        departures.sort_by_key(|d| d.expected_unix);
        boards.insert(
            code.clone(),
            StopBoard { departures, fetched: Instant::now() },
        );
    }
    Ok((boards, offset))
}

/// Seconds-since-epoch *as if the naive timestamp were UTC* — caller subtracts
/// the real UTC offset. Format: `2026-07-10T16:34:00`.
fn parse_naive_secs(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 19 || b[4] != b'-' || b[7] != b'-' || b[10] != b'T' {
        return None;
    }
    let num = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let (y, mo, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (h, mi, sec) = (num(11..13)?, num(14..16)?, num(17..19)?);
    Some(days_from_civil(y, mo, d) * 86_400 + h * 3600 + mi * 60 + sec)
}

/// UTC offset in seconds from a `…+0200` (or `…+02:00`) suffix.
fn parse_utc_offset(s: &str) -> Option<i32> {
    let tail = s.get(s.len().checked_sub(6)?..)?;
    let tail = if tail.as_bytes()[3] == b':' {
        format!("{}{}", &tail[..3], &tail[4..]) // "+02:00" → "+0200"
    } else {
        tail[1..].to_string() // skip the timestamp char before "+0200"
    };
    let sign = match tail.as_bytes().first()? {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let h: i32 = tail.get(1..3)?.parse().ok()?;
    let m: i32 = tail.get(3..5)?.parse().ok()?;
    Some(sign * (h * 3600 + m * 60))
}

/// Days since the Unix epoch for a civil date (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Paint a representative board and save a preview PNG (visual check that
    /// the rasteriser puts legible text where the layout says it should).
    #[test]
    fn board_texture_renders_text() {
        let painter = BoardPainter::new().expect("egui Hack font should rasterize");
        let poi = PoiInstance {
            cell: GridCell::default(),
            anchor: Vec3::ZERO,
            name: Some("UMCG Hoofdingang".to_string()),
            live_id: Some("10006090".to_string()),
            category: 2,
        };
        let now = 1_800_000_000i64;
        let dep = |line: &str, dest: &str, transport: &str, rel: i64, delay: i64, cancelled: bool| Departure {
            line: line.to_string(),
            destination: dest.to_string(),
            transport: transport.to_string(),
            target_unix: now + rel - delay,
            expected_unix: now + rel,
            cancelled,
            passed: false,
        };
        let board = StopBoard {
            departures: vec![
                dep("2", "Zuidhorn", "BUS", 40, 0, false),
                dep("6", "Hoofdstation", "BUS", 420, 120, false),
                dep("5", "P+R Haren met lange naam die niet past", "TRAM", 600, 200, false),
                dep("61", "Uithuizen", "BUS", 900, 0, true),
            ],
            fetched: Instant::now(),
        };
        let px = paint_board(&painter, &poi, &board, now, (16, 34, 0));
        assert_eq!(px.len(), BOARD_PX_W * BOARD_PX_H * 4);

        // Enough lit (non-background) pixels that text visibly rendered.
        let lit = px
            .chunks_exact(4)
            .filter(|c| c[0].max(c[1]).max(c[2]) > 90)
            .count();
        assert!(lit > 3000, "board looks blank: only {lit} lit pixels");

        let out = concat!(env!("CARGO_MANIFEST_DIR"), "/../../target/board_preview.png");
        image::save_buffer(
            out,
            &px,
            BOARD_PX_W as u32,
            BOARD_PX_H as u32,
            image::ColorType::Rgba8,
        )
        .expect("save preview");
    }
}

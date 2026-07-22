use bevy::prelude::*;
use rusqlite::Connection;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard};
use tokio::sync::mpsc::UnboundedSender;
use vibe_core::{NetMessage, PrimSurface, TextureEntry};

// ---------------------------------------------------------------------------
// AI assistant
// ---------------------------------------------------------------------------

#[derive(Resource)]
pub struct AiConfig {
    pub api_key: Option<String>,
    pub model: String,
}

impl Default for AiConfig {
    fn default() -> Self {
        Self {
            api_key: None,
            model: "claude-haiku-4-5-20251001".to_string(),
        }
    }
}

pub struct DisplayMessage {
    pub is_user: bool,
    pub text: String,
}

pub type PendingAiResponse = Arc<Mutex<Option<Result<serde_json::Value, String>>>>;

#[derive(Resource, Default)]
pub struct AiAssistantState {
    pub open: bool,
    pub input: String,
    pub display_messages: Vec<DisplayMessage>,
    /// Full conversation history in Anthropic Messages API format.
    pub api_messages: Vec<serde_json::Value>,
    /// Shared cell written by background thread, polled each frame.
    pub pending: Option<PendingAiResponse>,
    /// Set by render_ai_panel when user submits; consumed by poll_ai_response.
    pub pending_send: bool,
    /// Number of tool-call rounds in the current turn (safety cap).
    pub tool_round: u8,
    pub error: Option<String>,
    pub scroll_to_bottom: bool,
}

#[derive(Resource)]
pub struct Database {
    pub conn: Mutex<Connection>,
}

/// Stand-in for the primary `Window` when there isn't one: a scripted `--shot`
/// run is windowless (no winit event loop at all) and renders into an offscreen
/// image, but egui still needs a screen to lay out and scissor against, and the
/// tile/globe LOD heuristics still need a viewport height to size their split
/// thresholds in pixels. Present only in headless runs; every reader falls back
/// to the real `Window` first, so interactive mode is untouched.
#[derive(Resource, Clone, Copy)]
pub struct HeadlessSurface {
    /// Physical pixels of the offscreen target (`--size`).
    pub size: UVec2,
    /// Logical→physical scale. 1.0: there's no monitor to inherit a DPI from,
    /// and the capture is measured in its own pixels.
    pub scale: f32,
}

/// Developer tools panel (toggled with Ctrl+Shift+D, or View ▸ Dev Panel). Currently
/// exposes a day/night slider that drives the sun, and a distance-fog slider.
#[derive(Resource)]
pub struct DevPanelState {
    pub open: bool,
    /// Time of day in hours, 0.0–24.0 (12.0 = noon).
    pub time_of_day: f32,
    /// When true, `time_of_day` advances automatically, looping the full 24h.
    pub auto_cycle: bool,
    /// Real seconds for one full day/night loop when `auto_cycle` is on.
    pub cycle_seconds: f32,
    /// Atmospheric distance-fog density, 0.0–1.0: higher pulls the visibility
    /// distance in (see `free_camera::distance_fog`). 0.0 = off.
    pub fog_density: f32,
    /// Lunar age in days, 0–29.53 (see `utils::sun::LUNAR_CYCLE_DAYS`): 0 = new
    /// moon, ~14.8 = full. Drives the moon's sky position, phase, and light.
    pub moon_age_days: f32,
    /// Show the FPS / frame-time / entity-count overlay. Off by default.
    pub show_stats: bool,
    // ── Render feature toggles (perf/visual inspection) ─────────────────────
    /// Sun + moon shadow maps. Off = both directional lights respawn without
    /// shadows (`manage_lights`); toggling `shadow_maps_enabled` in place panics
    /// bevy_light, so it despawns/respawns instead. Safe to flip at runtime.
    ///
    /// (Atmosphere and env-map IBL are deliberately NOT live toggles: removing
    /// those components at runtime tears down in-flight GPU resources and hangs
    /// the device on this stack — they're startup env flags instead. See
    /// `main::env_flag` / `VIBERFOX_NO_ATMOSPHERE` / `VIBERFOX_NO_ENVMAP`.)
    pub shadows_enabled: bool,
    /// Planetary atmosphere on/off (dev panel toggle).
    pub atmosphere_enabled: bool,
    /// Environment-map image-based lighting on/off (dev panel toggle).
    pub envmap_enabled: bool,
}

impl Default for DevPanelState {
    fn default() -> Self {
        Self {
            open: false,
            time_of_day: 12.0,
            auto_cycle: false,
            cycle_seconds: 10.0,
            fog_density: 0.15,
            moon_age_days: 14.8, // full moon
            show_stats: false,
            shadows_enabled: true,
            atmosphere_enabled: true,
            envmap_enabled: true,
        }
    }
}

/// Number of README screenshot slots the dev-panel button cycles through
/// (`docs/img/screenshot-1.png` … `screenshot-3.png`).
pub const SCREENSHOT_SLOTS: usize = 3;

/// Fixed output resolution for captured screenshots (16:9). Every capture is
/// center-cropped to this aspect ratio and resampled to these exact dimensions, so
/// README images stay consistent regardless of the window size at capture time.
pub const SCREENSHOT_WIDTH: u32 = 1920;
pub const SCREENSHOT_HEIGHT: u32 = 1080;

/// Drives the dev-panel "Screenshot" button: on click, egui is suppressed for one
/// frame so the capture has no UI chrome, then a `Screenshot` of the primary window
/// is saved to the next slot. Kept **separate** from [`DevPanelState`] so toggling it
/// every frame doesn't re-trigger the lighting recompute.
#[derive(Resource, Default)]
pub struct ScreenshotState {
    /// Next slot to write (0-based), cycling `0..SCREENSHOT_SLOTS` → files 1..=N.
    pub next_slot: usize,
    /// When true, egui rendering is dropped this frame so the shot has no UI chrome.
    pub hide_ui: bool,
    /// A capture was requested this frame; the driver spawns the `Screenshot` and
    /// clears this on its next run (while `hide_ui` is still active).
    pub capture_requested: bool,
}

/// Smoothed runtime stats for the dev overlay. Kept **separate** from
/// [`DevPanelState`] on purpose: it updates every frame, and mutating
/// `DevPanelState` each frame would trip its `is_changed()` gate and re-run the
/// day/night lighting recompute continuously.
#[derive(Resource, Default)]
pub struct StatsState {
    /// Exponential moving average of frames per second.
    pub fps: f32,
    /// Exponential moving average of frame time in milliseconds.
    pub frame_ms: f32,
}

#[derive(Resource, Default)]
pub struct GameState {
    pub selected_prim_id: Option<i64>,
    pub regions_loaded: bool,
    pub prims_loaded: bool,
    pub editing_prim_id: Option<i64>,
    pub pending_prim_save: Option<EditDialogState>,
    pub pending_prim_revert: Option<EditDialogState>,
    /// Queued prim deletions (processed by send_prim_mutations each frame).
    pub prims_to_delete: Vec<i64>,
}

/// State for rubber-band (marquee) multi-select.
#[derive(Resource, Default)]
pub struct MarqueeState {
    /// Screen position where the mouse was pressed before we know if it's a click or drag.
    pub press_pos: Option<Vec2>,
    /// True while the rubber-band rectangle is being drawn.
    pub active: bool,
    /// Fixed corner (where the drag began).
    pub start: Vec2,
    /// Trailing corner (tracks the cursor).
    pub end: Vec2,
    /// Shift was held at drag start → add to existing selection instead of replacing.
    pub shift_add: bool,
}

/// Texture catalog with decoded egui handles for thumbnail display.
#[derive(Resource, Default)]
pub struct TextureLibrary {
    pub entries: Vec<TextureEntry>,
    /// Registered egui textures (same key as `entries[i].id`) for thumbnail rendering.
    pub egui_handles: HashMap<String, egui::TextureHandle>,
}

/// Client-side cache of decoded texture images, keyed by texture id.
#[derive(Resource, Default)]
pub struct PrimTextureCache {
    pub handles: HashMap<String, Handle<Image>>,
    pub pending: HashSet<String>,
}

/// Context menu state for prim interaction (right-click menus)
#[derive(Resource, Default, Debug, Clone, Copy)]
pub struct ContextMenuState {
    pub visible: bool,
    pub screen_pos: Vec2,
    pub hit_point: Vec3,
    pub hit_prim_id: Option<i64>,
    pub hit_region_id: Option<i64>,
}

/// Twist / taper / top-shear / slice parameters edited together in the dialog.
/// Grouped to avoid duplicating eight fields for both the live and revert copies.
#[derive(Debug, Clone, Copy)]
pub struct PrimWarp {
    pub twist_begin: f32,
    pub twist_end: f32,
    pub taper_x: f32,
    pub taper_y: f32,
    pub top_shear_x: f32,
    pub top_shear_y: f32,
    pub slice_begin: f32,
    pub slice_end: f32,
}

impl Default for PrimWarp {
    fn default() -> Self {
        Self {
            twist_begin: 0.0,
            twist_end: 0.0,
            taper_x: 0.0,
            taper_y: 0.0,
            top_shear_x: 0.0,
            top_shear_y: 0.0,
            slice_begin: 0.0,
            slice_end: 1.0,
        }
    }
}

/// Edit dialog state for prim properties
#[derive(Resource, Debug, Clone)]
pub struct EditDialogState {
    pub visible: bool,
    pub is_new: bool,
    pub prim_id: Option<i64>,
    pub active_tab: u8, // 0 = Edit, 1 = Object, 2 = Texture
    pub name: String,
    pub shape: String,
    pub position: [f32; 3],
    pub rotation: [f32; 3],
    pub scale: [f32; 3],
    pub color: [f32; 3],
    pub texture_id: Option<String>,
    pub texture_picker_open: bool,
    pub path_cut_begin: f32,
    pub path_cut_end: f32,
    pub hollow: f32,
    pub warp: PrimWarp,
    /// Texture tab surface params (transparency/glow/full-bright/repeats/rotation/offset).
    pub surface: PrimSurface,
    /// Transient "Repeats Per Meter" editor helper (not persisted); the Apply button
    /// derives `surface.repeat_u/v` from this and the object size.
    pub repeats_per_meter: f32,
    // Snapshot taken when the dialog opens; used to build a revert payload on Cancel.
    pub original_name: String,
    pub original_shape: String,
    pub original_position: [f32; 3],
    pub original_rotation: [f32; 3],
    pub original_scale: [f32; 3],
    pub original_color: [f32; 3],
    pub original_texture_id: Option<String>,
    pub original_path_cut_begin: f32,
    pub original_path_cut_end: f32,
    pub original_hollow: f32,
    pub original_warp: PrimWarp,
    pub original_surface: PrimSurface,
}

impl Default for EditDialogState {
    fn default() -> Self {
        Self {
            visible: false,
            is_new: false,
            prim_id: None,
            active_tab: 0,
            name: String::new(),
            shape: String::new(),
            position: [0.0; 3],
            rotation: [0.0; 3],
            scale: [0.0; 3],
            color: [0.0; 3],
            texture_id: None,
            texture_picker_open: false,
            path_cut_begin: 0.0,
            path_cut_end: 1.0,
            hollow: 0.0,
            warp: PrimWarp::default(),
            surface: PrimSurface::default(),
            repeats_per_meter: 0.0,
            original_name: String::new(),
            original_shape: String::new(),
            original_position: [0.0; 3],
            original_rotation: [0.0; 3],
            original_scale: [0.0; 3],
            original_color: [0.0; 3],
            original_texture_id: None,
            original_path_cut_begin: 0.0,
            original_path_cut_end: 1.0,
            original_hollow: 0.0,
            original_warp: PrimWarp::default(),
            original_surface: PrimSurface::default(),
        }
    }
}

impl EditDialogState {
    /// Geometry deformation params (path-cut / hollow / warp) as a protocol value,
    /// for online `CreatePrim` / `UpdatePrim`.
    pub fn geometry(&self) -> vibe_core::PrimGeometry {
        vibe_core::PrimGeometry {
            path_cut_begin: self.path_cut_begin,
            path_cut_end: self.path_cut_end,
            hollow: self.hollow,
            twist_begin: self.warp.twist_begin,
            twist_end: self.warp.twist_end,
            taper_x: self.warp.taper_x,
            taper_y: self.warp.taper_y,
            top_shear_x: self.warp.top_shear_x,
            top_shear_y: self.warp.top_shear_y,
            slice_begin: self.warp.slice_begin,
            slice_end: self.warp.slice_end,
        }
    }
}

#[derive(Resource)]
pub struct AvatarState {
    /// Authoritative sim position (from server when online).
    pub position: Vec3,
    /// Visual follow target: smoothed toward `position` online so camera and mesh do not stutter at tick rate.
    pub display_position: Vec3,
    /// Offline / local yaw for tank controls (radians).
    pub rotation: f32,
    /// Online: replicated world yaw from snapshots (authoritative for reconciliation).
    pub sim_facing_yaw: f32,
    /// Online: `fox_facing_yaw − π` for legacy parity; movement uses camera azimuth, not this field.
    pub online_tank_yaw: f32,
    pub is_flying: bool,
    pub is_walking: bool,
}

impl Default for AvatarState {
    fn default() -> Self {
        let p = Vec3::new(0.0, 2.2, 0.0);
        Self {
            position: p,
            display_position: p,
            rotation: 0.0,
            sim_facing_yaw: 0.0,
            online_tank_yaw: 0.0,
            is_flying: false,
            is_walking: false,
        }
    }
}

#[derive(Resource)]
pub struct CameraState {
    pub mode: CameraMode,
    pub distance: f32,
    pub azimuth: f32,
    pub pitch: f32,
    pub pan_offset: Option<Vec2>, // Last mouse position for delta calculation
    pub free_camera_rotation: Vec2, // pitch, yaw
    // ── Earth (orbit) mode ──────────────────────────────────────────────────
    /// Ground point the orbit camera pivots around, in absolute world metres
    /// (the anchored Web Mercator frame `map_stream` uses). y ≈ 0 (the ground).
    pub orbit_focus: bevy::math::DVec3,
    /// Distance from the focus to the camera (metres). The wheel moves
    /// `orbit_distance_target`; this glides toward it each frame, so zoom is a
    /// smooth camera-fly rather than a per-notch jump.
    pub orbit_distance: f64,
    pub orbit_distance_target: f64,
    /// Space tier: the unit direction from the Earth's centre to the camera
    /// (i.e. the sub-camera point on the globe). Drag rotates it.
    pub orbit_dir: bevy::math::DVec3,
    /// Space tier: the camera's up vector, **derived** each frame from the
    /// planet's north axis and [`Self::orbit_heading`] — not integrated through
    /// drags. Kept as state only so a frame where north is degenerate (looking
    /// straight down a pole) can carry the previous up over.
    pub orbit_up: bevy::math::DVec3,
    /// Space tier: view rotation about the view axis, in radians. 0 = north up,
    /// which is where the globe stays unless the user explicitly rotates
    /// (middle-drag / Ctrl+drag) — the Google Earth convention. Holding this as
    /// an explicit angle rather than letting an arcball accumulate roll is what
    /// keeps the horizon level: composed drag rotations do not commute, so an
    /// integrated up vector drifts off north a little more with every gesture.
    pub orbit_heading: f32,
    /// Azimuth around the focus (radians); horizontal drag spins it.
    pub orbit_yaw: f32,
    /// Elevation angle above the ground plane (radians): 0 = grazing the
    /// horizon, π/2 = straight down. Vertical drag tilts it.
    pub orbit_pitch: f32,
    /// Cleared on entering the mode so the first frame seeds the focus/distance
    /// from wherever the camera was looking.
    pub orbit_initialized: bool,
    /// True once zoomed far enough out that the orbit pivots the Earth's centre
    /// (space tier, ADR-025) instead of a ground focus. Reconstruction is the
    /// same spherical offset; only the pivot and the zoom clamps differ.
    pub orbit_space: bool,
    /// Set by `camera_controls` when the globe descent lands far from the current
    /// anchor: (lat, lng, altitude) to re-anchor the world to. Consumed by
    /// `apply_camera_reanchor`, which turns it into a re-anchoring teleport.
    pub reanchor_request: Option<(f64, f64, f64)>,
    /// Compass request: while true, the Orbit camera eases its view back to
    /// north-up each frame (yaw → 0 on the surface, heading → 0 in space). Set by
    /// the top-right compass overlay, cleared once aligned or as soon as the user
    /// rotates. Ignored outside Orbit (fly/avatar are free-form).
    pub orbit_north_reset: bool,
    /// Compass lock: while true the Orbit camera is *held* north-up instead of
    /// merely eased there once — the view-turn gesture (middle-drag / Ctrl+drag
    /// in space, horizontal rotate on the surface) is ignored and the heading is
    /// re-armed toward north every frame, so the polar axis stays vertical on
    /// screen no matter how the globe is spun. Toggled by clicking the compass.
    pub orbit_north_lock: bool,
}

#[derive(Resource, Default)]
#[allow(dead_code)] // drag/pan state kept for future mouse interaction
pub struct MouseState {
    pub last_position: Option<Vec2>,
    pub is_dragging: bool,
    pub is_panning: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CameraMode {
    Avatar, // Default: camera follows avatar
    Free,   // Free camera mode (FPS-style)
    Orbit,  // "Earth": Google-Earth-style orbit around a ground focus point
}

impl Default for CameraMode {
    fn default() -> Self {
        CameraMode::Avatar
    }
}

impl Default for CameraState {
    fn default() -> Self {
        Self {
            mode: CameraMode::Avatar,
            distance: 5.0,
            azimuth: 0.0,
            pitch: std::f32::consts::PI / 6.0,
            pan_offset: None,
            free_camera_rotation: Vec2::new(0.0, 0.0),
            orbit_focus: bevy::math::DVec3::ZERO,
            orbit_distance: 1500.0,
            orbit_distance_target: 1500.0,
            orbit_dir: bevy::math::DVec3::Z,
            orbit_up: bevy::math::DVec3::Y,
            orbit_heading: 0.0,
            orbit_yaw: 0.0,
            orbit_pitch: std::f32::consts::FRAC_PI_2 * 0.85,
            orbit_initialized: false,
            orbit_space: false,
            reanchor_request: None,
            orbit_north_reset: false,
            orbit_north_lock: false,
        }
    }
}

/// OSM tile URL template (`{z}`/`{x}`/`{y}`); filled from server handshake when online (ADR-014).
#[derive(Resource, Clone)]
pub struct OsmTileUrlTemplate(pub Arc<Mutex<String>>);

impl Default for OsmTileUrlTemplate {
    fn default() -> Self {
        Self(Arc::new(Mutex::new(String::new())))
    }
}

/// When set, client connects to `vibe_sim` instead of loading local SQLite world.
#[derive(Resource, Clone)]
pub struct ConnectAddr(pub String);

/// Credential presented in the ADR-016 handshake (resolved from `--login`, a login
/// file, env, or the solo dev default). Read by `network::spawn_network_thread`.
#[derive(Resource, Clone)]
pub struct ConnectAuth {
    pub credential: vibe_core::AuthCredential,
    /// Persist the issued session token for silent reconnect. True for external
    /// sims only — solo mode's loopback port is ephemeral, so a token is useless.
    pub persist_token: bool,
}

/// Who is currently authenticated (ADR-016). Set from `ServerHelloAck`; drives the
/// HUD profile chip and, later, chat sender identity.
#[derive(Resource, Clone, Default)]
pub struct AuthIdentity {
    pub username: Option<String>,
    pub principal_id: Option<uuid::Uuid>,
}

#[derive(Resource)]
pub struct OnlineSession {
    pub intent_tx: UnboundedSender<NetMessage>,
}

/// After the first `WorldSnapshot`, static regions/prims are not torn down each sim tick (ADR-011).
#[derive(Resource, Default)]
pub struct NetworkSyncState {
    pub received_initial_world: bool,
}

/// Set from `ServerHelloAck.your_avatar_id` so we can pick the local row in `WorldSnapshot::avatars`.
#[derive(Resource, Default, Clone, Copy)]
pub struct LocalAvatarSimId(pub Option<u64>);

/// Incoming messages from the network thread (`Receiver` is not `Sync`; wrap in `Mutex`).
#[derive(Resource)]
pub struct NetworkMailbox {
    pub rx: Mutex<std::sync::mpsc::Receiver<NetMessage>>,
}

impl NetworkMailbox {
    pub fn lock_rx(&self) -> MutexGuard<'_, std::sync::mpsc::Receiver<NetMessage>> {
        self.rx.lock().expect("network mailbox mutex poisoned")
    }
}

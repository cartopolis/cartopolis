use bevy::prelude::*;
use big_space::prelude::GridCell;
use futures_channel::mpsc::UnboundedSender;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard};
use viberfox_core::{NetMessage, PrimSurface, TextureEntry};

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

/// Avatar chooser window (View ▸ Avatar). Edits a `draft` copy of the appearance
/// so dragging the controls doesn't respawn the mesh every frame; Apply commits it
/// to the persisted [`crate::systems::user_store::UserStore`], which the avatar
/// systems watch to rebuild the model.
#[derive(Resource, Default)]
pub struct AvatarChooserState {
    pub open: bool,
    /// Working copy edited by the UI, committed to the store on Apply.
    pub draft: crate::systems::user_store::AvatarAppearance,
    /// Whether `draft` has been seeded from the saved appearance since opening.
    pub seeded: bool,
}

/// Which right-hand panel is open. Exactly one at a time (opening one replaces
/// any other); the bottom bar and the top menu toggle it, and each panel renders
/// with an egui slide-in animation gated on this. `None` = all closed.
#[derive(Resource, Default, Clone, Copy, PartialEq, Eq)]
pub enum RightPanel {
    #[default]
    None,
    Layers,
    Places,
    Route,
    Assistant,
    Chat,
    Appearance,
}

impl RightPanel {
    /// Toggle a panel: open it, or close it if it was already the active one.
    pub fn toggle(&mut self, panel: RightPanel) {
        *self = if *self == panel {
            RightPanel::None
        } else {
            panel
        };
    }

    #[must_use]
    pub fn is(self, panel: RightPanel) -> bool {
        self == panel
    }

    /// The panel's name — its dock tooltip, its menu entry and its header title,
    /// from one string so those three can't drift.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::None => "",
            Self::Layers => "Map layers",
            Self::Places => "Saved places",
            Self::Route => "Route",
            Self::Assistant => "AI Assistant",
            Self::Chat => "Nearby Chat",
            Self::Appearance => "Appearance",
        }
    }

    /// The panel's glyph, as a *preference list* for
    /// [`crate::systems::theme::first_drawable`].
    ///
    /// A list because egui's bundled font subset is much thinner than it looks and
    /// is not something to guess at — the old bottom bar shipped `🧭` for Route,
    /// `🤖` for the Assistant and `🧍` for Appearance, and all three drew as empty
    /// boxes for as long as they existed. Every list ends in a character that
    /// cannot fail.
    ///
    /// Lives on the enum rather than in the dock because the dock is no longer the
    /// only place a panel's icon appears: the header wears it too (and Appearance
    /// and the Assistant have no dock button at all, only a menu entry).
    #[must_use]
    pub fn icons(self) -> &'static [&'static str] {
        match self {
            Self::None => &[" "],
            Self::Layers => &["🗺", "▤", "◈", "L"],
            Self::Places => &["★", "☆", "•", "P"],
            // A destination flag, not a compass rose: `🧭` is not in the font, and
            // a rose would collide with the actual compass in the corner anyway.
            Self::Route => &["⚑", "🧭", "➤", "R"],
            Self::Assistant => &["🤖", "✨", "◎", "AI"],
            Self::Chat => &["💬", "🗨", "◎", "C"],
            Self::Appearance => &["🧍", "🙂", "☺", "A"],
        }
    }
}

/// How `time_of_day` (the sun/moon/fog clock) is driven — a light/dark/auto
/// switcher like a website's theme toggle. The HUD exposes Auto/Day/Night; the
/// dev panel's slider, presets, and Quick-cycle drop into `Manual`.
///
/// Serializable because it is persisted in `data/user.json`
/// ([`crate::systems::user_store::SkyPrefs`]) — the sky comes back the way it
/// was left. Named variants on the wire, so adding a mode never renumbers the
/// saved files.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SunMode {
    /// Realistic: the sun follows the real current time at the map location
    /// (local solar time from the anchor longitude — see `utils::sun`).
    #[default]
    Auto,
    /// Forced daylight (noon).
    Day,
    /// Forced night (midnight).
    Night,
    /// The dev panel owns the clock: the slider / presets / Quick-cycle set
    /// `time_of_day` directly and `main::apply_sun_mode` leaves it untouched.
    Manual,
}

/// Developer tools panel (toggled with Ctrl+Shift+D, or View ▸ Dev Panel). Currently
/// exposes a day/night slider that drives the sun, and a distance-fog slider.
#[derive(Resource)]
pub struct DevPanelState {
    pub open: bool,
    /// How the day/night clock is driven (Auto/Day/Night/Manual). `Manual` is
    /// entered automatically by touching the slider, a preset, or Quick-cycle.
    pub sun_mode: SunMode,
    /// Time of day in hours, 0.0–24.0 (12.0 = noon).
    pub time_of_day: f32,
    /// When true, `time_of_day` advances automatically, looping the full 24h.
    pub auto_cycle: bool,
    /// Real seconds for one full day/night loop when `auto_cycle` is on.
    pub cycle_seconds: f32,
    /// Atmospheric distance-fog density, 0.0–1.0: higher pulls the visibility
    /// distance in (see `free_camera::distance_fog`). 0.0 = off.
    pub fog_density: f32,
    /// Cloud coverage, 0.0–1.0: fraction of the sky the procedural cloud deck
    /// fills (see `main::update_clouds`). 0.0 = clear sky.
    pub cloud_cover: f32,
    /// Rain intensity, 0.0–1.0: density of the screen-space rain streaks (see
    /// `main::update_rain`). 0.0 = dry.
    pub precipitation: f32,
    /// Lunar age in days, 0–29.53 (see `utils::sun::LUNAR_CYCLE_DAYS`): 0 = new
    /// moon, ~14.8 = full. Drives the moon's sky position, phase, and light.
    pub moon_age_days: f32,
    /// Solar declination in radians (±0.409 ≈ ±23.44°): the sun's seasonal angle
    /// north/south of the celestial equator. In every automatic mode this rides
    /// the real date (`main::apply_sun_mode`), so the day length, sunrise/sunset
    /// times and peak elevation track the season instead of the old fixed
    /// 06:00-sunrise/18:00-sunset equinox arc. See `utils::sun::sun_direction`.
    pub sun_declination_rad: f32,
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
            sun_mode: SunMode::Auto,
            time_of_day: 12.0,
            auto_cycle: false,
            cycle_seconds: 10.0,
            fog_density: 0.15,
            cloud_cover: 0.0,
            precipitation: 0.0,
            moon_age_days: 14.8, // full moon
            // Overwritten from the real date on frame 1 by `apply_sun_mode`; this
            // default is the equinox (0°), only ever seen before the first update.
            sun_declination_rad: 0.0,
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
    /// Where the click landed on the map, in `(lat, lng)`.
    ///
    /// Separate from `hit_point`, which is a *render-space* position under the
    /// floating origin and means nothing outside the frame it was measured in.
    /// The routing entries need a geographic point, so it is resolved once at
    /// click time — the anchor it is relative to can move before the menu is
    /// clicked, and re-deriving it later would place the pin somewhere else.
    /// `None` when the map has no anchor yet (nothing streamed).
    pub lat_lng: Option<(f64, f64)>,
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
    pub fn geometry(&self) -> viberfox_core::PrimGeometry {
        viberfox_core::PrimGeometry {
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
    ///
    /// This is also where the **terrain** enters (`systems::terrain`): `position` is
    /// the sim's, on the flat plane the sim simulates, and this one stands on the
    /// measured ground. The gap between them is [`Self::terrain_lift`].
    pub display_position: Vec3,
    /// Metres of measured ground currently folded into `display_position` (online
    /// only — offline the terrain is the ground level itself, so the transform
    /// carries it). Stripped before each reconciliation so prediction and
    /// correction are compared on the same flat plane.
    pub terrain_lift: f32,
    /// Online client-side prediction: the world-space velocity most recently sent
    /// to the sim (after collision steering + fly scaling), replicating the sim's
    /// own `apply_intent` scaling. `smooth_online_avatar_display` integrates this
    /// at frame rate so movement is smooth between 20 Hz snapshots.
    pub predicted_velocity: Vec3,
    /// Online prediction: seconds since the last authoritative pose was applied.
    /// The reconcile target is extrapolated forward by this much, so the 20 Hz
    /// snapshot staircase doesn't drag the predicted display backwards on every
    /// frame between snapshots (see `smooth_online_avatar_display`).
    pub snapshot_age: f32,
    /// The avatar's current world yaw (radians) — the single source for the render
    /// transform, the replicated `display_yaw` and the follow-and-rotate camera.
    /// Turned toward the movement direction by `avatar::advance_facing`, so the
    /// avatar faces where it walks (A/D turn it, they don't strafe) instead of
    /// being pinned to camera-forward.
    pub facing_yaw: f32,
    /// Online: replicated world yaw from snapshots (authoritative for reconciliation).
    pub sim_facing_yaw: f32,
    pub is_flying: bool,
    pub is_walking: bool,
}

impl Default for AvatarState {
    fn default() -> Self {
        let p = Vec3::new(0.0, 2.2, 0.0);
        Self {
            position: p,
            display_position: p,
            terrain_lift: 0.0,
            predicted_velocity: Vec3::ZERO,
            snapshot_age: 0.0,
            facing_yaw: 0.0,
            sim_facing_yaw: 0.0,
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
    /// Follow (Avatar) mode is *derived* from the avatar, so it has no saved pose:
    /// cleared on entry so the first frame snaps the rig straight onto the avatar
    /// (reposition reliably even when returning from a distant Orbit `GridCell`),
    /// then the follow lerp takes over.
    pub avatar_initialized: bool,
    /// Fly (Free) mode remembers where you left it. Cleared on entry; the first
    /// frame then restores [`Self::free_saved`] if present (return to the last fly
    /// spot) or seeds the look from the live view (first-ever entry, no jump).
    pub free_initialized: bool,
    /// Last Fly placement — `(GridCell, local translation, pitch/yaw)` — written
    /// every frame in Fly mode so switching to another mode and back returns here
    /// instead of inheriting wherever Orbit/Follow last moved the shared camera.
    pub free_saved: Option<(GridCell, Vec3, Vec2)>,
    /// Fly-mode inertia: the camera's current velocity in local metres per second.
    /// The keys steer *this*, not the translation, so releasing them coasts to a
    /// stop instead of cutting dead. Cleared whenever the camera is placed rather
    /// than flown (mode entry, teleport) — a carried-over velocity would drift the
    /// camera off a fresh placement.
    pub free_velocity: Vec3,
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
    /// True once Earth mode has ever held a real pose (seeded, restored, or set by
    /// "View Whole Earth"). Re-entering Earth restores that pose instead of
    /// reseeding from the current view, so switching away and back returns you to
    /// the same globe location. The first-ever entry (this false) still seeds.
    pub orbit_has_pose: bool,
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
    /// Follow-and-rotate (Organic-Maps style): while true *and* in Avatar mode, the
    /// follow camera's [`Self::azimuth`] tracks the avatar's facing yaw, so the
    /// camera stays behind the avatar and swings with it like a chase cam. Toggled
    /// by the GPS/locate button; cleared when the user manually left-drags to orbit
    /// (matching OM, where touching the map drops the rotate-follow).
    pub follow_rotate: bool,
    /// Driver mode holds the chase rig itself (`systems::driver`): while this is
    /// set, the Follow camera ignores the wheel and the rotate-drag, and takes its
    /// distance, tilt and azimuth from the map orientation instead. A navigation
    /// view that drifts because the pointer brushed the map is not one.
    pub drive_locked: bool,
    /// Metres ahead of the avatar the Follow camera *aims* while
    /// [`Self::drive_locked`] is set. Looking straight at the avatar centres it, and
    /// then half the screen is road already driven; moving the look target forward
    /// drops the marker into the lower third, which is where every navigation device
    /// puts it. Ignored when not locked.
    pub drive_lookahead: f32,
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
            distance: 6.0,
            azimuth: 0.0,
            pitch: std::f32::consts::PI / 6.0,
            pan_offset: None,
            free_camera_rotation: Vec2::new(0.0, 0.0),
            avatar_initialized: false,
            free_initialized: false,
            free_saved: None,
            free_velocity: Vec3::ZERO,
            orbit_focus: bevy::math::DVec3::ZERO,
            orbit_distance: 1500.0,
            orbit_distance_target: 1500.0,
            orbit_dir: bevy::math::DVec3::Z,
            orbit_up: bevy::math::DVec3::Y,
            orbit_heading: 0.0,
            orbit_yaw: 0.0,
            orbit_pitch: std::f32::consts::FRAC_PI_2 * 0.85,
            orbit_initialized: false,
            orbit_has_pose: false,
            orbit_space: false,
            reanchor_request: None,
            orbit_north_reset: false,
            orbit_north_lock: false,
            follow_rotate: false,
            drive_locked: false,
            drive_lookahead: 0.0,
        }
    }
}

/// OSM tile URL template (`{z}`/`{x}`/`{y}`); filled from server handshake when online (ADR-014).
#[derive(Resource, Clone)]
pub struct OsmTileUrlTemplate(pub Arc<Mutex<String>>);

impl Default for OsmTileUrlTemplate {
    fn default() -> Self {
        Self(Arc::new(Mutex::new(
            tile_url_override().unwrap_or_default(),
        )))
    }
}

/// `VIBE_TILE_URL` — a `{z}/{x}/{y}` template overriding the tile source, matching
/// the `VIBE_OVERPASS_URL` / `VIBE_3DBAG_URL` convention. This is how the offline
/// and `--shot` paths (which never see a server handshake) reach a non-default
/// source, e.g. our self-hosted VersaTiles vector basemap (the default already):
///
/// ```text
/// VIBE_TILE_URL=https://tiles.garage44.eu/tiles/osm/{z}/{x}/{y}
/// ```
///
/// Vector vs raster is inferred from the template — see `viberfox_core::tile_source_kind`.
#[must_use]
pub fn tile_url_override() -> Option<String> {
    std::env::var("VIBE_TILE_URL")
        .ok()
        .filter(|s| !s.trim().is_empty())
}

/// When set, client connects to `simulator` instead of loading local SQLite world.
#[derive(Resource, Clone)]
pub struct ConnectAddr(pub String);

/// Credential presented in the ADR-016 handshake (resolved from `--login`, a login
/// file, env, or the solo dev default). Read by `network::spawn_network_thread`.
#[derive(Resource, Clone)]
pub struct ConnectAuth {
    pub credential: viberfox_core::AuthCredential,
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

/// Where the map is anchored: a (lat, lng) plus base zoom, and a counter that
/// bumps on every re-anchor.
///
/// This is the one thing most of the client wants from the tile streamer. 17 of
/// the 24 modules that took `Res<MapStream>` called nothing on it but
/// `geo_anchor()` — pulling in a resource that also carries a `CellStream`, a
/// `HashMap<TileKey, Entity>`, an `Arc<AtomicU32>` and two asset handles, to read
/// three numbers.
///
/// Splitting it out is not only tidiness. It is what makes the *ordering*
/// expressible: the anchor is written by exactly two systems
/// (`map_stream::init_map_stream` and `map_stream::apply_reanchor`) and read
/// everywhere, but a reader could only order itself against the whole streamer,
/// so almost none did — measured 2026-08-06, 17 of 23 readers had no ordering
/// relation to `apply_reanchor` at all and saw whichever value the frame happened
/// to hold. See `docs/notes/module-graph.md`.
///
/// `MapStream` remains the owner and the only writer; this is its published view.
///
/// Deliberately only the position. The re-anchor *generation* is not mirrored
/// here: every consumer of it (`osm_buildings`, `lod22`, `map_geometry`,
/// `data_layers`, `globe`, `weather`, `navigation`) also needs the streamer
/// itself, so a copy here would have no readers. That is also why the systems
/// below need no ordering against `apply_reanchor` — they compare generations
/// rather than assuming a fresh anchor. See `docs/notes/module-graph.md`.
#[derive(Resource, Default)]
pub struct MapAnchor {
    /// `(lat, lng, base zoom)`. `None` until the first anchor is placed.
    pub geo: Option<(f64, f64, u32)>,
}

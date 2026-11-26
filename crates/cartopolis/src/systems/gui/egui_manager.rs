//! Egui integration for Bevy 0.19.
//!
//! Manual integration using egui-wgpu as the rendering backend,
//! inspired by https://github.com/vladbat00/bevy_egui.
//!
//! Architecture:
//!   - `EguiManager` lives in the main world and holds the egui context plus a
//!     per-frame root `Ui` (egui 0.35 shows panels inside a `Ui`, not the `Context`).
//!   - `begin_egui_frame` (PreUpdate) collects Bevy input, starts the egui frame,
//!     and builds the shared root `Ui`.
//!   - UI systems run in Update: `egui_manager.ui_mut()` for panels,
//!     `egui_manager.ctx_mut()` for floating windows/areas.
//!   - `end_egui_frame` (PostUpdate) finalizes the root `Ui` and tessellates the
//!     output into `EguiRenderOutput`.
//!   - `ExtractResourcePlugin` copies `EguiRenderOutput` to the render world.
//!   - `egui_pass` is a system in the `Core3d` render schedule (Bevy 0.19's
//!     schedule-based render graph), ordered after `Core3dSystems::PostProcess`
//!     (tonemapping) and before `upscaling`.

use crate::platform::sync::Mutex;
use bevy::{
    core_pipeline::{upscaling::upscaling, Core3d},
    input::{keyboard::KeyboardInput, mouse::MouseScrollUnit, ButtonState},
    prelude::*,
    render::{
        camera::ExtractedCamera,
        extract_resource::{ExtractResource, ExtractResourcePlugin},
        render_resource::{RenderPassDescriptor, TextureFormat},
        renderer::{RenderContext, RenderDevice, RenderQueue, ViewQuery},
        view::ViewTarget,
        RenderApp,
    },
};

use crate::resources::HeadlessSurface;
use crate::systems::ui_input::{EguiVerdict, UiInput, UiInputPlugin, UiMetrics};

// ─── Main-world resource ─────────────────────────────────────────────────────

/// How a system reads [`EguiManager`]. A plain resource everywhere but the web.
///
/// egui 0.36 holds dropped files as `Arc<dyn DroppedFile>` without `Send + Sync`
/// on `wasm32` with atomics — a `web_sys::File` is not thread-safe there — so its
/// `Context`, and with it this whole struct, cannot be a Bevy `Resource` on the
/// web build. It is a non-send resource there instead, behind these two names, so
/// no system signature has to carry a `cfg`. Not an `unsafe impl Send`: that
/// would assert something about Bevy's executor a future Bevy is free to change
/// (the rule `platform::location` states for its callbacks).
#[cfg(not(target_arch = "wasm32"))]
pub type EguiRef<'w> = Res<'w, EguiManager>;
/// The mutable half of [`EguiRef`].
#[cfg(not(target_arch = "wasm32"))]
pub type EguiMut<'w> = ResMut<'w, EguiManager>;
#[cfg(target_arch = "wasm32")]
pub type EguiRef<'w> = bevy::ecs::system::NonSend<'w, EguiManager>;
#[cfg(target_arch = "wasm32")]
pub type EguiMut<'w> = bevy::ecs::system::NonSendMut<'w, EguiManager>;

/// Holds the egui context for the main Bevy world.
/// Call [`EguiManager::ctx_mut`] inside Update systems to build UI.
#[cfg_attr(not(target_arch = "wasm32"), derive(Resource))]
pub struct EguiManager {
    pub ctx: egui::Context,
    /// Full-viewport background `Ui` rebuilt each frame in `begin_egui_frame`.
    /// egui 0.35 shows panels inside a `Ui`; sharing one across all UI systems
    /// keeps the cooperative panel layout the old ctx-based panels had.
    root_ui: Option<egui::Ui>,
    elapsed_time: f64,
    /// The surface egui lays out against and the UI scale, refreshed each frame
    /// in [`begin_egui_frame`] — the same [`UiMetrics`] the `UiMetrics`
    /// resource holds, by the same arithmetic (`systems::ui_input`), so a
    /// system that reads the resource sees the points egui lays out in. UI
    /// systems read [`Self::compact`] off [`Self::viewport_pts`] to lay out
    /// responsively — egui has no media queries.
    ///
    /// Kept here (rather than read from the resource) because it is also the
    /// fallback for a frame that has **no** surface at all — see
    /// [`begin_egui_frame`] — and a test app may run egui alone.
    metrics: UiMetrics,
    /// A modal (currently only `systems::login_screen`) owns the screen this
    /// frame. Pointer input takes care of itself — the modal's backdrop is a real
    /// widget, so egui claims the pointer — but the keyboard does not: the gate
    /// below only yields on *text* focus, which would leave WASD flying the avatar
    /// behind a card that has the user's attention.
    modal: bool,
    /// Events produced by something *inside* the Update schedule that wants to
    /// reach egui as if a device had produced them — currently only
    /// `systems::onscreen_keyboard`, which is a keyboard drawn in egui and
    /// therefore cannot exist before the pass it types into has begun.
    ///
    /// Drained by [`begin_egui_frame`] into the **next** frame's `RawInput`,
    /// appended after the pointer batch exactly where the physical keyboard's
    /// events go. That placement is the point: `take_pass_events` splits the
    /// pointer stream so a press carries the position it had at its own point in
    /// the batch (`docs/notes/touch-input.md`), and a key event spliced into the
    /// middle of that would break the ordering contract for no gain — a key has
    /// no position.
    injected: Vec<egui::Event>,
    /// What the panels left of the root `Ui` at the end of the last pass — the
    /// map, in egui points. Read by [`Self::owns_click`]; `None` before the first
    /// pass has ended.
    map_rect: Option<egui::Rect>,
    /// Whether the last pass had an animation in progress — egui asked to be
    /// drawn again *now* (`repaint_delay` zero), which is what every
    /// `animate_*` does on each pass it moves. Read by
    /// `frame_pacing::present_rate`, so a surface arriving over a still map
    /// is drawn at the full rate and not the idle one.
    ui_animating: bool,
    /// The pointer is over a screen drawn beside egui (the component
    /// library's): a world click yields to it as it does to a panel. Set
    /// each frame by `lib_host::bridge_input`, for the next frame.
    overlay_owns_pointer: bool,
    /// A text field of a screen drawn beside egui holds the keyboard, so the
    /// game keys yield to it as to egui's own fields. Set each frame by
    /// `lib_host::bridge_input`, for the next frame.
    overlay_owns_keyboard: bool,
}

/// The UI's scale and compactness are `systems::ui_input`'s, shared with the
/// `UiMetrics` resource; named here too for the callers that grew up with it.
pub use crate::systems::ui_input::COMPACT_WIDTH_PTS;
#[cfg(test)]
use crate::systems::ui_input::UI_SCALE_RANGE;

/// What a compact right panel is exactly as wide as: **the screen**.
///
/// This used to read `viewport_w.max(240.0)` — a floor written so a very narrow
/// window could not produce a degenerate sliver of a panel. On the web phone
/// tier the floor inverted: the render budget's stage transform leaves a
/// Fairphone 4 with ~215 pt of viewport, so the floor docked a panel **26 pt
/// wider than the screen it sat in**, which is the exact failure it was added to
/// prevent (`docs/notes/touch-input.md` recorded it as open). A compact panel
/// *is* the viewport by definition, so there is nothing for a floor to protect:
/// a 100 pt window gets a 100 pt panel, which is useless but honest, and the
/// only genuinely degenerate input is a viewport that is not a width at all —
/// zero before the first surface arrives, or NaN.
#[must_use]
pub fn compact_panel_width(viewport_w: f32) -> f32 {
    /// Stand-in for a viewport we do not have yet. Any positive number does;
    /// this one matches the old floor so a run that somehow reaches it looks the
    /// way it always did.
    const FALLBACK_PTS: f32 = 240.0;
    if viewport_w.is_finite() && viewport_w > 1.0 {
        viewport_w
    } else {
        FALLBACK_PTS
    }
}

/// Build a right-docked [`egui::Panel`] that adapts to the viewport: on a wide
/// screen it docks beside the map at `wide_width` pt and stays resizable; on a
/// compact (phone-ish) screen it fills the viewport and is fixed, since a resize
/// handle is unusable on touch and a 320 pt dock would leave no map visible.
///
/// Takes `compact` / `viewport_w` as plain values (not `&EguiManager`) so it can
/// be called after a caller has already taken `egui.ui_mut()` — building the
/// panel then borrows nothing.
pub fn responsive_right_panel(
    id: impl Into<egui::Id>,
    compact: bool,
    viewport_w: f32,
    wide_width: f32,
) -> egui::Panel {
    // The page every panel shares (`theme::Palette::panel`, the midas hub's) and its
    // hairline, with room round the content. egui's own side-panel margin is
    // 8 x 2 pt, which set every title and row against the panel's edge; it was
    // 14 x 10 until 2026-11-08 ("less cramped"). The Chats screen sets its own
    // frame over this one.
    let frame = egui::Frame::NONE
        .fill(crate::systems::theme::p32().panel)
        .stroke(egui::Stroke::new(
            1.0,
            crate::systems::theme::p32().panel_rim,
        ))
        .inner_margin(egui::Margin::symmetric(16, 14));
    if compact {
        egui::Panel::right(id)
            .frame(frame)
            .resizable(false)
            // No separator line: it has no neighbour to separate from, and egui
            // paints it at the panel's sliding edge on the root `Ui`, so while
            // another panel slid shut behind this one its line kept crossing the
            // open panel.
            .show_separator_line(false)
            // Fill the whole viewport — any inset leaves a strip of map peeking
            // down the left edge of the docked panel.
            .exact_size(compact_panel_width(viewport_w))
    } else {
        egui::Panel::right(id)
            .frame(frame)
            .resizable(true)
            .default_size(wide_width)
    }
}

/// One right-hand panel, the whole dance: the slot check, the collapsible show,
/// the header with its ✕, and the close handling after the body returns.
///
/// This sequence was repeated near-verbatim in eleven panel systems, and it
/// carries an invariant that only a comment used to defend (the note lived in
/// the since-removed assistant panel): a panel must *collect* its close and
/// write the slot **after** `show_collapsible` returns, never mid-frame from
/// inside the draw
/// closure. The other half is the last line: a drag on the panel's edge that
/// pulls it shut writes `open = false` and never touches the header, so a panel
/// that consults only its ✕ reopens on the next frame — the slot still says it
/// is the active panel. Both are the same request. Absorbing the dance here
/// makes both rules structural.
///
/// The body runs only while the panel is expanding or expanded (egui skips it
/// collapsed), so per-frame work belongs inside it. A panel that needs extra
/// header widgets (the Layers panel's Reset) still writes the dance by hand.
pub fn right_panel(
    egui: &mut EguiManager,
    slot: &mut crate::resources::RightPanel,
    which: crate::resources::RightPanel,
    id: &'static str,
    wide_width: f32,
    body: impl FnOnce(&mut egui::Ui),
) -> bool {
    let open_before = slot.is(which);
    let mut open = open_before;
    let mut close = false;
    let compact = egui.compact();
    let viewport_w = egui.viewport_pts().x;
    let ctx = egui.ctx().clone();
    crate::systems::motion::with_panel_time(&ctx, open, || {
        responsive_right_panel(id, compact, viewport_w, wide_width).show_collapsible(
            egui.ui_mut(),
            &mut open,
            |ui| {
                close = crate::systems::theme::panel_header(ui, which.icon(), which.label());
                body(ui);
            },
        );
    });
    // Returned so a panel with teardown of its own (the search panel drops its
    // suggestion popup and its focus claim) can hook the same moment.
    let closed = close || (open_before && !open);
    if closed {
        *slot = crate::resources::RightPanel::None;
    }
    closed
}

/// [`right_panel`] as a sheet across the bottom of a phone's screen, for a
/// panel whose subject is in the world above it — the avatar designer, which a
/// full-screen panel would hide the avatar behind. `share` is the fraction of
/// the screen's height it takes. Same slot, header and close handling.
pub fn bottom_sheet(
    egui: &mut EguiManager,
    slot: &mut crate::resources::RightPanel,
    which: crate::resources::RightPanel,
    id: &'static str,
    share: f32,
    body: impl FnOnce(&mut egui::Ui),
) -> bool {
    let open_before = slot.is(which);
    let mut open = open_before;
    let mut close = false;
    let height = egui.viewport_pts().y * share;
    let frame = egui::Frame::NONE
        .fill(crate::systems::theme::p32().panel)
        .stroke(egui::Stroke::new(
            1.0,
            crate::systems::theme::p32().panel_rim,
        ))
        .inner_margin(egui::Margin::symmetric(14, 10));
    let ctx = egui.ctx().clone();
    crate::systems::motion::with_panel_time(&ctx, open, || {
        egui::Panel::bottom(id)
            .frame(frame)
            .resizable(false)
            .exact_size(height)
            .show_collapsible(egui.ui_mut(), &mut open, |ui| {
                close = crate::systems::theme::panel_header(ui, which.icon(), which.label());
                body(ui);
            });
    });
    let closed = close || (open_before && !open);
    if closed {
        *slot = crate::resources::RightPanel::None;
    }
    closed
}

impl Default for EguiManager {
    fn default() -> Self {
        let ctx = egui::Context::default();
        // The Cartopolis look — palette, spacing, type scale, brand font family.
        // Installed here, at construction, rather than from a startup system:
        // `set_fonts` only takes effect from the next `begin_pass`, so doing it
        // before the first one is what makes the brand family safe to paint with
        // on frame one (the boot splash's wordmark).
        crate::systems::theme::install(&ctx);
        // egui defaults *two* `DebugOptions` to `cfg!(debug_assertions)`, so every
        // debug build paints stray overlays over the real UI:
        //  - `show_unaligned` draws orange edges on widgets off the physical-pixel
        //    grid (common under fractional display scaling), and
        //  - `warn_if_rect_changes_id` strokes a red box around any widget that
        //    keeps its rect but changes `Id` between passes — which our animated
        //    panels (chat / bookmarks slide-in, conditionally-shown widgets that
        //    shift their neighbours' auto-ids) trip constantly, so it reads as red
        //    boxes all over the chat panel and bottom bar.
        // Force the whole debug overlay off so none of it leaks in, in any build.
        //
        // `Style::debug` is itself `#[cfg(debug_assertions)]` in egui, so this must
        // be gated the same way: with assertions off the field does not exist and
        // naming it is a compile error, not a no-op. That is why this only surfaced
        // on the first optimised build — and it would have broken a native
        // `--release` just as surely. There is nothing to turn off in that case;
        // egui compiles the overlay out entirely.
        #[cfg(debug_assertions)]
        ctx.all_styles_mut(|style| {
            style.debug.show_unaligned = false;
            style.debug.warn_if_rect_changes_id = false;
            style.debug.debug_on_hover = false;
            style.debug.show_interactive_widgets = false;
            style.debug.show_expand_width = false;
            style.debug.show_expand_height = false;
        });
        Self {
            ctx,
            root_ui: None,
            elapsed_time: 0.0,
            // The primary window's default resolution in `main.rs`; only ever
            // used for the frames before the first `begin_egui_frame`.
            metrics: UiMetrics::default(),
            modal: false,
            injected: Vec::new(),
            map_rect: None,
            ui_animating: false,
            overlay_owns_pointer: false,
            overlay_owns_keyboard: false,
        }
    }
}

/// Whether the last tessellated geometry can stand in for this frame.
///
/// `repaint_delay` is egui's own answer to "how long may I go without being
/// drawn": `Duration::MAX` for a UI waiting on input, and something finite when a
/// widget has called `request_repaint_after`. Measuring it against the time since
/// the last write is the whole of the test — a finite delay does not mean "draw
/// now", it means "draw within", and an animating widget re-requests on every pass
/// it draws.
fn geometry_still_current(repaint_delay: std::time::Duration, since_write: f32) -> bool {
    repaint_delay.as_secs_f32() > since_write
}

/// How soon [`EguiManager::animating`] asks for the next pass.
///
/// 250 ms because the fastest thing that animates on its own is a seconds clock
/// and a speed readout, and a quarter of a second is quick enough that neither
/// visibly lags while still being a fifteenth of a 60 Hz repaint. It is a *lower
/// bound on staleness*, not a frame rate: any real input repaints immediately.
const ANIMATING_REDRAW: std::time::Duration = std::time::Duration::from_millis(250);

/// The screen positions a world-anchored overlay painted this pass, so egui is
/// asked to draw it again on exactly the frames the world moved under it.
///
/// **A pin is a function of the camera, and the camera is not input.** The skip
/// in [`end_egui_frame`] re-presents last frame's geometry whenever egui saw no
/// events, and walking with a finger held still on the stick, the follow camera
/// catching up, a glide after a fling or a GPS fix are all frames with no
/// events. On those the map moved and the pins did not: they were painted at
/// their new spot and the paint was dropped, so they stood still for a few
/// frames and then jumped to catch up. [`EguiManager::animating`] is no answer —
/// a quarter-second redraw is a stutter against a map drawn at sixty.
///
/// Comparing positions rather than repainting whenever a pin is up keeps the
/// skip where it is worth most: a still map with a dozen pins on it. Dropping
/// this compares what was recorded against the previous pass under the same
/// `id` (kept in egui's own memory, so it costs a system no parameter) and
/// requests a repaint when anything moved, appeared or went. An early return
/// records nothing, which reads as "everything went", so a system may bail out
/// anywhere once this exists.
pub struct WorldAnchored {
    ctx: egui::Context,
    id: egui::Id,
    at: Vec<egui::Pos2>,
}

/// How far, in points, a painted position may move before it is a new picture.
/// Painting is sub-pixel (`place_pins` says why), so this is far under a pixel.
const ANCHORED_MOVE_PTS: f32 = 0.01;

impl WorldAnchored {
    pub fn new(ctx: &egui::Context, id: &'static str) -> Self {
        Self {
            ctx: ctx.clone(),
            id: egui::Id::new(("world_anchored", id)),
            at: Vec::new(),
        }
    }

    /// Record one point the overlay painted at, in egui points.
    pub fn at(&mut self, p: egui::Pos2) {
        self.at.push(p);
    }
}

impl Drop for WorldAnchored {
    fn drop(&mut self) {
        let now = std::mem::take(&mut self.at);
        let moved = self.ctx.data_mut(|d| {
            let last = d.get_temp_mut_or_default::<Vec<egui::Pos2>>(self.id);
            let moved = last.len() != now.len()
                || last
                    .iter()
                    .zip(&now)
                    .any(|(a, b)| a.distance_sq(*b) > ANCHORED_MOVE_PTS * ANCHORED_MOVE_PTS);
            *last = now;
            moved
        });
        if moved {
            self.ctx.request_repaint();
        }
    }
}

impl EguiManager {
    /// Borrow the egui context mutably. Only valid between `begin_egui_frame`
    /// and `end_egui_frame` (i.e. during the Update schedule). Use for floating
    /// windows/areas (`egui::Window`, `egui::Area`), which still take `&Context`.
    pub fn ctx_mut(&mut self) -> &mut egui::Context {
        &mut self.ctx
    }

    /// Borrow the egui context *shared* — same validity window as [`Self::ctx_mut`].
    ///
    /// `egui::Context` is `Sync` (an `Arc<RwLock<…>>` inside) and every
    /// floating-`Area` call takes `&Context`, so a system that draws only its
    /// own `Area`s — no shared root `Ui`, no event injection — can take the
    /// manager as a plain `Res`. That matters for scheduling, not hygiene: the
    /// mutable claim on this manager is one of the two cliques that serialise
    /// the whole Update schedule (`docs/notes/render-performance-survey.md`),
    /// and every demotion to `Res` lets that system overlap the others. Area
    /// z-order is unaffected — each `Area` owns its own layer, ordered by
    /// egui's memory, not by which system happened to register it first this
    /// frame.
    ///
    /// (Worded without naming the mutable-parameter type next to the root-`Ui`
    /// accessor: `every_root_ui_panel_is_in_the_chain` scans source chunks for
    /// exactly that pair of tokens.)
    pub fn ctx(&self) -> &egui::Context {
        &self.ctx
    }

    /// Declare that what was just drawn will change on its own, and ask egui for
    /// another pass within [`ANIMATING_REDRAW`].
    ///
    /// **A widget whose content is a function of the clock must call this, or it
    /// freezes.** [`end_egui_frame`] skips the whole tessellate → upload chain when
    /// egui reports nothing to redraw, and egui decides that from *input*: events,
    /// pointer movement, a wheel, an explicit `request_repaint`. A moving avatar, a
    /// GPS fix and the passage of time are none of those. So on any frame nobody is
    /// touching the screen the previously-tessellated geometry is simply
    /// re-presented — correct for a static panel, and wrong for a clock, a speed, a
    /// frame-rate readout or a countdown, which stop and stay stopped until
    /// something is tapped. That is worst on the platform with no mouse to move and
    /// on the screen whose entire job is a changing number.
    ///
    /// The skip is still worth having, which is why this is opt-in per site rather
    /// than a blanket repaint: four passes a second where the UI genuinely animates
    /// is a fifteenth of the sixty it would otherwise cost, and nothing at all where
    /// it does not.
    pub fn animating(&self) {
        self.ctx.request_repaint_after(ANIMATING_REDRAW);
    }

    /// Whether the UI moved on its own last pass (a `motion` in progress), as
    /// opposed to merely wanting a look within a quarter second. The idle
    /// throttle counts it as busy.
    #[must_use]
    pub fn ui_animating(&self) -> bool {
        self.ui_animating
    }

    /// Logical viewport size in points (physical pixels ÷ display scale), as egui
    /// lays out against it this frame.
    pub fn viewport_pts(&self) -> egui::Vec2 {
        let v = self.metrics.viewport_pts();
        egui::vec2(v.x, v.y)
    }

    /// Record the surface this frame is laid out against, given the *display's*
    /// scale factor — [`UiMetrics::set_surface`], which folds in the UI scale
    /// and sanitises a zero or NaN display scale.
    #[cfg(test)]
    fn set_surface(&mut self, physical: UVec2, display_scale: f32) {
        self.metrics.set_surface(physical, display_scale);
    }

    /// The UI magnification in force (`ui_input::resolve_ui_scale`). Also the
    /// factor between the window's logical points and egui's.
    pub fn ui_scale(&self) -> f32 {
        self.metrics.ui_scale()
    }

    /// The surface and scale egui lays out against this frame — the same
    /// value the `UiMetrics` resource holds.
    #[cfg(test)]
    #[must_use]
    pub fn metrics(&self) -> UiMetrics {
        self.metrics
    }

    /// The *display's* own scale factor, with [`Self::ui_scale`] divided back
    /// out — physical pixels per window logical point, which is what winit
    /// reports and what `devicePixelRatio` is on the web.
    ///
    /// Exposed separately from [`Self::viewport_pts`] because the two answer
    /// different questions: layout wants points, but anything reasoning about
    /// *physical* size (a touch target in millimetres) needs the pixel density
    /// and the magnification apart, not multiplied together.
    pub fn display_scale(&self) -> f32 {
        self.metrics.display_scale()
    }

    /// Queue events for the next `RawInput` — see the `injected` field.
    ///
    /// Deliberately additive and un-deduplicated: the caller is producing the
    /// events a device would have produced, and a device is allowed to repeat
    /// itself.
    pub fn inject_events(&mut self, events: impl IntoIterator<Item = egui::Event>) {
        self.injected.extend(events);
    }

    /// Convert a position in the window's logical points — which is what
    /// `Window::cursor_position`, `TouchInput::position` and
    /// `Camera::world_to_viewport` all speak — into egui points.
    ///
    /// The two spaces coincide only while [`Self::ui_scale`] is 1.0, so a
    /// world-anchored overlay that skips this is *correct on every desktop* and
    /// slides away from its subject on a phone, proportionally to how far it is
    /// from the top-left corner.
    pub fn window_to_ui(&self, p: Vec2) -> egui::Pos2 {
        let p = self.metrics.window_to_ui(p);
        egui::pos2(p.x, p.y)
    }

    /// Does this frame's `just_pressed(Left)` belong to egui rather than to the
    /// world under it? The gate for a *world click* — `picking::prim_picking` and
    /// `building_select::pick_building`, which must agree (`picking::PrimClick`).
    ///
    /// A mouse click asks `egui_wants_pointer_input()`, which is right for a
    /// mouse: the press reaches `ButtonInput` on the same frame egui sees it, so
    /// the button is still down and the answer is "did a widget take it".
    ///
    /// **A finger's click cannot ask that**, because it arrives a frame late
    /// (`touch::emulate_mouse`'s ordering note) and egui reads the `TouchInput`
    /// stream directly. When the finger lifted in that one frame — a brisk tap on
    /// a handset, and the second tap of a double tap is the brisk one — egui has
    /// already seen the release by the time the click lands, and with no button
    /// down `egui_wants_pointer_input()` falls through to `is_pointer_over_egui()`,
    /// which is unconditionally true here (the root `Ui` is ours, so egui never
    /// learns its `root_ui_available_rect`). The tap was swallowed over the bare
    /// map, and the building card never opened. So a finger's click is decided by
    /// *where it landed* instead: on a floating area or modal, or on the root
    /// layer outside the map (a panel), it is egui's — [`press_over_ui`].
    ///
    /// The world's clicks ask `UiInput::owns_click`, which carries this
    /// verdict over; this one stays for its tests, which pin the rule.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn owns_click(&self, touch_click: bool, cursor: Option<Vec2>) -> bool {
        if self.overlay_owns_pointer {
            return true;
        }
        match (touch_click, cursor) {
            (true, Some(at)) => press_over_ui(&self.ctx, self.map_rect, self.window_to_ui(at)),
            _ => self.ctx.egui_wants_pointer_input(),
        }
    }

    /// Declare (each frame) that the pointer is over a screen drawn beside
    /// egui — see the `overlay_owns_pointer` field. Only the library's
    /// screens (`ui-probe`) have one to declare.
    #[cfg_attr(not(feature = "ui-probe"), allow(dead_code))]
    pub fn set_overlay_owns_pointer(&mut self, owns: bool) {
        self.overlay_owns_pointer = owns;
    }

    /// Declare (each frame) that a text field beside egui has the keyboard —
    /// see the `overlay_owns_keyboard` field.
    #[cfg_attr(not(feature = "ui-probe"), allow(dead_code))]
    pub fn set_overlay_owns_keyboard(&mut self, owns: bool) {
        if self.overlay_owns_keyboard != owns {
            self.overlay_owns_keyboard = owns;
        }
    }

    /// A text field beside egui has the keyboard.
    #[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
    pub fn overlay_owns_keyboard(&self) -> bool {
        self.overlay_owns_keyboard
    }

    /// Declare (each frame) whether a modal owns the screen — see the `modal`
    /// field. Written by `login_screen::render_login_screen`, read through
    /// [`Self::verdict`] by `UiInput::ui_owns_keyboard`.
    pub fn set_modal(&mut self, modal: bool) {
        self.modal = modal;
    }

    /// True on a narrow (phone-ish) viewport — see [`COMPACT_WIDTH_PTS`]. UI
    /// systems branch on this for a responsive layout.
    pub fn compact(&self) -> bool {
        self.viewport_pts().x < COMPACT_WIDTH_PTS
    }

    /// Borrow this frame's shared root `Ui`, for showing panels
    /// (`egui::Panel`, `egui::CentralPanel`). Only valid during Update.
    ///
    /// [`begin_egui_frame`] builds the root `Ui` unconditionally — including on a
    /// frame with no window at all — so during Update this cannot fail. It still
    /// panics rather than no-ops, because the one remaining way to reach it is a
    /// UI system registered in the wrong schedule, and that should be loud.
    pub fn ui_mut(&mut self) -> &mut egui::Ui {
        self.root_ui
            .as_mut()
            .expect("ui_mut() called outside an egui frame")
    }

    /// egui's answers to `UiInput`'s questions this frame, for a press at the
    /// window `cursor` — see [`publish_ui_input`].
    pub fn verdict(&self, cursor: Option<Vec2>) -> EguiVerdict {
        verdict_of(&self.ctx, self.map_rect, self.modal, &self.metrics, cursor)
    }
}

/// [`EguiManager::verdict`] over its parts, so a test can drive a bare
/// `Context`: `press_over_ui` for [`EguiManager::owns_click`]'s finger, a
/// drag that started on the bare map (a sheet over the map steps aside for
/// it), and whether a text field has the keyboard.
pub(crate) fn verdict_of(
    ctx: &egui::Context,
    map_rect: Option<egui::Rect>,
    modal: bool,
    metrics: &UiMetrics,
    cursor: Option<Vec2>,
) -> EguiVerdict {
    let map_dragged = ctx
        .input(|i| {
            i.pointer
                .is_decidedly_dragging()
                .then(|| i.pointer.press_origin())
                .flatten()
        })
        .is_some_and(|at| !press_over_ui(ctx, map_rect, at));
    let press_over_ui = cursor.is_some_and(|at| {
        let p = metrics.window_to_ui(at);
        press_over_ui(ctx, map_rect, egui::pos2(p.x, p.y))
    });
    // Back into window points, by the inverse of `window_to_ui`.
    let k = metrics.ui_scale();
    let map_rect = map_rect.map(|r| Rect::new(r.min.x * k, r.min.y * k, r.max.x * k, r.max.y * k));
    EguiVerdict {
        wants_pointer: ctx.egui_wants_pointer_input(),
        text_focused: ctx.text_edit_focused(),
        modal,
        map_dragged,
        cursor,
        press_over_ui,
        map_rect,
    }
}

/// Write egui's verdict into [`UiInput`] (`PreUpdate`, right after
/// [`begin_egui_frame`] and after `touch::recognize_touch_gestures` has put
/// the window cursor on the finger), and hand the library's verdict — set by
/// `lib_host::bridge_input` last frame — to the egui systems that still ask
/// the manager.
///
/// Taken at the start of the pass, so a system in `Update` reads what the
/// frame began with; egui decides its pointer interaction in `begin_pass`
/// too, so only a focus or an `Area` that changes mid-pass is seen a frame
/// later than when it was asked of egui directly.
pub fn publish_ui_input(mut egui: EguiMut, mut input: ResMut<UiInput>, windows: Query<&Window>) {
    egui.set_overlay_owns_pointer(input.overlay_owns_pointer());
    egui.set_overlay_owns_keyboard(input.overlay_owns_keyboard());
    let cursor = windows.iter().next().and_then(|w| w.cursor_position());
    let verdict = egui.verdict(cursor);
    if input.egui_verdict() != verdict {
        input.set_egui(verdict);
    }
}

/// Copy `ui_animating` into [`UiInput`] once [`end_egui_frame`] has decided
/// it, for `frame_pacing::present_rate` in `Last`.
pub fn publish_ui_animating(egui: EguiRef, mut input: ResMut<UiInput>) {
    if input.ui_animating() != egui.ui_animating() {
        input.set_ui_animating(egui.ui_animating());
    }
}

// ─── Extracted render data ────────────────────────────────────────────────────

/// egui's [`egui::TexturesDelta`], owned the way this bridge owns it.
///
/// egui 0.36 debug-asserts that a delta is never dropped unapplied, which assumes
/// one painter consuming each delta exactly once. Here the delta is *cloned* into
/// the render world on every extract and the main-world copy is replaced once
/// [`APPLIED_GENERATION`] says it landed, so dropping a copy is the normal end of
/// its life — the generation, not the drop, is what tracks whether it was applied.
#[derive(Clone, Default)]
pub struct TextureUpdates(pub egui::TexturesDelta);

impl Drop for TextureUpdates {
    fn drop(&mut self) {
        self.0.clear();
    }
}

impl std::ops::Deref for TextureUpdates {
    type Target = egui::TexturesDelta;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for TextureUpdates {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

/// Tessellated egui output, extracted to the render world when it changes.
///
/// Deliberately *not* rewritten every frame — see [`end_egui_frame`]. `ExtractResource`'s
/// derive is a plain `clone()`, and `extract_resource` only skips it when the main-world
/// resource is unchanged, so an unconditional write meant deep-copying the whole UI
/// vertex/index geometry into the render world 60×/s for a pixel-identical picture.
#[derive(Resource, Clone, Default, ExtractResource)]
#[extract_app(RenderApp)]
pub struct EguiRenderOutput {
    pub clipped_primitives: Vec<egui::ClippedPrimitive>,
    pub textures_delta: TextureUpdates,
    /// Physical window size in pixels (for `ScreenDescriptor`).
    pub size_in_pixels: [u32; 2],
    pub pixels_per_point: f32,
    /// Bumped on every real write. [`egui_pass`] compares it against the generation
    /// it last uploaded so it can skip `update_buffers` / texture uploads for
    /// geometry already resident on the GPU — and, critically, so a `textures_delta`
    /// that lingers in a re-used output isn't re-applied every frame.
    pub generation: u64,
    /// Set by a system that mutated this resource in place (currently
    /// `ui::suppress_egui_for_screenshot`, which clears the geometry for one frame).
    /// Forces the next [`end_egui_frame`] to write even if egui reports no repaint,
    /// otherwise the suppressed state would stick and the UI would never come back.
    pub force_next_write: bool,
}

// ─── Render-world resource ─────────────────────────────────────────────────

/// Lazily-initialized egui-wgpu renderer in the render world.
/// Stores the renderer alongside the sample count it was compiled for so we
/// can detect and handle MSAA changes (e.g. camera settings changed), the
/// [`EguiRenderOutput::generation`] whose geometry/textures are currently resident
/// on the GPU (`None` until the first upload), and the textures egui has finished
/// with but which cannot be destroyed until the frame that drew them has been
/// **submitted** — see [`egui_pass`].
// On wasm `egui_wgpu::Renderer` holds WebGL handles that are `!Send + !Sync` (the
// browser GPU is main-thread-only), so it cannot be a normal Bevy `Resource` there
// — it is stored as a **non-send** resource instead (ADR-033). On native it stays a
// regular `Resource`. The `egui_pass` accessor and its registration mirror this.
#[cfg_attr(not(target_arch = "wasm32"), derive(Resource))]
#[derive(Default)]
pub struct EguiWgpuRenderer(
    pub  Mutex<
        Option<(
            egui_wgpu::Renderer,
            TextureFormat,
            u32,
            Option<u64>,
            Vec<egui::TextureId>,
        )>,
    >,
);

// ─── Plugin ──────────────────────────────────────────────────────────────────

pub struct EguiPlugin;

impl Plugin for EguiPlugin {
    fn build(&self, app: &mut App) {
        #[cfg(not(target_arch = "wasm32"))]
        app.init_resource::<EguiManager>();
        #[cfg(target_arch = "wasm32")]
        app.init_non_send::<EguiManager>();
        app.init_resource::<EguiRenderOutput>()
            .add_plugins(ExtractResourcePlugin::<EguiRenderOutput>::default())
            // MUST run after Bevy has written this frame's input: begin_egui_frame
            // reads `ButtonInput`, cursor position and the keyboard/mouse events to
            // build egui's `RawInput`. Unordered, it can run *before* `InputSystem`
            // and collect stale/partial input, which corrupts click and keyboard-
            // focus handling (fields lose focus on keypress).
            .add_systems(PreUpdate, begin_egui_frame.after(bevy::input::InputSystems))
            .add_systems(PostUpdate, end_egui_frame);
        // egui's half of `UiInput`, which a system that only asks "who owns the
        // input" reads instead of the manager. After the touch recogniser,
        // which moves the window cursor onto the finger a world click reads.
        if !app.is_plugin_added::<UiInputPlugin>() {
            app.add_plugins(UiInputPlugin);
        }
        app.add_systems(
            PreUpdate,
            publish_ui_input
                .after(begin_egui_frame)
                .after(crate::systems::touch::recognize_touch_gestures),
        )
        .add_systems(PostUpdate, publish_ui_animating.after(end_egui_frame));

        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };

        #[cfg(not(target_arch = "wasm32"))]
        {
            use bevy::render::GpuResourceAppExt as _;
            render_app.init_resource::<EguiWgpuRenderer>();
            // …and again on **every** device, which is what makes the renderer
            // survive `handle_render_error`'s `DeviceLost` recovery. `RenderStartup`
            // re-runs each time a `RenderDevice` is acquired, and this drops the
            // cached `egui_wgpu::Renderer` with it.
            //
            // Without this the recovery is worse than the loss. The cached renderer
            // holds pipelines, bind groups and textures belonging to the dead
            // device, and nothing in `egui_pass` would notice: `needs_recreate`
            // compares the *format* and the sample count, both unchanged by a
            // device loss, and `uploaded_gen` still matches the current output, so
            // not even a re-upload is attempted. egui would then draw dead handles
            // at the new device and raise `Validation` — which `handle_render_error`
            // halts on, by design, because validation errors recur. So a transient,
            // recoverable `DeviceLost` would be converted into the terminal halt the
            // recovery exists to avoid. The `init_resource` above stays because the
            // resource must exist before the first `RenderStartup` runs.
            render_app.init_gpu_resource::<EguiWgpuRenderer>();
        }
        // The wasm arm is a non-send resource (WebGL handles are main-thread-only)
        // and cannot go through `init_gpu_resource`, which requires `Resource`. It
        // is also the one target where bevy's device-loss recovery does not apply:
        // a lost WebGL context is delivered to the page, not to wgpu's callback.
        #[cfg(target_arch = "wasm32")]
        render_app.world_mut().init_non_send::<EguiWgpuRenderer>();
        render_app.add_systems(
            Core3d,
            // Draw egui AFTER upscaling, onto the final presented output — a stable,
            // single-sampled, sRGB swapchain texture. Drawing before upscaling (on the
            // ping-ponged HDR main texture) made the UI flicker and read too bright.
            egui_pass.after(upscaling),
        );
    }
}

// ─── Systems ──────────────────────────────────────────────────────────────────

/// State the pointer bridge threads through the window-event stream.
#[derive(Default)]
pub(crate) struct PointerBridge {
    /// The running pointer position, so a button event carries the position it
    /// had *at its point in the stream*.
    last: Option<egui::Pos2>,
    /// Which finger is currently standing in for egui's pointer. egui has
    /// exactly one pointer, so the first finger down owns it until it lifts:
    /// otherwise the second finger of a pinch would drag the UI out from under
    /// the first, and lifting either would report the pointer gone.
    touch: Option<u64>,
}

impl PointerBridge {
    /// Seed the position for a button that arrives before any motion has been
    /// seen (cursor already inside the window at startup).
    pub(crate) fn seed(&mut self, pos: Option<egui::Pos2>) {
        if self.last.is_none() {
            self.last = pos;
        }
    }
}

/// Map one Bevy window event to its egui pointer events, threading the running
/// cursor position so button events carry the position *at their point in the
/// stream*. Pure, so the ordering contract is unit-testable.
///
/// Order and per-event positions are load-bearing: a fast click-then-flick
/// batches `[press, release, move…]` into a single frame, and the old bridge —
/// moves first, then `ButtonInput` press/release stamped with the end-of-frame
/// `cursor_position()` — teleported the click to wherever the flick ended. egui
/// then either hit-tested the wrong widget or, with press and release split
/// across frames, cancelled the click as a drag (press→release further than its
/// ~6 pt click threshold). Panel checkboxes needed several tries because only a
/// motionless click survived; anything floating on an `Area` (menu bar, HUD)
/// felt fine since those get clicked deliberately.
///
/// **Touch is not a variant of the mouse here, it is the only input there is.**
/// winit's web backend sends a touch pointer as `WindowEvent::Touch` *only* —
/// never `CursorMoved`, never `MouseInput` — so without this arm every panel,
/// button and text field in the client is dead on a phone. One `Event::Touch`
/// goes out per finger (that is what egui's own multi-touch readers want), and
/// the finger holding the pointer role additionally synthesises the pointer
/// sequence egui needs for ordinary widgets: move-then-press on the way down,
/// press-release *then `PointerGone`* on the way up. `PointerGone` is not
/// optional — a finger leaves no cursor behind it, and without it egui goes on
/// hovering whatever was last touched, which lights up buttons under a finger
/// that left and keeps `egui_wants_pointer_input()` true over the map.
///
/// Appends rather than returning one event, because a single touch phase is
/// worth up to three egui events. `systems::touch` is the matching bridge for
/// the *world* (camera gestures and the emulated mouse) and does not go through
/// here — it reads the settled `Touches` set instead.
///
/// `ui_scale` converts the incoming positions — which are in the *window's*
/// logical points — into egui's (see [`EguiManager::window_to_ui`]). It is
/// applied here, at the one place events enter, so that `state.last` and every
/// event built from it are already in egui's space and nothing downstream has to
/// remember which of the two it is holding.
pub(crate) fn pointer_events(
    ev: &bevy::window::WindowEvent,
    ui_scale: f32,
    state: &mut PointerBridge,
    out: &mut Vec<egui::Event>,
) {
    use bevy::window::WindowEvent as We;
    let to_ui = |p: Vec2| egui::pos2(p.x / ui_scale, p.y / ui_scale);
    match ev {
        We::CursorMoved(e) => {
            let pos = to_ui(e.position);
            state.last = Some(pos);
            out.push(egui::Event::PointerMoved(pos));
        }
        We::MouseWheel(e) => {
            let (unit, delta) = match e.unit {
                MouseScrollUnit::Line => (egui::MouseWheelUnit::Line, egui::Vec2::new(e.x, e.y)),
                MouseScrollUnit::Pixel => (egui::MouseWheelUnit::Point, egui::Vec2::new(e.x, e.y)),
            };
            out.push(egui::Event::MouseWheel {
                unit,
                delta,
                // We don't distinguish trackpad scroll phases; "unknown" maps to Move.
                phase: egui::TouchPhase::Move,
                modifiers: egui::Modifiers::default(),
            });
        }
        We::MouseButtonInput(e) => {
            let button = match e.button {
                MouseButton::Left => egui::PointerButton::Primary,
                MouseButton::Right => egui::PointerButton::Secondary,
                MouseButton::Middle => egui::PointerButton::Middle,
                _ => return,
            };
            let Some(pos) = state.last else { return };
            out.push(egui::Event::PointerButton {
                pos,
                button,
                pressed: e.state == ButtonState::Pressed,
                modifiers: egui::Modifiers::default(),
            });
        }
        We::TouchInput(e) => {
            use bevy::input::touch::TouchPhase as BevyPhase;
            let pos = to_ui(e.position);
            out.push(egui::Event::Touch {
                // One touchscreen; egui only uses this to keep devices apart.
                device_id: egui::TouchDeviceId(0),
                id: egui::TouchId(e.id),
                phase: match e.phase {
                    BevyPhase::Started => egui::TouchPhase::Start,
                    BevyPhase::Moved => egui::TouchPhase::Move,
                    BevyPhase::Ended => egui::TouchPhase::End,
                    BevyPhase::Canceled => egui::TouchPhase::Cancel,
                },
                pos,
                force: None,
            });

            // Only the pointer-holding finger drives the widget-level events.
            if state.touch.is_some_and(|held| held != e.id) {
                return;
            }
            match e.phase {
                BevyPhase::Started => {
                    state.touch = Some(e.id);
                    state.last = Some(pos);
                    // Move first: egui hit-tests the press at the pointer's
                    // position, and a finger's press is the first time that
                    // position exists.
                    out.push(egui::Event::PointerMoved(pos));
                    out.push(egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: egui::Modifiers::default(),
                    });
                }
                BevyPhase::Moved => {
                    state.last = Some(pos);
                    out.push(egui::Event::PointerMoved(pos));
                }
                BevyPhase::Ended | BevyPhase::Canceled => {
                    state.touch = None;
                    out.push(egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed: false,
                        modifiers: egui::Modifiers::default(),
                    });
                    out.push(egui::Event::PointerGone);
                }
            }
        }
        _ => {}
    }
}

/// Release a finger the bridge still thinks is down but Bevy has already
/// forgotten, and say so to egui.
///
/// **One dropped touch-up is otherwise permanent input death.** `state.touch` is
/// set on `Started` and cleared in exactly one place — the `Ended | Canceled` arm
/// of [`pointer_events`] — so if that event never arrives, the id sticks. Android
/// touch ids only ever increase, so no later finger can match it, and
/// `pointer_events` returns early for every one of them: egui then receives
/// `Event::Touch` and nothing else, forever. No `PointerMoved`, no
/// `PointerButton`, no panel, no button, no way to close anything. Worse, the last
/// pointer event egui saw said `pressed: true`, so `is_using_pointer()` stays true
/// and the *world* consumers (`picking`, `camera_controls`) refuse every input as
/// well, on the grounds that egui has the pointer. The app renders at full rate
/// and responds to nothing, which is indistinguishable from a hang.
///
/// It is not hypothetical on this platform. The Android camera intent
/// (`platform::photo`) is the one thing that backgrounds the app mid-session, and
/// android-activity detaches the NativeActivity input queue on pause and discards
/// what is unconsumed — a finger still on the glass when the camera opens is
/// exactly the event that goes missing. `quiesce_while_backgrounded` calls
/// `Touches::release_all` on resume for that reason, and this is what turns that
/// into the release egui needs to hear.
///
/// The events are the same three the ordinary lift emits, in the same order, so
/// nothing downstream can tell a healed release from a real one — which is the
/// point: egui must not learn a new state, it must be told the one it missed.
/// `still_down` answers whether Bevy still has that finger pressed. Taken as a
/// predicate rather than as `&Touches` so this stays a pure function the tests
/// can drive — `Touches` has no public way to seed a pressed finger, and the one
/// behaviour worth pinning here is precisely what happens when it says "no".
fn heal_lost_touch(
    state: &mut PointerBridge,
    still_down: impl Fn(u64) -> bool,
    out: &mut Vec<egui::Event>,
) {
    let Some(held) = state.touch else { return };
    if still_down(held) {
        return;
    }
    state.touch = None;
    let Some(pos) = state.last else { return };
    tracing::debug!(held, "released a touch the bridge never saw lifted");
    out.push(egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::default(),
    });
    out.push(egui::Event::PointerGone);
}

/// Split off the pointer events for one egui pass: everything up to and
/// including the FIRST button event; the remainder stays in `pending` for the
/// next pass. egui hit-tests once per pass, at the batch's **final** pointer
/// position — so a pass whose batch continues past a press/release
/// re-attributes that button to wherever the pointer ended up afterwards (the
/// flick away from a fast click), and the click is lost. Deferring the tail
/// delays pointer motion by one frame only on frames containing a button, and
/// leaves drags intact: a `[press]` / `[moves…, release]` split keeps every
/// drag delta with its release.
pub(crate) fn take_pass_events(pending: &mut Vec<egui::Event>) -> Vec<egui::Event> {
    let split = pending
        .iter()
        .position(|e| matches!(e, egui::Event::PointerButton { .. }))
        .map(|i| i + 1)
        .unwrap_or(pending.len());
    let tail = pending.split_off(split);
    std::mem::replace(pending, tail)
}

/// Collect Bevy input and begin an egui frame (runs in PreUpdate).
pub fn begin_egui_frame(
    mut egui_manager: EguiMut,
    windows: Query<&Window>,
    headless: Option<Res<HeadlessSurface>>,
    keys: Res<ButtonInput<KeyCode>>,
    mut window_events: MessageReader<bevy::window::WindowEvent>,
    mut keyboard_input: MessageReader<KeyboardInput>,
    time: Res<Time>,
    touches: Res<bevy::input::touch::Touches>,
    mut pointer_bridge: Local<PointerBridge>,
    mut pointer_carry: Local<Vec<egui::Event>>,
) {
    // A frame with no surface at all still begins (and ends) an egui pass, laid
    // out against the last one we saw.
    //
    // The window entity is despawned by Bevy's `close_when_requested` (in `Last`)
    // while `Update` keeps running, so the shutdown sequence can put a whole frame
    // through the schedule with zero `Window`s in the world. Returning early here
    // left `root_ui` at `None` for that frame — and every panel system still ran,
    // each one hitting `ui_mut()`'s `expect` on a task-pool thread. That is the
    // macOS "ui_mut() called outside an egui frame" crash (#2): a stack with no
    // system name, on a compute-pool thread, at shutdown.
    //
    // The cost is one laid-out, tessellated frame that is never presented; the
    // gain is that `ui_mut()`'s invariant holds by construction instead of by the
    // convention that a window exists.
    egui_manager.metrics.observe(&windows, headless.as_deref());
    let (physical, scale) = (
        egui_manager.metrics.physical(),
        egui_manager.metrics.pixels_per_point(),
    );

    egui_manager.elapsed_time += time.delta_secs_f64();

    let logical_w = physical.x as f32 / scale;
    let logical_h = physical.y as f32 / scale;

    let mut events: Vec<egui::Event> = Vec::new();

    // Pointer input, from the ORDERED window-event stream (see [`pointer_event`]
    // for why the per-frame `ButtonInput`/`cursor_position()` snapshot lost
    // clicks). `last_pointer` persists across frames so a button event with no
    // motion this frame keeps the position of the last motion ever; the fallback
    // covers a click before any `CursorMoved` has been seen (cursor already
    // inside the window at startup). Headless runs see no window events at all.
    let ui_scale = egui_manager.ui_scale();
    pointer_bridge.seed(
        windows
            .iter()
            .next()
            .and_then(|w| w.cursor_position())
            .map(|p| egui::pos2(p.x / ui_scale, p.y / ui_scale)),
    );
    let mut pending = std::mem::take(&mut *pointer_carry);
    // A finger we still believe is down, that Bevy no longer has: heal before
    // reading this frame's events, so the recovery is ordered ahead of whatever
    // the new finger is doing.
    heal_lost_touch(
        &mut pointer_bridge,
        |id| touches.get_pressed(id).is_some(),
        &mut pending,
    );
    for ev in window_events.read() {
        pointer_events(ev, ui_scale, &mut pointer_bridge, &mut pending);
    }
    events.extend(take_pass_events(&mut pending));
    *pointer_carry = pending;

    // Keyboard: text input for text fields + special keys. Ctrl+C/V/X become
    // explicit clipboard events bridged to the system clipboard (this custom
    // integration has no winit clipboard plumbing of its own).
    //
    // Forwarded ONLY while text is actually being edited — the exact mirror of
    // `EguiManager::ui_owns_keyboard`, which yields the game's keys to the UI
    // under the same condition, so exactly one side ever acts on a key. egui
    // keeps *focus* on the last-clicked widget (a checkbox, a panel's collapse
    // button), and Space/Enter activate the focused widget: a game key as ordinary
    // as Space re-triggered whatever was clicked last, collapsing panels from
    // across the map. The reader is
    // drained regardless, or the keys held before clicking into a field would
    // arrive as a stale burst the moment it gains focus.
    let forward_keys = egui_manager.ctx.text_edit_focused();
    let ctrl = keys.pressed(KeyCode::ControlLeft) || keys.pressed(KeyCode::ControlRight);
    let shift = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);
    let alt = keys.pressed(KeyCode::AltLeft) || keys.pressed(KeyCode::AltRight);
    // macOS puts the accelerator on Super, not Control. Without this, `Cmd+C` in
    // a text field did nothing while `Ctrl+C` did — the wrong key on that
    // platform — and a held Cmd did not suppress avatar movement the way a held
    // Ctrl does.
    let cmd = keys.pressed(KeyCode::SuperLeft) || keys.pressed(KeyCode::SuperRight);
    let mac = cfg!(target_os = "macos");
    let accel = if mac { cmd } else { ctrl };
    let modifiers = egui::Modifiers {
        ctrl,
        shift,
        alt,
        mac_cmd: mac && cmd,
        command: accel,
    };
    for ev in keyboard_input.read() {
        // **Escape and Tab are dismissal and navigation, not activation.** The
        // gate above is right for keys that *fire* the focused widget — that is
        // what stopped Space collapsing a panel from across the map — and it was
        // applied to every key, which made every `Key::Escape` handler in the
        // client unreachable code and stranded Tab focus on the first widget it
        // reached. These two go through whether or not a field has focus;
        // everything else still waits its turn. See `systems::dismiss` for what
        // Escape then does with it.
        let always = matches!(ev.key_code, KeyCode::Escape | KeyCode::Tab);
        if !forward_keys && !always {
            continue;
        }
        if ev.state == ButtonState::Pressed && accel {
            match ev.key_code {
                KeyCode::KeyC => {
                    events.push(egui::Event::Copy);
                    continue;
                }
                KeyCode::KeyX => {
                    events.push(egui::Event::Cut);
                    continue;
                }
                KeyCode::KeyV => {
                    if let Some(text) = crate::utils::clipboard::get_text() {
                        events.push(egui::Event::Paste(text));
                    }
                    continue;
                }
                _ => {}
            }
        }
        if ev.state == ButtonState::Pressed && !accel {
            if let Some(ref text) = ev.text {
                let s = text.as_str();
                if !s.is_empty() && !s.chars().any(|c| c.is_control()) {
                    events.push(egui::Event::Text(s.to_string()));
                }
            }
        }
        if let Some(egui_key) = bevy_key_to_egui(ev.key_code) {
            events.push(egui::Event::Key {
                key: egui_key,
                physical_key: None,
                pressed: ev.state == ButtonState::Pressed,
                repeat: ev.repeat,
                modifiers,
            });
        }
    }

    // The in-app on-screen keyboard (`systems::onscreen_keyboard`), which is the
    // only keyboard two of our platforms have: NativeActivity brings up no IME at
    // all. It draws in Update, i.e. *inside* the pass it types into, so it cannot
    // put anything into this frame's `RawInput` — it queues instead, and lands one
    // frame later, here.
    //
    // Appended in the same place as the physical keyboard's events, after the
    // pointer batch, for the same reason: `take_pass_events` owns the ordering of
    // the pointer stream and a key event carries no position to order.
    //
    // Deliberately NOT behind `forward_keys`. That gate exists because egui keeps
    // focus on the last-clicked widget and would let a game key activate it; these
    // events are only produced while a text field is focused, and the very act of
    // tapping a key momentarily takes that focus away (which the keyboard puts
    // back) — so testing `text_edit_focused()` here would drop exactly the
    // keystroke that caused it.
    let injected = std::mem::take(&mut egui_manager.injected);
    events.extend(injected);

    let screen_rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(logical_w, logical_h));

    let mut viewport_info = egui::ViewportInfo::default();
    viewport_info.native_pixels_per_point = Some(scale);
    viewport_info.inner_rect = Some(screen_rect);

    let mut raw_input = egui::RawInput {
        screen_rect: Some(screen_rect),
        time: Some(egui_manager.elapsed_time),
        // The frame egui should plan for. Left at its default (1/60 s) egui
        // advanced its animations and the kinetic scroll by a sixtieth on a
        // phone frame of 25–40 ms, so a fling ran slow exactly where it is
        // most used. The last frame's length is the best guess for this one.
        predicted_dt: time.delta_secs().clamp(1.0 / 240.0, 0.1),
        events,
        focused: true,
        ..Default::default()
    };
    raw_input
        .viewports
        .insert(egui::ViewportId::ROOT, viewport_info);

    egui_manager.ctx.begin_pass(raw_input);

    // egui 0.35 shows panels inside a root `Ui` (not the `Context`). Build a
    // full-viewport background `Ui` that every UI system shares this frame, so
    // panels cooperate on layout exactly as the old ctx-based panels did.
    let root_ui = egui::Ui::new(
        egui_manager.ctx.clone(),
        egui::Id::new((egui_manager.ctx.viewport_id(), "__cartopolis_root_ui")),
        egui::UiBuilder::new()
            .layer_id(egui::LayerId::background())
            .max_rect(egui_manager.ctx.viewport_rect()),
    );
    egui_manager.root_ui = Some(root_ui);
}

fn bevy_key_to_egui(key: KeyCode) -> Option<egui::Key> {
    Some(match key {
        KeyCode::Escape => egui::Key::Escape,
        KeyCode::Tab => egui::Key::Tab,
        KeyCode::Backspace => egui::Key::Backspace,
        KeyCode::Delete => egui::Key::Delete,
        KeyCode::Enter => egui::Key::Enter,
        KeyCode::Space => egui::Key::Space,
        KeyCode::ArrowUp => egui::Key::ArrowUp,
        KeyCode::ArrowDown => egui::Key::ArrowDown,
        KeyCode::ArrowLeft => egui::Key::ArrowLeft,
        KeyCode::ArrowRight => egui::Key::ArrowRight,
        KeyCode::Home => egui::Key::Home,
        KeyCode::End => egui::Key::End,
        KeyCode::PageUp => egui::Key::PageUp,
        KeyCode::PageDown => egui::Key::PageDown,
        // The three letters egui's text editor binds to an accelerator. They
        // reach it only as a `Key` event — the `Event::Text` path above is
        // skipped while the accelerator is held — so without them `Ctrl+A`,
        // `Ctrl+Z` and `Ctrl+Y` were dead in every text field on the desktop,
        // while the on-screen keyboard, which injects `Key::A + COMMAND`
        // directly, could select-all on a phone. Nothing else in the client
        // wants a bare letter from egui, which is why the map stops here.
        KeyCode::KeyA => egui::Key::A,
        KeyCode::KeyZ => egui::Key::Z,
        KeyCode::KeyY => egui::Key::Y,
        _ => return None,
    })
}

/// Tessellate the egui frame and store the output for the render world (runs in PostUpdate).
/// Did a press at `at` (egui points) land on the UI?
///
/// A floating area, window or modal is found by `layer_id_at` and is the UI's
/// outright. The root layer covers the whole screen — egui registers it so panels
/// can catch clicks — so there the answer is whether the press is outside
/// `map_rect`, i.e. on a panel. Pure over the context's last pass, so it answers
/// the same whether or not the finger is still down.
fn press_over_ui(ctx: &egui::Context, map_rect: Option<egui::Rect>, at: egui::Pos2) -> bool {
    match ctx.layer_id_at(at) {
        None => false,
        Some(layer) if layer == egui::LayerId::background() => {
            map_rect.is_some_and(|map| !map.contains(at))
        }
        Some(_) => true,
    }
}

pub fn end_egui_frame(
    mut egui_manager: EguiMut,
    mut render_output: ResMut<EguiRenderOutput>,
    time: Res<Time<Real>>,
    // Wall-clock seconds since the last real write, for the repaint-delay test
    // below. A `Local` rather than a field on the output resource because writing
    // it there would go through `DerefMut` and mark the resource changed every
    // frame — defeating exactly the extract skip this function exists to set up.
    mut since_write: Local<f32>,
) {
    *since_write += time.delta_secs();
    // Unconditional, and deliberately not gated on a surface existing: every
    // `begin_pass` needs its `end_pass`, or a windowless frame would leave the
    // pass open and the next `begin_pass` would nest inside it. `begin_egui_frame`
    // laid this frame out against `surface` — quote the same numbers back.
    let physical = egui_manager.metrics.physical();

    // Drop the shared root `Ui`, finalizing its panel layers, before ending the pass.
    // What the panels left of it is the map, for `owns_click`'s next frame.
    egui_manager.map_rect = egui_manager
        .root_ui
        .take()
        .map(|root| root.available_rect_before_wrap());

    let full_output = egui_manager.ctx.end_pass();
    // Bridge egui-side copies (Ctrl+C in a text field) to the system clipboard.
    for command in &full_output.platform_output.commands {
        if let egui::OutputCommand::CopyText(text) = command {
            crate::utils::clipboard::set_text(text);
        }
    }
    let pixels_per_point = full_output.pixels_per_point;
    let size_in_pixels = [physical.x, physical.y];

    // Skip the whole tessellate → clone → upload chain when egui itself says the UI
    // is unchanged. `repaint_delay == Duration::MAX` is egui's "nothing to redraw"
    // signal; `Context::tessellate` never reuses the previous frame's work, so
    // without this a completely static menu bar + panel rebuilt (and reallocated)
    // its full vertex/index geometry, deep-copied it into the render world, and
    // re-uploaded it to the GPU, every single frame.
    //
    // Leaving `render_output` untouched is what makes it free: the last-written
    // primitives are still there, `egui_pass` redraws them against the fresh colour
    // target, and Bevy's change detection skips the extract clone.
    //
    // NB: these reads must not go through `DerefMut` — touching `render_output`
    // mutably would mark it changed and defeat the very skip we're setting up.
    let force = render_output.force_next_write;
    // How long egui is willing to go before it needs drawing again. `MAX` is "not
    // until something happens" — a static panel — and any finite value comes from a
    // `request_repaint_after`, i.e. from a widget that told us it animates
    // (`EguiManager::animating`).
    //
    // Comparing it against the time since the last write, rather than testing it
    // for `MAX`, is what makes those two cases different in the right way. A bare
    // `== MAX` test would treat "repaint within 250 ms" as "repaint now", and since
    // an animating widget re-requests on every pass it draws, the skip would be off
    // permanently while anything on screen animated — trading a frozen clock for
    // full-rate tessellation, which is not the trade worth making.
    let repaint_delay = full_output
        .viewport_output
        .get(&egui::ViewportId::ROOT)
        .map_or(std::time::Duration::MAX, |v| v.repaint_delay);
    // Zero, and only zero: `animating()`'s quarter-second ask (a clock
    // readout) is not motion the frame rate has to keep up with.
    egui_manager.ui_animating = repaint_delay.is_zero();
    let quiet = geometry_still_current(repaint_delay, *since_write);
    if !force
        && quiet
        && full_output.textures_delta.is_empty()
        && render_output.size_in_pixels == size_in_pixels
        && render_output.pixels_per_point == pixels_per_point
    {
        // Drop the shapes without tessellating them.
        return;
    }
    *since_write = 0.0;

    let clipped_primitives = egui_manager
        .ctx
        .tessellate(full_output.shapes, pixels_per_point);

    // Carry unconsumed texture deltas forward instead of dropping them.
    //
    // A delta is *incremental*: egui allocates a texture once with a full image
    // (`pos: None`) and every later change is a patch (`pos: Some`) against it.
    // egui clears its own accumulator on `end_pass`, so whatever is written here is
    // the only copy. Overwriting the previous `textures_delta` therefore loses the
    // allocation, and egui-wgpu answers the first patch against a texture it never
    // allocated with `panic!("Tried to update a texture that has not been allocated
    // yet.")` — an abort, not a dropped frame.
    //
    // Desktop never reached it because `egui_pass` runs from frame zero, so nothing
    // is ever unconsumed. **Android has no view for its first few seconds**: winit
    // logs `Creating new window` about five seconds after `android_main`, and every
    // egui frame before that produces deltas with no view to apply them. The font
    // atlas is allocated in that window, so the app aborted a few seconds after
    // startup on every launch (reproduced on the emulator 2026-08-15).
    //
    // `append` is egui's own merge and keeps order, so the allocation stays ahead of
    // its patches. It is a no-op once a view exists, which is the steady state on
    // every platform.
    // Bounded by `APPLIED_GENERATION`: carry forward only while the last write is
    // still unapplied, or every frame would re-upload the whole atlas forever.
    let mut textures_delta = if render_output.generation == applied_generation() {
        Default::default()
    } else {
        std::mem::take(&mut render_output.textures_delta)
    };
    textures_delta.append(full_output.textures_delta);

    *render_output = EguiRenderOutput {
        clipped_primitives,
        textures_delta,
        size_in_pixels,
        pixels_per_point,
        generation: render_output.generation.wrapping_add(1),
        force_next_write: false,
    };
}

/// Generation of the last [`EguiRenderOutput`] whose texture deltas actually
/// reached the GPU, written by `egui_pass` and read by `end_egui_frame`.
///
/// A `static` rather than a resource because the two live in different worlds:
/// `egui_pass` runs in the render world on an extracted copy, so it has no way to
/// tell the main world "I applied that" through ordinary change detection. One
/// `u64` of shared state is cheaper than a channel, and the only thing the main
/// world needs to know is whether its last write was consumed.
static APPLIED_GENERATION: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(u64::MAX);

fn applied_generation() -> u64 {
    APPLIED_GENERATION.load(std::sync::atomic::Ordering::Relaxed)
}

// ─── Render pass system ────────────────────────────────────────────────────────

/// Draws the tessellated egui output onto the view target. Runs per view in the
/// `Core3d` render schedule (Bevy 0.19's schedule-based render graph).
pub fn egui_pass(
    view: ViewQuery<(&'static ViewTarget, &'static ExtractedCamera)>,
    render_data: Res<EguiRenderOutput>,
    #[cfg(not(target_arch = "wasm32"))] egui_renderer_res: Res<EguiWgpuRenderer>,
    #[cfg(target_arch = "wasm32")] egui_renderer_res: bevy::ecs::system::NonSend<EguiWgpuRenderer>,
    render_device: Res<RenderDevice>,
    render_queue: Res<RenderQueue>,
    mut render_context: RenderContext,
) {
    if render_data.clipped_primitives.is_empty() && render_data.textures_delta.is_empty() {
        return;
    }

    let (view_target, camera) = view.into_inner();
    // The UI is the window's: a camera ordered before the main one renders
    // into a picture of its own (`avatar_video`'s, 512 × 384), and the
    // window's scissors on it fail the whole submission — silently, headless.
    if camera.order < 0 {
        return;
    }
    let render_queue = render_queue.into_inner();
    let mut guard = egui_renderer_res.0.lock().unwrap();

    // Draw onto the final presented output when there's a separate output texture
    // (HDR / upscaling), otherwise the main texture already is the swapchain. Either
    // way this is a single-sampled sRGB target, so egui reads correctly and stably.
    let has_out = view_target.out_texture().is_some();
    let texture_format = view_target
        .out_texture_view_format()
        .unwrap_or_else(|| view_target.main_texture_format());
    let sample_count = 1u32;

    // (Re-)create the wgpu renderer when the output format or sample count changes.
    let needs_recreate = guard
        .as_ref()
        .map(|(_, fmt, s, _, _)| *fmt != texture_format || *s != sample_count)
        .unwrap_or(true);

    if needs_recreate {
        *guard = Some((
            egui_wgpu::Renderer::new(
                render_device.wgpu_device(),
                texture_format,
                egui_wgpu::RendererOptions {
                    depth_stencil_format: None, // no depth buffer
                    msaa_samples: sample_count,
                    dithering: false,
                    ..Default::default()
                },
            ),
            texture_format,
            sample_count,
            // A fresh renderer holds no buffers, so force an upload below.
            None,
            // Whatever was queued for destruction belonged to the renderer this
            // replaces; dropping it releases those textures the safe way (wgpu
            // keeps a dropped resource alive until the buffers using it retire).
            Vec::new(),
        ));
    }

    let (renderer, _, _, uploaded_gen, pending_free) = guard.as_mut().unwrap();

    // Destroy last frame's retired textures *now*, at the top of the pass, rather
    // than at the bottom of the one that retired them — see the note where they
    // are queued. Bevy has submitted that frame's command buffers by the time this
    // runs again, so the destroy can no longer invalidate a recorded draw.
    for id in pending_free.drain(..) {
        renderer.free_texture(&id);
    }

    let screen_descriptor = egui_wgpu::ScreenDescriptor {
        size_in_pixels: render_data.size_in_pixels,
        pixels_per_point: render_data.pixels_per_point,
    };

    // Only re-upload when the output actually changed. `end_egui_frame` leaves
    // `render_data` untouched on quiet frames, so its `clipped_primitives` — and its
    // `textures_delta` — are the same ones already on the GPU; re-running these
    // would rewrite the whole vertex/index buffer set and re-apply a spent texture
    // delta every frame for an identical picture.
    let needs_upload = *uploaded_gen != Some(render_data.generation);

    let extra_cmd_bufs = if needs_upload {
        // Upload new / changed textures (font atlas, custom images).
        for (id, deltas) in &render_data.textures_delta.set {
            for delta in deltas {
                renderer.update_texture(render_device.wgpu_device(), render_queue, *id, delta);
            }
        }
        // Tell the main world these landed, so `end_egui_frame` stops carrying them
        // forward. Set here rather than at the end of the pass because this loop is
        // the whole of "applied" — everything below is geometry.
        APPLIED_GENERATION.store(render_data.generation, std::sync::atomic::Ordering::Relaxed);
        // Upload vertex/index buffers; collect any extra staging command buffers.
        let encoder = render_context.command_encoder();
        renderer.update_buffers(
            render_device.wgpu_device(),
            render_queue,
            encoder,
            &render_data.clipped_primitives,
            &screen_descriptor,
        )
    } else {
        Vec::new()
    };
    *uploaded_gen = Some(render_data.generation);

    // Draw egui on top of the presented frame.
    {
        let encoder = render_context.command_encoder();
        let color_attachment = if has_out {
            view_target.out_texture_color_attachment(None).unwrap()
        } else {
            view_target.get_color_attachment()
        };
        let mut render_pass = encoder
            .begin_render_pass(&RenderPassDescriptor {
                label: Some("egui_render_pass"),
                color_attachments: &[Some(color_attachment)],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            })
            .forget_lifetime();

        renderer.render(
            &mut render_pass,
            &render_data.clipped_primitives,
            &screen_descriptor,
        );
    }

    // Submit extra command buffers produced by paint callbacks.
    for cb in extra_cmd_bufs {
        render_context.add_command_buffer(cb);
    }

    // Queue the textures egui has finished with — once, on the frame the delta
    // arrived (see `needs_upload`), not every frame it stays resident. **Queued,
    // not freed**, because `Renderer::free_texture` calls `wgpu::Texture::destroy`,
    // which takes effect immediately rather than when the last user retires: doing
    // it here would destroy a texture whose draw call this very pass has just
    // recorded, and wgpu answers that at `Queue::submit` — after the render graph
    // has moved on — with
    //
    //     Validation Error … In a draw command … Texture with
    //     'egui_texid_Managed(3)' label has been destroyed
    //
    // `handle_render_error` halts the renderer on `Validation` (it recurs), so the
    // whole app freezes with the picture it last drew. Reported from a phone on
    // 2026-08-16: opening a saved place's photo and closing it again froze the app,
    // because egui retires a texture in the pass that its `TextureHandle` is
    // dropped in, and the viewer's window — image and all — was drawn in that same
    // pass. egui-wgpu's own winit painter has the same ordering constraint and
    // solves it by freeing after `Queue::submit`; the render graph submits for us,
    // so the equivalent here is "at the top of the next pass".
    if needs_upload {
        pending_free.extend(render_data.textures_delta.free.iter().copied());
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

/// A test's own egui passes, whose output nobody paints.
///
/// egui 0.36 debug-asserts that a `TexturesDelta` is never dropped unapplied, and
/// a test that runs a `Context` by hand has no painter to apply one, so these
/// clear the texture half of the output and hand back the rest.
#[cfg(test)]
pub(crate) trait UnpaintedPasses {
    fn run_ui_unpainted(
        &self,
        input: egui::RawInput,
        run_ui: impl FnMut(&mut egui::Ui),
    ) -> egui::FullOutput;
    fn end_pass_unpainted(&self) -> egui::FullOutput;
}

#[cfg(test)]
impl UnpaintedPasses for egui::Context {
    fn run_ui_unpainted(
        &self,
        input: egui::RawInput,
        run_ui: impl FnMut(&mut egui::Ui),
    ) -> egui::FullOutput {
        let mut out = self.run_ui(input, run_ui);
        out.textures_delta.clear();
        out
    }

    fn end_pass_unpainted(&self) -> egui::FullOutput {
        let mut out = self.end_pass();
        out.textures_delta.clear();
        out
    }
}

#[cfg(test)]
mod tests;

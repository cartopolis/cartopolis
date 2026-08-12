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

use bevy::{
    core_pipeline::{upscaling::upscaling, Core3d},
    input::{keyboard::KeyboardInput, mouse::MouseScrollUnit, ButtonState},
    prelude::*,
    render::{
        extract_resource::{ExtractResource, ExtractResourcePlugin},
        render_resource::{RenderPassDescriptor, TextureFormat},
        renderer::{RenderContext, RenderDevice, RenderQueue, ViewQuery},
        view::ViewTarget,
        RenderApp,
    },
};
use std::sync::Mutex;

use crate::resources::HeadlessSurface;

// ─── Main-world resource ─────────────────────────────────────────────────────

/// Holds the egui context for the main Bevy world.
/// Call [`EguiManager::ctx_mut`] inside Update systems to build UI.
#[derive(Resource)]
pub struct EguiManager {
    pub ctx: egui::Context,
    /// Full-viewport background `Ui` rebuilt each frame in `begin_egui_frame`.
    /// egui 0.35 shows panels inside a `Ui`; sharing one across all UI systems
    /// keeps the cooperative panel layout the old ctx-based panels had.
    root_ui: Option<egui::Ui>,
    elapsed_time: f64,
    /// The surface egui lays out against: physical pixels + the logical→physical
    /// scale, refreshed each frame in [`begin_egui_frame`]. UI systems read
    /// [`Self::compact`] off [`Self::viewport_pts`] to lay out responsively —
    /// egui has no media queries, so "responsive" is just branching on the live
    /// viewport width every frame.
    ///
    /// Kept here (rather than re-read from the `Window` each time) because it is
    /// also the fallback for a frame that has **no** surface at all — see
    /// [`begin_egui_frame`].
    ///
    /// The scale is the *effective* points-per-pixel — the display's own scale
    /// factor multiplied by [`Self::ui_scale`] — so everything derived from it
    /// ([`Self::viewport_pts`], [`Self::compact`]) is already in the same points
    /// egui lays out in.
    surface: (UVec2, f32),
    /// UI magnification on top of the display's scale factor. `1.0` everywhere a
    /// mouse is the pointer; larger on touch, where the layout is otherwise
    /// correct and physically too small to hit. See [`resolve_ui_scale`].
    ///
    /// This is the *only* thing that makes egui points differ from the window's
    /// logical points, which is why it is also the conversion factor every
    /// window→egui position has to go through ([`Self::window_to_ui`]).
    ui_scale: f32,
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
}

/// Viewport widths (in points) below this are treated as "compact" (phone-ish):
/// the bottom bar wraps and the right-side panels expand to fill the screen
/// instead of docking beside the map at a fixed 320 pt.
pub const COMPACT_WIDTH_PTS: f32 = 640.0;

/// Default UI magnification on the phone tier — **and why the web's is 1.0.**
///
/// The question is physical size, not taste. egui lays out in points, the
/// display scale factor turns those into pixels, and a phone's UI is therefore
/// already *sharp*; it is simply built for a mouse. `theme::style`'s
/// `interact_size` of 40×24 pt is the target to size, against a ~7.6 mm platform
/// guideline (Material's 48 dp) and a fingertip about that wide.
///
/// **On the web that has already been fixed, by something else.** The render
/// budget's stage transform (`index.html`, `docs/notes/wasm-performance.md`)
/// sizes the canvas to viewport ÷ k and scales it back up by k, where k is
/// `dpr / 1.5` on the phone tier. winit measures the *untransformed* box, so
/// Bevy's logical viewport shrinks by k and every egui point is then painted
/// across k CSS pixels. On the reported Fairphone 4 (dpr 2.75) k ≈ 1.83, which
/// takes a 24 pt row from 6.3 mm to **11.6 mm** on its own. Magnifying again on
/// top of that overshoots the guideline by half and — worse — takes the viewport
/// to 143 pt, well under the 240 pt floor `responsive_right_panel` gives a
/// compact panel, i.e. a panel wider than the screen it docks in.
///
/// Native Android has no such transform: nothing there has magnified anything,
/// and a point is a dp (1/160 in), so the same row is **3.8 mm**. That is the
/// platform this constant is still for.
///
/// So the default is per-platform and the *override* is the general answer —
/// see [`resolve_ui_scale`]. Both numbers are arguments from arithmetic, not
/// measurements; neither has been checked on glass.
const fn phone_ui_scale() -> f32 {
    if cfg!(target_arch = "wasm32") {
        1.0
    } else {
        1.5
    }
}

/// Clamp for an explicitly requested scale. A typo in a URL should make the UI
/// awkward, not unrecoverable — at 20× there is no longer a button to press to
/// get back.
const UI_SCALE_RANGE: std::ops::RangeInclusive<f32> = 0.5..=3.0;

/// How much to magnify the UI over the display's own scale factor.
///
/// Resolved once, at startup, from two sources in order:
///
/// 1. An explicit `?ui_scale=1.3` (web) / `CARTO_UI_SCALE=1.3` (desktop).
///    **This is the primary interface, not an escape hatch.** The physical size
///    of a point varies across devices with the display scale, the platform's
///    unit and — on the web — the render budget's stage transform, and this
///    function cannot see any of the three well enough to be right by default.
/// 2. Otherwise [`phone_ui_scale`] on the phone tier, and 1.0 off it.
///
/// **The tier comes from [`crate::quality`], which is deliberately the only
/// thing in the client that decides what kind of machine this is.** An earlier
/// version of this asked the browser `(pointer: coarse)` directly, which was a
/// second detector answering a nearly-identical question — and `quality`'s
/// module note is explicit that the browser decides once, in `index.html`,
/// because the canvas resolution is a page-layout fact Rust cannot reach. Two
/// detectors is how the UI ends up finger-sized on a machine rendering at
/// desktop resolution, or the reverse.
///
/// The cost of borrowing that answer is that `quality` asks for a coarse pointer
/// **and** a viewport under 900 px, so a large tablet is `Desktop` and gets
/// mouse-sized targets despite being driven by a finger. That is the wrong
/// answer for such a device and the right one for a touchscreen laptop, and it
/// is what `CARTO_UI_SCALE` is for until someone has a tablet to test on.
fn resolve_ui_scale() -> f32 {
    if let Some(raw) = crate::env_value("CARTO_UI_SCALE") {
        match raw.trim().parse::<f32>() {
            Ok(v) if v.is_finite() => {
                let clamped = v.clamp(*UI_SCALE_RANGE.start(), *UI_SCALE_RANGE.end());
                if clamped != v {
                    tracing::warn!("ui scale {v} out of range, using {clamped}");
                }
                return clamped;
            }
            _ => tracing::warn!("ignoring unparseable ui scale {raw:?}"),
        }
    }
    if crate::quality::is_phone() {
        phone_ui_scale()
    } else {
        1.0
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
    if compact {
        egui::Panel::right(id)
            .resizable(false)
            // Fill the whole viewport — any inset leaves a strip of map peeking
            // down the left edge of the docked panel.
            .exact_size(viewport_w.max(240.0))
    } else {
        egui::Panel::right(id)
            .resizable(true)
            .default_size(wide_width)
    }
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
        let ui_scale = resolve_ui_scale();
        if ui_scale != 1.0 {
            tracing::info!("UI scale {ui_scale}× (coarse pointer or CARTO_UI_SCALE)");
        }
        Self {
            ctx,
            root_ui: None,
            elapsed_time: 0.0,
            // Matches the primary window's default resolution in `main.rs`; only
            // ever used for the frames before the first `begin_egui_frame`.
            surface: (UVec2::new(1280, 720), ui_scale),
            ui_scale,
            modal: false,
            injected: Vec::new(),
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

    /// Logical viewport size in points (physical pixels ÷ display scale), as egui
    /// lays out against it this frame.
    pub fn viewport_pts(&self) -> egui::Vec2 {
        let (physical, scale) = self.surface;
        egui::vec2(physical.x as f32 / scale, physical.y as f32 / scale)
    }

    /// Record the surface this frame is laid out against, given the *display's*
    /// scale factor; [`Self::ui_scale`] is folded in here so there is one place
    /// the two are combined and no caller can use the wrong one.
    ///
    /// The scale is sanitised on the way in: it divides every size below, and a
    /// display handing back 0 (or NaN) — which is what a monitor being switched
    /// off under us looks like — would otherwise turn the whole viewport into
    /// infinities.
    fn set_surface(&mut self, physical: UVec2, display_scale: f32) {
        let display_scale = if display_scale.is_finite() && display_scale > 0.0 {
            display_scale
        } else {
            1.0
        };
        self.surface = (physical, display_scale * self.ui_scale);
    }

    /// The UI magnification in force (see [`resolve_ui_scale`]). Also the factor
    /// between the window's logical points and egui's.
    pub fn ui_scale(&self) -> f32 {
        self.ui_scale
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
        self.surface.1 / self.ui_scale.max(f32::EPSILON)
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
        egui::pos2(p.x / self.ui_scale, p.y / self.ui_scale)
    }

    /// Declare (each frame) whether a modal owns the screen — see the `modal`
    /// field. Written by `login_screen::render_login_screen`, read by
    /// [`Self::ui_owns_keyboard`].
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

    /// True when keyboard input belongs to the UI and the game (avatar movement,
    /// gizmo-mode keys) must ignore it: a text field is focused, the prim edit dialog
    /// is open, or a Ctrl chord is held (e.g. Ctrl+Shift+A toggling the AI panel).
    /// Pass `edit_dialog_open = false` for actions that should stay live while editing
    /// a prim (e.g. the gizmo Move/Rotate/Scale keys).
    ///
    /// Uses `text_edit_focused()`, NOT `egui_wants_keyboard_input()`: the latter is
    /// `memory.focused().is_some()`, i.e. true whenever *any* widget has focus — so a
    /// button/selectable in a window (e.g. the avatar chooser) kept focus after a
    /// click and swallowed WASD **and Space** (avatar movement + fly). We only want
    /// to yield the keyboard when text is actually being typed.
    pub fn ui_owns_keyboard(
        &self,
        edit_dialog_open: bool,
        keyboard: &ButtonInput<KeyCode>,
    ) -> bool {
        self.ctx.text_edit_focused()
            || self.modal
            || edit_dialog_open
            || keyboard.pressed(KeyCode::ControlLeft)
            || keyboard.pressed(KeyCode::ControlRight)
    }
}

// ─── Extracted render data ────────────────────────────────────────────────────

/// Tessellated egui output, extracted to the render world when it changes.
///
/// Deliberately *not* rewritten every frame — see [`end_egui_frame`]. `ExtractResource`'s
/// derive is a plain `clone()`, and `extract_resource` only skips it when the main-world
/// resource is unchanged, so an unconditional write meant deep-copying the whole UI
/// vertex/index geometry into the render world 60×/s for a pixel-identical picture.
#[derive(Resource, Clone, Default, ExtractResource)]
pub struct EguiRenderOutput {
    pub clipped_primitives: Vec<egui::ClippedPrimitive>,
    pub textures_delta: egui::TexturesDelta,
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
/// can detect and handle MSAA changes (e.g. camera settings changed), plus the
/// [`EguiRenderOutput::generation`] whose geometry/textures are currently resident
/// on the GPU (`None` until the first upload).
// On wasm `egui_wgpu::Renderer` holds WebGL handles that are `!Send + !Sync` (the
// browser GPU is main-thread-only), so it cannot be a normal Bevy `Resource` there
// — it is stored as a **non-send** resource instead (ADR-033). On native it stays a
// regular `Resource`. The `egui_pass` accessor and its registration mirror this.
#[cfg_attr(not(target_arch = "wasm32"), derive(Resource))]
#[derive(Default)]
pub struct EguiWgpuRenderer(
    pub Mutex<Option<(egui_wgpu::Renderer, TextureFormat, u32, Option<u64>)>>,
);

// ─── Plugin ──────────────────────────────────────────────────────────────────

pub struct EguiPlugin;

impl Plugin for EguiPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<EguiManager>()
            .init_resource::<EguiRenderOutput>()
            .add_plugins(ExtractResourcePlugin::<EguiRenderOutput>::default())
            // MUST run after Bevy has written this frame's input: begin_egui_frame
            // reads `ButtonInput`, cursor position and the keyboard/mouse events to
            // build egui's `RawInput`. Unordered, it can run *before* `InputSystem`
            // and collect stale/partial input, which corrupts click and keyboard-
            // focus handling (fields lose focus on keypress).
            .add_systems(PreUpdate, begin_egui_frame.after(bevy::input::InputSystems))
            .add_systems(PostUpdate, end_egui_frame);

        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };

        #[cfg(not(target_arch = "wasm32"))]
        render_app.init_resource::<EguiWgpuRenderer>();
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

/// The surface egui lays out against: the primary window, or the offscreen
/// target in a headless `--shot` run (see [`HeadlessSurface`]). Physical pixels
/// plus the logical→physical scale. `None` once the window has been despawned
/// and no headless target stands in — see [`begin_egui_frame`].
fn egui_surface(
    windows: &Query<&Window>,
    headless: Option<&Res<HeadlessSurface>>,
) -> Option<(UVec2, f32)> {
    if let Some(w) = windows.iter().next() {
        return Some((
            UVec2::new(w.physical_width(), w.physical_height()),
            w.scale_factor() as f32,
        ));
    }
    headless.map(|h| (h.size, h.scale))
}

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
    mut egui_manager: ResMut<EguiManager>,
    windows: Query<&Window>,
    headless: Option<Res<HeadlessSurface>>,
    keys: Res<ButtonInput<KeyCode>>,
    mut window_events: MessageReader<bevy::window::WindowEvent>,
    mut keyboard_input: MessageReader<KeyboardInput>,
    time: Res<Time>,
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
    if let Some((physical, scale)) = egui_surface(&windows, headless.as_ref()) {
        egui_manager.set_surface(physical, scale);
    }
    let (physical, scale) = egui_manager.surface;

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
    let ui_scale = egui_manager.ui_scale;
    pointer_bridge.seed(
        windows
            .iter()
            .next()
            .and_then(|w| w.cursor_position())
            .map(|p| egui::pos2(p.x / ui_scale, p.y / ui_scale)),
    );
    let mut pending = std::mem::take(&mut *pointer_carry);
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
    let modifiers = egui::Modifiers {
        ctrl,
        shift,
        command: ctrl,
        ..Default::default()
    };
    for ev in keyboard_input.read() {
        if !forward_keys {
            continue;
        }
        if ev.state == ButtonState::Pressed && ctrl {
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
        if ev.state == ButtonState::Pressed && !ctrl {
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
        _ => return None,
    })
}

/// Tessellate the egui frame and store the output for the render world (runs in PostUpdate).
pub fn end_egui_frame(
    mut egui_manager: ResMut<EguiManager>,
    mut render_output: ResMut<EguiRenderOutput>,
) {
    // Unconditional, and deliberately not gated on a surface existing: every
    // `begin_pass` needs its `end_pass`, or a windowless frame would leave the
    // pass open and the next `begin_pass` would nest inside it. `begin_egui_frame`
    // laid this frame out against `surface` — quote the same numbers back.
    let (physical, _) = egui_manager.surface;

    // Drop the shared root `Ui`, finalizing its panel layers, before ending the pass.
    egui_manager.root_ui = None;

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
    let quiet = full_output
        .viewport_output
        .get(&egui::ViewportId::ROOT)
        .is_some_and(|v| v.repaint_delay == std::time::Duration::MAX);
    if !force
        && quiet
        && full_output.textures_delta.is_empty()
        && render_output.size_in_pixels == size_in_pixels
        && render_output.pixels_per_point == pixels_per_point
    {
        // Drop the shapes without tessellating them.
        return;
    }

    let clipped_primitives = egui_manager
        .ctx
        .tessellate(full_output.shapes, pixels_per_point);

    *render_output = EguiRenderOutput {
        clipped_primitives,
        textures_delta: full_output.textures_delta,
        size_in_pixels,
        pixels_per_point,
        generation: render_output.generation.wrapping_add(1),
        force_next_write: false,
    };
}

// ─── Render pass system ────────────────────────────────────────────────────────

/// Draws the tessellated egui output onto the view target. Runs per view in the
/// `Core3d` render schedule (Bevy 0.19's schedule-based render graph).
pub fn egui_pass(
    view: ViewQuery<&'static ViewTarget>,
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

    let view_target = view.into_inner();
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
        .map(|(_, fmt, s, _)| *fmt != texture_format || *s != sample_count)
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
        ));
    }

    let (renderer, _, _, uploaded_gen) = guard.as_mut().unwrap();

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
        for (id, delta) in &render_data.textures_delta.set {
            renderer.update_texture(render_device.wgpu_device(), render_queue, *id, delta);
        }
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

    // Release GPU resources for textures egui no longer needs — once, on the frame
    // the delta arrived (see `needs_upload`), not every frame it stays resident.
    if needs_upload {
        for id in &render_data.textures_delta.free {
            renderer.free_texture(id);
        }
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::input::mouse::MouseButtonInput;
    use bevy::input::InputPlugin;
    use bevy::window::{
        CursorMoved, ExitCondition, PrimaryWindow, WindowCloseRequested, WindowEvent, WindowPlugin,
    };

    fn moved(x: f32, y: f32) -> WindowEvent {
        WindowEvent::CursorMoved(CursorMoved {
            window: Entity::PLACEHOLDER,
            position: Vec2::new(x, y),
            delta: None,
        })
    }

    fn button(pressed: bool) -> WindowEvent {
        WindowEvent::MouseButtonInput(MouseButtonInput {
            button: MouseButton::Left,
            state: if pressed {
                ButtonState::Pressed
            } else {
                ButtonState::Released
            },
            window: Entity::PLACEHOLDER,
        })
    }

    fn touch(id: u64, phase: bevy::input::touch::TouchPhase, x: f32, y: f32) -> WindowEvent {
        WindowEvent::TouchInput(bevy::input::touch::TouchInput {
            phase,
            position: Vec2::new(x, y),
            window: Entity::PLACEHOLDER,
            force: None,
            id,
        })
    }

    /// Run a window-event stream through the bridge, as `begin_egui_frame` does.
    fn bridge(stream: &[WindowEvent]) -> Vec<egui::Event> {
        bridge_scaled(stream, 1.0)
    }

    /// As [`bridge`], with an explicit UI scale.
    fn bridge_scaled(stream: &[WindowEvent], ui_scale: f32) -> Vec<egui::Event> {
        let mut state = PointerBridge::default();
        let mut out = Vec::new();
        for ev in stream {
            pointer_events(ev, ui_scale, &mut state, &mut out);
        }
        out
    }

    /// The click-then-flick frame: `[move→target, press, release, move→far]`
    /// arrives as one batch, and the buttons must be stamped with the position
    /// *at their point in the stream* (the target), not the end-of-frame cursor
    /// position (the far side of the flick). The old `ButtonInput` +
    /// `cursor_position()` bridge got exactly this wrong, which is why panel
    /// checkboxes only responded to a motionless click.
    #[test]
    fn buttons_carry_the_position_at_their_point_in_the_stream() {
        let target = egui::Pos2::new(100.0, 100.0);
        let stream = [
            moved(100.0, 100.0),
            button(true),
            button(false),
            moved(400.0, 300.0),
        ];
        let events = bridge(&stream);

        assert_eq!(events.len(), 4);
        match (&events[1], &events[2]) {
            (
                egui::Event::PointerButton {
                    pos: p1,
                    pressed: true,
                    ..
                },
                egui::Event::PointerButton {
                    pos: p2,
                    pressed: false,
                    ..
                },
            ) => {
                assert_eq!(*p1, target, "press teleported away from the pointer");
                assert_eq!(*p2, target, "release teleported away from the pointer");
            }
            other => panic!("expected press then release, got {other:?}"),
        }
        assert!(
            matches!(events[3], egui::Event::PointerMoved(p) if p.x == 400.0),
            "the flick motion must still be delivered, after the click"
        );
    }

    /// A finger down-move-up must produce the full pointer sequence, ending in
    /// `PointerGone` — a finger leaves no cursor behind it, and egui otherwise
    /// keeps hovering (and claiming) whatever it last touched.
    #[test]
    fn a_finger_synthesises_a_whole_pointer_sequence() {
        use bevy::input::touch::TouchPhase as P;
        let events = bridge(&[
            touch(7, P::Started, 100.0, 100.0),
            touch(7, P::Moved, 120.0, 140.0),
            touch(7, P::Ended, 120.0, 140.0),
        ]);
        // Every phase also goes out verbatim, for egui's own multi-touch readers.
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, egui::Event::Touch { .. }))
                .count(),
            3
        );
        let pointer: Vec<_> = events
            .iter()
            .filter(|e| !matches!(e, egui::Event::Touch { .. }))
            .collect();
        let down = egui::Pos2::new(100.0, 100.0);
        assert!(
            matches!(pointer[0], egui::Event::PointerMoved(p) if *p == down),
            "the press must be preceded by a move, or egui hit-tests it at the \
             stale position: {pointer:?}"
        );
        assert!(matches!(
            pointer[1],
            egui::Event::PointerButton { pressed: true, .. }
        ));
        assert!(matches!(pointer[2], egui::Event::PointerMoved(_)));
        assert!(matches!(
            pointer[3],
            egui::Event::PointerButton { pressed: false, .. }
        ));
        assert!(matches!(pointer[4], egui::Event::PointerGone));
        assert_eq!(pointer.len(), 5);
    }

    /// egui has one pointer. The second finger of a pinch must not drag the UI
    /// out from under the first, nor report the pointer gone when it lifts.
    #[test]
    fn only_the_first_finger_drives_the_egui_pointer() {
        use bevy::input::touch::TouchPhase as P;
        let events = bridge(&[
            touch(1, P::Started, 100.0, 100.0),
            touch(2, P::Started, 300.0, 100.0),
            touch(2, P::Moved, 400.0, 100.0),
            touch(2, P::Ended, 400.0, 100.0),
        ]);
        let pointer: Vec<_> = events
            .iter()
            .filter(|e| !matches!(e, egui::Event::Touch { .. }))
            .collect();
        // Only finger 1's move+press.
        assert_eq!(pointer.len(), 2, "{pointer:?}");
        assert!(
            !pointer
                .iter()
                .any(|e| matches!(e, egui::Event::PointerGone)),
            "the second finger lifting must not take the pointer with it"
        );
    }

    /// A cancelled touch (the browser taking the gesture, a call arriving) must
    /// release the pointer exactly as a lift does — otherwise egui is left with
    /// a button held down forever and every later tap reads as a drag.
    #[test]
    fn a_cancelled_touch_releases_the_pointer() {
        use bevy::input::touch::TouchPhase as P;
        let events = bridge(&[
            touch(1, P::Started, 100.0, 100.0),
            touch(1, P::Canceled, 100.0, 100.0),
            // The pointer is free again, so a fresh finger takes it.
            touch(2, P::Started, 200.0, 200.0),
        ]);
        let pressed: Vec<bool> = events
            .iter()
            .filter_map(|e| match e {
                egui::Event::PointerButton { pressed, .. } => Some(*pressed),
                _ => None,
            })
            .collect();
        assert_eq!(pressed, vec![true, false, true]);
    }

    /// End-to-end: a click-then-flick arriving as ONE frame's batch must still
    /// toggle a checkbox, via the pass-splitting the live bridge applies
    /// ([`take_pass_events`]). Without the split this exact scenario fails:
    /// egui hit-tests once per pass at the batch's final pointer position, so
    /// the click lands wherever the flick ended.
    #[test]
    fn click_then_flick_in_one_frame_still_clicks() {
        let ctx = egui::Context::default();
        crate::systems::theme::install(&ctx);
        let mut checked = true;
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 720.0));

        let mut target: Option<egui::Pos2> = None;
        let mut pending: Vec<egui::Event> = Vec::new();
        for frame in 0..6 {
            let mut raw = egui::RawInput::default();
            raw.screen_rect = Some(screen);
            if let (1, Some(pos)) = (frame, target) {
                // One frame's batch: arrive at the checkbox, click, flick away.
                pending = bridge(&[
                    moved(pos.x, pos.y),
                    button(true),
                    button(false),
                    moved(pos.x + 300.0, pos.y + 200.0),
                ]);
            }
            raw.events = take_pass_events(&mut pending);
            let rect = std::cell::Cell::new(None);
            let checked_ref = &mut checked;
            // The pass output is the paint job; this test only reads widget state.
            let _ = ctx.run_ui(raw, |ui| {
                egui::Panel::right("panel")
                    .default_size(260.0)
                    .show(ui, |ui| {
                        let resp = ui.checkbox(checked_ref, "a checkbox");
                        rect.set(Some(resp.rect));
                    });
            });
            if target.is_none() {
                target = rect.get().map(|r| r.center());
            }
        }
        assert!(pending.is_empty(), "the carry never drained");
        assert!(!checked, "the click-then-flick frame lost the click");
    }

    /// Drive a checkbox through the **app's own** frame shape: the shared root
    /// `Ui` that [`begin_egui_frame`] builds by hand, not the one `run_ui` hands
    /// out. Returns whether the click landed.
    ///
    /// `overlay` gets the map area (the root `Ui`'s leftover space, exactly what
    /// `place_pins` culls against) after the panel has taken its width, so a test
    /// can put a floating `Area` there and ask whether it eats the panel's clicks.
    fn click_checkbox_through_root_ui(overlay: impl Fn(&egui::Context, egui::Rect)) -> bool {
        click_checkbox_with_drift(overlay, 0.0)
    }

    /// As above, but the pointer drifts `drift` points between press and release —
    /// what a real hand does. The events interleave as
    /// `[moved, press]` … `[moved, release]`, which is the shape the live bridge
    /// hands egui.
    fn click_checkbox_with_drift(overlay: impl Fn(&egui::Context, egui::Rect), drift: f32) -> bool {
        let ctx = egui::Context::default();
        crate::systems::theme::install(&ctx);
        let mut checked = false;
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 720.0));

        // The panel slides open (`show_collapsible` animates), so a click aimed at
        // frame 0's rect would land on a moving target and prove nothing. Wait for
        // the checkbox to hold the same rect two passes running, then click that.
        let mut seen: Option<egui::Rect> = None;
        let mut settled: Option<egui::Pos2> = None;
        let mut clicked_at: Option<usize> = None;
        let mut pending: Vec<egui::Event> = Vec::new();
        for frame in 0..24 {
            let mut raw = egui::RawInput::default();
            raw.screen_rect = Some(screen);
            if let (None, Some(pos)) = (clicked_at, settled) {
                pending = bridge(&[
                    moved(pos.x, pos.y),
                    button(true),
                    moved(pos.x + drift, pos.y),
                    button(false),
                ]);
                clicked_at = Some(frame);
            }
            raw.events = take_pass_events(&mut pending);

            ctx.begin_pass(raw);
            // Verbatim from `begin_egui_frame` — the thing under test.
            let mut root = egui::Ui::new(
                ctx.clone(),
                egui::Id::new((ctx.viewport_id(), "__cartopolis_root_ui")),
                egui::UiBuilder::new()
                    .layer_id(egui::LayerId::background())
                    .max_rect(ctx.viewport_rect()),
            );
            let mut open = true;
            let rect = std::cell::Cell::new(None);
            let checked_ref = &mut checked;
            responsive_right_panel("layers_panel", false, 1280.0, 260.0).show_collapsible(
                &mut root,
                &mut open,
                |ui| {
                    let resp = crate::systems::theme::checkbox(ui, checked_ref, "Buildings");
                    rect.set(Some(resp.rect));
                },
            );
            overlay(&ctx, root.available_rect_before_wrap());
            drop(root);
            // The pass output is the paint job; this test only reads widget state.
            let _ = ctx.end_pass();

            if settled.is_none() {
                if let Some(r) = rect.get() {
                    if seen == Some(r) {
                        settled = Some(r.center());
                    }
                    seen = Some(r);
                }
            }
        }
        assert!(settled.is_some(), "the checkbox never settled into a rect");
        assert!(
            clicked_at.is_some_and(|f| f + 3 < 24),
            "the click was issued too late to be resolved"
        );
        checked
    }

    /// A checkbox in a right panel must respond to a click when nothing is over
    /// it. This is the app's real frame shape, which `run_ui` does not reproduce.
    #[test]
    fn panel_checkbox_clicks_through_the_shared_root_ui() {
        assert!(
            click_checkbox_through_root_ui(|_, _| {}),
            "a panel checkbox in the shared root `Ui` ignored a clean click"
        );
    }

    /// A hand does not hold a mouse still. Sub-pixel drift between press and
    /// release — measured at 0.67 and 0.89 pt in a live session, far inside egui's
    /// 6 pt `max_click_dist` — must not lose the click. In that session the only
    /// press that toggled anything was the one that drifted exactly 0.0 pt.
    #[test]
    fn a_drifting_hand_still_clicks() {
        for drift in [0.0, 0.5, 0.89, 2.0, 5.0] {
            assert!(
                click_checkbox_with_drift(|_, _| {}, drift),
                "{drift} pt of drift between press and release lost the click"
            );
        }
    }

    /// A floating `Area` covering the **map** must not eat clicks aimed at the
    /// panel beside it. Panels live on the background layer and every `Area`
    /// paints above them, so an overlay that fails to cull itself to the map area
    /// steals the panel's input — the whole reason `place_pins` culls rather than
    /// clips (see CLAUDE.md). This pins the contract from the panel's side.
    #[test]
    fn a_map_overlay_does_not_eat_panel_clicks() {
        assert!(
            click_checkbox_through_root_ui(|ctx, map_rect| {
                egui::Area::new(egui::Id::new("test_map_overlay"))
                    .fixed_pos(map_rect.left_top())
                    .show(ctx, |ui| {
                        ui.allocate_response(map_rect.size(), egui::Sense::click());
                    });
            }),
            "an overlay confined to the map area ate a click meant for the panel"
        );
    }

    /// A UI-bearing app with no render world (`EguiPlugin` skips its render-app
    /// half when there is none, which is exactly what a test wants).
    fn ui_app(primary_window: Option<Window>) -> App {
        let mut app = App::new();
        app.add_plugins((
            MinimalPlugins,
            InputPlugin,
            WindowPlugin {
                primary_window,
                exit_condition: ExitCondition::OnAllClosed,
                close_when_requested: true,
                ..default()
            },
            EguiPlugin,
        ))
        .add_systems(Update, touch_root_ui);
        app
    }

    /// The first thing every panel system does.
    fn touch_root_ui(mut egui: ResMut<EguiManager>) {
        let _ = egui.ui_mut();
    }

    /// The macOS shutdown crash (#2): Bevy despawns the window in `Last`, so the
    /// frames between the close and the process actually exiting run the whole
    /// `Update` schedule with no `Window` in the world. `begin_egui_frame` used to
    /// bail out there, leaving every panel system to panic on `ui_mut()`.
    #[test]
    fn update_survives_the_window_being_closed() {
        let mut app = ui_app(Some(Window::default()));
        app.update();

        let window = app
            .world_mut()
            .query_filtered::<Entity, With<PrimaryWindow>>()
            .single(app.world())
            .expect("primary window");
        app.world_mut()
            .write_message(WindowCloseRequested { window });

        // `close_when_requested` marks on one frame and despawns on the next, so
        // give it both — then run the frames that used to panic.
        for _ in 0..4 {
            app.update();
        }
        assert!(
            app.world_mut()
                .query_filtered::<Entity, With<Window>>()
                .iter(app.world())
                .next()
                .is_none(),
            "the window should be gone — otherwise this test never reaches the case it is about"
        );
    }

    /// The same invariant from the other end: a windowless run whose
    /// `HeadlessSurface` is missing — what a `--shot` capture looks like if the
    /// harness fails to insert one. The UI has nothing to lay out against, which
    /// is not a reason to take the process down.
    #[test]
    fn update_survives_a_run_with_no_surface_at_all() {
        let mut app = ui_app(None);
        app.update();
        app.update();
    }

    /// A zero/NaN scale factor divides every viewport size, so it is sanitised on
    /// the way in rather than propagated as infinities through panel layout.
    #[test]
    fn a_degenerate_scale_factor_never_reaches_layout() {
        let mut mgr = EguiManager::default();
        for bad in [0.0, -1.0, f32::NAN] {
            mgr.set_surface(UVec2::new(1280, 720), bad);
            assert_eq!(mgr.viewport_pts(), egui::vec2(1280.0, 720.0));
        }
        mgr.set_surface(UVec2::new(2560, 1440), 2.0);
        assert_eq!(mgr.viewport_pts(), egui::vec2(1280.0, 720.0));
    }

    /// The UI scale multiplies the widgets by dividing the viewport: the same
    /// screen holds fewer, larger points. Everything responsive keys off
    /// `viewport_pts`, so a phone that was merely narrow becomes narrow *and*
    /// compact from this one number.
    #[test]
    fn ui_scale_shrinks_the_viewport_it_magnifies() {
        let mut mgr = EguiManager::default();
        mgr.ui_scale = 1.5;
        // Android, where a phone's whole 1080 px width reaches Bevy: 393 pt of
        // viewport at a 2.75 scale factor, 262 pt of it after the magnification.
        mgr.set_surface(UVec2::new(1080, 2280), 2.75);
        assert!((mgr.viewport_pts().x - 261.8).abs() < 0.1);
        assert!(mgr.compact());
    }

    /// Why [`phone_ui_scale`] is 1.0 on the web: the render budget's stage
    /// transform has already shrunk the surface Bevy sees, so magnifying again
    /// takes the viewport under the 240 pt floor `responsive_right_panel` gives
    /// a compact panel — i.e. docks a panel wider than the screen.
    ///
    /// Same handset as above, but through the phone tier's k ≈ 1.83: winit
    /// measures the *untransformed* box, so Bevy's surface is 1080/1.83 px.
    #[test]
    fn the_web_phone_tier_has_no_room_to_magnify() {
        let surface = UVec2::new(590, 1245); // 1080×2280 ÷ 1.83
        let mut unscaled = EguiManager::default();
        unscaled.ui_scale = 1.0;
        unscaled.set_surface(surface, 2.75);
        assert!(unscaled.viewport_pts().x > 200.0, "usable at 1.0");

        let mut scaled = EguiManager::default();
        scaled.ui_scale = 1.5;
        scaled.set_surface(surface, 2.75);
        assert!(
            scaled.viewport_pts().x < 240.0,
            "1.5× would dock a 240 pt panel in a {} pt viewport",
            scaled.viewport_pts().x
        );
    }

    /// Positions enter in the window's logical points and must leave in egui's,
    /// or every widget is hit-tested at a position that drifts further from the
    /// finger the further it is from the top-left corner. Converted at the
    /// bridge so `state.last` — and the button events built from it — are
    /// already in egui's space.
    #[test]
    fn the_pointer_bridge_converts_into_egui_points() {
        let events = bridge_scaled(&[moved(300.0, 600.0), button(true)], 1.5);
        let positions: Vec<egui::Pos2> = events
            .iter()
            .filter_map(|e| match e {
                egui::Event::PointerMoved(p) => Some(*p),
                egui::Event::PointerButton { pos, .. } => Some(*pos),
                _ => None,
            })
            .collect();
        assert_eq!(positions, vec![egui::pos2(200.0, 400.0); 2]);
    }

    /// The same conversion, for the world-anchored overlays that get their
    /// positions from `Camera::world_to_viewport` rather than from an event.
    #[test]
    fn window_to_ui_matches_the_bridge() {
        let mut mgr = EguiManager::default();
        mgr.ui_scale = 1.5;
        assert_eq!(
            mgr.window_to_ui(Vec2::new(300.0, 600.0)),
            egui::pos2(200.0, 400.0)
        );
    }

    /// The seam the on-screen keyboard is built on: an event queued during
    /// `Update` must turn up in the **next** frame's `RawInput`, and the queue
    /// must be empty afterwards.
    ///
    /// It cannot be the same frame — `begin_egui_frame` has already assembled
    /// `RawInput` and begun the pass by the time any UI system runs — and a queue
    /// that were not drained would replay the last character typed on every frame
    /// forever, which is the failure this pins.
    #[test]
    fn injected_events_reach_the_next_frames_input() {
        let mut app = ui_app(Some(Window::default()));
        app.update();
        app.world_mut()
            .resource_mut::<EguiManager>()
            .inject_events([egui::Event::Text("q".to_owned())]);

        let before = app
            .world()
            .resource::<EguiManager>()
            .ctx
            .input(|i| i.events.len());
        app.update();

        let mgr = app.world().resource::<EguiManager>();
        assert!(
            mgr.injected.is_empty(),
            "the injection queue was not drained"
        );
        assert!(
            mgr.ctx.input(|i| i
                .events
                .iter()
                .any(|e| matches!(e, egui::Event::Text(t) if t == "q"))),
            "the queued character never reached egui (had {before} events before)"
        );

        // And exactly once: a second frame with nothing queued must be quiet.
        app.update();
        let mgr = app.world().resource::<EguiManager>();
        assert!(
            !mgr.ctx
                .input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Text(_)))),
            "the injected event was replayed on a later frame"
        );
    }

    /// The display scale and the UI magnification have to come apart again:
    /// `set_surface` multiplies them into one number, and anything reasoning
    /// about *physical* size (`onscreen_keyboard`'s 48 dp touch target) needs the
    /// density on its own.
    #[test]
    fn display_scale_survives_being_folded_into_the_surface() {
        let mut mgr = EguiManager::default();
        mgr.ui_scale = 1.5;
        mgr.set_surface(UVec2::new(1080, 2280), 2.75);
        assert!((mgr.display_scale() - 2.75).abs() < 1e-4);
    }

    /// An out-of-range scale is clamped rather than obeyed: at 20× there is no
    /// longer a button on screen to press to undo it.
    #[test]
    fn an_absurd_ui_scale_is_clamped() {
        for (raw, want) in [(20.0_f32, 3.0_f32), (0.01, 0.5), (1.4, 1.4)] {
            let clamped = raw.clamp(*UI_SCALE_RANGE.start(), *UI_SCALE_RANGE.end());
            assert_eq!(clamped, want, "clamping {raw}");
        }
    }

    /// Every system that draws into the **shared root `Ui`** must be in the panel
    /// chain in `lib.rs`, and this test is the guard the chain's comment used to
    /// say did not exist.
    ///
    /// Leaving one out does not misplace a panel — it makes two panels swap
    /// composition order between frames, and because they share egui's single
    /// `Background` layer the one that lands underneath stops receiving clicks on
    /// those frames. The symptom is a control that ignores about half its
    /// presses; `layer_id_at` still answers `Background`, so no layer-level
    /// diagnostic sees it, and it cannot reproduce in a single-panel test.
    ///
    /// This reads the sources rather than the schedule because the invariant is
    /// "draws into the root `Ui`" — a property of a function body, which the type
    /// system cannot express and a running `App` cannot be asked about.
    #[test]
    fn every_root_ui_panel_is_in_the_chain() {
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        let mut stack = vec![src.join("systems")];
        while let Some(dir) = stack.pop() {
            for e in std::fs::read_dir(&dir)
                .expect("systems/ is readable")
                .flatten()
            {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if p.extension().is_some_and(|x| x == "rs") {
                    files.push(p);
                }
            }
        }

        // The frame lifecycle brackets the panels by *schedule* (PreUpdate /
        // Update / PostUpdate + the render graph), not by ordering links, so it is
        // deliberately not in the chain.
        const LIFECYCLE: [&str; 3] = ["begin_egui_frame", "end_egui_frame", "egui_pass"];

        let mut drawers = Vec::new();
        for f in &files {
            let text = std::fs::read_to_string(f).expect("source is readable");
            // Split on top-level `fn ` starts; a body owns everything up to the next one.
            let mut idx: Vec<usize> = text
                .match_indices("\nfn ")
                .chain(text.match_indices("\npub fn "))
                .map(|(i, _)| i)
                .collect();
            idx.sort_unstable();
            idx.push(text.len());
            for w in idx.windows(2) {
                let chunk = &text[w[0]..w[1]];
                // A *system* that draws into the root `Ui` takes the manager
                // mutably; helpers like `responsive_right_panel` take plain
                // arguments and only build a widget, so they order nothing.
                if !chunk.contains("ui_mut()") || !chunk.contains("ResMut<EguiManager>") {
                    continue;
                }
                let name = chunk
                    .trim_start_matches(['\n'])
                    .trim_start_matches("pub ")
                    .trim_start_matches("fn ")
                    .split(['(', '<'])
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_string();
                if !name.is_empty() && !LIFECYCLE.contains(&name.as_str()) {
                    drawers.push(name);
                }
            }
        }
        assert!(
            drawers.len() >= 5,
            "found only {drawers:?} root-`Ui` drawers — the scan has broken, not the chain"
        );

        let lib = std::fs::read_to_string(src.join("lib.rs")).expect("lib.rs is readable");
        let chain = lib
            .split_once("Phase 7b")
            // Past the prose, which quotes `.chain()` while explaining it.
            .and_then(|(_, rest)| rest.split_once(".add_systems("))
            .and_then(|(_, rest)| rest.split_once(".chain()"))
            .map(|(block, _)| block.to_string())
            .expect("the Phase 7b panel chain still exists in lib.rs");
        assert!(
            chain.contains("render_place_pins"),
            "the chain block was extracted but looks wrong — anchors in lib.rs moved"
        );

        let missing: Vec<_> = drawers
            .iter()
            .filter(|d| !chain.contains(d.as_str()))
            .collect();
        assert!(
            missing.is_empty(),
            "these draw into the shared root `Ui` but are not in the Phase 7b chain: \
             {missing:?}\nAdd them to that tuple — an unordered pair swaps composition \
             order between frames and the panel underneath stops taking clicks."
        );
    }
}

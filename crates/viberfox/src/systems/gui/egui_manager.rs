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
    surface: (UVec2, f32),
}

/// Viewport widths (in points) below this are treated as "compact" (phone-ish):
/// the bottom bar wraps and the right-side panels expand to fill the screen
/// instead of docking beside the map at a fixed 320 pt.
pub const COMPACT_WIDTH_PTS: f32 = 640.0;

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
        // The Viberfox look — palette, spacing, type scale, brand font family.
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
            // Matches the primary window's default resolution in `main.rs`; only
            // ever used for the frames before the first `begin_egui_frame`.
            surface: (UVec2::new(1280, 720), 1.0),
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

    /// Record the surface this frame is laid out against. The scale is sanitised
    /// on the way in: it divides every size below, and a display handing back 0
    /// (or NaN) — which is what a monitor being switched off under us looks like
    /// — would otherwise turn the whole viewport into infinities.
    fn set_surface(&mut self, physical: UVec2, scale: f32) {
        let scale = if scale.is_finite() && scale > 0.0 {
            scale
        } else {
            1.0
        };
        self.surface = (physical, scale);
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

/// Map one Bevy window event to its egui pointer event, threading the running
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
pub(crate) fn pointer_event(
    ev: &bevy::window::WindowEvent,
    last_pointer: &mut Option<egui::Pos2>,
) -> Option<egui::Event> {
    use bevy::window::WindowEvent as We;
    match ev {
        We::CursorMoved(e) => {
            let pos = egui::Pos2::new(e.position.x, e.position.y);
            *last_pointer = Some(pos);
            Some(egui::Event::PointerMoved(pos))
        }
        We::MouseWheel(e) => {
            let (unit, delta) = match e.unit {
                MouseScrollUnit::Line => (egui::MouseWheelUnit::Line, egui::Vec2::new(e.x, e.y)),
                MouseScrollUnit::Pixel => (egui::MouseWheelUnit::Point, egui::Vec2::new(e.x, e.y)),
            };
            Some(egui::Event::MouseWheel {
                unit,
                delta,
                // We don't distinguish trackpad scroll phases; "unknown" maps to Move.
                phase: egui::TouchPhase::Move,
                modifiers: egui::Modifiers::default(),
            })
        }
        We::MouseButtonInput(e) => {
            let button = match e.button {
                MouseButton::Left => egui::PointerButton::Primary,
                MouseButton::Right => egui::PointerButton::Secondary,
                MouseButton::Middle => egui::PointerButton::Middle,
                _ => return None,
            };
            Some(egui::Event::PointerButton {
                pos: (*last_pointer)?,
                button,
                pressed: e.state == ButtonState::Pressed,
                modifiers: egui::Modifiers::default(),
            })
        }
        _ => None,
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
    mut last_pointer: Local<Option<egui::Pos2>>,
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
    if last_pointer.is_none() {
        *last_pointer = windows
            .iter()
            .next()
            .and_then(|w| w.cursor_position())
            .map(|p| egui::Pos2::new(p.x, p.y));
    }
    let mut pending = std::mem::take(&mut *pointer_carry);
    for ev in window_events.read() {
        pending.extend(pointer_event(ev, &mut last_pointer));
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
        egui::Id::new((egui_manager.ctx.viewport_id(), "__viberfox_root_ui")),
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

    /// The click-then-flick frame: `[move→target, press, release, move→far]`
    /// arrives as one batch, and the buttons must be stamped with the position
    /// *at their point in the stream* (the target), not the end-of-frame cursor
    /// position (the far side of the flick). The old `ButtonInput` +
    /// `cursor_position()` bridge got exactly this wrong, which is why panel
    /// checkboxes only responded to a motionless click.
    #[test]
    fn buttons_carry_the_position_at_their_point_in_the_stream() {
        let target = egui::Pos2::new(100.0, 100.0);
        let mut last = None;
        let stream = [
            moved(100.0, 100.0),
            button(true),
            button(false),
            moved(400.0, 300.0),
        ];
        let events: Vec<egui::Event> = stream
            .iter()
            .filter_map(|ev| pointer_event(ev, &mut last))
            .collect();

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
                let mut last = None;
                pending = [
                    moved(pos.x, pos.y),
                    button(true),
                    button(false),
                    moved(pos.x + 300.0, pos.y + 200.0),
                ]
                .iter()
                .filter_map(|ev| pointer_event(ev, &mut last))
                .collect();
            }
            raw.events = take_pass_events(&mut pending);
            let rect = std::cell::Cell::new(None);
            let checked_ref = &mut checked;
            ctx.run_ui(raw, |ui| {
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
                let mut last = None;
                pending = [
                    moved(pos.x, pos.y),
                    button(true),
                    moved(pos.x + drift, pos.y),
                    button(false),
                ]
                .iter()
                .filter_map(|ev| pointer_event(ev, &mut last))
                .collect();
                clicked_at = Some(frame);
            }
            raw.events = take_pass_events(&mut pending);

            ctx.begin_pass(raw);
            // Verbatim from `begin_egui_frame` — the thing under test.
            let mut root = egui::Ui::new(
                ctx.clone(),
                egui::Id::new((ctx.viewport_id(), "__viberfox_root_ui")),
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
            ctx.end_pass();

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

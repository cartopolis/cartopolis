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
    input::{
        keyboard::KeyboardInput,
        mouse::{MouseScrollUnit, MouseWheel},
        ButtonState,
    },
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
    /// Logical viewport size in **points** (physical pixels ÷ scale), refreshed
    /// each frame in [`begin_egui_frame`]. UI systems read [`Self::compact`] off
    /// this to lay out responsively — egui has no media queries, so "responsive"
    /// is just branching on the live viewport width every frame.
    viewport_pts: egui::Vec2,
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
            viewport_pts: egui::vec2(1280.0, 720.0),
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
        self.viewport_pts
    }

    /// True on a narrow (phone-ish) viewport — see [`COMPACT_WIDTH_PTS`]. UI
    /// systems branch on this for a responsive layout.
    pub fn compact(&self) -> bool {
        self.viewport_pts.x < COMPACT_WIDTH_PTS
    }

    /// Borrow this frame's shared root `Ui`, for showing panels
    /// (`egui::Panel`, `egui::CentralPanel`). Only valid during Update.
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
        render_app
            .world_mut()
            .init_non_send::<EguiWgpuRenderer>();
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
/// plus the logical→physical scale.
fn egui_surface(
    windows: &Query<&Window>,
    headless: Option<&Res<HeadlessSurface>>,
) -> Option<(UVec2, f32)> {
    if let Some(w) = windows.iter().next() {
        return Some((UVec2::new(w.physical_width(), w.physical_height()), w.scale_factor() as f32));
    }
    headless.map(|h| (h.size, h.scale))
}

/// Collect Bevy input and begin an egui frame (runs in PreUpdate).
pub fn begin_egui_frame(
    mut egui_manager: ResMut<EguiManager>,
    windows: Query<&Window>,
    headless: Option<Res<HeadlessSurface>>,
    mouse_buttons: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    mut mouse_wheel: MessageReader<MouseWheel>,
    mut cursor_moved: MessageReader<CursorMoved>,
    mut keyboard_input: MessageReader<KeyboardInput>,
    time: Res<Time>,
) {
    let Some((physical, scale)) = egui_surface(&windows, headless.as_ref()) else {
        return;
    };

    egui_manager.elapsed_time += time.delta_secs_f64();

    let logical_w = physical.x as f32 / scale;
    let logical_h = physical.y as f32 / scale;
    egui_manager.viewport_pts = egui::vec2(logical_w, logical_h);

    let mut events: Vec<egui::Event> = Vec::new();

    // Cursor movement
    for e in cursor_moved.read() {
        events.push(egui::Event::PointerMoved(egui::Pos2::new(
            e.position.x,
            e.position.y,
        )));
    }

    // Scroll wheel
    for e in mouse_wheel.read() {
        let (unit, delta) = match e.unit {
            MouseScrollUnit::Line => (
                egui::MouseWheelUnit::Line,
                egui::Vec2::new(e.x, e.y),
            ),
            MouseScrollUnit::Pixel => (
                egui::MouseWheelUnit::Point,
                egui::Vec2::new(e.x, e.y),
            ),
        };
        events.push(egui::Event::MouseWheel {
            unit,
            delta,
            // We don't distinguish trackpad scroll phases; "unknown" maps to Move.
            phase: egui::TouchPhase::Move,
            modifiers: egui::Modifiers::default(),
        });
    }

    // Mouse buttons. Headless has no cursor at all, so this block just doesn't run.
    if let Some(cursor_pos) = windows.iter().next().and_then(|w| w.cursor_position()) {
        let pos = egui::Pos2::new(cursor_pos.x, cursor_pos.y);
        for (bevy_btn, egui_btn) in [
            (MouseButton::Left, egui::PointerButton::Primary),
            (MouseButton::Right, egui::PointerButton::Secondary),
            (MouseButton::Middle, egui::PointerButton::Middle),
        ] {
            if mouse_buttons.just_pressed(bevy_btn) {
                events.push(egui::Event::PointerButton {
                    pos,
                    button: egui_btn,
                    pressed: true,
                    modifiers: egui::Modifiers::default(),
                });
            }
            if mouse_buttons.just_released(bevy_btn) {
                events.push(egui::Event::PointerButton {
                    pos,
                    button: egui_btn,
                    pressed: false,
                    modifiers: egui::Modifiers::default(),
                });
            }
        }
    }

    // Keyboard: text input for text fields + special keys. Ctrl+C/V/X become
    // explicit clipboard events bridged to the system clipboard (this custom
    // integration has no winit clipboard plumbing of its own).
    let ctrl = keys.pressed(KeyCode::ControlLeft) || keys.pressed(KeyCode::ControlRight);
    let shift = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);
    let modifiers = egui::Modifiers {
        ctrl,
        shift,
        command: ctrl,
        ..Default::default()
    };
    for ev in keyboard_input.read() {
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

    let screen_rect = egui::Rect::from_min_size(
        egui::Pos2::ZERO,
        egui::vec2(logical_w, logical_h),
    );

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
    windows: Query<&Window>,
    headless: Option<Res<HeadlessSurface>>,
) {
    let Some((physical, _)) = egui_surface(&windows, headless.as_ref()) else {
        return;
    };

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

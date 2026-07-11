//! Egui integration for Bevy 0.16.
//!
//! Manual integration using egui-wgpu as the rendering backend,
//! inspired by https://github.com/vladbat00/bevy_egui.
//!
//! Architecture:
//!   - `EguiManager` lives in the main world and holds the egui context.
//!   - `begin_egui_frame` (PreUpdate) collects Bevy input and starts the egui frame.
//!   - UI systems run in Update, calling `egui_manager.ctx_mut()` to build the UI.
//!   - `end_egui_frame` (PostUpdate) tessellates the output into `EguiRenderOutput`.
//!   - `ExtractResourcePlugin` copies `EguiRenderOutput` to the render world.
//!   - `EguiNode` (render graph) draws egui after `EndMainPassPostProcessing`.

use bevy::{
    core_pipeline::core_3d::graph::{Core3d, Node3d},
    ecs::query::QueryItem,
    input::{
        keyboard::KeyboardInput,
        mouse::{MouseScrollUnit, MouseWheel},
        ButtonState,
    },
    prelude::*,
    render::{
        extract_resource::{ExtractResource, ExtractResourcePlugin},
        render_graph::{
            NodeRunError, RenderGraphApp, RenderGraphContext, RenderLabel, ViewNode,
            ViewNodeRunner,
        },
        render_resource::RenderPassDescriptor,
        renderer::{RenderContext, RenderDevice, RenderQueue},
        view::{Msaa, ViewTarget},
        RenderApp,
    },
};
use std::sync::Mutex;

// ─── Main-world resource ─────────────────────────────────────────────────────

/// Holds the egui context for the main Bevy world.
/// Call [`EguiManager::ctx_mut`] inside Update systems to build UI.
#[derive(Resource)]
pub struct EguiManager {
    pub ctx: egui::Context,
    elapsed_time: f64,
}

impl Default for EguiManager {
    fn default() -> Self {
        Self {
            ctx: egui::Context::default(),
            elapsed_time: 0.0,
        }
    }
}

impl EguiManager {
    /// Borrow the egui context mutably. Only valid between `begin_egui_frame`
    /// and `end_egui_frame` (i.e. during the Update schedule).
    pub fn ctx_mut(&mut self) -> &mut egui::Context {
        &mut self.ctx
    }

    /// True when keyboard input belongs to the UI and the game (avatar movement,
    /// gizmo-mode keys) must ignore it: a text field is focused, the prim edit dialog
    /// is open, or a Ctrl chord is held (e.g. Ctrl+Shift+A toggling the AI panel).
    /// Pass `edit_dialog_open = false` for actions that should stay live while editing
    /// a prim (e.g. the gizmo Move/Rotate/Scale keys).
    pub fn ui_owns_keyboard(
        &self,
        edit_dialog_open: bool,
        keyboard: &ButtonInput<KeyCode>,
    ) -> bool {
        self.ctx.wants_keyboard_input()
            || edit_dialog_open
            || keyboard.pressed(KeyCode::ControlLeft)
            || keyboard.pressed(KeyCode::ControlRight)
    }
}

// ─── Extracted render data ────────────────────────────────────────────────────

/// Tessellated egui output, extracted to the render world each frame.
#[derive(Resource, Clone, Default, ExtractResource)]
pub struct EguiRenderOutput {
    pub clipped_primitives: Vec<egui::ClippedPrimitive>,
    pub textures_delta: egui::TexturesDelta,
    /// Physical window size in pixels (for `ScreenDescriptor`).
    pub size_in_pixels: [u32; 2],
    pub pixels_per_point: f32,
}

// ─── Render-world resource ─────────────────────────────────────────────────

/// Lazily-initialized egui-wgpu renderer in the render world.
/// Stores the renderer alongside the sample count it was compiled for so we
/// can detect and handle MSAA changes (e.g. camera settings changed).
#[derive(Resource, Default)]
pub struct EguiWgpuRenderer(pub Mutex<Option<(egui_wgpu::Renderer, u32)>>);

// ─── Plugin ──────────────────────────────────────────────────────────────────

pub struct EguiPlugin;

impl Plugin for EguiPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<EguiManager>()
            .init_resource::<EguiRenderOutput>()
            .add_plugins(ExtractResourcePlugin::<EguiRenderOutput>::default())
            .add_systems(PreUpdate, begin_egui_frame)
            .add_systems(PostUpdate, end_egui_frame);

        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };

        render_app
            .init_resource::<EguiWgpuRenderer>()
            .add_render_graph_node::<ViewNodeRunner<EguiNode>>(Core3d, EguiNodeLabel)
            .add_render_graph_edges(
                Core3d,
                (
                    Node3d::EndMainPassPostProcessing,
                    EguiNodeLabel,
                    Node3d::Upscaling,
                ),
            );
    }
}

// ─── Render graph label ───────────────────────────────────────────────────────

#[derive(Debug, Hash, PartialEq, Eq, Clone, RenderLabel)]
pub struct EguiNodeLabel;

// ─── Systems ──────────────────────────────────────────────────────────────────

/// Collect Bevy input and begin an egui frame (runs in PreUpdate).
pub fn begin_egui_frame(
    mut egui_manager: ResMut<EguiManager>,
    windows: Query<&Window>,
    mouse_buttons: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    mut mouse_wheel: EventReader<MouseWheel>,
    mut cursor_moved: EventReader<CursorMoved>,
    mut keyboard_input: EventReader<KeyboardInput>,
    time: Res<Time>,
) {
    let window = match windows.iter().next() {
        Some(w) => w,
        None => return,
    };

    egui_manager.elapsed_time += time.delta_secs_f64();

    let scale = window.scale_factor() as f32;
    let logical_w = window.width();
    let logical_h = window.height();

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
            modifiers: egui::Modifiers::default(),
        });
    }

    // Mouse buttons
    if let Some(cursor_pos) = window.cursor_position() {
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
    egui_manager: ResMut<EguiManager>,
    mut render_output: ResMut<EguiRenderOutput>,
    windows: Query<&Window>,
) {
    let window = match windows.iter().next() {
        Some(w) => w,
        None => return,
    };

    let full_output = egui_manager.ctx.end_pass();
    // Bridge egui-side copies (Ctrl+C in a text field) to the system clipboard.
    for command in &full_output.platform_output.commands {
        if let egui::OutputCommand::CopyText(text) = command {
            crate::utils::clipboard::set_text(text);
        }
    }
    let pixels_per_point = full_output.pixels_per_point;
    let clipped_primitives = egui_manager
        .ctx
        .tessellate(full_output.shapes, pixels_per_point);

    *render_output = EguiRenderOutput {
        clipped_primitives,
        textures_delta: full_output.textures_delta,
        size_in_pixels: [window.physical_width(), window.physical_height()],
        pixels_per_point,
    };
}

// ─── Render graph node ────────────────────────────────────────────────────────

#[derive(Default)]
pub struct EguiNode;

impl ViewNode for EguiNode {
    type ViewQuery = (&'static ViewTarget, &'static Msaa);

    fn run<'w>(
        &self,
        _graph: &mut RenderGraphContext,
        render_context: &mut RenderContext<'w>,
        (view_target, msaa): QueryItem<'w, Self::ViewQuery>,
        world: &'w World,
    ) -> Result<(), NodeRunError> {
        let render_data = world.resource::<EguiRenderOutput>();

        if render_data.clipped_primitives.is_empty() && render_data.textures_delta.is_empty() {
            return Ok(());
        }

        let render_device = world.resource::<RenderDevice>();
        let render_queue = world.resource::<RenderQueue>();
        let egui_renderer_res = world.resource::<EguiWgpuRenderer>();
        let mut guard = egui_renderer_res.0.lock().unwrap();

        let texture_format = view_target.main_texture_format();
        let sample_count = msaa.samples();

        // (Re-)create the wgpu renderer when format or MSAA sample count changes.
        let needs_recreate = guard
            .as_ref()
            .map(|(_, s)| *s != sample_count)
            .unwrap_or(true);

        if needs_recreate {
            *guard = Some((
                egui_wgpu::Renderer::new(
                    render_device.wgpu_device(),
                    texture_format,
                    None,        // no depth buffer
                    sample_count,
                    false,       // no dithering
                ),
                sample_count,
            ));
        }

        let (renderer, _) = guard.as_mut().unwrap();

        let screen_descriptor = egui_wgpu::ScreenDescriptor {
            size_in_pixels: render_data.size_in_pixels,
            pixels_per_point: render_data.pixels_per_point,
        };

        // Upload new / changed textures (font atlas, custom images).
        for (id, delta) in &render_data.textures_delta.set {
            renderer.update_texture(
                render_device.wgpu_device(),
                render_queue,
                *id,
                delta,
            );
        }

        // Upload vertex/index buffers; collect any extra staging command buffers.
        let extra_cmd_bufs = {
            let encoder = render_context.command_encoder();
            renderer.update_buffers(
                render_device.wgpu_device(),
                render_queue,
                encoder,
                &render_data.clipped_primitives,
                &screen_descriptor,
            )
        };

        // Draw egui on top of the current frame contents.
        {
            let encoder = render_context.command_encoder();
            let color_attachment = view_target.get_color_attachment();
            let mut render_pass = encoder
                .begin_render_pass(&RenderPassDescriptor {
                    label: Some("egui_render_pass"),
                    color_attachments: &[Some(color_attachment)],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
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

        // Release GPU resources for textures egui no longer needs.
        for id in &render_data.textures_delta.free {
            renderer.free_texture(id);
        }

        Ok(())
    }
}

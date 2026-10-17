//! The blur behind the glass.
//!
//! `theme::GLASS` made the chrome translucent; this is what makes it read as a
//! *material* rather than as a dimmed hole. Every panel, window, menu and
//! floating readout gets a blurred copy of the frame underneath it, and the
//! translucent fill then tints that copy instead of tinting the map itself.
//!
//! **Why this needed a render pass at all.** egui has no backdrop-blur anything:
//! its shapes are flat fills, and a fill composited over a scene shows that
//! scene's detail no matter how opaque it is. Blur is the one property of glass
//! that cannot be faked with a colour, and it is also the property that *buys*
//! transparency — with the detail behind a panel reduced to a wash, the fill can
//! be much thinner and still be read over. `theme::GLASS` went from 95 % to 84 %
//! the day this landed, and 84 % is a measured floor rather than a taste: see
//! the note by `theme::GLASS`.
//!
//! # How it hangs together
//!
//! Three pieces, in two worlds:
//!
//!  1. **Main world, [`insert_backdrops`]** — called from `end_egui_frame` on the
//!     shape list, *before* tessellation. Every `Shape::Rect` painted in one of
//!     the glass fills gains a twin underneath it: the same rectangle, the same
//!     corner radius, textured with the backdrop and tinted white (i.e. passed
//!     through). egui then tessellates and rounds both together, so the blur is
//!     masked to the panel's own corners with no work on our side.
//!  2. **Render world, [`Backdrop::render`]** — called from `egui_pass` before the
//!     egui draw, it downsamples the frame to a quarter and runs a separable
//!     Gaussian over it.
//!  3. **The seam between them** is one atomic holding an `egui::TextureId`. The
//!     main world needs the id at *tessellation* time and the texture is
//!     registered in the render world, so the first frame or two run with no
//!     backdrop and fall back to the flat fill — which is exactly the look
//!     before this existed, and is also the failure mode if anything here does
//!     not initialise. **Nothing in this module is allowed to be load-bearing.**
//!
//! # Three things that are load-bearing
//!
//!   - **The source is the main texture, not the output texture.** egui draws
//!     onto the post-upscaling output, and a texture cannot be sampled and
//!     rendered to in the same pass; the main texture holds the same tonemapped
//!     image one step earlier in the chain and is sampleable.
//!   - **Only part of the main texture is the picture.** `systems::render_scale`
//!     shrinks the *viewport* on the phone tier while the texture stays full
//!     size, so sampling `0..1` would blur the rendered image together with the
//!     unrendered margin beside it and smear the edge of the screen. Every UV
//!     here is scaled by `viewport / texture` and clamped to it.
//!   - **UVs are screen-relative, so a skipped tessellation is still correct.**
//!     `end_egui_frame` reuses last frame's geometry whenever egui says nothing
//!     changed, and that geometry carries the backdrop UVs. They address a fixed
//!     screen position rather than a moment in time, so a static panel over a
//!     moving map keeps a live blur without being re-tessellated.

use egui_wgpu::wgpu;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::systems::theme;

/// How much the frame is shrunk before it is blurred.
///
/// The downsample *is* most of the blur — a bilinear tap at quarter size already
/// averages a 4×4 block — so the Gaussian after it only has to hide the
/// downsample's own structure. Going wider costs a bigger kernel for the same
/// look; going narrower costs fill rate for detail that is about to be thrown
/// away.
const SHRINK: u32 = 4;

/// The blurred backdrop's `egui::TextureId`, plus one so that zero can mean
/// "there isn't one".
///
/// A `static` rather than a resource for the reason `APPLIED_GENERATION` in
/// `egui_manager` is one: the writer is in the render world and the reader is in
/// the main world, and change detection does not cross that boundary. One `u64`
/// is cheaper than a channel and the only thing the main world needs to know.
static BACKDROP_ID: AtomicU64 = AtomicU64::new(0);

/// The registered backdrop, if the render world has one yet.
#[must_use]
pub fn backdrop_id() -> Option<egui::TextureId> {
    match BACKDROP_ID.load(Ordering::Relaxed) {
        0 => None,
        n => Some(egui::TextureId::User(n - 1)),
    }
}

fn publish(id: egui::TextureId) {
    if let egui::TextureId::User(n) = id {
        BACKDROP_ID.store(n + 1, Ordering::Relaxed);
    }
}

/// Forget the backdrop — the next frames paint flat fills until one is
/// registered again.
///
/// Called when the renderer is rebuilt, which is also what happens after a
/// device loss: the id would otherwise name a texture belonging to a dead
/// device, and egui would draw it. That is a `Validation` error, and
/// `handle_render_error` halts the renderer on those *by design*, so the app
/// would freeze rather than lose a frame.
pub fn forget() {
    BACKDROP_ID.store(0, Ordering::Relaxed);
}

// ─── Main world: hanging a blurred twin under every glass rectangle ───────────

/// Is this fill one the chrome uses for glass?
///
/// Deliberately a colour test rather than a flag on the call site. Every glass
/// surface in the app already goes through `theme::GLASS` or `theme::OVERLAY`,
/// so matching on those two covers the panels, the windows, the menus, the
/// popups and the floating readouts at once — and, more usefully, covers the
/// next one somebody adds without their having to know this module exists.
///
/// `theme::LABEL_CHIP` is **not** in the set. Those are the few-word plates
/// painted straight onto the map, and they are deliberately dark and nearly
/// opaque: there is nothing to see through them, and blurring a 20 pt chip
/// costs the same as blurring a panel.
fn is_glass(fill: egui::Color32) -> bool {
    fill == theme::GLASS || fill == theme::OVERLAY
}

/// Screen geometry needed to turn a rectangle in points into texture coordinates.
struct Screen {
    /// Points per screen edge, i.e. the size the shapes are laid out against.
    size: egui::Vec2,
}

/// Put a blurred copy of the frame under every glass rectangle in `shapes`.
///
/// A no-op when the render world has not registered a backdrop yet, when the
/// screen has no area, or when the shape list holds no glass — so the cost on a
/// frame with nothing translucent on it is one walk of the list.
pub fn insert_backdrops(
    shapes: &mut Vec<egui::epaint::ClippedShape>,
    size_in_pixels: [u32; 2],
    pixels_per_point: f32,
) {
    let Some(id) = backdrop_id() else {
        return;
    };
    if pixels_per_point <= 0.0 || size_in_pixels[0] == 0 || size_in_pixels[1] == 0 {
        return;
    }
    let screen = Screen {
        size: egui::vec2(
            size_in_pixels[0] as f32 / pixels_per_point,
            size_in_pixels[1] as f32 / pixels_per_point,
        ),
    };
    for clipped in shapes.iter_mut() {
        expand(std::slice::from_mut(&mut clipped.shape), id, &screen);
    }
    // The walk above can only *replace* a shape in place, because a
    // `ClippedShape` holds exactly one. A glass rectangle at the top level is
    // therefore rewritten into a `Shape::Vec` of two — see `expand`.
}

/// Walk a shape list, replacing each glass rectangle with a two-shape group:
/// the blurred backdrop, then the rectangle itself.
///
/// Recursive because egui groups shapes into `Shape::Vec` in several places
/// (a `Frame`'s background and its stroke arrive as one, for instance), and a
/// panel background nested one level down is still a panel background.
fn expand(shapes: &mut [egui::Shape], id: egui::TextureId, screen: &Screen) {
    for shape in shapes.iter_mut() {
        match shape {
            egui::Shape::Rect(rect) if is_glass(rect.fill) => {
                let Some(backdrop) = backdrop_shape(rect, id, screen) else {
                    continue;
                };
                let glass = std::mem::replace(shape, egui::Shape::Noop);
                *shape = egui::Shape::Vec(vec![backdrop, glass]);
            }
            egui::Shape::Vec(inner) => expand(inner, id, screen),
            _ => {}
        }
    }
}

/// The blurred twin of one glass rectangle: same rectangle, same corners, the
/// backdrop texture, no tint and no stroke.
///
/// The tint is the glass rectangle drawn *over* this one, unchanged — which is
/// what keeps the palette in one place. A brush multiplies the texture by the
/// shape's fill, so tinting here instead would darken the blur by the panel
/// colour rather than compositing over it, and a 84 %-alpha slate would come out
/// as a near-black pane.
fn backdrop_shape(
    rect: &egui::epaint::RectShape,
    id: egui::TextureId,
    screen: &Screen,
) -> Option<egui::Shape> {
    let r = rect.rect;
    if !r.is_positive() || screen.size.x <= 0.0 || screen.size.y <= 0.0 {
        return None;
    }
    let uv = egui::Rect::from_min_max(
        egui::pos2(r.min.x / screen.size.x, r.min.y / screen.size.y),
        egui::pos2(r.max.x / screen.size.x, r.max.y / screen.size.y),
    );
    let mut backdrop = egui::epaint::RectShape::filled(r, rect.corner_radius, egui::Color32::WHITE);
    backdrop.round_to_pixels = rect.round_to_pixels;
    Some(egui::Shape::Rect(backdrop.with_texture(id, uv)))
}

// ─── Render world: making the blurred copy ───────────────────────────────────

/// The two working textures, the pipeline that fills them, and the id egui knows
/// the second one by.
pub struct Backdrop {
    /// Size of the main texture these were built against. A change means the
    /// window resized (or the render scale moved) and everything is rebuilt.
    source: (u32, u32),
    work: (u32, u32),
    horizontal: wgpu::TextureView,
    blurred: wgpu::TextureView,
    sampler: wgpu::Sampler,
    layout: wgpu::BindGroupLayout,
    pipeline: wgpu::RenderPipeline,
    /// The horizontal pass's uniform, rewritten each frame — its contents depend
    /// on the *viewport*, which the phone tier is free to move between frames.
    /// The vertical pass's is written once at build time and is kept alive by
    /// the bind group below.
    params_h: wgpu::Buffer,
    /// The second pass reads a texture this struct owns, so its bind group is
    /// built once. The first pass reads the *view target*, which ping-pongs
    /// between two textures, so its bind group has to be built per frame.
    bind_v: wgpu::BindGroup,
    id: egui::TextureId,
}

/// `vec4(uv_scale.xy, step.xy)` — see the shader.
const PARAMS_SIZE: u64 = 16;

impl Backdrop {
    /// Build the backdrop for a view of `size`, or hand back `None` if it is too
    /// small to blur.
    ///
    /// Fallible rather than panicking on purpose: a failure here has to end in a
    /// flat fill, never in a stopped renderer.
    fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        renderer: &mut egui_wgpu::Renderer,
        size: (u32, u32),
    ) -> Option<Self> {
        let work = (size.0 / SHRINK, size.1 / SHRINK);
        if work.0 < 2 || work.1 < 2 {
            return None;
        }
        let texture = |label: &str| {
            device
                .create_texture(&wgpu::TextureDescriptor {
                    label: Some(label),
                    size: wgpu::Extent3d {
                        width: work.0,
                        height: work.1,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    // sRGB, so the shader can work in linear and let the hardware
                    // encode on write — and so egui, which treats every texture it
                    // samples as sRGB, reads it back correctly.
                    format: wgpu::TextureFormat::Rgba8UnormSrgb,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                        | wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                })
                .create_view(&wgpu::TextureViewDescriptor::default())
        };
        let horizontal = texture("glass_backdrop_h");
        let blurred = texture("glass_backdrop");

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("glass_backdrop_sampler"),
            // Clamp, not repeat: a tap that walks off the edge must find the edge
            // pixel rather than the opposite side of the screen.
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });

        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("glass_backdrop_layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("glass_backdrop_shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("glass.wgsl").into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("glass_backdrop_pipeline_layout"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("glass_backdrop_pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vertex"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fragment"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::Rgba8UnormSrgb,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let params = |label: &str| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: PARAMS_SIZE,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        };
        let params_h = params("glass_backdrop_params_h");
        let params_v = params("glass_backdrop_params_v");
        // The vertical pass reads a texture this struct owns, so its whole input
        // is known now: the full image, one working texel per step.
        queue.write_buffer(&params_v, 0, &pack(1.0, 1.0, 0.0, 1.0 / work.1 as f32));

        let bind_v = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("glass_backdrop_bind_v"),
            layout: &layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&horizontal),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: params_v.as_entire_binding(),
                },
            ],
        });

        let id = renderer.register_native_texture(device, &blurred, wgpu::FilterMode::Linear);

        Some(Self {
            source: size,
            work,
            horizontal,
            blurred,
            sampler,
            layout,
            pipeline,
            params_h,
            bind_v,
            id,
        })
    }

    /// Blur `source` into the backdrop.
    ///
    /// `viewport` is the part of `source` that actually holds the picture — see
    /// the module note. Both passes run on this frame's encoder, before the egui
    /// pass records anything, so the texture egui samples is this frame's scene.
    #[allow(clippy::too_many_arguments)]
    fn render(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        source: &wgpu::TextureView,
        viewport: (u32, u32),
    ) {
        let scale = (
            (viewport.0 as f32 / self.source.0 as f32).clamp(0.0, 1.0),
            (viewport.1 as f32 / self.source.1 as f32).clamp(0.0, 1.0),
        );
        // The horizontal pass reads the *view target*, whose rendered region can
        // move under us (the phone tier's render scale is re-applied every frame),
        // so its uniform is written per frame and its bind group built per frame.
        // The step is one working texel wide, expressed in the source's UV space.
        queue.write_buffer(
            &self.params_h,
            0,
            &pack(scale.0, scale.1, scale.0 / self.work.0 as f32, 0.0),
        );
        let bind_h = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("glass_backdrop_bind_h"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(source),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: self.params_h.as_entire_binding(),
                },
            ],
        });

        for (label, target, bind) in [
            ("glass_backdrop_h", &self.horizontal, &bind_h),
            ("glass_backdrop_v", &self.blurred, &self.bind_v),
        ] {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some(label),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        // Every texel is written by the fullscreen triangle, so
                        // there is nothing to preserve and clearing is the cheaper
                        // load on a tiler.
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, bind, &[]);
            pass.draw(0..3, 0..1);
        }
    }
}

/// Four floats, little-endian, for a `vec4<f32>` uniform.
///
/// Written by hand rather than through `bytemuck` because four `f32`s do not
/// justify a dependency, and the layout of a `vec4<f32>` at offset zero is not
/// something that can drift.
fn pack(x: f32, y: f32, z: f32, w: f32) -> [u8; PARAMS_SIZE as usize] {
    let mut out = [0u8; PARAMS_SIZE as usize];
    for (i, v) in [x, y, z, w].into_iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
    }
    out
}

/// Make sure a backdrop exists for this view's size and fill it from `source`.
///
/// The single entry point from `egui_pass`. Rebuilds when the view resizes and
/// re-registers the texture with egui under the *same* id, so geometry already
/// tessellated against that id keeps working across a resize.
///
/// Answers `false` when there is no backdrop — the caller need not care, since
/// the shapes were already laid out for whichever answer the *previous* frame
/// gave, but it makes the state visible in a log or a test.
pub fn prepare(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    encoder: &mut wgpu::CommandEncoder,
    renderer: &mut egui_wgpu::Renderer,
    slot: &mut Option<Backdrop>,
    source: &wgpu::TextureView,
    size: (u32, u32),
    viewport: (u32, u32),
) -> bool {
    if disabled() {
        return false;
    }
    if size.0 == 0 || size.1 == 0 {
        return false;
    }
    let stale = slot.as_ref().is_none_or(|b| b.source != size);
    if stale {
        // Build first, and only replace on success: a failure has to leave the
        // previous backdrop working rather than turn the chrome flat.
        match Backdrop::new(device, queue, renderer, size) {
            Some(mut fresh) => {
                if let Some(old) = slot.as_ref() {
                    // Keep the id the geometry already refers to. egui's shapes
                    // are re-tessellated often but not necessarily this frame,
                    // and a resize that changed the id would leave last frame's
                    // panels sampling a texture nobody registered.
                    renderer.update_egui_texture_from_wgpu_texture(
                        device,
                        &fresh.blurred,
                        wgpu::FilterMode::Linear,
                        old.id,
                    );
                    fresh.id = old.id;
                }
                publish(fresh.id);
                *slot = Some(fresh);
            }
            None => return false,
        }
    }
    let Some(backdrop) = slot.as_ref() else {
        return false;
    };
    backdrop.render(device, queue, encoder, source, viewport);
    true
}

/// `CARTO_NO_GLASS_BLUR=1` — draw the chrome as flat translucent fills.
///
/// A bisecting switch in the manner of `CARTO_NO_ATMOSPHERE` and friends: it
/// removes one layer, and the layer it removes is the one most likely to be
/// implicated when a panel renders wrong. Read once.
#[must_use]
pub fn disabled() -> bool {
    use std::sync::OnceLock;
    static OFF: OnceLock<bool> = OnceLock::new();
    *OFF.get_or_init(|| std::env::var("CARTO_NO_GLASS_BLUR").is_ok_and(|v| v != "0"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Held by every test that sets or clears [`BACKDROP_ID`].
    ///
    /// The id is process-wide and libtest runs tests on parallel threads, so
    /// without this one test's `publish` lands between another's `publish` and
    /// its assertions — the backdrop then names the other test's texture, or
    /// none, if the other has already called `forget`. Both glass-rectangle
    /// tests failed that way together in a landing gate on 2026-10-17, after
    /// passing in the three runs before it.
    fn backdrop_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        // A test that panicked while holding it has still put the id back or
        // not; the next one sets its own either way, so poisoning is no reason
        // to fail it too.
        LOCK.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The fills that get a backdrop are exactly the two the chrome calls glass.
    ///
    /// Worth pinning because the test is a *colour comparison*: a palette edit
    /// that split `OVERLAY` into two tones, or gave a panel its own fill, would
    /// silently stop blurring behind it and there is nothing on screen that
    /// announces "this panel is no longer glass" except the panel looking a
    /// little wrong.
    #[test]
    fn only_the_glass_fills_get_a_backdrop() {
        assert!(is_glass(theme::GLASS));
        assert!(is_glass(theme::OVERLAY));
        for opaque in [
            theme::SURFACE,
            theme::SURFACE_RAISED,
            theme::LABEL_CHIP,
            theme::OVERLAY_SOFT,
            theme::ACCENT,
            egui::Color32::TRANSPARENT,
        ] {
            assert!(!is_glass(opaque), "{opaque:?} is not a glass fill");
        }
    }

    /// With no backdrop registered, the shape list comes back untouched — which
    /// is the state every launch starts in and the state a failed initialisation
    /// stays in.
    #[test]
    fn without_a_backdrop_the_shapes_are_left_alone() {
        let _held = backdrop_lock();
        forget();
        let mut shapes = vec![egui::epaint::ClippedShape {
            clip_rect: egui::Rect::EVERYTHING,
            shape: egui::Shape::Rect(egui::epaint::RectShape::filled(
                egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(100.0, 50.0)),
                egui::CornerRadius::same(8),
                theme::GLASS,
            )),
        }];
        insert_backdrops(&mut shapes, [800, 600], 1.0);
        assert_eq!(shapes.len(), 1);
        assert!(matches!(shapes[0].shape, egui::Shape::Rect(_)));
    }

    /// A glass rectangle becomes two shapes, the backdrop first, and the
    /// backdrop's texture coordinates address the rectangle's own place on
    /// screen.
    ///
    /// The UV is the whole trick and it is easy to get subtly wrong — a rect in
    /// *points* against a screen in *pixels* is off by the scale factor, which
    /// on a desktop at 1.0 looks perfect and on a phone at 2.75 samples a
    /// quarter of the screen. Hence a case with `pixels_per_point` deliberately
    /// not 1.
    #[test]
    fn a_glass_rectangle_gains_a_backdrop_addressing_its_own_position() {
        let _held = backdrop_lock();
        publish(egui::TextureId::User(7));
        let rect = egui::Rect::from_min_size(egui::pos2(100.0, 50.0), egui::vec2(100.0, 100.0));
        let mut shapes = vec![egui::epaint::ClippedShape {
            clip_rect: egui::Rect::EVERYTHING,
            shape: egui::Shape::Rect(egui::epaint::RectShape::filled(
                rect,
                egui::CornerRadius::same(8),
                theme::GLASS,
            )),
        }];
        // 800×400 points at 2× — the same screen a phone would lay out.
        insert_backdrops(&mut shapes, [1600, 800], 2.0);

        let egui::Shape::Vec(pair) = &shapes[0].shape else {
            panic!("the glass rect should have become a group of two");
        };
        assert_eq!(pair.len(), 2);
        let egui::Shape::Rect(backdrop) = &pair[0] else {
            panic!("the backdrop is drawn first, under the tint");
        };
        let egui::Shape::Rect(glass) = &pair[1] else {
            panic!("the glass rect itself is drawn second");
        };
        assert_eq!(glass.fill, theme::GLASS, "the tint is untouched");
        assert_eq!(
            backdrop.fill,
            egui::Color32::WHITE,
            "the backdrop is untinted"
        );
        assert_eq!(backdrop.corner_radius, glass.corner_radius);
        let brush = backdrop.brush.as_ref().expect("the backdrop is textured");
        assert_eq!(brush.fill_texture_id, egui::TextureId::User(7));
        // 100/800 → 0.125, 200/800 → 0.25 across; 50/400 → 0.125, 150/400 → 0.375 down.
        assert!((brush.uv.min.x - 0.125).abs() < 1e-6, "{:?}", brush.uv);
        assert!((brush.uv.max.x - 0.25).abs() < 1e-6, "{:?}", brush.uv);
        assert!((brush.uv.min.y - 0.125).abs() < 1e-6, "{:?}", brush.uv);
        assert!((brush.uv.max.y - 0.375).abs() < 1e-6, "{:?}", brush.uv);
        forget();
    }

    /// A glass rectangle nested inside a group is still a glass rectangle.
    #[test]
    fn a_nested_glass_rectangle_is_found() {
        let _held = backdrop_lock();
        publish(egui::TextureId::User(3));
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(10.0, 10.0));
        let mut shapes = vec![egui::epaint::ClippedShape {
            clip_rect: egui::Rect::EVERYTHING,
            shape: egui::Shape::Vec(vec![
                egui::Shape::Noop,
                egui::Shape::Rect(egui::epaint::RectShape::filled(
                    rect,
                    egui::CornerRadius::ZERO,
                    theme::OVERLAY,
                )),
            ]),
        }];
        insert_backdrops(&mut shapes, [100, 100], 1.0);
        let egui::Shape::Vec(outer) = &shapes[0].shape else {
            panic!("group");
        };
        let egui::Shape::Vec(pair) = &outer[1] else {
            panic!("the nested rect should have become a pair");
        };
        assert_eq!(pair.len(), 2);
        forget();
    }
}

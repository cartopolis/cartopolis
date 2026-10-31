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
    /// The sheen texture ([`sheen_image`]), once egui has one.
    sheen: Option<egui::TextureId>,
}

/// Put a blurred copy of the frame under every glass rectangle in `shapes`, and
/// say whether there was one.
///
/// A no-op when the render world has not registered a backdrop yet, when the
/// screen has no area, or when the shape list holds no glass — so the cost on a
/// frame with nothing translucent on it is one walk of the list.
///
/// The answer travels with the geometry (`EguiRenderOutput::glass`) so that
/// [`prepare`] blurs the frame only while something on screen samples the blur.
/// Without it the two quarter-size passes ran every frame, behind an opaque
/// full-screen panel and behind an empty map alike.
pub fn insert_backdrops(
    shapes: &mut Vec<egui::epaint::ClippedShape>,
    size_in_pixels: [u32; 2],
    pixels_per_point: f32,
    sheen: Option<egui::TextureId>,
) -> bool {
    let Some(id) = backdrop_id() else {
        return false;
    };
    if pixels_per_point <= 0.0 || size_in_pixels[0] == 0 || size_in_pixels[1] == 0 {
        return false;
    }
    let screen = Screen {
        size: egui::vec2(
            size_in_pixels[0] as f32 / pixels_per_point,
            size_in_pixels[1] as f32 / pixels_per_point,
        ),
        sheen,
    };
    let mut found = false;
    for clipped in shapes.iter_mut() {
        found |= expand(std::slice::from_mut(&mut clipped.shape), id, &screen);
    }
    // The walk above can only *replace* a shape in place, because a
    // `ClippedShape` holds exactly one. A glass rectangle at the top level is
    // therefore rewritten into a `Shape::Vec` group — see `expand`.
    found
}

/// Walk a shape list, replacing each glass rectangle with a group: the blurred
/// backdrop, the pane over it — a thin rim of glass round a dark core — and
/// its light.
///
/// Recursive because egui groups shapes into `Shape::Vec` in several places
/// (a `Frame`'s background and its stroke arrive as one, for instance), and a
/// panel background nested one level down is still a panel background.
fn expand(shapes: &mut [egui::Shape], id: egui::TextureId, screen: &Screen) -> bool {
    let mut found = false;
    for shape in shapes.iter_mut() {
        match shape {
            egui::Shape::Rect(rect) if is_glass(rect.fill) => {
                let Some(backdrop) = backdrop_shape(rect, id, screen) else {
                    continue;
                };
                let sheen = sheen_shapes(rect, screen.sheen);
                let egui::Shape::Rect(mut glass) = std::mem::replace(shape, egui::Shape::Noop)
                else {
                    unreachable!("matched as a rect above");
                };
                let mut group = vec![backdrop];
                match core_shape(&glass) {
                    // The pane keeps its stroke and takes the rim's thin tint;
                    // the core, inset inside it, carries the darkness. Without a
                    // backdrop none of this runs and the pane keeps `GLASS`.
                    Some(core) => {
                        glass.fill = theme::GLASS_RIM;
                        group.push(egui::Shape::Rect(glass));
                        group.push(core);
                    }
                    None => group.push(egui::Shape::Rect(glass)),
                }
                group.extend(sheen);
                *shape = egui::Shape::Vec(group);
                found = true;
            }
            egui::Shape::Vec(inner) => found |= expand(inner, id, screen),
            _ => {}
        }
    }
    found
}

/// How wide the clear rim of a glass pane is, in points.
///
/// **This is where the glass is.** The pane has to be dark wherever type can
/// sit — over white paper, the brightest thing behind it — and at that
/// darkness a blurred city shows as a sixth of itself, which the person
/// looking at it called invisible (2026-10-31). Making the whole pane clearer
/// costs that legibility; dimming the backdrop instead only darkened the slab.
/// So the clear part is a rim no type reaches — every glass frame's inner
/// margin is wider than this — and through it the blurred city shows at five
/// sixths of its light: the edge of a thick sheet, which is what makes glass
/// read as glass.
const RIM_WIDTH: f32 = 6.0;

/// The core's own hairline, white at this alpha.
const CORE_EDGE_ALPHA: u8 = 22;

/// The dark core of a glass pane: the pane inset by [`RIM_WIDTH`], corners
/// shrunk to match, in `theme::GLASS_CORE`. `None` for a pane too small to
/// have a rim and a core — a chip, a one-line readout — which keeps the plain
/// `GLASS` fill.
fn core_shape(rect: &egui::epaint::RectShape) -> Option<egui::Shape> {
    let r = rect.rect;
    if r.height() < SHEEN_MIN_HEIGHT || r.width() < 4.0 * RIM_WIDTH {
        return None;
    }
    let radius = f32::from(rect.corner_radius.nw.max(rect.corner_radius.ne))
        .min(r.height() * 0.5);
    let inner = (radius - RIM_WIDTH).max(2.0).round() as u8;
    let mut core = egui::epaint::RectShape::filled(
        r.shrink(RIM_WIDTH),
        egui::CornerRadius::same(inner),
        theme::GLASS_CORE,
    );
    core.round_to_pixels = rect.round_to_pixels;
    // The inner edge of the sheet, faint: it is what turns the rim from a
    // coloured border into the thickness of the glass.
    core.stroke = egui::Stroke::new(1.0, egui::Color32::from_white_alpha(CORE_EDGE_ALPHA));
    core.stroke_kind = egui::StrokeKind::Inside;
    Some(egui::Shape::Rect(core))
}

/// The blurred twin of one glass rectangle: same rectangle, same corners, the
/// backdrop texture passed through untinted, no stroke.
///
/// The tint is the pane drawn *over* this one. A brush multiplies the texture
/// by the shape's fill, so tinting here with the panel colour would turn the
/// city teal-black under the rim as well as the core.
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

/// How bright the lit top edge of a glass surface is: white at this alpha.
///
/// Brighter than the secondary key's 40 and under the primary's 120: on a
/// pane this is the one hard line that says *edge of a sheet*, where the sheen
/// under it is the soft half.
const RIM_ALPHA: u8 = 96;

/// A pane shorter than this gets no light: a chip or a one-line readout with
/// a highlight across its top reads as underlined from above.
const SHEEN_MIN_HEIGHT: f32 = 28.0;

/// How far down a pane the sheen reaches, as a fraction of its height.
const SHEEN_REACH: f32 = 0.4;

/// Rows in the sheen texture. It is stretched over each pane by its UVs, so
/// this only sets how smooth the fade is.
const SHEEN_ROWS: usize = 64;

/// The sheen: white, at `theme::GLASS_SHEEN_PEAK` along the top row and
/// fading quadratically to nothing at [`SHEEN_REACH`]. One column — it is
/// stretched across a pane's width.
#[must_use]
pub fn sheen_image() -> egui::ColorImage {
    let reach = (SHEEN_ROWS as f32 * SHEEN_REACH).max(1.0);
    let pixels = (0..SHEEN_ROWS)
        .map(|row| {
            let t = (row as f32 / reach).min(1.0);
            let a = f32::from(theme::GLASS_SHEEN_PEAK) * (1.0 - t) * (1.0 - t);
            egui::Color32::from_white_alpha(a.round() as u8)
        })
        .collect();
    egui::ColorImage::new([1, SHEEN_ROWS], pixels)
}

/// The light on one glass rectangle, drawn over it: a soft sheen down from the
/// top and a hard lit rim along it.
///
/// The blur alone made the chrome see-through but left it reading as a dark
/// plate laid on the map (2026-10-31, the place card and the reminder over
/// Groningen): nothing said *material*. What does is light caught on the upper
/// edge — the cue the primary and secondary keys already carry, so the panes
/// and the keys read as one family. The sheen is a textured rectangle with the
/// pane's own corners, so egui rounds it exactly as it rounds the pane; the rim
/// is inset by the corner radius so it runs only along the straight part.
fn sheen_shapes(
    rect: &egui::epaint::RectShape,
    sheen: Option<egui::TextureId>,
) -> Vec<egui::Shape> {
    let r = rect.rect;
    if r.height() < SHEEN_MIN_HEIGHT {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(2);
    if let Some(id) = sheen {
        let mut soft = egui::epaint::RectShape::filled(r, rect.corner_radius, egui::Color32::WHITE);
        soft.round_to_pixels = rect.round_to_pixels;
        out.push(egui::Shape::Rect(soft.with_texture(
            id,
            egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
        )));
    }
    // A pill's radius is "as round as it gets" (`RADIUS_PILL`), which egui clamps
    // to half the height; the rim has to clamp the same way or a pill gets none.
    let inset = f32::from(rect.corner_radius.nw.max(rect.corner_radius.ne))
        .min(r.height() * 0.5)
        .max(4.0);
    if r.width() > 2.0 * inset {
        out.push(egui::Shape::line_segment(
            [
                egui::pos2(r.left() + inset, r.top() + 1.0),
                egui::pos2(r.right() - inset, r.top() + 1.0),
            ],
            egui::Stroke::new(1.0, egui::Color32::from_white_alpha(RIM_ALPHA)),
        ));
    }
    out
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
/// `sampled` is whether the geometry about to be drawn holds a glass rectangle
/// ([`insert_backdrops`]'s answer). Without one the blur is built and kept —
/// the id has to be published before any shape can name it — but not filled,
/// since nothing would read it.
///
/// Answers `false` when there is no backdrop — the caller need not care, since
/// the shapes were already laid out for whichever answer the *previous* frame
/// gave, but it makes the state visible in a log or a test.
#[allow(clippy::too_many_arguments)]
pub fn prepare(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    encoder: &mut wgpu::CommandEncoder,
    renderer: &mut egui_wgpu::Renderer,
    slot: &mut Option<Backdrop>,
    source: &wgpu::TextureView,
    size: (u32, u32),
    viewport: (u32, u32),
    sampled: bool,
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
    if sampled {
        backdrop.render(device, queue, encoder, source, viewport);
    }
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
        insert_backdrops(&mut shapes, [800, 600], 1.0, None);
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
        assert!(
            insert_backdrops(&mut shapes, [1600, 800], 2.0, Some(egui::TextureId::User(9))),
            "glass was found"
        );

        let egui::Shape::Vec(group) = &shapes[0].shape else {
            panic!("the glass rect should have become a group");
        };
        assert_eq!(group.len(), 5, "backdrop, rim tint, core, sheen, lit edge");
        assert!(
            matches!(group[4], egui::Shape::LineSegment { .. }),
            "the lit edge is drawn last"
        );
        let egui::Shape::Rect(sheen) = &group[3] else {
            panic!("the sheen is a rect over the pane");
        };
        assert_eq!(
            sheen.brush.as_ref().map(|b| b.fill_texture_id),
            Some(egui::TextureId::User(9))
        );
        assert_eq!(sheen.corner_radius, egui::CornerRadius::same(8));
        let egui::Shape::Rect(core) = &group[2] else {
            panic!("the core is a rect inside the pane");
        };
        assert_eq!(core.fill, theme::GLASS_CORE);
        assert_eq!(core.rect, rect.shrink(RIM_WIDTH), "the core leaves the rim clear");
        let egui::Shape::Rect(backdrop) = &group[0] else {
            panic!("the backdrop is drawn first, under the tint");
        };
        let egui::Shape::Rect(glass) = &group[1] else {
            panic!("the glass rect itself is drawn second");
        };
        assert_eq!(glass.fill, theme::GLASS_RIM, "the pane itself is the clear rim");
        assert_eq!(backdrop.fill, egui::Color32::WHITE, "the backdrop is untinted");
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
        insert_backdrops(&mut shapes, [100, 100], 1.0, None);
        let egui::Shape::Vec(outer) = &shapes[0].shape else {
            panic!("group");
        };
        let egui::Shape::Vec(pair) = &outer[1] else {
            panic!("the nested rect should have become a pair");
        };
        // 10 pt tall: too small for a lit edge, so backdrop and tint only.
        assert_eq!(pair.len(), 2);
        forget();
    }

    /// No glass, no blur: the answer `egui_pass` skips the two passes on.
    #[test]
    fn a_frame_without_glass_says_so() {
        let _held = backdrop_lock();
        publish(egui::TextureId::User(5));
        let mut shapes = vec![egui::epaint::ClippedShape {
            clip_rect: egui::Rect::EVERYTHING,
            shape: egui::Shape::Rect(egui::epaint::RectShape::filled(
                egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(300.0, 600.0)),
                egui::CornerRadius::ZERO,
                theme::PANEL,
            )),
        }];
        assert!(!insert_backdrops(&mut shapes, [300, 600], 1.0, None));
        forget();
    }
}

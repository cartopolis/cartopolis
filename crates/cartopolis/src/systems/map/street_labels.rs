//! Street names, painted **on** the streets they name (Map ▸ Street labels).
//!
//! The names come from Shortbread's `street_labels` layer, parsed off the same
//! tile body the surfaces are tessellated from (`vector_tiles::extract_street_labels`)
//! and handed over per cell by `map_geometry`, in the same lifetime as that cell's
//! meshes. What this module does with them is lay each one flat on its street as a
//! textured quad in the world — not as screen text over the map.
//!
//! **Why it is world geometry.** A screen-space label is projected onto a street
//! but does not belong to it: it keeps its size while the city changes scale under
//! it, it draws over the buildings in front of it, and the eye reads it as a note
//! stuck to the glass rather than as a name written on the road. On the ground it
//! is a road marking — it takes the perspective of the street it lies in, a
//! building in front of it hides it, and it stays exactly where it was put.
//!
//! Three consequences, and they are the whole design:
//!
//! * **Placement is view-independent**, so it cannot flicker. A name goes where
//!   its street's own geometry puts it (arc midpoint, chord direction), it is
//!   drawn only where the street is long enough in *metres* to carry it, and the
//!   declutter pass compares world footprints. None of that changes as the camera
//!   moves, so nothing pops in and out while you fly. The work only re-runs when
//!   the loaded cells change, when the size band changes, or when the camera turns
//!   far enough that a label would read backwards.
//! * **Size is a band of altitude, not a continuous function of it** ([`HEIGHT_BANDS`]).
//!   A true fixed size is unreadable from 600 m and a billboard from 40 m; a
//!   continuous ramp would re-place every label on every frame of a climb. Four
//!   steps keep the letters in a readable range while leaving them still.
//! * **Text is a texture**, CPU-rasterised with `fontdue` (the same rasteriser the
//!   departure boards use) in the brand semibold the app already embeds. Ink with
//!   a pale halo baked in, because a street name crosses tarmac, grass, water and
//!   roof in one word and no single flat colour survives all four. Weight and
//!   mip chain are both load-bearing: a ground label is read at a dozen pixels
//!   through a foreshortening projection, and a light face without mips arrives
//!   as grey mush.
//!
//! One thing does follow the camera, and has to: **which** names are written.
//! Selection is a disc around the ground point the camera is aimed at
//! ([`SELECT_RADIUS_M`]), refreshed once the view has moved [`MOVE_REPLACE_M`].
//! Placement inside that disc is still the street's business — a name that stays
//! in view does not shift when the disc moves — but a pool spread evenly over the
//! whole loaded ring puts one or two names in a city view and the rest behind you.
//!
//! The quads live in the coplanar ground stack, one rung above the route line
//! (`vector_tiles::DEPTH_BIAS_STEP`) — you want to read the street you are being
//! routed along — and below the driver arrow.

use crate::components::FreeCamera;
use crate::components::StreetLabelSign;
use crate::grid::{WORLD_CELL_EDGE, WORLD_SWITCH_THRESHOLD};
use bevy::asset::RenderAssetUsages;
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use big_space::prelude::{BigSpace, Grid, GridCell};
use std::collections::HashMap;

use crate::systems::data_layers::DataLayers;
use crate::systems::globe::GlobeState;
use crate::systems::theme;
use crate::systems::vector_tiles::{StreetLabelLine, DEPTH_BIAS_STEP};

/// Texture height in world metres, per altitude ceiling. Stepped rather than
/// continuous: the size has to stay in a readable range across a climb, but a
/// label that resized every frame would also be re-placed every frame, and the
/// point of putting the names in the world is that they hold still.
///
/// The numbers are larger than the on-screen size suggests, because text lying on
/// the ground is **foreshortened**: at the 30° pitch a city view is usually flown
/// at, a label loses half its height to the projection. Sized for a plan view it
/// reads as a squashed smear from anywhere else.
/// The lowest band exists for the walking/driving range: at 45 m of altitude a
/// 7 m glyph fills a quarter of the frame and reads as a nameplate stuck to
/// the road, not as cartography.
const HEIGHT_BANDS: [(f32, f32); 5] = [
    (60.0, 3.5),
    (150.0, 7.0),
    (400.0, 11.0),
    (800.0, 18.0),
    (f32::MAX, 28.0),
];

/// Ground radius around the view centre that labels are chosen for.
///
/// Selection is world-space, so without this the pool spreads itself over the
/// whole loaded ring — 4 km across — and a city view holds one or two names.
const SELECT_RADIUS_M: f32 = 900.0;

/// How far the camera may travel before the selection is redrawn. Labels do not
/// move when this fires — placement is a fact about the street — but the ring of
/// names being shown catches up with where the camera now is.
const MOVE_REPLACE_M: f32 = 120.0;

/// How far ahead the ground point the camera is aimed at may be taken. A radius
/// around the camera alone empties the top of a pitched frame: everything it
/// points at is further away than everything under it.
const MAX_FOCUS_M: f32 = 1200.0;

/// Names kept rasterised. Trimmed to the live selection when it is passed, so
/// crossing a city does not accumulate every street it has ever shown.
const CACHE_MAX: usize = 400;

/// Above this altitude the layer switches off: the streets are a hairline each
/// and their names stop being a map and become a block of text laid over one.
const MAX_LABEL_ALT: f32 = 1200.0;

/// Size of the entity pool — the most names on the ground at once. Larger than a
/// screen-space layer would need: these cover a disc around the view rather than
/// the viewport, so the ones behind the camera are paid for too.
const MAX_LABELS: usize = 160;

/// A name may take at most this fraction of its street's length. Under 1.0 so a
/// label never runs past the junction at either end.
const FIT_MARGIN: f32 = 0.85;

/// Minimum ground distance between two labels carrying the same name. One street
/// is many features (Shortbread merges by attribute, the tiles clip it per cell),
/// so without this a long avenue gets its name stamped along itself six times.
const SAME_NAME_GAP_M: f32 = 260.0;

/// Height above the ground **under the label**, in metres. Just over the route
/// line's 0.5 — physical separation as well as the depth bias, exactly as the route
/// does it. Added to `TerrainField::at`, which is `0.0` with the Height layer off,
/// so with no relief this is the flat `y` it always was.
const LABEL_Y: f32 = 0.55;

/// Rung on the shared depth-bias ladder: above the route ribbon (20) and its
/// casing, below the driver arrow (24).
pub(crate) const LABEL_RUNG: f32 = 23.0;

/// Textures built per frame. Mass texture uploads are the documented way to lose
/// the device on a weak iGPU (see CLAUDE.md), and a fresh ring of cells wants a
/// few dozen names at once.
const MAX_TEXTURE_BUILDS_PER_FRAME: usize = 4;

/// Camera turn that forces a re-place, in radians. Ground text has a front and a
/// back; past this the labels are re-laid so they read left-to-right again.
const YAW_REPLACE: f32 = 0.35;

/// Glyph size for the raster, in texture pixels. The quad's world height is what
/// sets the on-screen size; this only decides how much detail the texture holds.
const GLYPH_PX: f32 = 40.0;

/// Halo radius in texture pixels. Wide relative to the stroke — a label on the
/// ground is read through a foreshortening projection at a dozen pixels, and a
/// hairline outline disappears in the first mip level — but not the 10 % of
/// glyph height it used to be, which up close read as text printed on a white
/// plate rather than a halo.
const HALO_PX: i32 = 3;

/// Ink and halo, off the shared palette ([`theme::LABEL_INK`] over
/// [`theme::LABEL_HALO`]) — near-black on a pale neutral, the map labels' own
/// pair rather than the chrome's. It used to be tinted slate-teal so the names
/// would belong to the city around them; the chrome carries no tint any more,
/// and a neutral halo is the better answer on a map that supplies all the hue
/// in the frame itself. Contrast is pinned in `theme`'s tests against both a
/// white and a night basemap.
const INK: [f32; 3] = srgb_of(theme::LABEL_INK);
const HALO: [f32; 3] = srgb_of(theme::LABEL_HALO);
/// The halo's own opacity, and deliberately not [`theme::LABEL_HALO`]'s alpha:
/// that one is set for text over a dimmed overlay, this one has bare tarmac
/// behind it. Just under 1.0, so it still reads as an outline around letters
/// rather than as a plate laid over the road.
const HALO_ALPHA: f32 = 0.95;

/// A palette colour as sRGB fractions for the CPU rasteriser — which writes
/// straight into an `Rgba8UnormSrgb` texture, so the bytes are the same encoding
/// egui hands out. Two things have to be undone: `Color32` stores its channels
/// *premultiplied*, and the alpha itself is dropped here — coverage supplies it
/// per pixel, scaled by [`HALO_ALPHA`]. Take the raw channels and a translucent
/// palette entry arrives pre-dimmed and composites twice.
const fn srgb_of(c: egui::Color32) -> [f32; 3] {
    let a = c.a() as f32 / 255.0;
    if a <= 0.0 {
        return [0.0; 3];
    }
    [
        c.r() as f32 / 255.0 / a,
        c.g() as f32 / 255.0 / a,
        c.b() as f32 / 255.0 / a,
    ]
}

/// The pool, as the placer writes it: where the quad goes, whether it is shown,
/// and the material whose texture carries the name.
type SignQuery<'w, 's> = Query<
    'w,
    's,
    (
        &'static mut Transform,
        &'static mut GridCell,
        &'static mut Visibility,
        &'static MeshMaterial3d<StandardMaterial>,
    ),
    (With<StreetLabelSign>, Without<FreeCamera>),
>;

/// A street's label line in the anchored world frame (metres, XZ).
pub struct WorldLabelLine {
    pub name: String,
    pub rank: u8,
    /// Polyline on the ground plane, world XZ metres — the frame the map cells
    /// are laid in.
    pub points: Vec<Vec2>,
    /// Ground length, precomputed: the fit test runs over every line in the ring
    /// on every re-place.
    length: f32,
    /// Where this line's ground actually is, in the anchored frame, when the
    /// contributor has measured it more finely than [`TerrainField`] can.
    ///
    /// `None` for every street, which is the ordinary case and keeps the coarse
    /// field's answer. `systems::dikes` is the one contributor that fills it: a
    /// 25 m field flattens a sea dike's crest by about two metres (measured on
    /// the Lauwersmeerdijk, 2026-08-29), and a name laid on that answer is buried
    /// inside the embankment it names.
    pub ground_m: Option<f32>,
}

impl WorldLabelLine {
    /// Build a line that is already in the anchored world frame — how a
    /// contributor that is not a map cell (`systems::dikes`) hands one over.
    #[must_use]
    pub fn new(name: String, rank: u8, points: Vec<Vec2>, ground_m: Option<f32>) -> Self {
        Self {
            name,
            rank,
            length: polyline_len(&points),
            points,
            ground_m,
        }
    }
}

/// Street label lines of the live map cells, held for exactly as long as their
/// surface meshes: `map_geometry` inserts a cell's lines when it uploads that
/// cell and drops them when it retires it. So the store never outlives the anchor
/// generation its coordinates belong to.
#[derive(Resource, Default)]
pub struct StreetLabelStore {
    cells: HashMap<(i64, i64), Vec<WorldLabelLine>>,
    /// The flood defences (`systems::dikes`), held apart from the streets.
    ///
    /// A separate list rather than more entries in `cells`, because the two are
    /// held for different lifetimes by different owners: a street's lines live
    /// exactly as long as the surface mesh they were parsed out of, while a
    /// defence's live as long as its own stream's cells and are replaced whole
    /// when a crest measurement lands. Sharing the cell keys would have one owner
    /// silently evicting the other's lines.
    dikes: Vec<WorldLabelLine>,
    /// Bumped on every change. The placer re-runs on a change of this, of the
    /// size band, or of the camera's heading — and on nothing else.
    revision: u32,
}

impl StreetLabelStore {
    /// Take a finished cell's tile-local lines and place them at the cell's world
    /// origin (`wx`, `wz` — the same north-west corner its meshes are placed at).
    pub fn insert_cell(&mut self, cell: (i64, i64), wx: f32, wz: f32, lines: Vec<StreetLabelLine>) {
        let placed: Vec<WorldLabelLine> = lines
            .into_iter()
            .filter(|l| l.points.len() >= 2)
            .map(|l| {
                let points: Vec<Vec2> = l
                    .points
                    .iter()
                    .map(|p| Vec2::new(wx + p[0], wz + p[1]))
                    .collect();
                WorldLabelLine {
                    name: l.name,
                    rank: l.rank,
                    length: polyline_len(&points),
                    points,
                    ground_m: None,
                }
            })
            .collect();
        self.cells.insert(cell, placed);
        self.revision = self.revision.wrapping_add(1);
    }

    pub fn remove_cell(&mut self, cell: &(i64, i64)) {
        if self.cells.remove(cell).is_some() {
            self.revision = self.revision.wrapping_add(1);
        }
    }

    /// Replace the flood-defence lines whole.
    ///
    /// Wholesale rather than per cell because that is how they change: a crest
    /// measurement landing rewrites one line's `ground_m`, and the contributor
    /// rebuilds its list from the stream it owns rather than tracking which cell
    /// each piece came from.
    pub fn set_dikes(&mut self, lines: Vec<WorldLabelLine>) {
        if self.dikes.is_empty() && lines.is_empty() {
            return;
        }
        self.dikes = lines;
        self.revision = self.revision.wrapping_add(1);
    }

    pub fn clear(&mut self) {
        if !self.cells.is_empty() || !self.dikes.is_empty() {
            self.cells.clear();
            self.dikes.clear();
            self.revision = self.revision.wrapping_add(1);
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.cells.is_empty() && self.dikes.is_empty()
    }

    /// Every line that can carry a name, streets and defences alike.
    fn lines(&self) -> impl Iterator<Item = &WorldLabelLine> {
        self.cells.values().flatten().chain(self.dikes.iter())
    }
}

/// The drawing side: the glyph rasteriser, the shared quad, the name→texture
/// cache and the entity pool.
///
/// The cache is keyed by name alone, not by name and size: the texture is
/// magnified onto a quad whose *world* size carries the altitude band, so one
/// raster serves every band.
#[derive(Resource, Default)]
pub struct StreetLabelSigns {
    font: Option<fontdue::Font>,
    quad: Option<Handle<Mesh>>,
    signs: HashMap<String, Handle<Image>>,
    pool: Vec<Entity>,
    /// State the pool currently reflects, so a frame that changes nothing does
    /// nothing at all.
    placed: Option<PlacedState>,
}

#[derive(PartialEq)]
struct PlacedState {
    revision: u32,
    /// `TerrainField::revision` the labels were lifted onto. The ground arrives a
    /// second or two after the anchor it belongs to and moves again whenever the
    /// layer is switched, so a label placed before it landed is sitting on the old
    /// flat plane — the same contract the surfaces, both building streamers and the
    /// route line carry (see CLAUDE.md).
    terrain_revision: u32,
    height_m: f32,
    /// Camera heading the labels were oriented for.
    yaw: f32,
    /// Ground point the selection disc was centred on.
    at: Vec2,
    /// True while some chosen name is still waiting for its texture, which is
    /// what makes the next frame try again under the per-frame build cap.
    complete: bool,
}

/// One name, placed on its street.
struct Placement<'a> {
    name: &'a str,
    rank: u8,
    /// Ground length of the street run it sits on — the tiebreak, and the reason
    /// a through road outranks the stub that shares its name.
    run_m: f32,
    centre: Vec2,
    /// Baseline direction in world XZ.
    dir: Vec2,
    width_m: f32,
    /// A ground height measured more finely than the terrain field, when the
    /// contributor has one — see [`WorldLabelLine::ground_m`].
    ground_m: Option<f32>,
}

/// Lay the street names on the ground.
///
/// Does nothing on a frame where nothing it depends on has changed — which is
/// almost every frame, because the labels are fixed to the world and the camera
/// moving is not a reason to touch them.
#[allow(clippy::too_many_arguments)]
pub fn update_street_labels(
    mut commands: Commands,
    mut signs: ResMut<StreetLabelSigns>,
    store: Res<StreetLabelStore>,
    layers: Res<DataLayers>,
    globe: Res<GlobeState>,
    camera: Query<(&GridCell, &Transform), With<FreeCamera>>,
    bigspace: Query<Entity, With<BigSpace>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    terrain: Res<crate::systems::terrain::TerrainField>,
    mut placed_query: SignQuery,
) {
    let Ok((cam_cell, cam_tf)) = camera.single() else {
        return;
    };
    let alt = globe.altitude_m(cam_cell, cam_tf) as f32;
    let on =
        layers.map_enabled && layers.street_labels && !store.is_empty() && alt <= MAX_LABEL_ALT;
    if !on {
        if signs.placed.take().is_some() {
            for (_, _, mut vis, _) in placed_query.iter_mut() {
                *vis = Visibility::Hidden;
            }
        }
        return;
    }

    let height_m = label_height(alt);
    // The camera's screen-right on the ground: which way round a name has to be
    // written to read left-to-right from where the camera is now.
    let right = cam_tf.right();
    let cam_right = Vec2::new(right.x, right.z).normalize_or_zero();
    let cam_yaw = cam_right.y.atan2(cam_right.x);
    let edge = f64::from(WORLD_CELL_EDGE);
    let cam_ground = Vec2::new(
        (cam_cell.x as f64 * edge + f64::from(cam_tf.translation.x)) as f32,
        (cam_cell.z as f64 * edge + f64::from(cam_tf.translation.z)) as f32,
    );
    let view_at = ground_focus(cam_ground, alt, cam_tf.forward().as_vec3());
    let want = PlacedState {
        revision: store.revision,
        terrain_revision: terrain.revision,
        height_m,
        yaw: cam_yaw,
        at: view_at,
        complete: true,
    };
    if let Some(p) = &signs.placed {
        let turned = angle_delta(p.yaw, cam_yaw).abs() > YAW_REPLACE;
        let moved = p.at.distance(view_at) > MOVE_REPLACE_M;
        if p.revision == want.revision
            && p.terrain_revision == want.terrain_revision
            && p.height_m == want.height_m
            && !turned
            && !moved
            && p.complete
        {
            return;
        }
    }

    let Ok(root) = bigspace.single() else {
        return;
    };
    if signs.font.is_none() {
        signs.font = load_font();
    }
    let Some(font) = signs.font.clone() else {
        return; // no rasteriser, no labels — nothing to draw and nothing to retry
    };

    // Choose what to write, in world space. The camera decides only *which* part
    // of the map is written and which way round the letters run — where a name
    // sits on its street is the street's business, and does not move.
    let mut chosen = choose_labels(&store, &font, height_m, cam_right, view_at);

    // Crossing a city would otherwise accumulate every name it has ever shown.
    // Trimming to the live selection is exact and only costs a re-raster of
    // whatever comes back into view.
    if signs.signs.len() > CACHE_MAX {
        let live: std::collections::HashSet<&str> = chosen.iter().map(|p| p.name).collect();
        signs.signs.retain(|name, _| live.contains(name.as_str()));
    }

    // Textures, under the per-frame cap. A name without one yet is dropped from
    // this pass and picked up on the next, which is what `complete` tracks.
    let mut complete = true;
    let mut built = 0usize;
    chosen.retain(|p| {
        if signs.signs.contains_key(p.name) {
            return true;
        }
        if built >= MAX_TEXTURE_BUILDS_PER_FRAME {
            complete = false;
            return false;
        }
        built += 1;
        let (pixels, w, h) = render_name(&font, p.name);
        signs
            .signs
            .insert(p.name.to_string(), images.add(label_image(pixels, w, h)));
        true
    });

    // The pool: one quad each, spawned once and re-aimed from then on.
    if signs.quad.is_none() {
        signs.quad = Some(meshes.add(label_quad()));
    }
    let quad = signs.quad.clone().expect("just created");
    while signs.pool.len() < MAX_LABELS {
        let material = materials.add(StandardMaterial {
            base_color: Color::WHITE,
            // A map annotation, not a surface: it must read at midnight as well
            // as at noon, so the sun does not get a vote.
            unlit: true,
            alpha_mode: AlphaMode::Blend,
            depth_bias: LABEL_RUNG * DEPTH_BIAS_STEP,
            // Winding is not allowed to be the thing that hides it (the route
            // ribbon learned this the expensive way).
            cull_mode: None,
            ..default()
        });
        let entity = commands
            .spawn((
                Mesh3d(quad.clone()),
                MeshMaterial3d(material),
                Transform::default(),
                Visibility::Hidden,
                GridCell::default(),
                ChildOf(root),
                StreetLabelSign,
            ))
            .id();
        signs.pool.push(entity);
    }

    let grid = Grid::new(WORLD_CELL_EDGE, WORLD_SWITCH_THRESHOLD);
    let pool = signs.pool.clone();
    for (slot, entity) in pool.iter().enumerate() {
        let Ok((mut tf, mut cell, mut vis, material)) = placed_query.get_mut(*entity) else {
            continue;
        };
        let Some(p) = chosen.get(slot) else {
            *vis = Visibility::Hidden;
            continue;
        };
        let Some(texture) = signs.signs.get(p.name) else {
            *vis = Visibility::Hidden;
            continue;
        };
        if let Some(mut m) = materials.get_mut(&material.0) {
            m.base_color_texture = Some(texture.clone());
        }
        // Stand on the measured ground, which the road under the label is standing
        // on too. One height for the whole quad — a label is a flat plate, and the
        // rule the water, the bridge decks and the buildings follow — but taken as
        // the *highest* of the two ends and the centre rather than the centre
        // alone: on a slope the ends are what a centre sample would bury, and
        // [`LABEL_Y`] is only half a metre of clearance to lose.
        let half = p.dir * p.width_m * 0.5;
        let ends = [p.centre - half, p.centre, p.centre + half];
        // A contributor that measured its own ground wins outright: the field is
        // 25 m per sample and a flood defence's crest is narrower than that, so
        // sampling it here would bury the name inside the embankment.
        let ground = p.ground_m.unwrap_or_else(|| {
            ends.iter()
                .map(|s| terrain.at(s.x, s.y))
                .fold(f32::MIN, f32::max)
        });
        let world = Vec3::new(p.centre.x, ground + LABEL_Y, p.centre.y);
        let (grid_cell, local) = grid.translation_to_grid(world.as_dvec3());
        *cell = grid_cell;
        // `dir` is where the baseline runs in world XZ; a yaw rotation sends the
        // quad's +X there. The sign on the Z term is the usual one: a yaw turns
        // +X towards −Z.
        tf.translation = local;
        tf.rotation = Quat::from_rotation_y((-p.dir.y).atan2(p.dir.x));
        tf.scale = Vec3::new(p.width_m, 1.0, height_m);
        *vis = Visibility::Visible;
    }

    tracing::debug!(
        alt,
        height_m,
        chosen = chosen.len(),
        cached = signs.signs.len(),
        complete,
        "street labels placed"
    );
    signs.placed = Some(PlacedState { complete, ..want });
}

/// Texture height in metres for an altitude — see [`HEIGHT_BANDS`].
fn label_height(alt: f32) -> f32 {
    HEIGHT_BANDS
        .iter()
        .find(|(ceiling, _)| alt < *ceiling)
        .map_or(HEIGHT_BANDS[HEIGHT_BANDS.len() - 1].1, |(_, h)| *h)
}

/// Where the camera is looking, on the ground plane — the centre of the disc the
/// names are chosen for. Falls back to the camera's own ground position when the
/// view is level or upward, where the ray meets the ground at infinity or not at
/// all, and is clamped so a near-level view doesn't aim past the loaded cells.
fn ground_focus(cam_ground: Vec2, altitude: f32, forward: Vec3) -> Vec2 {
    if forward.y >= -0.05 || altitude <= 0.0 {
        return cam_ground;
    }
    let t = (altitude / -forward.y).min(MAX_FOCUS_M);
    cam_ground + Vec2::new(forward.x, forward.z) * t
}

/// Signed difference between two angles, in (−π, π].
fn angle_delta(a: f32, b: f32) -> f32 {
    let mut d = b - a;
    while d > std::f32::consts::PI {
        d -= std::f32::consts::TAU;
    }
    while d <= -std::f32::consts::PI {
        d += std::f32::consts::TAU;
    }
    d
}

/// Pick the names to write and where to write them.
///
/// Pure world space: a street carries its name if it is long enough in metres,
/// the important streets are placed first, and a candidate whose footprint
/// overlaps a placed one is dropped. The camera only decides which way round the
/// letters run.
fn choose_labels<'a>(
    store: &'a StreetLabelStore,
    font: &fontdue::Font,
    height_m: f32,
    cam_right: Vec2,
    view_at: Vec2,
) -> Vec<Placement<'a>> {
    let mut candidates: Vec<Placement<'a>> = Vec::new();
    for line in store.lines() {
        if line.points[0].distance(view_at) > SELECT_RADIUS_M + line.length {
            continue; // nowhere near the view; cheap enough to test per line
        }
        let (tw, th) = text_extent(font, &line.name);
        let width_m = height_m * tw as f32 / th as f32;
        if width_m > line.length * FIT_MARGIN {
            continue; // the street cannot carry its own name at this size
        }
        let Some((centre, mut dir)) = place_along(&line.points, width_m) else {
            continue;
        };
        if centre.distance(view_at) > SELECT_RADIUS_M {
            continue;
        }
        // Read left-to-right from where the camera is. With no camera basis (a
        // straight-down view) fall back to the cartographic rule: eastwards, and
        // southwards for a north–south street.
        let flip = if cam_right == Vec2::ZERO {
            dir.x < 0.0 || (dir.x == 0.0 && dir.y < 0.0)
        } else {
            dir.dot(cam_right) < 0.0
        };
        if flip {
            dir = -dir;
        }
        candidates.push(Placement {
            name: &line.name,
            rank: line.rank,
            run_m: line.length,
            centre,
            dir,
            width_m,
            ground_m: line.ground_m,
        });
    }

    // Class first, then nearest to the view: a crowded junction keeps the arterial
    // and loses the alley, and the pool's slots are spent on the streets the
    // camera is actually over rather than on a through road a kilometre behind it.
    // Length only breaks a tie between two equally near streets of one class.
    candidates.sort_by(|a, b| {
        a.rank.cmp(&b.rank).then_with(|| {
            a.centre
                .distance_squared(view_at)
                .total_cmp(&b.centre.distance_squared(view_at))
                .then_with(|| b.run_m.total_cmp(&a.run_m))
        })
    });

    let mut placed: Vec<(Rect, &str, Vec2)> = Vec::new();
    let mut out: Vec<Placement<'a>> = Vec::new();
    for cand in candidates {
        if out.len() >= MAX_LABELS {
            break;
        }
        let bounds = footprint(&cand, height_m);
        let clash = placed.iter().any(|(r, name, at)| {
            !r.intersect(bounds).is_empty()
                || (*name == cand.name && at.distance(cand.centre) < SAME_NAME_GAP_M)
        });
        if clash {
            continue;
        }
        placed.push((bounds, cand.name, cand.centre));
        out.push(cand);
    }
    out
}

/// The ground footprint a label occupies, as an axis-aligned box.
///
/// Deliberately the *bounds* of the rotated quad rather than the quad: two names
/// crossing at an angle read as a collision even where their exact rectangles
/// miss each other, and a name wants air around it to be legible.
fn footprint(p: &Placement, height_m: f32) -> Rect {
    let (c, s) = (p.dir.x.abs(), p.dir.y.abs());
    let (hw, hh) = (p.width_m * 0.5, height_m * 0.5);
    Rect::from_center_size(p.centre, Vec2::new(hw * c + hh * s, hw * s + hh * c) * 2.0)
}

/// Where a name sits on its street and which way the baseline runs.
///
/// The direction is the **chord across the name's own width**, not the segment
/// under its midpoint: on a curving street the latter is a tangent at one point,
/// and the two ends of the word then walk off the road in opposite directions.
fn place_along(points: &[Vec2], width_m: f32) -> Option<(Vec2, Vec2)> {
    let total = polyline_len(points);
    if total <= f32::EPSILON {
        return None;
    }
    let half = (width_m * 0.5).min(total * 0.5);
    let mid = total * 0.5;
    let a = point_at(points, mid - half)?;
    let b = point_at(points, mid + half)?;
    let dir = (b - a).normalize_or_zero();
    if dir == Vec2::ZERO {
        return None;
    }
    Some((point_at(points, mid)?, dir))
}

/// The point `dist` along a polyline, clamped to its ends.
fn point_at(points: &[Vec2], dist: f32) -> Option<Vec2> {
    let mut left = dist.max(0.0);
    for w in points.windows(2) {
        let seg = w[0].distance(w[1]);
        if seg <= f32::EPSILON {
            continue;
        }
        if left <= seg {
            return Some(w[0].lerp(w[1], left / seg));
        }
        left -= seg;
    }
    points.last().copied()
}

fn polyline_len(points: &[Vec2]) -> f32 {
    points.windows(2).map(|w| w[0].distance(w[1])).sum()
}

/// A unit quad lying in the XZ plane, `u` along +X and `v` along +Z, so a yaw
/// rotation that sends +X down the street writes the name along it.
fn label_quad() -> Mesh {
    use bevy::render::mesh::{Indices, PrimitiveTopology};
    Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    )
    .with_inserted_attribute(
        Mesh::ATTRIBUTE_POSITION,
        vec![
            [-0.5f32, 0.0, -0.5],
            [0.5, 0.0, -0.5],
            [0.5, 0.0, 0.5],
            [-0.5, 0.0, 0.5],
        ],
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0f32, 1.0, 0.0]; 4])
    .with_inserted_attribute(
        Mesh::ATTRIBUTE_UV_0,
        vec![[0.0f32, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
    )
    .with_inserted_indices(Indices::U32(vec![0, 1, 2, 0, 2, 3]))
}

/// The rasteriser, over the brand semibold the app already embeds
/// (`theme::BRAND_TTF`). Weight is the point: a road marking is read at a dozen
/// pixels through a foreshortening projection, and egui's light body face breaks
/// up into grey mush at that size. Falls back to egui's own stack if the brand
/// face ever stops parsing.
fn load_font() -> Option<fontdue::Font> {
    if let Ok(font) = fontdue::Font::from_bytes(theme::BRAND_TTF, fontdue::FontSettings::default())
    {
        return Some(font);
    }
    let defs = egui::FontDefinitions::default();
    for family in ["Ubuntu-Light", "Hack"] {
        if let Some(data) = defs.font_data.get(family) {
            if let Ok(font) =
                fontdue::Font::from_bytes(data.font.as_ref(), fontdue::FontSettings::default())
            {
                return Some(font);
            }
        }
    }
    tracing::warn!("no embedded font for street labels");
    None
}

/// Wrap a rasterised name in a texture **with a mip chain**.
///
/// Not optional. A label is 8–20 m of world laid at a shallow angle, so its
/// texture is nearly always *minified* — level 0 alone samples one texel in four
/// and drops half of every stroke, which reads as thin grey text that gets worse
/// the further away it is. The chain is box-filtered on premultiplied alpha, or
/// the transparent margin bleeds its colour into the letters as they shrink.
fn label_image(level0: Vec<u8>, w: u32, h: u32) -> Image {
    let mut data = level0.clone();
    let (mut cur, mut cw, mut ch) = (level0, w, h);
    let mut levels = 1u32;
    while cw > 1 || ch > 1 {
        let (nw, nh) = ((cw / 2).max(1), (ch / 2).max(1));
        let mut next = vec![0u8; (nw * nh * 4) as usize];
        for y in 0..nh {
            for x in 0..nw {
                let mut acc = [0f32; 4];
                for dy in 0..2 {
                    for dx in 0..2 {
                        let sx = (x * 2 + dx).min(cw - 1);
                        let sy = (y * 2 + dy).min(ch - 1);
                        let i = ((sy * cw + sx) * 4) as usize;
                        let a = f32::from(cur[i + 3]) / 255.0;
                        acc[0] += f32::from(cur[i]) * a;
                        acc[1] += f32::from(cur[i + 1]) * a;
                        acc[2] += f32::from(cur[i + 2]) * a;
                        acc[3] += a;
                    }
                }
                let j = ((y * nw + x) * 4) as usize;
                if acc[3] > 0.0 {
                    for k in 0..3 {
                        next[j + k] = (acc[k] / acc[3]).clamp(0.0, 255.0) as u8;
                    }
                }
                next[j + 3] = (acc[3] / 4.0 * 255.0).clamp(0.0, 255.0) as u8;
            }
        }
        data.extend_from_slice(&next);
        cur = next;
        cw = nw;
        ch = nh;
        levels += 1;
    }

    let mut image = Image::new_uninit(
        Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD,
    );
    image.texture_descriptor.mip_level_count = levels;
    image.data = Some(data);
    image.sampler = crate::systems::tile_loader::ground_sampler();
    image
}

/// Texture size for a name, in pixels. Shared by the raster and by the fit test,
/// which needs the aspect ratio *before* any texture exists.
fn text_extent(font: &fontdue::Font, text: &str) -> (u32, u32) {
    let pad = (HALO_PX + 2) as f32;
    let advance: f32 = text
        .chars()
        .map(|c| font.metrics(c, GLYPH_PX).advance_width)
        .sum();
    let line = font
        .horizontal_line_metrics(GLYPH_PX)
        .map_or(GLYPH_PX * 1.25, |m| m.ascent - m.descent);
    (
        (advance + 2.0 * pad).ceil().max(4.0) as u32,
        (line + 2.0 * pad).ceil().max(4.0) as u32,
    )
}

/// Rasterise a name: ink over a pale halo, on transparent.
///
/// The halo is a dilation of the glyph coverage, not a second pass of offset
/// text — an outline has to be even on every side of a letter, and four diagonal
/// copies leave the horizontals thin.
fn render_name(font: &fontdue::Font, text: &str) -> (Vec<u8>, u32, u32) {
    let (w, h) = text_extent(font, text);
    let (wi, hi) = (w as i32, h as i32);
    let pad = (HALO_PX + 2) as f32;
    let ascent = font
        .horizontal_line_metrics(GLYPH_PX)
        .map_or(GLYPH_PX, |m| m.ascent);

    // 1. Glyph coverage.
    let mut cov = vec![0u8; (w * h) as usize];
    let mut pen = pad;
    for ch in text.chars() {
        let (m, bitmap) = font.rasterize(ch, GLYPH_PX);
        let gx = pen as i32 + m.xmin;
        let gy = (pad + ascent) as i32 - m.height as i32 - m.ymin;
        for row in 0..m.height {
            let y = gy + row as i32;
            if y < 0 || y >= hi {
                continue;
            }
            for col in 0..m.width {
                let x = gx + col as i32;
                if x < 0 || x >= wi {
                    continue;
                }
                let a = bitmap[row * m.width + col];
                let i = (y * wi + x) as usize;
                cov[i] = cov[i].max(a);
            }
        }
        pen += m.advance_width;
    }

    // 2. Halo: a disc dilation of the coverage.
    let mut halo = vec![0u8; cov.len()];
    for y in 0..hi {
        for x in 0..wi {
            let mut best = 0u8;
            for dy in -HALO_PX..=HALO_PX {
                for dx in -HALO_PX..=HALO_PX {
                    if dx * dx + dy * dy > HALO_PX * HALO_PX {
                        continue;
                    }
                    let (sx, sy) = (x + dx, y + dy);
                    if sx < 0 || sx >= wi || sy < 0 || sy >= hi {
                        continue;
                    }
                    best = best.max(cov[(sy * wi + sx) as usize]);
                }
            }
            halo[(y * wi + x) as usize] = best;
        }
    }

    // 3. Ink over halo, source-over onto transparent.
    let mut px = vec![0u8; cov.len() * 4];
    for i in 0..cov.len() {
        let ink_a = f32::from(cov[i]) / 255.0;
        let halo_a = f32::from(halo[i]) / 255.0 * HALO_ALPHA;
        let out_a = ink_a + halo_a * (1.0 - ink_a);
        let j = i * 4;
        if out_a <= 0.0 {
            continue;
        }
        for k in 0..3 {
            let c = (INK[k] * ink_a + HALO[k] * halo_a * (1.0 - ink_a)) / out_a;
            px[j + k] = (c.clamp(0.0, 1.0) * 255.0) as u8;
        }
        px[j + 3] = (out_a * 255.0) as u8;
    }
    (px, w, h)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn font() -> fontdue::Font {
        load_font().expect("egui ships an embedded font")
    }

    fn line(name: &str, points: Vec<Vec2>) -> WorldLabelLine {
        WorldLabelLine {
            name: name.into(),
            rank: 3,
            length: polyline_len(&points),
            points,
            ground_m: None,
        }
    }

    fn store_with(lines: Vec<WorldLabelLine>) -> StreetLabelStore {
        let mut store = StreetLabelStore::default();
        store.cells.insert((0, 0), lines);
        store
    }

    /// The whole reason the labels moved into the world: where a name goes is a
    /// fact about the street, not about the camera. Same map, two cameras, same
    /// positions — only the reading direction may differ.
    #[test]
    fn placement_does_not_depend_on_the_camera() {
        let store = store_with(vec![line(
            "Oosterstraat",
            vec![Vec2::new(0.0, 0.0), Vec2::new(300.0, 0.0)],
        )]);
        let font = font();
        let east = choose_labels(&store, &font, 5.0, Vec2::X, Vec2::ZERO);
        let west = choose_labels(&store, &font, 5.0, -Vec2::X, Vec2::ZERO);
        assert_eq!(east.len(), 1);
        assert_eq!(east[0].centre, west[0].centre, "the name stays on its spot");
        assert_eq!(
            east[0].dir, -west[0].dir,
            "only the reading direction turns"
        );
        assert!(east[0].dir.dot(Vec2::X) > 0.0, "written left-to-right");
    }

    /// A name is only written where the street can carry it, in metres. This is
    /// what makes labels appear as you descend — the size band shrinks and short
    /// streets become writable — with no altitude test of their own.
    #[test]
    fn a_street_too_short_for_its_name_keeps_quiet() {
        let font = font();
        let short = store_with(vec![line(
            "Sint-Walburgstraat",
            vec![Vec2::ZERO, Vec2::new(40.0, 0.0)],
        )]);
        assert!(
            choose_labels(&short, &font, 14.0, Vec2::X, Vec2::ZERO).is_empty(),
            "a 40 m street cannot carry an 18-character name at 14 m tall"
        );
        assert_eq!(
            choose_labels(&short, &font, 3.4, Vec2::X, Vec2::ZERO).len(),
            1,
            "at the street-level size it can"
        );
    }

    /// Two names crossing at a junction must not both be written.
    #[test]
    fn overlapping_names_lose_the_lesser_one() {
        let mut a = line(
            "Grote Markt",
            vec![Vec2::new(-200.0, 0.0), Vec2::new(200.0, 0.0)],
        );
        a.rank = 1;
        let mut b = line(
            "Kleine Steeg",
            vec![Vec2::new(0.0, -200.0), Vec2::new(0.0, 200.0)],
        );
        b.rank = 4;
        let store = store_with(vec![a, b]);
        let chosen = choose_labels(&store, &font(), 8.0, Vec2::X, Vec2::ZERO);
        assert_eq!(chosen.len(), 1, "both were written over each other");
        assert_eq!(chosen[0].name, "Grote Markt", "the bigger road wins");
    }

    /// One street is many features; its name must not be stamped along itself
    /// every 20 m. Two runs of the same name close together → one label.
    #[test]
    fn the_same_name_is_not_repeated_on_top_of_itself() {
        let a = line("Hereweg", vec![Vec2::new(0.0, 0.0), Vec2::new(300.0, 0.0)]);
        let b = line(
            "Hereweg",
            vec![Vec2::new(0.0, 30.0), Vec2::new(300.0, 30.0)],
        );
        let far = line(
            "Hereweg",
            vec![Vec2::new(0.0, 400.0), Vec2::new(300.0, 400.0)],
        );
        let store = store_with(vec![a, b, far]);
        let chosen = choose_labels(&store, &font(), 5.0, Vec2::X, Vec2::ZERO);
        assert_eq!(chosen.len(), 2, "near repeat dropped, far one kept");
        assert!(
            chosen[0].centre.distance(chosen[1].centre) >= SAME_NAME_GAP_M,
            "the two survivors are the far-apart pair"
        );
    }

    /// The baseline spans the word, not the segment under its middle — a curving
    /// street would otherwise throw the ends of the name off the road.
    #[test]
    fn the_baseline_spans_the_name() {
        let dogleg = vec![
            Vec2::new(0.0, -40.0),
            Vec2::new(40.0, -10.0),
            Vec2::new(60.0, -10.0),
            Vec2::new(100.0, 20.0),
        ];
        let (_, wide) = place_along(&dogleg, 90.0).expect("placeable");
        let (_, narrow) = place_along(&dogleg, 4.0).expect("placeable");
        assert!(
            narrow.y.abs() < 1e-3,
            "a short word follows the local segment"
        );
        assert!(wide.y > 0.05, "a long word follows the chord it spans");
    }

    /// Size is banded, and the bands are ordered — a continuous ramp would
    /// re-place every label on every frame of a climb.
    #[test]
    fn label_size_steps_with_altitude() {
        let heights: Vec<f32> = [50.0, 200.0, 500.0, 1000.0]
            .iter()
            .map(|a| label_height(*a))
            .collect();
        assert!(heights.windows(2).all(|w| w[1] > w[0]), "{heights:?}");
        assert_eq!(label_height(60.0), label_height(119.0), "inside one band");
    }

    /// The raster has to be legible against anything: ink in the middle of a
    /// stroke, halo around it, transparent away from the text.
    #[test]
    fn a_name_rasterises_to_ink_with_a_halo() {
        let font = font();
        let (px, w, h) = render_name(&font, "Oosterstraat");
        assert_eq!(px.len(), (w * h * 4) as usize);
        let alphas: Vec<u8> = px.chunks_exact(4).map(|p| p[3]).collect();
        assert!(alphas.iter().any(|a| *a > 250), "no solid text at all");
        assert!(
            alphas.iter().any(|a| *a == 0),
            "the texture is a solid block"
        );
        // Somewhere there must be a pale halo pixel: opaque, but far lighter than
        // the ink. Without it the names vanish over dark tarmac.
        assert!(
            px.chunks_exact(4)
                .any(|p| p[3] > 200 && p[0] > 200 && p[1] > 200),
            "no halo — the ink has nothing to sit on"
        );
        assert!(
            px.chunks_exact(4).any(|p| p[3] > 200 && p[0] < 60),
            "no ink — the name would read as a smear"
        );
    }

    /// The store is keyed and placed by cell, and the cell origin is what turns
    /// tile-local metres into the world frame. An off-by-origin here puts every
    /// name in the neighbouring cell.
    #[test]
    fn cell_lines_are_placed_at_the_cell_origin() {
        let mut store = StreetLabelStore::default();
        store.insert_cell(
            (3, 4),
            1000.0,
            -2000.0,
            vec![StreetLabelLine {
                name: "Oosterstraat".into(),
                rank: 3,
                points: vec![[0.0, 0.0], [100.0, 0.0]],
            }],
        );
        let placed = &store.cells[&(3, 4)][0];
        assert_eq!(placed.points[0], Vec2::new(1000.0, -2000.0));
        assert_eq!(placed.points[1], Vec2::new(1100.0, -2000.0));
        assert_eq!(placed.length, 100.0);
        let before = store.revision;
        store.remove_cell(&(3, 4));
        assert!(store.is_empty());
        assert_ne!(
            store.revision, before,
            "a change must be visible to the placer"
        );
    }
}

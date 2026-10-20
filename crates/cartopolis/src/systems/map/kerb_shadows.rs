//! The kerbs cast into the sun's near cascades and into no others.
//!
//! A kerb is a box 10 cm high and 15 cm wide (`cartopolis_geo::furniture`'s
//! `LineKind::Kerb`), so its shadow is a hand's width — and a street-level cell
//! has 40–66 thousand of them, more triangles than every other street object
//! together. On the desktop rung a cascade texel is ~4 cm, ~12 cm, ~35 cm and ~1 m
//! from the nearest cascade out, so only the first two can draw a kerb's shadow
//! at all; in the other two it is a fraction of a texel, and rendering it there
//! cost most of a street-level frame's shadow work (`deck-1`, 2026-10-20: leaving
//! the kerbs out of the scene altogether took the street frame from 49.8 to
//! ~33 ms, and 24 ms of the 53 were shadows).
//!
//! bevy has no per-cascade caster filter: an entity casts into every cascade its
//! box reaches or into none. So a kerb piece (`SurfaceGroup::Kerb`, cut into
//! compact patches by `cartopolis_geo::patches`) casts while any of it can lie in
//! the near cascades and carries `NotShadowCaster` otherwise. The test is
//! conservative in both directions it can be: distance is measured to the piece's
//! nearest point, and it is weighed against the cascade bound by the *largest*
//! angle a point can make with the view and still be on screen, so a piece
//! anywhere inside the second cascade's depth always casts. The kerbs themselves
//! are drawn everywhere; this only decides which of them go into the shadow maps.

use bevy::camera::Projection;
use bevy::light::NotShadowCaster;
use bevy::prelude::*;

use crate::components::FreeCamera;
use crate::grid::{anchored_xz, AnchoredCamera};
use crate::systems::globe::GlobeState;

/// Index of the farthest cascade a kerb's shadow can be drawn in. See the module
/// note for the texel sizes this rests on.
const LAST_KERB_CASCADE: usize = 1;

/// One cell's kerb piece: its ground box in the anchored world frame, and whether
/// it is casting now.
#[derive(Component)]
pub struct KerbPiece {
    min: Vec2,
    max: Vec2,
    casting: bool,
}

impl KerbPiece {
    /// From the piece's own XZ bounds (`xz_bounds`) and the cell origin it is
    /// placed at. Casting until the first placement says otherwise, which is what
    /// it was spawned doing.
    #[must_use]
    pub fn new(bounds: &[f32; 4], wx: f32, wz: f32) -> Self {
        Self {
            min: Vec2::new(wx + bounds[0], wz + bounds[1]),
            max: Vec2::new(wx + bounds[2], wz + bounds[3]),
            casting: true,
        }
    }

    /// Ground distance from `p` to the nearest point of the piece, metres.
    fn distance(&self, p: Vec2) -> f32 {
        let d = (self.min - p).max(p - self.max).max(Vec2::ZERO);
        d.length()
    }
}

/// `[min_x, min_z, max_x, max_z]` of a mesh's positions.
#[must_use]
pub fn xz_bounds(mesh: &Mesh) -> [f32; 4] {
    let mut b = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
    if let Some(bevy::mesh::VertexAttributeValues::Float32x3(ps)) =
        mesh.attribute(Mesh::ATTRIBUTE_POSITION)
    {
        for p in ps {
            b = [
                b[0].min(p[0]),
                b[1].min(p[2]),
                b[2].max(p[0]),
                b[3].max(p[2]),
            ];
        }
    }
    if b[0] > b[2] {
        [0.0; 4]
    } else {
        b
    }
}

/// How far from the camera a kerb piece may lie and still cast: the far bound of
/// [`LAST_KERB_CASCADE`], divided by the cosine of the view's corner half-angle —
/// a point that far off the axis is still on screen and its depth is the distance
/// times that cosine, so nothing that could fall inside the bound is left out.
fn reach_m(projection: Option<&Projection>) -> f32 {
    let bound = crate::systems::sky::sun_cascade_far_bound(LAST_KERB_CASCADE);
    let corner_cos = match projection {
        Some(Projection::Perspective(p)) => {
            let ty = (p.fov * 0.5).tan();
            let tx = ty * p.aspect_ratio.max(1e-3);
            1.0 / (1.0 + tx * tx + ty * ty).sqrt()
        }
        // Nothing to bound the angle by: cast as far as any cascade reaches.
        _ => return crate::quality::budget().shadow_distance_m,
    };
    bound / corner_cos.max(0.1)
}

/// Switch each kerb piece's shadow on or off against the camera. Only a change
/// touches the entity, so a still camera costs a walk over a few hundred
/// components and nothing else.
pub fn update_kerb_shadows(
    mut commands: Commands,
    camera: AnchoredCamera,
    projection: Query<&Projection, With<FreeCamera>>,
    globe: Res<GlobeState>,
    mut pieces: Query<(Entity, &mut KerbPiece)>,
) {
    if pieces.is_empty() {
        return;
    }
    let Some((cam_cell, cam_tf)) = camera.get() else {
        return;
    };
    let at = anchored_xz(cam_cell, cam_tf.translation);
    let height = globe.altitude_m(cam_cell, cam_tf).max(0.0) as f32;
    let reach = reach_m(projection.single().ok());
    for (entity, mut piece) in &mut pieces {
        let d = piece.distance(at);
        let cast = d * d + height * height <= reach * reach;
        if cast == piece.casting {
            continue;
        }
        piece.casting = cast;
        if cast {
            commands.entity(entity).try_remove::<NotShadowCaster>();
        } else {
            commands.entity(entity).try_insert(NotShadowCaster);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distance_is_to_the_nearest_point_of_the_box() {
        let k = KerbPiece::new(&[0.0, 0.0, 100.0, 50.0], 1000.0, 2000.0);
        assert_eq!(k.distance(Vec2::new(1050.0, 2025.0)), 0.0, "inside");
        assert_eq!(k.distance(Vec2::new(1150.0, 2025.0)), 50.0, "east of it");
        assert!(
            (k.distance(Vec2::new(1103.0, 2054.0)) - 5.0).abs() < 1e-4,
            "off a corner"
        );
    }

    /// The reach always covers the cascade's own depth, and covers more the wider
    /// the view, because a point far off the axis is nearer in depth than in range.
    #[test]
    fn the_reach_is_never_short_of_the_cascade() {
        let bound = crate::systems::sky::sun_cascade_far_bound(LAST_KERB_CASCADE);
        let narrow = Projection::Perspective(PerspectiveProjection {
            fov: 0.4,
            aspect_ratio: 1.0,
            ..default()
        });
        let wide = Projection::Perspective(PerspectiveProjection {
            fov: 1.4,
            aspect_ratio: 2.0,
            ..default()
        });
        let (n, w) = (reach_m(Some(&narrow)), reach_m(Some(&wide)));
        assert!(n >= bound, "{n} < {bound}");
        assert!(w > n, "a wider view must reach further: {w} vs {n}");
    }
}

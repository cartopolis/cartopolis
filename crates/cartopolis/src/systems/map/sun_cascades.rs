//! The sun's shadow cascades, sized for a steady lens.
//!
//! Bevy fits each cascade to the slice of the camera frustum it covers
//! (`bevy_light::cascade::build_directional_light_cascades`): the cascade's side
//! is the slice's diagonal rounded up to whole metres, and its texel the side
//! over the map's resolution. That keeps shadows still while the camera moves —
//! the texel grid is snapped in light space — but not while the **lens** changes,
//! and the follow camera widens its lens with speed (`FOLLOW_FOV_SPRINT_GAIN`,
//! eased every frame). Each whole metre the diagonal crossed re-scaled every
//! shadow on screen by a texel: walking, the shadow edges high on the facades,
//! where the far cascades are coarsest, crawled and flickered.
//!
//! So for any lens inside the follow camera's range this redoes bevy's fit
//! after it, with the frustum of the widest lens of that range. The cascades
//! still cover the narrower view (a wider frustum contains it), and their size
//! no longer moves. What it costs: at rest the cascades are those of a ~52° lens
//! rather than 45°, ~15 % wider, so each texel is that much coarser. A lens
//! outside the range — a camera picture's chosen focal length — is left to bevy:
//! it does not breathe.

use bevy::camera::{CameraProjection, PerspectiveProjection};
use bevy::light::cascade::Cascade;
use bevy::light::{CascadeShadowConfig, Cascades, DirectionalLightShadowMap};
use bevy::math::{Mat4, Vec3A, Vec4};
use bevy::prelude::*;

use crate::components::FreeCamera;
use crate::systems::free_camera::{FOLLOW_FOV_BASE, FOLLOW_FOV_SPRINT_GAIN};

/// The lens the cascades are fitted to for a camera whose lens is `fov`, or
/// `None` to leave bevy's fit alone.
pub(crate) fn steady_fov(fov: f32) -> Option<f32> {
    const SLACK: f32 = 1e-3;
    let widest = FOLLOW_FOV_BASE + FOLLOW_FOV_SPRINT_GAIN;
    (FOLLOW_FOV_BASE - SLACK..=widest + SLACK)
        .contains(&fov)
        .then_some(widest)
}

/// Refit the casting lights' cascades for the main camera with [`steady_fov`]'s
/// lens. Runs straight after bevy's own fit, which it overwrites for that one
/// view only.
pub fn steady_sun_cascades(
    shadow_map: Res<DirectionalLightShadowMap>,
    camera: Query<(Entity, &GlobalTransform, &Projection, &Camera), With<FreeCamera>>,
    mut lights: Query<(
        &GlobalTransform,
        &DirectionalLight,
        &CascadeShadowConfig,
        &mut Cascades,
    )>,
) {
    let Ok((view, view_tf, Projection::Perspective(lens), cam)) = camera.single() else {
        return;
    };
    let Some(fov) = cam.is_active.then(|| steady_fov(lens.fov)).flatten() else {
        return;
    };
    let steady = PerspectiveProjection {
        fov,
        ..lens.clone()
    };
    for (light_tf, light, config, mut cascades) in &mut lights {
        if !light.shadow_maps_enabled {
            continue;
        }
        let fitted = fit_cascades(
            &steady,
            view_tf.to_matrix(),
            light_tf.rotation(),
            config,
            shadow_map.size as f32,
        );
        cascades.cascades.insert(view, fitted);
    }
}

/// Bevy's `build_directional_light_cascades` for one view and one light, with
/// the projection given rather than read off the camera.
fn fit_cascades(
    projection: &PerspectiveProjection,
    world_from_view: Mat4,
    light_rotation: Quat,
    config: &CascadeShadowConfig,
    map_size: f32,
) -> Vec<Cascade> {
    // Built from the rotation alone, as bevy does: an orthogonal upper 3×3 with
    // no translation is what keeps the snapped grid exact.
    let world_from_light = Mat4::from_quat(light_rotation);
    let light_from_view = world_from_light.transpose() * world_from_view;
    let overlap = 1.0 - config.overlap_proportion;
    let near_bounds = std::iter::once(config.minimum_distance)
        .chain(config.bounds.iter().map(|far| overlap * far));
    near_bounds
        .zip(config.bounds.iter())
        .map(|(near, &far)| {
            let corners = projection.get_frustum_corners(-near, -far);
            fit_cascade(corners, map_size, world_from_light, light_from_view)
        })
        .collect()
}

/// Bevy's private `calculate_cascade` (bevy_light 0.20, `cascade.rs`), copied
/// unchanged: a texel-snapped orthographic box round one frustum slice. Re-copy
/// it on a bevy bump if bevy's changes.
fn fit_cascade(
    corners: [Vec3A; 8],
    map_size: f32,
    world_from_light: Mat4,
    light_from_view: Mat4,
) -> Cascade {
    let mut min = Vec3A::splat(f32::MAX);
    let mut max = Vec3A::splat(f32::MIN);
    for corner in corners {
        let p = light_from_view.transform_point3a(corner);
        min = min.min(p);
        max = max.max(p);
    }
    let body_diagonal = (corners[0] - corners[6]).length_squared();
    let far_plane_diagonal = (corners[4] - corners[6]).length_squared();
    let diameter = body_diagonal.max(far_plane_diagonal).sqrt().ceil();
    let texel = diameter / map_size;
    let near_plane_center = Vec3A::new(
        (0.5 * (min.x + max.x) / texel).floor() * texel,
        (0.5 * (min.y + max.y) / texel).floor() * texel,
        max.z,
    );
    let light_from_world = world_from_light.transpose();
    let cascade_from_world = Mat4::from_cols(
        light_from_world.x_axis,
        light_from_world.y_axis,
        light_from_world.z_axis,
        (-near_plane_center).extend(1.0),
    );
    let world_from_cascade = Mat4::from_cols(
        world_from_light.x_axis,
        world_from_light.y_axis,
        world_from_light.z_axis,
        world_from_light * near_plane_center.extend(1.0),
    );
    // Reverse Z, as bevy's.
    let r = (max.z - min.z).recip();
    let clip_from_cascade = Mat4::from_cols(
        Vec4::new(2.0 / diameter, 0.0, 0.0, 0.0),
        Vec4::new(0.0, 2.0 / diameter, 0.0, 0.0),
        Vec4::new(0.0, 0.0, r, 0.0),
        Vec4::new(0.0, 0.0, 1.0, 1.0),
    );
    Cascade {
        world_from_cascade,
        clip_from_cascade,
        clip_from_world: clip_from_cascade * cascade_from_world,
        texel_size: texel,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::light::CascadeShadowConfigBuilder;

    fn config() -> CascadeShadowConfig {
        CascadeShadowConfigBuilder {
            num_cascades: 4,
            minimum_distance: 2.0,
            maximum_distance: 1500.0,
            first_cascade_far_bound: 60.0,
            overlap_proportion: 0.2,
        }
        .build()
    }

    fn texels(fov: f32) -> Vec<f32> {
        let lens = PerspectiveProjection {
            fov,
            aspect_ratio: 16.0 / 9.0,
            ..default()
        };
        let sun = Transform::default()
            .looking_to(Vec3::new(-0.6, -0.3, 0.7), Vec3::Y)
            .rotation;
        let view = Transform::from_xyz(3.0, 1.7, -4.0)
            .looking_to(Vec3::new(0.2, 0.3, -1.0), Vec3::Y)
            .to_matrix();
        fit_cascades(&lens, view, sun, &config(), 4096.0)
            .iter()
            .map(|c| c.texel_size)
            .collect()
    }

    /// What this module exists for: across the follow camera's lens range bevy's
    /// own fit changes the texel (the flicker), the steady one does not.
    #[test]
    fn walking_lens_keeps_the_texels() {
        let rest = FOLLOW_FOV_BASE;
        let sprint = FOLLOW_FOV_BASE + FOLLOW_FOV_SPRINT_GAIN;
        assert_ne!(
            texels(rest),
            texels(sprint),
            "bevy's fit moves with the lens"
        );
        let steady: Vec<_> = (0..=20)
            .map(|i| rest + FOLLOW_FOV_SPRINT_GAIN * i as f32 / 20.0)
            .map(|fov| texels(steady_fov(fov).expect("in the follow range")))
            .collect();
        assert!(steady.windows(2).all(|w| w[0] == w[1]));
    }

    #[test]
    fn a_chosen_lens_is_left_to_bevy() {
        assert_eq!(steady_fov(0.3), None);
        assert_eq!(steady_fov(1.2), None);
    }
}

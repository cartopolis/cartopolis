//! Drawing a prim's particle emitter (`cartopolis_core::ParticleEffect`).
//!
//! # The shape of it
//!
//! A prim carrying an emitter grows exactly one child entity: a shared mesh, a
//! material of its own, and [`ParticleEmitter`] recording which effect it is
//! showing. Everything a particle does — where it is, how big, what colour, when
//! it dies — is computed in `assets/shaders/particles.wesl` from that material's
//! twenty floats and the clock. Nothing is simulated on the CPU and nothing is
//! uploaded after the emitter is built.
//!
//! **That is not a micro-optimisation, it is the constraint the feature had to
//! meet.** The obvious implementation — a `Vec<Particle>` stepped each frame into
//! a rewritten vertex buffer — is a per-frame mesh upload per emitter, which is
//! precisely the churn that loses the device on the hardware this ships to
//! (`docs/notes/upload-bursts.md`'s iGPU entry). `systems::grass`
//! reached the same conclusion for the same reason and this follows its pattern:
//! one shared lattice mesh, a small uniform block, and a vertex shader that turns
//! a *slot* into a thing.
//!
//! # A child, not the prim itself
//!
//! Two reasons, and the second is the load-bearing one. A prim already has a mesh
//! and a material, and they are pooled and shared by geometry — an emitter cannot
//! borrow either. And under `big_space` the child inherits the prim's `GridCell`
//! and floating-origin propagation for free, so the emitter follows a prim that a
//! script is flying across the map without this module knowing that prims move.
//!
//! The child's own transform is identity, and the shader takes only the
//! translation out of the model matrix — so the prim's rotation and scale reach
//! the flame not at all, which is what stops a flattened box from having a
//! flattened fire.
//!
//! # A burst plays once per sequence
//!
//! `ParticleEffect::burst` is a one-shot, but it arrives as a *level*: it is
//! carried in the prim for as long as the prim lives, so a client that connects
//! mid-explosion is told about it. What makes it play once is
//! [`ParticleEffect::sequence`] — this module records the sequence it last armed
//! and only re-arms when the number moves. Diffing on the effect itself would
//! restart the same blast twenty times a second, which is the trap
//! `EmoteState::sequence` documents for emotes.
//!
//! # Unverified
//!
//! Colour, brightness and blending are **not** evidence on the container's
//! software rasteriser (`docs/notes/headless-shots-software-renderer.md`). What a
//! headless capture does prove is the geometry half — that the pipeline built,
//! that the quads are placed and that they are in the frame — which is what
//! `particle_emitters` / `particle_quads` in `--dump-state` report and what
//! `--ui fire` exists to photograph.

use bevy::asset::RenderAssetUsages;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;
use cartopolis_core::{ParticleEffect, MAX_PARTICLES};

use crate::components::Prim;

/// The shared quad pool every emitter draws.
///
/// One handle for the whole world, built once at startup at the largest size any
/// effect may ask for; an emitter that wants fewer particles draws the same mesh
/// and collapses the surplus quads to a point in the vertex shader. Trimming the
/// index count per emitter would be a mesh per distinct count, which is the
/// upload this design exists to avoid — and a degenerate triangle is discarded by
/// the rasteriser before it costs a fragment.
#[derive(Resource)]
pub struct ParticlePool {
    mesh: Handle<Mesh>,
}

/// Marks a prim's emitter child, and remembers what it is currently showing.
#[derive(Component)]
pub struct ParticleEmitter {
    /// The effect this child was built for, so the material is only rewritten
    /// when something actually changed.
    effect: ParticleEffect,
    /// Whether this is a burst that has been armed. A burst that has run its
    /// course keeps its entity — the prim still carries the effect, and building
    /// and despawning the child on every blast would be exactly the churn the
    /// module note is about.
    armed_sequence: Option<u32>,
}

// The emitters' material lives in `cartopolis_materials`, so that Bevy's
// per-material plugin code is generated there rather than in this crate.
pub use cartopolis_materials::particles::ParticleMaterial;

/// Build the shared quad pool. Startup, once.
pub fn setup_particle_pool(mut commands: Commands, mut meshes: ResMut<Assets<Mesh>>) {
    commands.insert_resource(ParticlePool {
        mesh: meshes.add(quad_pool(MAX_PARTICLES)),
    });
}

/// `n` unit quads carrying nothing but their identity.
///
/// `position` is the corner (±0.5 in x and y, which is also a sane little
/// bounding box) and `uv.x` is which particle the vertex belongs to. Two
/// attributes and no more: the shader defines its own varyings, so there is
/// nothing to be gained from a normal or a vertex colour, and each one would be
/// four more floats per vertex in the one buffer the whole world shares.
fn quad_pool(n: u16) -> Mesh {
    let n = usize::from(n.max(1));
    let mut positions = Vec::with_capacity(n * 4);
    let mut uvs = Vec::with_capacity(n * 4);
    let mut indices = Vec::with_capacity(n * 6);
    const CORNERS: [[f32; 2]; 4] = [[-0.5, -0.5], [0.5, -0.5], [0.5, 0.5], [-0.5, 0.5]];
    for i in 0..n {
        for c in CORNERS {
            positions.push([c[0], c[1], 0.0]);
            uvs.push([i as f32, 0.0]);
        }
        let base = (i * 4) as u32;
        indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }
    Mesh::new(
        PrimitiveTopology::TriangleList,
        // MAIN_WORLD is not kept: nothing reads these vertices back, and the pool
        // is the one mesh in the world every emitter shares.
        RenderAssetUsages::RENDER_WORLD,
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
    .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, uvs)
    .with_inserted_indices(Indices::U32(indices))
}

/// Keep each prim's emitter child in step with its `particles` field.
///
/// Three cases, all of them here rather than spread over a spawn system and an
/// update system: a prim that gained an emitter, one that lost it, and one whose
/// effect changed (including a burst re-armed with a new sequence).
pub fn sync_particle_emitters(
    mut commands: Commands,
    pool: Option<Res<ParticlePool>>,
    mut materials: ResMut<Assets<ParticleMaterial>>,
    // The moving world's clock, not the loaders': frozen between the frames of a
    // `--video-fps` movie (`systems::anim_clock`), the virtual clock otherwise.
    time: Res<crate::systems::anim_clock::AnimClock>,
    prims: Query<(Entity, &Prim, Option<&Children>)>,
    mut emitters: Query<(
        Entity,
        &mut ParticleEmitter,
        &MeshMaterial3d<ParticleMaterial>,
    )>,
) {
    let Some(pool) = pool else {
        return; // startup has not run yet
    };
    // The same clock the shader reads. `globals.time` is bevy's *wrapped* elapsed
    // time, so a burst start taken from `elapsed_secs()` would be hours ahead of
    // it and every explosion would be in the future — i.e. nothing would ever be
    // drawn, with a healthy emitter count.
    let now = time.elapsed_secs_wrapped();

    for (prim_entity, prim, children) in prims.iter() {
        let existing = children
            .into_iter()
            .flatten()
            .find_map(|c| emitters.get(*c).map(|(e, ..)| e).ok());

        match (&prim.particles, existing) {
            (None, None) => {}
            // The emitter went out. The child goes with it — `try_despawn`,
            // because the prim may already be queued for despawn by a snapshot
            // apply or a session teardown running in the same flush
            // (`docs/notes/entity-commands-outlive-entities.md`).
            (None, Some(child)) => {
                commands.entity(child).try_despawn();
            }
            (Some(effect), None) => {
                let material = materials.add(ParticleMaterial::from_effect(effect, now));
                let child = commands
                    .spawn((
                        Mesh3d(pool.mesh.clone()),
                        MeshMaterial3d(material),
                        Transform::default(),
                        // The quads live nowhere near the mesh's own vertices —
                        // they are placed in the vertex shader, metres away — so
                        // the derived AABB describes the wrong volume and the
                        // emitter would be culled while its particles were on
                        // screen. `ParticleEffect::reach_m` knows the right
                        // radius; there is no component that takes one, so the
                        // honest answer is not to cull this handful of entities.
                        bevy::camera::visibility::NoFrustumCulling,
                        // Two reasons, and either alone would be enough. The
                        // shadow pass rasterises the pool's *lattice* mesh, whose
                        // vertices are all at the emitter's origin — the quads are
                        // placed in the vertex shader, which the depth-only
                        // pipeline does not run — so the shadow such an entity
                        // casts bears no relation to the particles anyone can see.
                        // And the material is `AlphaMode::Add`: a spark is light
                        // being added, not an occluder, so even a correct shadow
                        // of it would be wrong. The line above makes both matter,
                        // because an unculled entity goes into every cascade of
                        // every light with no test at all.
                        bevy::light::NotShadowCaster,
                        ParticleEmitter {
                            effect: *effect,
                            armed_sequence: effect.burst.then_some(effect.sequence),
                        },
                    ))
                    .id();
                commands.entity(prim_entity).add_child(child);
            }
            (Some(effect), Some(child)) => {
                let Ok((_, mut state, handle)) = emitters.get_mut(child) else {
                    continue;
                };
                // A burst re-arms when its sequence moves, and *only* then: the
                // same blast is re-described in every snapshot, and restarting it
                // each time would be a fire that never stops going off.
                let rearm = effect.burst && state.armed_sequence != Some(effect.sequence);
                if !rearm && state.effect == *effect {
                    continue;
                }
                if let Some(mut material) = materials.get_mut(&handle.0) {
                    *material = ParticleMaterial::from_effect(effect, now);
                }
                state.effect = *effect;
                state.armed_sequence = effect.burst.then_some(effect.sequence);
            }
        }
    }
}

/// Marks the prims `--ui fire` seeds, so the harness can find and hold them.
#[derive(Component)]
pub struct ScriptedFire {
    /// Set on the one showing a blast, which has to be re-armed to be
    /// photographable at all — see `shot_harness::hold_scripted_fire`.
    pub burst: bool,
}

/// `(emitters, quads)` — how many prims are drawing particles and how many quads
/// that adds up to, for `--dump-state`.
///
/// The geometry half of "is the fire there?", which is the half a software
/// rasteriser can answer: colour and blending are not evidence in this container,
/// and a count that is zero tells a scripted run that nothing spawned rather than
/// leaving it to read a dark PNG.
pub fn particle_census(emitters: &Query<&ParticleEmitter>) -> (u32, u32) {
    let mut n = 0;
    let mut quads = 0;
    for e in emitters.iter() {
        n += 1;
        quads += u32::from(e.effect.count);
    }
    (n, quads)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pool_is_four_vertices_and_six_indices_per_particle() {
        let mesh = quad_pool(8);
        assert_eq!(mesh.count_vertices(), 32);
        assert_eq!(mesh.indices().map(Indices::len), Some(48));
    }

    /// The vertex shader reads `uv.x` as the particle index, so the pool has to
    /// hand it one that counts. A pool whose uvs were all zero would draw every
    /// quad as particle 0 — one particle, in the right place, with the emitter
    /// count reporting healthy.
    #[test]
    fn every_quads_four_corners_carry_the_same_index() {
        let mesh = quad_pool(4);
        let uvs = match mesh.attribute(Mesh::ATTRIBUTE_UV_0).expect("uv0") {
            bevy::mesh::VertexAttributeValues::Float32x2(v) => v.clone(),
            other => panic!("uv0 is {other:?}, not a float pair"),
        };
        for particle in 0..4usize {
            for corner in 0..4usize {
                assert_eq!(uvs[particle * 4 + corner][0], particle as f32);
            }
        }
    }

    /// A continuous emitter must reach the shader with a negative burst time —
    /// that field is the *only* thing distinguishing a flame from a one-shot, and
    /// a zero would read as "burst, at time zero", i.e. an explosion that finished
    /// before the app started.
    #[test]
    fn a_flame_is_marked_continuous_and_a_blast_is_not() {
        let flame = ParticleMaterial::from_effect(&ParticleEffect::fire(), 12.0);
        assert!(flame.motion.w < 0.0);
        let blast = ParticleMaterial::from_effect(&ParticleEffect::blast(), 12.0);
        assert_eq!(blast.motion.w, 12.0);
    }

    /// Degrees at the author's end, radians at the shader's — the same boundary
    /// rule `set_rot` follows.
    #[test]
    fn the_cone_is_converted_to_radians() {
        let e = ParticleEffect {
            spread_deg: 180.0,
            ..ParticleEffect::fire()
        };
        let m = ParticleMaterial::from_effect(&e, 0.0);
        assert!((m.motion.y - std::f32::consts::PI).abs() < 1e-5);
    }
}

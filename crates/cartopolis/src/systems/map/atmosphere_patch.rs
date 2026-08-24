//! A patched copy of bevy_pbr's `atmosphere/functions.wgsl`, swapped in at runtime.
//!
//! **Why this exists.** On the Adreno 840 (OnePlus 15) the atmosphere renders a
//! black sky at ground level while the sun disc, the city lighting and the limb
//! from orbit are all correct. Three facts pin the fault into one place. The sun
//! disc rendering proves the `render_sky` pass runs, the transmittance LUT is
//! sane and the result is not NaN — the measured state is `inscattering ≈ 0,
//! transmittance ≈ 1`. Forcing `AtmosphereMode::Raymarched` at ground level
//! (`CARTO_GROUND_RAYMARCH`) is *also* black, and the sky-view LUT compute pass
//! is itself a call to `raymarch_atmosphere`, so both sky modes share one
//! integrator and the fault is inside it. And on that handset's tier
//! (`quality::Budget::balanced`, `space_raymarch: false`) orbit and ground run
//! the *identical* LookupTexture path — the app's density ramp even keeps the
//! optical column constant — so the fault is a function of the camera radius
//! alone: broken at `r ≈ inner_radius`, correct at orbit.
//!
//! What differs by `r` inside the integrator is which arm of
//! `get_raymarch_segment` runs: ground takes the inside-the-atmosphere arm of an
//! `if`/`else if`/`else` chain that merges straight into a raymarch loop written
//! as `for (var s = 0.0; s < sample_count; s += 1.0)` — a float counter against
//! a non-constant float bound. Qualcomm's shader compiler has a documented
//! history of miscompiling exactly that shape (branches converging into a loop:
//! ggml-org/llama.cpp#5186; Blender turned SPIR-V optimization off wholesale for
//! Qualcomm over the same class of fault), and this driver was already caught
//! miscompiling geometry on this same phone. A zero-trip loop or an empty
//! segment returns exactly `{inscattering: 0, transmittance: 1}` — the measured
//! state, with no second assumption.
//!
//! **What the patch changes.** The shader is a byte-for-byte copy of bevy_pbr
//! 0.19.0's `functions.wgsl` with three hunks, each tagged `PATCHED
//! (cartopolis)` so the next Bevy bump can diff it against upstream:
//!
//! 1. `get_raymarch_segment` computes the same three cases with `select` instead
//!    of the branch chain — no control-flow merge for a compiler to fumble.
//! 2. The raymarch loop counts with `u32` against `ceil(sample_count)` — the
//!    trip count is unchanged (upstream's float loop also runs
//!    `ceil(sample_count)` times) and the sample positions still divide by the
//!    float, so the arithmetic is identical on a correct compiler.
//! 3. Both sky-view UV parameterizations guard their horizon term:
//!    `sqrt(max(r*r - inner*inner, 0.0))`. At ground level that difference is a
//!    handful of ulps from zero and an FMA-contraction difference reaches
//!    `sqrt(negative)` → NaN → a NaN LUT coordinate. The sibling expression in
//!    `bruneton_functions.wgsl` has carried this exact guard all along, which is
//!    why the transmittance LUT (and with it the sun disc) never went black.
//!    The on-device A/Bs argue this one was not the cause (the 4.5 km
//!    inner-radius margin was black too), but the guard is what upstream should
//!    have written, costs nothing, and covers the case where those A/B switches
//!    never actually reached the device.
//!
//! **Why a runtime swap and not a fork.** The alternative is vendoring
//! `bevy_pbr` — fifty thousand lines re-forked on every Bevy bump, a trade
//! `docs/notes/sky-without-an-atmosphere.md` already refuses. This is one WGSL
//! file. It cannot be replaced through the embedded-asset registry either:
//! `load_shader_library!` registers *and loads* the asset inside
//! `AtmospherePlugin::build`, so a later registry insert races the async load.
//! Replacing the loaded `Shader` asset is race-free — `PipelineCache::
//! extract_shaders` handles the `Modified` event and re-composes every
//! dependent pipeline, the same machinery shader hot-reload rides on. The frames
//! before the swap lands render with the stock shader, which on every known-good
//! platform is identical anyway.
//!
//! **It runs on every platform on purpose.** The patch is arithmetic-identical
//! on a correct compiler, so desktop and the container's lavapipe render through
//! the patched module too — every `cargo shots` run and the scene-smoke CI job
//! verify it continuously, instead of it being a phone-only branch nobody can
//! see working. If the patched module ever fails to compose, every atmosphere
//! pipeline errors and the shot harness photographs a black sky — loudly.

use bevy::prelude::*;
use bevy::shader::{Shader, ShaderImport};

/// The patched module source. Lives with the app's other shaders so it is
/// findable, but is compiled in — it must exist before any asset IO settles,
/// and on Android there is no asset directory to lose it in.
const PATCHED_FUNCTIONS_WGSL: &str =
    include_str!("../../../../../assets/shaders/patched/bevy_pbr_atmosphere_functions.wgsl");

/// The naga_oil import path the stock module declares, which is how the loaded
/// asset is found — the handle `load_shader_library!` made was leaked, so the
/// path is the only address it has.
const FUNCTIONS_IMPORT_PATH: &str = "bevy_pbr::atmosphere::functions";

/// Swap the loaded `bevy_pbr::atmosphere::functions` shader for the patched
/// copy, once, as soon as the asset exists.
///
/// Retries until the async shader load lands (a few frames at startup), then
/// never looks again. Replacing the asset fires `Modified`, which
/// `PipelineCache::extract_shaders` answers by re-composing every pipeline that
/// imports the module.
pub fn patch_atmosphere_shader(mut shaders: ResMut<Assets<Shader>>, mut done: Local<bool>) {
    if *done {
        return;
    }
    let target = shaders.iter().find_map(|(id, shader)| {
        matches!(&shader.import_path, ShaderImport::Custom(path) if path == FUNCTIONS_IMPORT_PATH)
            .then_some(id)
    });
    let Some(id) = target else {
        // Not loaded yet — the shader loads asynchronously at startup.
        return;
    };
    // The Err arm is an id already in use, which an id taken from `iter` is by
    // construction — replacing is the point.
    let _ = shaders.insert(
        id,
        Shader::from_wgsl(
            PATCHED_FUNCTIONS_WGSL,
            "shaders/patched/bevy_pbr_atmosphere_functions.wgsl",
        ),
    );
    *done = true;
    tracing::info!(
        "atmosphere functions.wgsl swapped for the patched copy \
         (Adreno control-flow + horizon-sqrt hardening)"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The patch is a copy of upstream with four tagged edit sites (the sqrt
    /// guard lands in both sky-view UV functions). If the tags disappear — say
    /// a Bevy bump re-copies the file and forgets the hunks — the swap silently
    /// becomes a no-op that still logs success, which this pins against.
    #[test]
    fn the_patched_shader_carries_its_hunks() {
        assert_eq!(
            PATCHED_FUNCTIONS_WGSL
                .matches("PATCHED (cartopolis)")
                .count(),
            4
        );
        // The import path is the address the swap finds the stock module by;
        // the patched module must answer to the same one or nothing imports it.
        assert!(PATCHED_FUNCTIONS_WGSL
            .starts_with("#define_import_path bevy_pbr::atmosphere::functions"));
        // The load-bearing edits themselves.
        assert!(PATCHED_FUNCTIONS_WGSL
            .contains("sqrt(max(r * r - atmosphere.inner_radius * atmosphere.inner_radius, 0.0))"));
        assert!(PATCHED_FUNCTIONS_WGSL.contains("for (var i = 0u; i < n_samples; i += 1u)"));
        // And the shapes they replace must be gone: a float-counter loop or an
        // unguarded horizon sqrt anywhere in the module means a hunk regressed.
        // The hunk's own comment quotes the upstream loop, so match the code
        // form (with its brace), not the phrase.
        assert!(!PATCHED_FUNCTIONS_WGSL.contains("for (var s = 0.0; s < sample_count; s += 1.0) {"));
        assert!(!PATCHED_FUNCTIONS_WGSL
            .contains("sqrt(r * r - atmosphere.inner_radius * atmosphere.inner_radius);"));
    }

    /// naga_oil's preprocessor must read the patched source exactly as it reads
    /// the stock one: same import path, so the swap is invisible to every
    /// importer.
    #[test]
    fn the_patched_shader_declares_the_stock_import_path() {
        let shader = Shader::from_wgsl(PATCHED_FUNCTIONS_WGSL, "test.wgsl");
        assert_eq!(
            shader.import_path,
            ShaderImport::Custom(FUNCTIONS_IMPORT_PATH.to_string())
        );
    }
}

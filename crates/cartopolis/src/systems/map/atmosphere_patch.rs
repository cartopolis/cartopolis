//! A patched copy of bevy_pbr's `atmosphere/functions.wgsl`, swapped in at runtime.
//!
//! **Why this exists.** On the Adreno 840 (OnePlus 15) the atmosphere renders a
//! black sky inside the air while the sun disc and the city lighting look right
//! — the measured state is `inscattering ≈ 0, transmittance ≈ 1`, in both sky
//! modes, and it keys on the camera radius alone: broken inside the shell,
//! right from orbit. `docs/notes/adreno-atmosphere-miscompile.md` has the
//! whole record; the short version is two rounds of patching.
//!
//! **Round 1 (2026-08-24): control-flow hardening, which did not fix it.** It
//! assumed a Qualcomm miscompile of the integrator's shape (branches merging
//! into a float-counted loop, ggml-org/llama.cpp#5186). The device stayed black,
//! and five probe rounds after it ended in a deny-list entry. The hunks stay —
//! they are arithmetic-identical and cost nothing:
//!
//! 1. `get_raymarch_segment` computes the same three cases with `select`.
//! 2. The raymarch loop counts with `u32` against `ceil(sample_count)`, the same
//!    trip count upstream's float loop runs.
//! 3. Both sky-view UV parameterizations guard their horizon term,
//!    `sqrt(max(r*r - inner*inner, 0.0))`, as `bruneton_functions.wgsl` always
//!    has.
//!
//! **Round 2 (2026-09-28): no whole-struct load of the `atmosphere` uniform.**
//! The probe rounds' own readings, re-read, say the shader sees the camera
//! *above* the atmosphere (their camera check allowed up to twice the shell's
//! radius; their live-ray check only ran for a camera inside it). Upstream's
//! `get_view_position` passes the uniform by value to `clamp_to_surface`, and
//! the `Atmosphere` struct packs `inner_radius` into the padding of a `vec3` —
//! a driver that reads the copied struct with the `vec3` as 16 bytes gets the
//! *outer* radius there and lifts every camera inside the air to 1 m above its
//! top, where an empty sky is the correct answer. Three sites pass the struct
//! by value, and all three now read the fields through the uniform instead:
//!
//! 4. `sample_transmittance_lut` uses a local copy of the transmittance-LUT
//!    parameterization (`transmittance_lut_uv`).
//! 5. `max_atmosphere_distance` takes the top-of-atmosphere distance from a
//!    local copy (`distance_to_top_of_atmosphere`).
//! 6. `get_view_position` inlines the surface clamp.
//!
//! Same values on a driver that loads structs correctly. **Unconfirmed on the
//! device**: the Adreno 840 is still on the deny list in
//! `free_camera::driver_denies_atmosphere` until Settings ▸ Graphics ▸ Sky ▸
//! Full shows a blue sky there.
//!
//! Each hunk is tagged `PATCHED (cartopolis)` so the next Bevy bump can diff the
//! file against upstream (bevy_pbr 0.19.1's `functions.wgsl`).
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
         (no whole-struct uniform loads, plus the Adreno control-flow hardening)"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The patch is a copy of upstream with seven tagged edit sites (the sqrt
    /// guard lands in both sky-view UV functions, and the struct-load hunk is
    /// tagged at each of its three sites). If the tags disappear — say a Bevy
    /// bump re-copies the file and forgets the hunks — the swap silently
    /// becomes a no-op that still logs success, which this pins against.
    #[test]
    fn the_patched_shader_carries_its_hunks() {
        assert_eq!(
            PATCHED_FUNCTIONS_WGSL
                .matches("PATCHED (cartopolis)")
                .count(),
            7
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

    /// No code in the module passes the `atmosphere` uniform by value — that
    /// is a load of the whole struct, the thing the round-2 hunks exist to
    /// remove. The hunk's own comment names the upstream calls, so this looks
    /// only at code: every line with its `//` comment cut off.
    #[test]
    fn the_patched_shader_never_loads_the_uniform_whole() {
        let code: String = PATCHED_FUNCTIONS_WGSL
            .lines()
            .map(|line| line.split("//").next().unwrap_or(""))
            .collect::<Vec<_>>()
            .join("\n");
        for by_value in [
            "clamp_to_surface(atmosphere,",
            "transmittance_lut_r_mu_to_uv(atmosphere,",
            "distance_to_top_atmosphere_boundary(atmosphere,",
            "calculate_visible_sun_ratio(atmosphere,",
        ] {
            assert!(!code.contains(by_value), "{by_value} is back");
        }
        // The helpers that replace them read the fields through the uniform.
        assert!(code.contains("fn distance_to_top_of_atmosphere(r: f32, mu: f32)"));
        assert!(code.contains("fn transmittance_lut_uv(r: f32, mu: f32)"));
        assert!(code.contains("let min_radius = atmosphere.inner_radius + EPSILON;"));
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

//! Patched copies of four bevy_pbr shaders, swapped in at runtime — three so that
//! nothing in this app's frame loads bevy's `Atmosphere` struct whole, and one
//! (`render_sky.wesl`) so that the air's haze stops where the ground begins.
//!
//! **Why.** Bevy (0.19, and still 0.20) hands its `Atmosphere` binding to
//! helper functions *by value* —`clamp_to_surface(atmosphere, ..)`, `transmittance_lut_r_mu_to_uv(
//! atmosphere, ..)` and friends — and naga compiles each such call into one
//! load of the entire struct. The struct packs `inner_radius` into the padding
//! of `ground_albedo: vec3` (byte 12, `outer_radius` at 16). The Adreno 840
//! (OnePlus 15, driver 0842.x) reads that copy wrong: its `inner_radius` comes
//! out as the outer radius, so `clamp_to_surface` lifted the camera to 1 m above
//! the air and the sky rendered black with a full-strength sun disc. That
//! reading of the August probe rounds was confirmed on the device on
//! 2026-09-28, when the first copy below turned the phone's sky blue. The whole
//! record, and why a month of work blamed a compiler, is
//! `docs/notes/adreno-atmosphere-struct-copy.md`.
//!
//! **The copies.** Each is a byte copy of bevy_pbr 0.20.0's `.wesl` file with
//! tagged hunks (`PATCHED (cartopolis)`) that read the fields through the
//! binding — the same values on any driver that copies structs correctly. They
//! were first made from 0.19.1's naga_oil `.wgsl` files and re-applied, hunk
//! for hunk, to 0.20's WESL ones:
//!
//! 1. `bevy_pbr::atmosphere::functions` — every atmosphere pass: the sky, its
//!    lookup tables and the sky-reflection cubemap. Also guards the sky-view
//!    horizon `sqrt` against a negative, as `bruneton_functions.wesl`'s own
//!    sibling expression always has. In 0.20 `sample_transmittance_lut_segment`
//!    moved to `bruneton_functions`, but it still calls this module's
//!    `sample_transmittance_lut`, so it reads through the patched one.
//! 2. `bevy_pbr::render::pbr_lighting` (`bevy_pbr::lighting` in 0.19) — the
//!    air's dimming of sunlight on every lit surface. Without it the phone lit buildings with the sun as it is above
//!    the atmosphere: too bright, and never orange at sunset.
//! 3. `bevy_pbr::volumetric_fog::volumetric_fog` — the same dimming for the
//!    light shafts. This copy also carries a cheaper march, so unlike the other
//!    three it is not arithmetic-identical to stock: steps by ray length, no
//!    second march for point lights, the sun's dimming taken at the ray's ends,
//!    and no march at zero density (`light_shafts::altitude_factor`). Its hunks
//!    say why; `render-performance-survey.md` has what they bought.
//!
//! **The upgrade guard.** Each patch records the FNV-1a hash of the stock file
//! it was copied from. A stock shader that no longer matches is *not* replaced:
//! swapping in a copy of an older file would quietly run old lighting code
//! against a new renderer. A debug build panics on the mismatch — so a Bevy bump
//! fails `cargo shots` and the CI smokes at once with instructions — and a
//! release build logs an error and renders with bevy's own shader.
//!
//! **Why a runtime swap and not a fork.** The alternative is vendoring
//! `bevy_pbr` — fifty thousand lines re-forked on every Bevy bump. These are
//! four files. They cannot be replaced through the embedded-asset registry
//! either: `load_shader_library!` registers *and loads* the asset inside the
//! plugin's `build`, so a later registry insert races the async load.
//! Replacing the loaded `Shader` asset is race-free — `PipelineCache::
//! extract_shaders` handles the `Modified` event and re-composes every
//! dependent pipeline, the same machinery shader hot-reload rides on. The frames
//! before the swap lands render with the stock shaders.
//!
//! **Identity under WESL.** Bevy 0.20 names a WESL module by its asset path:
//! `Shader::from_wesl` turns `embedded://bevy_pbr/atmosphere/functions.wesl`
//! into the import path `bevy_pbr::atmosphere::functions`, and the shader cache
//! resolves every `import` of that module (and every pipeline whose entry point
//! it is) by that path. So the copy is built with the *stock shader's own path*,
//! never the repository path of the copy: anything else would register a second
//! module under a different name, which nothing imports and no pipeline uses.
//! Every one of the four is an embedded `.wesl` now — the 0.19 split between
//! libraries (which declared an import path) and entry points (which did not)
//! is gone, so all four are found by import path.
//!
//! **It runs on every platform on purpose.** The copies are arithmetic-identical
//! on a correct driver, so desktop and the container's lavapipe render through
//! them too — every `cargo shots` run and the scene-smoke CI job verify them
//! continuously, instead of a phone-only branch nobody can see working.

use bevy::prelude::*;
use bevy::shader::{Shader, ShaderImport, Source};

/// One stock shader this app replaces.
struct Patch {
    /// How the loaded stock asset is found.
    target: Target,
    /// FNV-1a of the stock source the copy was made from (bevy_pbr 0.20.0).
    stock_fnv: u64,
    /// The copy, compiled in: it must exist before any asset IO settles, and on
    /// Android there is no asset directory to lose it in.
    source: &'static str,
    /// Where the copy lives in this repository, for error messages.
    path: &'static str,
}

/// A stock shader, by the import path `Shader::from_wesl` derives from its
/// embedded asset path. For a library the handle `load_shader_library!` made
/// was leaked, so this is the only address it has; an entry point is found the
/// same way, since every embedded `.wesl` gets one.
#[derive(Clone, Copy)]
struct Target(&'static str);

impl Target {
    fn matches(self, shader: &Shader) -> bool {
        matches!(&shader.import_path, ShaderImport::Custom(p) if p == self.0)
    }
}

const PATCHES: [Patch; 4] = [
    Patch {
        target: Target("bevy_pbr::atmosphere::functions"),
        stock_fnv: 0x7604_8b8c_c6cf_49a5,
        source: include_str!(
            "../../../../../assets/shaders/patched/bevy_pbr_atmosphere_functions.wesl"
        ),
        path: "assets/shaders/patched/bevy_pbr_atmosphere_functions.wesl",
    },
    Patch {
        target: Target("bevy_pbr::render::pbr_lighting"),
        stock_fnv: 0x6287_af15_be87_6776,
        source: include_str!("../../../../../assets/shaders/patched/bevy_pbr_lighting.wesl"),
        path: "assets/shaders/patched/bevy_pbr_lighting.wesl",
    },
    Patch {
        target: Target("bevy_pbr::volumetric_fog::volumetric_fog"),
        stock_fnv: 0xd6b9_2688_4641_46be,
        source: include_str!("../../../../../assets/shaders/patched/bevy_pbr_volumetric_fog.wesl"),
        path: "assets/shaders/patched/bevy_pbr_volumetric_fog.wesl",
    },
    // Not the Adreno fix: the sky pass's aerial perspective, clamped to the
    // stretch of each ray that is in air, so the underground layers seen through
    // the opened ground are not hazed as if three kilometres of air lay between
    // (`systems::underground`, `docs/notes/underground.md`).
    Patch {
        target: Target("bevy_pbr::atmosphere::render_sky"),
        stock_fnv: 0xce9b_a992_c7ec_05e4,
        source: include_str!("../../../../../assets/shaders/patched/bevy_pbr_render_sky.wesl"),
        path: "assets/shaders/patched/bevy_pbr_render_sky.wesl",
    },
];

/// Frames to keep looking for a stock shader that has not loaded. Embedded
/// shaders load within a few frames of startup; a module that never appears
/// belongs to a plugin this run does not have, and polling every asset every
/// frame for ever would be the cost of that.
const GIVE_UP_AFTER_FRAMES: u32 = 600;

/// FNV-1a, 64-bit. Not a security hash: it only has to notice that bevy's file
/// is not the one the copy was made from.
const fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    let mut i = 0;
    while i < bytes.len() {
        hash ^= bytes[i] as u64;
        hash = hash.wrapping_mul(0x0100_0000_01b3);
        i += 1;
    }
    hash
}

#[derive(Default)]
pub struct PatchProgress {
    /// Per patch: swapped, or refused — either way, never looked at again.
    settled: [bool; PATCHES.len()],
    frames: u32,
}

/// Swap each loaded stock shader for its patched copy, once, as soon as it
/// exists — or refuse, loudly, if bevy's file is not the one it was copied from.
///
/// Replacing the asset fires `Modified`, which `PipelineCache::extract_shaders`
/// answers by re-composing every pipeline that uses it.
pub fn patch_atmosphere_shader(
    mut shaders: ResMut<Assets<Shader>>,
    mut progress: Local<PatchProgress>,
) {
    if progress.settled.iter().all(|settled| *settled) {
        return;
    }
    progress.frames += 1;
    for (index, patch) in PATCHES.iter().enumerate() {
        if progress.settled[index] {
            continue;
        }
        let Some((id, stock)) = shaders
            .iter()
            .find(|(_, shader)| patch.target.matches(shader))
        else {
            if progress.frames == GIVE_UP_AFTER_FRAMES {
                tracing::debug!(
                    copy = patch.path,
                    "stock shader never loaded; not patching it"
                );
                progress.settled[index] = true;
            }
            continue;
        };
        progress.settled[index] = true;
        let stock_fnv = match &stock.source {
            Source::Wesl(source) => fnv1a(source.as_bytes()),
            _ => 0,
        };
        if stock_fnv != patch.stock_fnv {
            let message = format!(
                "bevy's {} is not the file {} was copied from (FNV-1a {stock_fnv:#018x}, \
                 expected {:#018x}) — the stock shader stays. Re-copy it from the new bevy_pbr, \
                 re-apply the PATCHED (cartopolis) hunks and update the hash in atmosphere_patch.rs",
                stock.path, patch.path, patch.stock_fnv,
            );
            if cfg!(debug_assertions) {
                panic!("{message}");
            }
            tracing::error!("{message}");
            continue;
        }
        let replacement = patched_shader(patch, stock);
        // The Err arm is an id already in use, which an id taken from `iter` is by
        // construction — replacing is the point.
        let _ = shaders.insert(id, replacement);
        tracing::info!(
            copy = patch.path,
            "bevy shader swapped for its patched copy"
        );
    }
}

/// The copy, wearing the stock shader's identity: built from the stock asset
/// path so `from_wesl` derives the same import path and module path (and
/// re-scans the copy's own imports, which differ from stock where a hunk
/// dropped one), with the stock asset's shader defs — which the shader cache
/// folds into every importer's compile — and dependency handles carried over.
fn patched_shader(patch: &Patch, stock: &Shader) -> Shader {
    let mut shader = Shader::from_wesl(patch.source, stock.path.clone());
    shader.shader_defs = stock.shader_defs.clone();
    shader.file_dependencies = stock.file_dependencies.clone();
    shader.validate_shader = stock.validate_shader.clone();
    shader
}

#[cfg(test)]
mod tests {
    use super::*;

    fn patch(path: &str) -> &'static Patch {
        PATCHES
            .iter()
            .find(|p| p.path.ends_with(path))
            .expect("a patch with that file")
    }

    /// Every code line of a shader, with its `//` comment cut off — the hunks'
    /// own comments quote the upstream calls they replace.
    fn code(source: &str) -> String {
        source
            .lines()
            .map(|line| line.split("//").next().unwrap_or(""))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// If the tags disappear — say a Bevy bump re-copies a file and forgets the
    /// hunks — the swap silently runs stock code and still logs success, which
    /// this pins against.
    #[test]
    fn each_copy_carries_its_hunks() {
        for (file, hunks) in [
            // Two horizon sqrt guards, and three struct-read sites.
            ("bevy_pbr_atmosphere_functions.wesl", 5),
            // The import, the call site, and the helpers.
            ("bevy_pbr_lighting.wesl", 3),
            ("bevy_pbr_volumetric_fog.wesl", 3),
            // The import, and the aerial perspective's clamp to the air.
            ("bevy_pbr_render_sky.wesl", 2),
        ] {
            assert_eq!(
                patch(file).source.matches("PATCHED (cartopolis)").count(),
                hunks,
                "{file}"
            );
        }
    }

    /// The fix itself: no code in any copy hands the atmosphere binding to a
    /// function by value, which is a load of the whole struct.
    #[test]
    fn no_copy_loads_the_atmosphere_struct_whole() {
        for patch in &PATCHES {
            let code = code(patch.source);
            for by_value in [
                "clamp_to_surface(atmosphere,",
                "transmittance_lut_r_mu_to_uv(atmosphere,",
                "transmittance_lut_r_mu_to_uv(view_bindings::atmosphere,",
                "distance_to_top_atmosphere_boundary(atmosphere,",
                "calculate_visible_sun_ratio(atmosphere,",
                "= view_bindings::atmosphere;",
            ] {
                assert!(
                    !code.contains(by_value),
                    "{}: {by_value} is back",
                    patch.path
                );
            }
        }
        let functions = code(patch("bevy_pbr_atmosphere_functions.wesl").source);
        assert!(functions.contains("fn distance_to_top_of_atmosphere(r: f32, mu: f32)"));
        assert!(functions.contains("fn transmittance_lut_uv(r: f32, mu: f32)"));
        assert!(functions.contains("let min_radius = atmosphere.inner_radius + EPSILON;"));
        assert!(functions
            .contains("sqrt(max(r * r - atmosphere.inner_radius * atmosphere.inner_radius, 0.0))"));
        assert!(
            !functions.contains("sqrt(r * r - atmosphere.inner_radius * atmosphere.inner_radius);")
        );
        for file in ["bevy_pbr_lighting.wesl", "bevy_pbr_volumetric_fog.wesl"] {
            let copy = code(patch(file).source);
            for helper in [
                "fn atmosphere_clamp_to_surface(position: vec3<f32>)",
                "fn atmosphere_visible_sun_ratio(r: f32, mu: f32, sun_angular_size: f32)",
                "fn atmosphere_transmittance_uv(r: f32, mu: f32)",
            ] {
                assert!(copy.contains(helper), "{file}: {helper}");
            }
        }
    }

    /// The embedded asset path bevy_pbr 0.20 loads each stock shader from —
    /// `embedded_asset!` / `load_shader_library!` in the module beside it.
    fn stock_path(patch: &Patch) -> &'static str {
        match patch.target.0 {
            "bevy_pbr::atmosphere::functions" => "embedded://bevy_pbr/atmosphere/functions.wesl",
            "bevy_pbr::render::pbr_lighting" => "embedded://bevy_pbr/render/pbr_lighting.wesl",
            "bevy_pbr::volumetric_fog::volumetric_fog" => {
                "embedded://bevy_pbr/volumetric_fog/volumetric_fog.wesl"
            }
            "bevy_pbr::atmosphere::render_sky" => "embedded://bevy_pbr/atmosphere/render_sky.wesl",
            other => panic!("no stock path recorded for {other}"),
        }
    }

    /// The shader cache names a WESL module by the import path `from_wesl`
    /// derives from the asset path: a copy that answered to any other would be a
    /// second module nothing imports and no pipeline runs. So the stock shader is
    /// found by that path, and the copy built the way the swap builds it answers
    /// to the same one, from the same asset path — and is a WESL source, so the
    /// shader cache compiles it through the same resolver.
    #[test]
    fn each_copy_answers_to_the_stock_address() {
        for patch in &PATCHES {
            let stock = Shader::from_wesl("", stock_path(patch));
            assert!(
                patch.target.matches(&stock),
                "{}: target misses stock",
                patch.path
            );
            let copy = patched_shader(patch, &stock);
            assert_eq!(copy.path, stock.path, "{}", patch.path);
            assert_eq!(
                copy.import_path,
                ShaderImport::Custom(patch.target.0.to_string()),
                "{}",
                patch.path
            );
            assert!(patch.target.matches(&copy), "{}", patch.path);
            assert!(matches!(copy.source, Source::Wesl(_)), "{}", patch.path);
        }
    }

    /// The hash the guard compares is the standard FNV-1a, so the recorded
    /// values can be recomputed from bevy's files outside Rust.
    #[test]
    fn fnv1a_is_the_standard_one() {
        assert_eq!(fnv1a(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(fnv1a(b"foobar"), 0x85944171f73967e8);
    }
}

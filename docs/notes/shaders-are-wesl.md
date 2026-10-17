---
topic: shaders-are-wesl
triggers: [assets/shaders, glass.wgsl, atmosphere_patch.rs, systems::atmosphere_patch,
  from_wesl, WESL, naga_oil, "#ifdef", "@if", define_import_path, MATERIAL_BIND_GROUP,
  ShaderCacheError, "unresolved import", "writing a shader", "bumping Bevy"]
updated: 2026-10-17
---

# Shaders are WESL, and nothing in the test suite compiles them

Since the move to Bevy 0.20 (2026-10-09) bevy composes shaders with WESL and
naga_oil is gone from the tree. Every shader here that imports anything or
branches on a shader def is a `.wesl` file, and the habits from the naga_oil
dialect are now errors.

## The dialect, as it differs from what you remember

* `import a::b;` — with the semicolon — and **every import before any other
  item**.
* bevy's modules are named by their file under each crate's `src/`, so the path
  moved with the file: `bevy_pbr::render::forward_io`, not `bevy_pbr::forward_io`;
  `bevy_pbr::render::pbr_lighting`, not `bevy_pbr::lighting`.
* `@if(DEF)` / `@elif(…)` / `@else` attach to a whole declaration, statement or
  `{ }` block, replacing `#ifdef`. A block is a scope, so a `let` declared inside
  one is gone after it — declare outside, assign inside.
* `@group(constants::MATERIAL_BIND_GROUP)` replaces `#{MATERIAL_BIND_GROUP}`.
* Our own libraries are named by their file, not by `#define_import_path`:
  `cloud_field.wesl` and `water_common.wesl` are `super::cloud_field` /
  `super::water_common` from a sibling in `assets/shaders/`.

## Libraries are compiled in, under the path their importers resolve

`sky.rs` and `water.rs` also register those two libraries with
`Shader::from_wesl(include_str!(…), "shaders/cloud_field.wesl")`, so an import
never waits on asset IO (and on Android there is no asset directory to lose
them in). `from_wesl` derives the module path from the path string it is given,
and the shader cache resolves every `import` by that path — so the string must be
the asset path the importers resolve, never the repository path of a copy. That
is the whole trick of `atmosphere_patch`: each patched copy is registered under
the *stock* shader's own `embedded://` path, or it would be a second module that
nothing imports.

A plain `.wgsl` given to bevy is passed through untouched (`bevy_shader` 0.20's
`shader_cache.rs`), which is only right for a shader with no imports at all. The
only one left, `glass.wgsl`, never reaches bevy: `systems::glass` hands it
straight to wgpu.

## Where the errors show up

Not in `cargo check` and not in `cargo test`: nothing compiles a shader until a
pipeline that uses it is drawn. A WESL error is a line in the log of a run that
draws it, and the frame simply lacks whatever the pipeline was for. After any
shader edit, run a capture that shows it and grep its log for
`ShaderCacheError` and `unresolved import`.

On a Bevy bump, `atmosphere_patch`'s copies are re-made from the new bevy's own
files and their hunks re-applied (`adreno-atmosphere-struct-copy.md`); its FNV-1a
guard panics a debug build when a stock file has drifted from the one copied, so
`cargo shots` and the CI smokes fail at once rather than running old lighting
against a new renderer.

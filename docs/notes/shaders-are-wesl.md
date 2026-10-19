---
topic: shaders-are-wesl
triggers: [assets/shaders, crates/bevy_shader, shader_store.rs, glass.wgsl, atmosphere_patch.rs, systems::atmosphere_patch,
  from_wesl, water_common, cloud_field, "registered to a different shader", WESL, naga_oil, "#ifdef", "@if", define_import_path, MATERIAL_BIND_GROUP,
  ShaderCacheError, "unresolved import", "writing a shader", "bumping Bevy"]
updated: 2026-10-19
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

## Our libraries are loaded, never `Assets::add`ed beside the loader

`sky.rs` and `water.rs` hold a handle to `cloud_field.wesl` and
`water_common.wesl` so they stay loaded, and they get it from
`AssetServer::load` on the same path the importers resolve. **They must not
`Shader::from_wesl(include_str!(…), "shaders/…")` it into `Assets<Shader>`.** The
shader loader already loads every `super::…` import from that path, so an added
copy is a second asset at the same WESL module path. Bevy's shader cache then
warns `Shader module path … was registered to a different shader, replacing`,
and it drops and recompiles every pipeline built against the first. On the web
that compile runs on the page's only thread in the middle of a load. Until
2026-10-19 both libraries were registered this way, which was left over from
naga_oil's named imports. Android is not a reason to do it either: the APK
carries `assets/`, and the importing shaders already load from it.

The opposite case is `atmosphere_patch`, and there adding is right. Each patched
copy replaces a *stock* bevy module that nothing else loads, so it is registered
under that module's own `embedded://` path. Otherwise it would be a second module
that nothing imports.

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

**Do that in a debug build.** Since 2026-10-19 a release build composes WESL
without WESL's own validation (`crates/bevy_shader`, which saves half of every
compose on the web), so there a mistake surfaces as the renderer's WGSL error,
naming mangled identifiers. Composed shaders are also kept between runs
(`shader_store.rs`). An edited shader can never be served stale, because its
source hash is part of the entry, but a capture that composes nothing new does
not log the WESL errors of shaders it reused. Delete `shaders/wgsl-v1.bin` from
the cache to force a full compose ([wasm-performance.md](wasm-performance.md)).

On a Bevy bump, `atmosphere_patch`'s copies are re-made from the new bevy's own
files and their hunks re-applied (`adreno-atmosphere-struct-copy.md`); its FNV-1a
guard panics a debug build when a stock file has drifted from the one copied, so
`cargo shots` and the CI smokes fail at once rather than running old lighting
against a new renderer.

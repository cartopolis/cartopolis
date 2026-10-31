---
topic: bevy-features
triggers: [crates/cartopolis/Cargo.toml, avatar.rs, model_assets.rs, placeholder_models.rs, reflect_auto_register, gltf_animation, bevy_animation, bevy_gltf, bevy_world_serialization, WorldAssetRoot, GltfAssetLabel, AnimationClip, bevy_gizmos_render, "rest pose", "animations don't play", "unregistered type", "default-features = false"]
updated: 2026-10-17
---

# Two Bevy features the glTF path cannot live without

The workspace takes `bevy` with `default-features = false` and lists what it
needs (root `Cargo.toml`, where each entry carries its reason). A missing feature
there rarely fails the build. It fails the *run*, and two of them fail it in ways
that point somewhere else entirely.

## `reflect_auto_register`: or the first avatar panics

glTF scenes load as `WorldAsset`s spawned by `bevy_world_serialization`
(`WorldAssetRoot`, not `SceneRoot` — avatars, uploaded models and the street-object
placeholders all go through it), and it **reflect-spawns every component** in the
scene. Without auto-registration of every `#[derive(Reflect)]` type, spawning an
avatar character panics on the first type nobody registered by hand.

## `gltf_animation`: or every glTF stays in its rest pose

The symptom was "animations don't play, the mesh spawns in rest pose", for the fox
and every avatar alike, and it was first put down to an asset-label change. It was
not. The workspace enabled `bevy_animation` — the animation *crate* — but not
`gltf_animation`, and the glTF loader's clip parsing sits behind `bevy_gltf`'s
**own** `bevy_animation` feature, which only that umbrella turns on (bevy_internal
0.20: `gltf_animation = ["bevy_animation", "bevy_gltf?/bevy_animation"]`). With it
off, `bevy_gltf` `#[cfg]`-strips all clip parsing and `GltfAssetLabel::Animation(n)`
loads **zero** `AnimationClip`s — no error, no warning, just no clips.

To check it from a shell, look at the resolved features with `cargo tree -e
features` and confirm `bevy_gltf`'s `bevy_animation` feature is on.

## The pattern

Both are the same trap as `bevy_gizmos_render` (since the 0.19 upgrade
`bevy_gizmos` alone accepts every `Gizmos` call and draws nothing — the prim
editor's handles were invisible from 2026-07-16 to 2026-10-08; `gizmo_render` in `--dump-state` now
notices) and as the `audio` default that the Android and web lanes dropped with
`--no-default-features` ([audio-off-platform.md](audio-off-platform.md)): a
feature that gates a code path by `#[cfg]` deletes it without a trace. Before
removing a feature, find what it resolves to in `bevy_internal`'s `Cargo.toml`;
when something loads but does nothing, check the features before the content.
Crate-specific additions live beside the crate (`wayland` per target in
`crates/cartopolis/Cargo.toml`, `debug` for the Android build only in
`crates/cartopolis_android/Cargo.toml`).

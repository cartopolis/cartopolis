# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build commands

Scope builds to the crate you changed — avoid `cargo build` without `-p` unless you need an integration check.

```bash
# Type-check shared protocol types
cargo check -p vibe_core

# Type-check/check server only (no Bevy — fast)
cargo check -p vibe_sim

# Type-check Bevy client
cargo check -p viberfox

# Run the game client (debug, default profile)
cargo run -p viberfox

# Run with playable framerate in debug (deps at opt-level=3; slower cold builds)
cargo run -p viberfox --profile dev-bevy

# Run with fast incremental links via Bevy dynamic linking (local dev only, not for release)
cargo run -p viberfox --features fast-dev

# Run the headless sim server
cargo run -p vibe_sim

# Release build
cargo build -p viberfox --release
```

Unit tests live in `#[cfg(test)]` modules inside `viberfox`. It has no lib target, so
run them with `cargo test -p viberfox --bin viberfox` (`--lib` fails). Note `cargo check`
does **not** compile `cfg(test)` code — the suite silently rotted through the egui 0.35
migration until someone ran it, so run the tests, not just a check, when touching egui
or the building geometry.

**Linker:** `.cargo/config.toml` uses `clang + lld` on Linux. If linking is slow, switch to `mold` (`fuse-ld=mold`).

### Scripted screenshots (`--shot`)

Answer "what does it actually look like from there?" without flying the camera by
hand (`crates/viberfox/src/systems/shot_harness.rs`):

```bash
# one frame, straight down from 180 km
cargo run -p viberfox --profile dev-bevy -- \
    --shot /tmp/shot.png --at 53.2194,6.5665,180000 --look=-90,0

# an altitude sweep in ONE launch → /tmp/sweep_60000m.png, /tmp/sweep_1000000m.png, …
cargo run -p viberfox --profile dev-bevy -- --shot /tmp/sweep.png \
    --at 53.2194,6.5665,60000 --at 53.2194,6.5665,1000000 --look=-90,0
```

`--look` takes `pitch,yaw` in degrees and pairs with `--at` by position (note the
`=`: clap reads a leading `-90` as a flag otherwise). Runs are **headless**: no
window is created at all (`WinitPlugin` is disabled and `ScheduleRunnerPlugin`
drives the frames), and the camera renders into an offscreen image. Nothing
appears on the desktop, no compositor can occlude — and blacken — a capture, and
a shot works over SSH or in CI with no `DISPLAY`/`WAYLAND_DISPLAY` at all.
Each shot waits for the tile streams to go quiet (`--settle`, capped by `--wait`)
instead of sleeping a fixed time, and logs a `shot state` line — altitude, tier,
near/far, globe entity/visible/rendered counts — which is what separates "black
image" from "black scene". The egui overlay is included (the HUD makes a shot
self-documenting); `--hide-ui` omits it. Scripted runs never read or write
`data/user.json`. Other flags: `--size WxH`, `--shot-mode free|earth`, and
`--time HOURS` / `--fog DENSITY` (seed the day-night and fog state — how the
lighting curves get A/B'd: `--time 6` for dawn, `--time 18 --fog 0` to tell a
washed-out sky apart from a washed-out fog).

With no window, `--size` *is* the screen: `resources::HeadlessSurface` stands in
for the primary `Window` so egui has something to lay out and scissor against
(the overlay renders into the capture as usual) and the globe LOD keeps its
screen-space split threshold honest. Anything that reads `Query<&Window>` must
fall back to it or degrade gracefully — a scissor rect that doesn't match the
render target fails wgpu validation, and Bevy answers that by quitting the run
silently, mid-stream, before the first capture.

Startup switches for bisecting a render problem, each removing one layer:
`VIBERFOX_NO_ATMOSPHERE=1`, `VIBERFOX_NO_HAZE=1`, `VIBERFOX_NO_ENVMAP=1`.

## Architecture

This is a **Cargo workspace** with four crates:

| Crate | Role | Notable deps |
|-------|------|-------------|
| `vibe_core` | Shared protocol types, `NetMessage`, tile/coordinate math | `serde`, `postcard`, `glam` |
| `vibe_sim` | Headless TCP simulation server; SQLite authority; tick loop | `tokio`, `rusqlite`, `refinery` — **no Bevy** |
| `viberfox` | Bevy 0.19 client; rendering, ECS systems, UI | Bevy 0.19 + `big_space` (path dep), `egui` 0.35 |
| `big_space` | **Vendored fork** of `big_space` 0.10 ported to Bevy 0.19 (floating-origin transform propagation, ADR-019). Upstream tops out at Bevy 0.18, so we keep a copy under `crates/big_space` and advance it on each Bevy bump — re-sync the vendored transform-propagation code (`bevy_compat.rs`) against Bevy when upgrading. | Bevy sub-crates |

### Client (`viberfox`) — Bevy ECS layout

- **`components.rs`** — Bevy `Component` types: `Region`, `Prim`, `Avatar`, `RemoteAvatar`, `Selected`, etc.
- **`resources.rs`** — Bevy `Resource` types: `Database`, `GameState`, `AvatarState`, `CameraState`, `ContextMenuState`, `EditDialogState`, `MouseState`, `OsmTileUrlTemplate`, `ConnectAddr`
- **`systems/`** — one file per concern, all registered in `main.rs`:
  - `avatar` — movement, animation, remote avatar interpolation
  - `camera` / `free_camera` — orbit camera, mode toggle
  - `database` — local SQLite load of regions/prims (offline mode)
  - `network` — TCP connection to `vibe_sim`, snapshot apply, intent send
  - `rendering` — spawn/update Bevy meshes for regions and prims
  - `tile_loader` — async OSM tile fetch → region texture
  - `picking` — raycasting for prim selection
  - `ui` — egui context menu + prim edit dialog
  - `gizmo` — transform gizmo toolbar and drag interaction
  - `egui_manager` — custom `EguiPlugin` (wraps `egui-wgpu`); renders egui as the `egui_pass` **system** in the `Core3d` render schedule (Bevy 0.19's schedule-based render graph replaced the old `ViewNode`/render-graph API), drawing onto the post-upscaling sRGB output. Panels use a shared root `Ui` (egui 0.35 shows panels inside a `Ui`, not the `Context`).
- **`db/`** — local SQLite schema init (client-side offline mode only)

**Online vs offline mode:** the client runs in offline mode (local SQLite) by default. Pass `--connect <host:port>` to connect to `vibe_sim`; this replaces the `Database` resource with a network snapshot path.

### Server (`vibe_sim`) — async Tokio

- `config.rs` — layered config: `vibe.toml` → env (`VIBE_*`) → CLI flags
- `db.rs` — SQLite open, `refinery` migrations, world load
- `net.rs` — TCP `handle_connection`, tick loop, `WorldSnapshot` broadcast
- `state.rs` — `SimWorld`: authoritative avatar + prim state, AoI filtering

### Shared (`vibe_core`)

- `protocol.rs` — `NetMessage` enum, `encode/decode_app_frame`, `PROTOCOL_VERSION` (currently **8**)
- `world.rs` — `TileKey`, lat/lng ↔ tile math, `REGION_SIZE_METERS`
- `yaw.rs` — angle helpers for avatar direction continuity

**Wire format (ADR-008–009):** TCP length-delimited frames, little-endian `u32` length; frame body = `protocol_version` (u8) + `message_kind` (u8) + `request_id` (u64) + postcard payload.

## Architecture docs (meta → ADR → code)

Architecture documentation lives under `meta/` (not `docs/`):

- `meta/abstract/` — **why**: vision (`G-*` goals), principles (`P-*`), roadmap tiers (P0–P2), ADR resolution depth (R0–R3), capability map
- `meta/adr/` — **what we chose**: one file per decision; `index.md` lists all ADRs

**Before large or cross-cutting changes**, read in this order:
1. `meta/abstract/vision.md` — goals G-01…G-08
2. `meta/abstract/principles.md` — principles P-01…P-07
3. `meta/abstract/roadmap.md` — P0/P1/P2 priorities
4. `meta/abstract/adr-resolution.md` — R0–R3 depth guide
5. `meta/adr/index.md` — existing decisions

**Create or supersede an ADR** for: wire protocol changes, storage schema, security boundaries, major new dependencies, sim/client process boundary changes. Skip for bug fixes or refactors within an existing ADR's scope.

New ADR checklist: copy `meta/adr/template.md`, set `Aligns with: G-*, P-*`, add a row to `meta/adr/index.md` in the same commit. Match depth to R0–R3 (not every change needs a full R2 ADR).

## Known issues / workarounds

- **Atmosphere is Bevy 0.19's built-in `bevy_pbr` atmosphere** (not `bevy_atmosphere`, which was dropped). It's planetary-scale, so `free_camera::setup_camera` spawns it on a **planet-centered entity** 6,360 km below the local ground (surface at `y = 0`). The camera carries `AtmosphereSettings` + `AtmosphereEnvironmentMapLight` + a physical `Exposure`, the sun runs at `RAW_SUNLIGHT`, and `apply_day_night_cycle` adapts exposure by time of day. **Space tier:** the atmosphere stays on in both tiers; `globe::update_globe_visibility` switches the camera's `AtmosphereSettings` to `AtmosphereMode::Raymarched` (reduced `sky_max_samples`) above `GLOBE_SHOW_ALT` and back to `LookupTexture` below, and drops `AtmosphereEnvironmentMapLight` in space. This only works because `globe::update_atmosphere_origin` moves the planet-centered entity each frame to Earth's true centre in render coordinates: the camera is the big_space floating origin (`GlobalTransform` ≈ 0 at any real altitude), so the old fixed `(0, −R, 0)` placement made bevy_pbr always see a ground-level camera — "Raymarched from space" then integrated a *surface sky dome* over the whole screen (black/washed globe, wasted GPU), which was misdiagnosed as Raymarched being unusable from orbit. Escape hatch: `globe::SPACE_ATMOSPHERE = false` disables the atmosphere in space by stripping the planet entity's `Atmosphere` component (tagged `globe::PlanetAtmosphere`, which keeps a copy for restore). Stripping that component is the **only** real off-switch: bevy_pbr's `extract_atmosphere` inserts `ExtractedAtmosphere`/`GpuAtmosphereSettings` onto the camera's *retained* render-world entity and only removes them when **zero** `Atmosphere` entities exist — removing `AtmosphereSettings` from the camera just drops it out of the extract query, leaving the stale render-world components (and every atmosphere pass gating on them) running forever (an earlier fix did exactly that and was a render-world no-op). The globe-tier near/far override lives in `free_camera::camera_controls` **after** the mode match so it applies in every camera mode (a far plane stuck at 200 km otherwise clips the whole globe → black screen when high).
- **The atmosphere is fed by the sun light — don't pre-extinct it.** bevy_pbr scatters the sun `DirectionalLight`'s *colour and illuminance* and applies its own air-mass extinction on top. `main::sun_light` also models extinction (`utils::sun`, for the surface lighting bevy does *not* attenuate), so the two compound: at the full Kasten–Young curve dawn kept 9% of the beam, the atmosphere took its cut of that, and sunrise rendered as a near-black maroon sky over a maroon city. `SUN_EXTINCTION_SOFTNESS` bends only the large-air-mass end of our curve, `SUN_COLOR_SATURATION` keeps the hue mild, and `TWILIGHT_EXPOSURE_LIFT` supplies the eye adaptation that makes twilight *read* bright. Whenever the sun light changes, check the sky too — every knob moves both.
- **Sky props are viewer-centred shells sized for the ground** — the star field (150 km) and the moon disc (140 km). From orbit that is *nearer than the planet*: stars blended over the Earth disc and the moon quad hung in front of it. `GlobeState::sky_shell_radius` returns the space radius (past the far limb, inside the far plane `camera_controls` sets) and `update_star_sky` / `update_moon` scale the whole transform uniformly, which preserves every angular size. The star shader also takes a **space flag** (`params.z`): its horizon cut (`dir.y`, the *anchor's* up) hid the entire field from orbit, because looking down at the globe every star on screen is below that horizon — the flag drops the cut along with the air extinction and twinkle. Star brightness is likewise ungated from the local clock in space; it was the day/night fade that left the globe in a black void at noon.
- **The sun disk is Bevy's procedural one** (`bevy_light::SunDisk` on the sun `DirectionalLight`, drawn by the atmosphere's sky pass, so it needs no geometry and works raymarched from orbit). It's on by default at Earth's true 0.53°, which is a ~6 px dot; `SUN_DISK_ANGULAR_SIZE` widens it to ~1.4°. The glare around it is `Bloom` on the camera — bloom moved to the `bevy_post_process` feature in Bevy 0.19, which the workspace enables for exactly this.
- **The whole space tier crossfades on `globe::GlobeState::space_factor`** (0 at `GLOBE_SHOW_ALT`, 1 by `SPACE_FULL_ALT` ≈ 3,000 km), never on the `in_space` boolean. Exposure, bloom, sun colour/illuminance, ambient (`apply_day_night_cycle`), the atmosphere shell height + density, and the globe surface albedo all ride it. Switching any of them at the threshold put a hard cut in the middle of a zoom — brightness, haze and glare all stepping at once. The `dev.is_changed()` gate carries a `was_space: Local<f32>` so a climb through the ramp re-applies even when the clock hasn't moved.
- **The space tier's atmosphere is deliberately taller than Earth's, and its density rides altitude.** 100 km of air on a 6,371 km ball is 1.5% of the radius — a four-pixel limb that reads as a drawn outline, not air. `globe::SPACE_ATMOSPHERE_THICKNESS` (2.5×) raises the planet `Atmosphere`'s `outer_radius` with `space_factor`; `Falloff::Exponential`'s scale is a *fraction* of the shell thickness, so the profile stretches with it. Density (`update_globe_visibility` rewrites the shared `globe::AtmosphereMedia` medium via `with_density_multiplier`) is **not** a constant column — it's thin low down (`ATMOSPHERE_DENSITY_NEAR`) so the aerial-perspective wash lifts off the detailed map, rising with altitude to the column that makes the limb glow. Below `CITY_ATMOSPHERE_ALT` it's pinned at full Earth density (this same `Atmosphere` is the city's sky dome / dawn-dusk colour). The three bands blend continuously — the old constant-column rule buried the low-orbit map under blue. Field writes on the existing component are safe; add/remove hangs the device. Bloom also rises in space (`free_camera::SPACE_BLOOM_INTENSITY`) — glare is most of what makes a thin bright limb read as air.
- **The globe's imagery is a map, not a satellite photo** — pale beige land on near-white paper, albedo ~0.8 against the real Earth's ~0.3. The sunlit hemisphere blows out from orbit. Fix it with a **tint on the imagery**, not the camera exposure: the near-white map sits on the tonemapper's shoulder, where stopping down barely darkens the surface but *does* dim the limb and stars (the faint things a space view is made of). The tint is *ramped* with `space_factor` — `globe::GlobeSurfaceAlbedo` goes 1.0 at the threshold → `GLOBE_DEEP_ALBEDO` (0.55) in deep space, applied to each globe material's `base_color` (`update_globe_albedo`, throttled; new tiles read the current value at spawn so they don't pop bright). A *fixed* tint can't do this: the globe replaces the full-brightness flat tiles exactly at the boundary, so a constant 0.55 stepped the map a stop darker the instant the globe appeared — and exposure can't compensate, being global (it would over-expose the flat tiles just below). Ramped-to-1.0-at-the-boundary is the only seam-free option.
- **Space-tier lighting is its own model** (`apply_day_night_cycle`, gated on `globe::GlobeState::in_space`): white unattenuated sun at the solar constant, Earthshine-scale ambient, fixed exposure (`SPACE_EXPOSURE_STOPS`). Running the ground curves from orbit dimmed the planet by the *anchor's* local horizon — an 18:00 fly-out showed a dim red globe instead of a sunlit day side and a dark night side. The system's `dev.is_changed()` gate carries a `was_space` `Local` so crossing the tier boundary alone re-applies it.
- **Mass mesh/texture churn hangs the Intel iGPU (`VK_ERROR_DEVICE_LOST`):** bursts of large uploads + despawns packed into few frames overrun the command-buffer timeout — the device dies, then every render system panics in `Buffer::get_mapped_range` ("Buffer with '' label is invalid" — that panic is downstream noise, not the cause). Three guards exist: the flat-tile build cap, `globe::MAX_GLOBE_TILE_BUILDS_PER_FRAME`, and the OSM detail-patch altitude freeze + refetch cooldown (`osm_buildings::DETAIL_FREEZE_ALT` / `DETAIL_REFETCH_COOLDOWN` — a zoom-out once fired 13 full patch rebuilds, 6,776→46 buildings, in 2.4 s and lost the device at 4.8 km altitude, far below the space tier). Keep any new streaming path per-frame-bounded and drift/altitude-gated.
- **Two directional lights must have matching shadow cascades:** Bevy 0.19's `bevy_light` panics (`index out of bounds` in cascade-visibility) if the sun and moon `DirectionalLight`s have **different `num_cascades`**, or if `shadow_maps_enabled` is toggled at runtime. Keep both at `num_cascades: 4` and never toggle shadows after spawn (see `setup_sky` / `apply_day_night_cycle`).
- **`reflect_auto_register` bevy feature is required:** glTF scenes load as `WorldAsset`s spawned by `bevy_world_serialization`, which reflect-spawns every component; without it, spawning an avatar character panics on the first unregistered type.
- **glTF animations require the `gltf_animation` bevy feature (fixed):** the earlier "animations don't play, mesh spawns in rest pose" symptom was **not** an asset-label change — it was that the workspace enabled `bevy_animation` (the crate) but not `gltf_animation`, so `bevy_gltf` compiled with its own `bevy_animation` feature **off** and `#[cfg]`-stripped all clip parsing. `GltfAssetLabel::Animation(n)` then loaded **zero** `AnimationClip`s for every glTF (fox and avatars alike). Adding `"gltf_animation"` to the `bevy` features (workspace `Cargo.toml`) fixes it. Verify with `cargo tree -e features … | grep bevy_gltf` → must show `FEATURES=[bevy_animation]`.
- **Avatars are Quaternius "Ultimate Modular" characters (CC0), mixed per slot.** Each style is a self-contained glTF (embedded base64 buffers + solid-colour materials — no external textures) with the **same 24-clip set** embedded, and 4 slot-named meshes: `*_Head` (face/hair/headwear), `*_Body` (→ Top), `*_Legs`, `*_Feet`, plus optional accessory (Backpack) and sometimes a **weapon** (Pistol/Sword). `CHARACTER_STYLES` is the catalog (`models/avatar/characters/{Man,Woman}_<file>.gltf`); clip indices are constant (**Idle=4, Run=16**; no fly clip → Idle stands in). `AvatarAppearance` is per-slot (`{sex, head, top, legs, feet, accessory}`); `resolve_avatar_holders` groups the chosen slots by character file into holders, each with a `SlotMask`. `play_avatar_animation_when_ready` hides the meshes not in a holder's mask (and always hides weapons via `is_weapon_node` — no business avatar with a gun), then plays the embedded clips (Case 1). A preset = all four slots one style → a single holder. Mixed slots = several holders (separate skeletons/players) **phase-locked by `sync_avatar_animations`** (copies the first holder's `seek_time` to the rest) so a head from one style and a body from another don't drift. `bind_external_animations` (Case 2) is the fallback for a scene with no embedded clips — currently unexercised.
- **Avatar standing height comes from THREE places, all fox-tuned to 0.45 originally** — fixed to feet-on-ground (`GROUND_HEIGHT` = 0.05): `vibe_sim::state` spawn y (online), `avatar::handle_avatar_movement` gravity clamp (offline), and `geo_nav::AVATAR_GROUND_Y` (teleport / location restore). The teleport one was the sneaky one — it re-plants the avatar on every geo-nav and at startup. The models are origin-at-feet, so 0.45 (= ground + half fox height) floated them. Each present scene spawns as its own child "holder" entity so a per-scene `WorldInstanceReady` observer binds only its own skeleton; all holders play the same `AnimationGraph`, so body and clothing move in lockstep. The head/skull is welded into the base body's full-body mesh (only a thin `Face` surface is separate), so you can't graft a head-only onto an outfit — it's whole-base-body or nothing.
- **`ui_owns_keyboard` must gate on `ctx.text_edit_focused()`, not `egui_wants_keyboard_input()`.** The latter is `memory.focused().is_some()` — true whenever *any* widget (a button, a selectable in the avatar chooser) holds focus, which silently swallowed avatar WASD **and** the Space/Shift fly keys until you clicked empty space. Only real text entry (search bar, rename) should yield the keyboard.
- **Avatar standing height is y=0 (feet on the ground).** The Quaternius models are authored origin-at-feet, so `vibe_sim` spawns the avatar at `start` (not the old `+1.0`, which floated the humanoids a metre up — that offset only ever suited the fox, whose origin is its centre). The follow camera looks at `avatar_pos + 1.5·Y` (≈ chest of a 1.8 m avatar).
- **`bind_external_animations` (avatar Case 2) is the fallback for a scene with no embedded clips** — it roots an `AnimationPlayer` on the `Armature` node and hashes each bone's name-path into the `AnimationTargetId` `bevy_gltf` would have assigned, so clips from a *separate* glTF (matching skeleton) bind and play. Currently unexercised (the Ultimate Modular characters all embed their own clips → Case 1), but kept for future modular content on a shared rig.
- **`--shot-mode avatar` frames the avatar (the way to check avatars).** Free/earth shot modes put the camera at `--at` while the avatar sits at the map anchor — they usually don't coincide, so the avatar is off-screen (this caused a lot of "the avatar isn't rendering" confusion when it was fine, just elsewhere). In avatar mode `place_camera` puts the follow camera on the avatar: `--at` altitude = follow distance, `--look` = (elevation°, azimuth°). Use it at scale 1.0 instead of giant-scaling to hunt the avatar in a free-cam shot.
- **Wayland + GLES flicker:** force Vulkan with `WGPU_BACKEND=vulkan cargo run -p viberfox`.
- **Database migrations:** copy `data/regions.db` before schema upgrades; `refinery` migrations run automatically on `vibe_sim` startup (ADR-013).
- **`fast-dev` feature** uses Bevy dynamic linking — never use it for `--release` or distributable binaries.

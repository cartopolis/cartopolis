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

### The documentation image set (`cargo shots`)

The README images are **data, not a manual ritual**: `docs/shots.toml` remembers
six named camera positions (name, `at = [lat, lng, alt]`, `look = [pitch, yaw]`,
plus per-shot `mode` / `time` / `fog` / `clouds` / `rain` / `ui` / `hide_ui`), and

```bash
cargo shots                      # all of them → docs/img/<name>.png
cargo shots --only globe,route   # a subset
```

regenerates the whole set in one headless launch. `cargo shots` is an alias for
`cargo run -p viberfox --profile dev-bevy -- --shots`, so every `--shot` flag
below still applies (`--csv`, `--dump-state`, `--size`, `--settle`). A manifest
run is the same harness: each preset owns its filename, its camera mode, the
panels it opens and the time of day it is shot in, and the metrics CSV gains a
`name` column so six rows aren't six anonymous viewpoints. The manifest also
pins `tile_url` — a scripted run never sees the server handshake that supplies
one, and the built-in fallback is the *raster* basemap, on which the `route`
preset renders "Routing needs a vector tile source" instead of a route
(`VIBE_TILE_URL` still overrides it). Unknown manifest keys
are a hard error (a typo'd `hide-ui` that silently did nothing would only show up
as a chrome-covered README image later). To add a view, fly there and use the dev
panel's **📋 Copy as preset** button — it puts a pasteable `[[shot]]` block on the
clipboard. The dev panel's numbered capture slots remain, but they are scratch
files now; the README links to the preset outputs.

### Scripted screenshots (`--shot`)

Answer "what does it actually look like from there?" without flying the camera by
hand (`crates/viberfox/src/systems/shot_harness.rs`):

```bash
# one frame, straight down from 180 km
cargo run -p viberfox --profile dev-bevy -- \
    --shot /tmp/shot.png --at 53.2194,6.5665,180000 --look=-90,0

# an altitude sweep in ONE launch → /tmp/sweep_0_60000m.png, /tmp/sweep_1_1000000m.png, …
cargo run -p viberfox --profile dev-bevy -- --shot /tmp/sweep.png \
    --at 53.2194,6.5665,60000 --at 53.2194,6.5665,1000000 --look=-90,0

# a 24-frame flythrough between two --at, with the ramp scalars logged to CSV
cargo run -p viberfox --profile dev-bevy -- --shot /tmp/fly.png \
    --at 53.2194,6.5665,2000 --at 53.2194,6.5665,4000000 --frames 24 \
    --look=-90,0 --csv /tmp/ramps.csv
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

**Sweeps and metrics (the transition/introspection layer).** Every capture also
measures a `ShotMetrics` — altitude, tier, the altitude-driven crossfade scalars
(`space_factor`, exposure, bloom, `globe_albedo`), near/far, and the globe mesh
tally — from one source into three sinks: the `shot state` log line (always),
`--csv PATH` (one row per capture), and `--dump-state PATH` (the full metrics as
JSON — a bare object for a single shot, an array for a sweep). A new inspectable
fact is one field on `ShotMetrics` (`shot_harness.rs`) and it appears in all three
at once — this is the "inspect the settled world from a shell without a recompile
per question" path. `--frames N` (needs ≥2 `--at`) interpolates N viewpoints along
the waypoint polyline (**altitude log-interpolated**, so samples aren't all bunched
in space; lat/lng/look linear) instead of one capture per `--at`; sweep PNGs are
index- **and** altitude-tagged (`fly_00_2000m.png`) so the filmstrip sorts in
flight order. Pair `--frames` with `--csv` to catch a **hard cut in a ramp**: a
jump between adjacent rows of the `space_factor` / `exposure` / `bloom` /
`globe_albedo` columns is a seam a single still can't reveal. The metrics report
*actual* camera state (e.g. the placed altitude after the teleport snaps it), not
the requested `--at` — which is the point.

With no window, `--size` *is* the screen: `resources::HeadlessSurface` stands in
for the primary `Window` so egui has something to lay out and scissor against
(the overlay renders into the capture as usual) and the globe LOD keeps its
screen-space split threshold honest. Anything that reads `Query<&Window>` must
fall back to it or degrade gracefully — a scissor rect that doesn't match the
render target fails wgpu validation, and Bevy answers that by quitting the run
silently, mid-stream, before the first capture.

Startup switches for bisecting a render problem, each removing one layer:
`VIBERFOX_NO_ATMOSPHERE=1`, `VIBERFOX_NO_HAZE=1`, `VIBERFOX_NO_ENVMAP=1`,
`VIBERFOX_NO_OCEAN=1` (the globe's ocean split — see `systems/globe_ocean.rs`).

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
- **`resources.rs`** — Bevy `Resource` types: `GameState`, `AvatarState`, `CameraState`, `ContextMenuState`, `EditDialogState`, `MouseState`, `OsmTileUrlTemplate`, `ConnectAddr`
- **`systems/`** — one file per concern, all registered in `main.rs`:
  - `avatar` — movement, animation, remote avatar interpolation
  - `camera` / `free_camera` — orbit camera, mode toggle
  - `collision` — avatar↔building collision (footprint index; walk-block + teleport eject)
  - `network` — TCP connection to `vibe_sim`, snapshot apply, intent send
  - `rendering` — spawn/update Bevy meshes for regions and prims
  - `tile_loader` — async OSM tile fetch → region texture
  - `picking` — raycasting for prim selection
  - `ui` — egui context menu + prim edit dialog
  - `gizmo` — transform gizmo toolbar and drag interaction
  - `egui_manager` — custom `EguiPlugin` (wraps `egui-wgpu`); renders egui as the `egui_pass` **system** in the `Core3d` render schedule (Bevy 0.19's schedule-based render graph replaced the old `ViewNode`/render-graph API), drawing onto the post-upscaling sRGB output. Panels use a shared root `Ui` (egui 0.35 shows panels inside a `Ui`, not the `Context`).

**Run modes (`main.rs`, ADR-016/031):** the client is **server-authoritative** — the avatar and prims live in `vibe_sim` and are replicated over the wire; the client sends `ClientIntent` (a walk-velocity wish + fly flags) and applies `WorldSnapshot`s. The **default is solo**: an in-process `vibe_sim` on loopback (the `solo` feature in `crates/viberfox/src/solo.rs`), reached through the same authenticated path an external sim uses. `--connect <host:port>` connects to an **external** sim (split mode) instead. `--offline` (implied by `--shot`) runs **no sim at all** — single user, no auth, no world persistence; only the streamed OSM/3DBAG map and a local avatar move under client control. There is **no client-side database** anymore: the old local-SQLite path (`db/`, `systems/database.rs`, the `Database` resource) was removed. Persistence and world authority live in `vibe_sim` (`data/world.db`).

### Server (`vibe_sim`) — async Tokio

- `config.rs` — layered config: `vibe.toml` → env (`VIBE_*`) → CLI flags
- `db.rs` — SQLite open, `refinery` migrations, world load
- `net.rs` — TCP `handle_connection`, tick loop, `WorldSnapshot` broadcast
- `state.rs` — `SimWorld`: authoritative avatar + prim state, AoI filtering

### Shared (`vibe_core`)

- `protocol.rs` — `NetMessage` enum, `encode/decode_app_frame`, and `PROTOCOL_HISTORY`: the wire changelog, one `ProtocolRevision { version, adr, summary }` per revision. `PROTOCOL_VERSION` is *derived* from its last entry, so **bumping the version means appending a row** — don't write the number anywhere else (three docs drifted to "currently 8" while the wire was at 11). A unit test enforces the run stays gapless.
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
- **Avatar↔building collision is client-side prediction, not server physics** (`systems/collision.rs`). The sim (`vibe_sim`) is authoritative for avatar position but has **no building geometry** — OSM/3DBAG footprints stream only on the client. So both building streamers publish 2D footprint polygons into the `BuildingColliders` resource in the anchored world-XZ frame (`osm_buildings` = exact detail-patch rings; `lod22` = a convex hull of each 3DBAG building's projected vertices, since only a triangle soup survives there — cleared/rebuilt per cell and on re-anchor). Collision then acts on whichever channel owns movement in the current run mode: **solo/split (server-authoritative)** steer the *velocity wish* in `network::send_network_intent` (`steer_velocity` slides along walls and auto-walks a trapped avatar out — a snap-eject can't work there, the next snapshot would overwrite the transform); **`--offline`** moves the transform directly in `avatar::handle_avatar_movement` (`slide`) with `collision::eject_avatar_from_buildings` as a snap-out backstop. Gated on `DataLayers::buildings_enabled` (collision matches what's drawn) and skipped while flying. `avatar_state.position.xz` is the avatar's absolute world-XZ (the avatar sits near origin and the world re-anchors around it), so it compares directly against the footprints.
- **`bind_external_animations` (avatar Case 2) is the fallback for a scene with no embedded clips** — it roots an `AnimationPlayer` on the `Armature` node and hashes each bone's name-path into the `AnimationTargetId` `bevy_gltf` would have assigned, so clips from a *separate* glTF (matching skeleton) bind and play. Currently unexercised (the Ultimate Modular characters all embed their own clips → Case 1), but kept for future modular content on a shared rig.
- **`--shot-mode avatar` frames the avatar (the way to check avatars).** Free/earth shot modes put the camera at `--at` while the avatar sits at the map anchor — they usually don't coincide, so the avatar is off-screen (this caused a lot of "the avatar isn't rendering" confusion when it was fine, just elsewhere). In avatar mode `place_camera` puts the follow camera on the avatar: `--at` altitude = follow distance, `--look` = (elevation°, azimuth°). Use it at scale 1.0 instead of giant-scaling to hunt the avatar in a free-cam shot.
- **The routing graph must be noded, and skipping it fails silently** (ADR-037, `systems/routing.rs`). Tiles at a zoom share one integer lattice, so a way clipped into two tiles yields the *same* global node in both — stitching needs no tolerance. What it does need is `RoadGraph::finish`: Shortbread merges ways by attribute and generalises each merged feature independently, so a side street's tee is an interior vertex of the main road's line and simplification is free to drop it. That leaves 14% of nodes (measured on the Groningen fixture) lying exactly *on* a segment while being a vertex of neither, and the largest connected car component covers only 58% of usable nodes — ordinary cross-town routes then return `NoPath` with nothing in the log. `finish` splits every segment at the nodes on it (~0.55 m) and takes that to 92%. **Snapping must target the routable core, not the nearest node** (`RoadGraph::routable_core`, the largest SCC of the mode-filtered graph): a car snapped onto a genuine one-way residential street (`oneway=yes` in the source, so the parse was right) whose forward-reachable set was a three-node dead-end pocket, and the cross-town route returned `NoPath` — while the same pair routed for bikes, which ignore one-ways. `route` snaps both ends into the largest SCC so a path is guaranteed to exist between them; a stub a few metres off the through-road is never a valid start. **Range is capped by tiles, reported as distance:** `corridor_tiles` refuses a route whose capsule exceeds `MAX_ROUTE_TILES`, phrased as `TooFar { km }` (~`MAX_ROUTE_KM`); the capsule half-width grows with `√direct`, not linearly, or a long route gets both long and wide and the tile count blows up (a cross-province query once asked for ~700 tiles). Turn instructions are likewise only emitted at real junctions (≥3 usable neighbours) with the angle measured over a 25 m window either side; per-vertex classification narrates a curving street as "bear right, bear left, bear right…". **The drawn route line is a coplanar-ground problem, not a mesh problem.** Two independent failures hid it completely and neither logged anything: the ribbon triangles were wound downward (backface-culled — `navigation::ribbon_faces_up` pins the winding now), and at `ROUTE_Y = 0.12` the line lost the depth fight with the ground stack and rendered as horizontal bands. It sits at 0.5 m with `depth_bias` ~20 (the surfaces top out at 4). `ShotMetrics` carries `route_meshes`/`route_rendered` so "not spawned" and "spawned but not rendered" are one `--dump-state` apart; when both are non-zero and nothing is visible, widen the line and colour it magenta for one capture — that separates "placed wrong" from "depth-fighting" in a single frame. **The line uses bevel joins, not mitre** (`navigation::ribbon`): a mitre offset runs to infinity at a sharp turn, and on the sub-metre segments the noded graph emits it shot a spike across a junction (a "giant arrowhead") — each segment is now a butt-ended quad plus corner-fill triangles, which cannot spike or pinch, and the material is `cull_mode: None` so winding can never hide it again. **The full-width ribbon covers only the road path**, not the raw start/end pins: the pin-to-road gap is a thin connector, so a pin that snaps across a canal to reach a drivable road draws a thin lead-in, not a fat line through water. Routing needs a **vector** `VIBE_TILE_URL`; against a raster template the planner says so. `cargo test -p viberfox --bin viberfox solves_a_real_route -- --ignored --nocapture` prints a full turn list; `VIBE_ROUTE_FROM`/`VIBE_ROUTE_TO` ("lat,lng") override the `--ui route` demo seed to reproduce a specific route headlessly.
- **Water is two unrelated shaders, one per tier — and the local one is the only place a wave exists.** Local (`systems/water.rs`, `shaders/water.wgsl`): an `ExtendedMaterial<StandardMaterial, _>` over the vector surface meshes. Extending rather than replacing is the whole trick — the sun, shadow cascades, fog and the atmosphere's environment map already reach every standard material, so the sea reflects *this* sky through the day/night cycle for free. The polygons are flat (earcut on a canal emits two triangles), so every wave lives in the **normal**, never in a vertex. Three things are load-bearing: (1) the wave field is evaluated in **UV0 = metres from the map anchor**, not `world_position` — under `big_space` the origin jumps a whole grid cell as you move, which would snap the phase of the entire sea at once; (2) each octave carries its own **distance fade**, and the detail it drops is paid back as *roughness*, or a 20 cm ripple a kilometre out flickers as white noise under any camera motion; (3) sea state comes from `WeatherState`'s wind, and steepness/wavelength/speed/foam must move **together** — a longer wave at a lower steepness reads as a flat sheet. Whitecaps mask on *chop riding a big crest* (the product of the long and short octave bands); a threshold on total height paints continuous ribbons down every crest instead of spray.
- **Water's shore field is why it is the one surface group with a material per cell.** Earcut only ever emits the polygon's own boundary vertices, so a per-vertex distance-to-bank would be identically zero — the answer has to come from the shape's interior, i.e. a raster. `vector_tiles::water_shore_field` rasterises the water layers and runs an exact Felzenszwalb distance transform (seeded with a large *finite* value, never `∞`: the envelope subtracts one sample from another and `∞ − ∞` is a NaN that poisons a scanline). Distances are measured only against land **inside the tile** — that *is* the truth for the interior, since in MVT land is the absence of a water polygon, and padding the raster with a land border would draw a shoreline down the middle of every river that crosses a tile edge. That one number buys shelter (a 12 m canal never gets the open river's swell — the single most artificial thing flat water can do), a shallow tint, and a foam line that laps against the bank. It also means the shared-material rule the other groups follow cannot apply here, so the wind parameters live in `WaterSeaState` and new cells read them at spawn (same contract as `GlobeSurfaceAlbedo`).
- **The globe's ocean is a BRDF split, not water** (`systems/globe_ocean.rs`, ADR-025 tier). From orbit a wave is orders of magnitude below a pixel; what makes an ocean read as an ocean from space is that it is dark and glossy where land is matte, plus the sun's glint. So it is one mask per texel driving roughness/specular/tint on the globe's own tile materials, and it shares nothing with `water.wgsl`. The mask rides in the **alpha channel of the tile imagery** (opaque tiles, so alpha was free) — no extra texture, binding or upload, and the mip chain averages it so coastlines soften with distance instead of aliasing. Read it out of `pbr_input.material.base_color.a` **before** `alpha_discard`, which flattens alpha to 1 for an opaque material. The `has_mask` flag is not optional: an unbound base-colour texture samples as opaque white, so without it the backstop sphere (no imagery at all) turns the entire planet into ocean. Vector sources get an exact mask from geometry; raster ones are classified by `blue − red`, which separates map water and real sea from land where a hue test trips over the green end. Everything ramps on `space_factor` alongside the albedo — the flat map tiles below the boundary are matte and full-brightness, so anything arriving early is a step in the middle of a climb. `VIBERFOX_NO_OCEAN=1` strips it for bisecting.
- **Wayland + GLES flicker:** force Vulkan with `WGPU_BACKEND=vulkan cargo run -p viberfox`.
- **Database migrations:** copy `data/world.db` before schema upgrades; `refinery` migrations run automatically on `vibe_sim` startup (ADR-013).
- **`fast-dev` feature** uses Bevy dynamic linking — never use it for `--release` or distributable binaries.

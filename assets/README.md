# Client runtime assets

`viberfox` loads these from the **workspace root** `assets/` directory (see `AssetPlugin` in `crates/viberfox/src/main.rs`).

- **`models/animated/Fox.glb`** — Bevy’s sample fox mesh and animations (idle / run). Source: [bevyengine/bevy `assets/models/animated`](https://github.com/bevyengine/bevy/tree/main/assets/models/animated).

If the file is missing, restore it with:

```bash
mkdir -p assets/models/animated
curl -fsSL -o assets/models/animated/Fox.glb \
  https://raw.githubusercontent.com/bevyengine/bevy/main/assets/models/animated/Fox.glb
```

## Avatar packs (`models/avatar/`) — not committed

The player avatars use three **CC0** Quaternius packs (~119 MB total), gitignored
and fetched by hand from itch.io (their download is JS-gated, so there's no direct
`curl`). The avatar chooser falls back to the fox when they're absent, so the
client still runs without them.

Download the **glTF (Godot/UE)** exports of each and extract as shown:

| Pack | itch.io | Extract into |
|------|---------|--------------|
| Universal Base Characters | <https://quaternius.itch.io/universal-base-characters> | `models/avatar/base/` |
| Modular Character Outfits – Fantasy | <https://quaternius.itch.io/modular-character-outfits-fantasy> | `models/avatar/outfits/` |
| Universal Animation Library | <https://quaternius.itch.io/universal-animation-library> | `models/avatar/anims/` |

Flatten each pack's glTF folder into the target dir (the `.gltf` reference their
`.bin`/textures by bare filename), e.g.:

```bash
unzip -j 'Universal Base Characters[Standard].zip' \
  'Universal Base Characters\[Standard\]/Base Characters/Godot - UE/*' \
  -d assets/models/avatar/base
unzip -j 'Modular Character Outfits - Fantasy[Standard].zip' \
  'Modular Character Outfits - Fantasy\[Standard\]/Exports/glTF (Godot-Unreal)/Outfits/*' \
  -d assets/models/avatar/outfits
unzip -j 'Universal Animation Library[Standard].zip' \
  'Universal Animation Library\[Standard\]/Unreal-Godot/UAL1_Standard.glb' \
  -d assets/models/avatar/anims
```

The expected files (see `avatar::resolve_avatar_model`): bodies
`base/Superhero_{Male,Female}_FullBody.gltf`; outfits
`outfits/{Male,Female}_{Peasant,Ranger}.gltf`; clips `anims/UAL1_Standard.glb`.
All three packs share one `Armature → root → pelvis` rig, so the UAL clips drive
the bodies and outfits (see `avatar::bind_external_animations`).

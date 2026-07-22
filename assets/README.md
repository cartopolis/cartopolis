# Client runtime assets

`viberfox` loads these from the **workspace root** `assets/` directory (see `AssetPlugin` in `crates/viberfox/src/main.rs`).

- **`models/animated/Fox.glb`** — Bevy’s sample fox mesh and animations (idle / run). Source: [bevyengine/bevy `assets/models/animated`](https://github.com/bevyengine/bevy/tree/main/assets/models/animated).

If the file is missing, restore it with:

```bash
mkdir -p assets/models/animated
curl -fsSL -o assets/models/animated/Fox.glb \
  https://raw.githubusercontent.com/bevyengine/bevy/main/assets/models/animated/Fox.glb
```

## Avatar characters (`models/avatar/characters/`) — not committed

The player avatars are **CC0** Quaternius *Ultimate Modular* characters, gitignored
(~31 MB). Each is a self-contained glTF (embedded buffers + solid-colour materials)
that includes its own 24-clip animation set — one complete, clothed, self-animated
character per file. The chooser falls back to the fox when they're absent.

Files expected by `avatar::resolve_avatar_model` (style ∈ {Casual, Suit,
Adventurer, Worker, Punk}):

```
models/avatar/characters/{Man,Woman}_{Casual,Suit,Adventurer,Worker,Punk}.gltf
```

Source (Quaternius, CC0): *Ultimate Modular Men* and *Ultimate Modular Women* →
`Individual Characters/glTF/`. Download those glTFs and rename to `Man_<Style>` /
`Woman_<Style>` (the men's "Casual Hoodie" is used for `Man_Casual`). More styles
exist in the packs (Adventurer, King, Punk, Space Suit, Swat, Farmer, Beach /
Formal, Medieval, SciFi, Soldier, Witch …) — add a `ClothingTheme` variant + a
`resolve_avatar_model` arm to expose them.

> The earlier approach (Universal Base Characters + Modular Character Outfits –
> Fantasy + Universal Animation Library, under `base/`, `outfits/`, `anims/`) is
> retired — the free base pack only shipped the broad *Superhero* body, which
> poked out of the Regular-sized outfits. Those dirs, if present, are unused.

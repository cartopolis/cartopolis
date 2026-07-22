# Client runtime assets

`viberfox` loads these from the **workspace root** `assets/` directory (see `AssetPlugin` in `crates/viberfox/src/main.rs`).

## Avatar characters (`models/avatar/characters/`) — committed via Git LFS

The player avatars are **CC0** Quaternius *Ultimate Modular* characters (~62 MB,
20 glТFs). They are committed to the repo through **Git LFS** (`.gitattributes`
tracks `models/avatar/characters/*.gltf`), so a normal clone checks them out
automatically — **you need `git lfs` installed** (`git lfs install`), otherwise you
get pointer files and the chooser falls back to the fox.

Each file is a self-contained glTF (embedded buffers + solid-colour materials) with
its own embedded 24-clip animation set and 4 slot-named meshes (`*_Head`, `*_Body`,
`*_Legs`, `*_Feet`) — see `avatar::CHARACTER_STYLES` for the catalog. Files:

```
models/avatar/characters/{Man,Woman}_<Style>.gltf
```

Source (Quaternius, CC0): *Ultimate Modular Men* / *Women* → `Individual
Characters/glTF/`, renamed to `Man_<Style>` / `Woman_<Style>`. To add a style: drop
its glTF in (LFS tracks it automatically) and add a `man!`/`woman!` row to
`avatar::CHARACTER_STYLES`.

> The earlier approach (Universal Base Characters + Modular Character Outfits –
> Fantasy + Universal Animation Library, under `base/`, `outfits/`, `anims/`) is
> retired — the free base pack only shipped the broad *Superhero* body, which
> poked out of the Regular-sized outfits. Those dirs, if present, are unused.

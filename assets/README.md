# Client runtime assets

`viberfox` loads these from the **workspace root** `assets/` directory (see `AssetPlugin` in `crates/viberfox/src/main.rs`).

## Everything binary lives in Git LFS

`.gitattributes` tracks binary assets **by extension, anywhere in the repo** —
models (`.gltf` `.glb` `.bin`), images (`.png` `.jpg` `.ico` `.ktx2` `.hdr` …),
fonts (`.ttf` `.otf` `.woff2`), audio, and the vector-tile test fixtures. Dropping a
new asset in needs no edit to `.gitattributes`; `git add` puts it in LFS.

**You need `git lfs` installed** (`git lfs install`) before cloning. Without it every
one of these is a 130-byte pointer file, and the failure is quiet in both
directions: the avatar chooser falls back to the fox, *and* the four assets that are
`include_bytes!`-ed at compile time (the brand font, the app icon, `sky/bright_stars.bin`,
the `.mvt` fixtures) get the pointer compiled into the binary — so `cargo build`
succeeds and the app dies later. CI checkouts set `lfs: true` for that reason.

## Avatar characters (`models/avatar/characters/`)

The player avatars are **CC0** Quaternius *Ultimate Modular* characters (~62 MB,
20 glТFs).

Each file is a self-contained glTF (embedded buffers + solid-colour materials) with
its own embedded 24-clip animation set and 4 slot-named meshes (`*_Head`, `*_Body`,
`*_Legs`, `*_Feet`) — see `avatar::CHARACTER_STYLES` for the catalog. Files:

```
models/avatar/characters/{Man,Woman}_<Style>.gltf
```

Source (Quaternius, CC0): *Ultimate Modular Men* / *Women* → `Individual
Characters/glTF/`, renamed to `Man_<Style>` / `Woman_<Style>`. To add a style: drop
its glTF in and add a `man!`/`woman!` row to `avatar::CHARACTER_STYLES` — LFS picks
it up from the `*.gltf` pattern with no further setup.

> The earlier approach (Universal Base Characters + Modular Character Outfits –
> Fantasy + Universal Animation Library, under `base/`, `outfits/`, `anims/`) is
> retired — the free base pack only shipped the broad *Superhero* body, which
> poked out of the Regular-sized outfits. Those dirs, if present, are unused.

# Client runtime assets

`cartopolis` loads these from the **workspace root** `assets/` directory (see `AssetPlugin` in `crates/cartopolis/src/main.rs`).

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

## Avatars (`models/avatar/human/`)

The player avatars are built at run time from one MakeHuman body pack
(`default.humanpack`, ~14 MB) and its textures (`textures/`, ~10 MB): the base
mesh, the shape targets the designer's sliders blend, the rig, the clothes and
hair, and the motion-capture clips. All of it is CC0 (MakeHuman) or CC BY 4.0
(100STYLE) or free for any use (CMU motion capture); `LICENSE-THIRD-PARTY.md`
has the credits.

Nothing here is edited by hand. `human-pack` (`crates/human`) writes both from
the MakeHuman and motion-capture sources — `docs/notes/makehuman-avatars.md` has
the command and what each choice in it is for.

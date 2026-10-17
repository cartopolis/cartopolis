---
topic: content
triggers: [assets.rs, library.rs, model_assets.rs, model_library.rs, "PrimDto::model", glb, glTF, inventory, "shared library", upload, licence, CC0, "CC BY", Quaternius, Kenney, "asset hash", sim://, model_limits.rs, crates/atlas/web/src/lib/library.ts, crates/simulator/src/config.rs, crates/simulator/src/main.rs, resolved_api_base_url, api_base_url, derive_api_base]
updated: 2026-10-17
---

# Models a person uploads, and the library they can share them to

Until protocol 22 the only object anybody could build was a prim: one of five
procedural shapes with the Second Life deformation set and a texture out of an
operator-managed directory. There was no way to put a *model* in the world.
`systems::lod3` imports an IFC building and `assets/models/avatar` ships twenty
characters, but neither is content a person can add.

The three pieces are `simulator::assets` (the file store and what may go in it),
`simulator::library` (the database rows and the HTTP surface), and
`systems::model_assets` + `systems::model_library` on the client.

## The file is named after its own bytes, and that forces the schema apart

A model is stored as `data/assets/<blake2s-hex>.glb`. Two consequences, and the
second is the one that shapes everything else.

An immutable name means `/api/assets/{hash}.glb` can be cached forever — by the
client's own `platform::storage`, by a browser and by any proxy — with no
revalidation and no staleness question. That is why the download route sets
`immutable` and why the client's cache never expires.

And it means **one file can have two uploaders**. Alice and Bob both find the same
CC0 bench and upload it; the server keeps one file. So "who uploaded this", "what
is it called" and "is it shared" cannot be columns on the file, and
`V16__model_assets.sql` is four tables rather than one:

| table | what it is |
|---|---|
| `assets` | the bytes. Immutable, no owner, no name |
| `asset_claims` | one row per uploader: what licence they said it was under |
| `inventory_items` | a person's named reference to a file — renameable, disposable |
| `library_entries` | the catalogue: one row per file, what the atlas web UI curates |

Folding any two together breaks a case that happens on the first day: Bob deleting
his copy must not blank Alice's, and must not lose the category a curator set.

## The licence question is not a formality

**Using a model in a world and handing copies of it to other people are different
acts under most asset licences.** This is not hypothetical. Quaternius — whose
characters *this project's own avatars* are — moved off CC0 on 2026-08-28 to a
licence that permits the first and forbids the second in as many words
(`LICENSE-THIRD-PARTY.md` has the dates; our files are grandfathered, and nothing
new may be taken from there).

So the licence is asked at **upload**, when the person still has the answer in
front of them, and recorded per uploader. Four answers: `own`, `cc0`, `cc-by`,
`other`. `other` is uploadable and usable and **may never be shared or given
away** — that is the whole reason there are four values and not a boolean.
`cc-by` requires a credit line at upload, and that line is shown beside the model
everywhere it appears, because that is what the licence asks for.

The conservative direction is deliberate. Sharing under a licence that turns out
to forbid it is a wrong done to an author that cannot be taken back once other
people hold copies; refusing to share something that would have been fine costs
one person one dialog.

## Refusals are measured, never estimated

`assets::inspect` parses the upload once and counts what is in it: triangles
(topology-aware — a strip of *n* vertices is *n−2* triangles, and reading every
mode as a list over-counts a strip threefold and refuses honest models), nodes,
and each texture's dimensions read from its own header. Only the byte ceiling is
applied before parsing, and that is not an estimate of anything — it is the size
of the read.

This follows [mvt-capacity-guard.md](mvt-capacity-guard.md), which is the standing
warning: a guard that predicted a cost from the shape of a file before parsing it
refused real content and was reverted the same day.

The ceilings are `MAX_ASSET_BYTES` 16 MB, `MAX_TRIANGLES` 250,000, `MAX_NODES`
4,000 and `MAX_TEXTURE_EDGE` 2048 px, and they live in
`cartopolis_core::model_limits` because three surfaces *say* them before anybody
uploads: `assets.rs` enforces them, `model_library::upload_format_hint` builds the
client's hint from them, and the web UI's `UPLOAD_LIMITS`
(`crates/atlas/web/src/lib/library.ts`) is a TypeScript copy that a test in
`model_limits.rs` pins equal. They were three hand-written copies of four numbers
before that.

**Refusal strings are written to be shown** to the person who tried — they say
what was measured and what the limit is — so the Models panel prints the
server's message rather than replacing it with a generic failure.

**Self-contained glTF only, and the store holds one form of it.** What is refused
is a file that points *outside itself*: fetching whatever an uploader's JSON names
would make the server issue requests of a stranger's choosing — from inside the
private network that also holds nominatim, overpass and Forgejo — and storing it
unfetched would mean a model nothing can draw. So a `Uri` on a buffer or an image
is a refusal, in either spelling of the format.

A `.gltf` whose buffers and images are **data URIs** has no such problem, and
since 2026-09-11 it is accepted: `assets::repack_gltf` decodes them, merges every
buffer into one blob, moves each image into a `bufferView` with its media type,
and writes a GLB. Repacking rather than storing two formats is what keeps this to
one decision — the store, `/api/assets/{hash}.glb`, the client's `sim://` reader
and Bevy's loader all still see one kind of file. It was excluded at first on the
grounds that base64 is a third larger than the bytes it carries, which is true and
is an argument about *the wire*, not about what can be stored; and it made
Blender's three-entry export menu a coin flip, since "glTF Embedded" is a perfectly
self-contained file that was refused.

Two consequences. The repack is always **smaller** than the upload, so the byte
ceiling applied to the read still bounds what is stored. And the same model sent
both ways is **two files** — the hash is of the stored bytes, and deduplication
has never been more than identical bytes.

**FBX is not accepted.** Nothing in Rust parses it and Bevy has no loader, so it
would mean Blender headless or Autodesk's SDK on the VPS, a large C++ parser fed
attacker-controlled files, and a conversion that is lossy and ambiguous about
units, axis and materials. Blender's glTF exporter is the path everyone already
takes to get FBX into a web engine, and the refusal names the format to export.

Validation happens on the server because it happens **once, for everyone**. A
client that skips it cannot be detected, and the first anyone would hear about a
model nothing can draw is a phone dying in the street.

## The bytes are HTTP; the reference is the wire

`PrimDto::model` is an asset hash — 32 bytes of hex in a snapshot that goes to
every observer twenty times a second. The file itself travels over HTTP, for the
reason account management does (`hosted-simulator.md`): an authoring or operator
action must not run through the path a movement intent takes, and a 12 MB upload
must not sit in front of everybody's next snapshot.

**The client cannot work out where that HTTP surface is**, which is why
`ServerHelloAck::api_base_url` exists. The wire arrives over a TCP port or a
WebSocket path; the HTTP listener is a separate bind an operator points a reverse
proxy at. On the deployed sim they are different hostnames, and against a
development sim the HTTP port is whatever `--http-listen` said. A server that says
nothing leaves the client to derive `https://<host>` from the address it was given,
which is right when the wire and the API share a hostname — and
`model_assets::derive_api_base` answers `None` for loopback rather than guessing,
because a wrong base fails as a network error instead of as "this server did not
say".

### A dev sim had no model store at all, and said the wrong thing about it

Fixed 2026-09-11. `derive_api_base` answering `None` for loopback is right — the
HTTP port is a separate `--http-listen` and nothing in the game port implies it —
but the consequence was that the *documented* dev loop (`cargo run -p
cartopolis_simulator`, sign in to `127.0.0.1:4747`) had no model API, and the
Models panel told a signed-in person to sign in. Every report of "uploading does
nothing" traced back to this, not to the upload.

Three parts, and the shape of each matters:

- The **binary** defaults `--http-listen` to `127.0.0.1:4748` when nothing asked
  (`main.rs`), and falls back to off with a warning when that port is taken — an
  `--http-listen` that *was* asked for still fails the boot, because a deployment
  that cannot bind its listener must not come up looking healthy. `SimConfig::default`
  still says `None`, so the tests and every embedded `serve` are untouched.
- `SimConfig::resolved_api_base_url` answers an unset `api_base_url` with the
  listener's own address **only when the listener is loopback**. The hosted sim
  binds `172.17.0.1:4748` behind Caddy: telling a browser that address would
  replace a working derivation with one that cannot resolve.
- The panel separates "not signed in" from "this server has no model store", since
  the second is the operator's to fix and the first is not.

### Sharing is asked at upload now

The licence question was already asked there; the *use* of the answer was two
visits away — upload, wait for the list, find the row, press Share. A "Share it
with everyone on this server" checkbox sits under the licence radio group, is
absent (and forced off) for `other`, and the client chains
`POST /api/inventory/{id}/share` onto the upload's answer. The two server routes
stay separate: uploading something you may not redistribute is a legitimate act,
and the refusal still lives on the server.

### Deployment: `cartopolis.org` proxies three prefixes, not `/api/*`

Added to the Caddy block on 2026-09-05. Alongside `/ws`, the host now proxies
`/api/assets`, `/api/inventory` and `/api/library` (each bare and with `/*`) to
the same upstream, `172.17.0.1:4748`.

**Not `/api/*`,** and that is the point: the same prefix carries `/api/admin/*` —
accounts, reports, claims, curation — which belonged on `web.cartopolis.org`
while the operator GUI lived there, and had no business being reachable from the
page every player loads. (That hostname is retired: the web UI is at
`cartopolis.org` since 2026-09, on this same host.) Anything else under `/api` on this host falls through to the file
server and 404s.

The two 404s are worth telling apart when checking it. A path that reached the
simulator carries a `via: 1.1 Caddy` header (added by `reverse_proxy`); one that
fell through to the file server carries `server: Caddy` instead. Both are `404`
with an empty body, so the status alone proves nothing:

```
curl -sS -D - -o /dev/null https://cartopolis.org/api/library | grep -i '^via:'
```

Both prefixes are listed bare *and* with a wildcard because Caddy's `path`
matcher is exact per token: `/api/inventory*` would also match
`/api/inventoryfoo`, and `/api/inventory/*` alone would miss the listing itself.

## Loading it on the client is an asset *source*, not a download

`AssetServer` loads from a path, and the glTF loader wants a `LoadContext` to pull
a model's dependencies through; there is no public "here are some bytes, make me a
scene". Writing the file somewhere and loading that path fails on the two
platforms that most need this — Android's default reader reads out of the APK and
nothing else, and a browser has no filesystem.

So the fetch is registered *as* a place assets come from: `sim://<hash>.glb`, one
`AssetReader` over `platform::storage`'s cache with the server behind it, and the
glTF loader unmodified. It must be registered **before** the plugin group —
`AssetPlugin::build` consumes the registered sources, and one added afterwards is
silently never used.

A prim wearing a model keeps `PrimMesh` and loses `Mesh3d`: the marker is what
picking, selection and the context menu query on, so a model prim stays pickable
with no edit at any of those sites. What it is picked against is the same unit box
the procedural shapes use.

## Curating it

The operator half is a **Models tab in the atlas web UI**
(`crates/atlas/web/src/components/pages/library`), talking to
`/api/admin/library` and `/api/admin/assets`. It lists the catalogue
with the unreviewed entries first, edits a name, a category and a credit line,
takes a model down, and shows the stored files with what still refers to each.

That is a **hand-synced seam**, like the map data formats in
[where-the-map-data-is-built.md](where-the-map-data-is-built.md): the console's
`LibraryEntry` and `StoredAsset` in `crates/atlas/web/src/types.ts` are
hand-written TypeScript describing what `library::CuratorEntry` and
`library::StoredAsset` serialise here, and nothing but a person keeps them in
step — the console being in this repository (it was a separate repository until
2026-09) does not change that.

Categories are a fixed list on the console side because the in-world picker groups
by them, and a curator typing `furniture ` once makes a second category nobody can
find. They are stored as free text on the server, which is the looser of the two —
worth knowing before relying on the set being closed.

## What is not done

- **No thumbnails.** The simulator cannot render — no GPU, and deliberately no
  Bevy — so a picture has to come from somewhere else: the uploading client (which
  has a renderer and an offscreen path already, in the shot harness), or a curator
  attaching one from the atlas web UI. Either is a second asset kind to store, serve and
  moderate, which is why neither was done with the first version. The picker lists
  a name, a category and a triangle count and is usable without.
- **No collider — but no prim has one.** A model prim is picked against its unit
  box and walked through, and so is every procedural prim: nothing in the client
  feeds a prim into `collision::BuildingColliders`, which carries building
  footprints and street furniture only. So this is parity rather than a gap models
  introduced, and fixing it is a change to *prims*, not to models. The obvious
  shape when somebody does it is the flattened outline, the way `lod22` hulls a
  building.
- **No level of detail.** A model is refused if it is too heavy rather than
  simplified.
- **No starter library.** Kenney's packs are still CC0 (checked 2026-09-05) and
  are the obvious first content, uploaded through the same route as anybody's.
  Nothing has been.
- **Android cannot upload.** A document picker needs an activity result this APK
  has nowhere to receive (`android-build.md`), so the button is hidden there.
  Browsing and placing work everywhere.

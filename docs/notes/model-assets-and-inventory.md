---
topic: content
triggers: [assets.rs, library.rs, model_assets.rs, model_library.rs, "PrimDto::model", glb, glTF, inventory, "shared library", upload, licence, CC0, "CC BY", Quaternius, Kenney, "asset hash", sim://]
updated: 2026-09-05
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
| `library_entries` | the catalogue: one row per file, what cartopy curates |

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

**Binary glTF only.** A `.gltf` names its buffers and images as separate files, so
accepting one would mean either fetching whatever it names — an upload that makes
the server issue requests of the uploader's choosing — or storing a model that
cannot be drawn. Every buffer must be the GLB's own chunk and every image a view
into it; a `Uri` on either is refused. A data URI is refused for the same reason
plus one more: base64 in the JSON is a third larger than it needs to be and is a
way past a limit measured on the file.

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

### Deployment: `cartopolis.org` proxies three prefixes, not `/api/*`

Added to the Caddy block on 2026-09-05. Alongside `/ws`, the host now proxies
`/api/assets`, `/api/inventory` and `/api/library` (each bare and with `/*`) to
the same upstream, `172.17.0.1:4748`.

**Not `/api/*`,** and that is the point: the same prefix carries `/api/admin/*` —
accounts, reports, claims, curation — which belongs on `web.cartopolis.org` where
the operator GUI is, and has no business being reachable from the page every
player loads. Anything else under `/api` on this host falls through to the file
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

The operator half is a **Models tab in cartopy** (`src/components/pages/library`),
talking to `/api/admin/library` and `/api/admin/assets`. It lists the catalogue
with the unreviewed entries first, edits a name, a category and a credit line,
takes a model down, and shows the stored files with what still refers to each.

That is a **sixth hand-synced seam** between the two repositories, alongside the
five in [where-the-map-data-is-built.md](where-the-map-data-is-built.md):
cartopy's `LibraryEntry` and `StoredAsset` in `src/types.ts` describe what
`library::CuratorEntry` and `library::StoredAsset` serialise here, and nothing but
a person keeps them in step.

Categories are a fixed list on the cartopy side because the in-world picker groups
by them, and a curator typing `furniture ` once makes a second category nobody can
find. They are stored as free text on the server, which is the looser of the two —
worth knowing before relying on the set being closed.

## What is not done

- **No thumbnails.** The simulator cannot render — no GPU, and deliberately no
  Bevy — so a picture has to come from somewhere else: the uploading client (which
  has a renderer and an offscreen path already, in the shot harness), or a curator
  attaching one from cartopy. Either is a second asset kind to store, serve and
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

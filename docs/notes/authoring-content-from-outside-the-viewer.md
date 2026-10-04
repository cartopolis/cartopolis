---
triggers:
  - working on `crates/simulator/src/authoring.rs` or `tools/author.py`
  - "how do I make a prim without opening the client"
  - adding an operator action, and wondering whether it goes on the wire
  - building a world from a file, a fixture or a migration
  - a script needs to reach something the `vf` API cannot
updated: 2026-08-29
---

# Building the world without a viewer

There are two ways to author a prim, and they are for different people.

**The wire** (`CreatePrim` / `UpdatePrim` / `SetPrimScript`) is a *player*
building something in front of them. It is checked against the world's prim and
script budget, it needs a live session, and it is driven by the create dialog,
the gizmo and the script editor.

**`/api/admin/prims`** (`crates/simulator/src/authoring.rs`) is an *operator*
building something they may not be looking at. It needs an admin token and
nothing else — no GPU, no window, no session.

## Why the second one had to exist

Every in-world tool needs a person at a screen. That made a whole class of
ordinary things impossible: building a world from a file, restoring one, keeping
it in a repository, reviewing a change to it in a diff, standing a test fixture
up, or having an agent with no display set anything up at all. None of those is
exotic and none of them had an answer.

It is HTTP for exactly the reason account management is (`crates/simulator/src/http.rs`'s
module note): these are operator actions, and putting them on `NetMessage` would
run an authorisation decision through the same path as a movement intent *and*
move every client's protocol version whenever the authoring surface changed.

## Three things about it that are not obvious

**It goes through `SimWorld`, not through SQLite.** The world is the authority
and holds prims in memory; a row written behind its back is invisible until a
restart. So every handler takes the world's write lock — and drops it before
doing anything else, because the tick loop wants the same lock twenty times a
second.

**Absent is not null.** A `PATCH` naming only `color` changes only the colour;
`{"particles": null}` takes the emitter *off*. Serde cannot tell those apart with
`Option<Option<T>>` alone (the outer `Option` swallows the null), which is what
the `present` deserialiser is for. Without the distinction, clearing a field
would need a second boolean beside every nullable one — and, worse, an
`UpdatePrim`-shaped API would silently wipe an emitter every time somebody
changed a colour. That is not hypothetical: the gizmo and the edit dialog send a
*whole* prim, and both had to be taught to carry `particles` through, or dragging
a burning prim put the fire out.

**An emitter is described by what differs from a preset**, on both surfaces and
**in one vocabulary**. A JSON body naming `color_start` alone is a blue flame with
the fire preset's other eleven fields; so is
`particles{ color_start = {0,0.4,1} }` — the same twelve names either way.

They were not, for a day: the script API took `spread`, `radius`, `size`,
`end_size`, `color`, `end_color` while the HTTP body took `spread_deg`,
`source_radius`, `size_start`, `size_end`, `color_start`, `color_end`. Six of the
twelve fields had two names depending on which surface set them, which is a thing
a person has to translate every time they move between a script and a `curl`.
The longer spelling won for two reasons: the pairs are symmetric (`size_start`
beside `size_end`, where `size` beside `end_size` is not), and `spread_deg`
carries its unit in a codebase that has been bitten by degrees and radians
before. `scripting::tests::the_two_authoring_surfaces_use_one_vocabulary` reads
the HTTP names out of `ParticleSpec`'s own `Serialize` and the script names out
of the parser's own error, so neither list is hand-maintained and they cannot
drift apart again. A script using an old name is refused with its new one rather
than with the whole list.

Two fields are HTTP-only and meant to be: `burst`, because a script says which it
wants by calling `particles` or `particle_burst`, and `sequence`, which is
derived from the prim so a re-armed blast cannot reuse a number a client has
already played.

The leniency itself lives in `authoring::ParticleSpec` rather than on
`ParticleEffect`, and it has to: `ParticleEffect` is postcard-encoded in every
`WorldSnapshot`, postcard is not self-describing, and giving its fields serde
defaults or `Option`s would change what the bytes mean and desync every client.

## What it deliberately does not do

**Change a prim's shape.** `update_prim` does not carry the column, and nothing
has asked for it; refusing beats accepting it and silently drawing a cube.

**Tell you whether your script compiled.** The host compiles on its own thread
and answers with a `ScriptOutput` on the *wire*. Blocking an HTTP handler on a
script's load budget would be the wrong trade, so a 200 here means "stored", and
the script console in a connected viewer is still the only place the answer
appears. `PUT /prims/{id}/script` says so in its own reply.

## Content the server carries

`POST /api/admin/content/{name}` installs a named piece of world the *simulator*
defines — `crates/simulator/src/content.rs`, with the Luau `include_str!`-ed from
`crates/simulator/content/`.

It started the other way round: a `.lua` file in the repository and a Python
script that read it, built a prim specification and posted it. That is wrong
twice over. The definition of a demo — which prims, where, what shape, which
script — is a statement about this world, so it belongs to the thing that owns
the world; and **a deployed simulator has no repository beside it**, so content
kept in the tree is content a running server cannot install.

Two rules it follows.

A pack is a *canned request*: each prim is a `PrimSpec`, the same type an HTTP
body deserialises into, and installing one runs the same handler an author's
`POST` does. There is one code path that creates a prim, so a pack cannot drift
from it or acquire abilities the API does not have.

Installing twice **replaces**, matching on `prims.origin`
(`V14__prim_origin.sql`). The first version matched the prim *names* the pack
declares and the pack broke it itself: the beacon's script renames it to whatever
its label currently says, so a reinstall found nothing called "Beacon", left the
old one standing and added another — measured, three prims after two installs. A
script may rewrite any field a client can see; provenance is a fact about the row
and is not one of them. `origin` is deliberately not in `PrimDto`: no client has
any use for it.

## Import, export and transfer are owner-scoped by default

`GET /api/content/export`, `POST /api/content/import` and
`POST /api/content/transfer` are **not** under `/api/admin`. They authenticate
with `require_session` — any signed-in account — and the default scope is
`prims.owner = me` (schema V15).

The default is the security property. An ordinary account cannot read somebody
else's build out of this route by *any* argument it supplies. Reaching further is
an explicit query — `?owner=<username>` or `?all=true` — and both are refused
without an admin flag.

**An admin may always export**, including the whole world. That is not a hole: an
operator can already read `world.db` over ssh and list everything through
`/api/admin/prims`, so refusing here would have bought no confidentiality and cost
the one thing this route is actually good for — taking a copy of the world before
changing it. `?all=true` is also the only way to reach prims with **no** owner:
everything built before V15 and everything a content pack installs. Those belong
to the server, and the server is what an operator administers.

A username that does not exist is a 404 rather than an empty document, because an
empty document reads as "they have built nothing".

## Transferring ownership

`POST /api/content/transfer` takes `{prims: [id], to: "<username>"}`. Two callers
may do it, for different reasons: the **owner**, because giving your own work away
is yours to decide, and an **admin**, because somebody has to be able to re-home a
build whose author has gone. An unowned prim can only be moved by an admin —
there is no owner to consent.

**It is all-or-nothing.** Every prim is permission-checked before anything is
written, so a request naming one you own and one you do not moves neither. A
partial transfer is the worst outcome available, because the response gives the
caller no way to tell which half happened.

The target must be an *enabled* account. Handing a build to a disabled one puts it
somewhere nobody can reach, which is a way to lose work rather than to move it.

Ownership is recorded on the wire's `CreatePrim` path only. The admin API and the
content packs leave it NULL on purpose: those are the server building, not a
person. It is deliberately **not** in `PrimDto` — no client has a use for it, and
putting an account id on an object every observer receives twenty times a second
is a privacy decision nobody asked for.

## Ownership gates editing, and an admin overrides it

`UpdatePrim`, `DeletePrim` and `SetPrimScript` are refused unless the caller owns
the prim or is an admin. Before this they were open to any connected client:
anything anybody built could be moved, recoloured, rescripted or deleted by
anybody else.

`SetPrimScript` is the same gate rather than a lesser one, and that is the point
worth remembering — a script can move a prim, rename it, set it alight and delete
it, so being allowed to script somebody else's object is the most complete way to
take it over.

**An unowned prim belongs to the server**, so only an admin may change it. That is
everything built before schema V15 and everything a content pack installs, and it
is the deliberate side of a trade: treating "no owner" as "anybody's" would leave
the firebird and the beacon deletable by any passer-by. An operator re-homes a
genuinely orphaned build with `POST /api/content/transfer`.

The admin flag is read **once, at the handshake**, not per edit. That is the same
freshness `disabled` already has — `Sessions` is keyed by token with no principal
index, so revoking either takes effect on the next `ClientHello` — and it keeps
SQLite off a path that `UpdatePrim` walks at the gizmo's drag rate. The owner map
is in memory for the same reason (`SimWorld::owners`, loaded at startup and
maintained beside `scripted`).

### A refusal has to carry the truth

This is the half that is easy to miss. The edit dialog and the gizmo both apply a
change **locally** before the server answers, deliberately, for the feel of it. So
a refused client is left drawing something that never happened — a prim moved
where the world does not have it, or, on a refused delete, despawned locally while
the server still has it. Nothing would correct that: `PrimUpsert` is only
broadcast when something *changes*, and a refusal changes nothing.

So the server sends the authoritative prim straight after the 403
(`net::refuse_and_correct`), and the client deselects on that code before applying
it. The order matters and is not incidental: the client deliberately ignores
upserts for the prim the local user is manipulating, so without the deselect the
correction would be the one message it throws away.

### The document is a canned request

A `ContentDocument` is `{version, prims: [PrimSpec]}`, and `PrimSpec` is the same
type the create route accepts, so an export is literally a document you can post
back. Two things are deliberately dropped on the way out: the id (an id from one
server means nothing on another) and `position.x/z` (a client-frame echo every
client re-derives from the geo anchor).
Rotations are degrees on this surface and radians on the wire — an export that
wrote radians would re-import as a prim turned by 5157 degrees, which the round
trip test pins.

An emitter is written out **in full** rather than as the fields that differ from a
preset, unlike everywhere else. A document is a file somebody keeps, and "the fire
preset's speed" is not a stable thing to write down — a preset retuned later would
silently change every stored document.

**Import adds; it does not replace.** A document has no way to say which live prim
each entry *was*, so replacing would have to guess and guessing wrong deletes
somebody's work — and importing your own export twice (a second copy of a build,
somewhere else) is a reasonable thing to want. Deleting the old copy is one
`DELETE` per prim and is the caller's decision to make out loud. Note this is the
opposite rule from a content pack, which *does* replace: a pack is identified by
`prims.origin` and so knows exactly what it installed last time.

## The tool

`tools/author.py` is a convenience over `curl` and nothing more — the API is the
product. Its `--print` flag shows the request without sending it or signing in,
which is the fastest way to write one of these by hand. `tools/author.py install
firebird` is one POST with a name in it; everything about *what* is installed is
decided in the simulator.

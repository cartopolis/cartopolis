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
building something in front of them. It is checked against the region's prim and
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

**An emitter is described by what differs from a preset**, on both surfaces. A
JSON body naming `color_start` alone is a blue flame with the fire preset's other
eleven fields; so is `vfParticles{ color = {0,0.4,1} }`. That leniency lives in
`authoring::ParticleSpec` rather than on `ParticleEffect` itself, and it has to:
`ParticleEffect` is postcard-encoded in every `WorldSnapshot`, postcard is not
self-describing, and giving its fields serde defaults or `Option`s would change
what the bytes mean and desync every client.

## What it deliberately does not do

**Change a prim's shape.** `update_prim` does not carry the column, and nothing
has asked for it; refusing beats accepting it and silently drawing a cube.

**Change a prim's region.** A prim's region decides which budget it draws
against, so moving it is a capacity transaction rather than a field write.

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

## The tool

`tools/author.py` is a convenience over `curl` and nothing more — the API is the
product. Its `--print` flag shows the request without sending it or signing in,
which is the fastest way to write one of these by hand. `tools/author.py install
firebird` is one POST with a name in it; everything about *what* is installed is
decided in the simulator.

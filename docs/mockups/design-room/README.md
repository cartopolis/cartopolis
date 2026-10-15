---
topic: design-room
triggers: [design room, library_publish, "server library", "Design here", clean room, white room, Build, render_context_menu, ring of actions, render_edit_dialog, EditDialogState, prim editing, drafts, inventory, inventory_items, model_library.rs, library.rs, library_share, "shared library", Take, "Take copy"]
updated: 2026-10-15
---

# A white room to design an object in, and an inventory to keep it in

## v3 — thought through, proposed 2026-10-15

![v3](v3.png)

Redrawn around three answers (**all of the inventory on the server**, drafts
included; **no isometric or orthographic views**; **sharing stays open to
everyone**) and a second pass that changed four more things:

- The room is drawn in **perspective** with the map's orbit camera. Its views
  are places to stand: Orbit, Eye level, and From the street (the far side of
  the spot's real kerb, at 1.65 m).
- **Two ways to give to everyone.** *Share* is today's act, for any account,
  now for designs too: a copy in Library ▸ Shared by people, still yours,
  withdrawable. *Move to the server library* hands the item to the server
  (owner cleared, like a content pack) and needs a new `library_publish`
  permission, which only admins hold for now.
- **Placing makes a copy**, as Second Life's rez does; *Take* makes a new
  item, and *Design* on a placed group edits it in place.
- **A group needs the wire after all**: a `group` field on each object so one
  tap selects the whole stall (v2 claimed no wire change; that was wrong).
- While you design, others see your avatar pale and marked **designing**.

The sheet ends with what each piece is built on (the `/api/content/export`
document, `/api/content/import` for placing, `inventory_items`,
`library_entries`) and the questions still open.

## v2 — inventory and shared library, proposed 2026-10-15

![v2](v2-inventory.png)

Answers to v1's questions, agreed 2026-10-15: a design is **several ordinary
objects placed as a group** (no linked-object wire change), **drafts stay on
the device**, and **the avatar stands still** in the room as a scale figure.

The same answer asked for an **inventory, after Second Life's**: server-side,
per account, in folders — Objects (designs, stored as the `{version, prims}`
document `/api/content/export` already writes), Drafts (the same document,
on this device only), Models (today's `inventory_items`), Trash, and the
shared library under them, read-only. Items are placed by dragging onto the
map (desktop) or "Place in front of me" (phone); placed designs come back with
**Take** and **Take copy** on their ring. The room's left panel gets an
Inventory tab to drag models and objects in as parts.

Moving an item to the **shared library** needs a new permission,
`library_share`, granted to no role at first, so only admins (who hold every
permission) have it. That takes something away: today any signed-in account
may share an uploaded model whose licence allows it, from the upload dialog.

## v1 — the room, proposed 2026-10-15

![v1](v1.png)

The Construct from *The Matrix* as a mode of its own: **Design here** on the
map's ring (beside Build) fades the world to white and leaves the object, your
avatar for scale, a metre grid and a faint outline of the spot's kerb and
facade. The tools are today's Edit dialog (Basics, Shape, Surface) docked on
the right, plus a parts list on the left. **Place** fades the world back in
with the object standing on the spot, sent to the server as one act.

Drawn isometric for legibility: the client has no orthographic camera today
(every camera is `Projection::Perspective`), so the room's Front / Side / Top
views are new work.

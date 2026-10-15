---
topic: design-room
triggers: [design room, "Design here", clean room, white room, Build, render_context_menu, ring of actions, render_edit_dialog, EditDialogState, prim editing, drafts, inventory, inventory_items, model_library.rs, library.rs, library_share, "shared library", Take, "Take copy"]
updated: 2026-10-15
---

# A white room to design an object in, and an inventory to keep it in

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

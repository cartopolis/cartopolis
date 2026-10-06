---
topic: permissions
triggers: [permissions.rs, roles.rs, Permission, PermissionSet, "roles", "role", require, caller_permissions, is_operator, admin_only, "/api/admin/roles", "/api/admin/permissions", account_roles, role_permissions, V27, "player role", may_edit, may_script]
updated: 2026-10-06
---

# Roles and permissions

Until 2026-10-06 a server had two kinds of account: an admin (the
`accounts.admin` flag, V8) who could do everything, and a player who could do
whatever the code happened to allow. Now every decision names a **permission**
from one closed list (`cartopolis_core::permissions`), and an account holds
permissions through **roles** an operator edits in the web UI's Roles tab
(migration V27, `simulator::roles`).

## The model

- **The list is code, not data.** A permission exists because something checks
  it; the web UI draws its grid from `GET /api/admin/permissions` and keeps no
  copy, and a slug the list does not hold is refused when a role is saved.
- **Player is a built-in role every enabled account holds.** Its permissions
  are editable, and they started as exactly what every account could already do
  (`Permission::player_default`, pinned against the V27 seed), so introducing
  roles changed nobody's abilities.
- **Further roles add.** What an account may do is the player role plus every
  role it holds; there is no deny.
- **Admin stays the flag** and holds every permission, including ones added
  later. It is not a role because a role is a set somebody could forget to
  extend, and because the last-enabled-admin guard is about the flag.
- **A disabled account holds nothing.**

## Freshness

HTTP reads the account's permissions per request (`http::caller_permissions`),
so taking a role away works on the next call.

The game connection reads them at sign-in. It keeps that copy rather than
reading the database per edit, because `UpdatePrim` arrives at the gizmo's
drag rate. Since protocol 35 the copy is **kept current**:
- Every route that can change a permission ends in `roles::refresh_grants`:
  - a role's grants;
  - a role deleted;
  - an account's roles;
  - its admin or disabled flag.
- `refresh_grants` re-reads every connected account. It doesn't work out which
  accounts the edit touched, because an edit to Player touches all of them.
- The answer goes through a per-account watch in `ChatHub`. Each connection of
  that account updates its own gate and sends its client
  `NetMessage::Permissions`, the granted slugs.

The client uses that list (`AuthIdentity::may`) only to decide what to offer —
for example, the building card's "Replace with a model" button, shown only with
`building_override`. Disabling an account empties its permissions at once, but
the session itself stays open until the next handshake.

## Two permissions are trust, not features

- `roles_manage` can grant any role any permission, including to the account
  holding it. It is admin-equivalent and the web UI says so next to the box.
- `accounts_manage` can disable and delete accounts — **but not an admin's**,
  and it cannot grant or revoke the admin flag (`http::admin_only`); otherwise
  it would be a way to become an admin, or to lock the admins out.

## What changed behaviour

- `GetPrimScript` used to answer any connected client about any prim. It now
  needs ownership, `script_read_any` or `script_edit_any`. Player holds
  `script_read_any` by default, so nothing changed until an operator takes it
  away.
- A refusal by permission on your *own* prim says so ("this account may not
  …") instead of the old "belongs to somebody else", which was a lie told to
  its builder (`prims::refusal`).
- The operator web UI (`/admin`) now admits any account with an operator
  permission (the server group, `building_override` or `building_design_any`),
  and shows only the tabs it opens. Atlas's pipeline routes need
  `pipeline_run`; atlas reads it from `/api/admin/me`, and an older simulator
  that answers only `admin: true` is read as every permission.

## Not done

- Teleport is still unchecked and unthrottled on the wire.
- The client does not hide controls a role forbids (the Physical box, the
  script editor); the server refuses, and says why.

-- Roles and permissions (docs/notes/permissions.md).
--
-- A role is a named set of permissions from `cartopolis_core::permissions`;
-- an account holds the built-in `player` role implicitly and any number of
-- others explicitly; what it may do is the union. Admin stays the
-- `accounts.admin` flag and holds every permission, so it is not a row here.
--
-- No FOREIGN KEY, as V20-V22 have none: this connection runs with
-- `foreign_keys` off. Deleting a role or an account removes its rows in code.
CREATE TABLE roles (
    id         TEXT PRIMARY KEY,           -- a slug: 'player', 'builder'
    name       TEXT NOT NULL,              -- what the web UI shows
    built_in   INTEGER NOT NULL DEFAULT 0, -- 1 for 'player', which cannot be deleted
    created_at TEXT NOT NULL
);

CREATE TABLE role_permissions (
    role_id    TEXT NOT NULL,
    permission TEXT NOT NULL,              -- a `Permission::slug`
    PRIMARY KEY (role_id, permission)
);

CREATE TABLE account_roles (
    account_id TEXT NOT NULL,              -- accounts.id
    role_id    TEXT NOT NULL,
    PRIMARY KEY (account_id, role_id)
);

-- The player role starts with exactly what every signed-in account could do
-- before roles existed (`Permission::player_default`; a test pins the two
-- lists equal), so this migration changes nobody's abilities.
INSERT INTO roles (id, name, built_in, created_at)
    VALUES ('player', 'Player', 1, datetime('now'));
INSERT INTO role_permissions (role_id, permission) VALUES
    ('player', 'prim_create'),
    ('player', 'prim_edit_own'),
    ('player', 'prim_physical'),
    ('player', 'prim_video'),
    ('player', 'script_edit_own'),
    ('player', 'script_read_any'),
    ('player', 'building_claim'),
    ('player', 'building_design_own');

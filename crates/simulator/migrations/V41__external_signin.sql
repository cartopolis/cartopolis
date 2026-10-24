-- Signing in with another account: Discord, Reddit, Mastodon, Codeberg,
-- Google, GitHub, or a community's own OpenID server (`signin`,
-- docs/notes/external-sign-in.md). No FOREIGN KEY clauses, for the reason V20
-- gives; `auth::delete_account` and `communities::delete` clean up.

-- What the operator set up in Admin › Sign-in. One row per provider that has
-- ever been touched there; a preset nobody configured has no row and is listed
-- from code. `secret_sealed` is the client secret sealed with the server's
-- sign-in key (`signin::keys`), never the secret itself.
-- new_accounts: 0 anyone, 1 only members of a linked group, 2 nobody new.
CREATE TABLE signin_providers (
    id             TEXT PRIMARY KEY,         -- 'discord', 'google', 'oidc-<slug>' ...
    kind           TEXT NOT NULL,            -- discord | reddit | mastodon | github | oidc
    name           TEXT NOT NULL,
    issuer         TEXT,                     -- oidc only
    client_id      TEXT NOT NULL DEFAULT '',
    secret_sealed  BLOB,
    enabled        INTEGER NOT NULL DEFAULT 0,
    position       INTEGER NOT NULL DEFAULT 0,
    new_accounts   INTEGER NOT NULL DEFAULT 0,
    tested_at      INTEGER                   -- unix seconds of the last passed test
);

-- Who an account is elsewhere. `subject` is the provider's own stable id for
-- the person (never the address, which can change); for the fediverse it is
-- the account id qualified by its server.
CREATE TABLE account_identities (
    id          TEXT PRIMARY KEY,            -- UUID, what the web UI names a row by
    provider    TEXT NOT NULL,               -- signin_providers.id
    subject     TEXT NOT NULL,
    account     TEXT NOT NULL,               -- accounts.id
    handle      TEXT NOT NULL,               -- 'annadv', '@anna@mastodon.nl'
    linked_at   INTEGER NOT NULL,
    UNIQUE (provider, subject)
);
CREATE INDEX account_identities_account ON account_identities (account);

-- Mastodon (and every server that copies its API) has no central place to
-- register an app: this server registers itself with each one the first time
-- somebody from there signs in, and keeps what it was given.
CREATE TABLE fediverse_signin_apps (
    host           TEXT PRIMARY KEY,
    client_id      TEXT NOT NULL,
    secret_sealed  BLOB NOT NULL,
    created_at     INTEGER NOT NULL
);

-- A community linked to a Discord server or a subreddit by somebody who runs
-- both. Whoever signs in through that group is offered the community.
CREATE TABLE community_links (
    community   TEXT NOT NULL,               -- communities.id
    provider    TEXT NOT NULL,               -- signin_providers.id
    group_id    TEXT NOT NULL,               -- guild id, subreddit name (lower case)
    group_name  TEXT NOT NULL,
    linked_by   TEXT NOT NULL,               -- accounts.id
    linked_at   INTEGER NOT NULL,
    PRIMARY KEY (provider, group_id)
);
CREATE INDEX community_links_community ON community_links (community);

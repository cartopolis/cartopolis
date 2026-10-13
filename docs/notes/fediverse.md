---
topic: fediverse
triggers: [ActivityPub, fediverse, Mastodon, Lemmy, webfinger, "public posts", "crates/simulator/src/fediverse", posts.rs, community_public, fedi_deliveries, blocklist, "HTTP signatures"]
updated: 2026-10-13
---

# Public posts and the fediverse

A community can switch on a public half beside its encrypted topics: **posts**,
each with a place, that anybody may read. With them on, the simulator speaks
ActivityPub for it. Design: `docs/mockups/communities/v4-public-posts.html`.

## Decisions (2026-10-13, with the user)

- **A post goes out under its author's own address** (`@sanne@cartopolis.org`),
  and the community boosts it. That is how Lemmy works and what gives a reply
  somebody to answer. The cost: an account becomes findable from outside once it
  has posted. Before that, WebFinger and its actor answer 404 (`is_public_person`).
- **Posts from elsewhere appear straight away.** Moderators hide them afterwards
  and can block an author from the community.
- **Federation is open, with a block list.** There is no list of servers to
  follow. The control is a list of servers to refuse. It starts from a shared
  list re-read daily (`fediverse_blocklists`; Garden Fence by default) and
  carries the operator's own blocks, which no import touches.

## Where it lives and why

- **HTTP, not the wire.** Posts, settings, follows and blocks are
  `/api/communities/{id}/…` and `/api/posts/{id}`. The web page reads them with a
  plain `fetch`, and a change to them moves no client's protocol version. The
  JSON shapes are `cartopolis_core::posts`.
- **Our own implementation, not `activitypub_federation`.** Lemmy's library was
  checked on 2026-10-13 (0.6.6) and turned down. It targets axum 0.7 where this
  workspace is on 0.8, and it brings reqwest (a second HTTP and TLS stack beside
  ureq), moka and chrono. What we need is about 2,000 lines: signatures, a
  WebFinger answer, two actor types and an inbox.
- **RSA-SHA256 draft-cavage signatures**, via the pure-Rust `rsa` crate. That is
  the only scheme every server verifies. The `rsa` timing advisory
  (RUSTSEC-2023-0071) is discussed in `fediverse::signing`.

## Things that must stay true

- **Every request to another server goes through `fetch::Client`.** Its resolver
  refuses any name that resolves to a private, loopback, link-local or CGNAT
  address. It allows only `https` on port 443, and it follows redirects by hand.
  The VPS's private network holds nominatim, overpass and Forgejo, and federation
  means fetching URLs strangers wrote. `fediverse_allow_private` turns the guard
  off for tests and must never be set in a deployment.
- **Nothing from another server is rendered as HTML.** Content is reduced to
  plain text on arrival (`objects::html_to_text`). Pictures are copied into the
  picture store when the post arrives, so a reader's device never asks a remote
  server for one.
- **A handle is permanent and never reused**, even after its community is
  deleted (`retired_handles`). A handle can never equal a username, in either
  direction: both are found as `@name@server`. Registration checks handles, and
  setting a handle checks usernames.
- **Only a public post's picture is served without a session** (`/ap/media/`,
  checked against `posts.picture`). The rest of the picture store is chat
  ciphertext and stays behind a session.

## Deploying it

Caddy must send `/.well-known/webfinger`, `/.well-known/nodeinfo`, `/nodeinfo/*`
and `/ap/*` to the simulator. Without them, other servers get the web UI's
`index.html` and nothing federates. The live Caddyfile also lists player `/api`
paths one by one rather than `/api/*` (its comment says why), so
`/api/communities/*/public`, `…/posts`, `…/follows*`, `…/blocks` and
`/api/posts` + `/api/posts/*` need adding to its `@models` matcher, or the app
and the web page get a 404 from the file server. Federation switches on by
itself when `public_base_url` is a public `https` origin, which on the live
server is `https://cartopolis.org`. The address of everything is fixed by that
origin, so it must not change once anybody follows anything.

## Not done

- A deleted account's posts are deleted here but **not withdrawn from other
  servers**. `db::erase_account_traces` has no federation context to send the
  `Delete`s with.
- **Reports from other servers (`Flag`) are dropped**, and reports go out
  nowhere. The community's own report path is topic-shaped (a group id and a
  sequence number) and has no slot for a post.
- **No `Like` counts**, no editing of a post written here, no following of
  hashtags (a server-to-server follow cannot follow a hashtag).

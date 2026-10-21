---
topic: fediverse
triggers: [ActivityPub, fediverse, Mastodon, Lemmy, webfinger, "public posts", "crates/simulator/src/fediverse", posts.rs, community_public, fedi_deliveries, blocklist, "HTTP signatures", fedi_servers, post_votes, post_reports, fedi_gone_accounts, "pages/public", reader_url]
updated: 2026-10-21
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

## Speaking Mastodon's and Lemmy's dialects (2026-10-21)

Until then it had been tested only between two Cartopolis servers. What was
checked against Lemmy's source (0.19) and changed:

- **A community follows elsewhere as its _reader_**, a `Service` actor at
  `/ap/c/{id}/reader`, named `{handle}.reader`. Lemmy's `Follow` takes its
  actor as a person (Person, Service, Organization) and refuses a group, so a
  community here could not follow a Lemmy community as itself. A follow made
  before this, as the group, is still undone as the group (the follow's id
  names who made it). The dot in the name is what no handle has; registration
  and `handle_free` keep usernames off `{handle}.reader` too.
- **A community's outbox lists its `Announce`s of each `Create`**, not the
  `Create`s: Lemmy reads a group outbox as `Vec<AnnounceActivity>`, so with
  `Create`s the community showed empty when a Lemmy user first found it. The
  boost of a post written here has one id for good (`post_boost_id`), the
  same in the outbox and in the delivery.
- **Each follower's server gets the boost it reads**, by what its NodeInfo
  says it runs (`fediverse::servers`, `fedi_servers`): Lemmy's kind the
  `Announce` of the activity, Mastodon's kind the `Announce` of the object
  id; a server that never said gets both, as every server used to. Lemmy
  fetches a bare id and fails to read a `Note` as an activity, so every boost
  was an error there. The software is asked for when a server follows, and by
  the delivery worker the first time it delivers somewhere unasked.
- **An edit to a note goes out from its author**, to the community's followers
  as well as the author's: Mastodon believes an `Update` only from whoever
  wrote the note, and ignores one inside a group's boost. Lemmy gets the
  group's boost of it too.
- **Mentions of accounts elsewhere** (`@anna@mastodon.social` in the text) are
  looked up by WebFinger when the post is written or edited, kept in
  `post_mentions` so the note reads the same each time it is fetched, and
  written as Mastodon writes them: an `h-card` link, a `Mention` tag and a
  `cc`. At most ten per post; one that does not resolve stays words. A
  mention of somebody on this server is not resolved.
- **Likes and Lemmy's votes are counted** (`post_votes`, on the post's
  fediverse id, once per account), and **reports (`Flag`) wait for the
  community's moderators** (`post_reports`): Hide closes every open report on
  that post, Leave it closes one. A report naming only an account is dropped:
  a community's report is about a post. Reports from here still go nowhere.
- **A deleted account is withdrawn.** `posts::forget_account` records whom to
  tell in `fedi_gone_accounts` (it runs without the public address, from the
  shell as well as the app), and the delivery worker sends a `Delete` of each
  post and of the account (`removeData`, for Lemmy). The account's key stays:
  the deletion is signed with it, and its actor answers 410 Gone while it is
  there.
- **Profiles say who they are.** A person's actor has their picture, their
  join date, the communities they write in and `indexable: true` (Mastodon's
  search; their posts are public and sent out under their name already). A
  person's and a community's `url` are web pages, `/@{name}` and
  `/c/{handle}` (`crates/atlas/web/src/components/pages/public`), readable
  signed out, with "Follow from your own server", which asks
  `/api/fediverse/follow-link` for that server's own follow page.
- **Pictures say what they are**: the real type and size, read from the stored
  file, where every attachment used to claim `image/jpeg` with no size.
- **A Lemmy link post keeps its link** (an `attachment` of type `Link`),
  appended to the text.
- **A community can follow a hashtag through a relay**: the follow box takes
  an actor's `https://` URL as well as an address, for a relay with no
  address (`https://relay.fedi.buzz/tag/groningen`).
- **Discovery**: `/.well-known/host-meta` (Friendica and older Pleroma look
  there first), and NodeInfo's `activeMonth` (from `stats.db`, not published
  below `MIN_PEOPLE`) and `localComments`.

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

Caddy must send `/.well-known/webfinger`, `/.well-known/host-meta`,
`/.well-known/nodeinfo`, `/nodeinfo/*` and `/ap/*` to the simulator. Without them, other servers get the web UI's
`index.html` and nothing federates. The live Caddyfile also lists player `/api`
paths one by one rather than `/api/*` (its comment says why), so
`/api/communities/*/public`, `…/posts`, `…/follows*`, `…/blocks`,
`…/reports*`, `/api/posts` + `/api/posts/*` and `/api/fediverse/*` need adding
to its `@models` matcher, or the app and the web page get a 404 from the file
server. `/c/*` and `/@*` are web pages and need nothing: the file server
answers them with `index.html`. Federation switches on by
itself when `public_base_url` is a public `https` origin, which on the live
server is `https://cartopolis.org`. The address of everything is fixed by that
origin, so it must not change once anybody follows anything.

## Not done

- **Reports go out nowhere**: a member here cannot report a post from elsewhere
  to its server. The community's own report path is topic-shaped (a group id
  and a sequence number) and has no slot for a post.
- **Nobody here can like a post.** Likes are counted from elsewhere only.
- **Not yet tried against a live Lemmy.** The shapes above were checked
  against Lemmy's source; the first follow of a Lemmy community and the first
  Lemmy follower are the test.

---
topic: atlas-web-ui
triggers: [crates/atlas/web, types.ts, web_mirror.rs, ts_mirror.rs, mock.ts, base.ts, theme.css, web_theme.rs, deploy-web.yml, deploy/build/web.sh, follow_framed_places, FRAMED_PLACE_MESSAGE, "@garage44/mithril", mithril, "bun run mock", "/admin", "/world", "cartopolis.org"]
updated: 2026-10-17
---

# cartopolis.org is the atlas web UI, and three seams hold it to the Rust

`crates/atlas/web` is the site: a Mithril app (TypeScript under Bun, not Rust)
served as static files at the root of cartopolis.org. The renderer is
`@garage44/mithril`, our fork at `jeroen/mithril`, pinned in `package.json` by
the commit tarball it builds from. Its own README has the how-to (layout, the
store, the dev server); this note holds what constrains changes to it.

## Shape

One window with a **Community | World | Admin** switch in a top bar
(`docs/mockups/web-navigation/`). Community at `/` is the chat. World at
`/world` frames the app's own web build from `/viewer/` and flies it to a
community's place by `postMessage`: the viewer reads only messages from its own
origin (`place_link::follow_framed_places`, message type
`FRAMED_PLACE_MESSAGE`), so another site that frames it cannot steer it, and the
place goes through the same parser as a link. Admin at `/admin` appears only for
an account that may run something, on the chat's own sign-in, and groups the
operator's pages into People, Content and Data. Beside them are the players'
pages: `/register`, `/verify`, `/buildings`, and the floor-plan editor at
`/building/{pand}` (`building-designs.md`).

Caddy routes the viewer (`/viewer/`), the APK, the simulator's `/ws` and its
*listed* `/api` paths, and atlas's `/terrain`, `/height` and `/api/pipeline`
ahead of it; everything else falls back to the web UI's `index.html`. "Listed"
is the trap: an `/api` route missing from Caddy's `@webapi` matcher answers HTML
with a 200 (`hosted-simulator.md`).

It talks to more than the admin surface now. CLAUDE.md said "`/api/admin` and
`/api/pipeline/*` and nothing else", which stopped being true when Community
arrived: the chat worker opens `/ws`, and the pages call `/api/communities`,
`/api/register`, `/api/login`, `/api/tokens`, `/api/buildings`, `/api/posts`,
`/api/pictures`, `/api/push`, `/api/library`, `/api/inventory` and `/api/assets`
(counted from `src/` on 2026-10-17).

History: it was the *console*, at `/console/` (Caddy still redirects it to
`/admin`), beside a front page atlas served from a string; before that it was
`web.cartopolis.org`, a separate repository, retired with no redirect, until
2026-09.

## Publishing

**`midas deploy web`**, which builds on a farm node from
`deploy/build/web.sh` and publishes in about 80 s. `deploy-web.yml` is
`workflow_dispatch` only since 2026-10-16 and is the fallback for when the farm
is down; until then it ran on every push to `main` that touched the web UI.
Publishing static files restarts nothing.

**File names carry their content's hash** (2026-10-17, `build.ts`):
`app-<hash>.js`/`.css`, `call-worker-<hash>.js` and `chat-core-<hash>.wasm`,
with `index.html` rewritten to name them and the URLs put into the scripts by
`define`. Caddy serves those `immutable` and everything else `no-cache`, and
serves the `.br`/`.gz` files the build writes beside each script instead of
compressing the 2.9 MB engine again per request. Before, every file was
`no-cache`, so even a cached page cost a revalidation round trip per file, one
after another, before it could connect. Two consequences:

- **`chat-worker.js` keeps a fixed name.** Tabs share a SharedWorker only while
  its URL is the same; a new name per deploy would leave an old tab and a new
  one in two workers contending for the keys' Web Lock.
- **A deploy replaces the directory**, so a tab left open across one cannot
  load the previous `call-worker-<hash>.js`: a call started from a stale tab
  fails to start its encryption until the page is reloaded.

**The page does not wait for the API before drawing** (`app.ts`): the chat
page signs in from its `oncreate`, so `/api/admin/me` and `/api/register` are
asked together while it draws (`signedIn === null` and `registration ===
undefined` are their "still asking" states), and `adoptSession` shares one
request between the page start and the chat page's admin check.

## The seams to the Rust

- **`src/types.ts` is a hand-written description of what the simulator
  serialises** — `auth::AccountRow` and `library::PlaceholderModel`. One
  repository does not make it compile-checked, so `web_mirror`
  (`crates/simulator/tests/integration/web_mirror.rs`) serialises a real value
  of each and compares its keys with the interface's fields, both ways round,
  through `cartopolis_core::ts_mirror`. Change an account field and change
  `types.ts` in the same commit; the failure otherwise is a column that reads
  `undefined`, not an error.
- **`mock.ts` is not a test double.** `bun run mock` serves every page against
  typed in-memory fixtures with no simulator or atlas (`?mock-signin` opens it
  signed in), for design review and screenshots. The fixtures are typed against
  `types.ts`, so a renamed field fails `tsc` there — but `web_mirror` is what
  checks the wire, and a mock the tests leaned on would be a second, unchecked
  description of it.
- **The colours are generated from the client's.** `src/css/theme.css` is
  written by `systems::web_theme` from `systems::theme`'s constants, and a test
  fails when the checked-in file differs
  (`CARTO_WRITE_WEB_THEME=1 cargo test -p cartopolis web_theme` rewrites it).
  The copy used to be by hand and drifted: the client went charcoal and orange
  while the page stayed slate blue.

## Controls come from the fork's component library

Inputs, buttons, layout and the light/dark theme are
`@garage44/mithril/components` and `@garage44/mithril/validation` (since
2026-10; the fork's `docs/notes/components.md` has the design). Use those rather
than a hand-built input or a fresh colour: a fresh colour in `app.css` is
exactly how the two halves of the product came to look like two products.

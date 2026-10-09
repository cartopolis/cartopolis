# The atlas web UI

**[cartopolis.org](https://cartopolis.org)** itself: a small Mithril app, static
files behind Caddy at the site root. It is the front page (`/`), the operator's
pages (`/admin`) and the players' pages (`/register`, `/verify`, `/house`);
Caddy sends everything else — the viewer at `/viewer/`, the APK, the simulator's
`/api` and `/ws`, atlas's tiles and `/api/pipeline` — where it belongs. Every
path goes through `src/lib/base.ts`. It talks to the simulator's `/api/admin`
surface (`crates/simulator/src/http.rs`) and to atlas's `/api/pipeline` surface
(`crates/atlas/src/api.rs`).

It was called the *console*, lived at `/console/` beside a front page atlas
served from a string, had its own hostname (web.cartopolis.org) before that, and
a repository of its own until 2026-09. Caddy redirects `/console/` to `/admin`.

Sign in with a Cartopolis account carrying the `admin` flag — the same account
signs into the world itself, and atlas checks the bearer against the simulator
rather than keeping a list of its own.

## What it does

**For the operator** (`/admin`): list accounts, create them,
enable/disable, promote/demote to admin, reset a password, delete — and a
*Houses* tab listing who holds which address. Two rules are enforced by the
*server* and simply reported here, which is why its error text is passed through
verbatim rather than replaced: the last enabled admin cannot be removed, and you
cannot delete the account you are signed in with.

Note that **disabling stops the next sign-in, not a live session** — the
simulator's session store is keyed by token with no principal index.

**The Data tab** drives the pipeline: what the served tree covers now, a form to
refresh or extend it, and the live log of the run that is going. See
[The data pipeline](#the-data-pipeline) below.

**For anybody** (`/register` and `/verify`): make an account, confirm the email
address it was made with, and claim a house. Those two routes are the reason this
app is no longer admin-only, and the shape is worth knowing before changing it.

- The account exists from the moment the form is submitted — the username is held
  — and **cannot be signed in with until the address answers**. The simulator
  calls that state `pending`, and it is not `disabled`; the account row shows both
  as separate tags because they have different cures.
- The verification link lands on `/verify?token=…`, which POSTs the token **once**
  (`useEffect` with an empty dependency list — a re-render that spent it a second
  time would turn a successful sign-up into "that link is not valid").
- Confirming hands back an ordinary **session token**, kept in `$s.signup` and
  reached through a *second* `Api` instance (`signupApi`). It is deliberately not
  `$s.profile`: that one is the operator's and `App.init` validates it against
  `/api/admin/me`, so writing a player's token into it would sign the new person
  out of their own sign-up with a 401 as the explanation.
- A confirmed account can then **claim one house**, by typing an address. That is
  Groningen-only for now, because that is where the map has buildings, and it is
  first come first served — which makes the operator's Houses tab the only dispute
  mechanism there is.
- A newly registered account **cannot sign in here**. This page is admin-only and
  says so: the confirmation screen points at
  [cartopolis.org](https://cartopolis.org) for the world itself.

The registration rules (open or closed, minimum password length, how long a link
lasts) are fetched once from `GET /api/register` into `$s.registration`. A form
that guesses them finds out it was wrong on submit, and a "Create an account"
button on a server that takes no registrations is a button that leads to a
refusal — so the sign-in page only draws one when the server says `open`.

## The data pipeline

The Data tab drives atlas's map data pipelines (`crates/atlas/src/pipeline/`)
through `/api/pipeline/*` (`crates/atlas/src/api.rs`). They were a
TypeScript server until 2026-09, and the Python scripts in `tools/` before that; the
formats they write, the routes and the refusals are unchanged, so this page
needed no change beyond where the dev proxy points. `docs/notes/atlas.md` has
the datasets, the port and how it was checked.

## Architecture

Borrowed from [codebrew](https://code.garage44.eu/jeroen/codebrew), the house
style for Garage44 front-ends. Two pieces of it in particular:

- **State is one reactive store** (`src/lib/store.ts`), Mithril's `state()`. No
  context, no provider, no selectors: a component reads `$s.accounts` and redraws
  when it changes. A component's own state is the same thing at a smaller scale
  (`local()` in `src/lib/reactive.ts`), so a write from an awaited `fetch`
  redraws without an `m.redraw()`. `persistentState` is both the initial shape *and* the localStorage
  whitelist — `filterKeys` walks it as a blueprint, so a key absent from it is
  structurally incapable of being persisted, and `volatileState` is applied last
  so a stale account list can never be rendered as though the server had just
  returned it.
- **`App.init` owns the bootstrap** (`src/app.ts`), and everything shared — the
  store, the API client, the notifier — is built once at module scope and
  imported by name. One folder per component under
  `src/components/{pages,elements}`.

The renderer is **Mithril, Garage44 edition** (`@garage44/mithril`): our fork of
Bitstillery's Mithril with signals, at
[code.garage44.eu/jeroen/mithril](https://code.garage44.eu/jeroen/mithril). It
replaced Preact, preact-router and deepsignal in 2026-10. It is not on npm, so
`package.json` names a commit's tarball from Forgejo — a branch or a git URL
would be the obvious spelling, and Bun's git clone of this Forgejo fails where
the tarball does not. Bump it by changing the commit in that URL. Components are
`MithrilComponent` classes, because a plain `{view}` object is not something TSX
can type-check as a tag; routing is `m.route` with pathname URLs
(`src/components/main/main.tsx`).

The **controls, layout and theme are the fork's component library**
(`@garage44/mithril/components`, its stylesheet, and `@garage44/mithril/validation`),
since 2026-10; the fork's `docs/notes/components.md` has the design. Every stateful
control takes its state as a `model` signal — `bind(s, 'key')` in
`src/lib/reactive.ts` hands it a field of a `local()` — and a form's rules are a
`validations({...})` that drives its submit button. The operator's pages sit in the
library's `Shell` (`src/components/main/main.tsx`): the tabs in the left nav, and a
right-hand panel a page fills with `openContext()` (`src/lib/layout.ts`) to edit
one record beside the list — on a phone it covers the list instead. Everybody
else's pages get a header and a centred column.

The palette is the library's, which took it from the client's theme constants
(`systems::gui::theme`) and added a light half: the switch in the header follows
the OS until somebody picks. **No stylesheet here names a colour** — `src/css/app.css`
holds the frames and a page's own rules sit beside it as `<page>.css`, all in the
library's `--m-*` tokens, which is what makes both themes work.

The type-checker is **TypeScript 7**, the fork's: TypeScript 5 infers the
library's `m(Component, attrs)` overloads differently and fails inside it.

## Running it

```bash
bun install
bun run dev          # http://localhost:3100/, proxying /api to a local simulator
bun run mock         # the same, against a stand-in API in mock.ts: no simulator, no atlas
bun run build        # → dist/
bun test             # store rules, API errors, sign-up
bunx tsc --noEmit
```

`bun run dev` proxies `/api/*` to `$CARTO_ADMIN_API` (default
`http://127.0.0.1:4748`) and `/api/pipeline/*` to `$CARTO_PIPELINE_API` (default
atlas, `http://127.0.0.1:4749`) — two upstreams behind one prefix, exactly as
Caddy does in production, so the web UI sees one origin and needs no CORS.

`bun run mock` answers `/api/*` from `mock.ts` instead: typed fixtures of a
Groningen-sized world that writes change, for looking at the UI with neither
backend running. `/admin?mock-signin` opens signed in as an admin (`&mock-tab=Data`
picks a tab), `/buildings?mock-signin=player` as a player who owns two buildings.
It exists only behind that flag: the production build contains none of it.

## Deployment

`.forgejo/workflows/deploy-web.yml` builds `dist/` and publishes it to
`/data/cartopolis/admin` (the directory kept its old name so Caddy's mount did
not have to move). Caddy serves it as the fallback for every path on
cartopolis.org it routes nowhere else, with `index.html` for any path that is
not a file (`src/lib/base.ts`). The simulator's routes the web UI calls
(`/api/admin`, `/api/login`, `/api/logout`, `/api/register`, `/api/houses`)
are proxied to it on that hostname, and atlas gets `/api/pipeline/*`,
`/terrain/*`, `/height` and `/health`. The simulator's `CARTO_PUBLIC_BASE_URL`
is just the site, `https://cartopolis.org`: it builds
`{base}/verify?token=…`, which lands on the player page.

**Two hand-synced seams.** `src/types.ts` describes what the simulator's
`auth::AccountRow` and atlas's `jobs::Job` / `coverage::CoverageEntry`
serialise, and nothing but a person keeps a TypeScript interface in step with a
Rust struct — change a field on either side and change it here too.

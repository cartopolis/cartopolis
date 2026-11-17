---
topic: cms
triggers: [cms, "content pages", "$t(", crates/atlas/web/src/cms, registry.json, pages.html, pages-ssr.js, ssr.tsx, frame.tsx, pages/render.rs, script-host/src/render.ts, renderPage, cms-data, "/api/pages", "/mcp", PagesEdit, PagesPublish, hreflang, translation, static_files.rs]
updated: 2026-11-16
---

# Content pages: JSX blocks, every text a `$t`, made through MCP

## v3 (2026-11-16): drawn on the server for every visit, taken over in the browser

Asked for once Bun was in the server for prim scripts: serve the content pages
server-rendered, and keep them able to be interactive. Chosen by the owner over
rendering once per publish, so that a component with live data can have it in
the first HTML. Everything below v3 still holds, except "no server-side
rendering" and step 3 of "How a page reaches a reader".

- **The simulator renders every visit to a live page** with the web UI's own
  views. The web build writes them a second time for the server,
  `pages-ssr.js` (`src/cms/ssr.tsx`, `renderPage(data) → HTML`), beside
  `pages.html` and `pages-<hash>.js`, so the HTML, the script and the styles
  always come from one build. A web deploy replaces it; the simulator sees the
  new modification time and starts its renderer again.
- **The renderer is the script host's executable**, started as
  `cartopolis-script-host render <bundle>` (`tools/script-host/src/render.ts`):
  the server ships one Bun, not two. **Never the script process itself**: that
  one runs players' code, and a script that got out of its realm must not be
  able to write cartopolis.org's pages. It gets an empty environment, as the
  script process does.
- **One process, kept running, one page at a time** (`pages/render.rs`): a line
  of JSON in, a line of HTML out. Its stdout is the answer channel, so
  Mithril's server log on import and any component's `console.log` are moved to
  stderr before the bundle loads.
- **The browser takes the HTML over** (hydration): the page's data goes with it
  as `<script type="application/json" id="cms-data">` (every `<` written
  `<`), and `pages.tsx` mounts on the drawn body without fetching. Mithril
  keeps each element and attaches the components' handlers.
- **Nothing gets worse when it fails.** No bundle, no executable, a crash, a
  page that throws, an answer later than 2 s, more than 64 visits waiting: the
  visit gets the shell with the data and an empty body, and the browser draws
  it as v2 did. A hung renderer is killed and started again at the next visit;
  a failed start is retried after 30 s.
- **A draft is never server-rendered.** The session that may see it is in the
  editor's browser storage, not in the request.

**The rule for a component**: server and browser must draw the same tree, node
for node, and Mithril repairs a difference without saying so. So nothing in a
view may read the browser (`location`, `localStorage`, the theme, the clock,
the visitor): where the page is comes in as an argument (`frame.tsx`), and what
differs per visitor goes in `oncreate`, after the takeover. `ssr.test.ts` holds
the server's output to the browser's first view at the page's own address.

Measured (the live `home` page, 2.3 KB of HTML, real components):

| | Bun 1.3.14, container | Bun 1.4.4, mac-2 |
|---|---|---|
| first render after start | 7.6 ms | 1.2 ms |
| each render after | 0.12 ms | 0.025 ms |
| process start to 2001 renders done | 0.3 s | 50–70 ms |
| resident memory | 105 MB | 37 MB |

The takeover in headless Chromium, the page at `/nl/`: the server's `<h1>` kept
as the same element, the body's HTML unchanged, no request for the page, no
warning, 11.9 ms.

Rejected: **rendering once per publish**, cached per revision, with no process
kept running — cheaper, but a component with live data would show it only
after the takeover; **the script process** (above); **a renderer in Rust**,
a second set of views to keep equal to the first.

![v2](v2.png)

**v2 (2026-11-14), agreed, nothing built.** Asked for: cartopolis.org gets a
content management system whose pages are built from JSX components and made
through MCP; all text goes through `$t` placeholders stored with the page, so
one page shows in several languages. Decided with the owner on v1: **no
server-side rendering** — the server fills in only the page's title,
description, link-preview tags and language alternates — and **the Midas hub may
both make and publish pages**.

What v2 changed from v1 (below): the page is drawn in the browser, and
everything on the server lives in the simulator, in Rust. That removes the third
process, the Bun executable in the server package, its deploy target, a second
database and a second permission check. Lost: a crawler that runs no JavaScript
sees the title and description but not the body. Google runs it; link previews
(Mastodon, Signal, WhatsApp) read only the tags the server fills in. The stored
format is the same as v1's, so server rendering can be added later without
changing a page.

The frames: (1) a page as an agent writes it through MCP, (2) the texts stored
for it per language, (3) the same page at `/nl/…` and `/en/…`, (4) the
operator's list under Admin › Content › Pages.

## What a page is

**A page is JSX that is parsed, never run.** An agent writes
`<Hero heading={$t('hero.heading')}>…</Hero>`; the simulator parses it into a
tree of known components and their props, checks every prop against the
component's schema, and keeps both the source (what an agent reads back and
edits) and the tree (what the browser draws). Nothing in it is ever evaluated,
so a page cannot run code on the server or in a reader's browser, whoever wrote
it.

The parser takes a small, fixed subset of JSX:

- elements whose name is in the component registry, and nothing else (no `div`,
  no `script`, no lower-case HTML);
- props whose value is a string, a number, `true`/`false`, or `{$t('key')}`;
- children that are elements or `{$t('key')}`.

**Bare text is refused.** `<Text>Welkom</Text>` fails to save with the line and
column; it must be `<Text>{$t('welcome')}</Text>`. That rule is what makes every
visible word translatable instead of most of them. Each prop is typed in the
component's schema — `text` (must be a `$t`), `url`, `image`, `enum`, `number`,
`bool`, `place` — so a visible prop (`alt`, `heading`, a button label) cannot take a
literal string either, and a URL prop takes only `https:` or a site path.

**One registry, read by both sides.** The components' schemas are one file,
`crates/atlas/web/src/cms/registry.json`: the simulator embeds it to check a
page, the web UI imports it beside the views. There is no second description
to drift, so no mirror test is needed.

Rejected: **storing JSX as code** and running it: one bad or stolen token would
run code in every reader's browser; **a JSON tree as the format agents write**:
it is what the parser produces anyway, but JSX is what agents and people write
and read most easily, and what was asked.

## `$t`: texts belong to the page

A page's texts are rows beside it — `(page, key, language) → text` — not a
global translation file in the repository: pages are made at run time, and
their words with them.

- **Keys are scoped to the page.** `$t('hero.heading')` on `/about` and on
  `/download` are two different texts. Keys starting `site.` are shared by every
  page (the header, the footer, a component's own labels).
- **Every page has a source language**, the one it was written in. A reader
  asking for a language the text lacks gets the source text, marked with its own
  `lang` attribute so a screen reader pronounces it right.
- **A translation knows what it translated.** Each stores a hash of the source
  text it was made from; when the source changes, the translation shows as
  *stale* in the list (frame 4) and in MCP, rather than silently describing
  the old sentence.
- **Whole sentences per key.** A text may carry a small inline markup —
  `**bold**`, `*italic*`, `[link](/path)` — drawn as elements, never as HTML,
  so a translator can put the emphasis where their language puts it instead of
  the page splitting a sentence over three keys.
- **No variables in v1.** A page holds no data to put into a sentence, and
  `$t` takes only the key. The site's own texts (`site.` keys: the frame, the
  download buttons) have their defaults in `registry.json`.

## Where a page lives

`/{lang}/{slug}`: `/nl/over`, `/en/over`. One slug for every language (it is a
key, not text), the languages from the server's configuration (`nl`, `en` to
start). `/p/{slug}` redirects to the reader's language by `Accept-Language`, for
links that should not choose. The chat (`/`), World, Admin and the players'
pages are not affected; links into content pages are plain links.

## How a page reaches a reader

1. A request for `/nl/over` reaches the simulator's `static_files`, which
   already serves the web UI from `web_root` (Caddy passes the language prefixes
   and `/p/` to it on the hosted server).
2. It answers **`pages.html`**, a second, small shell from the web UI's build,
   with the page's `<title>`, description, `og:` tags, `<html lang>` and
   `<link rel="alternate" hreflang>` filled in from the live revision. A page
   that does not exist, or is not live, is a 404 with the same shell.
3. **`pages-<hash>.js`** — the registry, the components and the renderer, not
   the chat app — fetches `GET /api/pages/over?lang=nl`: the tree and the texts
   for that language with their fallbacks, and draws it with Mithril.

The components are the web UI's own (`@garage44/mithril/components`,
`theme.css`), so a page looks like the rest of the site.

## On the server

All of it in the simulator (`crates/simulator/src/pages/`), so the self-hosted
server has it with no change to the package.

- **Storage**: tables in `world.db` (one migration): `pages`, the draft per page,
  `page_revisions`, `page_texts`. Backed up with everything else.
- **Reads** are cheap: the live revision's tree and texts per language are held
  in memory and replaced on publish, so a busy page costs a map lookup on the
  HTTP side, not the tick.
- **Writes** go through `/api/admin/pages/*` and the MCP endpoint, both through
  one `PageService`, as `PrimService` serves the wire and the admin routes, so
  their checks cannot disagree.
- **Two new permissions** in `cartopolis_core::permissions`: **`pages_edit`**
  (make and change drafts) and **`pages_publish`** (put a draft live, take a
  page down, delete). Two rather than one costs nothing and lets a token for
  drafting exist later; the Midas hub's account holds both.

## Drafts, revisions, publishing

A page has one **draft** (its JSX and all its texts, changed freely) and
**revisions**: publishing copies the draft into a new, immutable revision and
makes it live. Changing a text of a live page changes the draft, not what
readers see, until it is published. Going back is restoring an older revision
into the draft and publishing it. A draft is visible at `/{lang}/{slug}?draft`,
which the page draws through the admin route with the session the web UI
already keeps: an editor who may write pages sees it, anybody else a 404.

## MCP

A Streamable-HTTP endpoint at **`https://cartopolis.org/mcp`** on the simulator
(`pages/mcp.rs`: one JSON answer per POST, no session or stream), signed in with an
access token from `/tokens`. Any MCP client adds it with the URL and the token.

**Midas makes and publishes.** The hub holds an access token for its own account
(a role with `pages_edit` and `pages_publish`) and offers the tools to sessions
as it offers Forgejo's, so a session never sees the token. That part is a
change in the midas repository, landed after this one.

| Tool | Does |
|---|---|
| `components` | The registry: every component, its props with their types, what children it takes, one example each. An agent reads this first. |
| `languages` | The configured languages and the default. |
| `list_pages` | Slug, title, source language, live revision, draft changed or not, per language: complete, missing *n*, stale *n*. |
| `get_page` | A page's draft JSX and its texts in every language. |
| `put_page` | Write a draft: JSX plus texts in one or more languages. Refused as a whole with line, column and reason when anything fails: an unknown component or prop, bare text, a `$t` key with no text in the source language. |
| `set_texts` | Texts for one language (a translation), each checked against the draft's keys. |
| `preview` | The draft in one language as readable text (the tree with its texts filled in), and its preview address. |
| `publish` / `unpublish` | Needs `pages_publish`. |
| `revisions` / `restore` | The history, and putting an old revision back into the draft. |
| `delete_page` | Needs `pages_publish`; a live page must be unpublished first. |

The usual flow: `components` → `put_page` (nl) → `preview` → `set_texts` (en)
→ `publish`.

## The first components

`Page` (title, description, children), `Hero`, `Section`, `Columns`, `Feature`,
`Text`, `Image` (a site path or `https:`; the simulator's pictures need a
session), `ButtonLink`, `Quote`, `PlaceLink` (opens the world at a place, as
`/go` does), `DownloadButtons` (APK, web, server). Each is a view in
`crates/atlas/web/src/cms/components.tsx` and an entry in `registry.json`. A
component with live data (a list of communities) waits for a public API to
read it from. Header, footer and the language switch are the page frame, not
components a page chooses.

**A page puts itself in the site's top bar** (2026-11-17,
`docs/mockups/site-header` v2) with two props of `Page`: `menu={1}`, its
position, and `nav={$t('nav')}`, its word there when the title is too long.
Both are part of the draft, so a page enters the bar when it is published and
leaves it when it is taken down. `PageService::menu` lists them; every page's
data carries the list, and `GET /api/pages?lang=` answers it to the app's bar.

## The operator's side

Admin › Content › **Pages** (frame 4): the list with each language's state,
Preview per language, Publish and Take down; the revisions and Restore are
MCP's for now.
Editing is MCP's in v1; a text editor for translations in the Admin is the
obvious second step, and the data shape already allows it.

## Not in v1

Server-side rendering of the body (v3 adds it); a visual editor; uploading pictures from the
CMS; components with live data; a slug per language; scheduled
publishing; pages federated as ActivityPub articles; the client app showing
these pages.

## The order of the work

1. `registry.json`, the first components, `$t` resolution and drawing, in the
   web UI under `bun test`, with pages from fixtures in `bun run mock`.
2. The JSX-subset parser and its checks in Rust, under `cargo test`, against
   the same registry.
3. Storage, `PageService`, the HTTP routes, the two permissions, `pages.html`
   with its tags from `static_files`, and the Caddy routes on the hosted server.
4. The MCP endpoint.
5. Admin › Content › Pages.
6. In midas: the hub's account, its token, and the tools offered to sessions.

![v1](v1.png)

**v1 (2026-11-14), proposed, superseded by v2.** The same pages, rendered on the
server by a third process in Bun (Mithril's SSR is Bun-only), with its own
`pages.db`, started by `cartopolis-server` as a compiled executable and run as a
systemd unit on the hosted server, and the body hydrated in the browser. Dropped
for v2 as more than the pages need: one more process, deploy target and
database, ~60–90 MB more in the server package, and a renderer whose output must
match the browser's node for node.

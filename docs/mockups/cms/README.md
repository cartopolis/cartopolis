---
topic: cms
triggers: [cms, "content pages", "$t(", crates/atlas/web/src/cms, registry.json, pages.html, "/api/pages", "/mcp", PagesEdit, PagesPublish, hreflang, translation, static_files.rs]
updated: 2026-11-14
---

# Content pages: JSX blocks, every text a `$t`, made through MCP

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
component's schema — `text` (must be a `$t`), `url`, `asset`, `enum`, `number`,
`bool` — so a visible prop (`alt`, `heading`, a button label) cannot take a
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
- **No variables in v1.** A page holds no data to put into a sentence; a
  component that shows live data (a count of communities) owns its sentence as
  a `site.` key with `{n}` in it.

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
into the draft and publishing it. A draft is visible only at a preview address
with a short-lived signed token.

## MCP

A Streamable-HTTP endpoint at **`https://cartopolis.org/mcp`** on the simulator
(`rmcp`, already in the workspace for the client's server), signed in with an
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
`Text`, `Image` (an `asset:` hash from the simulator's content-addressed store),
`ButtonLink`, `Quote`, `PlaceLink` (opens the viewer at a place, as `/go`
does), `CommunityList` (live), `DownloadButtons` (APK, web, server). Each is a
view under `crates/atlas/web/src/cms/components/` and an entry in
`registry.json`. Header, footer and the language switch are the page frame, not
components a page chooses.

## The operator's side

Admin › Content › **Pages** (frame 4): the list with each language's state,
Preview per language, Publish, Unpublish, and the revisions with Restore.
Editing is MCP's in v1; a text editor for translations in the Admin is the
obvious second step, and the data shape already allows it.

## Not in v1

Server-side rendering of the body; a visual editor; uploading pictures from the
CMS (pages use assets already in the store); a slug per language; scheduled
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

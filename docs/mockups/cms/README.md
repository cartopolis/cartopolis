---
topic: cms
triggers: [cms, "content pages", "$t(", pages-server, "/mcp", crates/atlas/web/src/cms, PagesEdit, PagesPublish, hreflang, translation]
updated: 2026-11-14
---

# Content pages: JSX blocks, rendered on the server, in every language

![v1](v1.png)

**v1 (2026-11-14), proposed, nothing built.** Asked for: cartopolis.org gets a
content management system whose pages are built from JSX components, rendered
on the server, and made through MCP; all text goes through `$t` placeholders,
stored with the page, so one page shows in several languages.

The frames: (1) a page as an agent writes it through MCP, (2) the texts stored
for it per language, (3) the same page served at `/nl/…` and `/en/…`, (4) the
operator's list under Admin › Content › Pages.

## What a page is

**A page is JSX that is parsed, never run.** An agent writes
`<Hero heading={$t('hero.heading')}>…</Hero>`; the server parses it into a tree
of known components and their props, checks every prop against the component's
schema, and keeps both the source (what an agent reads back and edits) and the
tree (what is rendered). Nothing in it is ever evaluated, so a page cannot run
code on the server or in a reader's browser, whoever wrote it.

What the parser takes is a small, fixed subset of JSX:

- elements whose name is in the component registry, and nothing else (no `div`,
  no `script`, no lower-case HTML);
- props whose value is a string, a number, `true`/`false`, or `{$t('key')}`;
- children that are elements or `{$t('key')}`.

**Bare text is refused.** `<Text>Welkom</Text>` fails to save with the line and
column; it must be `<Text>{$t('welcome')}</Text>`. That rule is what makes every
visible word translatable, instead of most of them. Each prop is typed in the
component's schema — `text` (must be a `$t`), `url`, `asset`, `enum`, `number`,
`bool` — so a visible prop (`alt`, `heading`, a button label) cannot take a
literal string either, and a URL prop takes only `https:` or a site path.

Rejected: **storing JSX as code** and running it (through `Bun.Transpiler` or a
sandbox): one bad or stolen token would run code on the server; **a JSON tree
as the stored format**: it is what the parser produces anyway, but JSX is what
agents and people write and read most easily, and was what was asked.

## `$t`: texts belong to the page

A page's texts are rows beside it — `(page, key, language) → text` — not a
global translation file in the repository: pages are made at run time, and
their words with them.

- **Keys are scoped to the page.** `$t('hero.heading')` on `/about` and on
  `/download` are two different texts. Keys starting `site.` are shared by every
  page (the header, the footer, a component's own labels).
- **Every page has a source language** — the one it was written in. A reader
  asking for a language the text lacks gets the source text, marked with its own
  `lang` attribute so a screen reader pronounces it right.
- **A translation knows what it translated.** Each stores a hash of the source
  text it was made from; when the source changes, the translation shows as
  *stale* in the list (frame 4) and in MCP, rather than silently describing
  the old sentence.
- **Whole sentences per key.** A text may carry a small inline markup —
  `**bold**`, `*italic*`, `[link](/path)` — rendered to elements, never to HTML,
  so a translator can move the emphasis where their language puts it instead of
  the page splitting a sentence over three keys.
- **No variables in v1.** A page holds no data to put into a sentence; a
  component that shows live data (a count of communities) owns its sentence as
  a `site.` key with `{n}` in it.

## Where a page lives

`/{lang}/{slug}`: `/nl/over`, `/en/over`. One slug for every language (it is a
key, not text), the languages from the server's configuration (`nl`, `en` to
start). `/p/{slug}` redirects to the reader's language by `Accept-Language`, for
links that should not choose. Each page names its other languages in
`<link rel="alternate" hreflang>`, and its title and description are `$t` texts
like the rest. The chat (`/`), World, Admin and the players' pages are not
affected: the Mithril app stays a static app, and links into content pages are
plain links.

## The process that renders them

**A third process, `cartopolis-pages`, in Bun**, beside the simulator and atlas.
Mithril's SSR (`@garage44/mithril`, `ssr/render_to_string.ts`) is Bun-only by
choice, and the components are the web UI's own (`@garage44/mithril/components`,
`theme.css`), so a page looks like the rest of the site.

- **The self-hosted server stays one download.** `bun build --compile` makes the
  process a single executable in the package; `cartopolis-server` starts it as
  it starts atlas (a child it restarts), and its HTTP listener passes `/{lang}/`,
  `/p/`, `/mcp` and `/api/pages` on to it, as `atlas_proxy` does for atlas. On
  the hosted server it is a systemd unit behind Caddy, as atlas is.
- **It owns its data:** `pages.db` (`bun:sqlite`) in the data directory. Reads
  never reach the simulator, so a busy page cannot cost the tick; a crashed
  render cannot end the world (the reason atlas is a process of its own).
- **Who may write is the simulator's answer.** A write carries a bearer — a
  session or an access token from `/tokens` — and the process asks the
  simulator's `/api/admin/me` what it may do (cached a minute), as atlas does.
  Two new permissions in `cartopolis_core::permissions`: **`pages_edit`** (make
  and change drafts) and **`pages_publish`** (put a draft live, take a page
  down). A bot's token can then hold only `pages_edit`, and a person publishes.
- **Render per request, cached per revision.** SSR uses `renderToString` on the
  page's tree with no router (the router singleton is the part of Mithril's SSR
  that is not request-isolated, `ssr.md` in the fork), and the HTML is kept per
  `(page, revision, language)` until the next publish. A component with live
  data (`CommunityList`) fetches in `oninit` on the server, and the cache for
  that page is short.
- **A small bundle hydrates it.** `pages-<hash>.js` holds the component registry
  and the renderer, not the 2.9 MB chat app, so a content page loads like a page.
  The tree and its texts for that language arrive in the SSR state script.

Rejected: **rendering in Rust** — the components would be written twice, once
for the server and once to hydrate; **inside the simulator** — the tick's
runtime (`atlas.md`'s first rule); **pre-rendering to static files at publish**
— live components and a per-request language would need a renderer anyway.

## Drafts, revisions, publishing

A page has one **draft** (its JSX and all its texts, changed freely) and
**revisions**: publishing copies the draft into a new, immutable revision and
makes it live. Changing a text of a live page changes the draft, not what
readers see, until it is published. Going back is publishing an older revision
again. A draft is visible only at a preview address with a short-lived signed
token.

## MCP

A Streamable-HTTP MCP endpoint at **`https://cartopolis.org/mcp`**, signed in
with an access token, so any MCP client adds it with the URL and the token —
Claude Code, the Claude app, the Midas hub. The tools:

| Tool | Does |
|---|---|
| `components` | The registry: every component, its props with their types, what children it takes, one example each. An agent reads this first. |
| `languages` | The configured languages and the default. |
| `list_pages` | Slug, title, source language, live revision, draft changed or not, per language: complete, missing *n*, stale *n*. |
| `get_page` | A page's draft JSX and its texts in every language. |
| `put_page` | Write a draft: JSX plus texts in one or more languages. Refused as a whole with line, column and reason when anything fails: an unknown component or prop, bare text, a `$t` key with no text in the source language. |
| `set_texts` | Texts for one language (a translation), each checked against the draft's keys. |
| `preview` | The draft rendered in one language, as text the agent can read, and the preview address. |
| `publish` / `unpublish` | Needs `pages_publish`. |
| `revisions` / `restore` | The history, and putting an old revision back into the draft. |
| `delete_page` | Needs `pages_publish`; a live page must be unpublished first. |

The usual flow: `components` → `put_page` (nl) → `preview` → `set_texts` (en)
→ a person publishes, or the agent does when its token holds `pages_publish`.

## The first components

`Page` (title, description, children), `Hero`, `Section`, `Columns`, `Feature`,
`Text`, `Image` (an `asset:` hash from the simulator's content-addressed store),
`ButtonLink`, `Quote`, `PlaceLink` (opens the viewer at a place, as `/go`
does), `CommunityList` (live), `DownloadButtons` (APK, web, server). Each is a
file under `crates/atlas/web/src/cms/components/` with its schema beside its
view; adding one is a file and a line in the registry. Header, footer and the
language switch are the page frame, not components a page chooses.

## The operator's side

Admin › Content › **Pages** (frame 4): the list with each language's state,
Preview per language, Publish, Unpublish, and the revisions with Restore.
Editing is MCP's in v1; a text editor for translations in the Admin is the
obvious second step, and the data shape already allows it.

## Not in v1

A visual editor; uploading pictures from the CMS (pages use assets already in
the store); a slug per language; scheduled publishing; pages federated as
ActivityPub articles; the client app showing these pages.

## The order of the work

1. The parser, the registry with the first components, `$t` resolution and
   rendering, under `bun test`, with pages from fixtures in `bun run mock`.
2. `pages.db`, the HTTP API, the two permissions, the process, the proxy in
   `cartopolis-server` and the Caddy routes on the hosted server.
3. The MCP endpoint.
4. Admin › Content › Pages.
5. Deploying: a systemd unit and a `midas deploy` target on the hosted server,
   the compiled executable in the server package.

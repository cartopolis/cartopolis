import './pages.css'

import m from '@garage44/mithril'

import {Frame, PageView, type Visitor, siteText} from './frame'
import {LANG_KEY} from './sitebar'
import {NO_MENU, type PageData, isTextRef, parsePath, resolve} from './tree'

/**
 * The content pages' own small app (`docs/mockups/cms`): `pages.html` and this,
 * not the chat app, so a page about Cartopolis loads like a page.
 *
 * A live page arrives already drawn: the simulator rendered it for this visit
 * (`ssr.tsx`) and put the data it drew from into `#cms-data`, so this takes the
 * HTML over — Mithril keeps the elements and attaches what handlers the
 * components have — without asking the server again. When the server could not
 * render (no renderer, or it failed) the body is empty and this draws it from
 * the same data; when there is no data either, it fetches the page.
 *
 * `?draft` draws the draft instead of the live revision, through the admin route,
 * with the session the web UI keeps (`src/lib/store.ts`): an editor's preview,
 * which needs `pages_edit` on the server and shows nothing to anybody else. A
 * draft is never server-rendered: the session that may see it is in this
 * browser's storage, not in the request.
 */

type State = {kind: 'loading'} | {kind: 'page'; data: PageData} | {kind: 'missing'} | {kind: 'failed'}

/** The web UI's stored session (`src/lib/state.ts`): the player's, and the operator's beside it. */
function stored(): {signup?: {token?: string; username?: string}; profile?: {token?: string}} {
    try {
        return JSON.parse(localStorage.getItem('cartopolis-admin') || '{}') ?? {}
    } catch {
        return {}
    }
}

function sessionToken(): string {
    const s = stored()
    return s.signup?.token || s.profile?.token || ''
}

/**
 * Who is reading, for the bar: signed in when the chat's session is kept here,
 * an operator (Admin in the bar) when its operator half is too. Read after the
 * takeover, never in a view, so the server's HTML and the first browser view
 * stay the same.
 */
function readVisitor(): Visitor | null {
    const s = stored()
    if (!s.signup?.token) return null
    return {admin: !!s.profile?.token, name: s.signup.username || '?'}
}

const where = parsePath(location.pathname)
const draft = new URLSearchParams(location.search).has('draft')
let state: State = {kind: 'loading'}
let visitor: Visitor | null = null

/** The page the server drew this visit from, if it put one in. */
function embedded(): PageData | null {
    const text = document.getElementById('cms-data')?.textContent
    if (!text || draft) return null
    try {
        return JSON.parse(text) as PageData
    } catch {
        return null
    }
}

async function load() {
    if (!where) {
        state = {kind: 'missing'}
        return
    }
    const q = `lang=${encodeURIComponent(where.lang)}`
    const slug = encodeURIComponent(where.slug)
    const token = draft ? sessionToken() : ''
    const res = await fetch(draft ? `/api/admin/pages/${slug}/draft?${q}` : `/api/pages/${slug}?${q}`, {
        headers: token ? {Authorization: `Bearer ${token}`} : {},
    }).catch(() => null)
    if (!res) state = {kind: 'failed'}
    else if (res.status === 404 || res.status === 401 || res.status === 403) state = {kind: 'missing'}
    else if (!res.ok) state = {kind: 'failed'}
    else {
        const data = (await res.json()) as PageData
        state = {data, kind: 'page'}
        const title = data.tree.p?.title
        if (title && isTextRef(title)) {
            const r = resolve(data, title.t)
            if (r) document.title = `${r.text} · Cartopolis`
        }
    }
    m.redraw()
}

export const PagesApp = {
    view() {
        const lang = where?.lang ?? document.documentElement.lang ?? 'en'
        if (state.kind === 'page') return PageView(state.data, where, draft, visitor)
        const message =
            state.kind === 'loading'
                ? siteText(lang, 'site.loading')
                : state.kind === 'missing'
                  ? siteText(lang, 'site.not_found')
                  : siteText(lang, 'site.failed')
        return Frame({
            body: <p class="cms-message">{message}</p>,
            ctx: null,
            draft,
            lang,
            languages: [],
            menu: NO_MENU,
            visitor,
            where,
        })
    },
}

if (typeof document !== 'undefined' && document.body?.dataset.cms !== undefined) {
    const data = embedded()
    if (data) state = {data, kind: 'page'}
    // With the server's HTML in the body, this mount is the takeover.
    m.mount(document.body, PagesApp)
    if (!data) void load()
    // After the takeover: who is reading, and the language they read in, which
    // the app's bar then speaks too.
    visitor = readVisitor()
    if (visitor) m.redraw()
    if (where) {
        try {
            localStorage.setItem(LANG_KEY, where.lang)
        } catch {
            // Private mode: the app falls back to the browser's language.
        }
    }
}

import './pages.css'

import m from '@garage44/mithril'

import {type Ctx, draw, inline, langOf} from './components'
import {type PageData, isTextRef, parsePath, registry, resolve} from './tree'

/**
 * The content pages' own small app (`docs/mockups/cms`): `pages.html` and this,
 * not the chat app, so a page about Cartopolis loads like a page. The server has
 * already put the title, description and language alternates into the HTML; this
 * fetches the page's tree and its texts in the address's language and draws them.
 *
 * `?draft` draws the draft instead of the live revision, through the admin route,
 * with the session the web UI keeps (`src/lib/store.ts`): an editor's preview,
 * which needs `pages_edit` on the server and shows nothing to anybody else.
 */

type State = {kind: 'loading'} | {kind: 'page'; data: PageData} | {kind: 'missing'} | {kind: 'failed'}

function sessionToken(): string {
    try {
        const stored = JSON.parse(localStorage.getItem('cartopolis-admin') || '{}')
        return stored?.signup?.token || stored?.profile?.token || ''
    } catch {
        return ''
    }
}

const where = parsePath(location.pathname)
const draft = new URLSearchParams(location.search).has('draft')
let state: State = {kind: 'loading'}

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

/** Words of the frame before a page has said which languages exist. */
function siteText(lang: string, key: string): string {
    const texts = registry.site_texts[key]
    return texts?.[lang] ?? texts?.en ?? ''
}

function Frame(lang: string, body: m.Children, languages: string[], ctx: Ctx | null) {
    const label = (key: string) => {
        if (!ctx) return siteText(lang, key)
        const r = resolve(ctx.data, key)
        return <span lang={langOf(ctx, r)}>{inline(r)}</span>
    }
    const others = languages.length > 1 && where
    return (
        <div class="cms-frame">
            <header class="cms-top">
                <a class="wordmark" href="/">
                    carto<b>polis</b>
                </a>
                <nav class="cms-nav">
                    <a href="/">{label('site.nav.community')}</a>
                    <a href="/world">{label('site.nav.world')}</a>
                </nav>
                {others ? (
                    <nav aria-label={siteText(lang, 'site.language')} class="cms-langs">
                        {languages.map((l) => (
                            <a
                                aria-current={l === lang ? 'page' : undefined}
                                class={l === lang ? 'on' : ''}
                                href={`/${l}/${where.slug}${draft ? '?draft' : ''}`}
                                hreflang={l}
                                lang={l}
                            >
                                {l.toUpperCase()}
                            </a>
                        ))}
                    </nav>
                ) : null}
            </header>
            {ctx && ctx.data.revision === null ? <div class="cms-draft">{label('site.draft')}</div> : null}
            <main class="cms-main">{body}</main>
        </div>
    )
}

export const PagesApp = {
    view() {
        const lang = where?.lang ?? document.documentElement.lang ?? 'en'
        if (state.kind === 'page') {
            const ctx = {data: state.data}
            return Frame(state.data.lang, draw(ctx, state.data.tree), state.data.languages, ctx)
        }
        const message =
            state.kind === 'loading'
                ? siteText(lang, 'site.loading')
                : state.kind === 'missing'
                  ? siteText(lang, 'site.not_found')
                  : siteText(lang, 'site.failed')
        return Frame(lang, <p class="cms-message">{message}</p>, [], null)
    },
}

if (typeof document !== 'undefined' && document.body?.dataset.cms !== undefined) {
    m.mount(document.body, PagesApp)
    void load()
}

import m from '@garage44/mithril'

import {type Ctx, draw, inline, langOf} from './components'
import {Initial, SiteBar, type Step} from './sitebar'
import {HOME, NO_MENU, type PageData, type SiteMenu, isTextRef, pageHref, resolve, siteText} from './tree'

export {siteText}

/**
 * What a content page draws, on the server and in the browser alike: the site's
 * bar (`sitebar.tsx`) and the page's tree under it.
 *
 * The simulator renders this to HTML for every visit (`ssr.tsx`) and the
 * browser then takes that HTML over (`pages.tsx`), keeping its elements and
 * attaching handlers. That works only while both draw the same tree, node for
 * node, so nothing here may read the browser — `location`, `localStorage`, the
 * clock, the theme: where the page is comes in as `where`, who is visiting as
 * `visitor` (null until after the takeover), and anything else that differs
 * per visitor belongs in a component's `oncreate`. Mithril repairs a difference
 * without saying so (`ssr.test.ts` holds the two to one output).
 */

export interface Where {
    lang: string
    slug: string
}

/** Who is reading, as far as this browser knows: set only after the takeover. */
export interface Visitor {
    name: string
    admin: boolean
}

export interface FrameAttrs {
    lang: string
    body: m.Children
    languages: string[]
    ctx: Ctx | null
    where: Where | null
    draft: boolean
    menu: SiteMenu
    visitor: Visitor | null
}

/** The page's word in the path: its menu label when it has one, else its title. */
function pageStep(ctx: Ctx | null, slug: string, menu: SiteMenu): m.Children {
    const item = menu.pages.find((p) => p.slug === slug)
    if (item) return <span lang={item.label.lang}>{item.label.text}</span>
    const title = ctx?.data.tree.p?.title
    if (!ctx || !isTextRef(title)) return slug
    const r = resolve(ctx.data, title.t)
    return r ? <span lang={langOf(ctx, r)}>{inline(r)}</span> : slug
}

export function Frame(a: FrameAttrs) {
    const {ctx, draft, lang, menu, where} = a
    const label = (key: string) => {
        if (!ctx) return siteText(lang, key)
        const r = resolve(ctx.data, key)
        return <span lang={langOf(ctx, r)}>{inline(r)}</span>
    }
    const home = where?.slug === HOME
    const path: Step[] | null = home ? null : where ? [{label: pageStep(ctx, where.slug, menu)}] : []
    const languages = where && a.languages.length > 1
        ? a.languages.map((l) => ({href: `${pageHref(l, where.slug)}${draft ? '?draft' : ''}`, lang: l}))
        : []
    return (
        <div class="cms-frame">
            {SiteBar({
                account: a.visitor
                    ? <a href="/chat" title={a.visitor.name}>{Initial(a.visitor.name)}</a>
                    : <a class="site-signin" href="/chat">{siteText(lang, 'site.signin')}</a>,
                admin: a.visitor?.admin ?? false,
                here: home ? {kind: 'home'} : {kind: 'page', slug: where?.slug ?? ''},
                lang,
                languages,
                menu,
                path,
            })}
            {ctx && ctx.data.revision === null ? <div class="cms-draft">{label('site.draft')}</div> : null}
            <main class="cms-main">{a.body}</main>
        </div>
    )
}

/** A page, drawn in its frame. */
export function PageView(data: PageData, where: Where | null, draft: boolean, visitor: Visitor | null = null): m.Children {
    const ctx = {data}
    return Frame({
        body: draw(ctx, data.tree),
        ctx,
        draft,
        lang: data.lang,
        languages: data.languages,
        menu: data.menu ?? NO_MENU,
        visitor,
        where,
    })
}

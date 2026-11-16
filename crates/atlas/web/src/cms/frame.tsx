import m from '@garage44/mithril'

import {type Ctx, draw, inline, langOf} from './components'
import {type PageData, pageHref, registry, resolve} from './tree'

/**
 * What a content page draws, on the server and in the browser alike: the frame
 * (wordmark, navigation, language switch) and the page's tree inside it.
 *
 * The simulator renders this to HTML for every visit (`ssr.tsx`) and the
 * browser then takes that HTML over (`pages.tsx`), keeping its elements and
 * attaching handlers. That works only while both draw the same tree, node for
 * node, so nothing here may read the browser — `location`, `localStorage`, the
 * clock, the theme: where the page is comes in as `where`, and anything that
 * differs per visitor belongs in a component's `oncreate`, after the takeover.
 * Mithril repairs a difference without saying so (`ssr.test.ts` holds the two
 * to one output).
 */

export interface Where {
    lang: string
    slug: string
}

/** Words of the frame before a page has said which languages exist. */
export function siteText(lang: string, key: string): string {
    const texts = registry.site_texts[key]
    return texts?.[lang] ?? texts?.en ?? ''
}

export function Frame(lang: string, body: m.Children, languages: string[], ctx: Ctx | null, where: Where | null, draft: boolean) {
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
                    <a href="/chat">{label('site.nav.community')}</a>
                    <a href="/world">{label('site.nav.world')}</a>
                </nav>
                {others ? (
                    <nav aria-label={siteText(lang, 'site.language')} class="cms-langs">
                        {languages.map((l) => (
                            <a
                                aria-current={l === lang ? 'page' : undefined}
                                class={l === lang ? 'on' : ''}
                                href={`${pageHref(l, where.slug)}${draft ? '?draft' : ''}`}
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

/** A page, drawn in its frame. */
export function PageView(data: PageData, where: Where | null, draft: boolean): m.Children {
    const ctx = {data}
    return Frame(data.lang, draw(ctx, data.tree), data.languages, ctx, where, draft)
}

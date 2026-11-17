import m from '@garage44/mithril'

import {HOME, type MenuItem, type SiteMenu, pageHref, siteText} from './tree'

/**
 * The site's top bar, the same on the content pages and in the app
 * (`docs/mockups/site-header` v2). It is drawn from one site map: the front
 * page is the root and the mark is its link; under it the site's pages (the
 * CMS pages a page puts in the menu) on the left and the app's places
 * (Community, World, Admin) on the right; anything deeper is the path on the
 * second line, which starts at the mark on every page but the front page.
 *
 * Drawn on the server for a content page and taken over in the browser, so like
 * `frame.tsx` it reads nothing of the browser: the caller says where the page
 * is and who is signed in.
 */

/** Where the page drawing the bar is in the site map. */
export type Here = {kind: 'home'} | {kind: 'page'; slug: string} | {kind: 'app'; place: AppPlace}

export type AppPlace = 'community' | 'world' | 'admin'

/** Where the language the reader last read the site in is kept, for the app's bar. */
export const LANG_KEY = 'cartopolis-lang'

export const APP_PATHS: Record<AppPlace, string> = {admin: '/admin', community: '/chat', world: '/world'}

/** One step of the path under the bar, after Home. */
export interface Step {
    label: m.Children
    /** Where the step goes; the last step goes nowhere. */
    href?: string
    go?: () => void
}

export interface SiteBarAttrs {
    lang: string
    menu: SiteMenu
    here: Here
    /** The steps after Home, or null for no second line (the front page). */
    path: Step[] | null
    /** Whether the visitor may open Admin. */
    admin: boolean
    unread?: number
    /** The other languages, each with where it goes. */
    languages: {lang: string; href?: string; go?: () => void}[]
    /**
     * For the app: follow one of its own places without loading the page.
     * Content pages leave it out, and every link is a plain one.
     */
    route?: (path: string) => void
    /** The account, or the way to sign in: the caller's. */
    account: m.Children
}

/** The mark's address: the front page in this language once it is published. */
export function homeHref(lang: string, menu: SiteMenu): string {
    return menu.home ? pageHref(lang, HOME) : '/'
}

function link(href: string, attrs: SiteBarAttrs, body: m.Children, cls: string, current: boolean) {
    const route = attrs.route
    const app = Object.values(APP_PATHS).includes(href)
    return (
        <a
            aria-current={current ? 'page' : undefined}
            class={current ? `${cls} is-on` : cls}
            href={href}
            onclick={
                route && app
                    ? (e: MouseEvent) => {
                          if (e.metaKey || e.ctrlKey || e.shiftKey || e.button !== 0) return
                          e.preventDefault()
                          route(href)
                      }
                    : undefined
            }
        >
            {body}
        </a>
    )
}

function pageLabel(item: MenuItem): m.Children {
    return <span lang={item.label.lang}>{item.label.text}</span>
}

export function SiteBar(attrs: SiteBarAttrs): m.Children {
    const {here, lang, menu} = attrs
    const t = (key: string) => siteText(lang, key)
    const onPage = (slug: string) => here.kind === 'page' && here.slug === slug
    const inApp = (place: AppPlace) => here.kind === 'app' && here.place === place
    const pages = menu.pages.map((item) =>
        link(pageHref(lang, item.slug), attrs, pageLabel(item), 'site-link', onPage(item.slug)),
    )
    const app = (place: AppPlace, extra = '') => {
        const body =
            place === 'community' ? (
                <>
                    {t('site.nav.community')}
                    {attrs.unread ? <span class="site-count">{attrs.unread}</span> : null}
                </>
            ) : place === 'world' ? (
                <>
                    <span class="site-live" />
                    {t('site.nav.world')}
                </>
            ) : (
                t('site.nav.admin')
            )
        return link(APP_PATHS[place], attrs, body, `site-link${extra}`, inApp(place))
    }
    const langs = attrs.languages.length > 1 && (
        <nav aria-label={t('site.language')} class="site-langs">
            {attrs.languages.map((l) =>
                l.go ? (
                    <button
                        aria-pressed={l.lang === lang ? 'true' : 'false'}
                        class={l.lang === lang ? 'is-on' : ''}
                        lang={l.lang}
                        onclick={l.go}
                        type="button"
                    >
                        {l.lang.toUpperCase()}
                    </button>
                ) : (
                    <a
                        aria-current={l.lang === lang ? 'page' : undefined}
                        class={l.lang === lang ? 'is-on' : ''}
                        href={l.href}
                        hreflang={l.lang}
                        lang={l.lang}
                    >
                        {l.lang.toUpperCase()}
                    </a>
                ),
            )}
        </nav>
    )
    const home = homeHref(lang, menu)
    return (
        <div class="site-head">
            <header class="site-bar">
                <a
                    aria-current={here.kind === 'home' ? 'page' : undefined}
                    aria-label={t('site.home')}
                    class={here.kind === 'home' ? 'site-mark is-on' : 'site-mark'}
                    href={home}
                >
                    <img alt="" class="mark-dark" src="/icon.svg" />
                    <img alt="" class="mark-light" src="/icon-light.svg" />
                    <span class="wordmark">carto<b>polis</b></span>
                </a>
                <nav aria-label={t('site.nav.site')} class="site-links site-pages">
                    {pages}
                </nav>
                <span class="site-gap" />
                <nav aria-label={t('site.nav.app')} class="site-links site-app">
                    {app('community')}
                    {app('world')}
                    {attrs.admin ? app('admin', ' is-admin') : null}
                </nav>
                <span class="site-sep" />
                {langs}
                <details class="site-more">
                    <summary aria-label={t('site.menu')} title={t('site.menu')}>
                        <svg aria-hidden="true" viewBox="0 0 24 24">
                            <path d="M4 7h16M4 12h16M4 17h16" />
                        </svg>
                    </summary>
                    <div class="site-more-menu">
                        <span class="site-more-head">{t('site.nav.site')}</span>
                        {link(home, attrs, t('site.home'), 'site-more-item', here.kind === 'home')}
                        {menu.pages.map((item) =>
                            link(pageHref(lang, item.slug), attrs, pageLabel(item), 'site-more-item', onPage(item.slug)),
                        )}
                        <span class="site-more-head">{t('site.nav.app')}</span>
                        {app('community', ' site-more-item')}
                        {app('world', ' site-more-item')}
                        {attrs.admin ? app('admin', ' site-more-item is-admin') : null}
                        {langs}
                    </div>
                </details>
                <span class="site-account">{attrs.account}</span>
            </header>
            {attrs.path && (
                <nav aria-label={t('site.path')} class="site-path">
                    <a class="site-step site-step-home" href={home}>
                        <img alt="" class="mark-dark" src="/icon.svg" />
                        <img alt="" class="mark-light" src="/icon-light.svg" />
                        <span>{t('site.home')}</span>
                    </a>
                    {attrs.path.map((step, i) => {
                        const last = i === attrs.path!.length - 1
                        return (
                            <>
                                <span class="site-step-sep">›</span>
                                {last || (!step.go && !step.href) ? (
                                    <span class={last ? 'site-step is-last' : 'site-step'}>{step.label}</span>
                                ) : step.go ? (
                                    <button class="site-step" onclick={step.go} type="button">
                                        {step.label}
                                    </button>
                                ) : (
                                    link(step.href!, attrs, step.label, 'site-step', false)
                                )}
                            </>
                        )
                    })}
                </nav>
            )}
        </div>
    )
}

/** A signed-in visitor's initial in a circle, for a page that has no account menu of its own. */
export function Initial(name: string): m.Children {
    return (
        <span aria-hidden="true" class="site-initial">
            {(name.trim()[0] ?? '?').toUpperCase()}
        </span>
    )
}

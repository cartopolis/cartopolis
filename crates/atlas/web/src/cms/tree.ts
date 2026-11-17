import registryJson from './registry.json'

/**
 * A content page as the simulator stores and serves it (`docs/mockups/cms`):
 * the tree its JSX parsed into, never the JSX run. `crates/simulator/src/pages`
 * writes the same shape; `registry.json` says which components and props exist.
 */

/** `{$t('key')}` in the page's JSX. */
export interface TextRef {
    t: string
}

export type PropValue = string | number | boolean | TextRef

export interface PageNode {
    /** The component's name in the registry. */
    c: string
    p?: Record<string, PropValue>
    ch?: (PageNode | TextRef)[]
}

/** A text with the language it is actually in, which is the page's unless it fell back. */
export interface Resolved {
    text: string
    lang: string
}

/** `GET /api/pages/{slug}?lang=` (and the draft route an editor previews through). */
export interface PageData {
    slug: string
    /** The language this answer is for. */
    lang: string
    /** The server's languages, the first its default: what the switch offers. */
    languages: string[]
    source_lang: string
    /** The live revision drawn, or null for the draft. */
    revision: number | null
    tree: PageNode
    /** Every key the tree and the frame use, already resolved for `lang` with fallbacks. */
    texts: Record<string, Resolved>
    /** The site's menu in `lang`, which the bar draws (`sitebar.tsx`). */
    menu: SiteMenu
}

/** A content page in the site's menu (`pages::MenuItem` on the simulator). */
export interface MenuItem {
    slug: string
    label: Resolved
}

/** `GET /api/pages?lang=`: the site's own pages in its top bar, for a language. */
export interface SiteMenu {
    /** Whether the front page is published: else the mark goes to `/`, the chat. */
    home: boolean
    pages: MenuItem[]
}

export const NO_MENU: SiteMenu = {home: false, pages: []}

export type PropType = 'text' | 'url' | 'image' | 'enum' | 'number' | 'bool' | 'place'

export interface PropSpec {
    type: PropType
    required?: boolean
    values?: string[]
}

export interface ComponentSpec {
    doc: string
    props: Record<string, PropSpec>
    children: 'none' | 'text' | 'blocks'
    allow?: string[]
    example: string
}

export interface Registry {
    root: string
    components: Record<string, ComponentSpec>
    site_texts: Record<string, Record<string, string>>
}

export const registry = registryJson as unknown as Registry

export function isTextRef(v: unknown): v is TextRef {
    return typeof v === 'object' && v !== null && typeof (v as TextRef).t === 'string'
}

/**
 * A link a page may carry: a site path, `https:` or `mailto:`. The simulator
 * refuses anything else in a `url` prop when the page is saved; a link inside a
 * text is only checked here, so a bad one is drawn as its words, not as a link.
 */
export function safeHref(href: string): boolean {
    if (href.startsWith('/')) return !href.startsWith('//') && !href.startsWith('/\\')
    return /^https:\/\/[^\s/]/.test(href) || /^mailto:[^\s]+$/.test(href)
}

/** Words of the site's own (the bar, the frame) in `lang`, before or without a page. */
export function siteText(lang: string, key: string): string {
    const texts = registry.site_texts[key]
    return texts?.[lang] ?? texts?.en ?? ''
}

/** Where a text a page names comes from: the answer, else the registry's own site texts. */
export function resolve(data: PageData, key: string): Resolved | null {
    const found = data.texts[key]
    if (found) return found
    const site = registry.site_texts[key]
    if (!site) return null
    const lang = site[data.lang] !== undefined ? data.lang : data.languages.find((l) => site[l] !== undefined)
    return lang === undefined ? null : {lang, text: site[lang]}
}

/**
 * The page the site's front door shows: `/` sends a reader to `/{lang}/`, which
 * is this page (`pages::HOME` on the simulator).
 */
export const HOME = 'home'

/**
 * `/{lang}/{slug}`, or `/{lang}/` for the home page: the address's language and
 * page, or null for any other path.
 */
export function parsePath(pathname: string): {lang: string; slug: string} | null {
    const match = /^\/([a-z]{2})(?:\/([a-z0-9][a-z0-9-]*))?\/?$/.exec(pathname)
    return match ? {lang: match[1], slug: match[2] ?? HOME} : null
}

/** Where a page lives in one language: the home page at `/{lang}/`, any other at `/{lang}/{slug}`. */
export function pageHref(lang: string, slug: string): string {
    return slug === HOME ? `/${lang}/` : `/${lang}/${slug}`
}

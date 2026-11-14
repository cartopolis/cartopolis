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
}

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

/** Where a text a page names comes from: the answer, else the registry's own site texts. */
export function resolve(data: PageData, key: string): Resolved | null {
    const found = data.texts[key]
    if (found) return found
    const site = registry.site_texts[key]
    if (!site) return null
    const lang = site[data.lang] !== undefined ? data.lang : data.languages.find((l) => site[l] !== undefined)
    return lang === undefined ? null : {lang, text: site[lang]}
}

/** `/{lang}/{slug}`: the address's language and page, or null for any other path. */
export function parsePath(pathname: string): {lang: string; slug: string} | null {
    const match = /^\/([a-z]{2})\/([a-z0-9][a-z0-9-]*)\/?$/.exec(pathname)
    return match ? {lang: match[1], slug: match[2]} : null
}

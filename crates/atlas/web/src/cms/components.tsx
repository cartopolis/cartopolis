import m from '@garage44/mithril'

import {Icon} from '@/lib/icons'

import {type Inline, paragraphs, parseInline} from './markup'
import {type PageData, type PageNode, type PropValue, type Resolved, isTextRef, registry, resolve, safeHref} from './tree'

/**
 * The content pages' components, drawn from a page's tree (`docs/mockups/cms`).
 * Each view gets its props with every `$t` already resolved for the language
 * being drawn, and its children already drawn (`blocks`) or resolved (`text`).
 * What a component may be given is `registry.json`, which the simulator checks a
 * page against when it is saved; a view still treats a missing prop as absent
 * rather than trusting that, because a tree saved by an older registry can meet a
 * newer bundle.
 */

export interface Ctx {
    data: PageData
}

type Props = Record<string, Resolved | string | number | boolean | undefined>

interface ViewArgs {
    ctx: Ctx
    props: Props
    /** `blocks` children, drawn. */
    blocks: m.Children[]
    /** A `text` child, resolved. */
    text: Resolved | null
}

/** The `lang` attribute a text needs: only when it fell back to another language. */
export function langOf(ctx: Ctx, r: Resolved | null | undefined): string | undefined {
    return r && r.lang !== ctx.data.lang ? r.lang : undefined
}

function drawInline(nodes: Inline[]): m.Children {
    return nodes.map((n): m.Children => {
        if (typeof n === 'string') return n
        if ('b' in n) return <strong>{drawInline(n.b)}</strong>
        if ('i' in n) return <em>{drawInline(n.i)}</em>
        // A link a text carries is only checked here: a bad one keeps its words.
        return safeHref(n.a) ? <a href={n.a}>{drawInline(n.ch)}</a> : drawInline(n.ch)
    }) as m.Children
}

/** A text inside one element (a heading, a button): its markup, its line breaks. */
export function inline(r: Resolved | null | undefined): m.Children {
    if (!r) return null
    const lines = r.text.split('\n')
    // Nested arrays are fragments to Mithril; its types only spell one level.
    return lines.map((line, i) => [i ? <br /> : null, drawInline(parseInline(line))]) as unknown as m.Children
}

/** A text as paragraphs, each marked with its language when it fell back. */
export function paras(ctx: Ctx, r: Resolved | null | undefined): m.Children {
    if (!r) return null
    const lang = langOf(ctx, r)
    return paragraphs(r.text).map((p) => <p lang={lang}>{inline({lang: r.lang, text: p})}</p>)
}

function str(v: Props[string]): string | undefined {
    return typeof v === 'string' ? v : undefined
}

function txt(v: Props[string]): Resolved | undefined {
    return typeof v === 'object' ? v : undefined
}

const VIEWS: Record<string, (a: ViewArgs) => m.Children> = {
    ButtonLink: ({ctx, props, text}) => {
        const href = str(props.href)
        if (!href || !safeHref(href)) return null
        const variant = str(props.variant) === 'secondary' ? 'secondary' : 'primary'
        return (
            <a class={`cms-button ${variant}`} href={href} lang={langOf(ctx, text)}>
                {inline(text)}
            </a>
        )
    },

    Columns: ({blocks}) => <div class="cms-columns">{blocks}</div>,

    DownloadButtons: ({ctx}) => {
        const label = (key: string) => {
            const r = resolve(ctx.data, key)
            return <span lang={langOf(ctx, r)}>{inline(r)}</span>
        }
        return (
            <div class="cms-downloads">
                <a class="cms-button primary" href="/apk/cartopolis.apk">
                    {label('site.download.android')}
                </a>
                <a class="cms-button secondary" href="/world">
                    {label('site.download.web')}
                </a>
                <a class="cms-button secondary" href="/server/">
                    {label('site.download.server')}
                </a>
            </div>
        )
    },

    Feature: ({ctx, props, text}) => {
        const heading = txt(props.heading)
        const icon = str(props.icon)
        return (
            <div class="cms-feature">
                {icon ? Icon(icon, 'cms-feature-icon') : null}
                <h3 lang={langOf(ctx, heading)}>{inline(heading)}</h3>
                {paras(ctx, text)}
            </div>
        )
    },

    Hero: ({ctx, props, blocks}) => {
        const heading = txt(props.heading)
        const image = str(props.image)
        return (
            <header class={`cms-hero${image && safeHref(image) ? ' with-image' : ''}`}>
                {image && safeHref(image) ? <img alt="" class="cms-hero-image" src={image} /> : null}
                <div class="cms-hero-body">
                    <h1 lang={langOf(ctx, heading)}>{inline(heading)}</h1>
                    {blocks}
                </div>
            </header>
        )
    },

    Image: ({ctx, props}) => {
        const src = str(props.src)
        if (!src || !safeHref(src)) return null
        const alt = txt(props.alt)
        const caption = txt(props.caption)
        return (
            <figure class="cms-image">
                <img alt={alt?.text ?? ''} lang={langOf(ctx, alt)} loading="lazy" src={src} />
                {caption ? <figcaption lang={langOf(ctx, caption)}>{inline(caption)}</figcaption> : null}
            </figure>
        )
    },

    Page: ({blocks}) => <article class="cms-page">{blocks}</article>,

    PlaceLink: ({ctx, props, text}) => {
        const place = str(props.place)
        if (!place) return inline(text)
        return (
            <a class="cms-place" href={`/go?place=${encodeURIComponent(place)}`} lang={langOf(ctx, text)}>
                {Icon('location_on', 'cms-place-icon')}
                {inline(text)}
            </a>
        )
    },

    Quote: ({ctx, props, text}) => {
        const by = txt(props.by)
        return (
            <blockquote class="cms-quote">
                {paras(ctx, text)}
                {by ? <footer lang={langOf(ctx, by)}>— {inline(by)}</footer> : null}
            </blockquote>
        )
    },

    Section: ({ctx, props, blocks}) => {
        const heading = txt(props.heading)
        return (
            <section class="cms-section">
                {heading ? <h2 lang={langOf(ctx, heading)}>{inline(heading)}</h2> : null}
                {blocks}
            </section>
        )
    },

    Text: ({ctx, props, text}) => <div class={`cms-text${props.lead === true ? ' lead' : ''}`}>{paras(ctx, text)}</div>,
}

/** Every component the registry names has a view, and nothing else does (`cms.test.ts`). */
export const VIEW_NAMES = Object.keys(VIEWS)

function prop(ctx: Ctx, v: PropValue): Props[string] {
    if (isTextRef(v)) return resolve(ctx.data, v.t) ?? undefined
    return v
}

/** Draw one node of a page's tree, and everything under it. */
export function draw(ctx: Ctx, node: PageNode): m.Children {
    const spec = registry.components[node.c]
    const view = VIEWS[node.c]
    if (!spec || !view) {
        // A tree from a newer registry than this bundle: leave the block out.
        console.warn(`content page: no component ${node.c}`)
        return null
    }
    const props: Props = {}
    for (const [name, value] of Object.entries(node.p ?? {})) props[name] = prop(ctx, value)
    const children = node.ch ?? []
    const blocks = spec.children === 'blocks' ? children.map((c) => (isTextRef(c) ? null : draw(ctx, c))) : []
    const first = children[0]
    const text = spec.children === 'text' && first && isTextRef(first) ? resolve(ctx.data, first.t) : null
    return view({blocks, ctx, props, text})
}

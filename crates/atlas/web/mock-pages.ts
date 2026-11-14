import type {PageData, PageNode, Resolved} from './src/cms/tree'

import {isTextRef, registry} from './src/cms/tree'

/**
 * `bun run mock`'s content pages (`docs/mockups/cms`): `/nl/over` and `/en/over`
 * with no simulator. The tree is written as the simulator's parser would produce
 * it from the page's JSX; the texts fall back as `crates/simulator/src/pages`
 * does — the asked language, else the page's source language, else the site's
 * own texts in `registry.json`.
 */

const LANGUAGES = ['nl', 'en']

interface MockPage {
    source_lang: string
    tree: PageNode
    texts: Record<string, Record<string, string>>
}

const t = (key: string) => ({t: key})

const PAGES: Record<string, MockPage> = {
    over: {
        source_lang: 'nl',
        texts: {
            nl: {
                description: 'Cartopolis is een gratis plek waar buren praten, plannen en bouwen, op de kaart van hun eigen stad.',
                'hero.cta': 'Maak een account',
                'hero.heading': 'Jouw stad, gedeeld',
                'hero.lead': 'Praat, plan en bouw samen met je buren — **op de kaart** van je eigen buurt.',
                'markt.link': 'Kijk op de Vismarkt',
                'quote.by': 'een buurtbewoner uit de Oosterpoort',
                'quote.text': 'Eindelijk zie ik waar mijn buren het over hebben.',
                title: 'Over Cartopolis',
                'why.chat.body': 'Versleutelde gesprekken, met je buren en je buurt.',
                'why.chat.heading': 'Samen praten',
                'why.heading': 'Waarom Cartopolis',
                'why.map.body': 'Gebouwd uit open data: elk huis staat waar het staat.',
                'why.map.heading': 'De echte stad',
                'why.server.body': 'Draai je eigen server; je data blijft van jou. [Hoe dat werkt](/server/).',
                'why.server.heading': 'Van jullie zelf',
            },
            en: {
                description: 'Cartopolis is a free place where neighbours talk, plan and build, on the map of their own city.',
                'hero.cta': 'Create an account',
                'hero.heading': 'Your city, shared',
                'hero.lead': 'Talk, plan and build with your neighbours — **on the map** of your own street.',
                'markt.link': 'Look at the Vismarkt',
                title: 'About Cartopolis',
                'why.heading': 'Why Cartopolis',
                'why.map.body': 'Built from open data: every house is where it is.',
                'why.map.heading': 'The real city',
                'why.server.body': 'Run your own server; your data stays yours. [How that works](/server/).',
                'why.server.heading': 'Your own',
                // why.chat.* and the quote are left out on purpose: they show the Dutch.
            },
        },
        tree: {
            c: 'Page',
            ch: [
                {
                    c: 'Hero',
                    ch: [
                        {c: 'Text', ch: [t('hero.lead')], p: {lead: true}},
                        {c: 'ButtonLink', ch: [t('hero.cta')], p: {href: '/register'}},
                    ],
                    p: {heading: t('hero.heading'), image: '/img/groningen.svg'},
                },
                {
                    c: 'Section',
                    ch: [
                        {
                            c: 'Columns',
                            ch: [
                                {c: 'Feature', ch: [t('why.map.body')], p: {heading: t('why.map.heading'), icon: 'location_on'}},
                                {c: 'Feature', ch: [t('why.chat.body')], p: {heading: t('why.chat.heading'), icon: 'chat'}},
                                {c: 'Feature', ch: [t('why.server.body')], p: {heading: t('why.server.heading'), icon: 'lock'}},
                            ],
                        },
                        {c: 'Quote', ch: [t('quote.text')], p: {by: t('quote.by')}},
                        {c: 'PlaceLink', ch: [t('markt.link')], p: {place: '53.2165,6.5629,120m,-25,0'}},
                    ],
                    p: {heading: t('why.heading')},
                },
                {c: 'Section', ch: [{c: 'DownloadButtons'}]},
            ],
            p: {description: t('description'), title: t('title')},
        },
    },
}

function keysOf(node: PageNode, into: Set<string>) {
    for (const v of Object.values(node.p ?? {})) if (isTextRef(v)) into.add(v.t)
    for (const c of node.ch ?? []) {
        if (isTextRef(c)) into.add(c.t)
        else keysOf(c, into)
    }
}

function pageData(slug: string, lang: string): PageData | null {
    const page = PAGES[slug]
    if (!page || !LANGUAGES.includes(lang)) return null
    const keys = new Set<string>(Object.keys(registry.site_texts))
    keysOf(page.tree, keys)
    const texts: Record<string, Resolved> = {}
    for (const key of keys) {
        const own = page.texts[lang]?.[key]
        const source = page.texts[page.source_lang]?.[key]
        const site = registry.site_texts[key]
        if (own !== undefined) texts[key] = {lang, text: own}
        else if (source !== undefined) texts[key] = {lang: page.source_lang, text: source}
        else if (site?.[lang] !== undefined) texts[key] = {lang, text: site[lang]}
        else if (site?.[page.source_lang] !== undefined) texts[key] = {lang: page.source_lang, text: site[page.source_lang]}
    }
    return {languages: LANGUAGES, lang, revision: 3, slug, source_lang: page.source_lang, texts, tree: page.tree}
}

export function mockPages(req: Request, url: URL): Response | null {
    const live = /^\/api\/pages\/([a-z0-9-]+)$/.exec(url.pathname)
    const draft = /^\/api\/admin\/pages\/([a-z0-9-]+)\/draft$/.exec(url.pathname)
    const match = live ?? draft
    if (!match || req.method !== 'GET') return null
    const data = pageData(match[1], url.searchParams.get('lang') ?? LANGUAGES[0])
    if (!data) return Response.json({error: 'no such page'}, {status: 404})
    return Response.json(draft ? {...data, revision: null} : data)
}

import {describe, expect, test} from 'bun:test'

import m from '@garage44/mithril'
import mServer from '@garage44/mithril/server'

import {PageView} from './frame'
import {renderPage} from './ssr'
import {type PageData, pageHref, parsePath, registry} from './tree'

/** A page with every component in it, its texts in Dutch with one falling back to English. */
function everything(slug: string): PageData {
    const t = (key: string) => ({t: key})
    return {
        lang: 'nl',
        languages: ['nl', 'en'],
        menu: {
            home: true,
            pages: [
                {label: {lang: 'nl', text: 'Over'}, slug: 'over'},
                {label: {lang: 'nl', text: 'Je eigen server'}, slug: 'server'},
            ],
        },
        revision: 3,
        slug,
        source_lang: 'nl',
        texts: {
            alt: {lang: 'nl', text: 'De Grote Markt'},
            by: {lang: 'nl', text: 'Een buur'},
            button: {lang: 'nl', text: 'Doe mee'},
            description: {lang: 'nl', text: 'Over ons'},
            feature: {lang: 'nl', text: 'Praat op de kaart.'},
            heading: {lang: 'nl', text: 'Jouw stad, **gedeeld**'},
            lead: {lang: 'en', text: 'Talk and build *together*.'},
            place: {lang: 'nl', text: 'Naar de markt'},
            quote: {lang: 'nl', text: 'Eindelijk.'},
            title: {lang: 'nl', text: 'Over'},
        },
        tree: {
            c: 'Page',
            ch: [
                {c: 'Hero', ch: [{c: 'Text', ch: [t('lead')], p: {lead: true}}, {c: 'ButtonLink', ch: [t('button')], p: {href: '/chat'}}, {c: 'DownloadButtons'}], p: {heading: t('heading')}},
                {
                    c: 'Section',
                    ch: [
                        {
                            c: 'Columns',
                            ch: [
                                {c: 'Feature', ch: [t('feature')], p: {heading: t('heading'), icon: 'chat'}},
                                {c: 'Image', p: {alt: t('alt'), src: '/img/groningen.svg'}},
                                {c: 'Quote', ch: [t('quote')], p: {by: t('by')}},
                            ],
                        },
                        {c: 'PlaceLink', ch: [t('place')], p: {place: '53.2190,6.5680'}},
                    ],
                    p: {heading: t('heading')},
                },
            ],
            p: {description: t('description'), title: t('title')},
        },
    }
}

describe('the server render', () => {
    test('draws every component, with its texts', async () => {
        const used = new Set<string>()
        const walk = (n: PageData['tree']) => {
            used.add(n.c)
            for (const c of n.ch ?? []) if ('c' in c) walk(c)
        }
        const data = everything('over')
        walk(data.tree)
        expect([...used].sort()).toEqual(Object.keys(registry.components).sort())

        const html = await renderPage(data)
        for (const text of ['Jouw stad, <strong>gedeeld</strong>', 'Doe mee', 'Praat op de kaart.', 'Eindelijk.', 'Naar de markt']) {
            expect(html).toContain(text)
        }
        // The text that fell back says which language it is in.
        expect(html).toContain('<p lang="en">Talk and build <em>together</em>.</p>')
        expect(html).not.toContain('undefined')
        expect(html).not.toContain('[object')
    })

    test('escapes what a page says', async () => {
        const data = everything('over')
        data.texts.quote = {lang: 'nl', text: '<script>alert(1)</script> & "zo"'}
        const html = await renderPage(data)
        expect(html).not.toContain('<script>')
        expect(html).toContain('&lt;script&gt;')
    })

    // The bar is drawn from the site map (docs/mockups/site-header v2): the
    // mark is the front page's item, and every other page has a path from it.
    test('draws the site bar: the menu, where you are, and the path from Home', async () => {
        const home = await renderPage(everything('home'))
        expect(home).toContain('href="/nl/" class="site-mark is-on"')
        expect(home).toContain('href="/nl/over"')
        expect(home).toContain('href="/nl/server"')
        expect(home).toContain('href="/chat"')
        expect(home).toContain('href="/world"')
        expect(home).not.toContain('site-path')

        const over = await renderPage(everything('over'))
        expect(over).toContain('href="/nl/" class="site-mark"')
        expect(over).toContain('aria-current="page" href="/nl/over" class="site-link is-on"')
        expect(over).toMatch(/class="site-path".*Home.*›.*Over/)
        // Signed out as far as the server knows; the browser says otherwise after the takeover.
        expect(over).toContain('href="/chat" class="site-signin"')
        expect(over).not.toContain('is-admin')
    })

    test('a server with no front page sends the mark to /', async () => {
        const data = everything('over')
        data.menu = {home: false, pages: []}
        expect(await renderPage(data)).toContain('href="/" class="site-mark"')
    })

    // The browser takes the server's HTML over node for node, and Mithril
    // repairs a difference silently: the two must draw the same. The browser
    // knows where it is from its address, the server from the page.
    test("is what the browser draws first, at the page's own address", async () => {
        for (const slug of ['over', 'home']) {
            const data = everything(slug)
            const where = parsePath(pageHref(data.lang, data.slug))
            const browser = (await mServer.renderToString(m({view: () => PageView(data, where, false)}))).html
            expect(await renderPage(data)).toBe(browser)
        }
    })
})

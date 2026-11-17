import {describe, expect, test} from 'bun:test'

import {ICONS, SYMBOLS} from '@/lib/icons'

import {VIEW_NAMES} from './components'
import {paragraphs, parseInline} from './markup'
import {NO_MENU, type PageData, pageHref, parsePath, registry, resolve, safeHref} from './tree'

describe('the registry', () => {
    test('every component has a view, and every view a component', () => {
        expect([...VIEW_NAMES].sort()).toEqual(Object.keys(registry.components).sort())
    })

    test('the root is a component that takes blocks', () => {
        expect(registry.components[registry.root]?.children).toBe('blocks')
    })

    test('allow names only components', () => {
        for (const spec of Object.values(registry.components)) {
            for (const name of spec.allow ?? []) expect(registry.components[name]).toBeDefined()
        }
    })

    test('every icon a page may choose can be drawn', () => {
        for (const value of registry.components.Feature.props.icon.values ?? []) {
            expect(ICONS[value] ?? SYMBOLS[value]).toBeDefined()
        }
    })

    test('every site text is in every language the mock serves', () => {
        for (const [key, texts] of Object.entries(registry.site_texts)) {
            expect(key.startsWith('site.')).toBe(true)
            expect(Object.keys(texts).sort()).toEqual(['en', 'nl'])
        }
    })
})

describe('inline markup', () => {
    test('bold, italic and links', () => {
        expect(parseInline('a **b** *c* [d](/e)')).toEqual(['a ', {b: ['b']}, ' ', {i: ['c']}, ' ', {a: '/e', ch: ['d']}])
    })

    test('nested', () => {
        expect(parseInline('**a *b***')).toEqual([{b: ['a ', {i: ['b']}]}])
        expect(parseInline('[**a**](/x)')).toEqual([{a: '/x', ch: [{b: ['a']}]}])
    })

    test('a marker with no partner stays a character', () => {
        expect(parseInline('5 * 3 = 15')).toEqual(['5 * 3 = 15'])
        expect(parseInline('**open')).toEqual(['**open'])
        expect(parseInline('[no link]')).toEqual(['[no link]'])
        expect(parseInline('[spaced](a b)')).toEqual(['[spaced](a b)'])
    })

    test('a backslash writes the character', () => {
        expect(parseInline('\\*not\\*')).toEqual(['*not*'])
    })

    test('paragraphs split on a blank line', () => {
        expect(paragraphs('one\nline\n\n  two \n \nthree')).toEqual(['one\nline', 'two', 'three'])
    })
})

describe('links', () => {
    test('site paths, https and mailto only', () => {
        for (const ok of ['/register', '/server/', 'https://example.org/x', 'mailto:a@b.nl']) expect(safeHref(ok)).toBe(true)
        for (const bad of ['//evil.org', '/\\evil.org', 'javascript:alert(1)', 'http://plain.org', 'data:text/html,x', 'https://']) {
            expect(safeHref(bad)).toBe(false)
        }
    })
})

describe('addresses', () => {
    test('/{lang}/{slug}', () => {
        expect(parsePath('/nl/over')).toEqual({lang: 'nl', slug: 'over'})
        expect(parsePath('/en/over/')).toEqual({lang: 'en', slug: 'over'})
        expect(parsePath('/nl/')).toEqual({lang: 'nl', slug: 'home'})
        expect(parsePath('/en')).toEqual({lang: 'en', slug: 'home'})
        expect(pageHref('nl', 'home')).toBe('/nl/')
        expect(pageHref('en', 'over')).toBe('/en/over')
        expect(parsePath('/nl/over/meer')).toBeNull()
        expect(parsePath('/admin')).toBeNull()
        expect(parsePath('/nl/Over')).toBeNull()
    })
})

describe('texts', () => {
    const data: PageData = {
        lang: 'en',
        languages: ['nl', 'en'],
        menu: NO_MENU,
        revision: 1,
        slug: 'over',
        source_lang: 'nl',
        texts: {title: {lang: 'nl', text: 'Over'}},
        tree: {c: 'Page'},
    }

    test("the answer's own resolution wins", () => {
        expect(resolve(data, 'title')).toEqual({lang: 'nl', text: 'Over'})
    })

    test('a site text the answer lacks comes from the registry', () => {
        expect(resolve(data, 'site.nav.world')).toEqual({lang: 'en', text: 'World'})
    })

    test('an unknown key is nothing', () => {
        expect(resolve(data, 'nope')).toBeNull()
    })
})

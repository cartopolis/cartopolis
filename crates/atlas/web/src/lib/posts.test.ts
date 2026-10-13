import {describe, expect, test} from 'bun:test'

import {parsePlace, placeLink, suggestHandle} from './posts'

describe('public posts', () => {
    test('a suggested handle is what the server accepts', () => {
        expect(suggestHandle('Buurt Oosterpoort')).toBe('buurt_oosterpoort')
        expect(suggestHandle('Oosterpoort Café')).toBe('oosterpoort_cafe')
        expect(suggestHandle('  --  ')).toBe('community')
        expect(suggestHandle('x'.repeat(40)).length).toBe(30)
    })

    test('a place link reads back into its place and view', () => {
        const p = parsePlace('https://cartopolis.org/go?place=53.21940,6.56650,120m,-12.5,45.0')
        expect(p).toEqual({latitude: 53.2194, longitude: 6.5665, view: {altitude: 120, pitch: -12.5, yaw: 45}})
        expect(placeLink(p!)).toBe('/go?place=53.21940,6.56650,120m,-12.5,45.0')
        expect(parsePlace('53.2, 6.5')).toEqual({latitude: 53.2, longitude: 6.5})
    })

    test('what is not a place on Earth is refused', () => {
        expect(parsePlace('95, 6.5')).toBeNull()
        expect(parsePlace('https://mastodon.social/@anna/1')).toBeNull()
        expect(parsePlace('hello')).toBeNull()
        expect(parsePlace('')).toBeNull()
    })
})

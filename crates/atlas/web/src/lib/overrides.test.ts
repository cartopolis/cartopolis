import {describe, expect, test} from 'bun:test'

import {pandOf, requestedPand} from '@/lib/overrides'

describe('arriving from the viewer', () => {
    test('reads the building the building card asked for', () => {
        // The Rust side of this link is `building_card::override_url`.
        expect(requestedPand('?override=0014100010953806')).toBe('0014100010953806')
    })

    test('ignores anything that is not a building number', () => {
        expect(requestedPand('')).toBe('')
        expect(requestedPand('?override=')).toBe('')
        expect(requestedPand('?override=<script>')).toBe('')
    })

    test('accepts the full identificatie people copy', () => {
        expect(pandOf(' NL.IMBAG.Pand.0014100010953806 ')).toBe('0014100010953806')
        expect(requestedPand('?override=NL.IMBAG.Pand.0014100010953806')).toBe('0014100010953806')
    })
})

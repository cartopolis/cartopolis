import {describe, expect, test} from 'bun:test'

import {EXPIRY_CHOICES, expiryAfter} from './tokens'

describe('expiryAfter', () => {
    test('never is no expiry, and is the first choice', () => {
        expect(expiryAfter('never')).toBeNull()
        expect(EXPIRY_CHOICES[0].value).toBe('never')
    })

    test('a number of days counts from now, in unix seconds', () => {
        const now = Date.UTC(2026, 9, 15)
        expect(expiryAfter('30', now)).toBe(now / 1000 + 30 * 86400)
    })

    test('every offered choice parses', () => {
        for (const c of EXPIRY_CHOICES.slice(1)) {
            expect(expiryAfter(c.value)).toBeGreaterThan(Date.now() / 1000)
        }
    })
})

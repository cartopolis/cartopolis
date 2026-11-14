import {describe, expect, test} from 'bun:test'

import {ago, duration} from './account-stats'

describe('ago', () => {
    const now = 1_800_000_000_000
    const at = (secondsBack: number) => now / 1000 - secondsBack

    test('reads as a phone list would say it', () => {
        expect(ago(at(30), now)).toBe('just now')
        expect(ago(at(25 * 60), now)).toBe('25 min ago')
        expect(ago(at(3 * 3600), now)).toBe('3 h ago')
        expect(ago(at(30 * 3600), now)).toBe('yesterday')
        expect(ago(at(4 * 86_400), now)).toBe('4 days ago')
        expect(ago(at(21 * 86_400), now)).toBe('3 weeks ago')
    })

    test('says so when the sessions kept do not reach back far enough', () => {
        expect(ago(null, now)).toBe('not seen lately')
    })
})

test('duration', () => {
    expect(duration(45)).toBe('45 min')
    expect(duration(120)).toBe('2 h')
    expect(duration(200)).toBe('3 h 20 min')
})

import {describe, expect, test} from 'bun:test'

import {clips, cueFor, SAMPLE_RATE, wantedLoop} from './call-tones'

describe('call tones', () => {
    test('the phone rules for which tone rings, as the app has them', () => {
        expect(wantedLoop(false, false, false)).toBe(null)
        expect(wantedLoop(true, false, false)).toBe('ringtone')
        expect(wantedLoop(true, true, true)).toBe('waiting')
        expect(wantedLoop(false, true, false)).toBe('ringback')
        expect(wantedLoop(true, true, false)).toBe('ringback')
        expect(wantedLoop(false, true, true)).toBe(null)
    })

    test('a cue marks each change in who is in the call', () => {
        expect(cueFor(null, 0)).toBe(null)
        expect(cueFor(0, 1)).toBe('joined')
        expect(cueFor(2, 1)).toBe('left')
        expect(cueFor(1, 0)).toBe(null)
        expect(cueFor(1, null)).toBe('ended')
        expect(cueFor(null, null)).toBe(null)
    })

    test('every clip has its length and headroom', () => {
        const lengths = {ended: 0.8, joined: 0.5, left: 0.5, ringback: 5, ringtone: 3, waiting: 3}
        for (const [name, secs] of Object.entries(lengths)) {
            const samples = clips[name as keyof typeof clips]()
            expect(samples.length).toBe(Math.floor(secs * SAMPLE_RATE))
            const peak = samples.reduce((m, v) => Math.max(m, Math.abs(v)), 0)
            expect(peak).toBeGreaterThan(0.05)
            // Clipping manufactures the harsh top end a tone must not have.
            expect(peak).toBeLessThan(1)
        }
    })
})

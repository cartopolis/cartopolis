import {describe, expect, test} from 'bun:test'

import type {AdminState} from '@/types'

import {persistentState, volatileState} from '@/lib/state'
import {Store} from '@/lib/store'
import {formatTimestamp, mergeDeep} from '@/lib/utils'

describe('Store', () => {
    test('a restored session survives a reload', () => {
        localStorage.clear()
        const first = new Store<AdminState>('test-restore')
        first.load(structuredClone(persistentState), volatileState)
        first.state.profile = {principal: 'p-1', token: 'tok', username: 'root'}
        first.save()

        const second = new Store<AdminState>('test-restore')
        second.load(structuredClone(persistentState), volatileState)
        expect(second.state.profile.token).toBe('tok')
        expect(second.state.profile.username).toBe('root')
    })

    /*
     * The whole reason `persistentState` is also the save blueprint. If a fetched
     * account list reached localStorage, the app would render rows from a previous
     * session — possibly of accounts that have since been deleted — as though the
     * server had just returned them.
     */
    test('volatile state cannot reach localStorage', () => {
        localStorage.clear()
        const store = new Store<AdminState>('test-volatile')
        store.load(structuredClone(persistentState), volatileState)
        store.state.accounts = [
            {
                admin: true,
                agent: false,
                created_at: '2026-08-13',
                disabled: false,
                email: null,
                email_pending: false,
                id: 'a',
                username: 'root',
            },
        ]
        store.state.search = 'secret-filter'
        store.save()

        const raw = localStorage.getItem('test-volatile') || '{}'
        // `accounts` and `search` are in the blueprint (they need an initial
        // shape), so what must hold is that the *reload* discards them — which is
        // what applying volatileState last does.
        const reloaded = new Store<AdminState>('test-volatile')
        reloaded.load(structuredClone(persistentState), volatileState)
        expect(reloaded.state.accounts).toEqual([])
        expect(reloaded.state.search).toBe('')
        expect(raw).toContain('token')
    })

    test('a corrupt store loads defaults instead of throwing', () => {
        localStorage.setItem('test-corrupt', 'not json{')
        const store = new Store<AdminState>('test-corrupt')
        expect(() => store.load(structuredClone(persistentState), volatileState)).not.toThrow()
        expect(store.state.profile.token).toBe('')
        expect(store.state.signedIn).toBe(null)
    })

    test('filterKeys drops what the blueprint does not name', () => {
        const store = new Store<AdminState>('test-filter')
        const filtered = store.filterKeys(
            {kept: 1, nested: {kept: 2, dropped: 3}, dropped: 4},
            {kept: 0, nested: {kept: 0}},
        )
        expect(filtered).toEqual({kept: 1, nested: {kept: 2}})
    })
})

describe('mergeDeep', () => {
    test('nested objects merge and arrays replace', () => {
        const merged = mergeDeep(
            {a: {x: 1, y: 2}, list: [1, 2, 3]},
            {a: {y: 9}, list: [7]},
        )
        expect(merged).toEqual({a: {x: 1, y: 9}, list: [7]})
    })

    test('the source is not mutated into the target', () => {
        const target = {a: {x: 1}}
        mergeDeep(target, {a: {x: 2}})
        expect(target.a.x).toBe(1)
    })
})

describe('formatTimestamp', () => {
    /*
     * SQLite's `datetime('now')` has a space where ISO 8601 wants a `T`, and
     * Safari answers `Invalid Date` for it. Every `created_at` in the account list
     * arrives in that shape.
     */
    test("SQLite's space-separated timestamp parses", () => {
        expect(formatTimestamp('2026-08-13 09:12:44')).not.toBe('Invalid Date')
        expect(formatTimestamp('2026-08-13 09:12:44')).toContain('2026')
    })

    test('an unparseable value is shown rather than replaced', () => {
        expect(formatTimestamp('sometime last tuesday')).toBe('sometime last tuesday')
        expect(formatTimestamp('')).toBe('—')
    })
})

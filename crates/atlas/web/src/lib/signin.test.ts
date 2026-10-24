import {describe, expect, test} from 'bun:test'

import type {AdminProvider, LinkedIdentity} from '@/types'

import {
    canRemove,
    createLabel,
    customId,
    fediverseHost,
    linkable,
    mayEnable,
    moved,
    newAccountChoices,
    providerLine,
    providerStatus,
    reauthValid,
    reauthWith,
    rememberServer,
    safeReturn,
    startUrl,
    welcomeQuery,
} from './signin'

const provider = (over: Partial<AdminProvider> = {}): AdminProvider => ({
    accounts: 0,
    client_id: '',
    custom: false,
    enabled: false,
    id: 'discord',
    kind: 'discord',
    links: 0,
    name: 'Discord',
    needs_keys: true,
    new_accounts: 'anyone',
    position: 0,
    redirect_url: 'https://cartopolis.org/api/auth/discord/callback',
    secret_set: false,
    setup_url: 'https://discord.com/developers/applications',
    ...over,
})

const identity = (provider: string, linked_at: number): LinkedIdentity => ({
    handle: 'anna',
    id: `${provider}-1`,
    kind: 'oidc',
    linked_at,
    provider,
    provider_name: provider,
})

describe('where sign-in sends the browser', () => {
    test('a return stays on this site', () => {
        expect(safeReturn('/tokens')).toBe('/tokens')
        expect(safeReturn('/?show=abc')).toBe('/?show=abc')
        // Every way of naming another site from a path is refused.
        expect(safeReturn('https://evil.example/')).toBe('/')
        expect(safeReturn('//evil.example/')).toBe('/')
        expect(safeReturn('/\\evil.example')).toBe('/')
        expect(safeReturn('javascript:alert(1)')).toBe('/')
        expect(safeReturn('')).toBe('/')
        expect(safeReturn(null)).toBe('/')
    })

    test('the start address carries the return, the app code and the server', () => {
        expect(startUrl('discord', '/buildings')).toBe('/api/auth/discord/start?return=%2Fbuildings')
        expect(startUrl('mastodon', '/', {server: 'mastodon.nl'})).toBe(
            '/api/auth/mastodon/start?return=%2F&server=mastodon.nl',
        )
        expect(startUrl('google', '//x', {app: 'KQ7-4MD'})).toBe('/api/auth/google/start?return=%2F&app=KQ7-4MD')
    })

    test('/welcome reads a ticket or an error', () => {
        expect(welcomeQuery('?ticket=abc')).toEqual({ticket: 'abc'})
        expect(welcomeQuery('?error=Registrations%20are%20closed')).toEqual({error: 'Registrations are closed'})
        expect(welcomeQuery('?ticket=abc&error=no')).toEqual({error: 'no'})
        expect(welcomeQuery('')).toBeNull()
    })
})

describe('the fediverse question', () => {
    test('finds the server in an address, a handle or a link', () => {
        expect(fediverseHost('@anna@mastodon.nl')).toBe('mastodon.nl')
        expect(fediverseHost('anna@Mastodon.NL')).toBe('mastodon.nl')
        expect(fediverseHost('  groningen.social ')).toBe('groningen.social')
        expect(fediverseHost('https://mastodon.social/@anna')).toBe('mastodon.social')
        expect(fediverseHost('mastodon.nl/@anna')).toBe('mastodon.nl')
    })

    test('a bare name is not a server', () => {
        expect(fediverseHost('anna')).toBeNull()
        expect(fediverseHost('@anna')).toBeNull()
        expect(fediverseHost('')).toBeNull()
        expect(fediverseHost('not a server.nl')).toBeNull()
    })

    test('remembers the newest first, once each, at most five', () => {
        expect(rememberServer(['a.nl', 'b.nl'], 'b.nl')).toEqual(['b.nl', 'a.nl'])
        expect(rememberServer(['a.nl', 'b.nl', 'c.nl', 'd.nl', 'e.nl'], 'f.nl')).toEqual([
            'f.nl',
            'a.nl',
            'b.nl',
            'c.nl',
            'd.nl',
        ])
    })
})

describe('ways into an account', () => {
    test('the last way in cannot be removed', () => {
        expect(canRemove({has_password: false, identities: [identity('google', 1)]})).toBe(false)
        expect(canRemove({has_password: true, identities: [identity('google', 1)]})).toBe(true)
        expect(canRemove({has_password: false, identities: [identity('google', 1), identity('github', 2)]})).toBe(true)
    })

    test('offers only providers not linked yet', () => {
        const available = [
            {id: 'google', kind: 'oidc' as const, name: 'Google'},
            {id: 'github', kind: 'github' as const, name: 'GitHub'},
        ]
        expect(linkable({available, identities: [identity('google', 1)]}).map((p) => p.id)).toEqual(['github'])
    })

    test('confirms with the oldest identity, and only for a while', () => {
        expect(reauthWith([identity('github', 5), identity('google', 1)])?.provider).toBe('google')
        expect(reauthWith([])).toBeNull()
        expect(reauthValid(100, 99_000)).toBe(true)
        expect(reauthValid(100, 100_000)).toBe(false)
        expect(reauthValid(undefined)).toBe(false)
    })

    test('the create button says how many groups it joins', () => {
        expect(createLabel(0)).toBe('Create my account')
        expect(createLabel(2)).toBe('Create my account and join 2')
    })
})

describe('Admin › People › Sign-in', () => {
    test('working means tested', () => {
        expect(providerStatus(provider())).toBeNull()
        expect(providerStatus(provider({client_id: '123', secret_set: true}))?.label).toBe('test first')
        expect(providerStatus(provider({client_id: '123', secret_set: true, tested_at: 1}))?.label).toBe('working')
        expect(providerStatus(provider({kind: 'mastodon', needs_keys: false}))?.label).toBe('no keys needed')
    })

    test('the small line says what is missing', () => {
        expect(providerLine(provider())).toBe('Not set up')
        expect(providerLine(provider({client_id: '1'}))).toBe('Keys entered, not tested yet')
        expect(providerLine(provider({accounts: 214, client_id: '1', links: 3, tested_at: 1}))).toBe(
            '214 accounts · 3 communities linked',
        )
        expect(providerLine(provider({accounts: 1, client_id: '1', tested_at: 1}))).toBe('1 account')
    })

    test('the switch waits for a test, except for Mastodon', () => {
        expect(mayEnable(provider({client_id: '1'}))).toBe(false)
        expect(mayEnable(provider({client_id: '1', tested_at: 1}))).toBe(true)
        expect(mayEnable(provider({needs_keys: false}))).toBe(true)
    })

    test('moves a row one place, and not past an end', () => {
        expect(moved(['a', 'b', 'c'], 'b', -1)).toEqual(['b', 'a', 'c'])
        expect(moved(['a', 'b', 'c'], 'b', 1)).toEqual(['a', 'c', 'b'])
        expect(moved(['a', 'b', 'c'], 'a', -1)).toEqual(['a', 'b', 'c'])
        expect(moved(['a', 'b', 'c'], 'c', 1)).toEqual(['a', 'b', 'c'])
    })

    test('a custom server gets an oidc- id', () => {
        expect(customId('Garage44 Forgejo')).toBe('oidc-garage44-forgejo')
        expect(customId('  !!  ')).toBe('')
    })

    test('linked groups are a choice only where groups exist', () => {
        expect(newAccountChoices('discord').map((c) => c.value)).toEqual(['anyone', 'members', 'none'])
        expect(newAccountChoices('oidc').map((c) => c.value)).toEqual(['anyone', 'none'])
    })
})

import type {AccessToken} from '@/types'

import {notifier, signupApi} from '@/app'
import {ApiError} from '@/lib/api'

/**
 * The signed-in person's access tokens (`/api/tokens`): credentials a bot or a
 * script signs in with, so a password never has to leave the person who knows it
 * (`docs/notes/access-tokens.md`).
 *
 * Through `signupApi`, the person's own session — the chat's and the buildings
 * page's — and never the admin session: a token is the account's, whatever the
 * account may administer. The server refuses all three routes to a request made
 * *with* an access token, so a stolen one cannot mint or revoke its siblings.
 */
function report(e: unknown, fallback: string) {
    notifier.error(e instanceof ApiError ? e.message : fallback)
}

export async function listTokens(): Promise<AccessToken[] | null> {
    try {
        const result = await signupApi.get<{tokens: AccessToken[]}>('/api/tokens')
        return result.tokens
    } catch (e) {
        report(e, 'Could not load your access tokens')
        return null
    }
}

export interface NewToken {
    /** Unix seconds, or null for a token that lasts until it is revoked. */
    expires_at: number | null
    elevated: boolean
    label: string
    /**
     * Asked again: a session can be a remembered one on somebody else's device.
     * Absent for an account without a password, which confirms through its
     * provider instead (the server accepts that for ten minutes).
     */
    password?: string
}

/**
 * Make a token. The answer's `token` is the secret, and nothing shows it again.
 * `'reauth'` when the account has no password and the server wants a fresh
 * confirmation through a provider first: the page offers that, not a toast.
 */
export async function createToken(spec: NewToken): Promise<{info: AccessToken; token: string} | 'reauth' | null> {
    try {
        return await signupApi.post<{info: AccessToken; token: string}>('/api/tokens', spec)
    } catch (e) {
        if (e instanceof ApiError && e.reauth) return 'reauth'
        report(e, 'Could not create the token')
        return null
    }
}

export async function revokeToken(id: string): Promise<boolean> {
    try {
        await signupApi.delete(`/api/tokens/${encodeURIComponent(id)}`)
        notifier.success('Token revoked — anything signed in with it is disconnected')
        return true
    } catch (e) {
        report(e, 'Could not revoke the token')
        return false
    }
}

/** Unix seconds as the page shows them; `null` reads as `fallback`. */
export function formatUnix(seconds: number | null, fallback: string): string {
    if (seconds === null) {
        return fallback
    }
    return new Date(seconds * 1000).toLocaleString(undefined, {
        day: 'numeric',
        hour: '2-digit',
        minute: '2-digit',
        month: 'short',
        year: 'numeric',
    })
}

/** The expiry choices the form offers, in days; `never` is the default (revoking is the off switch). */
export const EXPIRY_CHOICES: {label: string; value: string}[] = [
    {label: 'Never — until you revoke it', value: 'never'},
    {label: 'In 30 days', value: '30'},
    {label: 'In 90 days', value: '90'},
    {label: 'In a year', value: '365'},
]

/** An `EXPIRY_CHOICES` value → unix seconds, or null for never. */
export function expiryAfter(choice: string, now = Date.now()): number | null {
    const days = Number(choice)
    return choice === 'never' || !Number.isFinite(days) ? null : Math.floor(now / 1000) + days * 86400
}

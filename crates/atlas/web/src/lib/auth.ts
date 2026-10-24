import type {
    AccountSignin,
    AdminProvider,
    AdminSignin,
    CommunityLink,
    NewAccounts,
    SigninTicket,
    TicketSession,
} from '@/types'

import {$s, api, notifier, signupApi, store} from '@/app'
import {ApiError} from '@/lib/api'
import {safeReturn} from '@/lib/signin'
import {loadMyBuildings} from '@/lib/signup'

/**
 * Signing in through another site, the calls (`/api/auth`, `/api/account`,
 * `/api/admin/signin`, a community's links). The pure half is `signin.ts`.
 *
 * The person's own calls go through `signupApi`, the operator's through `api`,
 * for the reason `signup.ts` gives. Refusals are the server's sentences, passed
 * through: only it knows that a name is taken or that this is the last way in.
 */
function report(e: unknown, fallback: string): string {
    const message = e instanceof ApiError ? e.message : fallback
    notifier.error(message)
    return message
}

/** The sentence a refusal carries, for a form that shows it inline rather than as a toast. */
export function sentence(e: unknown, fallback: string): string {
    return e instanceof ApiError ? e.message : fallback
}

// ── The round trip ───────────────────────────────────────────────────────────

/**
 * Ask a signed-in session's purpose of the server (link, reauth, test,
 * community), then go: the answer is the provider's address, with the state
 * the server will check when the browser comes back.
 */
export async function begin(
    id: string,
    purpose: 'community' | 'link' | 'reauth' | 'test',
    opts: {community?: string; return?: string; server?: string} = {},
): Promise<boolean> {
    try {
        const client = purpose === 'test' ? api : signupApi
        const {url} = await client.post<{url: string}>(`/api/auth/${encodeURIComponent(id)}/begin`, {
            purpose,
            ...opts,
            return: opts.return === undefined ? undefined : safeReturn(opts.return),
        })
        location.assign(url)
        return true
    } catch (e) {
        report(e, 'Could not start that')
        return false
    }
}

/** What the round trip came to. Throws the server's sentence; `/welcome` shows it. */
export async function readTicket(ticket: string): Promise<SigninTicket> {
    return signupApi.post<SigninTicket>('/api/auth/ticket', {ticket})
}

/**
 * Keep a session the ticket handed over: the same `$s.signup` the password
 * sign-in fills (`signup.playerSignIn`), so every page that reads it works the
 * same whichever way the session was made.
 */
export async function adoptTicketSession(session: {token: string; username: string}) {
    $s.signup = {token: session.token, username: session.username}
    store.save()
    await loadMyBuildings()
}

/**
 * Make the account. No address is typed here: the server keeps only one the
 * provider confirmed, since an unconfirmed one is swept with its account.
 */
export async function createFromTicket(ticket: string, username: string, join: string[]): Promise<TicketSession> {
    return signupApi.post<TicketSession>('/api/auth/ticket/create', {join, ticket, username})
}

export async function linkFromTicket(ticket: string, password: string): Promise<TicketSession> {
    return signupApi.post<TicketSession>('/api/auth/ticket/link', {password, ticket})
}

export async function joinFromTicket(ticket: string, join: string[]): Promise<string[] | null> {
    try {
        const {joined} = await signupApi.post<{joined: string[]}>('/api/auth/ticket/join', {join, ticket})
        return joined
    } catch (e) {
        report(e, 'Could not join those communities')
        return null
    }
}

export async function communityFromTicket(ticket: string, group_id: string): Promise<CommunityLink> {
    const {link} = await signupApi.post<{link: CommunityLink}>('/api/auth/ticket/community', {group_id, ticket})
    return link
}

// ── The account's own ways in ────────────────────────────────────────────────

export async function loadAccountSignin(): Promise<AccountSignin | null> {
    try {
        return await signupApi.get<AccountSignin>('/api/account/signin')
    } catch (e) {
        report(e, 'Could not load how you sign in')
        return null
    }
}

export async function removeIdentity(id: string, name: string): Promise<boolean> {
    try {
        await signupApi.delete(`/api/account/identities/${encodeURIComponent(id)}`)
        notifier.success(`${name} is no longer a way into this account`)
        return true
    } catch (e) {
        report(e, `Could not remove ${name}`)
        return false
    }
}

/**
 * Add or change the password. `'reauth'` when the server wants the person to
 * confirm with a provider first (an account that has none yet), which the
 * page turns into a button rather than a toast.
 */
export async function setPassword(password: string, current?: string): Promise<'ok' | 'reauth' | string> {
    try {
        await signupApi.post('/api/account/password', {current_password: current || undefined, password})
        notifier.success(current ? 'Password changed' : 'Password added')
        return 'ok'
    } catch (e) {
        if (e instanceof ApiError && e.reauth) return 'reauth'
        return sentence(e, 'Could not save that password')
    }
}

// ── Admin › People › Sign-in ─────────────────────────────────────────────────

export async function loadAdminSignin(): Promise<AdminSignin | null> {
    try {
        return await api.get<AdminSignin>('/api/admin/signin')
    } catch (e) {
        report(e, 'Could not load the sign-in providers')
        return null
    }
}

/** The fields a save may change. A secret is sent only when a new one was typed. */
export interface ProviderChange {
    client_id?: string
    client_secret?: string
    enabled?: boolean
    issuer?: string
    name?: string
    new_accounts?: NewAccounts
}

export async function saveProvider(id: string, change: ProviderChange, quiet = false): Promise<boolean> {
    try {
        await api.put(`/api/admin/signin/${encodeURIComponent(id)}`, change)
        if (!quiet) notifier.success('Saved')
        return true
    } catch (e) {
        report(e, 'Could not save that provider')
        return false
    }
}

export async function orderProviders(ids: string[]): Promise<boolean> {
    try {
        await api.post('/api/admin/signin/order', {ids})
        return true
    } catch (e) {
        report(e, 'Could not change the order')
        return false
    }
}

export async function deleteProvider(p: AdminProvider): Promise<boolean> {
    try {
        await api.delete(`/api/admin/signin/${encodeURIComponent(p.id)}`)
        notifier.success(p.custom ? `Removed ${p.name}` : `Cleared the keys for ${p.name}`)
        return true
    } catch (e) {
        report(e, 'Could not remove that provider')
        return false
    }
}

// ── A community's linked groups ──────────────────────────────────────────────

export async function loadCommunityLinks(community: string): Promise<CommunityLink[] | null> {
    try {
        const {links} = await signupApi.get<{links: CommunityLink[]}>(
            `/api/communities/${encodeURIComponent(community)}/links`,
        )
        return links
    } catch (e) {
        report(e, 'Could not load the linked groups')
        return null
    }
}

export async function unlinkGroup(community: string, link: CommunityLink): Promise<boolean> {
    try {
        await signupApi.delete(
            `/api/communities/${encodeURIComponent(community)}/links/${encodeURIComponent(link.provider)}/${encodeURIComponent(link.group_id)}`,
        )
        notifier.success(`Unlinked ${link.group_name}`)
        return true
    } catch (e) {
        report(e, 'Could not unlink that group')
        return false
    }
}

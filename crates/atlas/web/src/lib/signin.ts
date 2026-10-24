import type {AccountSignin, AdminProvider, LinkedIdentity, NewAccounts, Provider, ProviderKind} from '@/types'

/**
 * Signing in through another site, the parts that need no server and no page:
 * where a button sends the browser, what `/welcome` was handed, what a
 * fediverse address names, and the few decisions the pages draw from
 * (`docs/mockups/external-sign-in/`).
 *
 * Kept free of `@/app` so the tests can call it without a store. The server
 * decides every one of these again; this only decides what is drawn.
 */

/**
 * A `return` the page may navigate to: a path on this site, or `/`.
 *
 * Every one of these arrives in a URL somebody else could have written, so a
 * full address, a protocol-relative `//evil.example` or a backslash variant of
 * it would make `/welcome` an open redirect that ends a real sign-in on another
 * site.
 */
export function safeReturn(path: string | null | undefined): string {
    const p = (path ?? '').trim()
    if (!p.startsWith('/') || p.startsWith('//') || p.startsWith('/\\') || /[\u0000-\u001f]/.test(p)) {
        return '/'
    }
    return p
}

/**
 * Where a provider's button sends the browser: a navigation, not a fetch, since
 * the answer is a redirect to the provider's own page.
 */
export function startUrl(id: string, ret: string, opts: {app?: string; server?: string} = {}): string {
    const q = new URLSearchParams({return: safeReturn(ret)})
    if (opts.app) q.set('app', opts.app)
    if (opts.server) q.set('server', opts.server)
    return `/api/auth/${encodeURIComponent(id)}/start?${q}`
}

/** What `/welcome` was opened with. */
export type WelcomeQuery = {error: string} | {ticket: string} | null

/**
 * `?ticket=…` or `?error=…`. An error wins: the server sends one only when
 * there is nothing to read back.
 */
export function welcomeQuery(search: string): WelcomeQuery {
    const q = new URLSearchParams(search)
    const error = q.get('error')?.trim()
    if (error) return {error}
    const ticket = q.get('ticket')?.trim()
    if (ticket) return {ticket}
    return null
}

/**
 * The server in what somebody typed on `/signin/fediverse`: `@anna@mastodon.nl`,
 * `anna@mastodon.nl`, `mastodon.nl`, or a profile address pasted from the
 * browser. Lower case, no port, or `null` when it names no server.
 */
export function fediverseHost(input: string): string | null {
    let text = input.trim()
    if (!text) return null
    if (/^https?:\/\//i.test(text)) {
        try {
            text = new URL(text).hostname
        } catch {
            return null
        }
    } else {
        // The path first: `mastodon.nl/@anna` names its server before the `@`.
        text = text.split(/[/?#]/)[0].replace(/^@/, '')
        const at = text.lastIndexOf('@')
        if (at >= 0) text = text.slice(at + 1)
    }
    const host = text.toLowerCase().replace(/\.$/, '')
    // A host name with at least one dot: `localhost` and a bare word are
    // somebody's handle typed without its server, not a server.
    const label = '[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?'
    return new RegExp(`^${label}(?:\\.${label})+$`).test(host) ? host : null
}

/** The recently used servers with `host` first, without repeats, at most `max`. */
export function rememberServer(list: readonly string[], host: string, max = 5): string[] {
    return [host, ...list.filter((h) => h !== host)].slice(0, max)
}

/** Whether a provider has to ask which fediverse server first. */
export function asksServer(p: {kind: ProviderKind}): boolean {
    return p.kind === 'mastodon'
}

/** The words for a provider's button where it acts on an account that exists. */
export function confirmLabel(name: string): string {
    return `Confirm with ${name}`
}

/** The create button on `/welcome`, which says how many groups it joins. */
export function createLabel(joining: number): string {
    return joining ? `Create my account and join ${joining}` : 'Create my account'
}

/**
 * What a removal would leave: an identity may go only when a password or another
 * identity remains. The server refuses the last way in as well; this greys the
 * button and says why first.
 */
export function canRemove(signin: Pick<AccountSignin, 'has_password' | 'identities'>): boolean {
    return signin.has_password || signin.identities.length > 1
}

/** The providers that are on and not linked yet: the account page's Link buttons. */
export function linkable(signin: Pick<AccountSignin, 'available' | 'identities'>): Provider[] {
    const linked = new Set(signin.identities.map((i) => i.provider))
    return signin.available.filter((p) => !linked.has(p.id))
}

/** The identity to confirm with when there is no password: the oldest one. */
export function reauthWith(identities: readonly LinkedIdentity[]): LinkedIdentity | null {
    return [...identities].sort((a, b) => a.linked_at - b.linked_at)[0] ?? null
}

/** Whether a confirmation from a provider still counts, at `now` (ms). */
export function reauthValid(until: number | undefined, now = Date.now()): boolean {
    return until !== undefined && until * 1000 > now
}

// ── Admin › People › Sign-in ─────────────────────────────────────────────────

/** Whether keys have been entered for a provider, tested or not. */
export function hasKeys(p: AdminProvider): boolean {
    return !p.needs_keys || !!p.client_id || p.secret_set
}

/**
 * The status pill: "working" only after a test came back, because a provider
 * with a wrong redirect address looks configured until a member hits its error
 * page. `null` for one that is not set up, which draws a "Set up" button.
 */
export function providerStatus(p: AdminProvider): {label: string; tone: 'success' | 'warning'} | null {
    if (!p.needs_keys) return {label: 'no keys needed', tone: 'success'}
    if (p.tested_at) return {label: 'working', tone: 'success'}
    if (hasKeys(p)) return {label: 'test first', tone: 'warning'}
    return null
}

/** The small line under a provider's name. */
export function providerLine(p: AdminProvider): string {
    const accounts = `${p.accounts} ${p.accounts === 1 ? 'account' : 'accounts'}`
    if (!p.needs_keys) return `Any server, minus the federation blocklist · ${accounts}`
    if (!hasKeys(p)) return 'Not set up'
    if (!p.tested_at) return 'Keys entered, not tested yet'
    const links = p.links ? ` · ${p.links} ${p.links === 1 ? 'community' : 'communities'} linked` : ''
    return accounts + links
}

/** Whether the switch may be turned on: after a test, or always for Mastodon. */
export function mayEnable(p: AdminProvider): boolean {
    return !p.needs_keys || !!p.tested_at
}

/** The order after moving `id` one place up (-1) or down (+1); unchanged at an end. */
export function moved(ids: readonly string[], id: string, by: -1 | 1): string[] {
    const out = [...ids]
    const i = out.indexOf(id)
    const j = i + by
    if (i < 0 || j < 0 || j >= out.length) return out
    ;[out[i], out[j]] = [out[j], out[i]]
    return out
}

/** A custom OpenID server's id from its name: `oidc-` and a slug, as the server requires. */
export function customId(name: string): string {
    const slug = name
        .toLowerCase()
        .trim()
        .replace(/[^a-z0-9]+/g, '-')
        .replace(/^-+|-+$/g, '')
        .slice(0, 32)
    return slug ? `oidc-${slug}` : ''
}

/** "Members of a linked group" means something only where groups can be linked. */
export function newAccountChoices(kind: ProviderKind): {label: string; value: NewAccounts}[] {
    const group = kind === 'discord' ? 'members of a linked server' : kind === 'reddit' ? 'members of a linked subreddit' : ''
    return [
        {label: 'Anyone', value: 'anyone'},
        ...(group ? [{label: group[0].toUpperCase() + group.slice(1), value: 'members' as const}] : []),
        {label: 'Nobody new', value: 'none'},
    ]
}

/**
 * The three things to do at a provider's site, in its own words where they
 * differ. A custom OpenID server gets the generic steps.
 */
export function setupSteps(p: Pick<AdminProvider, 'id' | 'kind' | 'name'>): string[] {
    const where: Record<string, [string, string]> = {
        codeberg: ['Open Codeberg › Settings › Applications and make a new OAuth2 application called “Cartopolis”.', 'Use the address below as its redirect URI.'],
        discord: ['Open the Discord developer portal and make a new application called “Cartopolis”.', 'Under OAuth2 › Redirects, add the address below.'],
        github: ['Open GitHub › Settings › Developer settings › OAuth Apps and make a new one called “Cartopolis”.', 'Use the address below as its authorization callback URL.'],
        google: ['Open the Google Cloud console › APIs & Services › Credentials and make an OAuth client ID for a web application.', 'Add the address below as an authorised redirect URI.'],
        reddit: ['Open reddit.com/prefs/apps and make a new “web app” called “Cartopolis”.', 'Use the address below as its redirect uri.'],
    }
    const [make, redirect] = where[p.id] ?? where[p.kind] ?? [
        `Make a new client called “Cartopolis” on ${p.name || 'the OpenID server'}.`,
        'Add the address below as its redirect address.',
    ]
    return [make, redirect, 'Copy the client ID and client secret into the fields here.']
}

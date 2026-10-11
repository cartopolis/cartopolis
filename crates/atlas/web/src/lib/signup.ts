import type {BuildingAddress, OwnedBuilding} from '@/types'

import {$s, notifier, signupApi, store} from '@/app'
import {ApiError} from '@/lib/api'

/**
 * Everything a person who does *not* have an account yet can do, plus the one
 * thing they do immediately after getting one.
 *
 * Separate from `accounts.ts` for the same reason `signupApi` is separate from
 * `api`: nothing here is an operator action, none of it goes through the admin
 * session, and a 401 on this side must not sign an operator out of the tab they
 * are working in. The shared piece is `report`'s rule — the server writes the
 * sentence, this passes it through — because the refusals here are the ones only
 * the server can phrase: a taken name, an expired link, a building somebody else
 * got to first.
 */
function report(e: unknown, fallback: string): string {
    const message = e instanceof ApiError ? e.message : fallback
    notifier.error(message)
    return message
}

/** What the register form got back. `error` is already on screen as a toast. */
export interface RegisterResult {
    error?: string
    ok: boolean
}

export async function register(
    username: string,
    email: string,
    password: string,
): Promise<RegisterResult> {
    try {
        await signupApi.post('/api/register', {email, password, username})
        return {ok: true}
    } catch (e) {
        return {error: report(e, 'Could not create that account'), ok: false}
    }
}

/**
 * Spend a verification link.
 *
 * The server hands back a session with it — opening the link proves control of
 * the mailbox — so this stores one, and the screen after it is signed in rather
 * than asking for the password again.
 */
export async function verify(token: string): Promise<boolean> {
    try {
        const result = await signupApi.post<{token: string; username: string}>(
            '/api/register/verify',
            {token},
        )
        $s.signup = {token: result.token, username: result.username}
        store.save()
        await loadMyBuildings()
        return true
    } catch (e) {
        report(e, 'Could not confirm that link')
        return false
    }
}

/**
 * Ask for another link.
 *
 * Always reports success, because the server always answers success: telling the
 * caller "no such address" would make this the account oracle that `/api/register`
 * refuses to be. So the message is about what was done, not about what was found.
 */
export async function resend(email: string): Promise<boolean> {
    try {
        await signupApi.post('/api/register/resend', {email})
        notifier.success('If that address has an unconfirmed account, a new link is on its way.')
        return true
    } catch (e) {
        report(e, 'Could not send another link')
        return false
    }
}

/**
 * Sign back in with the password, for the person who registered some other day.
 *
 * The verification link mints a session exactly once, so before this the
 * claim-a-building screen was reachable only in the minute after signing up.
 * It fills the same `$s.signup` session `verify` does — `signupApi` reads the
 * token from there, so everything on the buildings screen works identically
 * whichever way the session was made. For an account that may run something
 * it is the admin session too (`accounts.adoptSession`).
 */
export async function playerSignIn(username: string, password: string): Promise<boolean> {
    try {
        const result = await signupApi.post<{token: string; username: string}>('/api/login', {
            password,
            username,
        })
        $s.signup = {token: result.token, username: result.username}
        store.save()
        await loadMyBuildings()
        return true
    } catch (e) {
        // A 401 from a *login* is a wrong password, not an expired session —
        // `signupApi`'s handler has already cleared the (empty) signup state,
        // so the only job left is saying the right sentence.
        if (e instanceof ApiError && e.isUnauthorized) {
            notifier.error('Wrong username or password')
        } else {
            report(e, 'Could not sign you in')
        }
        return false
    }
}

export async function playerSignOut() {
    // Revoke server-side first, while the token is still in the store — a token
    // dropped from localStorage alone stays valid until it expires.
    try {
        await signupApi.post('/api/logout')
    } catch {
        // Signing out locally must succeed even when the server cannot be
        // reached; the token expires on its own.
    }
    $s.signup = {token: '', username: ''}
    $s.myBuildings = []
    store.save()
}

export async function searchBuildings(query: string): Promise<BuildingAddress[]> {
    if (!query.trim()) {
        return []
    }
    try {
        const result = await signupApi.get<{results: BuildingAddress[]}>(
            `/api/buildings/search?q=${encodeURIComponent(query)}`,
        )
        return result.results
    } catch (e) {
        report(e, 'Could not search for that address')
        return []
    }
}

export async function claimBuilding(address: BuildingAddress): Promise<boolean> {
    try {
        const result = await signupApi.post<{building: OwnedBuilding}>('/api/buildings/claim', {
            nummeraanduiding_id: address.nummeraanduiding_id,
        })
        notifier.success(`${result.building.address} is yours`)
        await loadMyBuildings()
        return true
    } catch (e) {
        report(e, 'Could not claim that building')
        return false
    }
}

export async function loadMyBuildings() {
    if (!$s.signup.token) {
        return
    }
    try {
        const result = await signupApi.get<{buildings: OwnedBuilding[]; limit: number}>(
            '/api/buildings/mine',
        )
        $s.myBuildings = result.buildings
        $s.buildingLimit = result.limit
    } catch {
        // Not worth a toast: this runs on load, and the screen it feeds shows
        // "nothing yet", which is also what a failure looks like from here.
        $s.myBuildings = []
    }
}

export async function releaseBuilding(building: OwnedBuilding): Promise<boolean> {
    try {
        await signupApi.delete(`/api/buildings/${building.pand_id}`)
        notifier.success(`Gave up ${building.address}`)
        await loadMyBuildings()
        return true
    } catch (e) {
        report(e, 'Could not give that building up')
        return false
    }
}

/**
 * The token out of `?token=…`.
 *
 * Read from the query and not from the path, because the link is built by the
 * server (`http::verification_link`) and a path segment would put a bearer
 * credential in every referrer header the page emits.
 */
export function tokenFromLocation(search: string): string {
    return new URLSearchParams(search).get('token')?.trim() || ''
}

/**
 * Whether these three fields can be submitted, and what is wrong if not.
 *
 * The same rules the server applies, applied here first — not because the server
 * cannot be trusted with them, but because a form that only finds out on submit
 * spends a round trip to say "that password is too short". The server remains the
 * one that decides; this only decides when the button is grey.
 */
export function whatIsMissing(
    username: string,
    email: string,
    password: string,
    minPassword: number,
    maxUsername: number,
): null | string {
    const name = username.trim()
    if (!name) {
        return 'Pick a username.'
    }
    if (name.length > maxUsername) {
        return `That username is longer than ${maxUsername} characters.`
    }
    // Deliberately shallow, and the same shallowness the server uses: an address
    // is about to be *tested* by sending mail to it, so a stricter check here can
    // only reject real addresses (plus tags, new TLDs) that would have worked.
    const at = email.trim().lastIndexOf('@')
    if (at < 1 || !email.trim().slice(at + 1).includes('.')) {
        return 'That does not look like an email address.'
    }
    if (password.length < minPassword) {
        return `The password needs at least ${minPassword} characters.`
    }
    return null
}

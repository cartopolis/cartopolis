import type {Account, OwnedBuilding} from '@/types'

import {$s, api, notifier, signupApi, store} from '@/app'
import {ApiError} from '@/lib/api'
import {closeContext} from '@/lib/layout'
import {applyMe} from '@/lib/roles'

/**
 * Every mutation the GUI can make, in one place.
 *
 * They are here rather than in the components for one reason that keeps proving
 * itself: each one has to write the store *and* say something to the user *and*
 * decide what a refusal means, and a component that also lays out a form ends up
 * doing two of the three. `report` is the single place an `ApiError` becomes a
 * message — which is what keeps the server's own sentence ("that is the last
 * enabled admin — promote someone else first") reaching the screen instead of
 * being replaced by a generic one here.
 */
function report(e: unknown, fallback: string) {
    if (e instanceof ApiError) {
        // A 401 has already signed the app out; a second toast about it would be
        // noise on top of a screen that has just changed.
        if (!e.isUnauthorized) {
            notifier.error(e.message)
        }
        return
    }
    notifier.error(fallback)
}

/**
 * Use the chat's session for the admin pages too, when the account may run
 * anything: one sign-in for the whole page (`docs/mockups/web-navigation/`,
 * "Decided"). The simulator issues the player and the admin token from the same
 * session store and checks the account's permissions on every admin call, so
 * this only decides what the page *shows*; it grants nothing.
 */
export async function adoptSession(): Promise<void> {
    const token = $s.signup.token
    if (!token) {
        dropOperator()
        return
    }
    try {
        const me = await signupApi.get<{admin: boolean; permissions?: string[]; principal: string; username: string}>(
            '/api/admin/me',
        )
        // Signed out (or into another account) while the answer was on its way.
        if ($s.signup.token !== token) return
        $s.profile = {principal: me.principal, token, username: me.username}
        applyMe({admin: me.admin, permissions: me.permissions ?? []})
        $s.signedIn = true
        store.save()
    } catch {
        // A 403 is the ordinary answer for a player: there is nothing to manage.
        if ($s.signup.token === token) dropOperator()
    }
}

/** Forget the operator half of the session: what this page shows as Admin. */
export function dropOperator() {
    $s.profile = {principal: '', token: '', username: ''}
    $s.me = {admin: false, permissions: []}
    $s.accounts = []
    $s.signedIn = false
    // Whatever record the panel held belongs to the session that just ended.
    closeContext()
    store.save()
}

export async function loadAccounts() {
    $s.loading = true
    try {
        const result = await api.get<{accounts: Account[]; roles?: Record<string, string[]>}>(
            '/api/admin/accounts',
        )
        $s.accounts = result.accounts
        $s.accountRoles = Object.entries(result.roles ?? {}).map(([id, roles]) => ({id, roles}))
    } catch (e) {
        report(e, 'Could not load accounts')
    } finally {
        $s.loading = false
    }
}

export async function createAccount(username: string, password: string, admin: boolean): Promise<boolean> {
    try {
        await api.post('/api/admin/accounts', {admin, password, username})
        notifier.success(`Created ${username}`)
        await loadAccounts()
        return true
    } catch (e) {
        report(e, 'Could not create that account')
        return false
    }
}

/**
 * Patch one account. Every call reloads the list rather than patching the row in
 * place: the server may have refused *part* of what was asked (the last-admin
 * guard is checked per request, not per field), so the authoritative row is the
 * one it hands back on the next read.
 */
export async function updateAccount(
    id: string,
    patch: {admin?: boolean; disabled?: boolean; password?: string},
    describe: string,
): Promise<boolean> {
    try {
        await api.patch(`/api/admin/accounts/${id}`, patch)
        notifier.success(describe)
        await loadAccounts()
        return true
    } catch (e) {
        report(e, 'Could not save that change')
        return false
    }
}

export async function deleteAccount(account: Account): Promise<boolean> {
    try {
        await api.delete(`/api/admin/accounts/${account.id}`)
        notifier.success(`Deleted ${account.username}`)
        await loadAccounts()
        return true
    } catch (e) {
        report(e, 'Could not delete that account')
        return false
    }
}

/**
 * Confirm a registration whose mail never arrived. The server mails the person
 * that it is done; when it could not (no relay, or the relay refused), say so,
 * because then the operator is the only one who can tell them.
 */
export async function activateAccount(account: Account): Promise<boolean> {
    try {
        const result = await api.post<{mailed: boolean}>(`/api/admin/accounts/${account.id}/activate`)
        if (result.mailed) {
            notifier.success(`Activated ${account.username} and mailed ${account.email}`)
        } else {
            notifier.info(`Activated ${account.username}, but the confirmation mail could not be sent`)
        }
        await loadAccounts()
        return true
    } catch (e) {
        report(e, 'Could not activate that account')
        return false
    }
}

/** The visible rows: the search filter applied to the fetched list. */
export function visibleAccounts(): Account[] {
    const needle = $s.search.trim().toLowerCase()
    if (!needle) {
        return $s.accounts
    }
    return $s.accounts.filter((a) => a.username.toLowerCase().includes(needle))
}

/**
 * Every house somebody holds. Operator-only, and unpaginated for the same reason
 * the account list is: the store is small, and a cursor here would be a guess at
 * a problem nobody has.
 */
export async function loadClaims() {
    try {
        const result = await api.get<{buildings: OwnedBuilding[]}>('/api/admin/claims')
        $s.claims = result.buildings
    } catch (e) {
        report(e, 'Could not load the buildings')
    }
}

/** Take a building back from its owner. */
export async function releaseClaim(claim: OwnedBuilding): Promise<boolean> {
    try {
        await api.delete(`/api/admin/claims/${claim.pand_id}`)
        notifier.success(`Released ${claim.address}`)
        await loadClaims()
        return true
    } catch (e) {
        report(e, 'Could not release that building')
        return false
    }
}

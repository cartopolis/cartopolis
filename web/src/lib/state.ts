import type {Account, AdminState, Notification} from '@/types'

/**
 * What survives a reload — and, because `Store.save` filters against this shape,
 * the *only* thing that can.
 *
 * The session token is here on purpose: without it every refresh is a new login,
 * and the alternative (an HttpOnly cookie) needs the API to set one, which would
 * make the admin surface stateful in a way the game's own token is not. The token
 * is the same opaque, expiring, revocable string the game client persists in
 * `user.json`; `Sessions::revoke` invalidates it server-side on sign-out.
 */
export const persistentState: AdminState = {
    accounts: [],
    editing: null,
    loading: false,
    notifications: [],
    profile: {
        principal: '',
        token: '',
        username: '',
    },
    search: '',
    signedIn: null,
}

/**
 * Applied *after* the restored state, so everything here is deliberately reset on
 * every load. The account list is the point: a stale list rendered before the
 * first fetch shows accounts that may have been deleted from another tab, and it
 * would show them as authoritative.
 */
export const volatileState: Partial<AdminState> = {
    accounts: [] as Account[],
    editing: null,
    loading: false,
    notifications: [] as Notification[],
    search: '',
    signedIn: null,
}

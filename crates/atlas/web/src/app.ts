
import type {AdminState, Notification, RegistrationRules} from '@/types'

import {Api} from '@/lib/api'
import {adoptSession} from '@/lib/accounts'
import {Notifier} from '@/lib/notifier'
import {persistentState, volatileState} from '@/lib/state'
import {Store} from '@/lib/store'

/**
 * Everything shared, built once at module scope and imported by name.
 *
 * Codebrew's setup, and the reason for it: these are singletons in fact — there
 * is one store, one API client, one notifier — so threading them through props or
 * a context is ceremony around a fact that never changes. `$s` is the same store
 * seen as Mithril reactive state, which is what components read.
 */
const store = new Store<AdminState>()
store.load(persistentState, volatileState)

const $s = store.state

/**
 * The API reads the token out of the live store and reports a 401 back into it,
 * so an expired session signs the app out from wherever it was noticed rather
 * than at each call site. It is the chat's session (`accounts.adoptSession`), so
 * a 401 here ends both halves of the page, not only Admin.
 */
const api = new Api(
    () => $s.profile.token,
    () => {
        $s.profile = {principal: '', token: '', username: ''}
        $s.signedIn = false
        $s.accounts = []
        $s.signup = {token: '', username: ''}
        store.save()
    },
)

/**
 * The same client, reading the person's own session (`$s.signup`): the chat's,
 * the buildings pages', and since 2026-10-11 the admin pages' too, which copy it
 * into `$s.profile` when the account may run something. Two objects remain
 * because `api` reads `$s.profile`, which is empty for a player, and because a
 * 401 on a sign-up screen (an expired link) must not drop a session elsewhere.
 */
const signupApi = new Api(
    () => $s.signup.token,
    () => {
        $s.signup = {token: '', username: ''}
        $s.myBuildings = []
        store.save()
    },
)

const notifier = new Notifier()

export class App {
    async init(mount: (root: Element) => void) {
        notifier.init($s.notifications as Notification[])

        // One session for the whole page (`accounts.adoptSession`). A token
        // remembered from the old separate admin sign-in is the same kind of
        // session, so it is carried over rather than asking for a password again;
        // one that disagrees with the chat's is dropped, the chat's winning.
        if (!$s.signup.token && $s.profile.token) {
            $s.signup = {token: $s.profile.token, username: $s.profile.username}
        }
        // A remembered token is a *claim* to be allowed into Admin, not a fact:
        // it may have expired, or the account lost its permissions.
        // `/api/admin/me` answers, and `signedIn` stays null until it does —
        // which is what the "Checking your session" splash is for.
        if ($s.signup.token) {
            $s.signedIn = null
            await adoptSession()
        } else {
            $s.signedIn = false
        }

        // What the server allows, asked once and read by both sign-up screens. It
        // is unauthenticated and cheap, and the alternative is a form that guesses
        // the rules and finds out it was wrong on submit.
        try {
            $s.registration = await api.get<RegistrationRules>('/api/register')
        } catch {
            // A server too old to have the endpoint, or one that is down. The
            // form says so rather than offering a button that cannot work.
            $s.registration = null
        }

        mount(document.body)
    }
}

export {$s, api, notifier, signupApi, store}

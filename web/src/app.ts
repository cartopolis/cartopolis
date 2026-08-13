import type {DeepSignal} from 'deepsignal'
import type {ComponentChild} from 'preact'

import {h, render} from 'preact'

import type {AdminState, Notification} from '@/types'

import {Api} from '@/lib/api'
import {Notifier} from '@/lib/notifier'
import {persistentState, volatileState} from '@/lib/state'
import {Store} from '@/lib/store'

/**
 * Everything shared, built once at module scope and imported by name.
 *
 * Codebrew's setup, and the reason for it: these are singletons in fact — there
 * is one store, one API client, one notifier — so threading them through props or
 * a context is ceremony around a fact that never changes. `$s` is the same store
 * seen as a deepsignal proxy, which is what components read.
 */
const store = new Store<AdminState>()
store.load(persistentState, volatileState)

const $s = store.state as DeepSignal<AdminState>

/**
 * The API reads the token out of the live store and reports a 401 back into it,
 * so an expired session signs the app out from wherever it was noticed rather
 * than at each call site.
 */
const api = new Api(
    () => $s.profile.token,
    () => {
        $s.profile = {principal: '', token: '', username: ''}
        $s.signedIn = false
        $s.accounts = []
        store.save()
    },
)

const notifier = new Notifier()

export class App {
    async init(Main: () => ComponentChild) {
        notifier.init($s.notifications as Notification[])

        // A token from localStorage is a *claim* to be signed in, not a fact: it
        // may have expired, been revoked, or belong to an account whose admin
        // flag is gone. `/api/admin/me` is the server answering the question, and
        // `signedIn` stays null until it does — which is what the splash is for.
        if ($s.profile.token) {
            try {
                const me = await api.get<{principal: string; username: string}>('/api/admin/me')
                $s.profile = {principal: me.principal, token: $s.profile.token, username: me.username}
                $s.signedIn = true
            } catch {
                // The 401 path in `Api` has already cleared the profile.
                $s.signedIn = false
            }
        } else {
            $s.signedIn = false
        }

        render(h(Main, {}), document.body)
    }
}

export {$s, api, notifier, store}

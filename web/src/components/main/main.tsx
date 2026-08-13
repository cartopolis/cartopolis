import {useEffect} from 'preact/hooks'

import {$s} from '@/app'
import {Notifications} from '@/components/elements/notifications/notifications'
import {Accounts} from '@/components/pages/accounts/accounts'
import {Login} from '@/components/pages/login/login'
import {loadAccounts, signOut} from '@/lib/accounts'

export const Main = () => {
    // Fetch on the transition into signed-in, from wherever it happened: a fresh
    // sign-in, or a remembered token that `App.init` has just had confirmed.
    useEffect(() => {
        if ($s.signedIn && !$s.accounts.length) {
            void loadAccounts()
        }
    }, [$s.signedIn])

    return (
        <div class="app">
            <header class="app-header">
                <span class="wordmark">
                    carto<b>polis</b>
                </span>
                <span class="spacer" />
                {$s.signedIn && (
                    <>
                        <span class="who">{$s.profile.username}</span>
                        <button class="ghost" onClick={() => void signOut()} type="button">
                            Sign out
                        </button>
                    </>
                )}
            </header>

            <main class="view">
                {/*
                 * Three states, not two. `null` is "we have a stored token and
                 * have not yet asked the server whether it is still good" —
                 * rendering the sign-in form there would flash a login at every
                 * signed-in operator on every reload.
                 */}
                {$s.signedIn === null ? (
                    <div class="splash">Checking your session…</div>
                ) : $s.signedIn ? (
                    <Accounts />
                ) : (
                    <Login />
                )}
            </main>

            <Notifications />
        </div>
    )
}

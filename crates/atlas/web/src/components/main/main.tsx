import type {RoutableProps} from 'preact-router'

import {Router, route} from 'preact-router'
import {useEffect} from 'preact/hooks'

import {$s} from '@/app'
import {Notifications} from '@/components/elements/notifications/notifications'
import {Accounts} from '@/components/pages/accounts/accounts'
import {Autopilot} from '@/components/pages/autopilot/autopilot'
import {Claims} from '@/components/pages/claims/claims'
import {Home} from '@/components/pages/home/home'
import {House} from '@/components/pages/house/house'
import {Library} from '@/components/pages/library/library'
import {Login} from '@/components/pages/login/login'
import {Pipeline} from '@/components/pages/pipeline/pipeline'
import {Register} from '@/components/pages/register/register'
import {Verify} from '@/components/pages/verify/verify'
import {loadAccounts, signOut} from '@/lib/accounts'
import {ADMIN, PLAYER_PATHS} from '@/lib/base'

/**
 * The operator's half: sign in, then one of two lists.
 *
 * Three states and not two. `null` is "we have a stored token and have not yet
 * asked the server whether it is still good" — rendering the sign-in form there
 * would flash a login at every signed-in operator on every reload.
 */
const AdminView = (_props: RoutableProps) => {
    // Fetch on the transition into signed-in, from wherever it happened: a fresh
    // sign-in, or a remembered token that `App.init` has just had confirmed.
    useEffect(() => {
        if ($s.signedIn && !$s.accounts.length) {
            void loadAccounts()
        }
    }, [$s.signedIn])

    if ($s.signedIn === null) {
        return <div class="splash">Checking your session…</div>
    }
    if (!$s.signedIn) {
        return <Login />
    }
    return (
        <>
            <nav class="tabs">
                <button
                    class={$s.adminTab === 'accounts' ? 'tab is-current' : 'tab'}
                    onClick={() => {
                        $s.adminTab = 'accounts'
                    }}
                    type="button"
                >
                    Accounts
                </button>
                <button
                    class={$s.adminTab === 'claims' ? 'tab is-current' : 'tab'}
                    onClick={() => {
                        $s.adminTab = 'claims'
                    }}
                    type="button"
                >
                    Houses
                </button>
                <button
                    class={$s.adminTab === 'library' ? 'tab is-current' : 'tab'}
                    onClick={() => {
                        $s.adminTab = 'library'
                    }}
                    type="button"
                >
                    Models
                </button>
                <button
                    class={$s.adminTab === 'pipeline' ? 'tab is-current' : 'tab'}
                    onClick={() => {
                        $s.adminTab = 'pipeline'
                    }}
                    type="button"
                >
                    Data
                </button>
                <button
                    class={$s.adminTab === 'autopilot' ? 'tab is-current' : 'tab'}
                    onClick={() => {
                        $s.adminTab = 'autopilot'
                    }}
                    type="button"
                >
                    Autopilot
                </button>
            </nav>
            {$s.adminTab === 'claims' ? (
                <Claims />
            ) : $s.adminTab === 'library' ? (
                <Library />
            ) : $s.adminTab === 'pipeline' ? (
                <Pipeline />
            ) : $s.adminTab === 'autopilot' ? (
                <Autopilot />
            ) : (
                <Accounts />
            )}
        </>
    )
}

/**
 * The whole site, told apart by the path (`lib/base.ts`).
 *
 * `/` is the front page. `/register` and `/verify` are for people who have no
 * account, `/house` is for players who do, and `/admin` is the operator's. They
 * share this build because they share an API, and because the verification
 * link has to point *somewhere* the simulator's `/api` is same-origin with.
 * Caddy serves `index.html` for every path it does not route elsewhere, so each
 * is a route rather than a 404 — and an unknown one lands on the front page.
 *
 * The header's sign-out button is deliberately absent on the player routes: the
 * session it would end is the operator's, and the person following a link out of
 * their inbox — or signing in to claim their house — has never seen it.
 */
export const Main = () => {
    const playerRoute = PLAYER_PATHS.includes(window.location.pathname)

    return (
        <div class="app">
            <header class="app-header">
                <a class="wordmark" href="/" onClick={(e) => {
                    e.preventDefault()
                    route('/')
                }}>
                    carto<b>polis</b>
                </a>
                <span class="spacer" />
                {$s.signedIn && !playerRoute && (
                    <>
                        <span class="who">{$s.profile.username}</span>
                        <button class="ghost" onClick={() => void signOut()} type="button">
                            Sign out
                        </button>
                    </>
                )}
            </header>

            <main class="view">
                <Router>
                    <Register path="/register" />
                    <Verify path="/verify" />
                    <House path="/house" />
                    <AdminView path={ADMIN} />
                    <Home default />
                </Router>
            </main>

            <Notifications />
        </div>
    )
}

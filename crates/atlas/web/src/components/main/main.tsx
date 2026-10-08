import m, {MithrilComponent, type ComponentType, type RouteResolver} from '@garage44/mithril'

import {$s} from '@/app'
import {Notifications} from '@/components/elements/notifications/notifications'
import {Accounts} from '@/components/pages/accounts/accounts'
import {Autopilot} from '@/components/pages/autopilot/autopilot'
import {Claims} from '@/components/pages/claims/claims'
import {Home} from '@/components/pages/home/home'
import {Building} from '@/components/pages/building/building'
import {Buildings} from '@/components/pages/buildings/buildings'
import {Library} from '@/components/pages/library/library'
import {Login} from '@/components/pages/login/login'
import {Pipeline} from '@/components/pages/pipeline/pipeline'
import {Register} from '@/components/pages/register/register'
import {Overrides} from '@/components/pages/overrides/overrides'
import {Roles} from '@/components/pages/roles/roles'
import {Verify} from '@/components/pages/verify/verify'
import {loadAccounts, signOut} from '@/lib/accounts'
import {requestedPand} from '@/lib/overrides'
import {may, visibleTabs} from '@/lib/roles'
import {ADMIN, PLAYER_PATHS} from '@/lib/base'

/**
 * The operator's half: sign in, then one of the tabs.
 *
 * Three states and not two. `null` is "we have a stored token and have not yet
 * asked the server whether it is still good" — rendering the sign-in form there
 * would flash a login at every signed-in operator on every reload.
 */
class AdminView extends MithrilComponent {
    /** The `signedIn` the last `sync` acted on, so it acts once per transition. */
    private seen: boolean | null | undefined = undefined

    // Fetch on the transition into signed-in, from wherever it happened: a fresh
    // sign-in, or a remembered token that `App.init` has just had confirmed.
    private sync() {
        if (this.seen === $s.signedIn) {
            return
        }
        this.seen = $s.signedIn
        if ($s.signedIn && !$s.accounts.length && may('accounts_manage')) {
            void loadAccounts()
        }
        // Sent here from the viewer's building card to replace one building.
        if ($s.signedIn && requestedPand() && may('building_override')) {
            $s.adminTab = 'overrides'
        }
    }

    oninit() {
        this.sync()
    }

    onupdate() {
        this.sync()
    }

    view() {
        if ($s.signedIn === null) {
            return <div class="splash">Checking your session…</div>
        }
        if (!$s.signedIn) {
            return <Login />
        }
        // Only the tabs this operator's roles open (`lib/roles.ts`). The server
        // refuses the rest anyway; hiding them just keeps the screen honest.
        const tabs = visibleTabs()
        if (!tabs.length) {
            return <div class="splash">This account has nothing to manage here.</div>
        }
        return (
            <>
                <nav class="tabs">
                    {tabs.map((t) => (
                        <button
                            class={$s.adminTab === t.tab ? 'tab is-current' : 'tab'}
                            key={t.tab}
                            onclick={() => {
                                $s.adminTab = t.tab
                            }}
                            type="button"
                        >
                            {t.label}
                        </button>
                    ))}
                </nav>
                {$s.adminTab === 'claims' ? (
                    <Claims />
                ) : $s.adminTab === 'overrides' ? (
                    <Overrides />
                ) : $s.adminTab === 'roles' ? (
                    <Roles />
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
}

/**
 * The chrome around every page.
 *
 * The header's sign-out button is deliberately absent on the player routes: the
 * session it would end is the operator's, and the person following a link out of
 * their inbox — or signing in to claim their house — has never seen it.
 */
class Layout extends MithrilComponent {
    view(vnode: m.Vnode) {
        const path = m.route.get() ?? window.location.pathname
        const playerRoute = PLAYER_PATHS.some((p) => path === p || path.startsWith(`${p}/`) ||
            path.startsWith(`${p}?`))

        return (
            <div class="app">
                <header class="app-header">
                    <a
                        class="wordmark"
                        href="/"
                        onclick={(e: Event) => {
                            e.preventDefault()
                            m.route.set('/')
                        }}
                    >
                        carto<b>polis</b>
                    </a>
                    <span class="spacer" />
                    {$s.signedIn && !playerRoute && (
                        <>
                            <span class="who">{$s.profile.username}</span>
                            <button class="ghost" onclick={() => void signOut()} type="button">
                                Sign out
                            </button>
                        </>
                    )}
                </header>

                <main class="view">{vnode.children}</main>

                <Notifications />
            </div>
        )
    }
}

/** One route: the page inside the layout, with the route's parameters as its attrs. */
const page = (Page: ComponentType<any>): RouteResolver => ({
    render: (vnode) => m(Layout, m(Page, vnode.attrs)),
})

/**
 * The whole site, told apart by the path (`lib/base.ts`).
 *
 * `/` is the front page. `/register` and `/verify` are for people who have no
 * account, `/buildings` and `/building/…` are for players who do, and `/admin`
 * is the operator's. They share this build because they share an API, and
 * because the verification link has to point *somewhere* the simulator's `/api`
 * is same-origin with. Caddy serves `index.html` for every path it does not route
 * elsewhere, so each is a route rather than a 404 — and an unknown one lands on
 * the front page, through the catch-all rather than a redirect, so the address
 * bar keeps what was typed.
 *
 * Pathname routing (`m.route.prefix = ''`), not Mithril's default `#!`: the
 * verification mail and the viewer's building card link to real paths.
 */
export function mountRoutes(root: Element) {
    m.route.prefix = ''
    m.route(root, '/', {
        '/': page(Home),
        '/register': page(Register),
        '/verify': page(Verify),
        '/buildings': page(Buildings),
        // The old path, from before a person owned buildings rather than one
        // house; links to it are in sent mail.
        '/house': page(Buildings),
        '/building/:pand': page(Building),
        [ADMIN]: page(AdminView),
        '/:rest...': page(Home),
    })
}

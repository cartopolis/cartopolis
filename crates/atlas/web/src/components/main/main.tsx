import m, {MithrilComponent, type ComponentType, type RouteResolver} from '@garage44/mithril'
import {Button, NavGroup, NavItem, Shell, ShellMenuButton, ThemeSwitch} from '@garage44/mithril/components'

import {$s} from '@/app'
import {Notifications} from '@/components/elements/notifications/notifications'
import {Accounts} from '@/components/pages/accounts/accounts'
import {Autopilot} from '@/components/pages/autopilot/autopilot'
import {Claims} from '@/components/pages/claims/claims'
import {Building} from '@/components/pages/building/building'
import {Chat} from '@/components/pages/chat/chat'
import {Buildings} from '@/components/pages/buildings/buildings'
import {Library} from '@/components/pages/library/library'
import {Login} from '@/components/pages/login/login'
import {Pipeline} from '@/components/pages/pipeline/pipeline'
import {Register} from '@/components/pages/register/register'
import {Overrides} from '@/components/pages/overrides/overrides'
import {Roles} from '@/components/pages/roles/roles'
import {Verify} from '@/components/pages/verify/verify'
import {loadAccounts, signOut} from '@/lib/accounts'
import {closeContext, contextContent, layout} from '@/lib/layout'
import {requestedPand} from '@/lib/overrides'
import {may, visibleTabs} from '@/lib/roles'
import {ADMIN} from '@/lib/base'

/** The wordmark, as the client draws it: "carto" in cream, "polis" in the brand orange. */
class Wordmark extends MithrilComponent {
    view() {
        return (
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
        )
    }
}

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
            return <PageFrame><div class="splash">Checking your session…</div></PageFrame>
        }
        if (!$s.signedIn) {
            return <PageFrame><Login /></PageFrame>
        }
        // Only the tabs this operator's roles open (`lib/roles.ts`). The server
        // refuses the rest anyway; hiding them just keeps the screen honest.
        const tabs = visibleTabs()
        if (!tabs.length) {
            return <PageFrame><div class="splash">This account has nothing to manage here.</div></PageFrame>
        }
        const current = tabs.find((t) => t.tab === $s.adminTab) ?? tabs[0]
        const context = contextContent()
        return (
            <Shell
                class="admin"
                navLabel="Admin"
                navOpen={layout.navOpen}
                navWidth={layout.navWidth}
                contextOpen={layout.contextOpen}
                contextWidth={layout.contextWidth}
                navHeader={<Wordmark />}
                nav={
                    <NavGroup title="Manage">
                        {tabs.map((t) => (
                            <NavItem
                                key={t.tab}
                                active={current.tab === t.tab}
                                onclick={() => {
                                    if ($s.adminTab !== t.tab) {
                                        // A form about the last tab's record means nothing on this one.
                                        closeContext()
                                    }
                                    $s.adminTab = t.tab
                                }}
                            >
                                {t.label}
                            </NavItem>
                        ))}
                    </NavGroup>
                }
                navFooter={
                    <div class="nav-account">
                        <span class="who">{$s.profile.username}</span>
                        <ThemeSwitch model={layout.theme} />
                        <Button size="sm" variant="ghost" onclick={() => void signOut()}>
                            Sign out
                        </Button>
                    </div>
                }
                header={[<ShellMenuButton model={layout.navOpen} />, <h1>{current.label}</h1>]}
                context={context ? context.body() : null}
                contextTitle={context?.title()}
                contextFooter={context?.footer?.()}
            >
                <div class="admin-page">
                    {current.tab === 'claims' ? (
                        <Claims />
                    ) : current.tab === 'overrides' ? (
                        <Overrides />
                    ) : current.tab === 'roles' ? (
                        <Roles />
                    ) : current.tab === 'library' ? (
                        <Library />
                    ) : current.tab === 'pipeline' ? (
                        <Pipeline />
                    ) : current.tab === 'autopilot' ? (
                        <Autopilot />
                    ) : (
                        <Accounts />
                    )}
                </div>
            </Shell>
        )
    }
}

/**
 * The frame of every page that is not the operator's: the front page, sign-up,
 * and a player's buildings. A header and a centred column; the operator's pages
 * get the `Shell` instead, because they have a nav and a context to show.
 *
 * The header's sign-out button is deliberately absent on the player routes: the
 * session it would end is the operator's, and the person following a link out of
 * their inbox — or signing in to claim their house — has never seen it.
 */
class PageFrame extends MithrilComponent<{wide?: boolean}> {
    view(vnode: m.Vnode<{wide?: boolean}>) {
        return (
            <div class="page-frame">
                <header class="page-header">
                    <Wordmark />
                    <span class="spacer" />
                    <ThemeSwitch model={layout.theme} />
                </header>
                <main class={vnode.attrs.wide ? 'page-body is-wide' : 'page-body'}>{vnode.children}</main>
            </div>
        )
    }
}

/**
 * The chat, full-screen: a work chat brings its own chrome (the rail, the
 * sidebar), so it is not put inside the site's header and column. The
 * notifier's toasts still show over it (a wrong password, for one).
 */
const chatPage: RouteResolver = {render: () => m.fragment(null, m(Chat), m(Notifications))}

/** One route: the page inside its frame, with the route's parameters as its attrs. */
const page = (Page: ComponentType<any>, frame: 'page' | 'wide' | 'admin' = 'page'): RouteResolver => ({
    render: (vnode) =>
        m.fragment(
            null,
            frame === 'admin' ? m(Page, vnode.attrs) : m(PageFrame, {wide: frame === 'wide'}, m(Page, vnode.attrs)),
            m(Notifications),
        ),
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
        // The site's front door is the chat (2026-10-09): signed in, you land in
        // it; signed out, its welcome card offers every other way in.
        '/': chatPage,
        '/register': page(Register),
        '/verify': page(Verify),
        '/buildings': page(Buildings),
        // The old path, from before a person owned buildings rather than one
        // house; links to it are in sent mail.
        '/house': page(Buildings),
        '/building/:pand': page(Building, 'wide'),
        '/chat': chatPage,
        [ADMIN]: page(AdminView, 'admin'),
        '/:rest...': chatPage,
    })
}

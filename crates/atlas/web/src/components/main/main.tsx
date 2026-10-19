import m, {MithrilComponent, type ComponentType, type RouteResolver} from '@garage44/mithril'
import {ThemeSwitch} from '@garage44/mithril/components'

import {Notifications} from '@/components/elements/notifications/notifications'
import {Building} from '@/components/pages/building/building'
import {Chat} from '@/components/pages/chat/chat'
import {Buildings} from '@/components/pages/buildings/buildings'
import {Register} from '@/components/pages/register/register'
import {Verify} from '@/components/pages/verify/verify'
import {Tokens} from '@/components/pages/tokens/tokens'
import {layout} from '@/lib/layout'
import {ADMIN} from '@/lib/base'
import {pageView} from '@/lib/stats'

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
 * The frame of the pages outside the switch: sign-up and a player's buildings.
 * A header and a centred column; Community, World and Admin are the chat page's
 * own frame (`pages/chat`).
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
 * Community | World | Admin, full-screen: the page brings its own chrome (the
 * top bar, the rail, the sidebar), so it is not put inside the site's header
 * and column. One resolver for all three paths, so moving between them redraws
 * the same component rather than making a new one — which is what keeps the
 * World frame loaded. The notifier's toasts still show over it.
 */
const chatPage: RouteResolver = {
    // Every change of address is one page view (`lib/stats.ts`), counted
    // without a cookie.
    onmatch: (_args, path) => pageView(path),
    render: () => m.fragment(null, m(Chat), m(Notifications)),
}

/** One route: the page inside its frame, with the route's parameters as its attrs. */
const page = (Page: ComponentType<any>, frame: 'page' | 'wide' = 'page'): RouteResolver => ({
    onmatch: (_args, path) => pageView(path),
    render: (vnode) => m.fragment(null, m(PageFrame, {wide: frame === 'wide'}, m(Page, vnode.attrs)), m(Notifications)),
})

/**
 * The whole site, told apart by the path (`lib/base.ts`).
 *
 * `/` is Community, `/world` World and `/admin` Admin: the three positions of the
 * page's switch. `/register` and `/verify` are for people who have no account,
 * `/buildings`, `/building/…` and `/tokens` are for players who do. They share this build because they share an API, and
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
        '/tokens': page(Tokens),
        '/chat': chatPage,
        '/world': chatPage,
        // Community's calendar: every plan from every chat (`docs/mockups/calendar/`).
        '/calendar': chatPage,
        [ADMIN]: chatPage,
        '/:rest...': chatPage,
    })
}

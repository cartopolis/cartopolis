import m, {MithrilComponent} from '@garage44/mithril'
import {NavGroup, NavItem, Shell, ShellMenuButton} from '@garage44/mithril/components'

import type {AdminTab} from '@/types'

import {$s} from '@/app'
import {Accounts} from '@/components/pages/accounts/accounts'
import {Claims} from '@/components/pages/claims/claims'
import {Library} from '@/components/pages/library/library'
import {Overrides} from '@/components/pages/overrides/overrides'
import {Pipeline} from '@/components/pages/pipeline/pipeline'
import {Reports} from '@/components/pages/reports/reports'
import {Roles} from '@/components/pages/roles/roles'
import {loadAccounts} from '@/lib/accounts'
import {closeContext, contextContent, layout} from '@/lib/layout'
import {requestedPand} from '@/lib/overrides'
import {AREAS, type AdminAreaId, areaOf, may, visibleAreas, visibleTabs} from '@/lib/roles'

/** The rail's glyphs: Material Symbols paths, as the chat's call screen draws its own. */
const GLYPHS: Record<AdminAreaId, string> = {
    content: 'M10 20v-6h4v6h5v-8h3L12 3 2 12h3v8z',
    data: 'M12 3C7.58 3 4 4.79 4 7v10c0 2.21 3.59 4 8 4s8-1.79 8-4V7c0-2.21-3.58-4-8-4zm6 14c0 .5-2.13 2-6 2s-6-1.5-6-2v-2.23c1.61.78 3.72 1.23 6 1.23s4.39-.45 6-1.23V17zm0-4.55c-1.3.95-3.58 1.55-6 1.55s-4.7-.6-6-1.55V9.64c1.47.83 3.61 1.36 6 1.36s4.53-.53 6-1.36v2.81zM12 9C8.13 9 6 7.5 6 7s2.13-2 6-2 6 1.5 6 2-2.13 2-6 2z',
    people: 'M16 11c1.66 0 2.99-1.34 2.99-3S17.66 5 16 5c-1.66 0-3 1.34-3 3s1.34 3 3 3zm-8 0c1.66 0 2.99-1.34 2.99-3S9.66 5 8 5C6.34 5 5 6.34 5 8s1.34 3 3 3zm0 2c-2.33 0-7 1.17-7 3.5V19h14v-2.5c0-2.33-4.67-3.5-7-3.5zm8 0c-.29 0-.62.02-.97.05 1.16.84 1.97 1.97 1.97 3.45V19h6v-2.5c0-2.33-4.67-3.5-7-3.5z',
}

function Glyph(path: string) {
    return (
        <svg aria-hidden="true" class="home-glyph" viewBox="0 0 24 24">
            <path d={path} fill="currentColor" />
        </svg>
    )
}

/** The tab being shown: the remembered one if this operator may still open it. */
export function currentTab() {
    const tabs = visibleTabs()
    return tabs.find((t) => t.tab === $s.adminTab) ?? tabs[0] ?? null
}

/** Admin › area › page, for the top bar's breadcrumb. */
export function adminCrumbs(): string[] {
    const tab = currentTab()
    if (!tab) return []
    return [AREAS.find((a) => a.id === areaOf(tab.tab))?.label ?? '', tab.label]
}

function go(tab: AdminTab) {
    if ($s.adminTab !== tab) {
        // A form about the last page's record means nothing on this one.
        closeContext()
    }
    $s.adminTab = tab
}

function page(tab: AdminTab) {
    switch (tab) {
        case 'claims':
            return <Claims />
        case 'library':
            return <Library />
        case 'overrides':
            return <Overrides />
        case 'pipeline':
            return <Pipeline />
        case 'reports':
            return <Reports />
        case 'roles':
            return <Roles />
        default:
            return <Accounts />
    }
}

/**
 * Admin, as the third segment of the page's switch (`docs/mockups/web-navigation/`):
 * a rail of three areas — People, Content, Data — and, beside it, the library's
 * `Shell` holding that area's pages and the context panel a record opens in.
 *
 * On a phone the rail goes and the drawer lists every area's pages under their
 * headings instead, so one menu reaches all of them.
 *
 * Signed-in-ness is the chat's (`accounts.adoptSession`): `signedIn` is null while
 * `/api/admin/me` is being asked, then whether this account may run anything.
 */
export class AdminArea extends MithrilComponent {
    /** The `signedIn` the last `sync` acted on, so it acts once per transition. */
    private seen: boolean | null | undefined = undefined

    private sync() {
        if (this.seen === $s.signedIn) return
        this.seen = $s.signedIn
        if ($s.signedIn && !$s.accounts.length && may('accounts_manage')) {
            void loadAccounts()
        }
        // Sent here from the app's building card to replace one building.
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
            return <div class="home-splash">Checking what this account may manage…</div>
        }
        const current = $s.signedIn ? currentTab() : null
        if (!current) {
            return <div class="home-splash">This account has nothing to manage here.</div>
        }
        const areas = visibleAreas()
        const area = areaOf(current.tab)
        const context = contextContent()
        return (
            <div class="home-admin">
                <nav aria-label="Admin areas" class="home-rail is-admin">
                    {areas.map((a) => (
                        <button
                            class={a.id === area ? 'home-tile is-current' : 'home-tile'}
                            key={a.id}
                            onclick={() => go(a.tabs[0].tab)}
                            title={a.hint}
                            type="button"
                        >
                            <span class="home-tile-face">{Glyph(GLYPHS[a.id])}</span>
                            <span class="home-tile-name">{a.label}</span>
                        </button>
                    ))}
                </nav>
                <Shell
                    class="admin"
                    contextOpen={layout.contextOpen}
                    contextTitle={context?.title()}
                    contextFooter={context?.footer?.()}
                    contextWidth={layout.contextWidth}
                    context={context ? context.body() : null}
                    navLabel="Admin"
                    navOpen={layout.navOpen}
                    navWidth={layout.navWidth}
                    nav={areas.map((a) => (
                        <div class={a.id === area ? 'admin-group is-current' : 'admin-group'} key={a.id}>
                            <NavGroup title={a.label}>
                                {a.tabs.map((t) => (
                                    <NavItem active={current.tab === t.tab} key={t.tab} onclick={() => go(t.tab)}>
                                        {t.label}
                                    </NavItem>
                                ))}
                            </NavGroup>
                        </div>
                    ))}
                    header={[<ShellMenuButton model={layout.navOpen} />, <h1>{current.label}</h1>]}
                >
                    <div class="admin-page">{page(current.tab)}</div>
                </Shell>
            </div>
        )
    }
}

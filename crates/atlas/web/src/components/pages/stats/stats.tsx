import m, {MithrilComponent} from '@garage44/mithril'
import {Button} from '@garage44/mithril/components'

import type {StatsDayApps, StatsOverview, StatsPlaces, StatsWeb, StatsWorld} from '@/types'

import {local} from '@/lib/reactive'
import {
    change,
    CLIENT_LABELS,
    count,
    dayLabel,
    downloadCsv,
    EVENT_LABELS,
    loadOverview,
    loadPlaces,
    loadWeb,
    loadWorld,
    placeName,
} from '@/lib/stats'

import {Card, Cohorts, compared, Funnel, LineDays, Period, Ranked, type Series, Spark, StackedDays, step, Tiles, WeekHours} from './charts'

/**
 * Admin › Stats (`docs/mockups/stats/`, v1): how the world is used and who
 * visits the sites. Four pages, each one filter row and the numbers for it;
 * the server leaves out anything about fewer than 5 people, so a gap here is
 * that rule, not a bug.
 */

const APPS: Series<StatsDayApps>[] = [
    {key: 'android', label: 'Android app', slot: 's1'},
    {key: 'web', label: 'App in a browser', slot: 's2'},
    {key: 'desktop', label: 'Desktop app', slot: 's3'},
]

/** Different accounts per day: the three apps, with the web chat and unnamed apps folded into "other". */
function appsWithOther(rows: StatsDayApps[]) {
    return rows.map((r) => ({...r, other: Math.max(0, r.all - r.android - r.web - r.desktop)}))
}

/** A busy square's place link: World opens above it. */
function placeLink(lat: number, lon: number) {
    return `/go?place=${lat.toFixed(5)},${lon.toFixed(5)},400m,-60.0,0.0`
}

/** A page that loads its numbers for a period, and again when the period changes. */
abstract class StatsPage<T> extends MithrilComponent {
    s = local({days: 30, data: null as null | T, loading: true})

    abstract fetch(days: number): Promise<null | T>

    oninit() {
        void this.load()
    }

    setDays = (days: number) => {
        this.s.days = days
        void this.load()
    }

    async load() {
        this.s.loading = true
        const got = await this.fetch(this.s.days)
        if (got) this.s.data = got as never
        this.s.loading = false
        m.redraw()
    }

    loadingView() {
        return <p class="stats-empty">{this.s.loading ? 'Loading…' : 'No numbers to show.'}</p>
    }
}

export class StatsOverviewPage extends StatsPage<StatsOverview> {
    fetch = loadOverview

    view() {
        const o = this.s.data
        if (!o) return <div class="stats">{this.loadingView()}</div>
        const f = o.funnel
        return (
            <div class="stats">
                {Period(this.s.days, this.setDays)}
                {Tiles([
                    {label: 'In the world now', note: `most today: ${o.peak_today}`, value: count(o.online_now)},
                    {label: 'Active today', note: 'so far', value: count(o.active_today)},
                    o.active_period === null
                        ? {label: `Active in ${o.days} days`, note: 'sessions are kept 90 days', value: '—'}
                        : {
                            change: change(o.active_period, o.active_prev),
                            label: `Active in ${o.days} days`,
                            note: `of ${count(o.accounts_total)} accounts`,
                            value: count(o.active_period),
                        },
                    compared('New accounts', o.new_accounts, o.new_accounts_prev),
                    compared('Web visitors', o.visitors, o.visitors_prev),
                ])}
                {Card(
                    'People in the world each day',
                    'different accounts signed in, by the app they used',
                    StackedDays(appsWithOther(o.daily), [...APPS, {key: 'other', label: 'Web chat and older apps', slot: 'q3'}]),
                )}
                <div class="stats-row is-three">
                    {Card('Visitors to the sites', 'per day', LineDays(o.visitors_daily, 170, 300))}
                    {Card(
                        'From a visit to a regular',
                        `${o.days} days`,
                        Funnel([
                            ['Visited a site', f.visitors],
                            ['Opened World', f.opened_world],
                            ['Made an account', f.accounts],
                            ['Signed in to the world', f.signed_in],
                            ['Back within a week', f.came_back],
                        ]),
                    )}
                    {Card(
                        'Busiest places',
                        'hours in the world',
                        Ranked(
                            o.places.map((p) => ({
                                label: <a href={placeLink(p.lat, p.lon)} rel="noopener" target="_blank">{placeName(p.lat, p.lon)}</a>,
                                n: Math.round(p.minutes / 60),
                            })),
                            undefined,
                            'No square has had 5 people in it yet',
                        ),
                    )}
                </div>
            </div>
        )
    }
}

export class StatsWorldPage extends StatsPage<StatsWorld> {
    fetch = loadWorld

    csv(w: StatsWorld) {
        downloadCsv(
            `world-${w.days}-days.csv`,
            ['what', 'this period', 'period before'],
            w.events.map((e) => [EVENT_LABELS[e.kind] ?? e.kind, e.n, e.prev]),
        )
    }

    view() {
        const w = this.s.data
        if (!w) return <div class="stats">{this.loadingView()}</div>
        const peakAt = w.peak_hour === null ? '' : new Date(w.peak_hour * 3_600_000).toLocaleString(undefined, {day: 'numeric', hour: '2-digit', minute: '2-digit', month: 'short', weekday: 'short'})
        return (
            <div class="stats">
                {Period(this.s.days, this.setDays, <Button onclick={() => this.csv(w)} size="sm" variant="ghost">Download CSV</Button>)}
                {Tiles([
                    compared('Sessions', w.sessions, w.sessions_prev),
                    {label: 'Typical session', note: `median · longest ${w.longest_minutes} min`, value: `${w.median_minutes} min`},
                    {label: 'Most at once', note: peakAt, value: count(w.peak)},
                    compared(
                        'Messages relayed',
                        w.events.filter((e) => e.kind.endsWith('_message')).reduce((s, e) => s + e.n, 0),
                        w.events.filter((e) => e.kind.endsWith('_message')).reduce((s, e) => s + e.prev, 0),
                        'content never seen',
                    ),
                    compared('Call minutes', w.events.find((e) => e.kind === 'call_minutes')?.n ?? 0, w.events.find((e) => e.kind === 'call_minutes')?.prev ?? 0),
                ])}
                <div class="stats-row">
                    {Card('When people are in the world', 'average signed in, your time zone', WeekHours(w.hours))}
                    {Card("Who comes back", "share of each week's new accounts seen again", Cohorts(w.cohorts))}
                </div>
                <div class="stats-row">
                    {Card(
                        'What people did',
                        '',
                        <table class="stats-table">
                            <thead>
                                <tr><th>What</th><th>This period</th><th>Before</th><th>Change</th><th /></tr>
                            </thead>
                            <tbody>
                                {w.events.length === 0 && <tr><td class="stats-faint" colspan={5}>Nothing counted yet</td></tr>}
                                {w.events.map((e) => (
                                    <tr key={e.kind}>
                                        <td>{EVENT_LABELS[e.kind] ?? e.kind}</td>
                                        <td>{count(e.n)}</td>
                                        <td class="stats-faint">{count(e.prev)}</td>
                                        <td>{change(e.n, e.prev) ?? ''}</td>
                                        <td>{Spark(e.daily)}</td>
                                    </tr>
                                ))}
                            </tbody>
                        </table>,
                    )}
                    {Card(
                        'Apps in use',
                        'people, last 7 days',
                        <table class="stats-table">
                            <thead>
                                <tr><th>App</th><th>Build</th><th>People</th></tr>
                            </thead>
                            <tbody>
                                {w.apps.map((a) => (
                                    <tr key={`${a.client}-${a.build}`}>
                                        <td>{CLIENT_LABELS[a.client] ?? a.client}</td>
                                        <td class="stats-mono">{a.build || '—'}</td>
                                        <td>{count(a.people)}</td>
                                    </tr>
                                ))}
                                {w.refused.map((r) => (
                                    <tr class="is-refused" key={`${r.reason}-${r.protocol}-${r.client}-${r.build}`}>
                                        <td>Turned away · {CLIENT_LABELS[r.client] ?? r.client}</td>
                                        <td class="stats-mono">{r.reason === 'protocol' ? `protocol ${r.protocol}` : 'sign-in refused'}</td>
                                        <td>{count(r.devices)} <span class="stats-faint">({count(r.attempts)} tries)</span></td>
                                    </tr>
                                ))}
                            </tbody>
                        </table>,
                        <p class="stats-note">An app turned away for its protocol is older or newer than this server: the app itself only says "connection reset".</p>,
                    )}
                </div>
            </div>
        )
    }
}

const SITES: [null | string, string][] = [
    [null, 'All sites'],
    ['web', 'cartopolis.org'],
    ['viewer', 'World'],
    ['about', 'about.cartopolis.org'],
]

export class StatsWebPage extends StatsPage<StatsWeb> {
    site: null | string = null

    fetch = (days: number) => loadWeb(days, this.site)

    setSite(site: null | string) {
        this.site = site
        void this.load()
    }

    view() {
        const w = this.s.data
        if (!w) return <div class="stats">{this.loadingView()}</div>
        const sites = (
            <div class="stats-seg" role="group">
                {SITES.map(([id, label]) => (
                    <button class={id === this.site ? 'is-on' : ''} key={label} onclick={() => this.setSite(id)} type="button">
                        {label}
                    </button>
                ))}
            </div>
        )
        const rank = (rows: {value: string; visitors: number}[]) => rows.map((r) => ({label: r.value, n: r.visitors}))
        return (
            <div class="stats">
                {Period(this.s.days, this.setDays, sites)}
                {Tiles([
                    compared('Visitors', w.visitors, w.visitors_prev),
                    {label: 'Pages shown', note: w.visitors ? `${(w.views / w.visitors).toFixed(1)} per visitor` : '', value: count(w.views)},
                    {label: 'Opened World', note: w.visitors ? `${Math.round((w.opened_world / Math.max(1, w.visitors)) * 100)}% of visitors` : '', value: count(w.opened_world)},
                    {label: 'Crawlers filtered', note: 'never counted as visitors', value: count(w.bots)},
                ])}
                <div class="stats-row is-wide">
                    {Card('Visitors per day', 'one browser on one site on one day', LineDays(w.daily.map((d) => ({day: d.day, n: d.visitors})), 190))}
                    {Card(
                        'Devices',
                        'by window width',
                        Ranked(rank(w.devices)),
                        <h3 class="stats-sub">Languages</h3>,
                        Ranked(rank(w.languages)),
                    )}
                </div>
                <div class="stats-row is-three">
                    {Card('Pages', '', Ranked(rank(w.pages), ['Page', 'Visitors']))}
                    {Card('Where they came from', '', Ranked(rank(w.sources), ['Site', 'Visitors']))}
                    {Card(
                        'How this is counted',
                        '',
                        <p class="stats-note">
                            Each page sends one request when it is shown: the site, the page with its ids removed, the site the visitor came from and the window width. A visitor is a hash of a secret that is thrown away at midnight, the address and the browser — no cookie, and no address is kept.
                        </p>,
                    )}
                </div>
            </div>
        )
    }
}

export class StatsPlacesPage extends StatsPage<StatsPlaces> {
    fetch = loadPlaces

    view() {
        const p = this.s.data
        if (!p) return <div class="stats">{this.loadingView()}</div>
        const sq = p.squares
        const max = Math.max(1, ...sq.map((s) => s.minutes))
        // Every square, unless they spread too far to read: then the 15 km
        // around the busiest one.
        const MAX = 60
        const busiest = sq[0] ?? {sx: 0, sy: 0}
        const near = sq.filter((s) => Math.abs(s.sx - busiest.sx) <= MAX / 2 && Math.abs(s.sy - busiest.sy) <= MAX / 2)
        const xs = near.map((s) => s.sx)
        const ys = near.map((s) => s.sy)
        const [minX, maxX, minY, maxY] = [Math.min(...xs) - 1, Math.max(...xs) + 1, Math.min(...ys) - 1, Math.max(...ys) + 1]
        const cell = 22
        return (
            <div class="stats">
                {Period(this.s.days, this.setDays)}
                <div class="stats-row is-wide">
                    {Card(
                        'Hours spent in each 250 m square',
                        `${sq.length} squares`,
                        sq.length === 0 ? (
                            <p class="stats-empty">No square has had 5 different people in it yet.</p>
                        ) : (
                            <svg class="stats-chart stats-squares" viewBox={`0 0 ${(maxX - minX + 1) * cell} ${(maxY - minY + 1) * cell}`}>
                                {sq
                                    .filter((s) => s.sx >= minX && s.sx <= maxX && s.sy >= minY && s.sy <= maxY)
                                    .map((s) => (
                                        <a href={placeLink(s.lat, s.lon)} key={`${s.sx}:${s.sy}`} target="_blank">
                                            <rect class={step(s.minutes, max)} height={cell - 2} rx="3" width={cell - 2} x={(s.sx - minX) * cell} y={(maxY - s.sy) * cell}>
                                                <title>{`${s.lat.toFixed(4)}, ${s.lon.toFixed(4)} · ${Math.round(s.minutes / 60)} h · ${s.people} person-days`}</title>
                                            </rect>
                                        </a>
                                    ))}
                            </svg>
                        ),
                        <p class="stats-note">North is up. A square appears only once 5 different people have been in it on one day; positions are never stored, only each square's minutes. Click a square to open World above it.</p>,
                    )}
                    {Card(
                        'Busiest squares',
                        'hours',
                        Ranked(
                            sq.slice(0, 12).map((s) => ({
                                label: <a href={placeLink(s.lat, s.lon)} rel="noopener" target="_blank">{placeName(s.lat, s.lon)}</a>,
                                n: Math.round(s.minutes / 60),
                            })),
                        ),
                    )}
                </div>
            </div>
        )
    }
}

import m, {MithrilComponent} from '@garage44/mithril'

import type {CommunityStats as Stats} from '@/types'

import {Card, compared, LineDays, Period, Ranked, Tiles} from '@/components/pages/stats/charts'
import {local} from '@/lib/reactive'
import {count, dayLabel, loadCommunityStats, placeName} from '@/lib/stats'

interface Attrs {
    id: string
    name: string
}

/**
 * A community's own numbers, for its owner and moderators
 * (`docs/mockups/stats/v2-community.html`): members, joining and leaving,
 * topics, public posts on the fediverse, and visitors who are not members.
 *
 * Counts, never names: there is no list of who is most active, and a topic
 * fewer than 5 people wrote in says so instead of showing numbers. The server
 * enforces both; this only draws what it sends.
 */
export class CommunityStats extends MithrilComponent<Attrs> {
    s = local({data: null as null | Stats, days: 30, id: '', loading: true})

    oninit(vnode: m.Vnode<Attrs>) {
        void this.load(vnode.attrs.id)
    }

    onupdate(vnode: m.VnodeDOM<Attrs>) {
        if (vnode.attrs.id !== this.s.id) void this.load(vnode.attrs.id)
    }

    async load(id: string) {
        this.s.id = id
        this.s.loading = true
        const got = await loadCommunityStats(id, this.s.days)
        if (this.s.id === id) {
            this.s.data = got
            this.s.loading = false
            m.redraw()
        }
    }

    weeks(rows: Stats['weeks']) {
        const w = 440
        const [x0, mid, sc] = [26, 100, 7]
        const max = Math.max(4, ...rows.map((r) => r.joined))
        const scale = Math.min(sc, 80 / max)
        const bw = (w - x0) / Math.max(1, rows.length)
        return (
            <div>
                <div class="stats-legend">
                    <span><i class="stats-s1" />Joined</span>
                    <span><i class="stats-s2" />Left</span>
                </div>
                <svg class="stats-chart" viewBox={`0 0 ${w} 150`}>
                    <line class="stats-base" x1={x0} x2={w} y1={mid} y2={mid} />
                    {rows.map((r, i) => {
                        const x = x0 + i * bw + 5
                        const bwi = bw - 10
                        return (
                            <g key={r.week}>
                                {r.joined > 0 && (
                                    <rect class="stats-s1" height={r.joined * scale} rx="3" width={bwi} x={x} y={mid - r.joined * scale}>
                                        <title>{`Week of ${dayLabel(r.week)}: ${r.joined} joined`}</title>
                                    </rect>
                                )}
                                {r.left > 0 && (
                                    <rect class="stats-s2" height={r.left * scale} rx="3" width={bwi} x={x} y={mid + 1}>
                                        <title>{`Week of ${dayLabel(r.week)}: ${r.left} left`}</title>
                                    </rect>
                                )}
                                {i % 4 === 0 && <text text-anchor="middle" x={x + bwi / 2} y={145}>{dayLabel(r.week)}</text>}
                            </g>
                        )
                    })}
                </svg>
            </div>
        )
    }

    view(vnode: m.Vnode<Attrs>) {
        const c = this.s.data
        return (
            <div class="chat-stats">
                <header class="chat-stats-head">
                    <h2>Stats</h2>
                    <span>{vnode.attrs.name} · seen only by the owner and moderators</span>
                </header>
                {!c ? (
                    <p class="stats-empty">{this.s.loading ? 'Loading…' : 'No numbers to show.'}</p>
                ) : (
                    <div class="stats">
                        {Period(this.s.days, (d) => {
                            this.s.days = d
                            void this.load(vnode.attrs.id)
                        })}
                        {Tiles([
                            {label: 'Members', note: `${c.joined} joined, ${c.left} left`, value: count(c.members)},
                            {label: 'Active members', note: 'wrote or joined a call', value: c.active_members === null ? 'fewer than 5' : count(c.active_members)},
                            compared('Messages', c.messages, c.messages_prev, 'all sealed'),
                            {label: 'Public posts', note: `${count(c.followers)} followers elsewhere`, value: count(c.posts)},
                            compared('Visitors', c.visitors, c.visitors_prev, 'not members'),
                        ])}
                        <div class="stats-row">
                            {Card('Joining and leaving', 'people, per week', this.weeks(c.weeks))}
                            {Card(
                                'Topics',
                                `${c.days} days`,
                                <table class="stats-table">
                                    <thead>
                                        <tr><th>Topic</th><th>Messages</th><th>People writing</th></tr>
                                    </thead>
                                    <tbody>
                                        {c.topics.map((t) => (
                                            <tr key={t.topic}>
                                                <td># {t.name}</td>
                                                {t.writers === null ? (
                                                    <td class="stats-faint" colspan={2}>fewer than 5 people wrote</td>
                                                ) : (
                                                    [<td key="m">{count(t.messages)}</td>, <td key="w">{count(t.writers)}</td>]
                                                )}
                                            </tr>
                                        ))}
                                    </tbody>
                                </table>,
                            )}
                        </div>
                        <div class="stats-row">
                            {Card(
                                'Public posts on the fediverse',
                                `${c.followers_new} new followers`,
                                Ranked(
                                    c.followers_by_server.slice(0, 6).map((f) => ({label: f.server, n: f.n})),
                                    ['Server following the community', 'Followers'],
                                    'Nobody follows the community from another server yet',
                                ),
                                <p class="stats-note">
                                    Posts brought {count(c.replies_in)} replies and {count(c.boosts_in)} posts from followed sources in from outside Cartopolis.
                                </p>,
                            )}
                            {Card('Visitors who are not members', 'per day', LineDays(c.visitors_daily, 150))}
                        </div>
                        {Card(
                            'Where the community posts from',
                            'places on public posts',
                            Ranked(
                                c.post_places.map((p) => ({label: placeName(p.lat, p.lon), n: p.people})),
                                ['Place', 'Posts'],
                                'No public post with a place yet',
                            ),
                        )}
                    </div>
                )}
            </div>
        )
    }
}

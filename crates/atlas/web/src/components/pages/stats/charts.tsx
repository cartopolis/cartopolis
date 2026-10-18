import m from '@garage44/mithril'

import {change, count, dayLabel} from '@/lib/stats'

/**
 * The charts Admin › Stats and a community's numbers are drawn with: plain SVG,
 * one axis each, thin marks, a `<title>` on every mark for its exact value, and
 * text in the page's own ink rather than a series colour (`docs/mockups/stats/`).
 */

export interface Tile {
    change?: null | string
    label: string
    note?: string
    value: string
}

export function Tiles(tiles: Tile[]) {
    return (
        <div class="stats-tiles">
            {tiles.map((t) => (
                <div class="stats-tile" key={t.label}>
                    <div class="stats-tile-label">{t.label}</div>
                    <div class="stats-tile-value">{t.value}</div>
                    <div class="stats-tile-note">
                        {t.change && <span class={t.change.startsWith('-') ? 'stats-down' : 'stats-up'}>{t.change}</span>}
                        {t.note}
                    </div>
                </div>
            ))}
        </div>
    )
}

/** A tile's value and its change against the period before. */
export function compared(label: string, now: number, prev: number | null, note?: string): Tile {
    return {change: change(now, prev), label, note: note ?? (prev === null ? '' : `${count(prev)} before`), value: count(now)}
}

export function Card(title: string, sub: string, ...body: m.Children[]) {
    return (
        <section class="stats-card">
            <h3>
                {title}
                {sub && <span>{sub}</span>}
            </h3>
            {body}
        </section>
    )
}

function niceMax(v: number): number {
    if (v <= 4) return 4
    const p = 10 ** Math.floor(Math.log10(v))
    const n = Math.ceil(v / p)
    return (n <= 2 ? 2 : n <= 5 ? 5 : 10) * p
}

function Axis(x0: number, x1: number, y0: number, y1: number, max: number) {
    return [0, 0.5, 1].map((f) => {
        const y = y1 - f * (y1 - y0)
        return (
            <g key={f}>
                <line class={f ? 'stats-grid' : 'stats-base'} x1={x0} x2={x1} y1={y} y2={y} />
                <text text-anchor="end" x={x0 - 6} y={y + 4}>{count(Math.round(max * f))}</text>
            </g>
        )
    })
}

export interface Series<T> {
    key: keyof T
    label: string
    /** `s1`, `s2` or `s3`: the chart palette's first three slots, in order. */
    slot: string
}

/** Stacked bars per day, with a legend; the last day is today and drawn lighter. */
export function StackedDays<T extends {day: number}>(rows: T[], series: Series<T>[], height = 170) {
    const w = 900
    const [x0, x1, y0, y1] = [34, w, 8, height - 22]
    const total = (r: T) => series.reduce((s, x) => s + Number(r[x.key] ?? 0), 0)
    const max = niceMax(Math.max(1, ...rows.map(total)))
    const bw = (x1 - x0) / Math.max(1, rows.length)
    const every = Math.max(1, Math.round(rows.length / 6))
    return (
        <div>
            <div class="stats-legend">
                {series.map((s) => (
                    <span key={String(s.key)}><i class={`stats-${s.slot}`} />{s.label}</span>
                ))}
            </div>
            <svg class="stats-chart" viewBox={`0 0 ${w} ${height}`}>
                {Axis(x0, x1, y0, y1, max)}
                {rows.map((r, i) => {
                    let y = y1
                    const x = x0 + i * bw + Math.min(3, bw * 0.15)
                    const bwi = Math.max(1, bw - 2 * Math.min(3, bw * 0.15))
                    // Mithril wants a keyed list without holes, so empty
                    // segments are left out rather than drawn as nothing.
                    const parts = series.flatMap((s) => {
                        const v = Number(r[s.key] ?? 0)
                        const h = (v / max) * (y1 - y0)
                        y -= h
                        return v > 0
                            ? [
                                <rect class={`stats-${s.slot}`} height={Math.max(h - 1.5, 1)} key={String(s.key)} rx="2" width={bwi} x={x} y={y}>
                                    <title>{`${dayLabel(r.day)} · ${s.label}: ${count(v)}`}</title>
                                </rect>,
                            ]
                            : []
                    })
                    return (
                        <g class={i === rows.length - 1 ? 'stats-today' : ''} key={r.day}>
                            {parts}
                            {(i % every === 0 || i === rows.length - 1) && (
                                <text text-anchor="middle" x={x + bwi / 2} y={y1 + 16}>{i === rows.length - 1 ? 'today' : dayLabel(r.day)}</text>
                            )}
                        </g>
                    )
                })}
            </svg>
        </div>
    )
}

/** One series per day as a line with a soft area under it; today dashed. */
export function LineDays(rows: {day: number; n: number}[], height = 160, w = 600) {
    const [x0, x1, y0, y1] = [40, w - 4, 10, height - 22]
    const max = niceMax(Math.max(1, ...rows.map((r) => r.n)))
    const dx = (x1 - x0) / Math.max(1, rows.length - 1)
    const pts = rows.map((r, i) => [x0 + i * dx, y1 - (r.n / max) * (y1 - y0)] as const)
    const path = (p: readonly (readonly [number, number])[]) => p.map(([x, y], i) => `${i ? 'L' : 'M'}${x.toFixed(1)},${y.toFixed(1)}`).join('')
    const peak = rows.reduce((b, r, i) => (r.n > rows[b].n ? i : b), 0)
    const every = Math.max(1, Math.round(rows.length / 5))
    return (
        <svg class="stats-chart" viewBox={`0 0 ${w} ${height}`}>
            {Axis(x0, x1, y0, y1, max)}
            {pts.length > 1 && <path class="stats-area" d={`${path(pts)}L${x1},${y1}L${x0},${y1}Z`} />}
            {pts.length > 1 && <path class="stats-line" d={path(pts.slice(0, -1))} />}
            {pts.length > 1 && <path class="stats-line is-today" d={path(pts.slice(-2))} />}
            {pts.map(([x, y], i) => (
                <circle class="stats-hit" cx={x} cy={y} key={rows[i].day} r="7">
                    <title>{`${dayLabel(rows[i].day)}: ${count(rows[i].n)}`}</title>
                </circle>
            ))}
            {rows.length > 0 && rows[peak].n > 0 && (
                <g>
                    <circle class="stats-dot" cx={pts[peak][0]} cy={pts[peak][1]} r="4" />
                    <text class="stats-value" text-anchor={peak > rows.length * 0.7 ? 'end' : 'start'} x={pts[peak][0] + (peak > rows.length * 0.7 ? -8 : 8)} y={pts[peak][1] + 4}>
                        {count(rows[peak].n)}
                    </text>
                </g>
            )}
            {rows.flatMap((r, i) =>
                i % every === 0 || i === rows.length - 1
                    ? [
                        <text key={r.day} text-anchor={i === rows.length - 1 ? 'end' : 'middle'} x={pts[i][0]} y={y1 + 16}>
                            {i === rows.length - 1 ? 'today' : dayLabel(r.day)}
                        </text>,
                    ]
                    : [],
            )}
        </svg>
    )
}

/** Steps from the first, each with its share of the one before. */
export function Funnel(steps: [string, number][]) {
    const first = Math.max(1, steps[0]?.[1] ?? 1)
    return (
        <div class="stats-funnel">
            {steps.map(([label, n], i) => (
                <div class="stats-funnel-row" key={label}>
                    <span class="stats-funnel-label">{label}</span>
                    <span class="stats-funnel-bar">
                        <i style={{width: `${Math.max(1, (n / first) * 100)}%`}} />
                    </span>
                    <b>{count(n)}</b>
                    <span class="stats-funnel-pct">{i && steps[i - 1][1] ? `${((n / steps[i - 1][1]) * 100).toFixed(n / steps[i - 1][1] < 0.1 ? 1 : 0)}%` : ''}</span>
                </div>
            ))}
        </div>
    )
}

/** A ranked list with a bar behind each row, as wide as its share of the first. */
export function Ranked(rows: {label: m.Children; n: number}[], head?: [string, string], empty = 'Nothing yet') {
    const top = Math.max(1, ...rows.map((r) => r.n))
    return (
        <div class="stats-ranked">
            {head && (
                <div class="stats-ranked-head">
                    <span>{head[0]}</span>
                    <span>{head[1]}</span>
                </div>
            )}
            {rows.length === 0 && <p class="stats-empty">{empty}</p>}
            {rows.map((r, i) => (
                <div class="stats-ranked-row" key={i}>
                    <i style={{width: `${(r.n / top) * 100}%`}} />
                    <span>{r.label}</span>
                    <span>{count(r.n)}</span>
                </div>
            ))}
        </div>
    )
}

const STEPS = 6

/** One of six steps of the blue ramp, for magnitude. */
export function step(v: number, max: number): string {
    return `stats-q${Math.min(STEPS, Math.max(1, Math.ceil((v / Math.max(max, 1e-9)) * STEPS)))}`
}

/** Average signed in by weekday and hour, in the reader's own zone. */
export function WeekHours(hours: {avg: number; hour: number}[]) {
    const sum = Array.from({length: 7}, () => Array(24).fill(0) as number[])
    const n = Array.from({length: 7}, () => Array(24).fill(0) as number[])
    for (const h of hours) {
        const d = new Date(h.hour * 3_600_000)
        const wd = (d.getDay() + 6) % 7
        sum[wd][d.getHours()] += h.avg
        n[wd][d.getHours()] += 1
    }
    const avg = sum.map((row, d) => row.map((s, h) => (n[d][h] ? s / n[d][h] : 0)))
    const max = Math.max(0.0001, ...avg.flat())
    const names = ['Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat', 'Sun']
    const [cw, ch, x0] = [16, 18, 34]
    return (
        <svg class="stats-chart" viewBox={`0 0 ${x0 + 24 * cw} ${7 * ch + 22}`}>
            {avg.map((row, d) => (
                <g key={d}>
                    <text text-anchor="end" x={x0 - 6} y={d * ch + 13}>{names[d]}</text>
                    {row.map((v, h) => (
                        <rect class={v > 0.05 ? step(v, max) : 'stats-q0'} height={ch - 2} key={h} rx="2" width={cw - 2} x={x0 + h * cw} y={d * ch}>
                            <title>{`${names[d]} ${String(h).padStart(2, '0')}:00 · ${v.toFixed(1)} on average`}</title>
                        </rect>
                    ))}
                </g>
            ))}
            {[0, 6, 12, 18, 23].map((h) => (
                <text key={h} text-anchor="middle" x={x0 + h * cw + cw / 2 - 1} y={7 * ch + 14}>{String(h).padStart(2, '0')}</text>
            ))}
        </svg>
    )
}

/** Each week's new accounts and the share seen again in the weeks after. */
export function Cohorts(rows: {back: number[]; size: number; week: number}[]) {
    const cols = Math.max(1, ...rows.map((r) => r.back.length))
    return (
        <table class="stats-cohorts">
            <thead>
                <tr>
                    <th>Joined the week of</th>
                    <th>New</th>
                    {Array.from({length: cols}, (_, i) => <th key={i}>week {i + 1}</th>)}
                </tr>
            </thead>
            <tbody>
                {rows.map((r) => (
                    <tr key={r.week}>
                        <td>{dayLabel(r.week)}</td>
                        <td>{r.size}</td>
                        {r.back.length === 0 ? (
                            <td class="stats-faint" colspan={cols}>fewer than 5 — not shown</td>
                        ) : (
                            Array.from({length: cols}, (_, i) =>
                                i < r.back.length ? (
                                    <td class={step(r.back[i], 1)} key={i}>{Math.round(r.back[i] * 100)}%</td>
                                ) : (
                                    <td key={i} />
                                ),
                            )
                        )}
                    </tr>
                ))}
            </tbody>
        </table>
    )
}

/** A sparkline: shape only, the numbers are beside it. */
export function Spark(values: number[]) {
    if (values.length < 2) return null
    const max = Math.max(...values)
    const min = Math.min(...values)
    const pts = values.map((v, i) => `${((i / (values.length - 1)) * 84 + 1).toFixed(1)},${(19 - ((v - min) / (max - min || 1)) * 16).toFixed(1)}`)
    return (
        <svg class="stats-spark" viewBox="0 0 86 20">
            <path class="stats-line" d={`M${pts.join('L')}`} />
        </svg>
    )
}

/** The one filter row every stats page starts with. */
export function Period(days: number, set: (d: number) => void, extra?: m.Children) {
    const options: [number, string][] = [
        [1, 'Today'],
        [7, '7 days'],
        [30, '30 days'],
        [90, '90 days'],
        [365, '12 months'],
    ]
    return (
        <div class="stats-filters">
            <div class="stats-seg" role="group">
                {options.map(([d, label]) => (
                    <button class={d === days ? 'is-on' : ''} key={d} onclick={() => set(d)} type="button">
                        {label}
                    </button>
                ))}
            </div>
            {extra}
            <span class="stats-faint">Compared with the period before · days in UTC</span>
        </div>
    )
}

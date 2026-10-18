import m from '@garage44/mithril'

import type {CommunityStats, StatsOverview, StatsPlaces, StatsWeb, StatsWorld} from '@/types'

import {api, notifier, signupApi} from '@/app'
import {ApiError} from '@/lib/api'

/**
 * Usage statistics (`docs/notes/usage-stats.md`): the page-view beacon every
 * page sends, and the loaders for Admin › Stats and a community's own numbers.
 * Kept out of `$s`: the numbers are the server's on every visit.
 */
function report(e: unknown, fallback: string) {
    if (e instanceof ApiError) {
        if (!e.isUnauthorized) notifier.error(e.message)
        return
    }
    notifier.error(fallback)
}

let lastPath = ''

/**
 * Tell the server a page was shown: the site, the page's path, where the
 * visitor came from and the window's width — nothing that identifies them,
 * and no cookie. Sent once per change of address; a failure is ignored,
 * because a page must never wait on its own counting.
 */
export function pageView(path: string, site: 'web' | 'viewer' = 'web') {
    if (path === lastPath) return
    lastPath = path
    void fetch('/api/stats/hit', {
        body: JSON.stringify({path, referrer: document.referrer, site, width: window.innerWidth}),
        headers: {'Content-Type': 'application/json'},
        keepalive: true,
        method: 'POST',
    }).catch(() => {})
}

async function get<T>(path: string, fallback: string): Promise<T | null> {
    try {
        return await api.get<T>(path)
    } catch (e) {
        report(e, fallback)
        return null
    }
}

export const loadOverview = (days: number) =>
    get<StatsOverview>(`/api/admin/stats/overview?days=${days}`, 'Could not load the overview')

export const loadWorld = (days: number) =>
    get<StatsWorld>(`/api/admin/stats/world?days=${days}`, 'Could not load the world numbers')

export const loadWeb = (days: number, site: string | null) =>
    get<StatsWeb>(`/api/admin/stats/web?days=${days}${site ? `&site=${site}` : ''}`, 'Could not load the web numbers')

export const loadPlaces = (days: number) =>
    get<StatsPlaces>(`/api/admin/stats/places?days=${days}`, 'Could not load the places')

/** A community's numbers, on the person's own session (its moderators only). */
export async function loadCommunityStats(id: string, days: number): Promise<CommunityStats | null> {
    try {
        return await signupApi.get<CommunityStats>(`/api/communities/${id}/stats?days=${days}`)
    } catch (e) {
        report(e, "Could not load the community's numbers")
        return null
    }
}

/**
 * Our own address server (`nominatim.cartopolis.org`, on the same VPS), which
 * names a square for the busiest-places lists: "Grote Markt" rather than two
 * coordinates. Only operators' and moderators' pages ask it, and nothing about
 * a visitor goes with the question.
 */
const NOMINATIM = 'https://nominatim.cartopolis.org'
const names = new Map<string, string | null>()

/** A place's name once known; its coordinates until then (asking once). */
export function placeName(lat: number, lon: number): string {
    const key = `${lat.toFixed(4)},${lon.toFixed(4)}`
    const known = names.get(key)
    if (known) return known
    if (known === undefined) {
        names.set(key, null)
        void fetch(`${NOMINATIM}/reverse?format=jsonv2&zoom=17&lat=${lat}&lon=${lon}`)
            .then((r) => (r.ok ? r.json() : null))
            .then((j: null | {address?: Record<string, string>; name?: string}) => {
                const a = j?.address ?? {}
                const name = j?.name || a.road || a.square || a.pedestrian
                const area = a.suburb || a.quarter || a.neighbourhood || a.city_district
                if (name || area) {
                    names.set(key, [name, area].filter(Boolean).join(' · '))
                    m.redraw()
                }
            })
            .catch(() => {})
    }
    return `${lat.toFixed(4)}, ${lon.toFixed(4)}`
}

/** A day number as a short date in the reader's zone. */
export function dayLabel(day: number): string {
    return new Date(day * 86_400_000).toLocaleDateString(undefined, {day: 'numeric', month: 'short', timeZone: 'UTC'})
}

/** The change from `prev` to `now`, as "+12%", or null when there is nothing to compare. */
export function change(now: number, prev: number | null | undefined): null | string {
    if (prev === null || prev === undefined || prev === 0) return null
    const pct = Math.round((now / prev - 1) * 100)
    return `${pct >= 0 ? '+' : ''}${pct}%`
}

/** Plain thousands: 4,948. */
export function count(n: number): string {
    return n.toLocaleString('en')
}

/** CSV of rows, for the Download buttons. */
export function downloadCsv(name: string, header: string[], rows: (number | string)[][]) {
    const quote = (v: number | string) => {
        const s = String(v)
        return /[",\n]/.test(s) ? `"${s.replaceAll('"', '""')}"` : s
    }
    const text = [header, ...rows].map((r) => r.map(quote).join(',')).join('\n')
    const url = URL.createObjectURL(new Blob([text], {type: 'text/csv'}))
    const a = document.createElement('a')
    a.href = url
    a.download = name
    a.click()
    URL.revokeObjectURL(url)
}

/** What each counted kind is called on the page. */
export const EVENT_LABELS: Record<string, string> = {
    call_minutes: 'Call minutes',
    call_started: 'Calls',
    community_left: 'Left a community',
    community_message: 'Community messages',
    everyone_message: 'Messages to everyone',
    here_message: 'Messages to people nearby',
    prim_created: 'Prims built',
    prim_edited: 'Prims edited',
    private_message: 'Private messages',
}

export const CLIENT_LABELS: Record<string, string> = {
    android: 'Android app',
    chat: 'Web chat',
    desktop: 'Desktop app',
    unknown: 'Not named',
    web: 'App in a browser',
}

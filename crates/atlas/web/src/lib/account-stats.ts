import type {AccountSeen, AccountStats} from '@/types'

import {api, notifier} from '@/app'
import {ApiError} from '@/lib/api'
import {local} from '@/lib/reactive'

/**
 * What Admin › Accounts shows about each account beyond its row: whether it is
 * here, when it last was, and — for the one line opened — what it has done
 * (`docs/mockups/admin-accounts/`, the simulator's `stats::accounts`). Kept out
 * of `$s` as the stats pages' numbers are: they are the server's on every visit.
 */
export type AccountSort = 'name' | 'newest' | 'seen'
export type AccountFilter = 'admin' | 'all' | 'disabled' | 'unconfirmed'

export const activity = local({
    filter: 'all' as AccountFilter,
    /** The account whose line is open, or '' for none. */
    open: '',
    seen: {} as Record<string, AccountSeen>,
    sort: 'seen' as AccountSort,
    /** The opened lines' numbers, by account id; fetched on each opening. */
    stats: {} as Record<string, AccountStats | null>,
})

function report(e: unknown, fallback: string) {
    if (e instanceof ApiError) {
        if (!e.isUnauthorized) notifier.error(e.message)
        return
    }
    notifier.error(fallback)
}

export async function loadActivity() {
    try {
        activity.seen = await api.get<Record<string, AccountSeen>>('/api/admin/accounts/activity')
    } catch (e) {
        report(e, 'Could not load when accounts were last seen')
    }
}

/** Open a line, or close it when it is the open one. */
export function toggleOpen(id: string) {
    if (activity.open === id) {
        activity.open = ''
        return
    }
    activity.open = id
    void loadAccountStats(id)
}

async function loadAccountStats(id: string) {
    try {
        const stats = await api.get<AccountStats>(`/api/admin/accounts/${id}/stats`)
        activity.stats = {...activity.stats, [id]: stats}
    } catch (e) {
        activity.stats = {...activity.stats, [id]: null}
        report(e, 'Could not load that account’s numbers')
    }
}

/** "now", "5 min ago", "3 h ago", "yesterday", "4 days ago", or a date. */
export function ago(unixS: null | number, nowMs = Date.now()): string {
    if (unixS === null) return 'not seen lately'
    const s = Math.max(0, Math.round(nowMs / 1000 - unixS))
    if (s < 120) return 'just now'
    if (s < 3600) return `${Math.round(s / 60)} min ago`
    if (s < 86_400) return `${Math.round(s / 3600)} h ago`
    const days = Math.floor(s / 86_400)
    if (days === 1) return 'yesterday'
    if (days < 14) return `${days} days ago`
    if (days < 60) return `${Math.round(days / 7)} weeks ago`
    return new Date(unixS * 1000).toLocaleDateString(undefined, {day: 'numeric', month: 'short', year: 'numeric'})
}

/** Minutes as "45 min" or "3 h 20 min". */
export function duration(minutes: number): string {
    if (minutes < 60) return `${minutes} min`
    const h = Math.floor(minutes / 60)
    const rest = minutes % 60
    return rest ? `${h} h ${rest} min` : `${h} h`
}

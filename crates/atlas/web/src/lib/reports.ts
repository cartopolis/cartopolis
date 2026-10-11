import type {UserReport} from '@/types'

import {api, notifier} from '@/app'
import {ApiError} from '@/lib/api'

/**
 * The server's report queue: reports about a person, filed from the app and
 * triaged by an operator holding `reports_triage`. Kept out of `$s` because it
 * is the server's state on every visit, never something to restore from
 * localStorage.
 */
function report(e: unknown, fallback: string) {
    if (e instanceof ApiError) {
        if (!e.isUnauthorized) notifier.error(e.message)
        return
    }
    notifier.error(fallback)
}

export async function loadReports(all: boolean): Promise<UserReport[] | null> {
    try {
        return await api.get<UserReport[]>(`/api/admin/reports${all ? '?all=true' : ''}`)
    } catch (e) {
        report(e, 'Could not load the reports')
        return null
    }
}

/** Close a report, saying what was done about it: the queue keeps the answer. */
export async function resolveReport(id: number, resolution: string): Promise<boolean> {
    try {
        await api.patch(`/api/admin/reports/${id}`, {resolution})
        notifier.success('Report closed')
        return true
    } catch (e) {
        report(e, 'Could not close that report')
        return false
    }
}

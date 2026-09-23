import type {AutopilotConfig} from '@/types'

import {$s, api, notifier} from '@/app'
import {ApiError} from '@/lib/api'

/**
 * The development lane's throttle, as the console reads and writes it.
 *
 * Same split as `pipeline.ts` and for the same reason: each of these writes the
 * store and decides what a refusal means, and the component that lays out the
 * sliders should do neither.
 */
function report(e: unknown, fallback: string) {
    if (e instanceof ApiError) {
        if (!e.isUnauthorized) {
            notifier.error(e.message)
        }
        return
    }
    notifier.error(fallback)
}

export async function loadAutopilot(): Promise<void> {
    try {
        const result = await api.get<{config: AutopilotConfig}>('/api/pipeline/autopilot')
        $s.autopilot = result.config
    } catch (e) {
        // A 503 is the server saying the autopilot does not run beside it; the
        // page draws that sentence rather than dead sliders.
        $s.autopilot = null
        report(e, 'Could not reach the autopilot')
    }
}

/**
 * Send the named fields and take the server's clamped answer as the new truth —
 * the store holds what the file now says, not what the slider asked for.
 */
export async function saveAutopilot(patch: Partial<AutopilotConfig>): Promise<void> {
    try {
        const result = await api.patch<{config: AutopilotConfig}>('/api/pipeline/autopilot', patch)
        $s.autopilot = result.config
    } catch (e) {
        report(e, 'Could not update the autopilot')
        // The slider showed a value the file never took; re-read to un-lie it.
        void loadAutopilot()
    }
}

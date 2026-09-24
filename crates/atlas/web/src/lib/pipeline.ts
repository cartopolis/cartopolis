import type {
    PipelineConfig,
    PipelineCoverage,
    PipelineJob,
    PipelineLogLine,
} from '@/types'

import {$s, api, notifier} from '@/app'
import {ApiError} from '@/lib/api'

/**
 * Everything the Data tab can ask the server to do.
 *
 * Same split as `accounts.ts` and for the same reason: each of these writes the
 * store, says something to the user and decides what a refusal means, and a
 * component that also lays out a form ends up doing two of the three.
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

/** The areas, the datasets and where the output goes. Asked once per sign-in. */
export async function loadPipelineConfig(): Promise<void> {
    try {
        $s.pipelineConfig = await api.get<PipelineConfig>('/api/pipeline/config')
    } catch (e) {
        // A pipeline server that is not deployed, or is down. The page says so
        // rather than drawing a form whose every button would fail.
        $s.pipelineConfig = null
        report(e, 'Could not reach the data pipeline')
    }
}

export async function loadCoverage(): Promise<void> {
    try {
        const result = await api.get<{coverage: PipelineCoverage[]}>('/api/pipeline/coverage')
        $s.pipelineCoverage = result.coverage
    } catch (e) {
        report(e, 'Could not read the coverage manifests')
    }
}

export async function loadJobs(): Promise<void> {
    try {
        const result = await api.get<{jobs: PipelineJob[]}>('/api/pipeline/jobs')
        $s.pipelineJobs = result.jobs
    } catch (e) {
        report(e, 'Could not list the runs')
    }
}

/**
 * Start a run and immediately follow it.
 *
 * The server answers with a `replaces` list — the datasets whose manifest is
 * currently stamped with a *different* area name. That matters because each
 * extractor rewrites the whole manifest for its dataset: extracting Amsterdam
 * does not add to Groningen, it takes its place, and the Groningen tiles stay on
 * disk while the client stops being told they are there. The web UI reports it
 * as a warning that outlives a toast, because it is the one irreversible thing
 * this page can do.
 */
export async function startRun(
    area: string,
    datasets: string[],
    bbox?: [number, number, number, number],
    fresh = false,
): Promise<null | PipelineJob> {
    try {
        const result = await api.post<{job: PipelineJob; replaces: Array<{dataset: string; name: string}>}>(
            '/api/pipeline/jobs',
            bbox ? {area, bbox, datasets, fresh} : {area, datasets, fresh},
        )
        $s.pipelineJobs = [result.job, ...$s.pipelineJobs]
        follow(result.job.id)
        if (result.replaces.length) {
            const names = [...new Set(result.replaces.map((r) => r.name))].join(', ')
            notifier.error(
                `This run replaces the coverage for ${names} in ` +
                `${result.replaces.map((r) => r.dataset).join(', ')} — those cells stay on disk ` +
                `but the client will stop being told about them.`,
            )
        } else {
            notifier.success(`Extracting ${area}`)
        }
        return result.job
    } catch (e) {
        report(e, 'Could not start that run')
        return null
    }
}

export async function cancelRun(id: string): Promise<void> {
    try {
        await api.post(`/api/pipeline/jobs/${id}/cancel`)
        notifier.info('Stopping — the current request has to finish first')
    } catch (e) {
        report(e, 'Could not stop that run')
    }
}

/**
 * How many lines of the followed run's log have been received — the server's
 * count of lines ever written, not the length of `$s.pipelineLog`. The server
 * keeps the last 2,000 lines and drops older ones, so once a long run has
 * dropped some, the number of lines drawn here and the number written there are
 * different numbers. The old cursor, `pipelineLog.length`, stopped the log dead
 * the moment a BAG run filled that buffer.
 */
let logCursor = 0

/** The most lines drawn at once — the same bound the server keeps. */
const LOG_LIMIT = 2_000

export function follow(id: string): void {
    $s.pipelineFollowing = id
    $s.pipelineLog = []
    logCursor = 0
}

/**
 * One poll of the followed run.
 *
 * `since` is a line count, not a timestamp: the web UI keeps what it has drawn
 * and asks for the rest, so a run that logs nothing costs an empty array rather
 * than re-sending the last two thousand lines every second. Returns whether the
 * run is still going, which is what stops the timer.
 */
export async function pollFollowed(): Promise<boolean> {
    const id = $s.pipelineFollowing
    if (!id) {
        return false
    }
    try {
        const result = await api.get<{job: PipelineJob; lines: PipelineLogLine[]; total: number}>(
            `/api/pipeline/jobs/${id}/log?since=${logCursor}`,
        )
        // Another run may have been followed while this request was in flight.
        if ($s.pipelineFollowing !== id) {
            return true
        }
        logCursor = result.total
        if (result.lines.length) {
            $s.pipelineLog = [...$s.pipelineLog, ...result.lines].slice(-LOG_LIMIT)
        }
        $s.pipelineJobs = $s.pipelineJobs.map((job) => (job.id === id ? result.job : job))
        const live = result.job.status === 'running' || result.job.status === 'queued'
        if (!live) {
            // The run has stopped, so what it wrote is now what the tree holds.
            void loadCoverage()
        }
        return live
    } catch {
        // A failed poll is not worth a toast per second; the next one either
        // works or the 401 path signs the app out.
        return true
    }
}

import {useEffect, useRef, useState} from 'preact/hooks'

import type {PipelineCount, PipelineJob} from '@/types'

import {$s} from '@/app'
import {
    cancelRun,
    follow,
    loadCoverage,
    loadJobs,
    loadPipelineConfig,
    pollFollowed,
    startRun,
} from '@/lib/pipeline'
import {formatTimestamp} from '@/lib/utils'

/** How often a running job's log is pulled. */
const POLL_MS = 1_000

function statusLabel(job: PipelineJob): string {
    switch (job.status) {
    case 'queued':
        return 'waiting for the runner'
    case 'running':
        return 'running'
    case 'done':
        return 'finished'
    case 'failed':
        return 'failed'
    case 'cancelled':
        return 'stopped'
    }
}

/** "94,052 buildings, 781 berths" — the zeroes left out, since they say nothing. */
function counts(list?: readonly PipelineCount[]): string {
    if (!list) {
        return ''
    }
    return list
        .filter((count) => count.n > 0)
        .map((count) => `${count.n.toLocaleString()} ${count.label}`)
        .join(', ')
}

/**
 * The Data tab: the map data pipelines.
 *
 * Three things, in the order an operator needs them: what the served tree covers
 * now, a form to refresh or extend it, and the log of the run that is going.
 *
 * The coverage panel is first on purpose. Every button below it rewrites a
 * manifest, and "what would I be replacing?" is the question this page exists to
 * answer before it is the one that runs anything.
 */
export const Pipeline = () => {
    const [area, setArea] = useState('groningen')
    const [custom, setCustom] = useState('')
    const [customName, setCustomName] = useState('')
    const [picked, setPicked] = useState<string[]>([])
    const [fresh, setFresh] = useState(false)
    const logRef = useRef<HTMLPreElement>(null)

    useEffect(() => {
        void loadPipelineConfig()
        void loadCoverage()
        void loadJobs()
    }, [])

    // Follow whatever is already running when the page opens, so an operator who
    // reloads mid-run is not shown an idle Data tab over a live job.
    useEffect(() => {
        if (!$s.pipelineFollowing) {
            const live = $s.pipelineJobs.find((job) => job.status === 'running' || job.status === 'queued')
            if (live) {
                follow(live.id)
            }
        }
    }, [$s.pipelineJobs.length])

    useEffect(() => {
        if (!$s.pipelineFollowing) {
            return
        }
        let stopped = false
        let timer = 0
        const tick = async () => {
            const live = await pollFollowed()
            if (stopped) {
                return
            }
            if (live) {
                timer = window.setTimeout(() => void tick(), POLL_MS)
            } else {
                // One last list refresh, so the history row shows its verdict.
                void loadJobs()
            }
        }
        void tick()
        return () => {
            stopped = true
            window.clearTimeout(timer)
        }
    }, [$s.pipelineFollowing])

    // Stick to the bottom of the log while it grows, which is where a run says
    // what it is doing now.
    useEffect(() => {
        if (logRef.current) {
            logRef.current.scrollTop = logRef.current.scrollHeight
        }
    }, [$s.pipelineLog.length])

    const config = $s.pipelineConfig
    const followed = $s.pipelineJobs.find((job) => job.id === $s.pipelineFollowing)
    const busy = $s.pipelineJobs.some((job) => job.status === 'running' || job.status === 'queued')

    if (!config) {
        return (
            <>
                <div class="section-head">
                    <h1>Data</h1>
                </div>
                <div class="empty">
                    The data pipeline is not answering. It runs beside the simulator on the
                    machine that holds the map tiles — if it is down, nothing here can start a
                    run, but the tiles already extracted are still being served.
                </div>
            </>
        )
    }

    const isCustom = area === '__custom'
    const minutes = config.datasets
        .filter((dataset) => picked.includes(dataset.id))
        .reduce((sum, dataset) => sum + dataset.typicalMinutes, 0)

    const submit = () => {
        if (!picked.length) {
            return
        }
        if (isCustom) {
            const parts = custom.trim().split(/[\s,]+/).map(Number)
            if (parts.length !== 4 || parts.some((v) => !Number.isFinite(v))) {
                return
            }
            void startRun(customName.trim(), picked, parts as [number, number, number, number], fresh)
        } else {
            void startRun(area, picked, undefined, fresh)
        }
    }

    return (
        <>
            <div class="section-head">
                <h1>Data</h1>
                <span class="count">{config.datasets.length}</span>
            </div>
            <p class="hint">
                The map's Dutch layers are extracted from open registers and served as static
                files from <span class="mono">{config.out}</span>. A run refreshes them in place.
            </p>

            <div class="card">
                <h2>What is being served</h2>
                <ul class="coverage-list">
                    {$s.pipelineCoverage.map((entry) => (
                        <li class="coverage" key={entry.dataset}>
                            <div class="coverage-main">
                                <span class="coverage-name">{entry.label}</span>
                                <span style="flex:1" />
                                {entry.name ? (
                                    <span class="tag">{entry.name}</span>
                                ) : (
                                    <span class="tag disabled">never extracted</span>
                                )}
                            </div>
                            {entry.name && (
                                <div class="account-meta">
                                    <span>{entry.cells.toLocaleString()} cells</span>
                                    {counts(entry.counts) && <span>{counts(entry.counts)}</span>}
                                    {entry.updatedAt && (
                                        <span>{formatTimestamp(new Date(entry.updatedAt).toISOString())}</span>
                                    )}
                                </div>
                            )}
                        </li>
                    ))}
                </ul>
            </div>

            <div class="card">
                <h2>Extract</h2>
                <label class="field">
                    <span>Area</span>
                    <select
                        onChange={(e) => setArea((e.target as HTMLSelectElement).value)}
                        value={area}
                    >
                        {config.areas.map((a) => (
                            <option key={a.id} value={a.id}>
                                {a.label} — {a.km2} km²
                            </option>
                        ))}
                        <option value="__custom">A bounding box I will type…</option>
                    </select>
                </label>

                {isCustom && (
                    <>
                        <label class="field">
                            <span>Name</span>
                            <input
                                onInput={(e) => setCustomName((e.target as HTMLInputElement).value)}
                                placeholder="delfzijl"
                                value={customName}
                            />
                        </label>
                        <label class="field">
                            <span>Bounding box</span>
                            <input
                                onInput={(e) => setCustom((e.target as HTMLInputElement).value)}
                                placeholder="229000 578000 238000 587000"
                                value={custom}
                            />
                        </label>
                        <p class="hint">
                            Four numbers — xmin ymin xmax ymax — in RD New (EPSG:28992), which is
                            the grid these registers are published on. Not degrees.
                        </p>
                    </>
                )}

                <div class="dataset-picks">
                    {config.datasets.map((dataset) => (
                        <label class="check" key={dataset.id}>
                            <input
                                checked={picked.includes(dataset.id)}
                                onChange={(e) => {
                                    const on = (e.target as HTMLInputElement).checked
                                    setPicked((was) =>
                                        on ? [...was, dataset.id] : was.filter((id) => id !== dataset.id),
                                    )
                                }}
                                type="checkbox"
                            />
                            <span>
                                <b>{dataset.label}</b>
                                <em>{dataset.description}</em>
                            </span>
                        </label>
                    ))}
                </div>

                <label class="check">
                    <input
                        checked={fresh}
                        onChange={(e) => setFresh((e.target as HTMLInputElement).checked)}
                        type="checkbox"
                    />
                    <span>
                        <b>Fetch fresh source data</b>
                        <em>
                            Otherwise what was fetched from the registers in the last month is
                            reused, which is what a re-run after a pipeline change wants.
                        </em>
                    </span>
                </label>

                <div class="row-actions">
                    <button
                        disabled={busy || !picked.length || (isCustom && (!custom.trim() || !customName.trim()))}
                        onClick={submit}
                        type="button"
                    >
                        {busy ? 'A run is already going' : 'Start'}
                    </button>
                    {picked.length > 0 && !busy && (
                        <span class="hint">roughly {minutes} minutes for an area this size</span>
                    )}
                </div>
            </div>

            {followed && (
                <div class="card">
                    <h2>
                        {followed.area} — {statusLabel(followed)}
                    </h2>
                    <ul class="step-list">
                        {followed.steps.map((step) => (
                            <li class={`step is-${step.status}`} key={step.dataset}>
                                <span class="step-name">{step.dataset}</span>
                                <span class="step-detail">
                                    {step.status === 'running' && step.progress.phase
                                        ? `${step.progress.phase} — ${(step.progress.done ?? 0).toLocaleString()}` +
                                          (step.progress.total ? ` / ${step.progress.total.toLocaleString()}` : '')
                                        : step.status === 'done'
                                          ? counts(step.counts) || 'finished'
                                          : step.error || statusLabel({...followed, status: step.status})}
                                </span>
                            </li>
                        ))}
                    </ul>
                    <pre class="run-log" ref={logRef}>
                        {$s.pipelineLog.map((line) => `${line.dataset}  ${line.text}`).join('\n')}
                    </pre>
                    {(followed.status === 'running' || followed.status === 'queued') && (
                        <div class="row-actions">
                            <button class="danger" onClick={() => void cancelRun(followed.id)} type="button">
                                Stop
                            </button>
                            <span class="hint">
                                What has already been written stays written — the manifest is
                                written last, so a stopped run leaves the previous one serving.
                            </span>
                        </div>
                    )}
                </div>
            )}

            {$s.pipelineJobs.length > 1 && (
                <div class="card">
                    <h2>Earlier runs</h2>
                    <ul class="account-list">
                        {$s.pipelineJobs
                            .filter((job) => job.id !== $s.pipelineFollowing)
                            .map((job) => (
                                <li class="account" key={job.id}>
                                    <div class="account-main">
                                        <span class="account-name">{job.area}</span>
                                        <span style="flex:1" />
                                        <button class="ghost" onClick={() => follow(job.id)} type="button">
                                            Show log
                                        </button>
                                    </div>
                                    <div class="account-meta">
                                        <span>{statusLabel(job)}</span>
                                        <span>{job.steps.map((s) => s.dataset).join(', ')}</span>
                                        <span>{job.by}</span>
                                        <span>{formatTimestamp(new Date(job.createdAt).toISOString())}</span>
                                    </div>
                                </li>
                            ))}
                    </ul>
                </div>
            )}
        </>
    )
}

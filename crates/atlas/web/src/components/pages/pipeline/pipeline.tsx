import m, {MithrilComponent} from '@garage44/mithril'
import {Badge, Button, Card, FieldCheckbox, FieldSelect, FieldText, Progress} from '@garage44/mithril/components'
import {check, required, validation, validations} from '@garage44/mithril/validation'

import type {Tone} from '@garage44/mithril/components'
import type {JobStatus, PipelineCount, PipelineJob} from '@/types'

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
import {bind, local} from '@/lib/reactive'
import {formatTimestamp} from '@/lib/utils'

/** How often a running job's log is pulled. */
const POLL_MS = 1_000

/** The `area` value that means "the bounding box typed below", not a configured area. */
const CUSTOM = '__custom'

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

/*
 * A stopped run is neutral rather than a warning: stopping is something the
 * operator did, and the previous manifest is still serving, so nothing is wrong.
 */
const STATUS_TONE: Record<JobStatus, Tone> = {
    cancelled: 'neutral',
    done: 'success',
    failed: 'danger',
    queued: 'warning',
    running: 'accent',
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

/** The typed bounding box as four numbers, or null when it is not four numbers. */
function parseBbox(text: string): [number, number, number, number] | null {
    const parts = text.trim().split(/[\s,]+/).map(Number)
    if (parts.length !== 4 || parts.some((v) => !Number.isFinite(v))) {
        return null
    }
    return parts as [number, number, number, number]
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
export class Pipeline extends MithrilComponent {
    s = local({
        area: 'groningen',
        custom: '',
        customName: '',
        fresh: false,
        picked: [] as string[],
    })

    /*
     * Only the typed-area fields have rules, and they only count while that area
     * is chosen: a configured area needs neither. The four-numbers rule is the one
     * `submit` used to apply silently — a box that was not four numbers made Start
     * do nothing at all — so it now says what is wrong instead.
     */
    v = validations({
        custom: validation(bind(this.s, 'custom'), [
            required,
            check((text: string) => parseBbox(text) !== null, 'Four numbers: xmin ymin xmax ymax'),
        ]),
        customName: validation(bind(this.s, 'customName'), [required]),
    })

    /** The run log's `<pre>`, while it is on the page. */
    private logEl: HTMLPreElement | null = null

    /** The `pipelineJobs.length` the live-run check last acted on. */
    private seenJobs: number | undefined = undefined

    /** The `pipelineFollowing` the poll loop was started for. */
    private seenFollowing: string | null | undefined = undefined

    /** Stops the running poll loop: what the effect's cleanup was. */
    private stopPoll: (() => void) | null = null

    /** The `pipelineLog.length` the log was last scrolled for. */
    private seenLog: number | undefined = undefined

    oninit() {
        void loadPipelineConfig()
        void loadCoverage()
        void loadJobs()
    }

    // The three effects that ran after a render with dependencies: each compares
    // its dependency against the value it last acted on.
    private sync() {
        // Follow whatever is already running when the page opens, so an operator who
        // reloads mid-run is not shown an idle Data tab over a live job.
        if (this.seenJobs !== $s.pipelineJobs.length) {
            this.seenJobs = $s.pipelineJobs.length
            if (!$s.pipelineFollowing) {
                const live = $s.pipelineJobs.find((job) => job.status === 'running' || job.status === 'queued')
                if (live) {
                    follow(live.id)
                }
            }
        }

        if (this.seenFollowing !== $s.pipelineFollowing) {
            this.seenFollowing = $s.pipelineFollowing
            this.stopPoll?.()
            this.stopPoll = null
            if ($s.pipelineFollowing) {
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
                this.stopPoll = () => {
                    stopped = true
                    window.clearTimeout(timer)
                }
            }
        }

        // Stick to the bottom of the log while it grows, which is where a run says
        // what it is doing now.
        if (this.seenLog !== $s.pipelineLog.length) {
            this.seenLog = $s.pipelineLog.length
            if (this.logEl) {
                this.logEl.scrollTop = this.logEl.scrollHeight
            }
        }
    }

    oncreate() {
        this.sync()
    }

    onupdate() {
        this.sync()
    }

    onremove() {
        this.stopPoll?.()
        this.stopPoll = null
    }

    view() {
        const s = this.s
        const config = $s.pipelineConfig
        const followed = $s.pipelineJobs.find((job) => job.id === $s.pipelineFollowing)
        const busy = $s.pipelineJobs.some((job) => job.status === 'running' || job.status === 'queued')

        if (!config) {
            return (
                <Card>
                    <p class="pipeline-muted">
                        The data pipeline is not answering. It runs beside the simulator on the
                        machine that holds the map tiles — if it is down, nothing here can start a
                        run, but the tiles already extracted are still being served.
                    </p>
                </Card>
            )
        }

        const isCustom = s.area === CUSTOM
        const minutes = config.datasets
            .filter((dataset) => s.picked.includes(dataset.id))
            .reduce((sum, dataset) => sum + dataset.typicalMinutes, 0)

        const submit = () => {
            if (!s.picked.length) {
                return
            }
            if (isCustom) {
                if (!this.v.$validate()) {
                    return
                }
                const bbox = parseBbox(s.custom)
                if (!bbox) {
                    return
                }
                void startRun(s.customName.trim(), s.picked, bbox, s.fresh)
            } else {
                void startRun(s.area, s.picked, undefined, s.fresh)
            }
        }

        // FieldSelect's model may be written `null` (its placeholder entry); this
        // one has no placeholder, so a null never arrives and is ignored if it does.
        const area = {
            get value(): string | null {
                return s.area
            },
            set value(next: string | null) {
                if (next !== null) {
                    s.area = next
                }
            },
        }

        const picks = (id: string) => ({
            get value() {
                return s.picked.includes(id)
            },
            set value(on: boolean) {
                const was = s.picked
                s.picked = on ? [...was, id] : was.filter((picked) => picked !== id)
            },
        })

        return (
            <>
                <p class="pipeline-intro">
                    <Badge>{config.datasets.length} datasets</Badge>{' '}
                    The map's Dutch layers are extracted from open registers and served as static
                    files from <code>{config.out}</code>. A run refreshes them in place.
                </p>

                <Card title="What is being served">
                    <ul class="pipeline-coverage">
                        {$s.pipelineCoverage.map((entry) => (
                            <li key={entry.dataset}>
                                <div class="pipeline-row">
                                    <span class="pipeline-name">{entry.label}</span>
                                    {entry.name ? (
                                        <Badge tone="info">{entry.name}</Badge>
                                    ) : (
                                        <Badge>never extracted</Badge>
                                    )}
                                </div>
                                {entry.name && (
                                    <div class="pipeline-meta">
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
                </Card>

                <Card title="Extract">
                    <div class="pipeline-form">
                        <FieldSelect
                            label="Area"
                            model={area}
                            options={[
                                ...config.areas.map((a) => ({label: `${a.label} — ${a.km2} km²`, value: a.id})),
                                {label: 'A bounding box I will type…', value: CUSTOM},
                            ]}
                        />

                        {isCustom && (
                            <>
                                <FieldText
                                    label="Name"
                                    model={bind(s, 'customName')}
                                    placeholder="delfzijl"
                                    validation={this.v.customName}
                                />
                                <FieldText
                                    help={
                                        'Four numbers — xmin ymin xmax ymax — in RD New (EPSG:28992), which is ' +
                                        'the grid these registers are published on. Not degrees.'
                                    }
                                    label="Bounding box"
                                    model={bind(s, 'custom')}
                                    placeholder="229000 578000 238000 587000"
                                    validation={this.v.custom}
                                />
                            </>
                        )}

                        <div class="pipeline-picks">
                            {config.datasets.map((dataset) => (
                                <FieldCheckbox
                                    help={dataset.description}
                                    key={dataset.id}
                                    label={<b>{dataset.label}</b>}
                                    model={picks(dataset.id)}
                                />
                            ))}
                        </div>

                        <FieldCheckbox
                            help={
                                'Otherwise what was fetched from the registers in the last month is ' +
                                'reused, which is what a re-run after a pipeline change wants.'
                            }
                            label={<b>Fetch fresh source data</b>}
                            model={bind(s, 'fresh')}
                        />

                        <div class="pipeline-actions">
                            <Button
                                disabled={busy || !s.picked.length || (isCustom && !this.v.$valid)}
                                onclick={submit}
                                variant="primary"
                            >
                                {busy ? 'A run is already going' : 'Start'}
                            </Button>
                            {s.picked.length > 0 && !busy && (
                                <span class="pipeline-muted">roughly {minutes} minutes for an area this size</span>
                            )}
                        </div>
                    </div>
                </Card>

                {followed && (
                    <Card
                        actions={
                            <Badge dot tone={STATUS_TONE[followed.status]}>
                                {statusLabel(followed)}
                            </Badge>
                        }
                        title={followed.area}
                    >
                        <ul class="pipeline-steps">
                            {followed.steps.map((step) => (
                                <li class={`is-${step.status}`} key={step.dataset}>
                                    <span class="pipeline-step-name">{step.dataset}</span>
                                    <span class="pipeline-step-detail">
                                        {step.status === 'running' && step.progress.phase
                                            ? `${step.progress.phase} — ${(step.progress.done ?? 0).toLocaleString()}` +
                                              (step.progress.total ? ` / ${step.progress.total.toLocaleString()}` : '')
                                            : step.status === 'done'
                                              ? counts(step.counts) || 'finished'
                                              : step.error || statusLabel({...followed, status: step.status})}
                                        {/*
                                          * A bar only where the step reports a total: a phase
                                          * that counts with no known end would otherwise draw a
                                          * bar that is always full or always empty.
                                          */}
                                        {step.status === 'running' && !!step.progress.total && (
                                            <Progress max={step.progress.total} value={step.progress.done ?? 0} />
                                        )}
                                    </span>
                                </li>
                            ))}
                        </ul>
                        <pre
                            class="pipeline-log"
                            oncreate={(v: m.VnodeDOM) => {
                                this.logEl = v.dom as HTMLPreElement
                            }}
                            onremove={() => {
                                this.logEl = null
                            }}
                        >
                            {$s.pipelineLog.map((line) => `${line.dataset}  ${line.text}`).join('\n')}
                        </pre>
                        {(followed.status === 'running' || followed.status === 'queued') && (
                            <div class="pipeline-actions">
                                <Button onclick={() => void cancelRun(followed.id)} variant="danger">
                                    Stop
                                </Button>
                                <span class="pipeline-muted">
                                    What has already been written stays written — the manifest is
                                    written last, so a stopped run leaves the previous one serving.
                                </span>
                            </div>
                        )}
                    </Card>
                )}

                {$s.pipelineJobs.length > 1 && (
                    <Card title="Earlier runs">
                        <ul class="pipeline-runs">
                            {$s.pipelineJobs
                                .filter((job) => job.id !== $s.pipelineFollowing)
                                .map((job) => (
                                    <li key={job.id}>
                                        <div class="pipeline-row">
                                            <span class="pipeline-name">{job.area}</span>
                                            <Badge tone={STATUS_TONE[job.status]}>{statusLabel(job)}</Badge>
                                            <Button onclick={() => follow(job.id)} size="sm" variant="ghost">
                                                Show log
                                            </Button>
                                        </div>
                                        <div class="pipeline-meta">
                                            <span>{job.steps.map((step) => step.dataset).join(', ')}</span>
                                            <span>{job.by}</span>
                                            <span>{formatTimestamp(new Date(job.createdAt).toISOString())}</span>
                                        </div>
                                    </li>
                                ))}
                        </ul>
                    </Card>
                )}
            </>
        )
    }
}

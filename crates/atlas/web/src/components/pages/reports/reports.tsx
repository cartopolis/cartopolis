import m, {MithrilComponent} from '@garage44/mithril'
import {Button, Card, FieldCheckbox, FieldText} from '@garage44/mithril/components'

import type {UserReport} from '@/types'

import {bind, local} from '@/lib/reactive'
import {loadReports, resolveReport} from '@/lib/reports'
import {formatTimestamp} from '@/lib/utils'

/** The reasons the app's Report offers, as the server names them. */
const REASONS: Record<string, string> = {
    harassment: 'Harassment or bullying',
    hate_speech: 'Hate speech or a slur',
    other: 'Something else',
    sexual_content: 'Sexual content',
    spam: 'Spam or a scam',
    threat: 'Threat or self-harm',
}

/**
 * Reports about a person, filed from the app: Admin › People › Reports.
 *
 * A report about a *line in a community's topic* is not here: it goes to that
 * community's moderators, through the chat (`docs/mockups/web-navigation/`,
 * "Decided"). This is the server's own queue, which until this page had no
 * screen at all — the simulator stored them and nobody could read them
 * without shell access.
 */
export class Reports extends MithrilComponent {
    s = local({
        all: false,
        loaded: false,
        /** What the operator is writing about each report, by id. */
        notes: {} as Record<string, string>,
        reports: [] as UserReport[],
    })

    /** The checkbox's model: changing it reloads the list. */
    showAll = (() => {
        const page = this
        return {
            get value() {
                return page.s.all
            },
            set value(v: boolean) {
                page.s.all = v
                void page.load()
            },
        }
    })()

    oninit() {
        void this.load()
    }

    async load() {
        const got = await loadReports(this.s.all)
        if (got) this.s.reports = got
        this.s.loaded = true
        m.redraw()
    }

    async resolve(r: UserReport) {
        const note = (this.s.notes[r.id] ?? '').trim()
        if (await resolveReport(r.id, note)) {
            delete this.s.notes[r.id]
            await this.load()
        }
    }

    view() {
        const s = this.s
        const open = s.reports.filter((r) => !r.resolved).length
        return (
            <div class="reports">
                <div class="reports-bar">
                    <p class="reports-count">
                        {!s.loaded ? 'Loading…' : open === 0 ? 'No open reports' : `${open} open ${open === 1 ? 'report' : 'reports'}`}
                    </p>
                    <FieldCheckbox
                        label="Show closed ones too"
                        model={this.showAll}
                    />
                </div>
                {s.loaded && s.reports.length === 0 && (
                    <p class="reports-empty">
                        Nobody has reported anyone. Reports about a line in a community go to that community's moderators instead.
                    </p>
                )}
                {s.reports.map((r) => (
                    <Card class={r.resolved ? 'reports-item is-closed' : 'reports-item'} key={r.id}>
                        <div class="reports-head">
                            <b>{r.subject_username}</b>
                            <span class="reports-reason">{REASONS[r.reason] ?? r.reason}</span>
                            <span class="reports-when">{formatTimestamp(r.created_at)}</span>
                        </div>
                        <p class="reports-by">
                            Reported by {r.reporter_username || 'an account that has since been deleted'}
                        </p>
                        {r.message && <blockquote class="reports-line">“{r.message}”</blockquote>}
                        {r.note && <p class="reports-note">{r.note}</p>}
                        {r.resolved ? (
                            <p class="reports-done">
                                Closed {r.resolved_at ? formatTimestamp(r.resolved_at) : ''}
                                {r.resolution ? `: ${r.resolution}` : ''}
                            </p>
                        ) : (
                            <div class="reports-act">
                                <FieldText
                                    label="What was done (kept with the report)"
                                    model={bind(s.notes, String(r.id))}
                                    placeholder="Warned them; disabled the account; nothing wrong"
                                />
                                <Button onclick={() => void this.resolve(r)} size="sm" variant="primary">
                                    Close the report
                                </Button>
                            </div>
                        )}
                    </Card>
                ))}
            </div>
        )
    }
}

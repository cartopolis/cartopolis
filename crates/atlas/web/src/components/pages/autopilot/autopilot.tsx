import {useEffect, useState} from 'preact/hooks'

import type {AutopilotPlanItem} from '@/types'

import {$s} from '@/app'
import {loadAutopilot, saveAutopilot} from '@/lib/autopilot'

/**
 * The development lane's throttle, and the queue it works through.
 *
 * Everything here is one JSON file the autopilot re-reads every tick, so a
 * change takes effect on its next pass with nothing restarted. Two write
 * rhythms, and the difference is what a mistake costs: a slider or a switch
 * saves on release, while the focus sentence and each plan row track the store
 * as you type and save on blur — a PATCH per keystroke is a write per letter,
 * and the sentence is a paragraph.
 */

const sliderCommit =
    (field: 'ticketsPerDay' | 'qaPerDay') => (e: Event) => {
        void saveAutopilot({[field]: Number((e.target as HTMLInputElement).value)})
    }

const sliderTrack =
    (field: 'ticketsPerDay' | 'qaPerDay') => (e: Event) => {
        if ($s.autopilot) {
            $s.autopilot[field] = Number((e.target as HTMLInputElement).value)
        }
    }

/*
 * Where the lane files. A filed row links to its ticket, and that ticket lives
 * in the repository the autopilot is pointed at — which this page cannot ask it
 * for, the control file carrying issue *numbers* and nothing else. So it is a
 * constant, and a hand-synced seam like the config field list in
 * atlas's `autopilot.rs`: move the lane to another repository and this moves too.
 */
const FORGE_ISSUES = 'https://code.garage44.eu/jeroen/cartopolis/issues'

/**
 * Grow a field to fit what it holds.
 *
 * These rows are read on a phone, where a one-line field shows about a third of
 * a title: the list said "Add a hand…" and "Answer the" and nothing on the page
 * could tell you what the lane was about to build. So the fields are textareas
 * that take as many lines as the text needs — on mount, because the rows arrive
 * already written, and on every keystroke.
 */
const grow = (el: HTMLTextAreaElement | null) => {
    if (!el) {
        return
    }
    el.style.height = 'auto'
    // `scrollHeight` is the content plus its padding and stops there, while the
    // box these are sized in is border-box — so the borders have to be added
    // back or every field ends one hairline short and clips its last line.
    el.style.height = `${el.scrollHeight + el.offsetHeight - el.clientHeight}px`
}

/** The plan as the store holds it, copied so an edit can be built up and handed back whole. */
const plan = (): AutopilotPlanItem[] => ($s.autopilot?.focusPlan ?? []).map((item) => ({...item}))

/** Show the edit at once, and send it. The server's answer replaces the store either way. */
function commitPlan(items: AutopilotPlanItem[]) {
    if ($s.autopilot) {
        $s.autopilot.focusPlan = items
    }
    /*
     * The untitled row is the one just added and not yet written in. The server
     * drops it — a title is what becomes a ticket over there — so sending it
     * would make the row the operator is typing into vanish under the cursor.
     */
    void saveAutopilot({focusPlan: items.filter((item) => item.title.trim())})
}

export const Autopilot = () => {
    const [replanning, setReplanning] = useState(false)

    useEffect(() => {
        void loadAutopilot()
    }, [])

    /*
     * Again after the layout has settled, and again on a rotate. `grow` measures
     * the field as it stands when the row first renders, and a width that changes
     * afterwards changes how many lines the same text takes — measured at one
     * width and painted at another, a three-line note is a two-line box with its
     * last line cut in half. No dependency array on purpose: every render of this
     * list is a chance for a row's text to have changed.
     */
    useEffect(() => {
        const all = () => {
            document.querySelectorAll<HTMLTextAreaElement>('.plan-body textarea').forEach(grow)
        }
        all()
        window.addEventListener('resize', all)
        return () => window.removeEventListener('resize', all)
    })

    const config = $s.autopilot
    if (!config) {
        return (
            <>
                <div class="section-head">
                    <h1>Autopilot</h1>
                </div>
                <div class="empty">
                    The autopilot is not answering. It runs beside the pipeline on the
                    production machine, steered through a config file there — if this page
                    cannot reach it, the lane itself is untouched and keeps whatever
                    settings it had.
                </div>
            </>
        )
    }

    const filed = config.focusPlan.filter((item) => item.issue).length
    const unfiled = config.focusPlan.length - filed

    /*
     * Clearing the list is how a new plan is asked for: the lane files a planning
     * ticket whenever a focus is set and nothing in the list is still waiting to
     * be filed. The revision is what stops it recognising the question it
     * answered last time and doing nothing.
     *
     * Sending an empty list is not the same as emptying it — the server hands
     * back every row that has become a ticket, because those are being built and
     * are not the web UI's to withdraw. So this discards the unfiled tail, and
     * the new plan arrives underneath what is already in flight.
     */
    const replan = () => {
        setReplanning(false)
        void saveAutopilot({focusPlan: [], planRevision: config.planRevision + 1})
    }

    /*
     * Unfiled rows only, at both ends: a filed row is a ticket somebody is
     * building and the queue has moved past it, so it neither moves nor gets
     * stepped over. The buttons say so by being disabled — this is the same rule
     * again, for a press that reached here another way.
     */
    const move = (index: number, by: number) => {
        const items = plan()
        const to = index + by
        if (to < 0 || to >= items.length || items[index].issue > 0 || items[to].issue > 0) {
            return
        }
        const [item] = items.splice(index, 1)
        items.splice(to, 0, item)
        commitPlan(items)
    }

    const edit = (index: number, field: 'note' | 'title', value: string) => {
        const items = plan()
        items[index] = {...items[index], [field]: value}
        if ($s.autopilot) {
            $s.autopilot.focusPlan = items
        }
    }

    return (
        <>
            <div class="section-head">
                <h1>Autopilot</h1>
            </div>
            <p class="hint">
                The unattended development lane: it files tickets, builds them, merges what
                passes CI and deploys. These knobs are read on its next pass — nothing
                restarts.
            </p>

            <div class="card">
                <h2>Focus</h2>
                <label class="field">
                    <span>What to work on</span>
                    <textarea
                        onBlur={(e) => {
                            void saveAutopilot({focus: (e.target as HTMLTextAreaElement).value})
                        }}
                        onInput={(e) => {
                            if ($s.autopilot) {
                                $s.autopilot.focus = (e.target as HTMLTextAreaElement).value
                            }
                        }}
                        placeholder="Focus on improving Cartopolis on the Steam Deck runner"
                        rows={3}
                        value={config.focus}
                    />
                </label>
                <p class="hint">
                    A sentence saying what the next stretch of work is for. The lane asks a
                    session to turn it into queued work below, and quotes it into every QA
                    pass and retrospective as well. Empty it and the queue keeps whatever is
                    already on it. Nothing here lifts a fence — a focus asking for a
                    wire-protocol change still files tickets that stop for review.
                </p>

                {!config.focus && (
                    <div class="empty">
                        No focus. The lane works the frontier in the direction document, and
                        whatever is queued below.
                    </div>
                )}
            </div>

            <div class="card">
                <h2>Queue</h2>
                <p class="hint">
                    What gets built, in the order it is in, ahead of the direction document.
                    Rows come from three places — the session that plans a focus, a QA pass
                    that walked into a gap, a retrospective that found something broken —
                    and none of them files a ticket. That happens here: the lane takes the
                    top row nobody has started yet, and from then on the row is locked and
                    links to its ticket. Tap any other row to reword it.
                </p>

                {!config.focusPlan.length && (
                    <div class="empty">
                        {config.focus
                            ? 'Nothing queued yet. The lane files one planning ticket for this focus, and the rows appear here when that session answers — usually within the hour. You can also write them yourself, one at a time.'
                            : 'Nothing queued. QA passes and retrospectives add rows here as they find work, or write one yourself.'}
                    </div>
                )}

                {config.focusPlan.length > 0 && (
                    <ul class="plan-list">
                        {config.focusPlan.map((item, index) => (
                            <li class={`plan-item ${item.issue ? 'is-filed' : ''}`} key={item.id || `new-${index}`}>
                                {/*
                                  * The buttons are a column beside the row rather than a
                                  * drag handle: this list is read and reordered on a phone
                                  * as often as on a desk, and a drag inside a scrolling
                                  * page fights the scroll.
                                  */}
                                <div class="plan-move">
                                    <button
                                        class="ghost"
                                        disabled={item.issue > 0 || index === 0 || config.focusPlan[index - 1].issue > 0}
                                        onClick={() => move(index, -1)}
                                        title={item.issue > 0 ? 'Filed — the ticket is the record now' : 'Move up'}
                                        type="button"
                                    >
                                        ↑
                                    </button>
                                    <button
                                        class="ghost"
                                        disabled={
                                            item.issue > 0 ||
                                            index === config.focusPlan.length - 1 ||
                                            config.focusPlan[index + 1].issue > 0
                                        }
                                        onClick={() => move(index, 1)}
                                        title={item.issue > 0 ? 'Filed — the ticket is the record now' : 'Move down'}
                                        type="button"
                                    >
                                        ↓
                                    </button>
                                </div>
                                {/*
                                  * A filed row is not a plan any more: a session is building
                                  * it against the wording that was filed, so rewriting the
                                  * row here would describe the work as something nobody was
                                  * asked to do — the ticket is the record and it is where an
                                  * amendment belongs. Static text rather than a disabled
                                  * field, because a field that will not take a cursor reads
                                  * as broken, and the server refuses the edit anyway.
                                  */}
                                {item.issue > 0 ? (
                                    <div class="plan-body is-filed">
                                        <span class="plan-title">{item.title}</span>
                                        {item.note && <span class="plan-note">{item.note}</span>}
                                    </div>
                                ) : (
                                    <div class="plan-body">
                                        <textarea
                                            onBlur={() => commitPlan(plan())}
                                            onInput={(e) => {
                                                grow(e.currentTarget as HTMLTextAreaElement)
                                                edit(index, 'title', (e.target as HTMLTextAreaElement).value)
                                            }}
                                            placeholder="What changes"
                                            ref={grow}
                                            rows={1}
                                            value={item.title}
                                        />
                                        <textarea
                                            class="plan-note-field"
                                            onBlur={() => commitPlan(plan())}
                                            onInput={(e) => {
                                                grow(e.currentTarget as HTMLTextAreaElement)
                                                edit(index, 'note', (e.target as HTMLTextAreaElement).value)
                                            }}
                                            placeholder="Where it lands, and how it would be judged"
                                            ref={grow}
                                            rows={1}
                                            value={item.note}
                                        />
                                    </div>
                                )}
                                {/*
                                  * The ticket number is the row's one live link: it is what
                                  * says the item stopped being a plan and started being
                                  * work, and following it is how anybody sees how that work
                                  * is going. It replaces the remove button rather than
                                  * sitting beside it — the queue may not drop a row that is
                                  * being built, and a ✕ that answers "no" is worse than no ✕.
                                  */}
                                <div class="plan-side">
                                    {item.issue > 0 ? (
                                        <a
                                            class="tag"
                                            href={`${FORGE_ISSUES}/${item.issue}`}
                                            rel="noreferrer"
                                            target="_blank"
                                            title="Open the ticket"
                                        >
                                            #{item.issue}
                                        </a>
                                    ) : (
                                        <button
                                            class="ghost"
                                            onClick={() => {
                                                const items = plan()
                                                items.splice(index, 1)
                                                commitPlan(items)
                                            }}
                                            title="Remove"
                                            type="button"
                                        >
                                            ✕
                                        </button>
                                    )}
                                </div>
                            </li>
                        ))}
                    </ul>
                )}

                <div class="row-actions">
                    {/*
                      * Tracked, not saved. The server drops an untitled row — a title is
                      * what becomes a ticket over there — so saving here would answer
                      * with a list the new row is not in and take it away again the
                      * instant it appeared. It is written to the file by the blur that
                      * leaves the title field, like every other edit on this page.
                      */}
                    <button
                        onClick={() => {
                            if ($s.autopilot) {
                                $s.autopilot.focusPlan = [...plan(), {id: '', issue: 0, note: '', title: ''}]
                            }
                        }}
                        type="button"
                    >
                        Add an item
                    </button>
                    {/*
                      * Only with a focus: this asks a session to plan one, and there is
                      * nothing to ask without a sentence to plan from. Two presses, like
                      * deleting an account and for the same reason — it throws away rows
                      * somebody may have spent an evening ordering, and a native confirm()
                      * is suppressible. With nothing unfiled to throw away it is one press:
                      * a confirmation that guards nothing teaches the operator to click
                      * through the one that does not. What it says is what it does — rows
                      * that are already tickets stay, whichever way this goes.
                      */}
                    {config.focus &&
                        (!unfiled ? (
                            <button class="ghost" onClick={replan} type="button">
                                Ask again
                            </button>
                        ) : replanning ? (
                            <>
                                <button class="danger" onClick={replan} type="button">
                                    Discard the {unfiled} unfiled and ask again
                                </button>
                                <button class="ghost" onClick={() => setReplanning(false)} type="button">
                                    Keep them
                                </button>
                            </>
                        ) : (
                            <button class="ghost" onClick={() => setReplanning(true)} type="button">
                                Ask for a new plan
                            </button>
                        ))}
                    <span style="flex:1" />
                    {config.focusPlan.length > 0 && (
                        <span class="hint">
                            {filed} of {config.focusPlan.length} filed
                        </span>
                    )}
                </div>
            </div>

            <div class="card">
                <h2>Pace</h2>
                <label class="field">
                    <span>
                        Feature tickets per day: <strong>{config.ticketsPerDay}</strong>
                    </span>
                    <input
                        max={24}
                        min={0}
                        onChange={sliderCommit('ticketsPerDay')}
                        onInput={sliderTrack('ticketsPerDay')}
                        type="range"
                        value={config.ticketsPerDay}
                    />
                </label>
                <label class="field">
                    <span>
                        QA passes per day: <strong>{config.qaPerDay}</strong>
                    </span>
                    <input
                        max={24}
                        min={0}
                        onChange={sliderCommit('qaPerDay')}
                        onInput={sliderTrack('qaPerDay')}
                        type="range"
                        value={config.qaPerDay}
                    />
                </label>
                <p class="hint">
                    Features come from the plan above, or from the frontier in the
                    repository's direction document when no focus is set; a QA pass walks one
                    functionality the way a player would and files what is missing. Zero
                    stops that half without touching work already in flight.
                </p>
            </div>

            <div class="card">
                <h2>Authority</h2>
                <label class="check">
                    <input
                        checked={config.enabled}
                        onChange={(e) => {
                            void saveAutopilot({enabled: (e.target as HTMLInputElement).checked})
                        }}
                        type="checkbox"
                    />
                    <span>File new tickets at all</span>
                </label>
                <label class="check">
                    <input
                        checked={config.ship}
                        onChange={(e) => {
                            void saveAutopilot({ship: (e.target as HTMLInputElement).checked})
                        }}
                        type="checkbox"
                    />
                    <span>Merge and deploy finished tickets (off leaves them for review)</span>
                </label>
                <label class="field">
                    <span>
                        Retrospective every <strong>{config.retroEveryDays || '—'}</strong>{' '}
                        day(s), 0 for never
                    </span>
                    <input
                        max={30}
                        min={0}
                        onChange={(e) => {
                            void saveAutopilot({
                                retroEveryDays: Number((e.target as HTMLInputElement).value),
                            })
                        }}
                        style="max-width: 6rem"
                        type="number"
                        value={config.retroEveryDays}
                    />
                </label>
            </div>
        </>
    )
}

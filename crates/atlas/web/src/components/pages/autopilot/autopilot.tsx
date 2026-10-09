import m, {MithrilComponent} from '@garage44/mithril'
import {
    Badge,
    Button,
    Card,
    CloseButton,
    FieldNumber,
    FieldSlider,
    FieldSwitch,
    FieldTextarea,
} from '@garage44/mithril/components'
import {integer, max, min, validation} from '@garage44/mithril/validation'

import type {AutopilotPlanItem} from '@/types'

import {$s} from '@/app'
import {loadAutopilot, saveAutopilot} from '@/lib/autopilot'
import {local} from '@/lib/reactive'

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

/**
 * A numeric setting as a field model: it reads the store and tracks it while a
 * slider is dragged, so the figure beside the track follows the thumb. Nothing is
 * sent from here — see `saveOnRelease`.
 */
const tracked = (field: 'qaPerDay' | 'ticketsPerDay') => ({
    get value(): number {
        return $s.autopilot?.[field] ?? 0
    },
    set value(next: number) {
        if ($s.autopilot) {
            $s.autopilot[field] = next
        }
    },
})

/*
 * The slider writes its model on every `input`, which is right for the figure
 * and wrong for the file. A range input's native `change` fires once, on release,
 * and bubbles — so the wrapper around the field is what sends, exactly as the
 * hand-built slider's own `onchange` did.
 */
const saveOnRelease = (field: 'qaPerDay' | 'ticketsPerDay') => (e: Event) => {
    void saveAutopilot({[field]: Number((e.target as HTMLInputElement).value)})
}

/**
 * An on/off setting as a field model. A switch applies at once: the write goes
 * straight to the server, and the store takes the server's answer, not the click.
 */
const switched = (field: 'enabled' | 'ship') => ({
    get value(): boolean {
        return $s.autopilot?.[field] ?? false
    },
    set value(next: boolean) {
        void saveAutopilot({[field]: next})
    },
})

export class Autopilot extends MithrilComponent {
    /*
     * `retro` is what the number box holds while it is being typed into, and
     * `undefined` when it shows the file's value. It has to be separate from the
     * store: an emptied box is `null`, which the store cannot hold, and a box that
     * refilled itself from the store the moment it was cleared could not be
     * retyped.
     */
    s = local({replanning: false, retro: undefined as number | null | undefined})

    /** The number box's model: the draft while there is one, the file's value otherwise. */
    private retroModel = {
        get: () => (this.s.retro !== undefined ? this.s.retro : ($s.autopilot?.retroEveryDays ?? 0)),
        set: (next: number | null) => {
            this.s.retro = next
        },
        get value(): number | null {
            return this.get()
        },
        set value(next: number | null) {
            this.set(next)
        },
    }

    /*
     * The server clamps `retroEveryDays` to 0…30 (`clamp_int` in atlas's
     * `autopilot.rs`), so the same bounds here say so before a write rather than
     * after one comes back changed. Required, because an empty box has no value
     * to send.
     */
    private vRetro = validation(this.retroModel)
        .required()
        .rule(integer)
        .rule(min(0))
        .rule(max(30))

    /*
     * Again after the layout has settled, and again on a rotate. `grow` measures
     * the field as it stands when the row first renders, and a width that changes
     * afterwards changes how many lines the same text takes — measured at one
     * width and painted at another, a three-line note is a two-line box with its
     * last line cut in half. No dependency array on purpose: every render of this
     * list is a chance for a row's text to have changed — so it runs from both
     * `oncreate` and `onupdate`, and the resize listener lives as long as the page.
     */
    private growAll = () => {
        document.querySelectorAll<HTMLTextAreaElement>('.autopilot-plan-body textarea').forEach(grow)
    }

    oninit() {
        void loadAutopilot()
    }

    oncreate() {
        this.growAll()
        window.addEventListener('resize', this.growAll)
    }

    onupdate() {
        this.growAll()
    }

    onremove() {
        window.removeEventListener('resize', this.growAll)
    }

    view() {
        const config = $s.autopilot
        if (!config) {
            return (
                <Card>
                    <p class="autopilot-muted">
                        The autopilot is not answering. It runs beside the pipeline on the
                        production machine, steered through a config file there — if this page
                        cannot reach it, the lane itself is untouched and keeps whatever
                        settings it had.
                    </p>
                </Card>
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
            this.s.replanning = false
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

        // Tracks the store as it is typed; the wrapper's `focusout` is what saves.
        const focus = {
            get value(): string {
                return $s.autopilot?.focus ?? ''
            },
            set value(next: string) {
                if ($s.autopilot) {
                    $s.autopilot.focus = next
                }
            },
        }

        return (
            <>
                <p class="autopilot-intro">
                    The unattended development lane: it files tickets, builds them, merges what
                    passes CI and deploys. These knobs are read on its next pass — nothing
                    restarts.
                </p>

                <Card title="Focus">
                    {/*
                      * Saved on blur, not per keystroke. `blur` does not bubble out of
                      * the field component, `focusout` does — same moment, same value.
                      */}
                    <div
                        onfocusout={() => {
                            void saveAutopilot({focus: $s.autopilot?.focus ?? ''})
                        }}
                    >
                        <FieldTextarea
                            autosize
                            help={
                                'A sentence saying what the next stretch of work is for. The lane asks a ' +
                                'session to turn it into queued work below, and quotes it into every QA ' +
                                'pass and retrospective as well. Empty it and the queue keeps whatever is ' +
                                'already on it. Nothing here lifts a fence — a focus asking for a ' +
                                'wire-protocol change still files tickets that stop for review.'
                            }
                            label="What to work on"
                            model={focus}
                            placeholder="Focus on improving Cartopolis on the Steam Deck runner"
                            rows={3}
                        />
                    </div>

                    {!config.focus && (
                        <p class="autopilot-empty">
                            No focus. The lane works the frontier in the direction document, and
                            whatever is queued below.
                        </p>
                    )}
                </Card>

                <Card
                    actions={
                        config.focusPlan.length > 0 ? (
                            <Badge>
                                {filed} of {config.focusPlan.length} filed
                            </Badge>
                        ) : null
                    }
                    title="Queue"
                >
                    <p class="autopilot-muted">
                        What gets built, in the order it is in, ahead of the direction document.
                        Rows come from three places — the session that plans a focus, a QA pass
                        that walked into a gap, a retrospective that found something broken —
                        and none of them files a ticket. That happens here: the lane takes the
                        top row nobody has started yet, and from then on the row is locked and
                        links to its ticket. Tap any other row to reword it.
                    </p>

                    {!config.focusPlan.length && (
                        <p class="autopilot-empty">
                            {config.focus
                                ? 'Nothing queued yet. The lane files one planning ticket for this focus, and the rows appear here when that session answers — usually within the hour. You can also write them yourself, one at a time.'
                                : 'Nothing queued. QA passes and retrospectives add rows here as they find work, or write one yourself.'}
                        </p>
                    )}

                    {config.focusPlan.length > 0 && (
                        <ul class="autopilot-plan">
                            {config.focusPlan.map((item, index) => (
                                <li class={item.issue ? 'is-filed' : ''} key={item.id || `new-${index}`}>
                                    {/*
                                      * The buttons are a column beside the row rather than a
                                      * drag handle: this list is read and reordered on a phone
                                      * as often as on a desk, and a drag inside a scrolling
                                      * page fights the scroll.
                                      */}
                                    <div class="autopilot-plan-move">
                                        <Button
                                            aria-label="Move up"
                                            disabled={item.issue > 0 || index === 0 || config.focusPlan[index - 1].issue > 0}
                                            onclick={() => move(index, -1)}
                                            size="sm"
                                            title={item.issue > 0 ? 'Filed — the ticket is the record now' : 'Move up'}
                                            variant="ghost"
                                        >
                                            ↑
                                        </Button>
                                        <Button
                                            aria-label="Move down"
                                            disabled={
                                                item.issue > 0 ||
                                                index === config.focusPlan.length - 1 ||
                                                config.focusPlan[index + 1].issue > 0
                                            }
                                            onclick={() => move(index, 1)}
                                            size="sm"
                                            title={item.issue > 0 ? 'Filed — the ticket is the record now' : 'Move down'}
                                            variant="ghost"
                                        >
                                            ↓
                                        </Button>
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
                                        <div class="autopilot-plan-body is-filed">
                                            <span class="autopilot-plan-title">{item.title}</span>
                                            {item.note && <span class="autopilot-plan-note">{item.note}</span>}
                                        </div>
                                    ) : (
                                        <div class="autopilot-plan-body">
                                            <textarea
                                                aria-label="What changes"
                                                onblur={() => commitPlan(plan())}
                                                oninput={(e: Event) => {
                                                    grow(e.currentTarget as HTMLTextAreaElement)
                                                    edit(index, 'title', (e.target as HTMLTextAreaElement).value)
                                                }}
                                                placeholder="What changes"
                                                oncreate={(v: m.VnodeDOM) => grow(v.dom as HTMLTextAreaElement)}
                                                rows={1}
                                                value={item.title}
                                            />
                                            <textarea
                                                aria-label="Where it lands, and how it would be judged"
                                                class="autopilot-plan-note-field"
                                                onblur={() => commitPlan(plan())}
                                                oninput={(e: Event) => {
                                                    grow(e.currentTarget as HTMLTextAreaElement)
                                                    edit(index, 'note', (e.target as HTMLTextAreaElement).value)
                                                }}
                                                placeholder="Where it lands, and how it would be judged"
                                                oncreate={(v: m.VnodeDOM) => grow(v.dom as HTMLTextAreaElement)}
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
                                    <div class="autopilot-plan-side">
                                        {item.issue > 0 ? (
                                            <a
                                                class="autopilot-ticket"
                                                href={`${FORGE_ISSUES}/${item.issue}`}
                                                rel="noreferrer"
                                                target="_blank"
                                                title="Open the ticket"
                                            >
                                                <Badge tone="info">#{item.issue}</Badge>
                                            </a>
                                        ) : (
                                            <CloseButton
                                                label="Remove"
                                                onclick={() => {
                                                    const items = plan()
                                                    items.splice(index, 1)
                                                    commitPlan(items)
                                                }}
                                            />
                                        )}
                                    </div>
                                </li>
                            ))}
                        </ul>
                    )}

                    <div class="autopilot-actions">
                        {/*
                          * Tracked, not saved. The server drops an untitled row — a title is
                          * what becomes a ticket over there — so saving here would answer
                          * with a list the new row is not in and take it away again the
                          * instant it appeared. It is written to the file by the blur that
                          * leaves the title field, like every other edit on this page.
                          */}
                        <Button
                            onclick={() => {
                                if ($s.autopilot) {
                                    $s.autopilot.focusPlan = [...plan(), {id: '', issue: 0, note: '', title: ''}]
                                }
                            }}
                        >
                            Add an item
                        </Button>
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
                                <Button onclick={replan} variant="ghost">
                                    Ask again
                                </Button>
                            ) : this.s.replanning ? (
                                <>
                                    <Button onclick={replan} variant="danger">
                                        Discard the {unfiled} unfiled and ask again
                                    </Button>
                                    <Button
                                        onclick={() => {
                                            this.s.replanning = false
                                        }}
                                        variant="ghost"
                                    >
                                        Keep them
                                    </Button>
                                </>
                            ) : (
                                <Button
                                    onclick={() => {
                                        this.s.replanning = true
                                    }}
                                    variant="ghost"
                                >
                                    Ask for a new plan
                                </Button>
                            ))}
                    </div>
                </Card>

                <Card title="Pace">
                    <div class="autopilot-form">
                        <div onchange={saveOnRelease('ticketsPerDay')}>
                            <FieldSlider label="Feature tickets per day" max={24} min={0} model={tracked('ticketsPerDay')} />
                        </div>
                        <div onchange={saveOnRelease('qaPerDay')}>
                            <FieldSlider label="QA passes per day" max={24} min={0} model={tracked('qaPerDay')} />
                        </div>
                        <p class="autopilot-muted">
                            Features come from the plan above, or from the frontier in the
                            repository's direction document when no focus is set; a QA pass walks one
                            functionality the way a player would and files what is missing. Zero
                            stops that half without touching work already in flight.
                        </p>
                    </div>
                </Card>

                <Card title="Authority">
                    <div class="autopilot-form">
                        <FieldSwitch label="File new tickets at all" model={switched('enabled')} />
                        <FieldSwitch
                            label="Merge and deploy finished tickets (off leaves them for review)"
                            model={switched('ship')}
                        />
                        {/*
                          * Sent as each valid value lands — a one- or two-digit number, so a
                          * write per keystroke is a write or two, and the steppers have no
                          * blur to wait for. An invalid box sends nothing; the field says why.
                          */}
                        {/*
                          * Leaving the box drops the draft, so it shows the file again — the
                          * clamped value the server took, or the old one if what was typed
                          * was never valid enough to send.
                          */}
                        <div
                            onfocusout={() => {
                                this.s.retro = undefined
                            }}
                        >
                            <FieldNumber
                                class="autopilot-retro"
                                help="0 for never"
                                label="Retrospective every"
                                max={30}
                                min={0}
                                model={this.retroModel}
                                onchange={(next) => {
                                    if (next !== null && this.vRetro.valid && next !== config.retroEveryDays) {
                                        void saveAutopilot({retroEveryDays: next})
                                    }
                                }}
                                unit="days"
                                validation={this.vRetro}
                            />
                        </div>
                    </div>
                </Card>
            </>
        )
    }
}

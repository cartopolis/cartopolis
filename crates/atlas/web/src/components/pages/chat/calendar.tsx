import m from '@garage44/mithril'
import {Button, Dialog, FieldSegmented, FieldSwitch, FieldText} from '@garage44/mithril/components'

import type {ChatClient} from '@/lib/chat'
import {
    type Entry,
    type FoundPlace,
    type MeetingEntry,
    type MeetupEntry,
    type NominatimResult,
    type Settings,
    type Source,
    addDays,
    clock,
    dayStart,
    entries,
    distinctPlaces,
    goingJson,
    loadSettings,
    longDate,
    meetupJson,
    monthStart,
    ruleWords,
    saveSettings,
    weekStart,
} from '@/lib/calendar'
import {type AnswerFor, type Plan, type Rsvp, RSVP_WORDS, isSkipped} from '@/lib/meeting'
import {Icon} from '@/lib/icons'
import {NOMINATIM} from '@/lib/stats'

/**
 * The calendar page (`docs/mockups/calendar/v1.html` section 2, and
 * `v2-repeat.html` for series): every meeting and meet-up this person is in,
 * from every chat, as a list by day, a week of hours or a month, with the one
 * picked open on the right.
 *
 * Nothing in it is new on the wire: it reads the histories the chat worker
 * keeps in this browser, and what it sends — an answer, "I'm going", a skip —
 * is exactly what the chat's own cards send. So it shows only what this
 * browser has: a chat whose history is on another device has no plans here.
 */

export type View = 'list' | 'week' | 'month'

/** What the page lends the calendar: the chat, faces and names, and its ways out. */
export interface CalendarCtx {
    client: ChatClient
    avatar: (address: string, size: number) => m.Children
    /** How a person is named: their bare name on this server. */
    shown: (address: string) => string
    /** Where a plan came from: "oosterparkwijk · # general", "Sanne". */
    where: (channel: string) => string
    /** Everybody in a chat, for "No answer". */
    people: (channel: string) => string[]
    /** The chats, as calendar sources. */
    sources: () => Source[]
    /** The communities, for "only the plans I answered". */
    communities: () => {id: string; name: string}[]
    openChat: (channel: string) => void
    /** Back to the list of communities, on a phone. */
    back: () => void
    join: (channel: string) => void
    /** Open a chat's plan dialog on a day (a local midnight), at a time if one
     *  was clicked in the week. */
    plan: (channel: string, day: number, at: number | null) => void
}

/** A meet-up being put together from the web: which chat, where, when. */
interface MeetupDraft {
    channel: string
    query: string
    found: FoundPlace[]
    place: FoundPlace | null
    /** "20:00", on [`st.day`]. */
    time: string
    note: string
    searching: boolean
    error: string | null
}

/** Everything the calendar page holds. */
const st = {
    view: 'week' as View,
    /** The day the shown period is about, a local midnight. */
    anchor: dayStart(Date.now()),
    /** The day picked: where Plan plans, and which day a month lists. */
    day: dayStart(Date.now()),
    /** The entry open on the right, by key. */
    selected: null as string | null,
    /** Whether an answer in the panel is for every date of the series. */
    every: false,
    planning: false,
    /** What Plan makes: a meeting, or a meet-up (a place and a time). */
    kind: 'meeting' as 'meeting' | 'meetup',
    /** The time clicked in the week, for Plan to start at. */
    planAt: null as number | null,
    /** The meet-up form, once a chat is picked for one. */
    meetup: null as MeetupDraft | null,
    settings: loadSettings() as Settings,
}

/** Open Plan on the picked day, at `at` if a time was clicked. */
function startPlanning(at: number | null): void {
    st.planAt = at
    st.kind = 'meeting'
    st.meetup = null
    st.planning = true
}

/** "20:00" for an instant, or for the next whole hour when there is none. */
function timeOf(at: number | null): string {
    const t = new Date(at ?? Date.now())
    if (at === null) {
        t.setMinutes(0, 0, 0)
        t.setHours(t.getHours() + 1)
    }
    return `${String(t.getHours()).padStart(2, '0')}:${String(t.getMinutes()).padStart(2, '0')}`
}

let searchTimer: ReturnType<typeof setTimeout> | null = null

/** Look the typed place up on our own address server, a moment after typing stops. */
function searchPlace(d: MeetupDraft): void {
    if (searchTimer) clearTimeout(searchTimer)
    const q = d.query.trim()
    if (q.length < 3) {
        d.found = []
        return
    }
    searchTimer = setTimeout(async () => {
        d.searching = true
        d.error = null
        m.redraw()
        try {
            // Biased towards the Netherlands, where the map has its detail,
            // without ruling anywhere else out.
            const r = await fetch(
                `${NOMINATIM}/search?format=jsonv2&addressdetails=1&limit=10&viewbox=3.2,53.7,7.3,50.7&q=${encodeURIComponent(q)}`,
            )
            const list = r.ok ? ((await r.json()) as NominatimResult[]) : []
            if (d.query.trim() === q) d.found = distinctPlaces(list).slice(0, 6)
            if (!r.ok) d.error = 'The address search did not answer. Try again in a moment.'
        } catch {
            d.error = 'The address search could not be reached.'
        }
        d.searching = false
        m.redraw()
    }, 350)
}

/** Open on today: a week of hours, or the list on a phone, which has no room for hours. */
export function openCalendar(): void {
    if (matchMedia('(max-width: 760px)').matches) st.view = 'list'
    st.anchor = dayStart(Date.now())
    st.day = st.anchor
}

const HOUR_PX = 44
const DAY_MS = 86_400_000
/** How far back the calendar looks: plans more than a month gone are left out. */
const PAST_MS = 30 * DAY_MS

/** The first and last hours the week grid shows: 9 to 21, stretched to what is in it. */
function hourRange(list: Entry[]): [number, number] {
    let from = 9
    let to = 21
    for (const e of list) {
        const start = new Date(e.startMs)
        from = Math.min(from, start.getHours())
        const end = new Date(Math.min(e.endMs, addDays(dayStart(e.startMs), 1)))
        const endH = end.getTime() === addDays(dayStart(e.startMs), 1) ? 24 : end.getHours() + (end.getMinutes() > 0 ? 1 : 0)
        to = Math.max(to, endH)
    }
    return [from, Math.min(24, to)]
}

/** The words for this person's answer on a row: their answer, "Skipped", or "Answer". */
function status(e: Entry, me: string): {cls: string; text: string} {
    if (e.kind === 'meeting' && e.skipped) return {cls: 'is-skipped', text: 'Skipped'}
    if (e.mine) return {cls: `is-${e.mine}`, text: e.kind === 'meetup' ? 'Going' : RSVP_WORDS[e.mine]}
    if (own(e, me)) return {cls: 'is-going', text: 'Yours'}
    return {cls: 'is-open', text: 'Answer'}
}

const own = (e: Entry, me: string) => (e.kind === 'meeting' ? e.planner === me : e.meetup.proposer === me)

/** The classes an entry is drawn with: its kind, and how it was answered. */
function entryClass(e: Entry, me: string, base: string): string {
    return [
        base,
        e.kind === 'meetup' ? 'is-meetup' : 'is-meeting',
        e.mine === null && !own(e, me) && !(e.kind === 'meeting' && e.skipped) ? 'is-unanswered' : '',
        e.mine === 'no' ? 'is-declined' : '',
        e.kind === 'meeting' && e.skipped ? 'is-skipped' : '',
        st.selected === e.key ? 'is-selected' : '',
    ].join(' ')
}

function dayWords(ms: number): string {
    const today = dayStart(Date.now())
    if (ms === today) return 'Today'
    if (ms === addDays(today, 1)) return 'Tomorrow'
    return longDate(ms)
}

function span(fromMs: number, toMs: number): string {
    const a = new Date(fromMs)
    const b = new Date(toMs - 1)
    const year = b.getFullYear()
    if (a.getMonth() === b.getMonth()) return `${a.getDate()} – ${b.getDate()} ${b.toLocaleDateString([], {month: 'long'})} ${year}`
    const day = (d: Date) => d.toLocaleDateString([], {day: 'numeric', month: 'long'})
    return `${day(a)} – ${day(b)} ${year}`
}

/** The period the view shows: [from, to), a title, and the step back and on. */
function period(): {from: number; step: (n: number) => number; title: string; to: number} {
    if (st.view === 'week') {
        const from = weekStart(st.anchor)
        const to = addDays(from, 7)
        return {from, step: (n) => addDays(st.anchor, 7 * n), title: span(from, to), to}
    }
    if (st.view === 'month') {
        const first = monthStart(st.anchor)
        const from = weekStart(first)
        const to = addDays(from, 42)
        const step = (n: number) => {
            const d = new Date(first)
            d.setMonth(d.getMonth() + n)
            return d.getTime()
        }
        return {from, step, title: new Date(first).toLocaleDateString([], {month: 'long', year: 'numeric'}), to}
    }
    const from = st.anchor
    const to = addDays(from, 28)
    const title = from === dayStart(Date.now()) ? 'From today' : `From ${longDate(from)}`
    return {from, step: (n) => addDays(st.anchor, 28 * n), title, to}
}

/** How many plans are today, for the calendar's row on the side. */
export function todayCount(ctx: Pick<CalendarCtx, 'client' | 'sources'>): number {
    const me = ctx.client.address
    if (!me) return 0
    const today = dayStart(Date.now())
    return entries(ctx.sources(), me, today, addDays(today, 1), st.settings)
        .filter((e) => !(e.kind === 'meeting' && e.skipped) && e.mine !== 'no').length
}

export function Calendar(ctx: CalendarCtx): m.Children {
    const me = ctx.client.address ?? ''
    const p = period()
    const from = Math.max(p.from, Date.now() - PAST_MS)
    const list = entries(ctx.sources(), me, from, p.to, st.settings)
    const picked = st.selected ? list.find((e) => e.key === st.selected) ?? null : null
    return (
        <div class={picked ? 'cal has-detail' : 'cal'}>
            <div class="cal-main">
                <header class="cal-head">
                    <Button class="chat-back" icon="‹" iconOnly onclick={ctx.back} title="Back" variant="ghost" />
                    <h1 class="cal-title">{p.title}</h1>
                    <span class="cal-nav">
                        <button aria-label="Before" class="cal-icon-btn" onclick={() => (st.anchor = p.step(-1))} title="Before" type="button">
                            {Icon('chevron_left')}
                        </button>
                        <button aria-label="After" class="cal-icon-btn" onclick={() => (st.anchor = p.step(1))} title="After" type="button">
                            {Icon('chevron_right')}
                        </button>
                    </span>
                    <Button
                        onclick={() => {
                            st.anchor = dayStart(Date.now())
                            st.day = st.anchor
                        }}
                        size="sm"
                        variant="ghost"
                    >
                        Today
                    </Button>
                    <span class="cal-spacer" />
                    <FieldSegmented
                        model={{
                            get value() {
                                return st.view
                            },
                            set value(v: View) {
                                st.view = v
                            },
                        }}
                        options={[
                            {label: 'List', value: 'list'},
                            {label: 'Week', value: 'week'},
                            {label: 'Month', value: 'month'},
                        ]}
                        size="sm"
                    />
                    {settingsMenu(ctx)}
                    <Button onclick={() => startPlanning(null)} size="sm" variant="primary">
                        {Icon('add')} Plan
                    </Button>
                </header>
                {st.view === 'week' && weekView(ctx, list, p.from, me)}
                {st.view === 'month' && monthView(ctx, list, p.from, me)}
                {st.view === 'list' && listView(ctx, list, p.from, p.to, me)}
                <p class="cal-foot">
                    From the chats on this browser. A plan in a chat whose history is only on another device is not here.
                </p>
            </div>
            {picked && <aside class="cal-detail">{detail(ctx, picked, me)}</aside>}
            {planDialog(ctx)}
        </div>
    )
}

function pick(e: Entry): void {
    st.selected = st.selected === e.key ? null : e.key
    st.every = false
}

/** One plan in the week grid or a list. */
function block(ctx: CalendarCtx, e: Entry, me: string, style: string | null = null) {
    const s = status(e, me)
    return (
        <button class={entryClass(e, me, 'cal-block')} key={e.key} onclick={() => pick(e)} style={style ?? undefined} type="button">
            <b>
                {clock(e.startMs)} {e.title}
            </b>
            {/* "Skipped" first: a narrow column cuts the end off. */}
            <small>{e.kind === 'meeting' && e.skipped ? `Skipped · ${ctx.where(e.channel)}` : `${ctx.where(e.channel)} · ${s.text}`}</small>
        </button>
    )
}

function weekView(ctx: CalendarCtx, list: Entry[], from: number, me: string) {
    const days = Array.from({length: 7}, (_, i) => addDays(from, i))
    const [h0, h1] = hourRange(list)
    const hours = h1 - h0
    /** Where an hour of the day is, down the grid. */
    const pct = (hour: number) => ((hour - h0) / hours) * 100
    const today = dayStart(Date.now())
    const now = Date.now()
    return (
        <div class="cal-week">
            <div class="cal-week-head">
                <span class="cal-gutter" />
                {days.map((d) => (
                    <button
                        class={['cal-colh', d === today ? 'is-today' : '', d === st.day ? 'is-day' : ''].join(' ')}
                        key={d}
                        onclick={() => (st.day = d)}
                        type="button"
                    >
                        {new Date(d).toLocaleDateString([], {weekday: 'short'})}
                        <b>{new Date(d).getDate()}</b>
                    </button>
                ))}
            </div>
            {/* At least HOUR_PX an hour, and stretched to fill the pane when the hours fit. */}
            <div class="cal-week-body" style={`grid-auto-rows:minmax(${hours * HOUR_PX}px, 1fr);--cal-hour:calc(100% / ${hours})`}>
                <div class="cal-hours">
                    {Array.from({length: hours}, (_, i) => (
                        <span style={`top:${pct(h0 + i)}%`}>
                            {new Date(2000, 0, 1, h0 + i).toLocaleTimeString([], {hour: 'numeric'})}
                        </span>
                    ))}
                </div>
                {days.map((d) => {
                    const end = addDays(d, 1)
                    const inDay = list.filter((e) => e.startMs >= d && e.startMs < end)
                    // Side by side where they overlap: each takes the first free lane.
                    const lanes: number[] = []
                    const lane = new Map<string, number>()
                    for (const e of inDay) {
                        let i = lanes.findIndex((until) => until <= e.startMs)
                        if (i < 0) i = lanes.push(0) - 1
                        lanes[i] = e.endMs
                        lane.set(e.key, i)
                    }
                    const width = 100 / Math.max(1, lanes.length)
                    // By the clock rather than by milliseconds since midnight, so a
                    // day with a change to summer time still has 20:00 at 20:00.
                    const hourOf = (ms: number) => {
                        if (ms >= end) return 24
                        const t = new Date(ms)
                        return t.getHours() + t.getMinutes() / 60
                    }
                    // A click on an empty part of the day plans something at
                    // that time, to the half hour; one on a plan opens it.
                    const planHere = (ev: MouseEvent) => {
                        if ((ev.target as HTMLElement).closest('.cal-block')) return
                        const r = (ev.currentTarget as HTMLElement).getBoundingClientRect()
                        const hour = Math.floor((h0 + ((ev.clientY - r.top) / r.height) * hours) * 2) / 2
                        const at = new Date(d)
                        at.setHours(Math.floor(hour), (hour % 1) * 60, 0, 0)
                        if (at.getTime() < Date.now()) return
                        st.day = d
                        startPlanning(at.getTime())
                    }
                    return (
                        <div
                            class={d === today ? 'cal-col is-today' : 'cal-col'}
                            key={d}
                            onclick={planHere}
                            title={d + DAY_MS > now ? 'Click to plan something at this time' : undefined}
                        >
                            {inDay.map((e) => {
                                const y = pct(hourOf(e.startMs))
                                const h = pct(hourOf(e.endMs)) - y
                                const x = lane.get(e.key)! * width
                                return block(ctx, e, me, `top:${y}%;height:max(${h}%, 38px);left:calc(${x}% + 2px);width:calc(${width}% - 4px)`)
                            })}
                            {d === today && now < end && <i class="cal-now" style={`top:${pct(hourOf(now))}%`} />}
                        </div>
                    )
                })}
            </div>
        </div>
    )
}

function monthView(ctx: CalendarCtx, list: Entry[], from: number, me: string) {
    const month = new Date(monthStart(st.anchor)).getMonth()
    const today = dayStart(Date.now())
    const cells = Array.from({length: 42}, (_, i) => addDays(from, i))
    const day = st.day
    const onDay = list.filter((e) => dayStart(e.startMs) === day)
    return (
        <div class="cal-month-wrap">
            <div class="cal-month">
                {cells.slice(0, 7).map((d) => (
                    <span class="cal-dow">{new Date(d).toLocaleDateString([], {weekday: 'short'})}</span>
                ))}
                {cells.map((d) => {
                    const has = list.filter((e) => dayStart(e.startMs) === d && !(e.kind === 'meeting' && e.skipped))
                    return (
                        <button
                            class={[
                                'cal-cell',
                                new Date(d).getMonth() === month ? '' : 'is-other',
                                d === today ? 'is-today' : '',
                                d === day ? 'is-day' : '',
                                d < today ? 'is-past' : '',
                            ].join(' ')}
                            onclick={() => (st.day = d)}
                            type="button"
                        >
                            <b>{new Date(d).getDate()}</b>
                            <span class="cal-dots">
                                {has.some((e) => e.kind === 'meetup') && <i class="is-meetup" />}
                                {has.some((e) => e.kind === 'meeting') && <i class="is-meeting" />}
                            </span>
                        </button>
                    )
                })}
            </div>
            <section class="cal-day">
                <h3 class="cal-day-h">
                    {dayWords(day)}
                    {day >= today && (
                        <Button onclick={() => startPlanning(null)} size="sm" variant="ghost">
                            {Icon('add')} Plan on this day
                        </Button>
                    )}
                </h3>
                {onDay.length === 0 ? <p class="chat-empty">Nothing planned.</p> : onDay.map((e) => row(ctx, e, me))}
            </section>
        </div>
    )
}

/** One plan in the list: its time, an edge in its kind's colour, what and where, and the answer. */
function row(ctx: CalendarCtx, e: Entry, me: string) {
    const s = status(e, me)
    const mins = Math.round((e.endMs - e.startMs) / 60000)
    return (
        <button class={entryClass(e, me, 'cal-row')} key={e.key} onclick={() => pick(e)} type="button">
            <span class="cal-row-time">
                {clock(e.startMs)}
                {e.kind === 'meeting' && <small>{mins % 60 === 0 ? `${mins / 60} h` : mins > 60 ? `${(mins / 60).toFixed(1)} h` : `${mins} min`}</small>}
            </span>
            <span class="cal-row-what">
                <b>{e.title}</b>
                <small>
                    {e.kind === 'meeting'
                        ? e.skipped
                            ? `Skipped by ${e.planner === me ? 'you' : ctx.shown(e.planner)}`
                            : <>{e.plan.repeat && <>{Icon('repeat')} {e.plan.repeat.every === 'week' ? 'weekly' : 'monthly'} · </>}{ctx.where(e.channel)}</>
                        : <>{Icon('location_on')} Meet-up · {ctx.where(e.channel)}</>}
                </small>
            </span>
            <span class={`cal-pill ${s.cls}`}>{s.text}</span>
        </button>
    )
}

function listView(ctx: CalendarCtx, list: Entry[], from: number, to: number, me: string) {
    const out: m.Children[] = []
    for (let d = from; d < to; d = addDays(d, 1)) {
        const onDay = list.filter((e) => dayStart(e.startMs) === d)
        if (!onDay.length) continue
        out.push(
            <section class="cal-day" key={d}>
                <h3>{dayWords(d)}</h3>
                {onDay.map((e) => row(ctx, e, me))}
            </section>,
        )
    }
    return (
        <div class="cal-list">
            {out.length ? out : <p class="chat-empty">Nothing planned in these four weeks. Plan a meeting in a chat, or with Plan above.</p>}
        </div>
    )
}

/** The plan picked, as the phone's sheet draws it (v1, D and E; v2, K). */
function detail(ctx: CalendarCtx, e: Entry, me: string) {
    return (
        <div class="cal-detail-body">
            <button aria-label="Close" class="cal-icon-btn cal-detail-close" onclick={() => (st.selected = null)} title="Close" type="button">
                {Icon('close')}
            </button>
            {e.kind === 'meeting' ? meetingDetail(ctx, e, me) : meetupDetail(ctx, e, me)}
        </div>
    )
}

function whenWords(startMs: number, endMs: number | null): string {
    const day = dayStart(startMs)
    const today = dayStart(Date.now())
    const date = day === today ? 'Today' : day === addDays(today, 1) ? 'Tomorrow' : longDate(startMs)
    return `${date}, ${clock(startMs)}${endMs === null ? '' : ` – ${clock(endMs)}`}`
}

const ORDINAL = (n: number) => `${n}${n % 100 >= 11 && n % 100 <= 13 ? 'th' : ['th', 'st', 'nd', 'rd'][n % 10] ?? 'th'}`

function meetingDetail(ctx: CalendarCtx, e: MeetingEntry, me: string) {
    const c = ctx.client
    const plan = e.plan
    const now = Date.now()
    const answerFor: AnswerFor = !plan.repeat ? null : st.every ? 'every' : e.startMs
    const answer = (r: Rsvp) => c.sendMeeting(e.channel, {date: answerFor, kind: 'answer', plan: plan.id, rsvp: r})
    // You first, then in the order they answered.
    const who = (r: Rsvp) => [...e.rsvps.entries()].filter(([, a]) => a === r).map(([w]) => w)
        .sort((a, b) => Number(b === me) - Number(a === me))
    const answered = new Set(e.rsvps.keys())
    const silent = ctx.people(e.channel).filter((p) => !answered.has(p))
    // A big community's "no answer" is most of it: the first few, and how many more.
    const names = (list: string[]) => {
        const named = list.slice(0, 6).map((p) => (p === me ? 'you' : ctx.shown(p))).join(', ')
        return list.length > 6 ? `${named}, +${list.length - 6}` : named
    }
    const joinable = !e.skipped && now >= e.startMs - 10 * 60000 && now <= e.endMs
    const over = now > e.endMs
    const mine = e.planner === me
    const lines: [string, string[]][] = [
        [`Going ${who('going').length}`, who('going')],
        [`Maybe ${who('maybe').length}`, who('maybe')],
        [`Can't come ${who('no').length}`, who('no')],
        [`No answer ${silent.length}`, silent],
    ]
    return (
        <>
            <div class="cal-kind">{Icon('videocam')} Meeting · a call</div>
            <h2 class={e.skipped ? 'cal-detail-title is-skipped' : 'cal-detail-title'}>{plan.title}</h2>
            <div class="cal-when">{whenWords(e.startMs, e.endMs)}</div>
            {plan.repeat && (
                <div class="cal-rule">
                    {Icon('repeat')} {ruleWords(plan, false)}
                    {e.count !== null ? ` · ${ORDINAL(e.index)} of ${e.count}` : ` · the ${ORDINAL(e.index)}`}
                </div>
            )}
            <div class="cal-from">
                {ctx.avatar(e.planner, 20)}
                <span>{ctx.where(e.channel)} · planned by {mine ? 'you' : ctx.shown(e.planner)}</span>
            </div>
            {e.skipped && <p class="cal-skipped">{mine ? 'You' : ctx.shown(e.planner)} took this date out of the series.</p>}
            {!e.skipped && !over && (
                <>
                    {plan.repeat && (
                        <FieldSegmented
                            model={{
                                get value() {
                                    return st.every ? 'every' : 'this'
                                },
                                set value(v: string) {
                                    st.every = v === 'every'
                                },
                            }}
                            options={[
                                {label: 'This date', value: 'this'},
                                {label: 'Every date', value: 'every'},
                            ]}
                            size="sm"
                        />
                    )}
                    <div class="cal-rsvp">
                        {(['going', 'maybe', 'no'] as Rsvp[]).map((r) => (
                            <button class={e.mine === r ? `cal-answer is-on is-${r}` : 'cal-answer'} onclick={() => answer(r)} type="button">
                                {RSVP_WORDS[r]}
                            </button>
                        ))}
                    </div>
                </>
            )}
            <div class="cal-who">
                {lines.map(([label, list]) => (
                    <div class="cal-who-line">
                        <span class="cal-who-label">{label}</span>
                        <span class="cal-who-faces">{list.slice(0, 5).map((p) => ctx.avatar(p, 20))}</span>
                        <span class="cal-who-names">{names(list)}</span>
                    </div>
                ))}
            </div>
            {plan.agenda.length > 0 && (
                <div class="cal-agenda">
                    <h3>Agenda</h3>
                    {plan.agenda.map((i) => (
                        <div class="cal-agenda-item">
                            <span>{i.name}</span>
                            {i.minutes > 0 && <small>{i.minutes} min</small>}
                        </div>
                    ))}
                </div>
            )}
            {plan.text && <p class="cal-text">{plan.text}</p>}
            <div class="cal-actions">
                <Button onclick={() => ctx.openChat(e.channel)} size="sm" variant="ghost">
                    {Icon('chat')} Open chat
                </Button>
                <Button disabled={!joinable} onclick={() => ctx.join(e.channel)} size="sm" variant={joinable ? 'primary' : 'ghost'}>
                    {Icon('videocam')} {joinable || over ? 'Join' : `Join · from ${clock(e.startMs - 10 * 60000)}`}
                </Button>
            </div>
            {mine && plan.repeat && !over && (
                <div class="cal-mine">
                    <h3>You planned this</h3>
                    <div class="cal-actions">
                        {!e.skipped && (
                            <Button onclick={() => skip(c, e)} size="sm" variant="ghost">
                                {Icon('block')} Skip this date
                            </Button>
                        )}
                        {e.skipped && (
                            <Button onclick={() => restore(c, e)} size="sm" variant="ghost">
                                {Icon('event')} Put it back
                            </Button>
                        )}
                        {plan.repeat.untilMs !== e.startMs && (
                            <Button onclick={() => stop(c, e)} size="sm" variant="ghost">
                                {Icon('stop')} Stop after this
                            </Button>
                        )}
                    </div>
                </div>
            )}
        </>
    )
}

/** The planner's changes: the plan again, as an update only theirs counts for. */
function update(c: ChatClient, channel: string, plan: Plan): void {
    c.sendMeeting(channel, {kind: 'update', plan})
}

function skip(c: ChatClient, e: MeetingEntry): void {
    if (isSkipped(e.plan, e.startMs)) return
    update(c, e.channel, {...e.plan, skip: [...e.plan.skip, e.startMs].sort((a, b) => a - b)})
}

function restore(c: ChatClient, e: MeetingEntry): void {
    update(c, e.channel, {...e.plan, skip: e.plan.skip.filter((d) => d !== e.startMs)})
}

function stop(c: ChatClient, e: MeetingEntry): void {
    if (!e.plan.repeat) return
    if (!confirm(`Stop ${e.plan.title} after ${longDate(e.startMs)}? Everybody in the chat sees it end.`)) return
    update(c, e.channel, {...e.plan, repeat: {...e.plan.repeat, untilMs: e.startMs}})
}

function meetupDetail(ctx: CalendarCtx, e: MeetupEntry, me: string) {
    const mu = e.meetup
    const going = e.mine === 'going'
    const over = Date.now() > e.endMs
    return (
        <>
            <div class="cal-kind is-meetup">{Icon('location_on')} Meet-up · a place and a time</div>
            <h2 class="cal-detail-title">{e.title}</h2>
            <div class="cal-when">{whenWords(e.startMs, null)}</div>
            <div class="cal-from">
                {ctx.avatar(mu.proposer, 20)}
                <span>{ctx.where(e.channel)} · proposed by {mu.proposer === me ? 'you' : ctx.shown(mu.proposer)}</span>
            </div>
            {mu.text && mu.text !== "I'm going" && <p class="cal-text">{mu.text}</p>}
            <div class="cal-who">
                <div class="cal-who-line">
                    <span class="cal-who-label">Going {mu.going.length}</span>
                    <span class="cal-who-faces">{mu.going.slice(0, 5).map((p) => ctx.avatar(p, 20))}</span>
                    <span class="cal-who-names">{mu.going.map((p) => (p === me ? 'you' : ctx.shown(p))).join(', ')}</span>
                </div>
            </div>
            <p class="chat-hint">
                The way there, and sharing where you are as you go, are in the app. Here you can say you are coming.
            </p>
            <div class="cal-actions">
                {!over && (
                    <Button
                        disabled={going}
                        onclick={() => ctx.client.sendPoint(e.channel, goingJson(mu), "I'm going")}
                        size="sm"
                        variant={going ? 'ghost' : 'primary'}
                    >
                        {Icon('check')} {going ? "You're going" : "I'm going"}
                    </Button>
                )}
                <Button onclick={() => ctx.openChat(e.channel)} size="sm" variant="ghost">
                    {Icon('chat')} Open chat
                </Button>
            </div>
        </>
    )
}

/** What shows, chosen on this browser: plans you can't come to, and big communities' noise. */
function settingsMenu(ctx: CalendarCtx) {
    const set = st.settings
    const save = () => saveSettings(set)
    const communities = ctx.communities()
    return (
        <details class="cal-settings">
            <summary aria-label="What the calendar shows" class="cal-icon-btn" title="What the calendar shows">
                {Icon('settings')}
            </summary>
            <div class="cal-settings-menu">
                <h3>Show</h3>
                <FieldSwitch
                    help="Faded, so you can still change your mind"
                    label="Plans I can't come to"
                    model={{
                        get value() {
                            return set.showDeclined
                        },
                        set value(v: boolean) {
                            set.showDeclined = v
                            save()
                        },
                    }}
                />
                {communities.length > 0 && <h3>Only the plans I answered</h3>}
                {communities.map((x) => (
                    <FieldSwitch
                        label={x.name}
                        model={{
                            get value() {
                                return set.answeredOnly.includes(x.id)
                            },
                            set value(v: boolean) {
                                set.answeredOnly = v ? [...set.answeredOnly, x.id] : set.answeredOnly.filter((id) => id !== x.id)
                                save()
                            },
                        }}
                    />
                ))}
                <p class="chat-hint">Kept on this browser only.</p>
            </div>
        </details>
    )
}

/**
 * Plan: what kind and in which chat (v1, G), then that chat's own plan dialog
 * on the day picked. A meet-up needs a place picked on a map, which is the
 * app's; so here it is a meeting.
 */
function planDialog(ctx: CalendarCtx) {
    const open = {
        get value() {
            return st.planning
        },
        set value(v: boolean) {
            st.planning = v
            if (!v) st.meetup = null
        },
    }
    const when = dayWords(st.day) === 'Today' || dayWords(st.day) === 'Tomorrow' ? dayWords(st.day).toLowerCase() : `on ${longDate(st.day)}`
    const at = st.planAt === null ? '' : ` at ${clock(st.planAt)}`
    return (
        <Dialog class="chat-dialog cal-plan" model={open} title={st.meetup ? `Meet up ${when}` : `Plan something ${when}${at}`}>
            {st.meetup ? meetupForm(ctx, st.meetup) : planPicker(ctx)}
        </Dialog>
    )
}

/** What to plan, and in which chat. */
function planPicker(ctx: CalendarCtx) {
    const chats = ctx.client.chats
    const kind = (k: 'meeting' | 'meetup', icon: string, title: string, sub: string) => (
        <button class={st.kind === k ? 'cal-plan-kind is-on' : 'cal-plan-kind'} onclick={() => (st.kind = k)} type="button">
            {Icon(icon)}
            <span>
                <b>{title}</b>
                <small>{sub}</small>
            </span>
            {st.kind === k && Icon('check')}
        </button>
    )
    return [
        <div class="cal-plan-kinds">
            {kind('meeting', 'videocam', 'Meeting', 'A call, with an agenda')}
            {kind('meetup', 'location_on', 'Meet up', 'A place and a time')}
        </div>,
        <h3 class="cal-plan-h">In which chat</h3>,
        <div class="cal-plan-chats">
            {chats.length === 0 && <p class="chat-empty">No chats yet.</p>}
            {chats.map((x) => (
                <button
                    class="cal-plan-chat"
                    key={x.channel}
                    onclick={() => {
                        if (st.kind === 'meeting') {
                            st.planning = false
                            ctx.plan(x.channel, st.day, st.planAt)
                            return
                        }
                        st.meetup = {
                            channel: x.channel,
                            error: null,
                            found: [],
                            note: '',
                            place: null,
                            query: '',
                            searching: false,
                            time: timeOf(st.planAt),
                        }
                    }}
                    type="button"
                >
                    {ctx.where(x.channel).includes(' # ')
                        ? <span class="cal-plan-hash">#</span>
                        : x.group ? <span class="cal-plan-hash">⧉</span> : ctx.avatar(x.people[0] ?? x.label, 24)}
                    <span>{ctx.where(x.channel)}</span>
                </button>
            ))}
        </div>,
    ]
}

/**
 * A meet-up from the web: the place found by name on our own address server
 * (there is no map on this page to point at), a time on the picked day, and an
 * optional line. It goes out as the same message the app's `+ → Meet up` sends.
 */
function meetupForm(ctx: CalendarCtx, d: MeetupDraft) {
    const [h, mm] = d.time.split(':').map(Number)
    const start = new Date(st.day)
    start.setHours(h || 0, mm || 0, 0, 0)
    const past = start.getTime() < Date.now()
    const ready = d.place !== null && !past
    const model = <K extends 'query' | 'note'>(key: K, then?: () => void) => ({
        get value() {
            return d[key]
        },
        set value(v: string) {
            d[key] = v
            then?.()
        },
    })
    return [
        <p class="cal-plan-in">In {ctx.where(d.channel)}</p>,
        d.place ? (
            <div class="cal-place is-picked">
                {Icon('location_on')}
                <span>
                    <b>{d.place.name}</b>
                    <small>{d.place.area}</small>
                </span>
                <Button onclick={() => (d.place = null)} size="sm" variant="ghost">
                    Change
                </Button>
            </div>
        ) : (
            <div class="cal-place-search">
                <FieldText
                    label="Where"
                    model={model('query', () => searchPlace(d))}
                    placeholder="A square, a café, an address: Vismarkt, Groningen"
                />
                {d.searching && <p class="chat-empty">Looking…</p>}
                {d.error && <p class="chat-empty">{d.error}</p>}
                {!d.searching && d.query.trim().length >= 3 && d.found.length === 0 && !d.error && (
                    <p class="chat-empty">Nothing found by that name.</p>
                )}
                <div class="cal-place-list">
                    {d.found.map((p) => (
                        <button class="cal-place" key={`${p.latitude},${p.longitude}`} onclick={() => (d.place = p)} type="button">
                            {Icon('location_on')}
                            <span>
                                <b>{p.name}</b>
                                <small>{p.area}</small>
                            </span>
                        </button>
                    ))}
                </div>
            </div>
        ),
        <label class="cal-time">
            <span>When, {dayWords(st.day).toLowerCase() === 'today' ? 'today' : longDate(st.day)}</span>
            <input oninput={(e: Event) => (d.time = (e.target as HTMLInputElement).value)} type="time" value={d.time} />
        </label>,
        past && <p class="chat-empty">That time has passed.</p>,
        <FieldText label="A line with it (optional)" model={model('note')} placeholder="Coffee first?" />,
        <footer class="cal-plan-foot">
            <Button onclick={() => (st.meetup = null)} variant="ghost">
                Back
            </Button>
            <Button
                disabled={!ready}
                onclick={() => {
                    if (!d.place) return
                    const json = meetupJson(d.place, start.getTime(), d.note)
                    ctx.client.sendPoint(d.channel, json, d.note.trim() || d.place.name)
                    st.meetup = null
                    st.planning = false
                }}
                variant="primary"
            >
                {Icon('send')} Propose the meet-up
            </Button>
        </footer>,
    ]
}

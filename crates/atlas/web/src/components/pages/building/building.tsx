import m, {MithrilComponent} from '@garage44/mithril'
import {Alert, Badge, Button, ButtonGroup, Card, FieldNumber, FieldSegmented, FieldText, Spinner} from '@garage44/mithril/components'

import type {
    BuildingDesign,
    BuildingResponse,
    OpeningKind,
    P2,
    Selection,
    Stair,
    Storey,
    Wall,
} from '@/lib/design'

import {$s, api, notifier, signupApi} from '@/app'
import {GameLink} from '@/components/elements/buildings/buildings'
import {ApiError} from '@/lib/api'
import {
    addOpening,
    addStair,
    addStorey,
    addWall,
    alongWall,
    clampOpening,
    clone,
    copyWindowsUp,
    dist,
    flipStair,
    moveCorner,
    nearestOnSegment,
    norm,
    pointInRing,
    problems,
    removeSelected,
    removeTopStorey,
    restack,
    snap,
    stairRect,
    slideOpening,
    spreadWindows,
    sub,
    suggestedWindows,
    toLocal,
    wallLength,
} from '@/lib/design'
import {bind, local} from '@/lib/reactive'
import {findRooms, labelPoint, nameRoomAt, relabel} from '@/lib/rooms'

type Tool = 'door' | 'select' | 'stair' | 'wall' | 'window'

/**
 * The tools, each with its keyboard key. There is no room tool: a room is
 * whatever the walls enclose, found by `lib/rooms`, and naming one is a tap on
 * it with Select.
 */
const TOOLS: {hint: string; id: Tool; key: string; label: string}[] = [
    {
        hint: 'Tap a wall, a door, a window or the stairs to change them — drag a wall’s end or a door to move it. Tap a room to name it. Drag the plan to move around.',
        id: 'select',
        key: 'v',
        label: 'Select',
    },
    {hint: 'Tap where a wall starts, then where it ends. Keep tapping to carry on; Done (or Esc) to stop. Rooms appear by themselves.', id: 'wall', key: 'w', label: 'Wall'},
    {hint: 'Tap a wall where the door should be. A door in an outside wall is a front door in the street too.', id: 'door', key: 'd', label: 'Door'},
    {hint: 'Tap a wall where the window should be. Windows in outside walls are the building’s windows in the street.', id: 'window', key: 'n', label: 'Window'},
    {hint: 'Tap where the stairs start, then the direction they climb. They rise to the floor above.', id: 'stair', key: 's', label: 'Stairs'},
]

/** How far, in screen pixels, a tap may be from a thing and still hit it. */
const HIT_PX = 14

/** The session to edit with: the player's, or an operator's on the admin side. */
const client = () => ($s.signup.token ? signupApi : api)

interface View {
    cx: number
    cy: number
    /** Metres across the plan. */
    w: number
}

interface BuildingAttrs {
    pand?: string
}

/**
 * The floor-plan editor for one building (`/building/{pand}`).
 *
 * Opens with the building's real outline and whatever design it has — its
 * stored one, or the one the game would generate from the registers, which is
 * where every owner starts. The plan is one storey at a time, north up, metres
 * on a grid; the storey below shows faintly underneath, because a staircase has
 * to land somewhere.
 *
 * Everything here edits a draft. Nothing reaches the world until Save, and the
 * game picks the save up within half a minute (`systems::building_designs`).
 */
export class Building extends MithrilComponent<BuildingAttrs> {
    s = local({
        busy: false,
        cursor: null as P2 | null,
        data: null as BuildingResponse | null,
        design: null as BuildingDesign | null,
        dirty: false,
        error: '',
        /** An enclosed area nobody has named yet, tapped with Select. */
        face: null as P2[] | null,
        future: [] as BuildingDesign[],
        history: [] as BuildingDesign[],
        k: 0,
        pending: null as P2 | null,
        sel: null as Selection | null,
        tool: 'select' as Tool,
        view: {cx: 0, cy: 0, w: 30} as View,
    })

    /** The `pand` the last load was for, so a new one in the route reloads. */
    private pand: string | undefined = undefined
    private svg: SVGSVGElement | null = null
    /** Numbers being typed and not yet applied, by selection and field (see `num` in the view). */
    private drafts = new Map<string, number | null>()
    private drag: {
        /** A wall end or an opening being dragged, with the draft before it. */
        edit?: {before: BuildingDesign; from: P2; target: {kind: 'corner'} | {id: string; kind: 'opening'; wallId: string}}
        moved: boolean
        start: [number, number]
        view: View
    } | null = null

    /** The outline and neighbours, recomputed only when the data or the origin moves. */
    private memo: {
        data: BuildingResponse | null
        lat: number | undefined
        lng: number | undefined
        neighbours: P2[][]
        outline: P2[]
    } | null = null

    private load = async () => {
        const s = this.s
        const pand = this.pand ?? ''
        const query = new URLSearchParams(window.location.search)
        const hint = query.get('lat') && query.get('lng') ? `?lat=${query.get('lat')}&lng=${query.get('lng')}` : ''
        if (!$s.signup.token && !$s.profile.token) {
            s.error = 'Sign in to open the building editor.'
            return
        }
        try {
            const r = await client().get<BuildingResponse>(`/api/pipeline/buildings/${pand}${hint}`)
            s.data = r
            s.design = r.design ? clone(r.design) : null
            s.history = []
            s.future = []
            s.dirty = false
            s.sel = null
            s.k = 0
            if (r.design) {
                s.view = fit(r)
            }
        } catch (e) {
            s.error = e instanceof ApiError ? e.message : 'Could not open that building'
        }
    }

    private syncPand(vnode: m.Vnode<BuildingAttrs>) {
        const pand = vnode.attrs.pand ?? ''
        if (pand === this.pand) {
            return
        }
        this.pand = pand
        void this.load()
    }

    // The keyboard, for whoever has one. Registered once, for the life of the
    // page; the handler reads the current draft whenever a key arrives.
    private onKey = (e: KeyboardEvent) => this.keys(e)

    // Leaving with unsaved work is the one mistake this page can make for you.
    private warn = (e: BeforeUnloadEvent) => {
        if (this.s.dirty) {
            e.preventDefault()
        }
    }

    oninit(vnode: m.Vnode<BuildingAttrs>) {
        this.syncPand(vnode)
    }

    onupdate(vnode: m.VnodeDOM<BuildingAttrs>) {
        this.syncPand(vnode)
    }

    oncreate() {
        window.addEventListener('keydown', this.onKey)
        window.addEventListener('beforeunload', this.warn)
    }

    onremove() {
        window.removeEventListener('keydown', this.onKey)
        window.removeEventListener('beforeunload', this.warn)
    }

    // ── Derived values ─────────────────────────────────────────────────────

    private memoised() {
        const {data, design} = this.s
        const lat = design?.origin[0]
        const lng = design?.origin[1]
        const memo = this.memo
        if (memo && memo.data === data && memo.lat === lat && memo.lng === lng) {
            return memo
        }
        const outline: P2[] = data?.facts && design
            ? data.facts.outline.map(([la, lo]) => toLocal(design.origin, la, lo))
            : []
        const neighbours: P2[][] = data?.facts && design
            ? data.facts.neighbours.map((r) => r.map(([la, lo]) => toLocal(design.origin, la, lo)))
            : []
        this.memo = {data, lat, lng, neighbours, outline}
        return this.memo
    }

    private get outline() {
        return this.memoised().outline
    }

    private get canEdit() {
        return !!this.s.data?.access.can_edit
    }

    private get storey(): Storey {
        const design = this.s.design!
        return design.storeys[Math.min(this.s.k, design.storeys.length - 1)]
    }

    private get pxPerM() {
        return this.svg ? this.svg.clientWidth / this.s.view.w : 20
    }

    private get hitM() {
        return HIT_PX / this.pxPerM
    }

    // ── Selection details ──────────────────────────────────────────────────

    private get selWall() {
        const sel = this.s.sel
        return sel?.kind === 'wall' ? this.storey.walls.find((w) => w.id === sel.id) : undefined
    }

    private get selOpeningWall() {
        const sel = this.s.sel
        return sel?.kind === 'opening' ? this.storey.walls.find((w) => w.id === sel.wallId) : undefined
    }

    private get selOpening() {
        const sel = this.s.sel
        return this.selOpeningWall?.openings.find((o) => sel && o.id === sel.id)
    }

    /** Make `next` the draft, with an undo step back to the current one. */
    private commit = (next: BuildingDesign, before: BuildingDesign = this.s.design!) => {
        const s = this.s
        // Rooms follow their walls: every change re-finds them, and a named room
        // keeps its name wherever its walls went.
        for (const st of next.storeys) {
            st.rooms = relabel(st)
        }
        s.history = [...s.history.slice(-60), before]
        s.future = []
        s.design = next
        s.dirty = true
    }

    /** Change the draft through a function, with an undo step. */
    private mutate = (fn: (d: BuildingDesign, s: Storey) => void) => {
        const next = clone(this.s.design!)
        fn(next, next.storeys[Math.min(this.s.k, next.storeys.length - 1)])
        this.commit(next)
    }

    private undo = () => {
        const s = this.s
        const prev = s.history[s.history.length - 1]
        if (!prev) {
            return
        }
        const design = s.design!
        s.future = [design, ...s.future]
        s.history = s.history.slice(0, -1)
        s.design = prev
        s.sel = null
        s.dirty = true
    }

    private redo = () => {
        const s = this.s
        const [next, ...rest] = s.future
        if (!next) {
            return
        }
        s.history = [...s.history, s.design!]
        s.future = rest
        s.design = next
        s.sel = null
        s.dirty = true
    }

    private save = async () => {
        const s = this.s
        s.busy = true
        try {
            const r = await client().put<{design: BuildingDesign}>(
                `/api/pipeline/buildings/${this.pand ?? ''}/design`,
                s.design,
            )
            s.design = r.design
            s.data = {...s.data!, design: r.design, stored: true}
            s.dirty = false
            notifier.success('Saved. The game shows it within half a minute.')
        } catch (e) {
            notifier.error(e instanceof ApiError ? e.message : 'Could not save the design')
        }
        s.busy = false
    }

    private reset = async () => {
        const s = this.s
        s.busy = true
        try {
            if (s.data!.stored) {
                await client().delete(`/api/pipeline/buildings/${this.pand ?? ''}/design`)
            }
            notifier.success('Back to the layout the game generates')
            await this.load()
        } catch (e) {
            notifier.error(e instanceof ApiError ? e.message : 'Could not reset the design')
        }
        s.busy = false
    }

    private keys(e: KeyboardEvent) {
        const s = this.s
        // Only once the editor is on screen, as under Preact, where the handler
        // was installed by the full render.
        if (s.error || !s.data || !s.design) {
            return
        }
        const canEdit = this.canEdit
        // The tool and storey pickers are radio inputs, and one keeps the focus
        // after a click; that is choosing, not typing, so the keys still work.
        const typing =
            (e.target instanceof HTMLInputElement && e.target.type !== 'radio') || e.target instanceof HTMLTextAreaElement
        if (typing) {
            return
        }
        const mod = e.ctrlKey || e.metaKey
        if (mod && e.key.toLowerCase() === 'z') {
            e.preventDefault()
            if (e.shiftKey) {
                this.redo()
            } else {
                this.undo()
            }
        } else if (mod && e.key.toLowerCase() === 'y') {
            e.preventDefault()
            this.redo()
        } else if (e.key === 'Escape') {
            s.pending = null
            s.sel = null
            s.face = null
        } else if ((e.key === 'Delete' || e.key === 'Backspace') && s.sel && canEdit) {
            e.preventDefault()
            const was = s.sel
            this.mutate((_, st) => removeSelected(st, was))
            s.sel = null
        } else if (!mod && canEdit) {
            const t = TOOLS.find((x) => x.key === e.key.toLowerCase())
            if (t) {
                this.pick(t.id)
            }
        }
    }

    private pick = (tid: Tool) => {
        const s = this.s
        s.tool = tid
        s.pending = null
        s.sel = null
        s.face = null
    }

    // ── Pointer handling ───────────────────────────────────────────────────

    private toPlan = (e: PointerEvent | WheelEvent): P2 | null => {
        const el = this.svg
        const ctm = el?.getScreenCTM()
        if (!el || !ctm) {
            return null
        }
        const pt = new DOMPoint(e.clientX, e.clientY).matrixTransform(ctm.inverse())
        // SVG y runs down; the plan's runs north.
        return [pt.x, -pt.y]
    }

    private snapped = (p: P2) => snap(p, this.storey, this.outline, this.hitM)

    private hitTest = (p: P2): Selection | null => {
        const storey = this.storey
        const hitM = this.hitM
        for (const w of storey.walls) {
            for (const o of w.openings) {
                const c = alongWall(w, o.at)
                if (dist(c, p) <= Math.max(hitM, o.width / 2)) {
                    return {id: o.id, kind: 'opening', wallId: w.id}
                }
            }
        }
        for (const s of storey.stairs) {
            if (pointInRing(stairRect(s), p)) {
                return {id: s.id, kind: 'stair'}
            }
        }
        let best: {d: number; id: string} | null = null
        for (const w of storey.walls) {
            const d = nearestOnSegment(p, w.a, w.b).off
            if (d < hitM && (!best || d < best.d)) {
                best = {d, id: w.id}
            }
        }
        if (best) {
            return {id: best.id, kind: 'wall'}
        }
        const room = storey.rooms.find((r) => pointInRing(r.outline, p))
        return room ? {id: room.id, kind: 'room'} : null
    }

    /** The unnamed enclosed area under `p`, if the tap found nothing else. */
    private faceAt = (p: P2): P2[] | null => findRooms(this.storey).find((f) => pointInRing(f, p)) ?? null

    private tap = (raw: P2) => {
        const s = this.s
        const tool = s.tool
        const design = s.design!
        const k = s.k
        if (!this.canEdit && tool !== 'select') {
            return
        }
        const p = this.snapped(raw)
        switch (tool) {
            case 'select': {
                const hit = this.hitTest(raw)
                s.sel = hit
                s.face = hit ? null : this.faceAt(raw)
                return
            }
            case 'wall':
                if (!s.pending) {
                    s.pending = p
                } else if (dist(s.pending, p) < 0.05) {
                    s.pending = null
                } else {
                    const from = s.pending
                    this.mutate((_, st) => {
                        addWall(st, from, p)
                    })
                    s.pending = p
                }
                return
            case 'door':
            case 'window': {
                const kind: OpeningKind = tool
                // Tried on a copy first: a tap that misses every wall must not
                // leave an undo step that undoes nothing.
                const next = clone(design)
                if (addOpening(next.storeys[k], raw, kind, this.hitM * 2)) {
                    this.commit(next)
                } else {
                    notifier.error(`Tap on a wall to put a ${kind} in it`)
                }
                return
            }
            case 'stair':
                if (!s.pending) {
                    s.pending = p
                } else {
                    const foot = s.pending
                    if (k === design.storeys.length - 1) {
                        notifier.error('Stairs climb to the floor above — add a storey first')
                    } else {
                        this.mutate((_, st) => {
                            addStair(st, foot, p)
                        })
                    }
                    s.pending = null
                }
                return
        }
    }

    private onPointerDown = (e: PointerEvent) => {
        const s = this.s
        ;(e.target as Element).setPointerCapture?.(e.pointerId)
        this.drag = {moved: false, start: [e.clientX, e.clientY], view: s.view}
        // With Select, a press on the selected wall's end or the selected
        // opening grabs it instead of the plan.
        const p = this.toPlan(e)
        if (!p || s.tool !== 'select' || !this.canEdit) {
            return
        }
        const design = s.design!
        const grab = this.hitM * 1.5
        const selWall = this.selWall
        const selOpening = this.selOpening
        const selOpeningWall = this.selOpeningWall
        if (selWall) {
            const end = [selWall.a, selWall.b].find((c) => dist(c, p) < grab)
            if (end) {
                this.drag.edit = {before: design, from: end, target: {kind: 'corner'}}
            }
        } else if (selOpening && selOpeningWall) {
            if (dist(alongWall(selOpeningWall, selOpening.at), p) < Math.max(grab, selOpening.width / 2)) {
                this.drag.edit = {
                    before: design,
                    from: p,
                    target: {id: selOpening.id, kind: 'opening', wallId: selOpeningWall.id},
                }
            }
        }
    }

    private onPointerMove = (e: PointerEvent) => {
        const s = this.s
        const p = this.toPlan(e)
        if (p) {
            s.cursor = this.snapped(p)
        }
        const d = this.drag
        if (!d) {
            return
        }
        const dx = e.clientX - d.start[0]
        const dy = e.clientY - d.start[1]
        if (!d.moved && Math.hypot(dx, dy) < 6) {
            return
        }
        d.moved = true
        if (d.edit && p) {
            // Rebuilt from the draft as it was at the press, so the drag is one
            // move however many frames it took, and one undo step.
            const next = clone(d.edit.before)
            const st = next.storeys[Math.min(s.k, next.storeys.length - 1)]
            const target = d.edit.target
            if (target.kind === 'corner') {
                const others: Storey = {...st, walls: st.walls.filter((w) => dist(w.a, d.edit!.from) > 0.03 && dist(w.b, d.edit!.from) > 0.03)}
                moveCorner(st, d.edit.from, snap(p, others, this.outline, this.hitM))
            } else {
                const wall = st.walls.find((w) => w.id === target.wallId)
                if (wall) {
                    slideOpening(wall, target.id, p)
                }
            }
            s.design = next
            return
        }
        // Any tool pans on a drag: on a phone there is no other way to move.
        const pxPerM = this.pxPerM
        s.view = {...d.view, cx: d.view.cx - dx / pxPerM, cy: d.view.cy + dy / pxPerM}
    }

    private onPointerUp = (e: PointerEvent) => {
        const d = this.drag
        this.drag = null
        if (d?.edit && d.moved) {
            this.commit(clone(this.s.design!), d.edit.before)
            return
        }
        if (d && !d.moved) {
            const p = this.toPlan(e)
            if (p) {
                this.tap(p)
            }
        }
    }

    private onWheel = (e: WheelEvent) => {
        e.preventDefault()
        this.zoom(e.deltaY > 0 ? 1.15 : 1 / 1.15)
    }

    private zoom = (f: number) => {
        const view = this.s.view
        this.s.view = {...view, w: Math.max(4, Math.min(200, view.w * f))}
    }

    view() {
        const s = this.s
        const pand = this.pand ?? ''
        const {data, design, error, face, k, pending, cursor, sel, tool, view, history, future, busy, dirty} = s

        if (error) {
            return (
                <Card title="Building">
                    <p class="designer__hint">{error}</p>
                    <Button onclick={() => m.route.set('/buildings')} variant="ghost">
                        Your buildings
                    </Button>
                </Card>
            )
        }
        if (!data) {
            return (
                <div class="designer__splash">
                    <Spinner />
                    <span>Opening the building…</span>
                </div>
            )
        }
        if (!design) {
            return (
                <Card title={`Building ${pand}`}>
                    <p class="designer__hint">
                        This building is too small to have an inside, or the map data for it is not on this
                        server yet.
                    </p>
                </Card>
            )
        }

        const {outline, neighbours} = this.memoised()
        const canEdit = data.access.can_edit
        const storey = this.storey
        const below = k > 0 ? design.storeys[k - 1] : null
        const pxPerM = this.pxPerM
        const mutate = this.mutate

        const selWall = this.selWall
        const selOpeningWall = this.selOpeningWall
        const selOpening = this.selOpening
        const selStair = sel?.kind === 'stair' ? storey.stairs.find((st) => st.id === sel.id) : undefined
        const selRoom = sel?.kind === 'room' ? storey.rooms.find((r) => r.id === sel.id) : undefined
        const faces = findRooms(storey)
        const unnamed = faces.filter((f) => !storey.rooms.some((r) => pointInRing(f, labelPoint(r.outline))))

        /** Edit the selected thing in a fresh draft, found again by id. */
        const editSel = (fn: (x: {opening?: Wall['openings'][0]; room?: Storey['rooms'][0]; stair?: Stair; wall?: Wall}) => void) =>
            mutate((_, st) => {
                if (!sel) {
                    return
                }
                const wall = st.walls.find((w) => w.id === (sel.kind === 'opening' ? sel.wallId : sel.id))
                fn({
                    opening: sel.kind === 'opening' ? wall?.openings.find((o) => o.id === sel.id) : undefined,
                    room: st.rooms.find((r) => r.id === sel.id),
                    stair: st.stairs.find((x) => x.id === sel.id),
                    wall: sel.kind === 'wall' ? wall : undefined,
                })
            })


        /**
         * A number field for a property of the selection (or of the storey).
         *
         * `FieldNumber` writes its model on every keystroke, and a write here is
         * an edit with an undo step — typing "1.25" would be four of them, and
         * a minimum clamped mid-word ("0" of "0.3" against 0.02) would rewrite
         * the box under the cursor. So keystrokes land in a draft, and the
         * draft is applied, clamped, when the field commits: on `change` (blur
         * or Enter) and after a stepper press or an arrow key, which bubble up
         * to the wrapper after the field has written the draft. The old field
         * applied on `change` only; the steppers are new.
         */
        const num = (key: string, label: string, value: number, step: number, set: (v: number) => void, min = 0, max = 100) => {
            const id = `${sel ? sel.id : `storey-${k}`}:${key}`
            const drafts = this.drafts
            const apply = () => {
                if (!drafts.has(id)) {
                    return
                }
                const v = drafts.get(id)
                drafts.delete(id)
                if (v != null && Number.isFinite(v)) {
                    set(Math.max(min, Math.min(max, v)))
                }
            }
            return (
                <div
                    class="designer__num"
                    key={id}
                    onchange={apply}
                    onclick={(e: MouseEvent) => {
                        if ((e.target as Element).closest('.m-input__step')) {
                            apply()
                        }
                    }}
                    onkeyup={(e: KeyboardEvent) => {
                        if (e.key === 'ArrowUp' || e.key === 'ArrowDown') {
                            apply()
                        }
                    }}
                >
                    <FieldNumber
                        disabled={!canEdit}
                        label={label}
                        max={max}
                        min={min}
                        model={{
                            get value() {
                                return drafts.has(id) ? (drafts.get(id) ?? null) : Number(value.toFixed(2))
                            },
                            set value(v: number | null) {
                                drafts.set(id, v)
                            },
                        }}
                        step={step}
                        unit="m"
                    />
                </div>
            )
        }

        /**
         * A name field. It writes on `change` (`lazy`), as the old one did, so a
         * name is one undo step and not one per letter. `FieldText` has no
         * `maxlength`, so the 120-character cap the input carried is a slice.
         */
        const nameField = (key: string, value: string, set: (v: string) => void, placeholder?: string) => (
            <FieldText
                disabled={!canEdit}
                key={key}
                label="Name"
                lazy
                model={{
                    get value() {
                        return value
                    },
                    set value(v: string) {
                        set(v.slice(0, 120))
                    },
                }}
                placeholder={placeholder}
            />
        )


        const issues = problems(design, data.facts?.height ?? null)
        const toolHint = TOOLS.find((t) => t.id === tool)?.hint

        // ── The plan ───────────────────────────────────────────────────────────

        const aspect = 0.75
        const h = view.w * aspect
        const vb = `${view.cx - view.w / 2} ${-view.cy - h / 2} ${view.w} ${h}`
        const pts = (r: P2[]) => r.map((p) => `${p[0]},${-p[1]}`).join(' ')
        const stroke = (metres: number) => Math.max(metres, 1.5 / pxPerM)

        const grid: m.Vnode[] = []
        {
            const step = view.w > 60 ? 5 : 1
            const x0 = Math.floor((view.cx - view.w / 2) / step) * step
            const y0 = Math.floor((view.cy - h / 2) / step) * step
            for (let x = x0; x <= view.cx + view.w / 2; x += step) {
                grid.push(<line class={x % 5 === 0 ? 'g5' : 'g1'} key={`x${x}`} x1={x} x2={x} y1={-(view.cy + h)} y2={-(view.cy - h)} />)
            }
            for (let y = y0; y <= view.cy + h / 2; y += step) {
                grid.push(<line class={y % 5 === 0 ? 'g5' : 'g1'} key={`y${y}`} x1={view.cx - view.w} x2={view.cx + view.w} y1={-y} y2={-y} />)
            }
        }

        const drawWall = (w: Wall, faint: boolean) => {
            const selected = sel?.kind === 'wall' && sel.id === w.id
            const width = stroke(w.exterior ? 0.24 : Math.max(w.thickness, 0.08))
            return (
                <g class={faint ? 'faint' : ''} key={w.id}>
                    <line
                        class={`wall ${w.exterior ? 'outer' : 'inner'} ${selected ? 'is-selected' : ''}`}
                        stroke-width={width}
                        x1={w.a[0]}
                        x2={w.b[0]}
                        y1={-w.a[1]}
                        y2={-w.b[1]}
                    />
                    {!faint &&
                        w.openings.map((o) => {
                            const d = norm(sub(w.b, w.a)) ?? ([1, 0] as P2)
                            const p = alongWall(w, o.at - o.width / 2)
                            const q = alongWall(w, o.at + o.width / 2)
                            const left: P2 = [-d[1], d[0]]
                            const leaf: P2 = [p[0] + left[0] * o.width, p[1] + left[1] * o.width]
                            const isSel = sel?.kind === 'opening' && sel.id === o.id
                            // The swing, from the open leaf round to the far jamb about
                            // the hinge — in screen coordinates (y down), where a
                            // positive cross product is clockwise, which is SVG's
                            // sweep flag 1.
                            const u = [leaf[0] - p[0], -(leaf[1] - p[1])]
                            const v = [q[0] - p[0], -(q[1] - p[1])]
                            const sweep = u[0] * v[1] - u[1] * v[0] > 0 ? 1 : 0
                            return (
                                <g class={`opening ${o.kind} ${isSel ? 'is-selected' : ''}`} key={o.id}>
                                    <line class="gap" stroke-width={width + stroke(0.04)} x1={p[0]} x2={q[0]} y1={-p[1]} y2={-q[1]} />
                                    {o.kind === 'door' ? (
                                        <>
                                            <line stroke-width={stroke(0.04)} x1={p[0]} x2={leaf[0]} y1={-p[1]} y2={-leaf[1]} />
                                            <path
                                                d={`M ${leaf[0]} ${-leaf[1]} A ${o.width} ${o.width} 0 0 ${sweep} ${q[0]} ${-q[1]}`}
                                                fill="none"
                                                stroke-dasharray={`${stroke(0.1)} ${stroke(0.08)}`}
                                                stroke-width={stroke(0.02)}
                                            />
                                        </>
                                    ) : (
                                        <line stroke-width={stroke(0.06)} x1={p[0]} x2={q[0]} y1={-p[1]} y2={-q[1]} />
                                    )}
                                </g>
                            )
                        })}
                </g>
            )
        }

        const drawStair = (st: Stair, faint: boolean) => {
            const r = stairRect(st)
            const d = norm(st.dir) ?? ([1, 0] as P2)
            const isSel = sel?.kind === 'stair' && sel.id === st.id
            const treads = []
            for (let i = 1; i < 16; i++) {
                const t = (i / 16) * st.run
                const a: P2 = [r[0][0] + d[0] * t, r[0][1] + d[1] * t]
                const b: P2 = [r[3][0] + d[0] * t, r[3][1] + d[1] * t]
                treads.push(<line key={i} stroke-width={stroke(0.015)} x1={a[0]} x2={b[0]} y1={-a[1]} y2={-b[1]} />)
            }
            const mid0: P2 = [(r[0][0] + r[3][0]) / 2, (r[0][1] + r[3][1]) / 2]
            const mid1: P2 = [(r[1][0] + r[2][0]) / 2, (r[1][1] + r[2][1]) / 2]
            return (
                <g class={`stair ${faint ? 'faint' : ''} ${isSel ? 'is-selected' : ''}`} key={st.id}>
                    <polygon points={pts(r)} stroke-width={stroke(0.03)} />
                    {treads}
                    <line class="arrow" marker-end="url(#arrow)" stroke-width={stroke(0.04)} x1={mid0[0]} x2={mid1[0]} y1={-mid0[1]} y2={-mid1[1]} />
                </g>
            )
        }

        const area = (r: P2[]) => Math.abs(r.reduce((a, p, i) => a + p[0] * r[(i + 1) % r.length][1] - r[(i + 1) % r.length][0] * p[1], 0) / 2)

        const owner = data.access.owned_by
        const [lat, lng] = design.origin
        // ── The properties panel: whatever is selected, or the storey ────────────

        let panel: {actions?: m.Children; body: m.Children; title: string}
        if (selWall) {
            panel = {
                actions: canEdit && (
                    <>
                        <Button
                            onclick={() => editSel(({wall}) => wall && spreadWindows(wall, suggestedWindows(wall)))}
                            size="sm"
                        >
                            Space {suggestedWindows(selWall)} windows along it
                        </Button>
                        {k + 1 < design.storeys.length && selWall.openings.some((o) => o.kind === 'window') && (
                            <Button
                                onclick={() => {
                                    let n = 0
                                    mutate((d) => {
                                        n = copyWindowsUp(d, k, selWall.id)
                                    })
                                    notifier.success(
                                        n
                                            ? `Same windows on ${n} floor${n === 1 ? '' : 's'} above`
                                            : 'No floor above has a wall on the same line',
                                    )
                                }}
                                size="sm"
                            >
                                Same windows on the floors above
                            </Button>
                        )}
                    </>
                ),
                body: (
                    <>
                        <p class="designer__hint">
                            {wallLength(selWall).toFixed(2)} m long.
                            {canEdit && ' Drag either end to move it; the walls meeting it come along.'}
                        </p>
                        {!selWall.exterior && (
                            <div class="designer__fields">
                                {num('thickness', 'Thickness', selWall.thickness, 0.01, (v) => editSel(({wall}) => wall && (wall.thickness = v)), 0.02, 1.5)}
                            </div>
                        )}
                    </>
                ),
                title: selWall.exterior ? 'Outer wall' : 'Wall',
            }
        } else if (selOpening && selOpeningWall) {
            panel = {
                body: (
                    <>
                        <div class="designer__fields">
                            {num('width', 'Width', selOpening.width, 0.05, (v) =>
                                editSel(({opening}) => {
                                    if (opening) {
                                        opening.width = Math.min(v, wallLength(selOpeningWall))
                                        opening.at = clampOpening(opening.at, opening.width, wallLength(selOpeningWall))
                                    }
                                }), 0.2, 6)}
                            {num('height', 'Height', selOpening.height, 0.05, (v) => editSel(({opening}) => opening && (opening.height = v)), 0.2, 10)}
                            {num('sill', 'Sill', selOpening.sill, 0.05, (v) => editSel(({opening}) => opening && (opening.sill = v)), 0, 5)}
                            {num('at', 'From the wall end', selOpening.at, 0.05, (v) =>
                                editSel(({opening}) => opening && (opening.at = clampOpening(v, opening.width, wallLength(selOpeningWall)))), 0, 100)}
                        </div>
                        <p class="designer__hint">
                            {selOpening.kind === 'door'
                                ? 'A door with a sill above 30 cm cannot be walked through.'
                                : 'A window is a hole you can see through, not walk through.'}
                        </p>
                    </>
                ),
                title: selOpening.kind === 'door' ? 'Door' : 'Window',
            }
        } else if (selStair) {
            panel = {
                actions: canEdit && (
                    <Button onclick={() => editSel(({stair}) => stair && flipStair(stair))} size="sm">
                        Turn round
                    </Button>
                ),
                body: (
                    <div class="designer__fields">
                        {num('width', 'Width', selStair.width, 0.05, (v) => editSel(({stair}) => stair && (stair.width = v)), 0.5, 5)}
                        {num('run', 'Length', selStair.run, 0.1, (v) => editSel(({stair}) => stair && (stair.run = v)), 1, 15)}
                    </div>
                ),
                title: 'Stairs',
            }
        } else if (selRoom) {
            panel = {
                body: (
                    <>
                        {nameField(`room-${selRoom.id}`, selRoom.name, (v) => editSel(({room}) => room && (room.name = v)))}
                        <p class="designer__hint">{area(selRoom.outline).toFixed(1)} m². The name shows in the game when somebody stands in it.</p>
                    </>
                ),
                title: 'Room',
            }
        } else if (face) {
            panel = {
                body: (
                    <>
                        {nameField(
                            'face',
                            '',
                            (raw) => {
                                const v = raw.trim()
                                if (!v) {
                                    return
                                }
                                const at = labelPoint(face)
                                let id: null | string = null
                                mutate((_, st) => {
                                    id = nameRoomAt(st, at, v)
                                })
                                if (id) {
                                    s.sel = {id, kind: 'room'}
                                    s.face = null
                                }
                            },
                            'Keuken, Woonkamer…',
                        )}
                        <p class="designer__hint">{area(face).toFixed(1)} m², not named yet.</p>
                    </>
                ),
                title: 'Room',
            }
        } else {
            panel = {
                actions: canEdit && k === design.storeys.length - 1 && design.storeys.length > 1 && (
                    <Button
                        onclick={() => {
                            mutate((d) => {
                                removeTopStorey(d)
                            })
                            s.k = k - 1
                        }}
                        size="sm"
                    >
                        Remove this storey
                    </Button>
                ),
                body: (
                    <>
                        <div class="designer__fields">
                            {nameField(`storey-${k}`, storey.name, (v) =>
                                mutate((_, st) => {
                                    st.name = v
                                }))}
                            {num('height', 'Floor to floor', storey.height, 0.1, (v) =>
                                mutate((d, st) => {
                                    st.height = v
                                    restack(d)
                                }), 2, 12)}
                        </div>
                        <p class="designer__hint">
                            {storey.walls.length} walls, {storey.walls.reduce((a, w) => a + w.openings.length, 0)} doors and
                            windows, {storey.stairs.length} stairs. Floor at {storey.elevation.toFixed(2)} m.
                        </p>
                    </>
                ),
                title: storey.name,
            }
        }
        const deleteSel = canEdit && sel && (
            <Button
                onclick={() => {
                    const was = sel
                    mutate((_, st) => removeSelected(st, was))
                    s.sel = null
                }}
                size="sm"
                variant="danger"
            >
                Delete
            </Button>
        )

        return (
            <div class="designer">
                <header class="designer__head">
                    <h1 class="designer__title">{owner ? `${owner}'s building` : 'Building'}</h1>
                    <code class="designer__pand">{pand}</code>
                    <span class="designer__spacer" />
                    <GameLink lat={lat} lng={lng} />
                </header>

                {!canEdit && (
                    <Alert tone="info">
                        You can look, not change: only the building's owner can.{' '}
                        {data.stored ? 'This is the design its owner saved.' : 'Nobody has designed it yet; this is what the game generates.'}
                    </Alert>
                )}

                {/*
                  * One storey is shown at a time, so the storeys are a one-of
                  * choice. Picking one drops the selection and any wall or
                  * flight half drawn, which belong to the storey they were on.
                  */}
                <div class="designer__storeys">
                    <FieldSegmented
                        model={{
                            get value() {
                                return k
                            },
                            set value(i: number) {
                                s.k = i
                                s.sel = null
                                s.pending = null
                                s.face = null
                            },
                        }}
                        options={design.storeys.map((st, i) => ({label: st.name || `Storey ${i}`, value: i}))}
                        size="sm"
                    />
                    {canEdit && (
                        <Button
                            onclick={() => {
                                // The count before the new one: its index once added.
                                const top = design.storeys.length
                                mutate((d) => {
                                    addStorey(d)
                                })
                                s.k = top
                            }}
                            size="sm"
                            title="Add a storey on top"
                            variant="ghost"
                        >
                            + storey
                        </Button>
                    )}
                </div>

                {canEdit && (
                    <div class="designer__toolbar">
                        <FieldSegmented
                            model={bind(s, 'tool')}
                            onchange={(t: Tool) => this.pick(t)}
                            options={TOOLS.map((t) => ({
                                label: <span title={`${t.label} (${t.key.toUpperCase()})`}>{t.label}</span>,
                                value: t.id,
                            }))}
                            size="sm"
                        />
                        <ButtonGroup attached>
                            {pending && (
                                <Button
                                    onclick={() => {
                                        s.pending = null
                                    }}
                                    size="sm"
                                >
                                    Done
                                </Button>
                            )}
                            <Button aria-label="Undo" disabled={!history.length} onclick={this.undo} size="sm" title="Undo">
                                ↶
                            </Button>
                            <Button aria-label="Redo" disabled={!future.length} onclick={this.redo} size="sm" title="Redo">
                                ↷
                            </Button>
                        </ButtonGroup>
                    </div>
                )}
                {canEdit && toolHint && <p class="designer__hint designer__tool-hint">{toolHint}</p>}

                <div class="designer__body">
                    <div class="designer__plan-wrap">
                        <svg
                            class={`designer__plan tool-${tool}`}
                            oncreate={(v: m.VnodeDOM) => {
                                this.svg = v.dom as SVGSVGElement
                            }}
                            onpointerdown={this.onPointerDown}
                            onpointerleave={() => {
                                s.cursor = null
                            }}
                            onpointermove={this.onPointerMove}
                            onpointerup={this.onPointerUp}
                            onremove={() => {
                                this.svg = null
                            }}
                            onwheel={this.onWheel}
                            preserveAspectRatio="xMidYMid meet"
                            viewBox={vb}
                        >
                            <defs>
                                <marker id="arrow" markerHeight="4" markerWidth="4" orient="auto" refX="2" refY="2" viewBox="0 0 4 4">
                                    <path d="M0,0 L4,2 L0,4 z" />
                                </marker>
                            </defs>
                            <g class="grid" stroke-width={stroke(0.01)}>
                                {grid}
                            </g>
                            {neighbours.map((r, i) => (
                                <polygon class="neighbour" key={`n${i}`} points={pts(r)} stroke-width={stroke(0.03)} />
                            ))}
                            {outline.length > 2 && (
                                <polygon
                                    class="outline"
                                    points={pts(outline)}
                                    stroke-dasharray={`${stroke(0.3)} ${stroke(0.2)}`}
                                    stroke-width={stroke(0.05)}
                                />
                            )}
                            {storey.floor.length > 2 && <polygon class="floor" points={pts(storey.floor)} />}
                            {/* Over the floor, or the floor hides it: the storey below, faint,
                                because a staircase has to land somewhere. */}
                            {below && <g class="below">{below.walls.map((w) => drawWall(w, true))}</g>}
                            {/* The flights arriving from below, as the hole they come up
                                through — under this storey's own flights, which may rise
                                out of the same well. */}
                            {below?.stairs.map((st) => (
                                <polygon class="well" key={`well${st.id}`} points={pts(stairRect(st))} stroke-width={stroke(0.03)} />
                            ))}
                            {unnamed.map((f, i) => {
                                const c = labelPoint(f)
                                const isSel = face !== null && pointInRing(f, labelPoint(face))
                                return (
                                    <g class={`room unnamed ${isSel ? 'is-selected' : ''}`} key={`f${i}`}>
                                        {isSel && <polygon points={pts(f)} stroke-width={stroke(0.03)} />}
                                        <text class="area" font-size={Math.max(0.24, 9 / pxPerM)} x={c[0]} y={-c[1]}>
                                            {area(f).toFixed(1)} m²
                                        </text>
                                    </g>
                                )
                            })}
                            {storey.rooms.map((r) => {
                                const c = labelPoint(r.outline)
                                const isSel = sel?.kind === 'room' && sel.id === r.id
                                return (
                                    <g class={`room ${isSel ? 'is-selected' : ''}`} key={r.id}>
                                        <polygon points={pts(r.outline)} stroke-width={stroke(0.03)} />
                                        <text font-size={Math.max(0.3, 11 / pxPerM)} x={c[0]} y={-c[1]}>
                                            {r.name}
                                        </text>
                                        <text class="area" font-size={Math.max(0.24, 9 / pxPerM)} x={c[0]} y={-c[1] + Math.max(0.4, 14 / pxPerM)}>
                                            {area(r.outline).toFixed(1)} m²
                                        </text>
                                    </g>
                                )
                            })}
                            {storey.walls.map((w) => drawWall(w, false))}
                            {storey.stairs.map((st) => drawStair(st, false))}
                            {pending && cursor && (
                                <g class="preview">
                                    <line
                                        stroke-dasharray={`${stroke(0.15)} ${stroke(0.1)}`}
                                        stroke-width={stroke(tool === 'wall' ? 0.1 : 0.04)}
                                        x1={pending[0]}
                                        x2={cursor[0]}
                                        y1={-pending[1]}
                                        y2={-cursor[1]}
                                    />
                                    <text font-size={Math.max(0.3, 11 / pxPerM)} x={(pending[0] + cursor[0]) / 2} y={-(pending[1] + cursor[1]) / 2 - 0.3}>
                                        {dist(pending, cursor).toFixed(2)} m
                                    </text>
                                </g>
                            )}
                            {cursor && tool !== 'select' && <circle class="cursor" cx={cursor[0]} cy={-cursor[1]} r={stroke(0.08)} />}
                            <text class="north" font-size={Math.max(0.4, 12 / pxPerM)} x={view.cx - view.w / 2 + 14 / pxPerM} y={-view.cy - h / 2 + 22 / pxPerM}>
                                N ↑
                            </text>
                        </svg>
                        <ButtonGroup class="designer__zoom">
                            <Button aria-label="Zoom in" onclick={() => this.zoom(1 / 1.4)} size="sm" title="Zoom in">
                                +
                            </Button>
                            <Button aria-label="Zoom out" onclick={() => this.zoom(1.4)} size="sm" title="Zoom out">
                                −
                            </Button>
                            <Button
                                aria-label="Fit the building"
                                onclick={() => {
                                    s.view = fit(data)
                                }}
                                size="sm"
                                title="Fit the building"
                            >
                                ⤢
                            </Button>
                        </ButtonGroup>
                    </div>

                    <div class="designer__side">
                        <Card actions={deleteSel || undefined} footer={panel.actions || undefined} title={panel.title}>
                            {panel.body}
                        </Card>

                        {issues.length > 0 && (
                            <Alert tone="warning">
                                {issues.map((p) => (
                                    <p class="designer__issue" key={p}>
                                        {p}
                                    </p>
                                ))}
                            </Alert>
                        )}

                        <p class="designer__hint">
                            In the game, walk in — or tap the building and choose Look inside — and pick a floor on
                            the right. The floor names are yours, and a room's name shows when somebody stands in it.
                        </p>
                    </div>
                </div>

                {/*
                  * Pinned to the bottom of the window: on a phone the plan and the
                  * panel are taller than the screen, and Save must not be a
                  * scroll away from the edit it saves.
                  */}
                {canEdit && (
                    <div class="designer__save">
                        <Button disabled={busy} onclick={() => void this.reset()} size="sm" variant="ghost">
                            {data.stored ? 'Back to generated' : 'Start over'}
                        </Button>
                        <span class="designer__spacer" />
                        <Badge dot tone={dirty ? 'warning' : data.stored ? 'success' : 'neutral'}>
                            {dirty ? 'Unsaved changes' : data.stored ? 'Saved' : 'Not saved yet'}
                        </Badge>
                        <Button disabled={!dirty} loading={busy} onclick={() => void this.save()} variant="primary">
                            Save
                        </Button>
                    </div>
                )}
            </div>
        )
    }
}

/** A view framing the building's outline (or its ground floor) with a margin. */
function fit(r: BuildingResponse): View {
    const d = r.design
    if (!d) {
        return {cx: 0, cy: 0, w: 30}
    }
    const pts: P2[] = r.facts?.outline.length
        ? r.facts.outline.map(([la, lo]) => toLocal(d.origin, la, lo))
        : d.storeys[0]?.floor ?? []
    if (!pts.length) {
        return {cx: 0, cy: 0, w: 30}
    }
    const xs = pts.map((p) => p[0])
    const ys = pts.map((p) => p[1])
    const [x0, x1, y0, y1] = [Math.min(...xs), Math.max(...xs), Math.min(...ys), Math.max(...ys)]
    return {cx: (x0 + x1) / 2, cy: (y0 + y1) / 2, w: Math.max(12, Math.max(x1 - x0, (y1 - y0) / 0.75) * 1.35)}
}

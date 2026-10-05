import type {JSX} from 'preact'
import type {RoutableProps} from 'preact-router'

import {route} from 'preact-router'
import {useEffect, useMemo, useRef, useState} from 'preact/hooks'

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
import {gamePlaceUrl} from '@/components/elements/buildings/buildings'
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
export const Building = (props: RoutableProps & {pand?: string}) => {
    const pand = props.pand ?? ''
    const query = new URLSearchParams(window.location.search)
    const hint = query.get('lat') && query.get('lng') ? `?lat=${query.get('lat')}&lng=${query.get('lng')}` : ''

    const [data, setData] = useState<BuildingResponse | null>(null)
    const [error, setError] = useState('')
    const [design, setDesign] = useState<BuildingDesign | null>(null)
    const [history, setHistory] = useState<BuildingDesign[]>([])
    const [future, setFuture] = useState<BuildingDesign[]>([])
    const [k, setK] = useState(0)
    const [tool, setTool] = useState<Tool>('select')
    const [sel, setSel] = useState<Selection | null>(null)
    const [pending, setPending] = useState<P2 | null>(null)
    /** An enclosed area nobody has named yet, tapped with Select. */
    const [face, setFace] = useState<P2[] | null>(null)
    const [cursor, setCursor] = useState<P2 | null>(null)
    const [view, setView] = useState<View>({cx: 0, cy: 0, w: 30})
    const [dirty, setDirty] = useState(false)
    const [busy, setBusy] = useState(false)
    const svg = useRef<SVGSVGElement>(null)
    const drag = useRef<{
        /** A wall end or an opening being dragged, with the draft before it. */
        edit?: {before: BuildingDesign; from: P2; target: {kind: 'corner'} | {id: string; kind: 'opening'; wallId: string}}
        moved: boolean
        start: [number, number]
        view: View
    } | null>(null)

    const load = async () => {
        if (!$s.signup.token && !$s.profile.token) {
            setError('Sign in to open the building editor.')
            return
        }
        try {
            const r = await client().get<BuildingResponse>(`/api/pipeline/buildings/${pand}${hint}`)
            setData(r)
            setDesign(r.design ? clone(r.design) : null)
            setHistory([])
            setFuture([])
            setDirty(false)
            setSel(null)
            setK(0)
            if (r.design) {
                setView(fit(r))
            }
        } catch (e) {
            setError(e instanceof ApiError ? e.message : 'Could not open that building')
        }
    }

    useEffect(() => {
        void load()
    }, [pand])

    // The keyboard, for whoever has one. Registered once, here above the early
    // returns where hooks must be; the handler itself is rebuilt each render
    // (below) so it sees the current draft.
    const keys = useRef<(e: KeyboardEvent) => void>(() => {})
    useEffect(() => {
        const on = (e: KeyboardEvent) => keys.current(e)
        window.addEventListener('keydown', on)
        return () => window.removeEventListener('keydown', on)
    }, [])

    // Leaving with unsaved work is the one mistake this page can make for you.
    useEffect(() => {
        const warn = (e: BeforeUnloadEvent) => {
            if (dirty) {
                e.preventDefault()
            }
        }
        window.addEventListener('beforeunload', warn)
        return () => window.removeEventListener('beforeunload', warn)
    }, [dirty])

    const outline = useMemo<P2[]>(
        () => (data?.facts && design ? data.facts.outline.map(([la, lo]) => toLocal(design.origin, la, lo)) : []),
        [data, design?.origin[0], design?.origin[1]],
    )
    const neighbours = useMemo<P2[][]>(
        () =>
            data?.facts && design
                ? data.facts.neighbours.map((r) => r.map(([la, lo]) => toLocal(design.origin, la, lo)))
                : [],
        [data, design?.origin[0], design?.origin[1]],
    )

    if (error) {
        return (
            <div class="card">
                <h1>Building</h1>
                <p class="hint">{error}</p>
                <div class="row-actions">
                    <button class="ghost" onClick={() => route('/buildings')} type="button">
                        Your buildings
                    </button>
                </div>
            </div>
        )
    }
    if (!data) {
        return <div class="splash">Opening the building…</div>
    }
    if (!design) {
        return (
            <div class="card">
                <h1>Building {pand}</h1>
                <p class="hint">
                    This building is too small to have an inside, or the map data for it is not on this
                    server yet.
                </p>
            </div>
        )
    }

    const canEdit = data.access.can_edit
    const storey = design.storeys[Math.min(k, design.storeys.length - 1)]
    const below = k > 0 ? design.storeys[k - 1] : null
    const pxPerM = svg.current ? svg.current.clientWidth / view.w : 20
    const hitM = HIT_PX / pxPerM

    /** Make `next` the draft, with an undo step back to the current one. */
    const commit = (next: BuildingDesign, before: BuildingDesign = design) => {
        // Rooms follow their walls: every change re-finds them, and a named room
        // keeps its name wherever its walls went.
        for (const st of next.storeys) {
            st.rooms = relabel(st)
        }
        setHistory([...history.slice(-60), before])
        setFuture([])
        setDesign(next)
        setDirty(true)
    }

    /** Change the draft through a function, with an undo step. */
    const mutate = (fn: (d: BuildingDesign, s: Storey) => void) => {
        const next = clone(design)
        fn(next, next.storeys[Math.min(k, next.storeys.length - 1)])
        commit(next)
    }

    const undo = () => {
        const prev = history[history.length - 1]
        if (!prev) {
            return
        }
        setFuture([design, ...future])
        setHistory(history.slice(0, -1))
        setDesign(prev)
        setSel(null)
        setDirty(true)
    }

    const redo = () => {
        const [next, ...rest] = future
        if (!next) {
            return
        }
        setHistory([...history, design])
        setFuture(rest)
        setDesign(next)
        setSel(null)
        setDirty(true)
    }

    const save = async () => {
        setBusy(true)
        try {
            const r = await client().put<{design: BuildingDesign}>(
                `/api/pipeline/buildings/${pand}/design`,
                design,
            )
            setDesign(r.design)
            setData({...data, design: r.design, stored: true})
            setDirty(false)
            notifier.success('Saved. The game shows it within half a minute.')
        } catch (e) {
            notifier.error(e instanceof ApiError ? e.message : 'Could not save the design')
        }
        setBusy(false)
    }

    const reset = async () => {
        setBusy(true)
        try {
            if (data.stored) {
                await client().delete(`/api/pipeline/buildings/${pand}/design`)
            }
            notifier.success('Back to the layout the game generates')
            await load()
        } catch (e) {
            notifier.error(e instanceof ApiError ? e.message : 'Could not reset the design')
        }
        setBusy(false)
    }

    keys.current = (e: KeyboardEvent) => {
        const typing = e.target instanceof HTMLInputElement || e.target instanceof HTMLTextAreaElement
        if (typing) {
            return
        }
        const mod = e.ctrlKey || e.metaKey
        if (mod && e.key.toLowerCase() === 'z') {
            e.preventDefault()
            if (e.shiftKey) {
                redo()
            } else {
                undo()
            }
        } else if (mod && e.key.toLowerCase() === 'y') {
            e.preventDefault()
            redo()
        } else if (e.key === 'Escape') {
            setPending(null)
            setSel(null)
            setFace(null)
        } else if ((e.key === 'Delete' || e.key === 'Backspace') && sel && canEdit) {
            e.preventDefault()
            const was = sel
            mutate((_, st) => removeSelected(st, was))
            setSel(null)
        } else if (!mod && canEdit) {
            const t = TOOLS.find((x) => x.key === e.key.toLowerCase())
            if (t) {
                pick(t.id)
            }
        }
    }

    const pick = (tid: Tool) => {
        setTool(tid)
        setPending(null)
        setSel(null)
        setFace(null)
    }

    // ── Pointer handling ───────────────────────────────────────────────────

    const toPlan = (e: PointerEvent | WheelEvent): P2 | null => {
        const el = svg.current
        const m = el?.getScreenCTM()
        if (!el || !m) {
            return null
        }
        const pt = new DOMPoint(e.clientX, e.clientY).matrixTransform(m.inverse())
        // SVG y runs down; the plan's runs north.
        return [pt.x, -pt.y]
    }

    const snapped = (p: P2) => snap(p, storey, outline, hitM)

    const hitTest = (p: P2): Selection | null => {
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
    const faceAt = (p: P2): P2[] | null => findRooms(storey).find((f) => pointInRing(f, p)) ?? null

    const tap = (raw: P2) => {
        if (!canEdit && tool !== 'select') {
            return
        }
        const p = snapped(raw)
        switch (tool) {
            case 'select': {
                const hit = hitTest(raw)
                setSel(hit)
                setFace(hit ? null : faceAt(raw))
                return
            }
            case 'wall':
                if (!pending) {
                    setPending(p)
                } else if (dist(pending, p) < 0.05) {
                    setPending(null)
                } else {
                    const from = pending
                    mutate((_, s) => {
                        addWall(s, from, p)
                    })
                    setPending(p)
                }
                return
            case 'door':
            case 'window': {
                const kind: OpeningKind = tool
                // Tried on a copy first: a tap that misses every wall must not
                // leave an undo step that undoes nothing.
                const next = clone(design)
                if (addOpening(next.storeys[k], raw, kind, hitM * 2)) {
                    commit(next)
                } else {
                    notifier.error(`Tap on a wall to put a ${kind} in it`)
                }
                return
            }
            case 'stair':
                if (!pending) {
                    setPending(p)
                } else {
                    const foot = pending
                    if (k === design.storeys.length - 1) {
                        notifier.error('Stairs climb to the floor above — add a storey first')
                    } else {
                        mutate((_, s) => {
                            addStair(s, foot, p)
                        })
                    }
                    setPending(null)
                }
                return
        }
    }

    const onPointerDown = (e: PointerEvent) => {
        ;(e.target as Element).setPointerCapture?.(e.pointerId)
        drag.current = {moved: false, start: [e.clientX, e.clientY], view}
        // With Select, a press on the selected wall's end or the selected
        // opening grabs it instead of the plan.
        const p = toPlan(e)
        if (!p || tool !== 'select' || !canEdit) {
            return
        }
        const grab = hitM * 1.5
        if (selWall) {
            const end = [selWall.a, selWall.b].find((c) => dist(c, p) < grab)
            if (end) {
                drag.current.edit = {before: design, from: end, target: {kind: 'corner'}}
            }
        } else if (selOpening && selOpeningWall) {
            if (dist(alongWall(selOpeningWall, selOpening.at), p) < Math.max(grab, selOpening.width / 2)) {
                drag.current.edit = {
                    before: design,
                    from: p,
                    target: {id: selOpening.id, kind: 'opening', wallId: selOpeningWall.id},
                }
            }
        }
    }

    const onPointerMove = (e: PointerEvent) => {
        const p = toPlan(e)
        if (p) {
            setCursor(snapped(p))
        }
        const d = drag.current
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
            const st = next.storeys[Math.min(k, next.storeys.length - 1)]
            const target = d.edit.target
            if (target.kind === 'corner') {
                const others: Storey = {...st, walls: st.walls.filter((w) => dist(w.a, d.edit!.from) > 0.03 && dist(w.b, d.edit!.from) > 0.03)}
                moveCorner(st, d.edit.from, snap(p, others, outline, hitM))
            } else {
                const wall = st.walls.find((w) => w.id === target.wallId)
                if (wall) {
                    slideOpening(wall, target.id, p)
                }
            }
            setDesign(next)
            return
        }
        // Any tool pans on a drag: on a phone there is no other way to move.
        setView({...d.view, cx: d.view.cx - dx / pxPerM, cy: d.view.cy + dy / pxPerM})
    }

    const onPointerUp = (e: PointerEvent) => {
        const d = drag.current
        drag.current = null
        if (d?.edit && d.moved) {
            commit(clone(design), d.edit.before)
            return
        }
        if (d && !d.moved) {
            const p = toPlan(e)
            if (p) {
                tap(p)
            }
        }
    }

    const onWheel = (e: WheelEvent) => {
        e.preventDefault()
        zoom(e.deltaY > 0 ? 1.15 : 1 / 1.15)
    }

    const zoom = (f: number) => setView({...view, w: Math.max(4, Math.min(200, view.w * f))})

    // ── Selection details ──────────────────────────────────────────────────

    const selWall = sel?.kind === 'wall' ? storey.walls.find((w) => w.id === sel.id) : undefined
    const selOpeningWall = sel?.kind === 'opening' ? storey.walls.find((w) => w.id === sel.wallId) : undefined
    const selOpening = selOpeningWall?.openings.find((o) => sel && o.id === sel.id)
    const selStair = sel?.kind === 'stair' ? storey.stairs.find((s) => s.id === sel.id) : undefined
    const selRoom = sel?.kind === 'room' ? storey.rooms.find((r) => r.id === sel.id) : undefined
    const faces = findRooms(storey)
    const unnamed = faces.filter((f) => !storey.rooms.some((r) => pointInRing(f, labelPoint(r.outline))))

    /** Edit the selected thing in a fresh draft, found again by id. */
    const editSel = (fn: (x: {opening?: Wall['openings'][0]; room?: Storey['rooms'][0]; stair?: Stair; wall?: Wall}) => void) =>
        mutate((_, s) => {
            if (!sel) {
                return
            }
            const wall = s.walls.find((w) => w.id === (sel.kind === 'opening' ? sel.wallId : sel.id))
            fn({
                opening: sel.kind === 'opening' ? wall?.openings.find((o) => o.id === sel.id) : undefined,
                room: s.rooms.find((r) => r.id === sel.id),
                stair: s.stairs.find((st) => st.id === sel.id),
                wall: sel.kind === 'wall' ? wall : undefined,
            })
        })

    const num = (label: string, value: number, step: number, set: (v: number) => void, min = 0, max = 100) => (
        <label class="field compact">
            <span>{label}</span>
            <input
                disabled={!canEdit}
                max={max}
                min={min}
                onChange={(e) => {
                    const v = Number((e.target as HTMLInputElement).value)
                    if (Number.isFinite(v)) {
                        set(Math.max(min, Math.min(max, v)))
                    }
                }}
                step={step}
                type="number"
                value={Number(value.toFixed(2))}
            />
        </label>
    )

    const issues = problems(design, data.facts?.height ?? null)
    const toolHint = TOOLS.find((t) => t.id === tool)?.hint

    // ── The plan ───────────────────────────────────────────────────────────

    const aspect = 0.75
    const h = view.w * aspect
    const vb = `${view.cx - view.w / 2} ${-view.cy - h / 2} ${view.w} ${h}`
    const pts = (r: P2[]) => r.map((p) => `${p[0]},${-p[1]}`).join(' ')
    const stroke = (m: number) => Math.max(m, 1.5 / pxPerM)

    const grid: JSX.Element[] = []
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

    const drawStair = (s: Stair, faint: boolean) => {
        const r = stairRect(s)
        const d = norm(s.dir) ?? ([1, 0] as P2)
        const isSel = sel?.kind === 'stair' && sel.id === s.id
        const treads = []
        for (let i = 1; i < 16; i++) {
            const t = (i / 16) * s.run
            const a: P2 = [r[0][0] + d[0] * t, r[0][1] + d[1] * t]
            const b: P2 = [r[3][0] + d[0] * t, r[3][1] + d[1] * t]
            treads.push(<line key={i} stroke-width={stroke(0.015)} x1={a[0]} x2={b[0]} y1={-a[1]} y2={-b[1]} />)
        }
        const mid0: P2 = [(r[0][0] + r[3][0]) / 2, (r[0][1] + r[3][1]) / 2]
        const mid1: P2 = [(r[1][0] + r[2][0]) / 2, (r[1][1] + r[2][1]) / 2]
        return (
            <g class={`stair ${faint ? 'faint' : ''} ${isSel ? 'is-selected' : ''}`} key={s.id}>
                <polygon points={pts(r)} stroke-width={stroke(0.03)} />
                {treads}
                <line class="arrow" marker-end="url(#arrow)" stroke-width={stroke(0.04)} x1={mid0[0]} x2={mid1[0]} y1={-mid0[1]} y2={-mid1[1]} />
            </g>
        )
    }

    const area = (r: P2[]) => Math.abs(r.reduce((a, p, i) => a + p[0] * r[(i + 1) % r.length][1] - r[(i + 1) % r.length][0] * p[1], 0) / 2)

    const owner = data.access.owned_by
    const [lat, lng] = design.origin

    return (
        <div class="designer">
            <div class="section-head">
                <h1>{owner ? `${owner}'s building` : 'Building'}</h1>
                <span class="count mono">{pand}</span>
                <span style="flex:1" />
                <a class="ghost-link" href={gamePlaceUrl(lat, lng)} rel="noreferrer" target="_blank">
                    View in the game
                </a>
            </div>

            {!canEdit && (
                <p class="hint">
                    You can look, not change: only the building's owner can.{' '}
                    {data.stored ? 'This is the design its owner saved.' : 'Nobody has designed it yet; this is what the game generates.'}
                </p>
            )}

            <nav class="tabs storeys">
                {design.storeys.map((s, i) => (
                    <button
                        class={i === k ? 'tab is-current' : 'tab'}
                        key={s.id || i}
                        onClick={() => {
                            setK(i)
                            setSel(null)
                            setPending(null)
                            setFace(null)
                        }}
                        type="button"
                    >
                        {s.name || `Storey ${i}`}
                    </button>
                ))}
                {canEdit && (
                    <button
                        class="tab"
                        onClick={() => {
                            mutate((d) => {
                                addStorey(d)
                            })
                            setK(design.storeys.length)
                        }}
                        title="Add a storey on top"
                        type="button"
                    >
                        + storey
                    </button>
                )}
            </nav>

            {canEdit && (
                <div class="toolbar">
                    {TOOLS.map((t) => (
                        <button
                            class={t.id === tool ? 'primary' : 'ghost'}
                            key={t.id}
                            onClick={() => pick(t.id)}
                            title={`${t.label} (${t.key.toUpperCase()})`}
                            type="button"
                        >
                            {t.label}
                        </button>
                    ))}
                    <span style="flex:1" />
                    {pending && (
                        <button
                            class="ghost"
                            onClick={() => {
                                setPending(null)
                            }}
                            type="button"
                        >
                            Done
                        </button>
                    )}
                    <button class="ghost" disabled={!history.length} onClick={undo} title="Undo" type="button">
                        ↶
                    </button>
                    <button class="ghost" disabled={!future.length} onClick={redo} title="Redo" type="button">
                        ↷
                    </button>
                </div>
            )}
            {canEdit && toolHint && <p class="hint tool-hint">{toolHint}</p>}

            <div class="plan-wrap">
                <svg
                    class={`plan tool-${tool}`}
                    onPointerDown={onPointerDown}
                    onPointerLeave={() => setCursor(null)}
                    onPointerMove={onPointerMove}
                    onPointerUp={onPointerUp}
                    onWheel={onWheel}
                    preserveAspectRatio="xMidYMid meet"
                    ref={svg}
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
                    {below?.stairs.map((s) => (
                        <polygon class="well" key={`well${s.id}`} points={pts(stairRect(s))} stroke-width={stroke(0.03)} />
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
                    {storey.stairs.map((s) => drawStair(s, false))}
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
                <div class="zoom">
                    <button class="ghost" onClick={() => zoom(1 / 1.4)} type="button">
                        +
                    </button>
                    <button class="ghost" onClick={() => zoom(1.4)} type="button">
                        −
                    </button>
                    <button class="ghost" onClick={() => setView(fit(data))} type="button">
                        ⤢
                    </button>
                </div>
            </div>

            <div class="card props">
                {selWall ? (
                    <>
                        <h3>{selWall.exterior ? 'Outer wall' : 'Wall'}</h3>
                        <p class="hint">
                            {wallLength(selWall).toFixed(2)} m long.
                            {canEdit && ' Drag either end to move it; the walls meeting it come along.'}
                        </p>
                        {!selWall.exterior &&
                            num('Thickness (m)', selWall.thickness, 0.01, (v) => editSel(({wall}) => wall && (wall.thickness = v)), 0.02, 1.5)}
                        {canEdit && (
                            <div class="row-actions wrap">
                                <button
                                    class="ghost"
                                    onClick={() => editSel(({wall}) => wall && spreadWindows(wall, suggestedWindows(wall)))}
                                    type="button"
                                >
                                    Space {suggestedWindows(selWall)} windows along it
                                </button>
                                {k + 1 < design.storeys.length && selWall.openings.some((o) => o.kind === 'window') && (
                                    <button
                                        class="ghost"
                                        onClick={() => {
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
                                        type="button"
                                    >
                                        Same windows on the floors above
                                    </button>
                                )}
                            </div>
                        )}
                    </>
                ) : selOpening && selOpeningWall ? (
                    <>
                        <h3>{selOpening.kind === 'door' ? 'Door' : 'Window'}</h3>
                        <div class="fields">
                            {num('Width (m)', selOpening.width, 0.05, (v) =>
                                editSel(({opening}) => {
                                    if (opening) {
                                        opening.width = Math.min(v, wallLength(selOpeningWall))
                                        opening.at = clampOpening(opening.at, opening.width, wallLength(selOpeningWall))
                                    }
                                }), 0.2, 6)}
                            {num('Height (m)', selOpening.height, 0.05, (v) => editSel(({opening}) => opening && (opening.height = v)), 0.2, 10)}
                            {num('Sill (m)', selOpening.sill, 0.05, (v) => editSel(({opening}) => opening && (opening.sill = v)), 0, 5)}
                            {num('From the wall end (m)', selOpening.at, 0.05, (v) =>
                                editSel(({opening}) => opening && (opening.at = clampOpening(v, opening.width, wallLength(selOpeningWall)))), 0, 100)}
                        </div>
                        <p class="hint">
                            {selOpening.kind === 'door'
                                ? 'A door with a sill above 30 cm cannot be walked through.'
                                : 'A window is a hole you can see through, not walk through.'}
                        </p>
                    </>
                ) : selStair ? (
                    <>
                        <h3>Stairs</h3>
                        <div class="fields">
                            {num('Width (m)', selStair.width, 0.05, (v) => editSel(({stair}) => stair && (stair.width = v)), 0.5, 5)}
                            {num('Length (m)', selStair.run, 0.1, (v) => editSel(({stair}) => stair && (stair.run = v)), 1, 15)}
                        </div>
                        {canEdit && (
                            <div class="row-actions">
                                <button class="ghost" onClick={() => editSel(({stair}) => stair && flipStair(stair))} type="button">
                                    Turn round
                                </button>
                            </div>
                        )}
                    </>
                ) : selRoom ? (
                    <>
                        <h3>Room</h3>
                        <label class="field compact">
                            <span>Name</span>
                            <input
                                disabled={!canEdit}
                                maxLength={120}
                                onChange={(e) => {
                                    const v = (e.target as HTMLInputElement).value
                                    editSel(({room}) => room && (room.name = v))
                                }}
                                type="text"
                                value={selRoom.name}
                            />
                        </label>
                        <p class="hint">{area(selRoom.outline).toFixed(1)} m². The name shows in the game when somebody stands in it.</p>
                    </>
                ) : face ? (
                    <>
                        <h3>Room</h3>
                        <label class="field compact">
                            <span>Name</span>
                            <input
                                disabled={!canEdit}
                                maxLength={120}
                                onChange={(e) => {
                                    const v = (e.target as HTMLInputElement).value.trim()
                                    if (!v) {
                                        return
                                    }
                                    const at = labelPoint(face)
                                    let id: null | string = null
                                    mutate((_, st) => {
                                        id = nameRoomAt(st, at, v)
                                    })
                                    if (id) {
                                        setSel({id, kind: 'room'})
                                        setFace(null)
                                    }
                                }}
                                placeholder="Keuken, Woonkamer…"
                                type="text"
                                value=""
                            />
                        </label>
                        <p class="hint">{area(face).toFixed(1)} m², not named yet.</p>
                    </>
                ) : (
                    <>
                        <h3>{storey.name}</h3>
                        <div class="fields">
                            <label class="field compact">
                                <span>Name</span>
                                <input
                                    disabled={!canEdit}
                                    maxLength={120}
                                    onChange={(e) => {
                                        const v = (e.target as HTMLInputElement).value
                                        mutate((_, s) => {
                                            s.name = v
                                        })
                                    }}
                                    type="text"
                                    value={storey.name}
                                />
                            </label>
                            {num('Floor to floor (m)', storey.height, 0.1, (v) =>
                                mutate((d, s) => {
                                    s.height = v
                                    restack(d)
                                }), 2, 12)}
                        </div>
                        <p class="hint">
                            {storey.walls.length} walls, {storey.walls.reduce((a, w) => a + w.openings.length, 0)} doors and
                            windows, {storey.stairs.length} stairs. Floor at {storey.elevation.toFixed(2)} m.
                        </p>
                        {canEdit && k === design.storeys.length - 1 && design.storeys.length > 1 && (
                            <div class="row-actions">
                                <button
                                    class="ghost"
                                    onClick={() => {
                                        mutate((d) => {
                                            removeTopStorey(d)
                                        })
                                        setK(k - 1)
                                    }}
                                    type="button"
                                >
                                    Remove this storey
                                </button>
                            </div>
                        )}
                    </>
                )}
                {canEdit && sel && (
                    <div class="row-actions">
                        <button
                            class="danger"
                            onClick={() => {
                                const s = sel
                                mutate((_, st) => removeSelected(st, s))
                                setSel(null)
                            }}
                            type="button"
                        >
                            Delete
                        </button>
                    </div>
                )}
            </div>

            {issues.length > 0 && (
                <div class="card problems">
                    {issues.map((p) => (
                        <p class="hint" key={p}>
                            {p}
                        </p>
                    ))}
                </div>
            )}

            <p class="hint">
                In the game, walk in — or tap the building and choose Look inside — and pick a floor on
                the right. The floor names are yours, and a room's name shows when somebody stands in it.
            </p>

            {canEdit && (
                <div class="row-actions save-bar">
                    <button class="ghost" disabled={busy} onClick={() => void reset()} type="button">
                        {data.stored ? 'Back to generated' : 'Start over'}
                    </button>
                    <span style="flex:1" />
                    <span class="hint">{dirty ? 'Unsaved changes' : data.stored ? 'Saved' : 'Not saved yet'}</span>
                    <button class="primary" disabled={busy || !dirty} onClick={() => void save()} type="button">
                        {busy ? 'Saving…' : 'Save'}
                    </button>
                </div>
            )}
        </div>
    )
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

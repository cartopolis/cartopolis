#!/usr/bin/env bun
// The app mark — the brand pin standing on Groningen's street plan — made from
// OpenStreetMap, and every copy of it written from one run.
//
//   bun tools/brand-mark.ts
//
// The mark exists in five notations, and they used to be kept in step by hand
// (the old tower cluster's own comment said "change one, change all three"):
//
//   assets/icon/cartopolis.svg          the dark mark, source of the files below
//   assets/icon/cartopolis-light.svg    the light-theme variant (the web UI)
//   assets/icon/cartopolis.png / .ico   the OS icons (256 px; .ico 256→16)
//   crates/cartopolis/src/systems/gui/mark_table.rs
//                                       the same lines as a table, which
//                                       `theme::paint_mark` draws with egui
//   crates/cartopolis/index.html        the web loader's inline copy, between
//                                       the `mark:begin` / `mark:end` comments
//
// Here the street plan is *data*, a few hundred polylines, so nothing can be
// kept in step by hand any more: rerun this instead.
//
// **The drawing needs no clip and no mask**, on purpose. egui has neither, and
// the in-app mark has to be the same picture as the icon, so the two cuts are
// geometry: every line is cut where it would leave the rounded tile, and where
// it would come within `HALO` of the pin — that gap is what makes the pin look
// cut out of the map. Inks are opaque (a translucent cream pre-mixed over the
// tile) for the same reason: two translucent egui strokes double up where they
// cross, and an SVG path does not.
//
// The PNGs are rendered with resvg (Bun installs it on first run); the .ico is
// packed by ImageMagick (`magick` or `convert`).

import { $ } from 'bun'
import path from 'node:path'
import { Resvg } from '@resvg/resvg-js@2.6.2'

const root = path.resolve(import.meta.dir, '..')
const OVERPASS = process.env.ATLAS_OVERPASS_URL ?? 'https://overpass.cartopolis.org/api/interpreter'

/** Groningen's centre — the Grote Markt a little above middle. */
const CENTRE = [53.2168, 6.5668]
/** Metres across the 512-unit tile. */
const SPAN_M = 1500
const UNIT = 512
const CORNER = 115

/** Stroke width in tile units by highway class; smaller roads are left out. */
const WIDTHS: Record<string, number> = {
    primary: 6.6, secondary: 6, tertiary: 5.4,
    unclassified: 4.2, residential: 4.2,
    living_street: 3.6, pedestrian: 3.6,
}
const WATER_W = 14

/** The brand pin (the old mark's teardrop), in its own units with the tip at 0,0. */
const PIN = [
    [[0, -65], [-14.95, -65], [-27.3, -53.95], [-27.3, -39]],
    [[-27.3, -39], [-27.3, -20.15], [0, 0], [0, 0]],
    [[0, 0], [0, 0], [27.3, -20.15], [27.3, -39]],
    [[27.3, -39], [27.3, -53.95], [14.95, -65], [0, -65]],
] as const
const PIN_AT = [256, 410]
const PIN_SCALE = 4.6
const PIN_DOT = [0, -39]
const PIN_DOT_R = 9
const PIN_KEYLINE = 3
/** Street plan cleared this far beyond the pin's edge, in tile units. */
const HALO = 6 * PIN_SCALE

type Pt = [number, number]
const INKS = {
    dark: { tileTop: '#2e424e', tileBottom: '#1B2830', street: '#8C9295', water: '#626D72' },
    light: { tileTop: '#F2EFEA', tileBottom: '#F2EFEA', street: '#B7BBBB', water: '#CFD0CE' },
}
const BRAND = '#FD6012'
const CREAM = '#F2EFEA'

// ─── Geometry ───────────────────────────────────────────────────────────────

const k = Math.cos((CENTRE[0] * Math.PI) / 180)
const project = (lat: number, lon: number): Pt => [
    UNIT / 2 + ((lon - CENTRE[1]) * 111320 * k) / SPAN_M * UNIT,
    UNIT / 2 - ((lat - CENTRE[0]) * 111320) / SPAN_M * UNIT,
]

const pinOutline: Pt[] = PIN.flatMap(([a, b, c, d]) =>
    Array.from({ length: 12 }, (_, i) => {
        const t = i / 12, u = 1 - t
        const x = u ** 3 * a[0] + 3 * u * u * t * b[0] + 3 * u * t * t * c[0] + t ** 3 * d[0]
        const y = u ** 3 * a[1] + 3 * u * u * t * b[1] + 3 * u * t * t * c[1] + t ** 3 * d[1]
        return [PIN_AT[0] + x * PIN_SCALE, PIN_AT[1] + y * PIN_SCALE] as Pt
    }),
)

function insidePolygon(p: Pt, poly: Pt[]) {
    let inside = false
    for (let i = 0, j = poly.length - 1; i < poly.length; j = i++) {
        const [xi, yi] = poly[i], [xj, yj] = poly[j]
        if (yi > p[1] !== yj > p[1] && p[0] < ((xj - xi) * (p[1] - yi)) / (yj - yi) + xi) inside = !inside
    }
    return inside
}
function distToSegment(p: Pt, a: Pt, b: Pt) {
    const [dx, dy] = [b[0] - a[0], b[1] - a[1]]
    const t = Math.max(0, Math.min(1, ((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / (dx * dx + dy * dy || 1)))
    return Math.hypot(p[0] - a[0] - t * dx, p[1] - a[1] - t * dy)
}
function distToPin(p: Pt) {
    if (insidePolygon(p, pinOutline)) return 0
    let d = Infinity
    for (let i = 0; i < pinOutline.length; i++) d = Math.min(d, distToSegment(p, pinOutline[i], pinOutline[(i + 1) % pinOutline.length]))
    return d
}
/** Inside the rounded tile, shrunk by `inset`. */
function insideTile([x, y]: Pt, inset: number) {
    const lo = inset, hi = UNIT - inset, r = CORNER - inset
    if (x < lo || x > hi || y < lo || y > hi) return false
    const cx = Math.min(Math.max(x, lo + r), hi - r), cy = Math.min(Math.max(y, lo + r), hi - r)
    return Math.hypot(x - cx, y - cy) <= r
}

function simplify(pts: Pt[], eps: number): Pt[] {
    if (pts.length < 3) return pts
    const [a, b] = [pts[0], pts[pts.length - 1]]
    let far = 0, at = 0
    for (let i = 1; i < pts.length - 1; i++) {
        const d = distToSegment(pts[i], a, b)
        if (d > far) [far, at] = [d, i]
    }
    if (far <= eps) return [a, b]
    return [...simplify(pts.slice(0, at + 1), eps).slice(0, -1), ...simplify(pts.slice(at), eps)]
}

/** A polyline cut to what may be drawn of it: inside the tile, clear of the pin. */
function cut(line: Pt[], width: number): Pt[][] {
    const keep = (p: Pt) => insideTile(p, width / 2) && distToPin(p) > HALO + width / 2
    const runs: Pt[][] = []
    let run: Pt[] = []
    for (let i = 0; i + 1 < line.length; i++) {
        const [a, b] = [line[i], line[i + 1]]
        const steps = Math.max(1, Math.ceil(Math.hypot(b[0] - a[0], b[1] - a[1]) / 1.5))
        for (let s = i === 0 ? 0 : 1; s <= steps; s++) {
            const p: Pt = [a[0] + ((b[0] - a[0]) * s) / steps, a[1] + ((b[1] - a[1]) * s) / steps]
            if (keep(p)) run.push(p)
            else if (run.length) (runs.push(run), (run = []))
        }
    }
    if (run.length) runs.push(run)
    return runs
        .map((r) => simplify(r, 0.6).map(([x, y]) => [Math.round(x), Math.round(y)] as Pt))
        .filter((r) => r.length > 1 && Math.hypot(r[0][0] - r.at(-1)![0], r[0][1] - r.at(-1)![1]) >= 3)
}

// ─── Data ───────────────────────────────────────────────────────────────────

const half = [(SPAN_M / 2 + 100) / 111320, (SPAN_M / 2 + 100) / (111320 * k)]
const bbox = `(${CENTRE[0] - half[0]},${CENTRE[1] - half[1]},${CENTRE[0] + half[0]},${CENTRE[1] + half[1]})`
const query = `[out:json][timeout:60];(way["highway"~"^(${Object.keys(WIDTHS).join('|')})$"]${bbox};way["waterway"~"^(canal|river)$"]${bbox};);out geom tags;`
const res = await fetch(OVERPASS, { method: 'POST', body: new URLSearchParams({ data: query }) })
if (!res.ok) throw new Error(`Overpass answered ${res.status}`)
const osm = (await res.json()) as { elements: { tags?: Record<string, string>; geometry?: { lat: number; lon: number }[] }[] }

type Line = { width: number; ink: 'water' | 'street'; points: Pt[] }
const lines: Line[] = []
for (const el of osm.elements) {
    if (!el.geometry) continue
    const water = !!el.tags?.waterway
    const width = water ? WATER_W : WIDTHS[el.tags?.highway ?? '']
    if (!width) continue
    for (const points of cut(el.geometry.map((g) => project(g.lat, g.lon)), width)) {
        lines.push({ width, ink: water ? 'water' : 'street', points })
    }
}
// Paint order: water under streets, and the wider streets over the narrower,
// so a main road runs unbroken through the side streets meeting it.
lines.sort((a, b) => (a.ink === b.ink ? a.width - b.width : a.ink === 'water' ? -1 : 1))

// ─── Writers ────────────────────────────────────────────────────────────────

const pathOf = (pts: Pt[]) => pts.map(([x, y], i) => `${i ? 'L' : 'M'}${x} ${y}`).join('')
const f = (n: number) => +n.toFixed(2)

function svg(theme: keyof typeof INKS, opts: { standalone: boolean }) {
    const ink = INKS[theme]
    const groups = new Map<string, string>()
    for (const l of lines) {
        const key = `${l.ink === 'water' ? ink.water : ink.street} ${l.width}`
        groups.set(key, (groups.get(key) ?? '') + pathOf(l.points))
    }
    const tile = ink.tileTop === ink.tileBottom
        ? `<rect width="512" height="512" rx="${CORNER}" fill="${ink.tileTop}"/>`
        : `<defs><linearGradient id="mark-bg" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="${ink.tileTop}"/><stop offset="1" stop-color="${ink.tileBottom}"/></linearGradient></defs><rect width="512" height="512" rx="${CORNER}" fill="url(#mark-bg)"/>`
    const plan = [...groups].map(([key, d]) => {
        const [colour, width] = key.split(' ')
        return `<path d="${d}" fill="none" stroke="${colour}" stroke-width="${width}" stroke-linecap="round" stroke-linejoin="round"/>`
    }).join('\n')
    const pin = `<path d="${pathOf(pinOutline.map(([x, y]) => [f(x), f(y)] as Pt))}Z" fill="${BRAND}" stroke="${CREAM}" stroke-width="${f(PIN_KEYLINE * PIN_SCALE)}" stroke-linejoin="round"/>` +
        `<circle cx="${PIN_AT[0] + PIN_DOT[0] * PIN_SCALE}" cy="${f(PIN_AT[1] + PIN_DOT[1] * PIN_SCALE)}" r="${f(PIN_DOT_R * PIN_SCALE)}" fill="${CREAM}"/>`
    const head = opts.standalone
        ? `<svg xmlns="http://www.w3.org/2000/svg" width="512" height="512" viewBox="0 0 512 512">\n<!-- The Cartopolis mark (${theme}): the brand pin on Groningen's centre, streets and canals from OpenStreetMap (© OpenStreetMap contributors, ODbL). Generated by tools/brand-mark.ts; do not edit by hand. -->\n`
        : ''
    return `${head}${tile}\n${plan}\n${pin}${opts.standalone ? '\n</svg>\n' : ''}`
}

const icon = path.join(root, 'assets/icon')
await Bun.write(path.join(icon, 'cartopolis.svg'), svg('dark', { standalone: true }))
await Bun.write(path.join(icon, 'cartopolis-light.svg'), svg('light', { standalone: true }))

const sizes = [256, 128, 64, 48, 32, 16]
const tmp = (await $`mktemp -d`.text()).trim()
for (const px of sizes) {
    const png = new Resvg(svg('dark', { standalone: true }), { fitTo: { mode: 'width', value: px } }).render().asPng()
    await Bun.write(path.join(tmp, `${px}.png`), png)
}
await $`cp ${path.join(tmp, '256.png')} ${path.join(icon, 'cartopolis.png')}`
const magick = (await $`command -v magick`.nothrow().quiet()).exitCode === 0 ? 'magick' : 'convert'
await $`${magick} ${sizes.map((px) => path.join(tmp, `${px}.png`))} ${path.join(icon, 'cartopolis.ico')}`
await $`rm -r ${tmp}`

// The egui table.
const pts = (p: Pt[]) => p.map(([x, y]) => `[${x}.0, ${y}.0]`).join(', ')
const rust = `// Generated by \`tools/brand-mark.ts\` — do not edit by hand; rerun it.
//
// The mark's street plan and pin outline in the icon's 512-unit square, already
// cut to the tile and clear of the pin (see the tool for why that is geometry
// and not a clip). Included into \`theme::mark\`.

/// Every line of the street plan, in paint order.
pub const LINES: &[Line] = &[
${lines.map((l) => `    Line { width: ${l.width.toFixed(1)}, ink: Ink::${l.ink === 'water' ? 'Water' : 'Street'}, points: &[${pts(l.points)}] },`).join('\n')}
];

/// The pin's teardrop, sampled from the icon's curve. Convex, tip at the bottom.
pub const PIN_OUTLINE: [[f32; 2]; ${pinOutline.length}] = [${pinOutline.map(([x, y]) => `[${x.toFixed(1)}, ${y.toFixed(1)}]`).join(', ')}];
/// The pin's dot: centre and radius.
pub const PIN_DOT_C: [f32; 2] = [${(PIN_AT[0] + PIN_DOT[0] * PIN_SCALE).toFixed(1)}, ${(PIN_AT[1] + PIN_DOT[1] * PIN_SCALE).toFixed(1)}];
pub const PIN_DOT_R: f32 = ${(PIN_DOT_R * PIN_SCALE).toFixed(1)};
/// The cream keyline around the pin, centred on its edge.
pub const PIN_KEYLINE: f32 = ${(PIN_KEYLINE * PIN_SCALE).toFixed(1)};
/// How far the street plan stays clear of the pin's edge. Only the tests read
/// it: the cut is already in the lines above.
#[cfg(test)]
pub const PIN_HALO: f32 = ${HALO.toFixed(1)};
`
await Bun.write(path.join(root, 'crates/cartopolis/src/systems/gui/mark_table.rs'), rust)

// The web loader's inline copy.
const loader = path.join(root, 'crates/cartopolis/index.html')
const html = await Bun.file(loader).text()
const marked = /(<!-- mark:begin -->)[\s\S]*?(<!-- mark:end -->)/
if (!marked.test(html)) throw new Error('index.html has no mark:begin / mark:end region')
await Bun.write(loader, html.replace(marked, (_, a, b) => `${a}\n${svg('dark', { standalone: false })}\n${b}`))

const count = lines.reduce((n, l) => n + l.points.length, 0)
console.log(`${lines.length} lines, ${count} points; wrote the SVGs, PNG, ICO, mark_table.rs and the loader`)

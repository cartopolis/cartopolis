#!/usr/bin/env bun
// A shot manifest that photographs every tunnel mouth an atlas run built, and
// every flaw its audit found, placed from the data rather than by hand.
//
//   bun tools/structure_shots.ts site/roads/mouths.json > /tmp/mouths.toml
//   cargo run -p cartopolis -- --shots /tmp/mouths.toml
//
// `roads/mouths.json` is written by atlas's terrain grading
// (`pipeline/terrain_grade.rs`, `write_mouths`): every place a tunnel's roof
// stops with sky beyond it, the bearing out of the tunnel and how deep the road
// is there; and every flaw `tunnels::audit` found — a street an opening cuts
// through, a roof lower than any underpass, an open end dropping into nothing.
// Each mouth gets two cameras out along its open side, looking back into it —
// close, at a person's eye over the cutting, and wide — and each flaw one
// camera looking at it; the manifest asks for the aerial reference beside every
// capture (`--aerial-reference`), so each render comes with the photograph of
// the same ground from the same camera.
//
// **This is how a tunnel change is checked, not by aiming at one tunnel.** A
// rule tuned until one well-known portal looks right can be wrong at every
// other; a gallery of every mouth in a city, each beside its photograph, is what
// shows whether the rules hold. Mouths within `JOIN_M` of each other facing the
// same way — the two bores of a dual carriageway — share their cameras.
//
// **A camera is placed where it can see.** Aimed blindly 35 m out, a camera
// landed inside a tree crown, or inside the building beside a garage ramp, and
// photographed nothing. Each camera tries a fixed ladder of positions — the
// bearing out first, then turned either way, nearer and further — and takes the
// first that stands in no building footprint (the BAG cells), within `TREE_M`
// of no surveyed tree (the BGT cells), and has no footprint across its line of
// sight. The ladder is fixed, so the same data always gives the same shots.
//
// Flags: `--out-dir DIR` (default /tmp/mouths), `--tile-url URL`, `--limit N`,
// `--size WxH`, `--data URL` (where the BAG and BGT cells are served; default
// https://3dbag.cartopolis.org), `--no-flaws`.

const args = process.argv.slice(2);
const flag = (name: string, fallback: string) => {
  const i = args.indexOf(name);
  return i >= 0 && args[i + 1] ? args[i + 1] : fallback;
};
const input = args.find((a, i) => !a.startsWith("--") && !args[i - 1]?.startsWith("--"));
if (!input) {
  console.error("usage: bun tools/structure_shots.ts <roads/mouths.json> [--out-dir DIR] [--limit N]");
  process.exit(2);
}
const outDir = flag("--out-dir", "/tmp/mouths");
const tileUrl = flag("--tile-url", "https://tiles.cartopolis.org/tiles/osm/{z}/{x}/{y}");
const dataUrl = flag("--data", "https://3dbag.cartopolis.org");
const limit = Number(flag("--limit", "1000"));
const size = flag("--size", "900x1600");
const withFlaws = !args.includes("--no-flaws");

type Mouth = { lat: number; lng: number; out_deg: number; depth: number };
type Flaw = { kind: string; lat: number; lng: number };
const data = (await Bun.file(input).json()) as {
  name?: string;
  mouths: Mouth[];
  unbuilt: unknown[];
  flaws?: Flaw[];
};

/// Mouths closer than this, facing within `JOIN_DEG` of each other, are one
/// portal, metres.
const JOIN_M = 40;
const JOIN_DEG = 35;
/// How near a surveyed tree a camera may stand, metres — a crown's radius.
const TREE_M = 6;

const M_PER_DEG = 111_320;
const cosLat = (lat: number) => Math.cos((lat * Math.PI) / 180);
const metres = (a: { lat: number; lng: number }, b: { lat: number; lng: number }) =>
  Math.hypot((a.lat - b.lat) * M_PER_DEG, (a.lng - b.lng) * M_PER_DEG * cosLat(a.lat));
const turn = (a: number, b: number) => Math.abs(((a - b + 540) % 360) - 180);

/// The point `d` metres from (lat, lng) on compass bearing `deg`.
const ahead = (lat: number, lng: number, deg: number, d: number): [number, number] => {
  const r = (deg * Math.PI) / 180;
  return [lat + (Math.cos(r) * d) / M_PER_DEG, lng + (Math.sin(r) * d) / (M_PER_DEG * cosLat(lat))];
};
const f6 = (v: number) => v.toFixed(6);

// ─── What a camera must not stand in or look through ─────────────────────────

const tileOf = (lat: number, lng: number): [number, number] => {
  const n = 2 ** 14;
  const r = (lat * Math.PI) / 180;
  return [
    Math.floor(((lng + 180) / 360) * n),
    Math.floor(((1 - Math.log(Math.tan(r) + 1 / Math.cos(r)) / Math.PI) / 2) * n),
  ];
};
const footprints = new Map<string, [number, number][][]>();
const trees = new Map<string, [number, number][]>();
const getJson = async (url: string) => {
  try {
    const r = await fetch(url);
    return r.ok ? await r.json() : null;
  } catch {
    return null;
  }
};
/// The BAG outlines and BGT trees of every cell around a point, fetched once.
const obstacles = async (lat: number, lng: number) => {
  const [tx, ty] = tileOf(lat, lng);
  const fps: [number, number][][] = [];
  const ts: [number, number][] = [];
  for (const dx of [-1, 0, 1])
    for (const dy of [-1, 0, 1]) {
      const key = `${tx + dx}/${ty + dy}`;
      if (!footprints.has(key)) {
        const bag = await getJson(`${dataUrl}/bag/14/${key}.json`);
        footprints.set(key, (bag?.buildings ?? []).map((b: { fp?: [number, number][] }) => b.fp).filter(Boolean));
        const bgt = await getJson(`${dataUrl}/bgt/14/${key}.json`);
        trees.set(key, bgt?.trees ?? []);
      }
      fps.push(...footprints.get(key)!);
      ts.push(...trees.get(key)!);
    }
  return { fps, ts };
};
const inside = (p: [number, number], ring: [number, number][]) => {
  let c = false;
  for (let i = 0, j = ring.length - 1; i < ring.length; j = i++) {
    const [yi, xi] = ring[i];
    const [yj, xj] = ring[j];
    if (yi > p[0] !== yj > p[0] && p[1] < ((xj - xi) * (p[0] - yi)) / (yj - yi) + xi) c = !c;
  }
  return c;
};
/// Whether a camera at `cam` sees `aim`: it stands in no footprint and near no
/// tree, and no footprint lies on the line between (sampled every two metres,
/// the last five short of the aim, where a mouth may sit under its building).
const clear = (cam: [number, number], aim: [number, number], o: { fps: [number, number][][]; ts: [number, number][] }) => {
  const at = { lat: cam[0], lng: cam[1] };
  if (o.ts.some(([la, ln]) => metres(at, { lat: la, lng: ln }) < TREE_M)) return false;
  const d = metres(at, { lat: aim[0], lng: aim[1] });
  const near = o.fps.filter((fp) => fp.some(([la, ln]) => metres(at, { lat: la, lng: ln }) < d + 80));
  for (let s = 0; s <= d - 5; s += 2) {
    const t = s / d;
    const p: [number, number] = [cam[0] + (aim[0] - cam[0]) * t, cam[1] + (aim[1] - cam[1]) * t];
    if (near.some((fp) => inside(p, fp))) return false;
  }
  return true;
};
/// The first position on the ladder that sees the aim, or the plain one.
const place = async (lat: number, lng: number, bearing: number, dist: number) => {
  const o = await obstacles(lat, lng);
  for (const turnBy of [0, 20, -20, 40, -40, 70, -70])
    for (const scale of [1, 0.75, 1.3]) {
      const cam = ahead(lat, lng, bearing + turnBy, dist * scale);
      if (clear(cam, [lat, lng], o)) return cam;
    }
  return ahead(lat, lng, bearing, dist);
};

// ─── The manifest ────────────────────────────────────────────────────────────

const portals: Mouth[][] = [];
for (const m of data.mouths) {
  const p = portals.find((g) => metres(g[0], m) < JOIN_M && turn(g[0].out_deg, m.out_deg) < JOIN_DEG);
  if (p) p.push(m);
  else portals.push([m]);
}
const flaws = withFlaws ? (data.flaws ?? []) : [];

let toml = `# Generated by tools/structure_shots.ts from ${input}
# ${data.mouths.length} mouths at ${portals.length} portals; ${data.unbuilt.length} tunnels built as no structure; ${flaws.length} flaws.
tile_url = "${tileUrl}"
size = "${size}"
out_dir = "${outDir}"
hide_ui = true
mode = "free"
time = 15.0
fog = 0.0
clouds = 0.0
rain = 0.0
settle = 3.0
wait = 240.0
aerial_reference = true
`;
const shot = (name: string, description: string, cam: [number, number], alt: number, aim: [number, number]) => {
  toml += `
[[shot]]
name = "${name}"
description = "${description}"
at = [${f6(cam[0])}, ${f6(cam[1])}, ${alt}]
look_at = [${f6(aim[0])}, ${f6(aim[1])}]
`;
};
for (const [i, g] of portals.slice(0, limit).entries()) {
  const lat = g.reduce((s, m) => s + m.lat, 0) / g.length;
  const lng = g.reduce((s, m) => s + m.lng, 0) / g.length;
  // The mean bearing out, on the circle.
  const out =
    (Math.atan2(
      g.reduce((s, m) => s + Math.sin((m.out_deg * Math.PI) / 180), 0),
      g.reduce((s, m) => s + Math.cos((m.out_deg * Math.PI) / 180), 0),
    ) *
      180) /
    Math.PI;
  const depth = Math.max(...g.map((m) => m.depth));
  const n = String(i).padStart(2, "0");
  for (const [kind, dist, alt] of [
    ["close", 35, 12],
    ["wide", 90, 45],
  ] as const) {
    const cam = await place(lat, lng, out, dist);
    shot(
      `mouth-${n}-${kind}`,
      `Tunnel mouth ${n}: ${g.length} bore(s), road ${depth.toFixed(1)} m under its land, looking back in from ${dist} m out`,
      cam,
      alt,
      [lat, lng],
    );
  }
}
for (const [i, f] of flaws.slice(0, limit).entries()) {
  // No bearing belongs to a flaw, so the ladder starts from the south-west,
  // looking north-east with the light behind.
  const cam = await place(f.lat, f.lng, 225, 40);
  shot(`flaw-${String(i).padStart(2, "0")}-${f.kind}`, `Audit flaw ${i}: ${f.kind}`, cam, 25, [f.lat, f.lng]);
}
process.stdout.write(toml);
console.error(
  `${data.mouths.length} mouths, ${portals.length} portals, ${data.unbuilt.length} unbuilt tunnels, ${flaws.length} flaws`,
);

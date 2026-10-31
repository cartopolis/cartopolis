// The hex board from the street to the globe: H3 cells whose resolution follows altitude,
// drawn with the client's nadir camera (45° vertical fov), as macro-view/gen-globe.ts does.
// Data: ne_50m_land/countries/lakes, lc_z3.geojson (WorldCover via our tile server), from the macro-view data dir.
// Usage (in that dir): OUT=<dir> bun gen-globe.ts
import { geoPath } from "d3-geo";
import { geoSatellite } from "d3-geo-projection";
import * as h3 from "h3-js";
const outD = process.env.OUT ?? ".";
const J = async (f: string) => Bun.file(f).json();
const [land, lakes, countries, places, lc3] = await Promise.all([
  J("ne_50m_land.geojson"), J("ne_50m_lakes.geojson"), J("ne_50m_admin_0_countries.geojson"),
  J("ne_10m_populated_places_simple.geojson"), J("lc_z3.geojson")]);
const LC: Record<string, string> = { farmland: "#979262", grassland: "#6f8a55", forest: "#3e5a3c", residential: "#8a817a",
  marsh: "#5f7c6e", heath: "#86727f", scrub: "#7b8559", bare_rock: "#cdc29c", swamp: "#5f7c6e", glacier: "#e8eef0" };
const ER = 6371;
const r1 = (n: number) => Math.round(n * 10) / 10;

// --- point lookups --------------------------------------------------------
function inRing(r: number[][], x: number, y: number) { let c = false; for (let i = 0, j = r.length - 1; i < r.length; j = i++) { const [xi, yi] = r[i], [xj, yj] = r[j]; if (yi > y !== yj > y && x < ((xj - xi) * (y - yi)) / (yj - yi) + xi) c = !c; } return c; }
const polys = (g: any): number[][][][] => (g.type === "Polygon" ? [g.coordinates] : g.type === "MultiPolygon" ? g.coordinates : []);
const inPoly = (p: number[][][], x: number, y: number) => inRing(p[0], x, y) && !p.slice(1).some((h) => inRing(h, x, y));
type P = { bb: number[]; p: number[][][]; kind: string };
const bb = (p: number[][][]) => { let w = 1e9, s = 1e9, e = -1e9, n = -1e9; for (const [x, y] of p[0]) { if (x < w) w = x; if (x > e) e = x; if (y < s) s = y; if (y > n) n = y; } return [w, s, e, n]; };
const landP: P[] = land.features.flatMap((f: any) => polys(f.geometry).map((p) => ({ bb: bb(p), p, kind: "land" })));
const lcP: P[] = lc3.features.flatMap((f: any) => polys(f.geometry).map((p) => ({ bb: bb(p), p, kind: f.properties.kind })));
const hit = (ps: P[], x: number, y: number) => { let k: string | null = null; for (const q of ps) { if (x < q.bb[0] || x > q.bb[2] || y < q.bb[1] || y > q.bb[3]) continue; if (inPoly(q.p, x, y)) k = q.kind; } return k; };
const isLand = (lon: number, lat: number) => hit(landP, lon, lat) !== null;
const LCCOVER = [-45.9, -40.7, 90.9, 82.8];
function cellClass(c: string): string | null {
  // majority over the centre and the seven children's centres
  const pts = [h3.cellToLatLng(c), ...h3.cellToChildren(c, Math.min(h3.getResolution(c) + 1, 15)).map((x) => h3.cellToLatLng(x))];
  const votes: Record<string, number> = {}; let landN = 0;
  for (const [lat, lon] of pts) {
    if (!isLand(lon, lat)) continue; landN++;
    let k = lon >= LCCOVER[0] && lon <= LCCOVER[2] && lat >= LCCOVER[1] && lat <= LCCOVER[3] ? hit(lcP, lon, lat) : null;
    if (k === "water") continue;
    k ??= Math.abs(lat) > 60 ? "heath" : Math.abs(lat) < 23 ? "scrub" : "grassland";
    votes[k] = (votes[k] ?? 0) + 1;
  }
  if (landN * 2 < pts.length) return null;
  return Object.entries(votes).sort((a, b) => b[1] - a[1])[0]?.[0] ?? "grassland";
}
const classCache = new Map<string, string | null>();
const cls = (c: string) => { if (!classCache.has(c)) classCache.set(c, cellClass(c)); return classCache.get(c)!; };

// --- the camera -----------------------------------------------------------
function camera(altKm: number, W: number, H: number, lon: number, lat: number) {
  const Pd = (ER + altKm) / ER;
  const clip = (Math.acos(1 / Pd) * 180) / Math.PI;
  const proj = geoSatellite().distance(Pd).tilt(0).rotate([-lon, -lat]).translate([W / 2, H / 2]).scale(1).clipAngle(clip - 1e-6).precision(0.3);
  const th = (0.2 * Math.PI) / 180;
  const alpha = Math.atan((ER * Math.sin(th)) / (ER + altKm - ER * Math.cos(th)));
  const want = (H / 2 / Math.tan(Math.PI / 8)) * Math.tan(alpha);
  const got = H / 2 - proj([lon, lat + 0.2])![1];
  proj.scale(want / got);
  const pxPerKm = (H / 2) / (altKm * Math.tan(Math.PI / 8));
  return { proj, pth: geoPath(proj), clip, pxPerKm, altKm, W, H, lon, lat };
}
type Cam = ReturnType<typeof camera>;
// pick the resolution whose cells are 26–70 px across at the centre of the view (the crossfade band is where two qualify)
const acrossKm = (res: number) => 2 * h3.getHexagonEdgeLengthAvg(res, "km");
function resFor(cam: Cam) { for (let r = 0; r <= 8; r++) if (acrossKm(r) * cam.pxPerKm < 70) return r; return 8; }
function visibleCells(cam: Cam, res: number): string[] {
  const angDeg = Math.min(cam.clip, ((Math.hypot(cam.W, cam.H) / 2 / cam.pxPerKm) / ER) * (180 / Math.PI) * 1.15);
  const near = (c: string) => { const [la, lo] = h3.cellToLatLng(c); return d3dist([cam.lon, cam.lat], [lo, la]) < angDeg; };
  if (res <= 2) return h3.getRes0Cells().flatMap((c) => h3.cellToChildren(c, res)).filter(near);
  // a disk around the centre
  const centre = h3.latLngToCell(cam.lat, cam.lon, res);
  const k = Math.ceil((angDeg * Math.PI / 180 * ER) / (acrossKm(res) * 0.86)) + 1;
  return h3.gridDisk(centre, k).filter(near);
}
function d3dist(a: number[], b: number[]) { const r = Math.PI / 180; const s = Math.sin((b[1] - a[1]) * r / 2) ** 2 + Math.cos(a[1] * r) * Math.cos(b[1] * r) * Math.sin((b[0] - a[0]) * r / 2) ** 2; return (2 * Math.asin(Math.min(1, Math.sqrt(s)))) / r; }
const cellGeo = (c: string) => ({ type: "Polygon", coordinates: [(() => { const b = h3.cellToBoundary(c, true); return [...b, b[0]].reverse(); })()] });

const defs = `<defs>
 <radialGradient id="space" cx="50%" cy="50%" r="75%"><stop offset="0" stop-color="#0b1622"/><stop offset="1" stop-color="#03060a"/></radialGradient>
 <radialGradient id="shade" cx="42%" cy="38%" r="62%"><stop offset=".6" stop-color="#000" stop-opacity="0"/><stop offset="1" stop-color="#000" stop-opacity=".5"/></radialGradient>
 <filter id="glow" x="-20%" y="-20%" width="140%" height="140%"><feGaussianBlur stdDeviation="12"/></filter>
 <filter id="shadow" x="-20%" y="-20%" width="140%" height="160%"><feDropShadow dx="0" dy="2" stdDeviation="2.2" flood-color="#000" flood-opacity=".55"/></filter>
</defs>`;
function frame(cam: Cam, o: { res?: number; fadeRes?: number; fade?: number; lens?: (c: string) => { fill: string; op: number } | null; labels?: number; parents?: boolean; pentagon?: boolean } = {}) {
  const { W, H, pth } = cam;
  let s = `<svg xmlns="http://www.w3.org/2000/svg" width="${W}" height="${H}" viewBox="0 0 ${W} ${H}">
<style>@font-face{font-family:Jakarta;src:url(../../../assets/fonts/PlusJakartaSans-SemiBold.ttf)}</style>${defs}<rect width="${W}" height="${H}" fill="url(#space)"/>`;
  let rng = 3; const rnd = () => ((rng = (rng * 16807) % 2147483647) / 2147483647);
  for (let i = 0; i < 200; i++) s += `<circle cx="${r1(rnd() * W)}" cy="${r1(rnd() * H)}" r="${(0.4 + rnd() * 0.9).toFixed(2)}" fill="#fff" opacity="${(0.3 + rnd() * 0.6).toFixed(2)}"/>`;
  const sphere = pth({ type: "Sphere" } as any)!;
  s += `<path d="${sphere}" fill="none" stroke="#6fb4e0" stroke-width="18" opacity=".25" filter="url(#glow)"/>`;
  s += `<path d="${sphere}" fill="#1f3650"/>`;
  s += `<path d="${pth(land as any)}" fill="#55603f"/>`;
  s += `<path d="${pth(lakes as any)}" fill="#1f3650"/>`;
  const drawRes = (res: number, op: number, lens = o.lens) => {
    const cells = visibleCells(cam, res);
    const groups: Record<string, string> = {}; let lines = "";
    for (const c of cells) {
      const d = pth(cellGeo(c) as any); if (!d) continue;
      const L = lens ? lens(c) : (() => { const k = cls(c); return k ? { fill: LC[k] ?? "#6f8a55", op: 0.92 } : null; })();
      if (L) { const key = L.fill + "|" + L.op; groups[key] = (groups[key] ?? "") + d; }
      lines += d;
    }
    let g = `<g opacity="${op}">`;
    for (const [k, d] of Object.entries(groups)) { const [fill, a] = k.split("|"); g += `<path d="${d}" fill="${fill}" opacity="${a}"/>`; }
    g += `<path d="${lines}" fill="none" stroke="#fff8e0" stroke-width=".7" opacity=".28"/></g>`;
    return { g, n: cells.length };
  };
  const res = o.res ?? resFor(cam);
  const main = drawRes(res, o.fadeRes !== undefined ? 1 - (o.fade ?? 0.5) : 1);
  s += main.g;
  if (o.fadeRes !== undefined) s += drawRes(o.fadeRes, o.fade ?? 0.5).g;
  if (o.parents && res > 0) {
    const ps = visibleCells(cam, res - 1).map((c) => pth(cellGeo(c) as any)).join("");
    s += `<path d="${ps}" fill="none" stroke="#fff8e0" stroke-width="1.6" opacity=".45"/>`;
  }
  s += `<path d="${pth(countries as any)}" fill="none" stroke="#f3e2b8" stroke-width="1" opacity=".55"/>`;
  if (o.pentagon) for (const p of h3.getPentagons(res)) {
    const d = pth(cellGeo(p) as any); if (!d) continue;
    s += `<path d="${d}" fill="#5cc6c0" opacity=".35" stroke="#5cc6c0" stroke-width="2"/>`;
    const [la, lo] = h3.cellToLatLng(p); const xy = cam.proj([lo, la]);
    if (xy && d3dist([cam.lon, cam.lat], [lo, la]) < cam.clip - 6) s += `<text x="${r1(xy[0] + 14)}" y="${r1(xy[1] + 4)}" font-family="Jakarta" font-size="12" fill="#bff0ec" filter="url(#shadow)">a pentagon, at sea</text>`;
  }
  // labels: the biggest places in view
  let n = 0; const taken: number[][] = [];
  for (const f of [...places.features].sort((a: any, b: any) => b.properties.pop_max - a.properties.pop_max)) {
    if (n >= (o.labels ?? 0)) break;
    const [lo, la] = f.geometry.coordinates; if (d3dist([cam.lon, cam.lat], [lo, la]) > cam.clip - 9) continue;
    const xy = cam.proj([lo, la]); if (!xy || xy[0] < 30 || xy[1] < 30 || xy[0] > W - 120 || xy[1] > H - 30) continue;
    if (taken.some(([x, y]) => Math.abs(x - xy[0]) < 110 && Math.abs(y - xy[1]) < 24)) continue;
    taken.push(xy); n++;
    s += `<g font-family="Jakarta"><circle cx="${r1(xy[0])}" cy="${r1(xy[1])}" r="3" fill="#fff6dc" stroke="#2b2118" stroke-width="1.4"/><text x="${r1(xy[0] + 7)}" y="${r1(xy[1] + 4)}" font-size="12.5" fill="#fff6dc" filter="url(#shadow)">${f.properties.name}</text></g>`;
  }
  s += `<path d="${sphere}" fill="url(#shade)"/>`;
  s += `</svg>`;
  return { svg: s, res, cells: main.n };
}

const which = (process.env.ONLY ?? "ladder,fade,hero").split(",");
// --- 1. the ladder: one resolution per altitude band --------------------------
if (which.includes("ladder")) {
  for (const alt of [300, 1000, 3000, 8000, 20000]) {
    const cam = camera(alt, 960, 600, alt >= 8000 ? 12 : 6.2, alt >= 8000 ? 46 : alt >= 3000 ? 50 : 52.2);
    const f = frame(cam, { labels: alt <= 1000 ? 8 : 6, parents: true, pentagon: alt >= 3000 });
    await Bun.write(`${outD}/globe-${alt}.svg`, f.svg);
    console.log(alt, "km → res", f.res, "across", acrossKm(f.res).toFixed(0), "km =", (acrossKm(f.res) * cam.pxPerKm).toFixed(0), "px,", f.cells, "cells");
  }
}
// --- 2. a crossfade moment: res 4 giving way to res 3 -------------------------
if (which.includes("fade")) {
  const cam = camera(1700, 960, 600, 6.2, 51);
  const f = frame(cam, { res: 4, fadeRes: 3, fade: 0.5, labels: 6 });
  await Bun.write(`${outD}/globe-fade.svg`, f.svg);
  console.log("fade at 1700 km: res4 px", (acrossKm(4) * cam.pxPerKm).toFixed(0), "res3 px", (acrossKm(3) * cam.pxPerKm).toFixed(0));
}
// --- 3. the hero: the world board with the nuclear scenario rolled up ----------
if (which.includes("hero")) {
  // the fallout field of v1-nuclear (same sources and model as gen.ts)
  const bases = [[5.7086, 51.6564], [5.47, 51.1683], [7.0633, 50.1736], [12.5961, 46.0319], [10.2678, 45.4322]];
  const src: { lon: number; lat: number; kt: number }[] = bases.map(([lon, lat]) => ({ lon, lat, kt: 100 }));
  let rng = 11; const rnd = () => ((rng = (rng * 16807) % 2147483647) / 2147483647);
  for (let i = 0; i < 70; i++) src.push({ lon: 15 + rnd() * 13, lat: 50 + rnd() * 6.5, kt: 10 + rnd() * 40 });
  for (let i = 0; i < 40; i++) src.push({ lon: 27 + rnd() * 6, lat: 52 + rnd() * 5, kt: 10 + rnd() * 40 });
  const windTo = (lat: number) => 68 + (lat - 50) * 2.2;
  const dose = (lon: number, lat: number) => { let d = 0; for (const q of src) { const kx = 111.32 * Math.cos((q.lat * Math.PI) / 180); const dx = (lon - q.lon) * kx, dy = (lat - q.lat) * 110.57; const a = (windTo(q.lat) * Math.PI) / 180; const along = dx * Math.sin(a) + dy * Math.cos(a), across = dx * Math.cos(a) - dy * Math.sin(a); if (along < -15) continue; const s = Math.pow(q.kt / 100, 0.45), al = Math.max(along, 0) / s, wd = (1.5 + 0.1 * al) * s; d += 8000 * Math.pow(al + 3, -2.3) * Math.exp(-((across / wd) ** 2)) * (along < 0 ? Math.exp(along / 3) : 1); } return d; };
  const bands = [{ min: 10, col: "#c9262a" }, { min: 1, col: "#ec6a2c" }, { min: 0.1, col: "#f2c14c" }, { min: 0.02, col: "#9cc7a8" }];
  // roll up: sample the cell's res-5 grandchildren; colour by the worst band, opacity by the share in any band
  const stats = new Map<string, { worst: number; share: number }>();
  const lens = (c: string) => {
    if (!stats.has(c)) {
      const kids = h3.cellToChildren(c, h3.getResolution(c) + 3); let inN = 0, worst = 99;
      for (const k of kids) { const [la, lo] = h3.cellToLatLng(k); const d = dose(lo, la); const bi = bands.findIndex((b) => d >= b.min); if (bi >= 0) { inN++; worst = Math.min(worst, bi); } }
      stats.set(c, { worst, share: inN / kids.length });
    }
    const st = stats.get(c)!;
    if (st.share === 0) { const k = cls(c); return k ? { fill: LC[k] ?? "#6f8a55", op: 0.55 } : null; }
    return { fill: bands[st.worst].col, op: Math.round((0.45 + 0.5 * Math.min(1, st.share * 3)) * 20) / 20 };
  };
  const cam = camera(12000, 1600, 1000, 16, 47);
  const f = frame(cam, { res: 2, lens, labels: 10 });
  await Bun.write(`${outD}/globe-hero.svg`, f.svg);
  const pl = h3.latLngToCell(52.2, 19.5, 2), st = stats.get(pl);
  console.log("hero res", f.res, "px", (acrossKm(2) * cam.pxPerKm).toFixed(0), "cells", f.cells, "Poland cell", pl, JSON.stringify(st));
  const inBand = [...stats.values()].filter((x) => x.share > 0).length; console.log("cells touched by fallout", inBand);
}
console.log("done");

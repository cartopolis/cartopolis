// City detail at a distance, from what the client's OSM tiles already carry (Shortbread):
// building footprints with heights, land use, streets, stations, district names.
// Usage: bun city.ts <outdir> <name> <lat> <lon> <altKm> <buildingZoom|0> <landZoom>
import { VectorTile } from "@mapbox/vector-tile";
import { PbfReader as Pbf } from "pbf";
import { mkdirSync, existsSync } from "fs";

const [out, name] = [process.argv[2], process.argv[3]];
const [lat0, lon0, altKm] = process.argv.slice(4, 7).map(Number);
const bz = Number(process.argv[7]), lz = Number(process.argv[8]);
const W = 1600, H = 900;

// --- camera framing: straight down, 45° vertical fov (matches the client at 350 km) ---
const hh = altKm * Math.tan(Math.PI / 8), hw = (hh * W) / H;
const dLat = hh / 110.57, dLon = hw / (111.32 * Math.cos((lat0 * Math.PI) / 180));
const BB = [lon0 - dLon, lat0 - dLat, lon0 + dLon, lat0 + dLat];
const merc = (lat: number) => Math.log(Math.tan(Math.PI / 4 + (lat * Math.PI) / 360));
const kx = W / (BB[2] - BB[0]), ky = kx * (180 / Math.PI);
const cy = (merc(BB[3]) + merc(BB[1])) / 2;
const P = (lon: number, lat: number): [number, number] => [W / 2 + (lon - lon0) * kx, H / 2 - (merc(lat) - cy) * ky];
const mPerPx = (hw * 2 * 1000) / W;

// --- tiles, cached on disk ---
async function tile(z: number, x: number, y: number): Promise<VectorTile | null> {
  const f = `tiles/${z}/${x}/${y}.pbf`;
  if (!existsSync(f)) {
    const r = await fetch(`https://tiles.cartopolis.org/tiles/osm/${z}/${x}/${y}`);
    if (!r.ok) return null;
    mkdirSync(`tiles/${z}/${x}`, { recursive: true });
    await Bun.write(f, await r.arrayBuffer());
  }
  return new VectorTile(new Pbf(new Uint8Array(await Bun.file(f).arrayBuffer())));
}
const lon2x = (l: number, z: number) => Math.floor(((l + 180) / 360) * 2 ** z);
const lat2y = (l: number, z: number) => { const r = (l * Math.PI) / 180; return Math.floor(((1 - Math.log(Math.tan(r) + 1 / Math.cos(r)) / Math.PI) / 2) * 2 ** z); };
async function eachTile(z: number, fn: (vt: VectorTile, x: number, y: number) => void) {
  const xs: [number, number][] = [];
  for (let x = lon2x(BB[0], z); x <= lon2x(BB[2], z); x++) for (let y = lat2y(BB[3], z); y <= lat2y(BB[1], z); y++) xs.push([x, y]);
  let i = 0;
  const worker = async () => { while (i < xs.length) { const [x, y] = xs[i++]; const vt = await tile(z, x, y); if (vt) fn(vt, x, y); } };
  await Promise.all(Array.from({ length: 16 }, worker));
  console.error(name, "z", z, xs.length, "tiles");
}
// tile-local point → screen pixel
function toPx(z: number, x: number, y: number, ext: number) {
  const n = 2 ** z;
  return (px: number, py: number): [number, number] => {
    const lon = ((x + px / ext) / n) * 360 - 180;
    const t = Math.PI * (1 - (2 * (y + py / ext)) / n);
    const lat = (Math.atan(Math.sinh(t)) * 180) / Math.PI;
    return P(lon, lat);
  };
}

// --- a small scanline rasteriser (even-odd over all rings of one shape) ---
function fill(buf: Uint8Array, BW: number, BH: number, rings: [number, number][][], v: number) {
  let y0 = 1e9, y1 = -1e9;
  for (const r of rings) for (const p of r) { if (p[1] < y0) y0 = p[1]; if (p[1] > y1) y1 = p[1]; }
  y0 = Math.max(0, Math.floor(y0)); y1 = Math.min(BH - 1, Math.ceil(y1));
  const xs: number[] = [];
  for (let y = y0; y <= y1; y++) {
    const sy = y + 0.5; xs.length = 0;
    for (const r of rings) for (let i = 0, j = r.length - 1; i < r.length; j = i++) {
      const [ax, ay] = r[i], [bx, by] = r[j];
      if ((ay > sy) !== (by > sy)) xs.push(ax + ((sy - ay) / (by - ay)) * (bx - ax));
    }
    xs.sort((a, b) => a - b);
    for (let k = 0; k + 1 < xs.length; k += 2) {
      const a = Math.max(0, Math.ceil(xs[k] - 0.5)), b = Math.min(BW - 1, Math.floor(xs[k + 1] - 0.5));
      for (let x = a; x <= b; x++) buf[y * BW + x] = v;
    }
  }
}

// --- sun: 30 October, 12:00 local (the captures' --time 12) over the Netherlands ---
const sunElev = (24 * Math.PI) / 180; // 90 − 52 − 14
const shadowDir: [number, number] = [0.12, -1]; // north, a touch east, in screen space

let svg = `<svg xmlns="http://www.w3.org/2000/svg" width="${W}" height="${H}" viewBox="0 0 ${W} ${H}">
<style>@font-face{font-family:Jakarta;src:url(../../../assets/fonts/PlusJakartaSans-SemiBold.ttf)}</style>
<defs>
 <filter id="paper"><feTurbulence type="fractalNoise" baseFrequency="0.9" numOctaves="2" seed="4" result="n"/><feColorMatrix in="n" type="matrix" values="0 0 0 0 0  0 0 0 0 0  0 0 0 0 0  0 0 0 .08 0" result="g"/><feComposite in="g" in2="SourceGraphic" operator="in" result="gg"/><feBlend in="SourceGraphic" in2="gg" mode="multiply"/></filter>
 <filter id="lift" x="-5%" y="-5%" width="110%" height="110%"><feDropShadow dx="0.4" dy="-1.6" stdDeviation="0.9" flood-color="#141c22" flood-opacity=".6"/></filter>
 <filter id="shadow" x="-20%" y="-20%" width="140%" height="160%"><feDropShadow dx="0" dy="2" stdDeviation="2.2" flood-color="#000" flood-opacity=".55"/></filter>
 <filter id="halo"><feMorphology operator="dilate" radius="1.6" in="SourceAlpha" result="d"/><feFlood flood-color="#1a2224" flood-opacity=".85"/><feComposite in2="d" operator="in" result="h"/><feMerge><feMergeNode in="h"/><feMergeNode in="SourceGraphic"/></feMerge></filter>
 <pattern id="trees" width="14" height="12" patternUnits="userSpaceOnUse"><path d="M4 9 l3 -7 l3 7z" fill="#2a4229" opacity=".4"/></pattern>
</defs>`;

// --- land use ---
const LAND: Record<string, string> = {};
const set = (c: string, ks: string[]) => ks.forEach((k) => (LAND[k] = c));
set("#6f8a55", ["grass", "meadow", "grassland", "village_green", "greenfield", "recreation_ground", "playground", "golf_course", "miniature_golf"]);
set("#62804f", ["park", "garden", "allotments", "cemetery"]);
set("#3e5a3c", ["forest"]); set("#7b8559", ["scrub", "heath"]);
set("#979262", ["farmland", "orchard", "plant_nursery", "vineyard", "farmyard", "greenhouse_horticulture"]);
set("#5f7c6e", ["marsh", "wet_meadow", "swamp", "bog"]); set("#cdc29c", ["sand", "beach", "scree", "quarry"]);
set("#958d82", ["residential", "garages"]);
set("#8b8a92", ["industrial", "railway", "brownfield", "landfill", "construction"]);
set("#9a8f8e", ["commercial", "retail"]);
const ORDER = ["#979262", "#6f8a55", "#7b8559", "#5f7c6e", "#cdc29c", "#958d82", "#8b8a92", "#9a8f8e", "#62804f", "#3e5a3c"];
const landD: Record<string, string> = {}, water: string[] = [], waterSmall: string[] = [], streets: Record<string, string[]> = {};
const labels: { kind: string; name: string; x: number; y: number; pop?: number }[] = [];
const stations: { name: string; x: number; y: number }[] = [];
const r1 = (n: number) => Math.round(n * 10) / 10;
const ringD = (rings: [number, number][][], close: boolean) => rings.map((r) => r.map((p, i) => (i ? "L" : "M") + r1(p[0]) + " " + r1(p[1])).join("") + (close ? "Z" : "")).join("");

await eachTile(lz, (vt, x, y) => {
  const L = (k: string) => (vt.layers[k] as any);
  const T = (l: any) => toPx(lz, x, y, l.extent);
  const geo = (l: any, i: number) => { const t = T(l); return l.feature(i).loadGeometry().map((r: any[]) => r.map((p) => t(p.x, p.y))) as [number, number][][]; };
  const land = L("land"); if (land) for (let i = 0; i < land.length; i++) { const c = LAND[land.feature(i).properties.kind]; if (c) landD[c] = (landD[c] ?? "") + ringD(geo(land, i), true); }
  for (const k of ["water_polygons", "ocean"]) { const l = L(k); if (l) for (let i = 0; i < l.length; i++) {
    const g = geo(l, i); let a = 0;
    for (const r of g) for (let j = 0, m = r.length - 1; j < r.length; m = j++) a += (r[m][0] * r[j][1] - r[j][0] * r[m][1]) / 2;
    const A = Math.abs(a);
    if (A < 6) continue; // ditches: thinner than a pixel at this scale
    (A > 400 ? water : waterSmall).push(ringD(g, true));
  } }
  const st = L("streets"); if (st) for (let i = 0; i < st.length; i++) { const k = st.feature(i).properties.kind as string; (streets[k] ??= []).push(ringD(geo(st, i), false)); }
  const wl = L("water_lines"); if (wl) for (let i = 0; i < wl.length; i++) (streets["~" + wl.feature(i).properties.kind] ??= []).push(ringD(geo(wl, i), false));
});

// labels and stations from the building zoom (or the land zoom when there is none)
await eachTile(bz || Math.min(lz + 1, 14), (vt, x, y) => {
  const z = bz || Math.min(lz + 1, 14);
  const pl = vt.layers.place_labels as any;
  if (pl) for (let i = 0; i < pl.length; i++) { const f = pl.feature(i); const p = f.loadGeometry()[0][0]; const [sx, sy] = toPx(z, x, y, pl.extent)(p.x, p.y); labels.push({ kind: f.properties.kind, name: f.properties.name, x: sx, y: sy, pop: f.properties.population }); }
  const pt = vt.layers.public_transport as any;
  if (pt) for (let i = 0; i < pt.length; i++) { const f = pt.feature(i); if (f.properties.kind !== "station" && f.properties.kind !== "railway_station") continue; const p = f.loadGeometry()[0][0]; const [sx, sy] = toPx(z, x, y, pt.extent)(p.x, p.y); if (f.properties.name) stations.push({ name: f.properties.name, x: sx, y: sy }); }
});

svg += `<g filter="url(#paper)"><rect width="${W}" height="${H}" fill="#7f8f60"/>`;
for (const c of ORDER) if (landD[c]) { const built = ["#958d82", "#8b8a92", "#9a8f8e"].includes(c) && !bz;
  svg += `<path d="${landD[c]}" fill="${c}" stroke="${c}" stroke-width="1.4" fill-rule="evenodd"${built ? ' filter="url(#lift)"' : ""}/>`; if (c === "#3e5a3c") svg += `<path d="${landD[c]}" fill="url(#trees)"/>`; }
svg += `<path d="${water.join("")}" fill="#36607e" stroke="#36607e" stroke-width="2.2" fill-rule="evenodd"/><path d="${waterSmall.join("")}" fill="#36607e" stroke="#36607e" stroke-width="1.2" fill-rule="evenodd"/>`;
svg += `<path d="${(streets["~river"] ?? []).concat(streets["~canal"] ?? []).join("")}" fill="none" stroke="#36607e" stroke-width="${altKm < 20 ? 2 : 1.2}"/>`;
svg += `</g>`;

// --- buildings: footprint + a shadow from the height, rasterised ---
if (bz) {
  const S = altKm < 20 ? 2 : 3, BW = W * S, BH = H * S;
  const cov = new Uint8Array(BW * BH), shd = new Uint8Array(BW * BH), tone = new Uint8Array(BW * BH);
  let n = 0;
  await eachTile(bz, (vt, x, y) => {
    const b = vt.layers.buildings as any; if (!b) return;
    const t = toPx(bz, x, y, b.extent);
    for (let i = 0; i < b.length; i++) {
      const f = b.feature(i);
      const rings = f.loadGeometry().map((r: any[]) => r.map((p: any) => { const q = t(p.x, p.y); return [q[0] * S, q[1] * S] as [number, number]; }));
      const h = Math.max(3, Number(f.properties.height ?? 6));
      const len = ((h / Math.tan(sunElev)) / mPerPx) * S;
      for (let s = 1; s <= Math.ceil(len); s += 1) {
        const k = Math.min(s, len);
        fill(shd, BW, BH, rings.map((r: [number, number][]) => r.map(([a, c]) => [a + shadowDir[0] * k, c + shadowDir[1] * k] as [number, number])), 1);
      }
      // roof tone from the building's own id-free hash of its first vertex: red tiles, brown, grey flats
      const hv = Math.abs(Math.floor(rings[0][0][0] * 7.3 + rings[0][0][1] * 3.1)) % 4 + 1;
      fill(tone, BW, BH, rings, h > 15 ? 3 : hv);
      fill(cov, BW, BH, rings, 1);
      n++;
    }
  });
  console.error(name, n, "buildings");
  const ROOF = [[0, 0, 0], [138, 92, 74], [118, 98, 88], [128, 124, 122], [104, 100, 102]];
  const rgba = new Uint8Array(W * H * 4);
  for (let y = 0; y < H; y++) for (let x = 0; x < W; x++) {
    let c = 0, s = 0, r = 0, g = 0, bl = 0;
    for (let j = 0; j < S; j++) for (let i = 0; i < S; i++) {
      const k = (y * S + j) * BW + x * S + i;
      if (cov[k]) { c++; const t = ROOF[tone[k]]; r += t[0]; g += t[1]; bl += t[2]; } else if (shd[k]) s++;
    }
    const o = (y * W + x) * 4, a = c / (S * S), sa = (s / (S * S)) * 0.45;
    const al = a + sa * (1 - a);
    if (al <= 0) continue;
    const rr = c ? r / c : 0, gg = c ? g / c : 0, bb = c ? bl / c : 0;
    // shadow is a dark blue-grey
    rgba[o] = Math.round((rr * a + 28 * sa * (1 - a)) / al); rgba[o + 1] = Math.round((gg * a + 34 * sa * (1 - a)) / al);
    rgba[o + 2] = Math.round((bb * a + 44 * sa * (1 - a)) / al); rgba[o + 3] = Math.round(al * 255);
  }
  await Bun.write(`/tmp/s525-${name}.rgba`, rgba);
  Bun.spawnSync(["convert", "-size", `${W}x${H}`, "-depth", "8", `rgba:/tmp/s525-${name}.rgba`, `${out}/${name}-bld.png`]);
  svg += `<image href="${name}-bld.png" width="${W}" height="${H}"/>`;
}

// --- streets by rank ---
const S2 = (ks: string[]) => ks.flatMap((k) => streets[k] ?? []).join("");
const near = altKm < 20;
if (near) svg += `<path d="${S2(["residential", "living_street", "unclassified", "service"])}" fill="none" stroke="#d9d2c4" stroke-width=".8" opacity=".55"/>`;
svg += `<path d="${S2(["tertiary"])}" fill="none" stroke="#e8dfc9" stroke-width="${near ? 1.6 : 0.9}" opacity=".85"/>`;
svg += `<path d="${S2(["secondary", "primary"])}" fill="none" stroke="#4a3a2e" stroke-width="${near ? 4.4 : 3}" opacity=".45"/><path d="${S2(["secondary", "primary"])}" fill="none" stroke="#f1e4c4" stroke-width="${near ? 2.6 : 1.6}"/>`;
svg += `<path d="${S2(["motorway", "trunk"])}" fill="none" stroke="#4a3426" stroke-width="${near ? 7 : 5.5}" opacity=".6" stroke-linecap="round"/><path d="${S2(["motorway", "trunk"])}" fill="none" stroke="#e9c97e" stroke-width="${near ? 4 : 3}" stroke-linecap="round"/>`;
svg += `<path d="${S2(["rail"])}" fill="none" stroke="#3b3430" stroke-width="2.4" opacity=".55"/><path d="${S2(["rail"])}" fill="none" stroke="#efe6d2" stroke-width="1" stroke-dasharray="5 5"/>`;

// --- district names, stations, city banners ---
const taken: number[][] = [];
const free = (b: number[]) => !taken.some((t) => !(b[2] < t[0] || b[0] > t[2] || b[3] < t[1] || b[1] > t[3])) && (taken.push(b), true);
const seen = new Set<string>();
const cityKinds = ["capital", "state_capital", "city", "town"];
for (const l of labels.filter((l) => cityKinds.includes(l.kind)).sort((a, b) => (b.pop ?? 0) - (a.pop ?? 0))) {
  if (seen.has(l.name) || l.x < 60 || l.x > W - 60 || l.y < 40 || l.y > H - 20) continue; seen.add(l.name);
  if (altKm > 30 && l.kind === "town" && (l.pop ?? 0) < 40000) continue;
  const pop = l.pop ? (l.pop >= 1e6 ? (l.pop / 1e6).toFixed(1) + "M" : Math.round(l.pop / 1e3) + "k") : "";
  const tw = l.name.length * 8.2 + 8, pw = pop ? pop.length * 7 + 12 : 0, w = tw + pw + 10, h = 23;
  if (!free([l.x - w / 2, l.y - 34, l.x + w / 2, l.y + 4])) continue;
  svg += `<g transform="translate(${r1(l.x)},${r1(l.y)})" font-family="Jakarta"><circle r="4" fill="#fff6dc" stroke="#2b2118" stroke-width="1.5"/><g transform="translate(${r1(-w / 2)},-31)" filter="url(#shadow)"><rect width="${r1(w)}" height="${h}" rx="11.5" fill="#13191b" stroke="rgba(255,255,255,.22)"/>${pop ? `<rect x="3" y="3" width="${r1(pw)}" height="17" rx="8.5" fill="#5cc6c0"/><text x="${r1(3 + pw / 2)}" y="15.5" font-size="11" text-anchor="middle" fill="#0f1416">${pop}</text>` : ""}<text x="${r1(pw + 9)}" y="16" font-size="13.5" fill="#f3f7f9">${l.name}</text></g></g>`;
}
for (const s of altKm > 60 ? [] : stations) {
  if (seen.has("st:" + s.name) || s.x < 20 || s.x > W - 20 || s.y < 20 || s.y > H - 20) continue; seen.add("st:" + s.name);
  const towns = labels.filter((l) => cityKinds.includes(l.kind)).map((l) => l.name);
  const named = near && towns.some((t) => s.name.startsWith(t));
  const w = named ? s.name.length * 6.4 + 30 : 22;
  if (!free([s.x - 11, s.y - 11, s.x - 11 + w, s.y + 11])) continue;
  svg += `<g transform="translate(${r1(s.x)},${r1(s.y)})" font-family="Jakarta"><circle r="9" fill="#13191b" stroke="#5cc6c0" stroke-width="1.6"/><path d="M-3.5 -4h7v6h-7z M-3.5 0h7 M-2 4l-1.5 2 M2 4l1.5 2" stroke="#f3f7f9" stroke-width="1.3" fill="none"/>${named ? `<text x="13" y="4" font-size="11.5" fill="#f3f7f9" filter="url(#halo)">${s.name}</text>` : ""}</g>`;
}
const distKinds = altKm < 20 ? ["suburb", "quarter"] : ["suburb"];
for (const l of labels.filter((l) => distKinds.includes(l.kind))) {
  if (seen.has(l.name) || l.x < 30 || l.x > W - 30 || l.y < 20 || l.y > H - 20) continue; seen.add(l.name);
  const t = l.name.toUpperCase(), fs = l.kind === "neighbourhood" ? 9.5 : 10.5, w = t.length * fs * 0.72;
  if (!free([l.x - w / 2, l.y - 8, l.x + w / 2, l.y + 6])) continue;
  svg += `<text x="${r1(l.x)}" y="${r1(l.y)}" text-anchor="middle" font-family="Jakarta" font-size="${fs}" letter-spacing="1.4" fill="#fff8e6" opacity=".9" filter="url(#halo)">${t}</text>`;
}
svg += `</svg>`;
await Bun.write(`${out}/${name}.svg`, svg);
console.error(name, "done", mPerPx.toFixed(1), "m/px");

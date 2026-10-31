// Draws the macro-view mockup maps from real data into SVG.
// Data (fetched once into this directory):
//   Natural Earth 10m/50m GeoJSON (land, countries, lakes, rivers, roads, railroads, provinces, places)
//   lc_z6.geojson: ESA WorldCover 2021 via tiles.cartopolis.org/tiles/landcover, z6
//   cities.json:   OSM place=city nodes from overpass.cartopolis.org
// Usage: bun gen.ts <outdir>
const out = process.argv[2] ?? ".";
const J = async (f: string) => Bun.file(f).json();
const [land, countries, lakes, rivers, roads, rails, provs, lc, cityJ, nePlaces] = await Promise.all([
  J("ne_10m_land.geojson"), J("ne_50m_admin_0_countries.geojson"), J("ne_10m_lakes.geojson"),
  J("ne_10m_rivers_lake_centerlines.geojson"), J("ne_10m_roads.geojson"), J("ne_10m_railroads.geojson"),
  J("ne_10m_admin_1_states_provinces_lines.geojson"), J("lc_z6.geojson"), J("cities.json"),
  J("ne_10m_populated_places_simple.geojson"),
]);
const countries10 = await J("ne_10m_admin_0_countries.geojson");

type BBox = [number, number, number, number]; // W S E N
const merc = (lat: number) => Math.log(Math.tan(Math.PI / 4 + (lat * Math.PI) / 360));
function view(b: BBox, W: number, H: number) {
  const [w, s, e, n] = b;
  const sx = W / (e - w), y0 = merc(n), y1 = merc(s);
  const sy = H / (y0 - y1);
  const k = Math.min(sx, sy * (Math.PI / 180)); // keep conformal: px per degree lon
  const kx = k, ky = k * (180 / Math.PI);
  const cx = (w + e) / 2, cy = (y0 + y1) / 2;
  const P = (lon: number, lat: number): [number, number] => [W / 2 + (lon - cx) * kx, H / 2 - (merc(lat) - cy) * ky];
  const inv = (x: number, y: number): [number, number] => {
    const lon = cx + (x - W / 2) / kx;
    const m = cy - (y - H / 2) / ky;
    return [lon, (Math.atan(Math.sinh(m)) * 180) / Math.PI];
  };
  // metres → px at a latitude
  const kmPx = (km: number, lat: number) => (km / (111.32 * Math.cos((lat * Math.PI) / 180))) * kx;
  return { W, H, b, P, inv, kmPx };
}
type V = ReturnType<typeof view>;

const r1 = (n: number) => Math.round(n * 10) / 10;
function ringsOf(g: any): number[][][] {
  if (!g) return [];
  if (g.type === "Polygon") return g.coordinates;
  if (g.type === "MultiPolygon") return g.coordinates.flat();
  if (g.type === "LineString") return [g.coordinates];
  if (g.type === "MultiLineString") return g.coordinates;
  return [];
}
function bboxHit(ring: number[][], b: BBox, pad = 0.5) {
  let w = 1e9, s = 1e9, e = -1e9, n = -1e9;
  for (const [x, y] of ring) { if (x < w) w = x; if (x > e) e = x; if (y < s) s = y; if (y > n) n = y; }
  return !(e < b[0] - pad || w > b[2] + pad || n < b[1] - pad || s > b[3] + pad);
}
function path(v: V, g: any, closed: boolean, minPx = 0.6) {
  let d = "";
  for (const ring of ringsOf(g)) {
    if (!bboxHit(ring, v.b)) continue;
    let last: [number, number] | null = null, seg = "", n = 0;
    for (const [lon, lat] of ring) {
      const p = v.P(lon, lat);
      if (last && Math.abs(p[0] - last[0]) < minPx && Math.abs(p[1] - last[1]) < minPx) continue;
      seg += (n ? "L" : "M") + r1(p[0]) + " " + r1(p[1]);
      last = p; n++;
    }
    if (n > 1) d += seg + (closed ? "Z" : "");
  }
  return d;
}
const feats = (fc: any, f?: (p: any) => boolean) => fc.features.filter((x: any) => !f || f(x.properties));

// --- point in polygon over landcover, for hex classification -------------
type Poly = { kind: string; rings: number[][][]; bb: BBox };
const lcPolys: Poly[] = lc.features.filter((f: any) => f.properties.layer === "land").flatMap((f: any) => {
  const g = f.geometry; const polys = g.type === "Polygon" ? [g.coordinates] : g.type === "MultiPolygon" ? g.coordinates : [];
  return polys.map((rings: number[][][]) => {
    let w = 1e9, s = 1e9, e = -1e9, n = -1e9;
    for (const [x, y] of rings[0]) { w = Math.min(w, x); e = Math.max(e, x); s = Math.min(s, y); n = Math.max(n, y); }
    return { kind: f.properties.kind, rings, bb: [w, s, e, n] as BBox };
  });
});
function inRing(r: number[][], x: number, y: number) {
  let c = false;
  for (let i = 0, j = r.length - 1; i < r.length; j = i++) {
    const [xi, yi] = r[i], [xj, yj] = r[j];
    if (yi > y !== yj > y && x < ((xj - xi) * (y - yi)) / (yj - yi) + xi) c = !c;
  }
  return c;
}
function inRings(rings: number[][][], x: number, y: number) {
  if (!inRing(rings[0], x, y)) return false;
  for (let i = 1; i < rings.length; i++) if (inRing(rings[i], x, y)) return false;
  return true;
}
function kindAt(lon: number, lat: number): string | null {
  // later features draw on top; take the last hit (smallest usually)
  let k: string | null = null;
  for (const p of lcPolys) {
    const b = p.bb; if (lon < b[0] || lon > b[2] || lat < b[1] || lat > b[3]) continue;
    if (inRings(p.rings, lon, lat)) k = p.kind;
  }
  return k;
}
const landPolys = land.features.flatMap((f: any) => (f.geometry.type === "Polygon" ? [f.geometry.coordinates] : f.geometry.coordinates));
const isLand = (lon: number, lat: number) => landPolys.some((r: number[][][]) => inRings(r, lon, lat)) &&
  !lakes.features.some((f: any) => (f.geometry.type === "Polygon" ? [f.geometry.coordinates] : f.geometry.coordinates).some((r: number[][][]) => inRings(r, lon, lat)));

// --- palette ---------------------------------------------------------------
const LC: Record<string, string> = {
  farmland: "#979262", grassland: "#6f8a55", forest: "#3e5a3c", residential: "#8a817a",
  marsh: "#5f7c6e", heath: "#86727f", scrub: "#7b8559", bare_rock: "#cdc29c",
};
const defs = `
<defs>
  <filter id="paper" x="0" y="0" width="100%" height="100%">
    <feTurbulence type="fractalNoise" baseFrequency="0.9" numOctaves="2" seed="4" result="n"/>
    <feColorMatrix in="n" type="matrix" values="0 0 0 0 0  0 0 0 0 0  0 0 0 0 0  0 0 0 .10 0" result="g"/>
    <feComposite in="g" in2="SourceGraphic" operator="in" result="gg"/>
    <feBlend in="SourceGraphic" in2="gg" mode="multiply"/>
  </filter>
  <filter id="wobble"><feTurbulence type="fractalNoise" baseFrequency="0.035" numOctaves="2" seed="9"/><feDisplacementMap in="SourceGraphic" scale="5"/></filter>
  <filter id="soft"><feGaussianBlur stdDeviation="1.2"/></filter>
  <filter id="glow" x="-50%" y="-50%" width="200%" height="200%"><feGaussianBlur stdDeviation="6"/></filter>
  <filter id="shadow" x="-20%" y="-20%" width="140%" height="160%"><feDropShadow dx="0" dy="2" stdDeviation="2.2" flood-color="#000" flood-opacity=".55"/></filter>
  <pattern id="trees" width="14" height="12" patternUnits="userSpaceOnUse">
    <path d="M4 9 l3 -7 l3 7z" fill="#2a4229" opacity=".45"/><path d="M10 12 l2.5 -5.5 l2.5 5.5z" fill="#2a4229" opacity=".3"/>
  </pattern>
  <pattern id="reeds" width="10" height="8" patternUnits="userSpaceOnUse"><path d="M1 7h4M5 4h4" stroke="#3e6e66" stroke-width="1" opacity=".7"/></pattern>
  <pattern id="waves" width="46" height="26" patternUnits="userSpaceOnUse">
    <path d="M3 8 q5 -4 10 0 t10 0" stroke="#7fb6c9" stroke-width="1" fill="none" opacity=".08"/>
    <path d="M26 20 q5 -4 10 0 t10 0" stroke="#7fb6c9" stroke-width="1" fill="none" opacity=".06"/>
  </pattern>
  <radialGradient id="sea" cx="40%" cy="40%" r="80%"><stop offset="0" stop-color="#2f5878"/><stop offset="1" stop-color="#1f3650"/></radialGradient>
</defs>`;

function hexPath(cx: number, cy: number, r: number) {
  let d = "";
  for (let i = 0; i < 6; i++) { const a = (Math.PI / 180) * (60 * i - 30); d += (i ? "L" : "M") + r1(cx + r * Math.cos(a)) + " " + r1(cy + r * Math.sin(a)); }
  return d + "Z";
}
function hexCentres(W: number, H: number, r: number) {
  const out: [number, number][] = [], w = Math.sqrt(3) * r, h = 1.5 * r;
  for (let row = 0, y = 0; y < H + r; row++, y += h) for (let x = row % 2 ? w / 2 : 0; x < W + w; x += w) out.push([x, y]);
  return out;
}

// --- base map: sea, coast shallows, land cover, water, lines ---------------
function baseMap(v: V, opt: { lc?: boolean; mute?: number; roads?: boolean; rails?: boolean; provinces?: boolean; lcData?: any; hexR?: number } = {}) {
  const landD = path(v, { type: "MultiPolygon", coordinates: landPolys }, true);
  const lakeD = lakes.features.map((f: any) => path(v, f.geometry, true)).join("");
  let s = `<rect width="${v.W}" height="${v.H}" fill="url(#sea)"/><rect width="${v.W}" height="${v.H}" fill="url(#waves)"/>`;
  s += `<path d="${landD}" fill="none" stroke="#4f7f98" stroke-width="22" stroke-linejoin="round" opacity=".35" filter="url(#wobble)"/>`;
  s += `<path d="${landD}" fill="none" stroke="#6f9db3" stroke-width="8" stroke-linejoin="round" opacity=".45" filter="url(#wobble)"/>`;
  s += `<clipPath id="landclip"><path d="${landD}"/></clipPath>`;
  s += `<g clip-path="url(#landclip)"><path d="${landD}" fill="#7a8659"/>`;
  if (opt.lc !== false) {
    const by: Record<string, string> = {};
    for (const f of (opt.lcData ?? lc).features) if (f.properties.layer === "land") by[f.properties.kind] = (by[f.properties.kind] ?? "") + path(v, f.geometry, true, 0.8);
    for (const k of ["farmland", "grassland", "scrub", "heath", "marsh", "bare_rock", "forest", "residential"]) {
      if (!by[k]) continue;
      s += `<path d="${by[k]}" fill="${LC[k]}" stroke="${LC[k]}" stroke-width=".8"/>`;
      if (k === "forest") s += `<path d="${by[k]}" fill="url(#trees)"/>`;
      if (k === "marsh") s += `<path d="${by[k]}" fill="url(#reeds)"/>`;
    }
  }
  if (opt.mute) s += `<rect width="${v.W}" height="${v.H}" fill="#1b2326" opacity="${opt.mute}"/>`;
  s += `</g>`;
  s += `<path d="${lakeD}" fill="#36607e" stroke="#6f9db3" stroke-width="2" opacity="${opt.mute ? 0.7 : 1}"/>`;
  const riv = feats(rivers, (p) => p.featurecla !== "Lake Centerline").map((f: any) => path(v, f.geometry, false)).join("");
  s += `<path d="${riv}" fill="none" stroke="#4a7a98" stroke-width="2.6" stroke-linecap="round" opacity="${opt.mute ? 0.5 : 0.95}"/>`;
  if (opt.provinces) {
    const pv = feats(provs, (p) => ["NLD", "BEL", "DEU"].includes(p.adm0_a3)).map((f: any) => path(v, f.geometry, false)).join("");
    s += `<path d="${pv}" fill="none" stroke="#fff6dc" stroke-width="1.2" stroke-dasharray="2 4" opacity=".55"/>`;
  }
  if (opt.rails) {
    const rl = feats(rails, (p) => !p.disp_scale || true).map((f: any) => path(v, f.geometry, false)).join("");
    s += `<path d="${rl}" fill="none" stroke="#3b3430" stroke-width="2.2" opacity=".55"/><path d="${rl}" fill="none" stroke="#efe6d2" stroke-width="1" stroke-dasharray="5 5" opacity=".7"/>`;
  }
  if (opt.roads !== false) {
    const sec = feats(roads, (p) => p.continent === "Europe" && p.type === "Secondary Highway").map((f: any) => path(v, f.geometry, false)).join("");
    const maj = feats(roads, (p) => p.continent === "Europe" && p.type === "Major Highway").map((f: any) => path(v, f.geometry, false)).join("");
    s += `<path d="${sec}" fill="none" stroke="#5a4636" stroke-width="3" stroke-linecap="round" opacity=".55"/><path d="${sec}" fill="none" stroke="#f3e2b8" stroke-width="1.4" stroke-linecap="round" opacity=".9"/>`;
    s += `<path d="${maj}" fill="none" stroke="#4a3426" stroke-width="5" stroke-linecap="round" stroke-linejoin="round" opacity=".7"/><path d="${maj}" fill="none" stroke="#e9c97e" stroke-width="2.6" stroke-linecap="round" stroke-linejoin="round"/>`;
  }
  return s;
}

function countryLines(v: V, codes: string[], color: string, width = 3) {
  const fc = countries10;
  let s = "";
  for (const f of fc.features) {
    if (!codes.includes(f.properties.ADM0_A3)) continue;
    const d = path(v, f.geometry, true);
    const id = "cc" + f.properties.ADM0_A3 + Math.round(Math.random() * 1e6);
    s += `<clipPath id="${id}"><path d="${d}"/></clipPath><path d="${d}" fill="none" stroke="${color}" stroke-width="16" opacity=".28" clip-path="url(#${id})"/>`;
    s += `<path d="${d}" fill="none" stroke="${color}" stroke-width="${width}" stroke-linejoin="round" opacity=".95"/>`;
  }
  return s;
}

// --- cities ---------------------------------------------------------------
type City = { name: string; pop: number; lon: number; lat: number };
const cities: City[] = cityJ.elements.map((e: any) => ({ name: e.tags["name:en"] && e.tags.name === "Den Haag" ? "Den Haag" : e.tags.name, pop: +(e.tags.population || 0), lon: e.lon, lat: e.lat }));
const foreign = ["Antwerpen", "Gent", "Brussels", "Liège", "Essen", "Duisburg", "Düsseldorf", "Cologne", "Dortmund", "Bonn", "Lille", "Brugge", "Wuppertal", "Hasselt", "Emden"];
for (const f of nePlaces.features) if (foreign.includes(f.properties.name)) cities.push({ name: f.properties.name === "Cologne" ? "Köln" : f.properties.name, pop: f.properties.pop_max, lon: f.geometry.coordinates[0], lat: f.geometry.coordinates[1] });
cities.sort((a, b) => b.pop - a.pop);
const fmtPop = (n: number) => (n >= 1e6 ? (n / 1e6).toFixed(1) + "M" : Math.round(n / 1e3) + "k");
function banner(v: V, c: City, o: { color?: string; badge?: string; small?: boolean; dim?: boolean } = {}) {
  const [x, y] = v.P(c.lon, c.lat);
  if (x < -40 || y < -20 || x > v.W + 40 || y > v.H + 20) return "";
  const big = c.pop >= 300000 && !o.small;
  const fs = big ? 15 : 12.5, h = big ? 26 : 21;
  const tw = c.name.length * fs * 0.6 + 8;
  const pw = fmtPop(c.pop).length * (fs - 2) * 0.62 + 12;
  const w = tw + pw + 10 + (o.badge ? 22 : 0);
  const col = o.color ?? "#13191b";
  const op = o.dim ? 0.55 : 1;
  let s = `<g transform="translate(${r1(x)},${r1(y)})" opacity="${op}" font-family="Jakarta, sans-serif">`;
  s += `<circle r="${big ? 6 : 4.5}" fill="#fff6dc" stroke="#2b2118" stroke-width="2"/>`;
  s += `<g transform="translate(${r1(-w / 2)},${-h - 9})" filter="url(#shadow)">`;
  s += `<rect width="${r1(w)}" height="${h}" rx="${h / 2}" fill="${col}" stroke="rgba(255,255,255,.22)"/>`;
  s += `<rect x="3" y="3" width="${r1(pw)}" height="${h - 6}" rx="${(h - 6) / 2}" fill="#5cc6c0"/>`;
  s += `<text x="${r1(3 + pw / 2)}" y="${h / 2 + fs * 0.33}" font-size="${fs - 2}" text-anchor="middle" fill="#0f1416">${fmtPop(c.pop)}</text>`;
  s += `<text x="${r1(pw + 9)}" y="${h / 2 + fs * 0.36}" font-size="${fs}" fill="#f3f7f9">${c.name}</text>`;
  if (o.badge) s += `<circle cx="${r1(w - 13)}" cy="${h / 2}" r="8" fill="#fd6012"/><text x="${r1(w - 13)}" y="${h / 2 + 3.6}" font-size="10" text-anchor="middle" fill="#fff">${o.badge}</text>`;
  s += `</g></g>`;
  return s;
}
function placeBanners(v: V, list: City[], max: number, f: (c: City, i: number) => string) {
  // greedy declutter in pixel space
  const taken: [number, number, number, number][] = [];
  let s = "", n = 0;
  for (const c of list) {
    if (n >= max) break;
    const [x, y] = v.P(c.lon, c.lat);
    if (x < 30 || x > v.W - 30 || y < 40 || y > v.H - 10) continue;
    const w = c.name.length * 9 + 60, box: [number, number, number, number] = [x - w / 2, y - 38, x + w / 2, y + 6];
    if (taken.some((t) => !(box[2] < t[0] || box[0] > t[2] || box[3] < t[1] || box[1] > t[3]))) continue;
    taken.push(box); s += f(c, n); n++;
  }
  return s;
}

const svgWrap = (v: V, body: string) => `<svg xmlns="http://www.w3.org/2000/svg" width="${v.W}" height="${v.H}" viewBox="0 0 ${v.W} ${v.H}">
<style>@font-face{font-family:Jakarta;src:url(../../../assets/fonts/PlusJakartaSans-SemiBold.ttf)}</style>${defs}${body}</svg>`;

// ===========================================================================
// Scenario sheets (docs/mockups/scenarios). Everything above is the macro-view
// generator's base map, copied so this folder stands on its own.
// Extra data: ne_10m_admin_1_states_provinces.geojson (Natural Earth, polygons).
// ===========================================================================
const adm1 = await J("ne_10m_admin_1_states_provinces.geojson");
const polysOf = (g: any): number[][][][] => (g.type === "Polygon" ? [g.coordinates] : g.type === "MultiPolygon" ? g.coordinates : []);
const bbOf = (pp: number[][][][]) => { let w = 1e9, s = 1e9, e = -1e9, n = -1e9; for (const p of pp) for (const [x, y] of p[0]) { if (x < w) w = x; if (x > e) e = x; if (y < s) s = y; if (y > n) n = y; } return [w, s, e, n]; };
type Region = { name: string; a3: string; pp: number[][][][]; bb: number[]; g: any };
const regionsOf = (a3s: string[]): Region[] => adm1.features.filter((f: any) => a3s.includes(f.properties.adm0_a3)).map((f: any) => { const pp = polysOf(f.geometry); return { name: f.properties.name, a3: f.properties.adm0_a3, pp, bb: bbOf(pp), g: f.geometry }; });
const inRegion = (r: Region, lon: number, lat: number) => lon >= r.bb[0] && lon <= r.bb[2] && lat >= r.bb[1] && lat <= r.bb[3] && r.pp.some((p) => inRings(p, lon, lat));
const kmBetween = (a: number[], b: number[]) => { const kx = 111.32 * Math.cos((((a[1] + b[1]) / 2) * Math.PI) / 180); return Math.hypot((b[0] - a[0]) * kx, (b[1] - a[1]) * 110.57); };
const EUR = ["POL","LTU","LVA","EST","DEU","CZE","SVK","NLD","BEL","DNK","AUT","HUN","SWE","FIN","FRA","LUX","CHE","ITA","SVN","HRV","ROU","NOR","GBR"];
const HOSTILE = ["RUS", "BLR"];
// the six NATO nuclear-sharing bases named in public reporting (as macro-view/gen.ts)
const bases = [
  { name: "Volkel", lon: 5.7086, lat: 51.6564 }, { name: "Kleine Brogel", lon: 5.47, lat: 51.1683 },
  { name: "Büchel", lon: 7.0633, lat: 50.1736 }, { name: "Aviano", lon: 12.5961, lat: 46.0319 },
  { name: "Ghedi", lon: 10.2678, lat: 45.4322 }, { name: "Incirlik", lon: 35.4259, lat: 37.0021 },
];

// a dark political base for continent-scale scenario maps
function politicalBase(v: V, o: { regionsOf?: string[] } = {}) {
  let s = `<rect width="${v.W}" height="${v.H}" fill="#0f2230"/><rect width="${v.W}" height="${v.H}" fill="url(#waves)"/>`;
  for (const f of countries10.features) {
    const a = f.properties.ADM0_A3;
    const d = path(v, f.geometry, true, 0.8);
    if (!d) continue;
    const fill = EUR.includes(a) ? "#22333b" : HOSTILE.includes(a) ? "#3a2526" : "#272b2c";
    s += `<path d="${d}" fill="${fill}"/>`;
  }
  const lakeD = lakes.features.map((f: any) => path(v, f.geometry, true)).join("");
  s += `<path d="${lakeD}" fill="#173246"/>`;
  const riv = feats(rivers, (p) => p.featurecla !== "Lake Centerline" && (p.scalerank ?? 9) <= 6).map((f: any) => path(v, f.geometry, false)).join("");
  s += `<path d="${riv}" fill="none" stroke="#2c5570" stroke-width="1.4" opacity=".8"/>`;
  if (o.regionsOf) {
    const rl = regionsOf(o.regionsOf).map((r) => path(v, r.g, true, 0.8)).join("");
    s += `<path d="${rl}" fill="none" stroke="#9fb2ba" stroke-width=".7" opacity=".28"/>`;
  }
  for (const f of countries10.features) {
    const a = f.properties.ADM0_A3, d = path(v, f.geometry, true, 0.8);
    if (!d) continue;
    s += `<path d="${d}" fill="none" stroke="${HOSTILE.includes(a) ? "#c8574c" : "#7d9099"}" stroke-width="${HOSTILE.includes(a) ? 1.8 : 1.1}" opacity=".75"/>`;
  }
  return s;
}
function capitals(v: V, minPop: number, dim = 1) {
  let s = "";
  const seen = new Set<string>();
  for (const f of nePlaces.features) {
    const p = f.properties; if (p.pop_max < minPop || seen.has(p.name)) continue;
    if (!(p.adm0cap || p.pop_max > 2.5e6)) continue;
    const [x, y] = v.P(f.geometry.coordinates[0], f.geometry.coordinates[1]);
    if (x < 20 || y < 20 || x > v.W - 20 || y > v.H - 20) continue;
    seen.add(p.name);
    s += `<g opacity="${dim}" font-family="Jakarta"><circle cx="${r1(x)}" cy="${r1(y)}" r="3.2" fill="#e8eef0" stroke="#0f1416" stroke-width="1.2"/><text x="${r1(x + 7)}" y="${r1(y + 4)}" font-size="12.5" fill="#dbe2e5" filter="url(#shadow)">${p.name}</text></g>`;
  }
  return s;
}

const outD = process.env.OUT ?? ".";
const which = (process.env.ONLY ?? "strat,nuke,drones,phone").split(",");

// ===== A. the strategy map with community territories (Civ-style borders) =====
if (which.includes("strat")) {
  const v = view([2.6, 50.35, 9.4, 53.95], 1600, 1000);
  let s = `<g filter="url(#paper)">` + baseMap(v, { provinces: true, rails: true }) + `</g>`;
  const R = 15, w = Math.sqrt(3) * R, h = 1.5 * R;
  const cell = (col: number, row: number): [number, number] => [col * w + (row % 2 ? w / 2 : 0), row * h];
  const rows = Math.ceil(v.H / h) + 1, cols = Math.ceil(v.W / w) + 1;
  const hx = hexCentres(v.W, v.H, R).map(([x, y]) => hexPath(x, y, R)).join("");
  s += `<path d="${hx}" fill="none" stroke="#fff8e0" stroke-width=".6" opacity=".16" clip-path="url(#landclip)"/>`;
  // invented communities' reach, as a set of hexes around a seat, Civ-style tinted territory with a glowing edge
  const comms = [
    { name: "Groningen", lon: 6.567, lat: 53.219, km: 34, col: "#5cc6c0" },
    { name: "Utrecht", lon: 5.12, lat: 52.09, km: 30, col: "#f2b84b" },
    { name: "Eindhoven", lon: 5.478, lat: 51.441, km: 26, col: "#c58be0" },
    { name: "Gent", lon: 3.72, lat: 51.054, km: 22, col: "#7fb1ff" },
  ];
  const owner: (number | -1)[][] = [];
  for (let r = 0; r < rows; r++) { owner[r] = []; for (let c = 0; c < cols; c++) {
    const [x, y] = cell(c, r); const [lon, lat] = v.inv(x, y); let o = -1, best = 1e9;
    comms.forEach((m, i) => { const d = kmBetween([lon, lat], [m.lon, m.lat]) / m.km; if (d < 1 && d < best) { best = d; o = i; } });
    if (o >= 0 && !isLand(lon, lat)) o = -1;
    owner[r][c] = o;
  } }
  const nb = (c: number, r: number) => { const odd = r % 2; return [[c + 1, r], [c - 1, r], [c + (odd ? 1 : 0), r - 1], [c - (odd ? 0 : 1), r - 1], [c + (odd ? 1 : 0), r + 1], [c - (odd ? 0 : 1), r + 1]]; };
  // hex corner i at angle 60i-30; edge between corners i and i+1 faces neighbour direction:
  const edgeDir = [[1, 0], [0, 1], [-1, 1], [-1, 0], [-1, -1], [0, -1]]; // placeholder, resolved below by geometry
  comms.forEach((m, i) => {
    let fill = "", edge = "";
    for (let r = 0; r < rows; r++) for (let c = 0; c < cols; c++) {
      if (owner[r][c] !== i) continue;
      const [x, y] = cell(c, r); fill += hexPath(x, y, R);
      const corners = [...Array(6)].map((_, k) => { const a = (Math.PI / 180) * (60 * k - 30); return [x + R * Math.cos(a), y + R * Math.sin(a)]; });
      for (let k = 0; k < 6; k++) {
        const a = corners[k], b = corners[(k + 1) % 6];
        const mx = (a[0] + b[0]) / 2, my = (a[1] + b[1]) / 2; // neighbour centre is mirror of own centre over this edge midpoint
        const nx = 2 * mx - x, ny = 2 * my - y;
        const nr = Math.round(ny / h), nc = Math.round((nx - (nr % 2 ? w / 2 : 0)) / w);
        const o = owner[nr]?.[nc] ?? -1;
        if (o !== i) edge += `M${r1(a[0])} ${r1(a[1])}L${r1(b[0])} ${r1(b[1])}`;
      }
    }
    void edgeDir;
    s += `<path d="${fill}" fill="${m.col}" opacity=".16"/>`;
    s += `<path d="${edge}" fill="none" stroke="${m.col}" stroke-width="9" opacity=".22" stroke-linecap="round"/>`;
    s += `<path d="${edge}" fill="none" stroke="${m.col}" stroke-width="2.4" opacity=".95" stroke-linecap="round"/>`;
  });
  // fog of data: east of the Ems the map has no fresh community data (invented)
  s += `<defs><linearGradient id="fog" x1="0" x2="1"><stop offset="0" stop-color="#cfd8dc" stop-opacity="0"/><stop offset=".35" stop-color="#cfd8dc" stop-opacity=".30"/><stop offset="1" stop-color="#cfd8dc" stop-opacity=".42"/></linearGradient>
    <pattern id="fogh" width="10" height="10" patternUnits="userSpaceOnUse" patternTransform="rotate(35)"><path d="M0 5h10" stroke="#e9eef0" stroke-width="1" opacity=".25"/></pattern></defs>`;
  const [fx] = v.P(7.6, 52);
  s += `<g clip-path="url(#landclip)"><rect x="${r1(fx)}" y="0" width="${r1(v.W - fx)}" height="${v.H}" fill="url(#fog)" filter="url(#wobble)"/><rect x="${r1(fx + 120)}" y="0" width="${r1(v.W - fx)}" height="${v.H}" fill="url(#fogh)"/></g>`;
  s += countryLines(v, ["NLD"], "#fd6012", 3);
  s += countryLines(v, ["BEL", "DEU"], "#d9cbb0", 1.5);
  s += placeBanners(v, cities, 24, (c) => banner(v, c, {}));
  // a stack of people, Civ-unit style counters (invented)
  for (const p of [{ lon: 5.0, lat: 52.37, n: 38 }, { lon: 4.48, lat: 51.92, n: 14 }, { lon: 6.57, lat: 53.22, n: 21 }, { lon: 5.48, lat: 51.44, n: 9 }]) {
    const [x, y] = v.P(p.lon, p.lat);
    s += `<g transform="translate(${r1(x + 22)},${r1(y + 10)})" font-family="Jakarta" filter="url(#shadow)"><path d="M0 0h30v22l-15 8l-15 -8z" fill="#13191b" stroke="#5cc6c0" stroke-width="1.6"/><circle cx="15" cy="8" r="3.2" fill="#5cc6c0"/><path d="M9 17c0-3.5 12-3.5 12 0" stroke="#5cc6c0" stroke-width="2" fill="none"/><text x="15" y="44" font-size="12" fill="#f3f7f9" text-anchor="middle">${p.n}</text></g>`;
  }
  await Bun.write(`${outD}/strat.svg`, svgWrap(v, s));
  console.log("strat done");
}

// ===== B. a nuclear exchange at continent scale: fallout as a hex field =====
if (which.includes("nuke")) {
  const v = view([-5, 43.2, 33, 60.5], 1600, 1000);
  let s = politicalBase(v, { regionsOf: EUR });
  // sources: the publicly named sharing bases (100 kt surface, assumed) and an illustrative cloud of strikes on forces
  const src: { lon: number; lat: number; kt: number }[] = bases.slice(0, 5).map((b) => ({ lon: b.lon, lat: b.lat, kt: 100 }));
  let rng = 11; const rnd = () => ((rng = (rng * 16807) % 2147483647) / 2147483647);
  for (let i = 0; i < 70; i++) src.push({ lon: 15 + rnd() * 13, lat: 50 + rnd() * 6.5, kt: 10 + rnd() * 40 });
  for (let i = 0; i < 40; i++) src.push({ lon: 27 + rnd() * 6, lat: 52 + rnd() * 5, kt: 10 + rnd() * 40 });
  // wind field: from the west-south-west, veering with latitude
  const windTo = (lat: number) => 68 + (lat - 50) * 2.2; // degrees from north, blowing towards
  const dose = (lon: number, lat: number) => {
    let d = 0;
    for (const q of src) {
      const kx = 111.32 * Math.cos((q.lat * Math.PI) / 180);
      const dx = (lon - q.lon) * kx, dy = (lat - q.lat) * 110.57;
      const a = (windTo(q.lat) * Math.PI) / 180;
      const along = dx * Math.sin(a) + dy * Math.cos(a), across = dx * Math.cos(a) - dy * Math.sin(a);
      if (along < -15) continue;
      // 48 h outdoor dose along the plume axis, fitted to the 100 kt H+1 contours of v1-nuclear
      // (1000/100/10/1 rad/h at 9/32/80/150 km, times ~5 for 48 h): a power law, scaled by yield
      const s = Math.pow(q.kt / 100, 0.45), al = Math.max(along, 0) / s, wd = (1.5 + 0.1 * al) * s;
      d += 8000 * Math.pow(al + 3, -2.3) * Math.exp(-((across / wd) ** 2)) * (along < 0 ? Math.exp(along / 3) : 1);
    }
    return d; // Gy in the first 48 h outdoors, illustrative scale
  };
  const bands = [ { min: 10, col: "#c9262a" }, { min: 1, col: "#ec6a2c" }, { min: 0.1, col: "#f2c14c" }, { min: 0.02, col: "#9cc7a8" } ];
  const R = 4.5;
  let hexes = "";
  const byBand: string[] = bands.map(() => "");
  for (const [x, y] of hexCentres(v.W, v.H, R)) {
    const [lon, lat] = v.inv(x, y);
    let d = dose(lon, lat);
    for (let k = 0; k < 6; k++) { const a = (k * Math.PI) / 3; const [l2, b2] = v.inv(x + Math.cos(a) * R * 0.6, y + Math.sin(a) * R * 0.6); d = Math.max(d, dose(l2, b2)); }
    const bi = bands.findIndex((b) => d >= b.min);
    if (bi >= 0) byBand[bi] += hexPath(x, y, R - 0.6);
  }
  bands.forEach((b, i) => { hexes += `<path d="${byBand[bands.length - 1 - i] ? byBand[bands.length - 1 - i] : ""}" fill="${bands[bands.length - 1 - i].col}" opacity=".62"/>`; });
  s += `<g>${hexes}</g>`;
  for (const q of src) { const [x, y] = v.P(q.lon, q.lat); s += `<circle cx="${r1(x)}" cy="${r1(y)}" r="${q.kt >= 100 ? 4 : 2}" fill="#fff1c9" opacity="${q.kt >= 100 ? 1 : 0.7}"/>`; }
  for (const b of bases.slice(0, 5)) { const [x, y] = v.P(b.lon, b.lat); s += `<text x="${r1(x - 8)}" y="${r1(y - 8)}" text-anchor="end" font-family="Jakarta" font-size="12.5" fill="#fff1c9" filter="url(#shadow)">${b.name}</text>`; }
  s += capitals(v, 1.2e6, 0.85);
  // where you are: Groningen
  const [gx, gy] = v.P(6.567, 53.219);
  s += `<g transform="translate(${r1(gx)},${r1(gy)})"><circle r="26" fill="#5cc6c0" opacity=".18"/><circle r="9" fill="#13191b" stroke="#5cc6c0" stroke-width="3"/><circle r="3.5" fill="#5cc6c0"/></g>`;
  // wind arrows
  for (const [lon, lat] of [[0, 47], [2, 55], [12, 45.5], [22, 58.6], [-2, 51]]) {
    const [x, y] = v.P(lon, lat);
    s += `<g transform="translate(${r1(x)},${r1(y)}) rotate(${windTo(lat)})" opacity=".55"><path d="M0 18 L0 -16 M-6 -8 L0 -17 L6 -8" stroke="#cfe3ea" stroke-width="2" fill="none"/></g>`;
  }
  console.log("Groningen dose", dose(6.567, 53.219).toFixed(3), "Berlin", dose(13.4, 52.52).toFixed(2), "Warsaw", dose(21.0, 52.23).toFixed(2));
  await Bun.write(`${outD}/nuke.svg`, svgWrap(v, s));
}

// ===== C. a drone wave at night: regions light up by time to arrival =====
type Group = { id: string; t0: number; n: number; pts: number[][]; downed: number };
const SPEED = 170; // km/h, a slow one-way attack drone
function posAt(g: Group, t: number): number[] | null {
  let dist = ((t - g.t0) / 60) * SPEED; if (dist < 0) return null;
  for (let i = 0; i < g.pts.length - 1; i++) {
    const L = kmBetween(g.pts[i], g.pts[i + 1]);
    if (dist <= L) { const f = dist / L; return [g.pts[i][0] + (g.pts[i + 1][0] - g.pts[i][0]) * f, g.pts[i][1] + (g.pts[i + 1][1] - g.pts[i][1]) * f]; }
    dist -= L;
  }
  return null;
}
function drawGroups(v: V, groups: Group[], now: number, horizon: number) {
  let s = "";
  for (const g of groups) {
    // past
    let past = "", fut = "", cone = "";
    for (let t = g.t0; t <= now; t += 4) { const p = posAt(g, t); if (!p) break; const [x, y] = v.P(p[0], p[1]); past += (past ? "L" : "M") + r1(x) + " " + r1(y); }
    const head = posAt(g, now);
    if (!head) continue;
    const left: string[] = [], right: string[] = [];
    let prev = head;
    for (let t = now; t <= now + horizon; t += 5) {
      const p = posAt(g, t); if (!p) break;
      const [x, y] = v.P(p[0], p[1]); fut += (fut ? "L" : "M") + r1(x) + " " + r1(y);
      const [px, py] = v.P(prev[0], prev[1]); const dx = x - px, dy = y - py, l = Math.hypot(dx, dy) || 1;
      const spread = v.kmPx(6 + (t - now) * 0.55, p[1]);
      left.push(`${r1(x - (dy / l) * spread)} ${r1(y + (dx / l) * spread)}`); right.unshift(`${r1(x + (dy / l) * spread)} ${r1(y - (dx / l) * spread)}`);
      prev = p;
    }
    if (left.length > 2) cone = `M${left.join("L")}L${right.join("L")}Z`;
    s += `<path d="${cone}" fill="#ff7a4a" opacity=".13" stroke="#ff9a6a" stroke-opacity=".35" stroke-width="1"/>`;
    s += `<path d="${past}" fill="none" stroke="#ffb27a" stroke-width="5" opacity=".16" stroke-linecap="round"/><path d="${past}" fill="none" stroke="#ffcf9a" stroke-width="1.6" opacity=".75"/>`;
    s += `<path d="${fut}" fill="none" stroke="#ffcf9a" stroke-width="1.6" stroke-dasharray="3 6" opacity=".8"/>`;
    // ETA ticks every 30 min
    for (let t = Math.ceil(now / 30) * 30; t <= now + horizon; t += 30) {
      const p = posAt(g, t); if (!p) break; const [x, y] = v.P(p[0], p[1]);
      const hh = Math.floor((22 * 60 + t) / 60) % 24, mm = (22 * 60 + t) % 60;
      s += `<circle cx="${r1(x)}" cy="${r1(y)}" r="2.6" fill="#ffcf9a"/><text x="${r1(x)}" y="${r1(y - 8)}" font-family="Jakarta" font-size="10.5" fill="#ffd9b0" text-anchor="middle" opacity=".85">${String(hh).padStart(2, "0")}:${String(mm).padStart(2, "0")}</text>`;
    }
    // interceptions along the past track
    for (let k = 1; k <= 3; k++) { const p = posAt(g, g.t0 + ((now - g.t0) * (0.35 + 0.2 * k))); if (!p) continue; const [x, y] = v.P(p[0], p[1]); s += `<path d="M${r1(x - 4)} ${r1(y - 4)}L${r1(x + 4)} ${r1(y + 4)}M${r1(x + 4)} ${r1(y - 4)}L${r1(x - 4)} ${r1(y + 4)}" stroke="#9fd3ff" stroke-width="1.8" opacity=".85"/>`; }
    // the head: a cluster
    const [hx, hy] = v.P(head[0], head[1]);
    s += `<circle cx="${r1(hx)}" cy="${r1(hy)}" r="20" fill="#ff6a3a" opacity=".35" filter="url(#glow)"/>`;
    for (let k = 0; k < 7; k++) { const a = k * 2.4, rr = 3 + (k % 3) * 3.2; s += `<circle cx="${r1(hx + Math.cos(a) * rr)}" cy="${r1(hy + Math.sin(a) * rr)}" r="2.2" fill="#fff1c9"/>`; }
    s += `<g transform="translate(${r1(hx + 14)},${r1(hy + 14)})" font-family="Jakarta" filter="url(#shadow)"><rect width="${g.id.length * 7 + 54}" height="20" rx="10" fill="#13191b" stroke="#ff8a5a"/><text x="9" y="14" font-size="11.5" fill="#ffd9b0">${g.id} · ${g.n - g.downed}</text></g>`;
  }
  return s;
}
function regionStates(regs: Region[], groups: Group[], now: number) {
  const st = new Map<Region, { k: "now" | "soon" | "later" | "passed"; eta: number }>();
  for (const r of regs) {
    let best = 1e9, wasIn = false;
    for (const g of groups) {
      for (let t = g.t0; t <= now + 180; t += 4) {
        const p = posAt(g, t); if (!p) break;
        if (inRegion(r, p[0], p[1])) { if (t <= now - 8) wasIn = true; else best = Math.min(best, Math.max(t - now, 0)); }
      }
    }
    if (best <= 0) st.set(r, { k: "now", eta: 0 });
    else if (best <= 60) st.set(r, { k: "soon", eta: best });
    else if (best <= 180) st.set(r, { k: "later", eta: best });
    else if (wasIn) st.set(r, { k: "passed", eta: -1 });
  }
  return st;
}
const STATE_COL = { now: "#d63a2f", soon: "#ef8a2e", later: "#d9c25a", passed: "#5a7f99" } as const;
function fillRegions(v: V, st: Map<Region, { k: keyof typeof STATE_COL; eta: number }>) {
  let s = "";
  for (const [r, x] of st) {
    const d = path(v, r.g, true, 0.8);
    s += `<path d="${d}" fill="${STATE_COL[x.k]}" opacity="${x.k === "passed" ? 0.35 : x.k === "now" ? 0.6 : 0.42}" stroke="${STATE_COL[x.k]}" stroke-width="1.2" stroke-opacity=".9"/>`;
    if (x.k === "passed") s += `<path d="${d}" fill="url(#fogh)"/>`;
  }
  return s;
}
const drone: Group[] = [
  { id: "Group A", t0: 30, n: 64, downed: 18, pts: [[30.5, 53.9], [23.8, 52.6], [19.0, 52.3], [14.5, 52.4], [11, 52.3], [7.5, 52.2], [5.5, 52.1]] },
  { id: "Group B", t0: 60, n: 41, downed: 9, pts: [[29.5, 55.5], [26.0, 55.8], [24.0, 56.3], [21.5, 56.8], [18, 57.5], [14.5, 56.0], [11.5, 55.3]] },
  { id: "Group C", t0: 0, n: 52, downed: 21, pts: [[31.0, 52.0], [24.0, 51.2], [21.0, 50.3], [18.0, 49.8], [15, 50.0], [12.5, 50.2]] },
  { id: "Group D", t0: 75, n: 36, downed: 6, pts: [[31.5, 54.5], [27, 54.6], [23.0, 54.4], [19.5, 54.6], [15.5, 54.4], [12.5, 54.3], [10, 54.3]] },
];
if (which.includes("drones")) {
  const v = view([4.5, 48.3, 33.5, 59.2], 1600, 1000);
  const now = 250; // 02:10
  const regs = regionsOf(EUR);
  let s = `<defs><pattern id="fogh" width="8" height="8" patternUnits="userSpaceOnUse" patternTransform="rotate(35)"><path d="M0 4h8" stroke="#cfe3ea" stroke-width="1" opacity=".35"/></pattern>
  <pattern id="launch" width="9" height="9" patternUnits="userSpaceOnUse" patternTransform="rotate(-35)"><path d="M0 4.5h9" stroke="#e46a5c" stroke-width="1.4" opacity=".5"/></pattern></defs>`;
  s += politicalBase(v, { regionsOf: EUR });
  const st = regionStates(regs, drone, now);
  s += fillRegions(v, st);
  // launch areas, broad and illustrative
  for (const [lon, lat, rx, ry] of [[30.7, 53.2, 70, 110], [30.0, 55.4, 55, 45]]) { const [x, y] = v.P(lon, lat); s += `<ellipse cx="${r1(x)}" cy="${r1(y)}" rx="${rx}" ry="${ry}" fill="url(#launch)" stroke="#e46a5c" stroke-dasharray="4 4" opacity=".9"/>`; }
  s += drawGroups(v, drone, now, 150);
  s += capitals(v, 1e6, 0.8);
  const counts: Record<string, number> = { now: 0, soon: 0, later: 0, passed: 0 };
  const names: Record<string, string[]> = { now: [], soon: [], later: [], passed: [] };
  for (const [r, x] of st) { counts[x.k]++; names[x.k].push(`${r.name} (${r.a3}${x.k === "soon" || x.k === "later" ? " " + x.eta + "m" : ""})`); }
  console.log("drone regions", JSON.stringify(counts), JSON.stringify(names));
  for (const g of drone) { const p = posAt(g, now); console.log(g.id, p?.map((n) => n.toFixed(2))); }
  await Bun.write(`${outD}/drones.svg`, svgWrap(v, s));
}

// ===== D. a phone: one person's view of a warning, the Netherlands =====
if (which.includes("phone")) {
  const v = view([3.3, 50.72, 7.5, 53.58], 780, 1180);
  const now = 0;
  const g: Group[] = [{ id: "Drones", t0: -60, n: 12, downed: 3, pts: [[10.2, 52.05], [7.4, 52.3], [5.9, 52.2], [4.6, 52.0]] }];
  const regs = regionsOf(["NLD", "DEU", "BEL"]);
  let s = `<defs><pattern id="fogh" width="8" height="8" patternUnits="userSpaceOnUse" patternTransform="rotate(35)"><path d="M0 4h8" stroke="#cfe3ea" stroke-width="1" opacity=".35"/></pattern></defs>`;
  s += `<g filter="url(#paper)">` + baseMap(v, { mute: 0.5, provinces: true }) + `</g>`;
  const st = regionStates(regs, g, now);
  s += fillRegions(v, st);
  s += countryLines(v, ["NLD"], "#fd6012", 2.2);
  s += drawGroups(v, g, now, 120);
  s += placeBanners(v, cities, 9, (c) => banner(v, c, { small: true }));
  const [px, py] = v.P(5.969, 52.211); // Apeldoorn
  s += `<g transform="translate(${r1(px)},${r1(py)})"><circle r="34" fill="#5cc6c0" opacity=".2"/><circle r="15" fill="#13191b" stroke="#5cc6c0" stroke-width="4"/><circle r="6" fill="#5cc6c0"/></g>`;
  for (const [r, x] of st) console.log("phone", r.name, x.k, x.eta);
  await Bun.write(`${outD}/phone.svg`, svgWrap(v, s));
}
console.log("all done");

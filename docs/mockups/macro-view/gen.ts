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

if (!process.env.FRAMES) {
// ===== 1. the macro view of the Netherlands ===============================
{
  const v = view([3.1, 50.68, 7.35, 53.62], 1600, 1000);
  let s = `<g filter="url(#paper)">` + baseMap(v, { provinces: true, rails: true }) + `</g>`;
  // hex grid over land, faint
  const hx = hexCentres(v.W, v.H, 22).map(([x, y]) => hexPath(x, y, 22)).join("");
  s += `<path d="${hx}" fill="none" stroke="#fff8e0" stroke-width=".7" opacity=".22" clip-path="url(#landclip)"/>`;
  s += countryLines(v, ["NLD"], "#fd6012", 3.5);
  s += countryLines(v, ["BEL", "DEU"], "#d9cbb0", 1.6);
  s += placeBanners(v, cities, 26, (c, i) => banner(v, c, { badge: i < 9 ? String([12, 9, 8, 7, 5, 4, 4, 3, 3][i]) : undefined }));
  await Bun.write(`${out}/map-nl.svg`, svgWrap(v, s));
  // hex lenses
  const R = 20, cs = hexCentres(v.W, v.H, R);
  const lens: { x: number; y: number; k: Record<string, number>; n: number }[] = [];
  for (const [x, y] of cs) {
    const k: Record<string, number> = {}; let n = 0;
    for (const [dx, dy] of [[0, 0], [8, 0], [-8, 0], [4, 7], [-4, 7], [4, -7], [-4, -7], [12, 6], [-12, -6]]) {
      const [lon, lat] = v.inv(x + dx, y + dy);
      if (!isLand(lon, lat)) continue; n++;
      const kk = kindAt(lon, lat) ?? "farmland"; k[kk] = (k[kk] ?? 0) + 1;
    }
    if (n >= 3) lens.push({ x, y, k, n });
  }
  // terrain lens: majority class per hex
  let t = `<g filter="url(#paper)">` + baseMap(v, { lc: false, roads: false, mute: 0.0 }) + `</g>`;
  for (const h of lens) {
    const top = Object.entries(h.k).sort((a, b) => b[1] - a[1])[0][0];
    t += `<path d="${hexPath(h.x, h.y, R - 1)}" fill="${LC[top]}" stroke="#2b2118" stroke-width="1.2" stroke-opacity=".35"/>`;
    if (top === "forest") t += `<path d="${hexPath(h.x, h.y, R - 1)}" fill="url(#trees)"/>`;
  }
  t += countryLines(v, ["NLD"], "#fd6012", 3);
  await Bun.write(`${out}/lens-terrain.svg`, svgWrap(v, t));
  // built-up lens: residential share per hex
  let u = baseMap(v, { lc: false, roads: false, mute: 0.62 });
  for (const h of lens) {
    const f = (h.k.residential ?? 0) / h.n;
    if (f <= 0) continue;
    const col = f > 0.66 ? "#fd6012" : f > 0.33 ? "#fd9212" : "#f3c66b";
    u += `<path d="${hexPath(h.x, h.y, R - 1.5)}" fill="${col}" opacity="${0.35 + 0.6 * f}"/>`;
  }
  u += countryLines(v, ["NLD"], "#c7d3d8", 2);
  u += placeBanners(v, cities, 14, (c) => banner(v, c, { small: true }));
  await Bun.write(`${out}/lens-builtup.svg`, svgWrap(v, u));
  // mobility lens
  let m = baseMap(v, { lc: false, roads: false, mute: 0.7 });
  const maj = feats(roads, (p) => p.continent === "Europe" && p.type === "Major Highway").map((f: any) => path(v, f.geometry, false)).join("");
  const sec = feats(roads, (p) => p.continent === "Europe" && p.type === "Secondary Highway").map((f: any) => path(v, f.geometry, false)).join("");
  const rl = rails.features.map((f: any) => path(v, f.geometry, false)).join("");
  m += `<path d="${sec}" fill="none" stroke="#c7d3d8" stroke-width="1.6" opacity=".55"/>`;
  m += `<path d="${maj}" fill="none" stroke="#ffd98a" stroke-width="6" opacity=".25" filter="url(#glow)"/><path d="${maj}" fill="none" stroke="#e9c97e" stroke-width="2.6"/>`;
  m += `<path d="${rl}" fill="none" stroke="#5cc6c0" stroke-width="2" stroke-dasharray="7 4"/>`;
  m += countryLines(v, ["NLD"], "#c7d3d8", 1.5);
  m += placeBanners(v, cities, 14, (c) => banner(v, c, { small: true }));
  await Bun.write(`${out}/lens-mobility.svg`, svgWrap(v, m));
}

// ===== 2. a nuclear exchange, Europe ========================================
const NATO = ["ALB","BEL","BGR","CAN","HRV","CZE","DNK","EST","FIN","FRA","DEU","GRC","HUN","ISL","ITA","LVA","LTU","LUX","MNE","NLD","MKD","NOR","POL","PRT","ROU","SVK","SVN","ESP","SWE","TUR","GBR","USA"];
const RUS = ["RUS", "BLR"];
const bases = [
  { name: "Volkel", lon: 5.7086, lat: 51.6564 }, { name: "Kleine Brogel", lon: 5.47, lat: 51.1683 },
  { name: "Büchel", lon: 7.0633, lat: 50.1736 }, { name: "Aviano", lon: 12.5961, lat: 46.0319 },
  { name: "Ghedi", lon: 10.2678, lat: 45.4322 }, { name: "Incirlik", lon: 35.4259, lat: 37.0021 },
];
{
  const v = view([-11, 35.5, 44, 63], 1600, 1000);
  let s = `<rect width="${v.W}" height="${v.H}" fill="#0f2230"/>`;
  for (const f of countries.features) {
    const a = f.properties.ADM0_A3;
    const fill = NATO.includes(a) ? "#2c4a63" : RUS.includes(a) ? "#5a2a2a" : "#3a3f3f";
    const stroke = NATO.includes(a) ? "#6fa3c9" : RUS.includes(a) ? "#d0645a" : "#6c7474";
    s += `<path d="${path(v, f.geometry, true, 1)}" fill="${fill}" stroke="${stroke}" stroke-width="1" stroke-opacity=".6"/>`;
  }
  // bloc borders, Civ-like glow
  const blocD = (codes: string[]) => countries.features.filter((f: any) => codes.includes(f.properties.ADM0_A3)).map((f: any) => path(v, f.geometry, true, 1)).join("");
  s += `<path d="${blocD(RUS)}" fill="none" stroke="#ff5a4a" stroke-width="3" opacity=".9"/>`;
  // plan A, phase 1: missile arcs from the Russian side to the known sharing bases, plus a haze of the ~300
  const launch = [{ lon: 20.5, lat: 54.7 }, { lon: 27.5, lat: 53.9 }, { lon: 37.6, lat: 55.7 }, { lon: 39.7, lat: 47.2 }];
  let arcs = "";
  bases.forEach((b, i) => {
    const L = launch[i % launch.length];
    const [x0, y0] = v.P(L.lon, L.lat), [x1, y1] = v.P(b.lon, b.lat);
    const mx = (x0 + x1) / 2, my = Math.min(y0, y1) - 90;
    arcs += `<path d="M${r1(x0)} ${r1(y0)} Q${r1(mx)} ${r1(my)} ${r1(x1)} ${r1(y1)}" fill="none" stroke="#ffb27a" stroke-width="1.6" stroke-dasharray="6 5" opacity=".85"/>`;
  });
  s += arcs;
  // haze of strikes on forces (scattered, not individual targets)
  let rng = 7; const rnd = () => ((rng = (rng * 16807) % 2147483647) / 2147483647);
  let haze = "";
  for (let i = 0; i < 150; i++) {
    const lon = 14 + rnd() * 20, lat = 49 + rnd() * 9;
    const [x, y] = v.P(lon, lat);
    haze += `<circle cx="${r1(x)}" cy="${r1(y)}" r="${r1(2 + rnd() * 3)}" fill="#ffcf8a" opacity="${r1(0.25 + rnd() * 0.4)}"/>`;
  }
  s += `<g>${haze}</g>`;
  for (const b of bases) {
    const [x, y] = v.P(b.lon, b.lat);
    s += `<circle cx="${r1(x)}" cy="${r1(y)}" r="22" fill="#ff8a3a" opacity=".35" filter="url(#glow)"/><circle cx="${r1(x)}" cy="${r1(y)}" r="7" fill="#fff1c9" stroke="#ff6a1a" stroke-width="3"/>`;
    s += `<text x="${r1(x + 12)}" y="${r1(y + 4)}" font-family="Jakarta" font-size="13" fill="#fff1c9" filter="url(#shadow)">${b.name}</text>`;
  }
  // the warning shot
  const [wx, wy] = v.P(20.0, 55.6);
  s += `<circle cx="${r1(wx)}" cy="${r1(wy)}" r="14" fill="none" stroke="#fff1c9" stroke-width="2"/><circle cx="${r1(wx)}" cy="${r1(wy)}" r="26" fill="none" stroke="#fff1c9" stroke-width="1" opacity=".5"/>`;
  s += `<text x="${r1(wx - 10)}" y="${r1(wy - 30)}" font-family="Jakarta" font-size="13" fill="#fff1c9" text-anchor="middle">warning shot</text>`;
  await Bun.write(`${out}/map-eu.svg`, svgWrap(v, s));
}

// ===== 3. the same, zoomed onto the Low Countries ==========================
// Effects of an assumed 100 kt surface burst (Glasstone & Dolan scaling, as NUKEMAP uses):
const effects = [
  { key: "fireball", km: 0.5, col: "#fff3c4" }, { key: "20 psi", km: 1.2, col: "#ff4a2a" },
  { key: "5 psi", km: 2.3, col: "#ff8a3a" }, { key: "3rd-degree burns", km: 4.3, col: "#ffc35a" },
  { key: "1 psi", km: 5.4, col: "#c7d3d8" },
];
// fallout plume, dose rate at H+1, illustrative WSEG-style ellipses for 100 kt, 24 km/h wind
const plume = [ { rad: 1, len: 150, wid: 26, col: "#c9e36b" }, { rad: 10, len: 80, wid: 14, col: "#f2d04c" },
  { rad: 100, len: 32, wid: 6, col: "#f08a2c" }, { rad: 1000, len: 9, wid: 2.4, col: "#d9302c" } ];
const windTo = 60; // degrees from north the wind blows towards (a south-westerly)
{
  const v = view([3.3, 49.75, 8.6, 52.95], 1600, 1000);
  let s = `<g filter="url(#paper)">` + baseMap(v, { mute: 0.45, provinces: false }) + `</g>`;
  s += countryLines(v, ["NLD"], "#fd6012", 2.5);
  s += countryLines(v, ["BEL", "DEU"], "#d9cbb0", 1.4);
  const inside: Record<number, string[]> = {};
  for (const b of bases.slice(0, 3)) {
    const [x, y] = v.P(b.lon, b.lat);
    const pk = v.kmPx(1, b.lat);
    const a = ((windTo - 90) * Math.PI) / 180; // svg angle
    for (const p of plume) {
      const L = p.len * pk, Wd = p.wid * pk;
      // teardrop: ellipse whose upwind tip sits just behind ground zero
      const cx = x + Math.cos(a) * (L / 2 - 2 * pk), cy = y + Math.sin(a) * (L / 2 - 2 * pk);
      s += `<ellipse cx="${r1(cx)}" cy="${r1(cy)}" rx="${r1(L / 2)}" ry="${r1(Wd / 2)}" transform="rotate(${windTo - 90} ${r1(cx)} ${r1(cy)})" fill="${p.col}" opacity=".42" stroke="${p.col}" stroke-width="1.5" stroke-opacity=".9"/>`;
      // which cities fall inside
      for (const c of cities) {
        const dx = (c.lon - b.lon) * 111.32 * Math.cos((b.lat * Math.PI) / 180), dy = (c.lat - b.lat) * 110.57;
        const ang = (windTo * Math.PI) / 180;
        const along = dx * Math.sin(ang) + dy * Math.cos(ang) + 2, across = dx * Math.cos(ang) - dy * Math.sin(ang);
        const e = ((along - p.len / 2) / (p.len / 2)) ** 2 + (across / (p.wid / 2)) ** 2;
        if (e <= 1) (inside[p.rad] ??= []).includes(c.name) || inside[p.rad].push(c.name);
      }
    }
    for (const e of [...effects].reverse()) s += `<circle cx="${r1(x)}" cy="${r1(y)}" r="${r1(Math.max(e.km * pk, 1.5))}" fill="${e.col}" fill-opacity=".35" stroke="${e.col}" stroke-width="1.2"/>`;
    s += `<circle cx="${r1(x)}" cy="${r1(y)}" r="30" fill="#ffb060" opacity=".45" filter="url(#glow)"/>`;
    s += `<text x="${r1(x - 12)}" y="${r1(y + 22)}" text-anchor="end" font-family="Jakarta" font-size="14" fill="#fff1c9" filter="url(#shadow)">${b.name}</text>`;
  }
  s += placeBanners(v, cities, 22, (c) => banner(v, c, { small: true }));
  // wind arrow
  s += `<g transform="translate(110,880)" font-family="Jakarta"><circle r="42" fill="#13191b" opacity=".85" stroke="rgba(255,255,255,.2)"/><g transform="rotate(${windTo})"><path d="M0 26 L0 -22 M-8 -12 L0 -24 L8 -12" stroke="#f3f7f9" stroke-width="3" fill="none"/></g><text y="62" text-anchor="middle" font-size="13" fill="#a5b2b7">wind SW · 24 km/h</text></g>`;
  // scale bar
  const k50 = v.kmPx(50, 51.5);
  s += `<g transform="translate(${v.W - 60 - k50},960)" font-family="Jakarta"><rect width="${r1(k50)}" height="6" fill="#f3f7f9"/><rect width="${r1(k50 / 2)}" height="6" fill="#13191b"/><text y="-8" font-size="12" fill="#f3f7f9">0</text><text x="${r1(k50)}" y="-8" font-size="12" fill="#f3f7f9" text-anchor="end">50 km</text></g>`;
  await Bun.write(`${out}/map-strike.svg`, svgWrap(v, s));
  console.log("inside plumes:", JSON.stringify(inside));
  // a close-up of the blast rings around Volkel
  const b = bases[0];
  const vz = view([b.lon - 0.16, b.lat - 0.085, b.lon + 0.16, b.lat + 0.085], 560, 440);
  let z = `<g filter="url(#paper)">` + baseMap(vz, { mute: 0.3 }) + `</g>`;
  const [x, y] = vz.P(b.lon, b.lat), pk = vz.kmPx(1, b.lat);
  for (const e of [...effects].reverse()) z += `<circle cx="${r1(x)}" cy="${r1(y)}" r="${r1(e.km * pk)}" fill="${e.col}" fill-opacity=".28" stroke="${e.col}" stroke-width="2"/>`;
  for (const e of effects.slice(1)) z += `<text x="${r1(x + e.km * pk * 0.71 + 4)}" y="${r1(y - e.km * pk * 0.71)}" font-family="Jakarta" font-size="12" fill="#f3f7f9" filter="url(#shadow)">${e.key} · ${e.km} km</text>`;
  for (const c of cities) {
    const [cx, cy] = vz.P(c.lon, c.lat);
    if (cx > 0 && cx < vz.W && cy > 0 && cy < vz.H) z += banner(vz, c, { small: true });
  }
  // nearby towns not in the city list (OSM place=town, positions from OSM)
  for (const t of [{ name: "Uden", lon: 5.61486, lat: 51.65909 }, { name: "Volkel", lon: 5.6557, lat: 51.64408 }, { name: "Zeeland", lon: 5.67291, lat: 51.69464 }, { name: "Veghel", lon: 5.54469, lat: 51.61345 }, { name: "Boekel", lon: 5.67268, lat: 51.6029 }]) {
    const [cx, cy] = vz.P(t.lon, t.lat);
    z += `<circle cx="${r1(cx)}" cy="${r1(cy)}" r="3.5" fill="#fff6dc" stroke="#2b2118" stroke-width="1.5"/><text x="${r1(cx + 7)}" y="${r1(cy - 6)}" font-family="Jakarta" font-size="13" fill="#fff6dc" filter="url(#shadow)">${t.name}</text>`;
  }
  await Bun.write(`${out}/map-volkel.svg`, svgWrap(vz, z));
}
}
console.log("done");

// ===== 4. frames matching the client's camera, for the transition strip =====
// Straight down, 45° vertical fov (the capture at 350 km lines up with England at the left edge).
if (process.env.FRAMES) {
  const frameBox = (lat: number, lon: number, altKm: number, W: number, H: number): BBox => {
    const hh = altKm * Math.tan(Math.PI / 8), hw = (hh * W) / H;
    const dLat = hh / 110.57, dLon = hw / (111.32 * Math.cos((lat * Math.PI) / 180));
    return [lon - dLon, lat - dLat, lon + dLon, lat + dLat];
  };
  const lcUtr = await J("lc_z10_utr.geojson");
  for (const f of [
    { name: "frame-40km", lat: 52.1, lon: 5.3, alt: 40, lcData: lcUtr, hex: 0, banners: 10 },
    { name: "frame-350km", lat: 52.2, lon: 5.5, alt: 350, lcData: lc, hex: 22, banners: 24 },
  ]) {
    const v = view(frameBox(f.lat, f.lon, f.alt, 1600, 900), 1600, 900);
    let s = `<g filter="url(#paper)">` + baseMap(v, { lcData: f.lcData, rails: true, provinces: true }) + `</g>`;
    if (f.hex) s += `<path d="${hexCentres(v.W, v.H, f.hex).map(([x, y]) => hexPath(x, y, f.hex)).join("")}" fill="none" stroke="#fff8e0" stroke-width=".7" opacity=".22" clip-path="url(#landclip)"/>`;
    s += countryLines(v, ["NLD"], "#fd6012", 3);
    s += placeBanners(v, cities, f.banners, (c) => banner(v, c, {}));
    await Bun.write(`${out}/${f.name}.svg`, svgWrap(v, s));
  }
}

// ===== 5. the 40 km frame from the client's own OSM tiles (Shortbread) =====
if (process.env.FRAMES) {
  const osm = await J("osm_utr.geojson");
  const cls: Record<string, string> = {};
  for (const k of ["forest"]) cls[k] = "forest";
  for (const k of ["scrub"]) cls[k] = "scrub";
  for (const k of ["farmland", "orchard", "plant_nursery", "greenhouse_horticulture", "farmyard", "vineyard"]) cls[k] = "farmland";
  for (const k of ["grass", "meadow", "grassland", "park", "garden", "recreation_ground", "golf_course", "village_green", "allotments", "greenfield", "playground", "miniature_golf"]) cls[k] = "grassland";
  for (const k of ["residential", "garages"]) cls[k] = "residential";
  for (const k of ["industrial", "commercial", "retail", "railway", "brownfield", "landfill", "quarry"]) cls[k] = "industrial";
  for (const k of ["sand", "beach", "scree"]) cls[k] = "bare_rock";
  for (const k of ["heath"]) cls[k] = "heath";
  for (const k of ["marsh", "wet_meadow", "swamp", "bog"]) cls[k] = "marsh";
  const PAL = { ...LC, industrial: "#8c7d74", grassland: "#6f8a55" } as Record<string, string>;
  const hh = 40 * Math.tan(Math.PI / 8), hw = (hh * 1600) / 900;
  const v = view([5.3 - hw / (111.32 * Math.cos((52.1 * Math.PI) / 180)), 52.1 - hh / 110.57, 5.3 + hw / (111.32 * Math.cos((52.1 * Math.PI) / 180)), 52.1 + hh / 110.57], 1600, 900);
  let s = `<rect width="1600" height="900" fill="#71895a"/>`;
  const by: Record<string, string> = {};
  for (const f of osm.features) if (f.properties.layer === "land" && cls[f.properties.kind]) by[cls[f.properties.kind]] = (by[cls[f.properties.kind]] ?? "") + path(v, f.geometry, true, 0.5);
  for (const k of ["farmland", "grassland", "scrub", "heath", "marsh", "bare_rock", "forest", "residential", "industrial"]) if (by[k]) {
    s += `<path d="${by[k]}" fill="${PAL[k]}" stroke="${PAL[k]}" stroke-width=".6"/>`;
    if (k === "forest") s += `<path d="${by[k]}" fill="url(#trees)"/>`;
    if (k === "marsh") s += `<path d="${by[k]}" fill="url(#reeds)"/>`;
  }
  const wat = osm.features.filter((f: any) => f.properties.layer === "water_polygons").map((f: any) => path(v, f.geometry, true, 0.5)).join("");
  s += `<path d="${wat}" fill="#36607e" stroke="#6f9db3" stroke-width="1"/>`;
  const st = (kinds: string[]) => osm.features.filter((f: any) => f.properties.layer === "streets" && kinds.includes(f.properties.kind)).map((f: any) => path(v, f.geometry, false, 0.8)).join("");
  const rail = st(["rail"]), sec = st(["secondary"]), pri = st(["primary"]), mot = st(["motorway", "trunk"]);
  s += `<path d="${rail}" fill="none" stroke="#3b3430" stroke-width="2.6" opacity=".6"/><path d="${rail}" fill="none" stroke="#efe6d2" stroke-width="1.2" stroke-dasharray="6 6"/>`;
  s += `<path d="${sec}" fill="none" stroke="#f3e2b8" stroke-width="1.3" opacity=".8"/>`;
  s += `<path d="${pri}" fill="none" stroke="#5a4636" stroke-width="4" opacity=".5"/><path d="${pri}" fill="none" stroke="#f3e2b8" stroke-width="2.2"/>`;
  s += `<path d="${mot}" fill="none" stroke="#4a3426" stroke-width="7" stroke-linecap="round" opacity=".7"/><path d="${mot}" fill="none" stroke="#e9c97e" stroke-width="3.8" stroke-linecap="round"/>`;
  s = `<g filter="url(#paper)">${s}</g>`;
  s += placeBanners(v, cities, 8, (c) => banner(v, c, {}));
  await Bun.write(`${out}/frame-40km.svg`, svgWrap(v, s));
}

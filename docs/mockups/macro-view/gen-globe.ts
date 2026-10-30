// The painted map wrapped onto the globe: a nadir camera at an altitude, 45° vertical fov like the client.
// Usage: bun gen-globe.ts <outdir>
import { geoPath, geoGraticule10, geoCircle } from "d3-geo";
import { geoSatellite } from "d3-geo-projection";
const out = process.argv[2] ?? ".";
const J = async (f: string) => Bun.file(f).json();
const urban = await J("ne_10m_urban_areas.geojson");
const [land, lakes, countries, places, lc3, lc4, eqlc] = await Promise.all([
  J("ne_50m_land.geojson"), J("ne_50m_lakes.geojson"), J("ne_50m_admin_0_countries.geojson"),
  J("ne_10m_populated_places_simple.geojson"), J("lc_z3.geojson"), J("lc_z4.geojson"), Promise.resolve(null)]);
const LC: Record<string, string> = { farmland: "#979262", grassland: "#6f8a55", forest: "#3e5a3c", residential: "#8a817a",
  marsh: "#6f9a86", heath: "#9c7d93", scrub: "#9aa25c", bare_rock: "#e6d6a6" };
const ER = 6371;
async function frame(altKm: number, W: number, H: number, lon: number, lat: number, lc: any, opt: { labels: number; hex?: boolean; zoom?: number; night?: [number, number]; clouds?: number }) {
  const P = (ER + altKm) / ER;
  const proj = geoSatellite().distance(P).tilt(0).rotate([-lon, -lat]).translate([W / 2, H / 2]).scale(1)
    .clipAngle((Math.acos(1 / P) * 180) / Math.PI - 1e-6).precision(0.3);
  // calibrate: a point θ north must sit at the pixel a 45° vertical fov puts it
  const th = 2 * Math.PI / 180;
  const alpha = Math.atan((ER * Math.sin(th)) / (ER + altKm - ER * Math.cos(th)));
  const want = (H / 2) / Math.tan(Math.PI / 8) * Math.tan(alpha) * (opt.zoom ?? 1);
  const got = H / 2 - proj([lon, lat + 2])![1];
  proj.scale(want / got);
  const pth = geoPath(proj);
  let s = `<svg xmlns="http://www.w3.org/2000/svg" width="${W}" height="${H}" viewBox="0 0 ${W} ${H}">
<style>@font-face{font-family:Jakarta;src:url(../../../assets/fonts/PlusJakartaSans-SemiBold.ttf)}</style>
<defs>
 <radialGradient id="space" cx="50%" cy="50%" r="75%"><stop offset="0" stop-color="#0b1622"/><stop offset="1" stop-color="#03060a"/></radialGradient>
 <radialGradient id="sea" cx="45%" cy="40%" r="60%"><stop offset="0" stop-color="#2f5878"/><stop offset="1" stop-color="#1c3149"/></radialGradient>
 <radialGradient id="shade" cx="40%" cy="35%" r="65%"><stop offset=".55" stop-color="#000" stop-opacity="0"/><stop offset="1" stop-color="#000" stop-opacity=".45"/></radialGradient>
 <filter id="lights" x="-50%" y="-50%" width="200%" height="200%"><feGaussianBlur stdDeviation="2.2"/></filter>
 <filter id="glow" x="-20%" y="-20%" width="140%" height="140%"><feGaussianBlur stdDeviation="14"/></filter>
 <filter id="paper"><feTurbulence type="fractalNoise" baseFrequency="0.9" numOctaves="2" seed="4" result="n"/><feColorMatrix in="n" type="matrix" values="0 0 0 0 0  0 0 0 0 0  0 0 0 0 0  0 0 0 .10 0" result="g"/><feComposite in="g" in2="SourceGraphic" operator="in" result="gg"/><feBlend in="SourceGraphic" in2="gg" mode="multiply"/></filter>
 <filter id="shadow" x="-20%" y="-20%" width="140%" height="160%"><feDropShadow dx="0" dy="2" stdDeviation="2.2" flood-color="#000" flood-opacity=".55"/></filter>
 <pattern id="trees" width="14" height="12" patternUnits="userSpaceOnUse"><path d="M4 9 l3 -7 l3 7z" fill="#2a4229" opacity=".35"/></pattern>
</defs>
<rect width="${W}" height="${H}" fill="url(#space)"/>`;
  let rng = 3; const rnd = () => ((rng = (rng * 16807) % 2147483647) / 2147483647);
  for (let i = 0; i < 260; i++) s += `<circle cx="${(rnd() * W).toFixed(1)}" cy="${(rnd() * H).toFixed(1)}" r="${(0.4 + rnd() * 0.9).toFixed(2)}" fill="#fff" opacity="${(0.3 + rnd() * 0.6).toFixed(2)}"/>`;
  const sphere = pth({ type: "Sphere" } as any);
  s += `<path d="${sphere}" fill="none" stroke="#6fb6e6" stroke-width="18" opacity=".55" filter="url(#glow)"/>`;
  s += `<clipPath id="sph"><path d="${sphere}"/></clipPath><g clip-path="url(#sph)" filter="url(#paper)"><path d="${sphere}" fill="url(#sea)"/>`;
  const landD = land.features.map((f: any) => pth(f)).join("");
  s += `<path d="${landD}" fill="none" stroke="#4f7f98" stroke-width="10" opacity=".35"/>`;
  s += `<path d="${landD}" fill="#7a8659"/>`;
  s += `<clipPath id="landc"><path d="${landD}"/></clipPath><g clip-path="url(#landc)">`;
  const by: Record<string, string> = {};
  for (const f of lc.features) if (f.properties.layer === "land") by[f.properties.kind] = (by[f.properties.kind] ?? "") + (pth(f) ?? "");
  for (const k of ["farmland", "grassland", "scrub", "heath", "marsh", "bare_rock", "forest", "residential"]) if (by[k]) {
    s += `<path d="${by[k]}" fill="${LC[k]}" stroke="${LC[k]}" stroke-width=".6"/>`;
    if (k === "forest") s += `<path d="${by[k]}" fill="url(#trees)"/>`;
  }
  s += `</g>`;
  s += `<path d="${lakes.features.map((f: any) => pth(f)).join("")}" fill="#36607e"/>`;
  s += `<path d="${countries.features.map((f: any) => pth(f)).join("")}" fill="none" stroke="#fff6dc" stroke-width=".9" opacity=".45"/>`;
  const nl = countries.features.find((f: any) => f.properties.ADM0_A3 === "NLD");
  s += `<path d="${pth(nl)}" fill="none" stroke="#fd6012" stroke-width="2.4"/>`;
  s += `<path d="${pth(geoGraticule10())}" fill="none" stroke="#e8f4ff" stroke-width=".6" opacity=".06"/>`;
  if (opt.clouds) {
    s += `<filter id="cl" x="0" y="0" width="100%" height="100%"><feTurbulence type="fractalNoise" baseFrequency="0.0045 0.007" numOctaves="5" seed="11"/><feColorMatrix type="matrix" values="0 0 0 0 1  0 0 0 0 1  0 0 0 0 1  0 0 0 2.6 -1.25"/></filter>`;
    s += `<rect width="${W}" height="${H}" filter="url(#cl)" opacity="${opt.clouds}"/>`;
  }
  if (opt.night) {
    const [slon, slat] = opt.night;
    const anti: [number, number] = [slon + 180, -slat];
    // twilight: stacked caps round the antisolar point, darkest in the middle
    const toV = (lo: number, la: number) => { const a = lo * Math.PI / 180, b = la * Math.PI / 180; return [Math.cos(b) * Math.cos(a), Math.cos(b) * Math.sin(a), Math.sin(b)]; };
    const sun = toV(slon, slat);
    const night = new Uint8Array(W * H);
    for (let y = 0; y < H; y++) for (let x = 0; x < W; x++) {
      const ll = proj.invert!([x + 0.5, y + 0.5]); if (!ll || !isFinite(ll[0]) || !pth({ type: "Point", coordinates: ll } as any)) { night[y * W + x] = x ? night[y * W + x - 1] : 0; continue; }
      const v = toV(ll[0], ll[1]); const c = v[0] * sun[0] + v[1] * sun[1] + v[2] * sun[2];
      const t = Math.min(1, Math.max(0, (0.06 - c) / 0.2)); // twilight band about 12 degrees wide
      night[y * W + x] = Math.round(255 * t * t * (3 - 2 * t));
    }
    const pgm = Buffer.concat([Buffer.from(`P5 ${W} ${H} 255\n`), Buffer.from(night)]);
    await Bun.write(`/tmp/s525-night.pgm`, pgm);
    Bun.spawnSync(["convert", "/tmp/s525-night.pgm", `${out}/night-mask.png`]);
    s += `<mask id="nm" maskUnits="userSpaceOnUse" x="0" y="0" width="${W}" height="${H}"><image href="night-mask.png" width="${W}" height="${H}"/></mask>`;
    s += `<rect width="${W}" height="${H}" fill="#040812" opacity=".78" mask="url(#nm)"/>`;
    s += `<g mask="url(#nm)">`;
    const ud = urban.features.map((u: any) => pth(u) ?? "").join("");
    s += `<path d="${ud}" fill="#ffb347" opacity=".55" filter="url(#lights)"/><path d="${ud}" fill="#ffe2a8" opacity=".9"/>`;
    for (const p of places.features) {
      const v = p.properties.pop_max; if (v < 150000) continue;
      const q = proj(p.geometry.coordinates); if (!q || !pth({ type: "Point", coordinates: p.geometry.coordinates } as any)) continue;
      s += `<circle cx="${q[0].toFixed(1)}" cy="${q[1].toFixed(1)}" r="${(0.6 + Math.sqrt(v) / 1400).toFixed(2)}" fill="#ffd27a" opacity=".85" filter="url(#lights)"/>`;
    }
    s += `</g>`;
  }
  s += `<path d="${sphere}" fill="url(#shade)"/></g>`;
  s += `<path d="${sphere}" fill="none" stroke="#9fd6ff" stroke-width="2" opacity=".5"/>`;
  // city banners: biggest places on the near side
  const cand = places.features.filter((f: any) => f.properties.pop_max > 0).sort((a: any, b: any) => b.properties.pop_max - a.properties.pop_max);
  const taken: number[][] = []; let n = 0;
  const fmt = (v: number) => (v >= 1e6 ? (v / 1e6).toFixed(1) + "M" : Math.round(v / 1e3) + "k");
  for (const f of cand) {
    if (n >= opt.labels) break;
    const [lo, la] = f.geometry.coordinates;
    const vis = pth({ type: "Point", coordinates: [lo, la] } as any);
    if (!vis) continue;
    const p = proj([lo, la]); if (!p) continue;
    const [x, y] = p; if (x < 60 || x > W - 60 || y < 50 || y > H - 20) continue;
    const name = f.properties.name; const pop = fmt(f.properties.pop_max);
    const tw = name.length * 7.6 + 8, pw = pop.length * 6.6 + 12, w = tw + pw + 10, h = 21;
    const box = [x - w / 2, y - 36, x + w / 2, y + 4];
    if (taken.some((t) => !(box[2] < t[0] || box[0] > t[2] || box[3] < t[1] || box[1] > t[3]))) continue;
    taken.push(box); n++;
    s += `<g transform="translate(${x.toFixed(1)},${y.toFixed(1)})" font-family="Jakarta"><circle r="3.5" fill="#fff6dc" stroke="#2b2118" stroke-width="1.5"/>
<g transform="translate(${(-w / 2).toFixed(1)},${-h - 8})" filter="url(#shadow)"><rect width="${w.toFixed(1)}" height="${h}" rx="10.5" fill="#13191b" stroke="rgba(255,255,255,.22)"/>
<rect x="3" y="3" width="${pw.toFixed(1)}" height="15" rx="7.5" fill="#5cc6c0"/><text x="${(3 + pw / 2).toFixed(1)}" y="14.5" font-size="10.5" text-anchor="middle" fill="#0f1416">${pop}</text>
<text x="${(pw + 9).toFixed(1)}" y="15" font-size="12.5" fill="#f3f7f9">${name}</text></g></g>`;
  }
  return s + `</svg>`;
}
await Bun.write(`${out}/globe-3000.svg`, await frame(3000, 1600, 900, 8, 52, lc4, { labels: 14, zoom: 1.44 }));
await Bun.write(`${out}/globe-8000.svg`, await frame(8000, 1600, 900, 8, 52, lc3, { labels: 12, zoom: 0.92 }));
await Bun.write(`${out}/globe-8000-night.svg`, await frame(8000, 1600, 900, 8, 52, lc3, { labels: 0, zoom: 0.92, night: [-78.5, -14] }));
await Bun.write(`${out}/globe-3000-clouds.svg`, await frame(3000, 1600, 900, 8, 52, lc4, { labels: 14, zoom: 1.44, clouds: 0.45 }));
console.log("done");

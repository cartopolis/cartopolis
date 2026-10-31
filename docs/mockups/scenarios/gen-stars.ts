// Beyond the globe: the solar system and the stellar neighbourhood as node-and-lane boards (Master of Orion's structure).
// Data: the HYG star catalogue v4.1 (astronexus/HYG-Database, CC BY-SA 4.0) as hyg.csv, in the macro-view data dir.
// Usage (there): OUT=<dir> bun gen-stars.ts
const outD = process.env.OUT ?? ".";
const r1 = (n: number) => Math.round(n * 10) / 10;
const LY = 3.26156;
const t = await Bun.file("hyg.csv").text();
const L = t.split("\n"); const h = L[0].split(",").map((s) => s.replace(/"/g, "")); const ix = (n: string) => h.indexOf(n);
type Star = { name: string; d: number; x: number; y: number; z: number; spect: string; mag: number };
const stars: Star[] = [{ name: "Sol", d: 0, x: 0, y: 0, z: 0, spect: "G2V", mag: 4.83 }];
for (const l of L.slice(1)) {
  if (!l) continue; const c = l.split(",").map((s) => s.replace(/"/g, ""));
  const d = +c[ix("dist")]; if (!(d > 0 && d <= 4.9)) continue;
  stars.push({ name: c[ix("proper")] || c[ix("gl")] || "HYG " + c[ix("id")], d, x: +c[ix("x")], y: +c[ix("y")], z: +c[ix("z")], spect: c[ix("spect")], mag: +c[ix("absmag")] });
}
// one node per system: stars within 0.15 pc of a brighter one join it
type Sys = { name: string; members: Star[]; x: number; y: number; z: number; d: number; lead: Star };
const systems: Sys[] = [];
for (const s of [...stars].sort((a, b) => a.mag - b.mag)) {
  const host = systems.find((q) => Math.hypot(q.x - s.x, q.y - s.y, q.z - s.z) < 0.15);
  if (host) host.members.push(s); else systems.push({ name: s.name, members: [s], x: s.x, y: s.y, z: s.z, d: s.d, lead: s });
}
const rename: Record<string, string> = { "Rigil Kentaurus": "Alpha Centauri", "Gl 71": "Tau Ceti", "Ran": "Epsilon Eridani", "Gl 845": "Epsilon Indi", "Gl 820A": "61 Cygni", "Gl 65A": "Luyten 726-8", "Gl 380": "Groombridge 1618", "Gl 674": "Gliese 674", "Gl 876": "Gliese 876", "GJ 1061": "GJ 1061", "Gl 725A": "Struve 2398", "Struve 2398 A": "Struve 2398" };
for (const s of systems) s.name = rename[s.name] ?? s.name;
// planets listed in the NASA Exoplanet Archive (a selection, by host)
const planets: Record<string, number> = { "Alpha Centauri": 1, "Barnard's Star": 4, "Lalande 21185": 1, "Epsilon Eridani": 1, "Ross 128": 1, "Luyten's Star": 2, "Wolf 1061": 3, "Gliese 876": 4, "GJ 1061": 3, "Lacaille 9352": 2, "Groombridge 34": 1, "Tau Ceti": 4, "Gliese 674": 1, "Sol": 8 };
// lanes: each system to its nearest neighbours, up to 2.9 pc (9.5 ly), at least one each
const key = (a: number, b: number) => (a < b ? a + "-" + b : b + "-" + a);
const lanes = new Map<string, [number, number, number]>();
systems.forEach((s, i) => {
  const near = systems.map((q, j) => ({ j, d: Math.hypot(q.x - s.x, q.y - s.y, q.z - s.z) })).filter((o) => o.j !== i).sort((a, b) => a.d - b.d);
  near.slice(0, 3).forEach((o, k) => { if (k === 0 || o.d < 2.9) lanes.set(key(i, o.j), [Math.min(i, o.j), Math.max(i, o.j), o.d]); });
});
const COL = (sp: string) => ({ O: "#9bb0ff", B: "#aabfff", A: "#e4ecff", F: "#fbf8ff", G: "#fff1b0", K: "#ffc27a", M: "#ff8a5c", D: "#dfe8ff" } as Record<string, string>)[(sp.replace(/^(sd|d)/, "")[0] ?? "M").toUpperCase()] ?? "#ff8a5c";
const defs = `<defs><radialGradient id="bg" cx="50%" cy="50%" r="75%"><stop offset="0" stop-color="#0c1420"/><stop offset="1" stop-color="#03050a"/></radialGradient>
<filter id="glow" x="-100%" y="-100%" width="300%" height="300%"><feGaussianBlur stdDeviation="6"/></filter>
<filter id="shadow" x="-20%" y="-20%" width="140%" height="160%"><feDropShadow dx="0" dy="2" stdDeviation="2" flood-color="#000" flood-opacity=".7"/></filter></defs>`;
const head = (W: number, H: number) => `<svg xmlns="http://www.w3.org/2000/svg" width="${W}" height="${H}" viewBox="0 0 ${W} ${H}"><style>@font-face{font-family:Jakarta;src:url(../../../assets/fonts/PlusJakartaSans-SemiBold.ttf)}</style>${defs}<rect width="${W}" height="${H}" fill="url(#bg)"/>`;
function dust(W: number, H: number, n: number) { let s = "", rng = 5; const rnd = () => ((rng = (rng * 16807) % 2147483647) / 2147483647); for (let i = 0; i < n; i++) s += `<circle cx="${r1(rnd() * W)}" cy="${r1(rnd() * H)}" r="${(0.3 + rnd() * 0.7).toFixed(2)}" fill="#cfe0ff" opacity="${(0.15 + rnd() * 0.4).toFixed(2)}"/>`; return s; }

// ===== the stellar neighbourhood =====
{
  const W = 1600, H = 1000, k = 98, cx = W / 2 + 40, cy = H / 2 + 20;
  // a gentle tilt so height above the plane reads: x right, y up-screen squashed, z up
  const P = (x: number, y: number, z: number): [number, number] => [cx + x * k, cy - y * k * 0.62 - z * k * 0.55];
  const Pp = (x: number, y: number): [number, number] => P(x, y, 0);
  let s = head(W, H) + dust(W, H, 500);
  // the plane: rings every 5 ly around Sol
  for (const ly of [5, 10, 15]) { const r = ly / LY; s += `<ellipse cx="${cx}" cy="${cy}" rx="${r1(r * k)}" ry="${r1(r * k * 0.62)}" fill="none" stroke="#5cc6c0" stroke-width="1" opacity=".22" stroke-dasharray="4 6"/><text x="${r1(cx + r * k + 6)}" y="${cy + 4}" font-family="Jakarta" font-size="11" fill="#5cc6c0" opacity=".6">${ly} ly</text>`; }
  // stalks
  const sorted = systems.map((q, i) => ({ q, i })).sort((a, b) => a.q.y - b.q.y);
  for (const { q } of sorted) { const [x0, y0] = Pp(q.x, q.y), [x1, y1] = P(q.x, q.y, q.z); s += `<line x1="${r1(x0)}" y1="${r1(y0)}" x2="${r1(x1)}" y2="${r1(y1)}" stroke="#7f9aa3" stroke-width="1" opacity=".35"/><ellipse cx="${r1(x0)}" cy="${r1(y0)}" rx="3" ry="1.6" fill="#7f9aa3" opacity=".45"/>`; }
  // lanes
  for (const [a, b, d] of lanes.values()) {
    const A = systems[a], B = systems[b]; const [x0, y0] = P(A.x, A.y, A.z), [x1, y1] = P(B.x, B.y, B.z);
    const hot = A.name === "Sol" || B.name === "Sol";
    s += `<line x1="${r1(x0)}" y1="${r1(y0)}" x2="${r1(x1)}" y2="${r1(y1)}" stroke="${hot ? "#5cc6c0" : "#8fb3d9"}" stroke-width="${hot ? 2.2 : 1.3}" opacity="${hot ? 0.85 : 0.45}"/>`;
    if (hot || d * LY < 3.5) s += `<text x="${r1((x0 + x1) / 2)}" y="${r1((y0 + y1) / 2 - 4)}" font-family="Jakarta" font-size="10.5" fill="#a5c4e0" text-anchor="middle" opacity=".85">${(d * LY).toFixed(1)} ly</text>`;
  }
  // nodes
  for (const { q } of sorted) {
    const [x, y] = P(q.x, q.y, q.z); const c = COL(q.lead.spect); const sz = Math.max(3, 9 - q.lead.mag * 0.38);
    s += `<circle cx="${r1(x)}" cy="${r1(y)}" r="${r1(sz * 2.2)}" fill="${c}" opacity=".45" filter="url(#glow)"/>`;
    q.members.forEach((m, n) => { const a = n * 2.1; const o = n ? 5 : 0; s += `<circle cx="${r1(x + Math.cos(a) * o)}" cy="${r1(y + Math.sin(a) * o)}" r="${r1(n ? sz * 0.55 : sz)}" fill="${COL(m.spect)}"/>`; });
    const pl = planets[q.name];
    if (q.name === "Sol") s += `<circle cx="${r1(x)}" cy="${r1(y)}" r="16" fill="none" stroke="#fd6012" stroke-width="2.5"/>`;
    const named = !/^(Gl |GJ |HYG )/.test(q.name) || pl;
    if (named) s += `<g font-family="Jakarta" filter="url(#shadow)"><text x="${r1(x + sz + 7)}" y="${r1(y + 4)}" font-size="${q.name === "Sol" ? 15 : 12.5}" fill="${q.name === "Sol" ? "#ffd2b8" : "#e8eef0"}">${q.name}</text>${pl ? `<text x="${r1(x + sz + 7)}" y="${r1(y + 18)}" font-size="10.5" fill="#9fd3b4">${"●".repeat(Math.min(pl, 8))}</text>` : ""}</g>`;
  }
  await Bun.write(`${outD}/stars.svg`, s + "</svg>");
  console.log("systems", systems.length, "lanes", lanes.size, "with planets", systems.filter((q) => planets[q.name]).map((q) => q.name).join(", "));
  const ac = systems.find((q) => q.name === "Alpha Centauri")!; const solL = [...lanes.values()].filter(([a, b]) => systems[a].name === "Sol" || systems[b].name === "Sol").map(([a, b, d]) => (systems[a].name === "Sol" ? systems[b] : systems[a]).name + " " + (d * LY).toFixed(1));
  console.log("Sol lanes", solL.join(" | "), "AC members", ac.members.map((m) => m.name).join(","));
}

// ===== the solar system as a node board =====
{
  const W = 960, H = 600, cx = 150, cy = 300;
  const pl = [["Mercury", 0.39, "#b8a99a", 4], ["Venus", 0.72, "#e8cf9c", 6], ["Earth", 1, "#5c9bd6", 7], ["Mars", 1.52, "#d0714a", 5], ["Jupiter", 5.2, "#d9b38c", 13], ["Saturn", 9.54, "#e6d3a3", 11], ["Uranus", 19.2, "#9fd8e0", 9], ["Neptune", 30.1, "#5f84d9", 9]] as const;
  const R = (au: number) => 70 + Math.log(1 + au * 2.2) * 150;
  let s = head(W, H) + dust(W, H, 200);
  s += `<circle cx="${cx}" cy="${cy}" r="40" fill="#fff1b0" opacity=".5" filter="url(#glow)"/><circle cx="${cx}" cy="${cy}" r="22" fill="#fff1b0"/>`;
  pl.forEach(([n, au], i) => { const r = R(au); s += `<path d="M ${r1(cx + r * Math.cos(-0.62))} ${r1(cy + r * Math.sin(-0.62))} A ${r1(r)} ${r1(r)} 0 0 1 ${r1(cx + r * Math.cos(0.62))} ${r1(cy + r * Math.sin(0.62))}" fill="none" stroke="#5cc6c0" stroke-width="1" opacity=".18"/>`; });
  const pos = pl.map(([n, au, c, sz], i) => { const a = -0.45 + (i % 3) * 0.42 + (i > 3 ? 0.1 : 0); const r = R(au); return { n, c, sz, x: cx + r * Math.cos(a), y: cy + r * Math.sin(a) }; });
  // lanes: transfer routes between neighbours, plus the Moon off Earth
  for (let i = 0; i < pos.length - 1; i++) s += `<line x1="${r1(pos[i].x)}" y1="${r1(pos[i].y)}" x2="${r1(pos[i + 1].x)}" y2="${r1(pos[i + 1].y)}" stroke="#8fb3d9" stroke-width="1.3" opacity=".45"/>`;
  const e = pos[2]; const moon = { x: e.x + 34, y: e.y - 30 };
  s += `<line x1="${r1(e.x)}" y1="${r1(e.y)}" x2="${moon.x}" y2="${moon.y}" stroke="#5cc6c0" stroke-width="2" opacity=".8"/><circle cx="${r1(moon.x)}" cy="${r1(moon.y)}" r="4" fill="#cfd5d8"/><text x="${r1(moon.x + 8)}" y="${r1(moon.y + 4)}" font-family="Jakarta" font-size="11.5" fill="#cfd5d8">Moon</text>`;
  for (const p of pos) {
    s += `<circle cx="${r1(p.x)}" cy="${r1(p.y)}" r="${p.sz * 2}" fill="${p.c}" opacity=".35" filter="url(#glow)"/><circle cx="${r1(p.x)}" cy="${r1(p.y)}" r="${p.sz}" fill="${p.c}"/>`;
    if (p.n === "Earth") s += `<circle cx="${r1(p.x)}" cy="${r1(p.y)}" r="${p.sz + 8}" fill="none" stroke="#fd6012" stroke-width="2.5"/>`;
    s += `<text x="${r1(p.x)}" y="${r1(p.y + p.sz + 18)}" font-family="Jakarta" font-size="12.5" fill="${p.n === "Earth" ? "#ffd2b8" : "#e8eef0"}" text-anchor="middle" filter="url(#shadow)">${p.n}</text>`;
  }
  s += `<text x="${cx}" y="${cy + 46}" font-family="Jakarta" font-size="12.5" fill="#fff1b0" text-anchor="middle">Sun</text>`;
  await Bun.write(`${outD}/system.svg`, s + "</svg>");
}
console.log("done");

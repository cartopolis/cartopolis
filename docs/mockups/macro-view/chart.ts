// Layer opacity against altitude (log axis) for the transition sheet.
const W = 1600, H = 330, L = 230, R = 30, T = 30, rowH = 40;
const lo = Math.log10(0.5), hi = Math.log10(20000);
const X = (km: number) => L + ((Math.log10(km) - lo) / (hi - lo)) * (W - L - R);
type Seg = [number, number][]; // [km, opacity]
const rows: { name: string; note: string; col: string; pts: Seg; today?: boolean }[] = [
  { name: "3D city", note: "buildings sink into the ground", col: "#c7d3d8", pts: [[0.5, 1], [15, 1], [50, 0], [20000, 0]] },
  { name: "Painted ground", note: "land cover, roads by rank", col: "#86a957", pts: [[0.5, 0], [15, 0], [50, 1], [20000, 1]] },
  { name: "City banners", note: "more of them lower down", col: "#5cc6c0", pts: [[0.5, 0], [2, 0], [5, 1], [20000, 1]] },
  { name: "Hex grid", note: "the unit lenses count in", col: "#fff2c4", pts: [[0.5, 0], [100, 0], [250, 1], [1500, 1], [4000, 0], [20000, 0]] },
  { name: "Globe and space", note: "space_factor, exists today", col: "#6fb6e6", pts: [[0.5, 0], [50, 0], [3000, 1], [20000, 1]], today: true },
];
let s = `<svg xmlns="http://www.w3.org/2000/svg" width="${W}" height="${H}" viewBox="0 0 ${W} ${H}" font-family="Jakarta">`;
rows.forEach((r, i) => {
  const y0 = T + i * rowH, y1 = y0 + rowH - 12;
  s += `<text x="0" y="${y0 + 13}" font-size="14" fill="#f3f7f9">${r.name}</text><text x="0" y="${y0 + 29}" font-size="11.5" fill="#a5b2b7">${r.note}</text>`;
  s += `<rect x="${L}" y="${y0}" width="${W - L - R}" height="${rowH - 12}" fill="#1f2829" rx="4"/>`;
  const d = r.pts.map(([k, o], j) => `${j ? "L" : "M"}${X(k).toFixed(1)} ${(y1 - o * (rowH - 12)).toFixed(1)}`).join("");
  s += `<path d="${d}L${X(20000).toFixed(1)} ${y1}L${L} ${y1}Z" fill="${r.col}" opacity="${r.today ? 0.35 : 0.75}"/>`;
});
const yb = T + rows.length * rowH + 4;
for (const [km, lab] of [[0.5, "500 m"], [3, "3 km"], [15, "15 km"], [40, "40 km"], [50, ""], [350, "350 km"], [3000, "3,000 km"], [12000, "12,000 km"]] as [number, string][]) {
  s += `<line x1="${X(km).toFixed(1)}" x2="${X(km).toFixed(1)}" y1="${T - 6}" y2="${yb}" stroke="#63797c" stroke-width="1" stroke-dasharray="${lab ? "3 4" : "1 3"}"/>`;
  if (lab) s += `<text x="${X(km).toFixed(1)}" y="${yb + 18}" font-size="12.5" text-anchor="middle" fill="#a5b2b7">${lab}</text>`;
}
s += `</svg>`;
await Bun.write(process.argv[2], s);

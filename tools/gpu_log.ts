/**
 * The client's five-second `gpu` log line, read back by the browser probes
 * (`cdp_probe.ts`, `firefox_probe.ts`) and printed as one table.
 *
 * The numbers are the client's own: `wgpu::meter` counts every buffer and texture
 * the process creates and every byte it writes, at wgpu's API, so they mean the
 * same in Chrome, in Firefox and natively (`systems::upload_meter::GpuReport`).
 * The probes only collect them, with the time each arrived and the moments the
 * camera script moved, so a burst can be lined up with what caused it.
 */

export type GpuRow = { t: number; f: Record<string, number> };
export type Mark = { t: number; label: string };

/** The `gpu` line's heap-by-label text (a `heap-labels` build), or `null`. */
export function parseHeapBy(line: string): string | null {
  const m = /\bheap_by = ([^;]+);/.exec(line);
  const p = /\bheap_peak_by = ([^;]+);/.exec(line);
  if (!m || !m[1].trim()) return null;
  return p && p[1].trim() ? `${m[1].trim()}  | peaks since last: ${p[1].trim()}` : m[1].trim();
}

/** The `mesh owners` line's text, or `null`. */
export function parseOwnersLine(line: string): string | null {
  const m = /\bmesh owners owners = (.*?);\s*surface_cells = (\d+);\s*surface_ring_mb = (\d+);\s*altitude_m = (-?\d+)/.exec(line);
  return m ? `at ${m[4]} m: ${m[1]} (surface cells ${m[2]}, ${m[3]} MB)` : null;
}

/** A `map load` line's summary (`load_timing`): what set a load off, how long it
 * took and which layers finished last. Marked in the probes' timelines, it is
 * how two runs' fill times compare. */
export function parseMapLoad(line: string): string | null {
  const m = /\bmap load load = (.*)$/.exec(line);
  return m ? m[1].replace(/[;"]+\s*$/, "") : null;
}

/**
 * A flight for the probes' `flight` script: altitudes over one place, each
 * posted to the page as the `cartopolis.place` message the World page uses
 * (`place_link::follow_framed_places`), so the camera goes exactly there in any
 * browser, on `/viewer/` and inside `/world`. Down to street level, out past the city, and back.
 */
export const FLIGHT_PLACE = "53.2194,6.5665";
export const FLIGHT_ALTITUDES = [1200, 300, 50, 12, 50, 300, 1200, 5000, 40000, 5000, 1200, 12];
export const FLIGHT_DWELL_MS = 8000;
/** The expression that sends the camera to `alt` metres over `FLIGHT_PLACE`. */
export function flyExpression(alt: number): string {
  // On the World page the viewer is the `.home-world-frame` iframe; elsewhere it is the page.
  // The mark names the step in a browser profile (`firefox_probe --gecko-profile`).
  return `(performance.mark("flew to ${alt} m"), (document.querySelector("iframe.home-world-frame")?.contentWindow ?? window).postMessage({type: "cartopolis.place", place: "${FLIGHT_PLACE},${alt}m"}, location.origin))`;
}

/** The fields of a `gpu`, `mesh memory` or `frame split` line, or `null` for any other. */
export function parseGpuLine(line: string): Record<string, number> | null {
  const gpu = /\bgpu resident_mb\s*=/.test(line);
  const mesh = /\bmesh memory lod22_mb\s*=/.test(line);
  const split = /\bframe split frames\s*=/.test(line);
  if (!gpu && !mesh && !split) return null;
  const f: Record<string, number> = {};
  for (const m of line.matchAll(/(\w+)\s*=\s*(-?[\d.]+)/g)) f[m[1]] = Number(m[2]);
  return f;
}

const MESH_COLS: [string, string][] = [
  ["lod22_mb", "3DBAG MB"],
  ["buildings_mb", "OSM bldg MB"],
  ["tiles_mb", "tiles MB"],
  ["globe_mb", "globe MB"],
  ["other_mb", "other MB"],
  ["unowned_mb", "no entity MB"],
];

const SPLIT_COLS: [string, string][] = [
  ["frames", "frames"],
  ["mean_ms", "mean ms"],
  ["main_ms", "main ms"],
  ["render_ms", "render ms"],
  ["other_ms", "other ms"],
  ["worst_ms", "worst ms"],
  ["worst_main_ms", "…main"],
  ["worst_render_ms", "…render"],
  ["worst_other_ms", "…other"],
];

const COLS: [string, string][] = [
  ["resident_mb", "resident MB"],
  ["textures_mb", "textures MB"],
  ["buffers_mb", "buffers MB"],
  ["textures", "#tex"],
  ["buffers", "#buf"],
  ["written_mb_s", "written MB/s"],
  ["max_frame_mb", "max frame MB"],
  ["streamed_assets_mb", "streamed MB"],
  ["heap_mb", "heap MB"],
  ["wasm_memory_mb", "wasm mem MB"],
  ["cpu_meshes_mb", "cpu mesh MB"],
  ["cpu_images_mb", "cpu img MB"],
  ["storage_view_mb", "store MB"],
];

/** The table, with the camera script's marks interleaved, and the run's extremes. */
export function printGpuTable(all: GpuRow[], marks: Mark[]): void {
  const rows = all.filter((r) => "resident_mb" in r.f);
  const meshRows = all.filter((r) => "lod22_mb" in r.f);
  const splitRows = all.filter((r) => "worst_other_ms" in r.f);
  table("where each frame's time went (the client's frame split)", splitRows, SPLIT_COLS, marks);
  console.log("\n══ GPU, as the client counts it (wgpu::meter) ═════════════");
  if (!rows.length) {
    console.log("  no `gpu` lines: a build older than the meter, or a log filter dropping INFO");
    return;
  }
  console.log("      t  " + COLS.map(([, h]) => h.padStart(13)).join(""));
  const events = [
    ...rows.map((r) => ({ t: r.t, row: r as GpuRow | undefined, mark: undefined as string | undefined })),
    ...marks.map((m) => ({ t: m.t, row: undefined, mark: m.label })),
  ].sort((a, b) => a.t - b.t);
  for (const e of events) {
    if (e.mark) console.log(`  ${e.t.toFixed(0).padStart(5)}s  ── ${e.mark}`);
    else console.log(`  ${e.t.toFixed(0).padStart(5)}s` + COLS.map(([k]) => String(e.row!.f[k] ?? "").padStart(13)).join(""));
  }
  if (meshRows.length) {
    console.log("\n══ mesh memory by streamer (the client's census) ══════════");
    console.log("      t  " + MESH_COLS.map(([, h]) => h.padStart(13)).join(""));
    const ev = [
      ...meshRows.map((r) => ({ t: r.t, row: r as GpuRow | undefined, mark: undefined as string | undefined })),
      ...marks.map((m) => ({ t: m.t, row: undefined, mark: m.label })),
    ].sort((a, b) => a.t - b.t);
    for (const e of ev) {
      if (e.mark) console.log(`  ${e.t.toFixed(0).padStart(5)}s  ── ${e.mark}`);
      else console.log(`  ${e.t.toFixed(0).padStart(5)}s` + MESH_COLS.map(([k]) => String(e.row!.f[k] ?? "").padStart(13)).join(""));
    }
  }
  const max = (k: string) => Math.max(...rows.map((r) => r.f[k] ?? 0));
  console.log(
    `  ── peak resident ${max("peak_mb") || max("resident_mb")} MB, ` +
      `most written in one frame ${max("max_frame_mb")} MB, ` +
      `fastest ${max("written_mb_s")} MB/s`,
  );
}

/** One table of rows with the marks interleaved. */
function table(title: string, rows: GpuRow[], cols: [string, string][], marks: Mark[]): void {
  if (!rows.length) return;
  console.log(`\n══ ${title} ${"═".repeat(Math.max(4, 58 - title.length))}`);
  console.log("      t  " + cols.map(([, h]) => h.padStart(10)).join(""));
  const ev = [
    ...rows.map((r) => ({ t: r.t, row: r as GpuRow | undefined, mark: undefined as string | undefined })),
    ...marks.map((m) => ({ t: m.t, row: undefined, mark: m.label })),
  ].sort((a, b) => a.t - b.t);
  for (const e of ev) {
    if (e.mark) console.log(`  ${e.t.toFixed(0).padStart(5)}s  ── ${e.mark}`);
    else console.log(`  ${e.t.toFixed(0).padStart(5)}s` + cols.map(([k]) => String(e.row!.f[k] ?? "").padStart(10)).join(""));
  }
}

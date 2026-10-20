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

/** The `mesh owners` line's text, or `null`. */
export function parseOwnersLine(line: string): string | null {
  const m = /\bmesh owners owners = (.*?);\s*surface_cells = (\d+);\s*altitude_m = (-?\d+)/.exec(line);
  return m ? `at ${m[3]} m: ${m[1]} (surface cells ${m[2]})` : null;
}

/** The fields of a `gpu` or `mesh memory` line, or `null` for any other line. */
export function parseGpuLine(line: string): Record<string, number> | null {
  const gpu = /\bgpu resident_mb\s*=/.test(line);
  const mesh = /\bmesh memory lod22_mb\s*=/.test(line);
  if (!gpu && !mesh) return null;
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

const COLS: [string, string][] = [
  ["resident_mb", "resident MB"],
  ["textures_mb", "textures MB"],
  ["buffers_mb", "buffers MB"],
  ["textures", "#tex"],
  ["buffers", "#buf"],
  ["written_mb_s", "written MB/s"],
  ["max_frame_mb", "max frame MB"],
  ["streamed_assets_mb", "streamed MB"],
];

/** The table, with the camera script's marks interleaved, and the run's extremes. */
export function printGpuTable(all: GpuRow[], marks: Mark[]): void {
  const rows = all.filter((r) => "resident_mb" in r.f);
  const meshRows = all.filter((r) => "lod22_mb" in r.f);
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

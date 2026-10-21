#!/usr/bin/env bun
/**
 * Read a Firefox profile (`firefox_probe --gecko-profile`) and say where each
 * thread's time went: busy time, the functions that were running, and the long
 * stretches without a pause that a person sees as a freeze.
 *
 *   bun tools/gecko_profile.ts /tmp/ff.json                  # every busy thread
 *   bun tools/gecko_profile.ts /tmp/ff.json --from 25 --to 60 # a window, in seconds
 *   bun tools/gecko_profile.ts /tmp/ff.json --thread GeckoMain --top 40
 *   bun tools/gecko_profile.ts /tmp/ff.json --stretch-ms 100 # freezes of 100 ms or more
 *   bun tools/gecko_profile.ts /tmp/ff.json --marks .         # every user-timing mark, not only the flight's
 *   bun tools/gecko_profile.ts /tmp/ff.json --wasm dist/cartopolis-*_bg.wasm   # full wasm names
 *   bun tools/gecko_profile.ts /tmp/ff.json --thread GeckoMain --timeline 5      # running vs on-CPU, per 5 s
 *
 * **Running is not on a CPU.** Each thread's header gives the time it was
 * running (not waiting for work) and how much of that it actually had a core
 * for, from Firefox's per-sample CPU deltas. The rest is time the OS gave its
 * core to another thread. `--timeline` breaks that down per bucket beside the
 * CPU the same process's other threads used, which is how a thread starved by
 * its own process's workers shows itself.
 *
 * **Pass `--wasm`** with the module the profile ran. Firefox cuts every frame
 * name at 511 characters, and a Bevy system's name is the last thing in its
 * `FunctionSystem<fn(…params…), …, path::to::system>`, so without it most
 * systems read alike. Each wasm frame also records its byte offset in the module
 * (`…_bg.wasm:64310102`); `--wasm` finds the function body that holds that
 * offset and takes its name from the module's `name` section, uncut, and a
 * system frame is then shown as `system path::to::system`.
 *
 * Times are seconds from the profile's start (the browser's launch), the same
 * clock as `flew to … m`, the probe's marks, which it lists first.
 *
 * It reads the raw format Firefox writes at shutdown: per thread, a sample table
 * (stack, time), a stack table (prefix, frame) and a frame table, each with its
 * own `schema`, and child processes under `processes`, each with its own start
 * time. Names come from three places: profiler labels (`nsRefreshDriver::Tick`,
 * `GC`, …), JavaScript functions, and the wasm module's name section (only in a
 * build kept unstripped). Native machine code is left as addresses, which this
 * skips when it names a frame: the nearest named frame stands for it.
 */
import { parseArgs } from "node:util";
import { gunzipSync } from "node:zlib";

const { values: opt, positionals } = parseArgs({
  allowPositionals: true,
  options: {
    from: { type: "string" },
    to: { type: "string" },
    thread: { type: "string" },
    top: { type: "string", default: "25" },
    "stretch-ms": { type: "string", default: "50" },
    "min-busy-ms": { type: "string", default: "200" },
    stretches: { type: "string", default: "15" },
    timeline: { type: "string", default: "0" },
    marks: { type: "string", default: "^flew to" },
    wasm: { type: "string" },
  },
});
const path = positionals[0];
if (!path) {
  console.error("usage: gecko_profile.ts PROFILE.json [--from s] [--to s] [--thread name] [--top n] [--stretch-ms ms]");
  process.exit(2);
}

const bytes = new Uint8Array(await Bun.file(path).arrayBuffer());
const text = new TextDecoder().decode(bytes[0] === 0x1f && bytes[1] === 0x8b ? gunzipSync(bytes) : bytes);
const root = JSON.parse(text);

type Table = { schema: Record<string, number>; data: any[][] };
type Thread = {
  name: string;
  process: string;
  pid: string;
  offset: number; // ms from the root profile's start
  samples: Table;
  markers: Table;
  stackTable: Table;
  frameTable: Table;
  stringTable: string[];
};

const threads: Thread[] = [];
const rootStart: number = root.meta.startTime;
const categories: string[] = (root.meta.categories ?? []).map((c: any) => c.name);
const interval: number = root.meta.interval ?? 1;

function collect(p: any) {
  const offset = (p.meta?.startTime ?? rootStart) - rootStart;
  for (const t of p.threads ?? []) {
    if (!t.samples?.schema || !t.stackTable?.schema) continue;
    threads.push({
      name: t.name,
      process: t.processType ?? "?",
      pid: String(t.pid ?? p.meta?.pid ?? "?"),
      offset,
      samples: t.samples,
      markers: t.markers ?? { schema: {}, data: [] },
      stackTable: t.stackTable,
      frameTable: t.frameTable,
      stringTable: t.stringTable ?? [],
    });
  }
  for (const c of p.processes ?? []) collect(c);
}
collect(root);

const from = opt.from !== undefined ? Number(opt.from) * 1000 : -Infinity;
const to = opt.to !== undefined ? Number(opt.to) * 1000 : Infinity;
const top = Number(opt.top);
const stretchMs = Number(opt["stretch-ms"]);
const sec = (ms: number) => (ms / 1000).toFixed(1).padStart(6);

/** Function bodies of the wasm module by byte range, with their names. */
type Body = { start: number; end: number; name: string };
const bodies: Body[] = opt.wasm ? readWasmNames(new Uint8Array(await Bun.file(opt.wasm).arrayBuffer())) : [];

function readWasmNames(m: Uint8Array): Body[] {
  let p = 8;
  const leb = () => {
    let r = 0, shift = 0, b;
    do {
      b = m[p++];
      r += (b & 0x7f) * 2 ** shift;
      shift += 7;
    } while (b & 0x80);
    return r;
  };
  const str = () => {
    const n = leb();
    const v = new TextDecoder().decode(m.subarray(p, p + n));
    p += n;
    return v;
  };
  let imported = 0;
  const ranges: { start: number; end: number }[] = [];
  const names = new Map<number, string>();
  while (p < m.length) {
    const id = m[p++];
    const size = leb();
    const end = p + size;
    if (id === 2) {
      // Imports: only function imports take a function index.
      for (let n = leb(); n > 0; n--) {
        str();
        str();
        const kind = m[p++];
        if (kind === 0) {
          leb();
          imported++;
        } else if (kind === 1) {
          p++;
          const flags = leb();
          leb();
          if (flags & 1) leb();
        } else if (kind === 2) {
          const flags = leb();
          leb();
          if (flags & 1) leb();
        } else if (kind === 3) {
          p += 2;
        } else if (kind === 4) {
          p++;
          leb();
        }
      }
    } else if (id === 10) {
      for (let n = leb(); n > 0; n--) {
        const len = leb();
        ranges.push({ start: p, end: p + len });
        p += len;
      }
    } else if (id === 0 && str() === "name") {
      while (p < end) {
        const sub = m[p++];
        const subLen = leb();
        const subEnd = p + subLen;
        if (sub === 1) for (let n = leb(); n > 0; n--) names.set(leb(), str());
        p = subEnd;
      }
    }
    p = end;
  }
  return ranges.map((r, i) => ({ ...r, name: names.get(imported + i) ?? `wasm-function[${imported + i}]` }));
}

/** Every body's name, sorted, for the frames Firefox cut short. */
const sortedNames = bodies.map((b) => b.name).sort();

/** The full names that begin with `prefix`. */
function namesStarting(prefix: string): string[] {
  let lo = 0, hi = sortedNames.length;
  while (lo < hi) {
    const mid = (lo + hi) >> 1;
    if (sortedNames[mid] < prefix) lo = mid + 1;
    else hi = mid;
  }
  const out: string[] = [];
  for (let i = lo; i < sortedNames.length && sortedNames[i].startsWith(prefix) && out.length < 64; i++) out.push(sortedNames[i]);
  return out;
}

function bodyAt(offset: number): Body | undefined {
  let lo = 0, hi = bodies.length - 1;
  while (lo <= hi) {
    const mid = (lo + hi) >> 1;
    if (bodies[mid].end <= offset) lo = mid + 1;
    else if (bodies[mid].start > offset) hi = mid - 1;
    else return bodies[mid];
  }
  return undefined;
}

/** The last type argument of `FunctionSystem<…>`, which is the system's path. */
function systemOf(name: string): string | null {
  const at = name.indexOf("FunctionSystem<");
  if (at < 0) return null;
  let depth = 0, lastArg = -1;
  for (let i = at + "FunctionSystem<".length; i < name.length; i++) {
    const c = name[i];
    if (c === "<" || c === "(" || c === "[") depth++;
    else if (c === ")" || c === "]") depth--;
    else if (c === ">" && name[i - 1] !== "-") {
      if (depth === 0) return lastArg < 0 ? null : name.slice(lastArg, i).trim();
      depth--;
    } else if (c === "," && depth === 0) lastArg = i + 1;
  }
  return null;
}

const nameCache = new Map<string, string | null>();

/** A frame's name, or null for a bare machine-code address. */
function frameName(t: Thread, frame: number): string | null {
  const row = t.frameTable.data[frame];
  const loc = t.stringTable[row[t.frameTable.schema.location]] ?? "";
  let v = nameCache.get(loc);
  if (v === undefined) {
    v = nameOf(loc);
    nameCache.set(loc, v);
  }
  return v;
}

function nameOf(loc: string): string | null {
  if (/^0x[0-9a-f]+$/i.test(loc)) return null;
  const off = /\.wasm:(\d+)\)?$/.exec(loc);
  let name = loc;
  if (off && bodies.length) {
    const body = bodyAt(Number(off[1]));
    if (body) name = body.name;
  } else if (bodies.length && loc.endsWith("...") && loc.length >= 500) {
    // Cut at 511 characters, offset and all: find it by what is left. Several
    // methods of one system share the prefix; any of them names the system.
    const matches = namesStarting(loc.replace(/^cartopolis\.wasm\./, "").slice(0, -3));
    const systems = new Set(matches.map((m) => systemOf(m.replace(/\[[0-9a-f]{16}\]/g, ""))));
    if (matches.length && systems.size === 1 && !systems.has(null)) name = matches[0];
    else if (matches.length === 1) name = matches[0];
  }
  name = name.replace(/\[[0-9a-f]{16}\]/g, "");
  const system = systemOf(name);
  if (system) return `system ${system.replace(/^<(.*)>::run_\w+$/, "$1")}`;
  return tidy(name);
}

/** Shorten a wasm or JS location: drop the URL, keep the function. */
function tidy(loc: string): string {
  return loc
    .replace(/^cartopolis\.wasm\./, "")
    .replace(/\s*\((?:https?|blob|resource|chrome|moz-extension):[^)]*\)\s*$/, "")
    .replace(/::h[0-9a-f]{16}$/, "")
    .slice(0, 140);
}

function frameCategory(t: Thread, frame: number): string {
  const row = t.frameTable.data[frame];
  const c = row[t.frameTable.schema.category];
  return c === null || c === undefined ? "" : (categories[c] ?? String(c));
}

/** Frames of a stack, leaf first. */
function framesOf(t: Thread, stack: number | null): number[] {
  const out: number[] = [];
  const { prefix, frame } = t.stackTable.schema;
  for (let s = stack; s !== null && s !== undefined; s = t.stackTable.data[s][prefix]) {
    out.push(t.stackTable.data[s][frame]);
  }
  return out;
}

/** `cpu` is the share of the sample's interval the thread spent on a CPU, from
 * `threadCPUDelta`; `null` where the profile has no CPU figures. */
type Sample = { time: number; weight: number; stack: number | null; cpu: number | null };

function samplesOf(t: Thread): Sample[] {
  const { stack, time, threadCPUDelta } = t.samples.schema;
  const unit = root.meta.sampleUnits?.threadCPUDelta;
  // To milliseconds; "variable CPU cycles" (Windows) has no fixed scale.
  const toMs = unit === "µs" ? 1e-3 : unit === "ns" ? 1e-6 : null;
  const rows = t.samples.data;
  const out: Sample[] = [];
  for (let i = 0; i < rows.length; i++) {
    const at = rows[i][time] + t.offset;
    const next = i + 1 < rows.length ? rows[i + 1][time] + t.offset : at + interval;
    if (at < from || at > to) continue;
    // A gap in sampling is not time spent: cap each sample's weight.
    const weight = Math.min(next - at, interval * 4);
    // The delta is the CPU time since the previous sample, so it belongs to the
    // gap before this one; close enough at a 1 ms interval.
    const delta = threadCPUDelta !== undefined ? rows[i][threadCPUDelta] : null;
    const gap = i > 0 ? at - (rows[i - 1][time] + t.offset) : interval;
    const cpu = toMs !== null && delta !== null && delta !== undefined && gap > 0 ? Math.min(1, (delta * toMs) / gap) : null;
    out.push({ time: at, weight, stack: rows[i][stack], cpu });
  }
  return out;
}

/** Whether the thread was waiting for work at this sample, by its innermost
 * named frame (`Native event loop idle`, `…::Wait`), since Firefox leaves most
 * native frames without a category.
 *
 * Not by the CPU figures: a thread in the middle of a frame that the OS has
 * taken the core from is not waiting for work, it is **starved**, and on a busy
 * machine that is most of a freeze (Firefox on mac-1, 2026-10-21: the page's
 * thread was off the CPU for 44% of the time it spent inside frames, in step
 * with the Web Workers' load). The CPU figures say how much of the running time
 * was on a CPU; see `onCpu` below. */
function isIdle(t: Thread, s: Sample, cache: Map<number, boolean>): boolean {
  if (s.stack === null || s.stack === undefined) return true;
  let v = cache.get(s.stack);
  if (v === undefined) {
    const frames = framesOf(t, s.stack);
    const named = frames.map((f) => frameName(t, f)).find((n) => n !== null) ?? "";
    // A content process's event loop idles with nothing named below its root.
    v =
      frameCategory(t, frames[0]) === "Idle" ||
      /idle|::Wait$|^Wait/i.test(named) ||
      /^(XRE_InitChildProcess|XREMain::XRE_main|\(root\))$/.test(named);
    cache.set(s.stack, v);
  }
  return v;
}

function add(m: Map<string, number>, k: string, v: number) {
  m.set(k, (m.get(k) ?? 0) + v);
}

function topOf(m: Map<string, number>, n: number, total: number): string[] {
  return [...m.entries()]
    .sort((a, b) => b[1] - a[1])
    .slice(0, n)
    .map(([k, v]) => `${v.toFixed(0).padStart(8)} ms ${((100 * v) / Math.max(total, 1)).toFixed(1).padStart(5)}%  ${k}`);
}

// The probe's marks, and other user-timing marks, from every thread.
const marks: { t: number; name: string }[] = [];
const markFilter = new RegExp(opt.marks!);
for (const t of threads) {
  const sch = t.markers.schema;
  for (const m of t.markers.data) {
    const data = m[sch.data];
    if (data?.type !== "UserTiming") continue;
    const name: string = data.name ?? t.stringTable[m[sch.name]] ?? "";
    // The client's tracing layer marks every log line too; keep the ones asked for.
    if (!markFilter.test(name)) continue;
    marks.push({ t: (m[sch.startTime] ?? 0) + t.offset, name });
  }
}
marks.sort((a, b) => a.t - b.t);
if (marks.length) {
  console.log("══ marks ═════════════════════════════════════════════════");
  for (const m of marks) console.log(`  ${sec(m.t)}s  ${m.name}`);
}

const minBusy = Number(opt["min-busy-ms"]);
for (const t of threads) {
  if (opt.thread && !t.name.includes(opt.thread)) continue;
  const samples = samplesOf(t);
  if (!samples.length) continue;
  const idleCache = new Map<number, boolean>();
  let busy = 0;
  let span = 0;
  let onCpu = 0;
  let cpuKnown = 0;
  const bucketMs = Number(opt.timeline) * 1000;
  const timeline = new Map<number, { running: number; onCpu: number; known: number }>();
  const self = new Map<string, number>();
  const total = new Map<string, number>();
  const cats = new Map<string, number>();
  const bySystem = new Map<string, number>();
  const stretches: { start: number; end: number; samples: Sample[] }[] = [];
  let run: Sample[] = [];
  const closeRun = () => {
    if (run.length) {
      const start = run[0].time;
      const last = run[run.length - 1];
      const end = last.time + last.weight;
      if (end - start >= stretchMs) stretches.push({ start, end, samples: run });
    }
    run = [];
  };
  for (const s of samples) {
    span += s.weight;
    if (isIdle(t, s, idleCache)) {
      closeRun();
      continue;
    }
    busy += s.weight;
    if (s.cpu !== null) {
      onCpu += s.cpu * s.weight;
      cpuKnown += s.weight;
    }
    if (bucketMs > 0) {
      const k = Math.floor(s.time / bucketMs);
      const b = timeline.get(k) ?? { running: 0, onCpu: 0, known: 0 };
      b.running += s.weight;
      if (s.cpu !== null) {
        b.onCpu += s.cpu * s.weight;
        b.known += s.weight;
      }
      timeline.set(k, b);
    }
    run.push(s);
    const frames = framesOf(t, s.stack);
    add(cats, frameCategory(t, frames[0]) || "?", s.weight);
    const named = frames.map((f) => frameName(t, f)).filter((n): n is string => n !== null);
    add(self, named[0] ?? "(native code only)", s.weight);
    for (const n of new Set(named)) add(total, n, s.weight);
    add(bySystem, named.find((n) => n.startsWith("system ")) ?? "(in no system)", s.weight);
  }
  closeRun();
  if (busy < minBusy && !opt.thread) continue;

  const cpuNote = cpuKnown > 0 ? `; on a CPU for ${((100 * onCpu) / cpuKnown).toFixed(0)}% of that, off it ${(cpuKnown - onCpu).toFixed(0)} ms` : "";
  console.log(`\n══ ${t.name} (${t.process} ${t.pid}) ══ running ${busy.toFixed(0)} ms of ${span.toFixed(0)} ms sampled (${((100 * busy) / span).toFixed(0)}%)${cpuNote}`);
  if (bucketMs > 0) printTimeline(t, timeline, bucketMs);
  console.log("  by category:");
  for (const l of topOf(cats, 8, busy)) console.log(`  ${l}`);
  if (bodies.length) {
    console.log(`  by system (the innermost Bevy system on the stack):`);
    for (const l of topOf(bySystem, top, busy)) console.log(`  ${l}`);
  }
  console.log(`  self (the innermost named frame):`);
  for (const l of topOf(self, top, busy)) console.log(`  ${l}`);
  console.log(`  total (every named frame on the stack):`);
  for (const l of topOf(total, top, busy)) console.log(`  ${l}`);

  stretches.sort((a, b) => b.end - b.start - (a.end - a.start));
  const longest = stretches.slice(0, Number(opt.stretches));
  const sum = stretches.reduce((a, s) => a + s.end - s.start, 0);
  // What the freezes as a whole were made of: the same two views, over only the
  // samples inside a stretch.
  const selfStretch = new Map<string, number>();
  const totalStretch = new Map<string, number>();
  const systemStretch = new Map<string, number>();
  for (const st of stretches) {
    for (const s of st.samples) {
      const named = framesOf(t, s.stack)
        .map((f) => frameName(t, f))
        .filter((n): n is string => n !== null);
      add(selfStretch, named[0] ?? "(native code only)", s.weight);
      for (const n of new Set(named)) add(totalStretch, n, s.weight);
      add(systemStretch, named.find((n) => n.startsWith("system ")) ?? "(in no system)", s.weight);
    }
  }
  if (bodies.length) {
    console.log(`  inside stretches of ≥${stretchMs} ms, by system:`);
    for (const l of topOf(systemStretch, top, sum)) console.log(`  ${l}`);
  }
  console.log(`  inside stretches of ≥${stretchMs} ms, self:`);
  for (const l of topOf(selfStretch, top, sum)) console.log(`  ${l}`);
  console.log(`  inside stretches of ≥${stretchMs} ms, total:`);
  for (const l of topOf(totalStretch, top, sum)) console.log(`  ${l}`);
  console.log(`  ${stretches.length} stretches of ≥${stretchMs} ms without a pause, ${sum.toFixed(0)} ms in all; the longest:`);
  for (const st of longest.sort((a, b) => a.start - b.start)) {
    const inside = new Map<string, number>();
    const selfIn = new Map<string, number>();
    const systemIn = new Map<string, number>();
    let w = 0;
    for (const s of st.samples) {
      w += s.weight;
      const named = framesOf(t, s.stack)
        .map((f) => frameName(t, f))
        .filter((n): n is string => n !== null);
      add(selfIn, named[0] ?? "(native code only)", s.weight);
      for (const n of new Set(named)) add(inside, n, s.weight);
      add(systemIn, named.find((n) => n.startsWith("system ")) ?? "(in no system)", s.weight);
    }
    // The most specific frames that still cover a third of the stretch: of those,
    // the ones covering least are the deepest.
    const cover = [...inside.entries()]
      .filter(([, v]) => v >= 0.3 * w)
      .sort((a, b) => b[1] - a[1])
      .map(([k, v]) => `${k} ${((100 * v) / w).toFixed(0)}%`);
    console.log(`  ${sec(st.start)}s  ${(st.end - st.start).toFixed(0).padStart(5)} ms`);
    console.log(`        mostly inside: ${cover.slice(-3).join("  ›  ") || "?"}`);
    for (const l of topOf(bodies.length ? systemIn : selfIn, 4, w)) console.log(`        ${l}`);
  }
}

/** Per bucket: the thread's running and on-CPU time, and the CPU every other
 * thread of its process used, workers included. */
function printTimeline(t: Thread, timeline: Map<number, { running: number; onCpu: number; known: number }>, bucketMs: number) {
  const others = new Map<number, number>();
  for (const o of threads) {
    if (o === t || o.pid !== t.pid) continue;
    for (const s of samplesOf(o)) {
      if (s.cpu === null) continue;
      const k = Math.floor(s.time / bucketMs);
      others.set(k, (others.get(k) ?? 0) + s.cpu * s.weight);
    }
  }
  console.log(`  per ${bucketMs / 1000} s:   running   on CPU   off CPU   other threads of this process, CPU (cores)`);
  for (const k of [...timeline.keys()].sort((a, b) => a - b)) {
    const b = timeline.get(k)!;
    const off = b.known - b.onCpu;
    const o = others.get(k) ?? 0;
    console.log(`  ${sec(k * bucketMs)}s  ${b.running.toFixed(0).padStart(8)}  ${b.onCpu.toFixed(0).padStart(7)}  ${off.toFixed(0).padStart(8)}   ${(o / bucketMs).toFixed(1).padStart(5)}`);
  }
}

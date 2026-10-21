#!/usr/bin/env bun
/**
 * Drive the web build in a real browser and report what it actually did.
 *
 * ADR-033 notes that `--shot` is compiled out on wasm, which leaves the browser
 * build with no scripted verification at all — every question about it ("did it
 * render?", "how fast?", "was that 70 MB really downloaded?") has to be answered
 * by a human looking at a tab. This is that missing harness: it launches
 * headless Chromium, speaks the DevTools Protocol to it, and prints the three
 * things that turn out to matter.
 *
 *   1. **Which adapter got picked.** Headless Chromium falls back to SwiftShader
 *      — a CPU rasteriser — without saying so anywhere obvious, and frame times
 *      from it are meaningless as a measure of the real thing. The report leads
 *      with the adapter and refuses to present software numbers as if they were
 *      GPU numbers, because that mistake is very easy to make and very hard to
 *      notice afterwards.
 *   2. **Console output**, reassembled. Bevy's wasm logger writes through
 *      `console.log("%c…", style, …)`, so the raw CDP arguments are a format
 *      string plus a pile of CSS. This re-runs the browser's own substitution to
 *      get the line back.
 *   3. **What was fetched.** `Network.*` is the only way to answer whether a
 *      request actually left the browser — the DOM cannot tell you, because
 *      Chrome's preload scanner runs *ahead* of the parser and can start a fetch
 *      for a `<link rel=preload>` that a blocking script was about to prevent.
 *
 * Usage:
 *   bun tools/cdp_probe.ts --url 'http://127.0.0.1:8195/?fps_log' --seconds 30
 *   bun tools/cdp_probe.ts --no-webgpu            # prove the unsupported-browser gate
 *   bun tools/cdp_probe.ts --gpu                  # attempt a real adapter
 *   bun tools/cdp_probe.ts --raw                  # every console line, unfiltered
 *   bun tools/cdp_probe.ts --profile-out /tmp/web  # also write web.cpuprofile + web.callgrind
 *   bun tools/cdp_probe.ts --trace /tmp/web.json   # a Chrome trace of every process (chrome://tracing, Perfetto)
 *   bun tools/cdp_probe.ts --remote --port 9444    # drive a browser already listening there (e.g. an
 *                                                  # ssh -L to a machine with a real GPU) instead of spawning one
 *   bun tools/cdp_probe.ts --drive 20              # from t=20 s, move the camera the way a person does
 *   bun tools/cdp_probe.ts --drive 20 --drive-script zoomout   # …or zoom out in steps, pausing at each
 *   bun tools/cdp_probe.ts --drive 20 --drive-script flight    # …or fly a fixed list of altitudes (`gpu_log.ts`)
 *   bun tools/cdp_probe.ts --drive 20 --drive-script drag      # …or drag the map back and forth, steadily
 *   bun tools/cdp_probe.ts --keep-profile          # a returning visitor: keep the browser profile (its
 *                                                  # IndexedDB cache) from the run before, and after this one
 *
 * A still camera hides most of what a person calls stutter: a settled scene
 * streams nothing. `--drive` repeats a fixed script — pan (left drag), turn
 * (middle drag), zoom in and back out (wheel) — so two runs move through the same
 * places and their frame stats compare.
 *
 * A main thread that stops answering for `--hang-after` seconds is paused in
 * the debugger and its stack printed, a few times over, so a spin and a hang
 * read differently (see "main thread hung" in the report).
 */
import { parseArgs } from "node:util";
import { FLIGHT_ALTITUDES, FLIGHT_DWELL_MS, flyExpression, parseGpuLine, parseHeapBy, parseMapLoad, parseOwnersLine, printGpuTable, type GpuRow, type Mark } from "./gpu_log.ts";

const { values: opt } = parseArgs({
  options: {
    url: { type: "string", default: "http://127.0.0.1:8195/" },
    seconds: { type: "string", default: "30" },
    port: { type: "string", default: "9333" },
    gpu: { type: "boolean", default: false },
    profile: { type: "boolean", default: false },
    "no-webgpu": { type: "boolean", default: false },
    raw: { type: "boolean", default: false },
    "profile-out": { type: "string" },
    "profile-from": { type: "string" },
    "hang-after": { type: "string", default: "8" },
    trace: { type: "string" },
    remote: { type: "boolean", default: false },
    drive: { type: "string" },
    "drive-script": { type: "string", default: "wander" },
    "keep-profile": { type: "boolean", default: false },
  },
});

const RUN_MS = Number(opt.seconds) * 1000;
const PROFILING = opt.profile || opt["profile-out"] !== undefined;
const HANG_MS = Number(opt["hang-after"]) * 1000;
/** When the run started, for timing an exception against the console log. */
const started = Date.now();
const PORT = Number(opt.port);
const profile = `/tmp/vf-cdp-${PORT}`;

// `--headless=new` still wants an explicit size; Bevy sizes its canvas from the
// window, and a 0-sized canvas quietly renders nothing at all.
const flags = [
  "--headless=new",
  "--no-sandbox",
  "--disable-dev-shm-usage",
  "--window-size=1600,900",
  `--user-data-dir=${profile}`,
  `--remote-debugging-port=${PORT}`,
  // Chromium picks a random unused port when told 0; a fixed one keeps this
  // script simple, and the profile dir is per-port so parallel runs don't clash.
];

if (opt["no-webgpu"]) {
  // NOT `--disable-features=WebGPU`: that leaves `navigator.gpu` in place and
  // merely makes `requestAdapter()` resolve null, which is a different failure
  // than the one seen in the wild. Firefox-without-WebGPU has no `navigator.gpu`
  // at all, and that absence is what index.html's gate keys on — so this mode
  // deletes the property instead (below, before any page script runs).
} else if (opt.gpu) {
  // Ask for the real device. Whether this works depends on the machine, which is
  // exactly why the report prints the adapter rather than assuming.
  // The Vulkan flags are what present without losing the device on Linux; a
  // Mac has Metal and needs none of them.
  flags.push("--enable-unsafe-webgpu");
  if (process.platform !== "darwin") {
    flags.push("--enable-features=Vulkan,VulkanFromANGLE,DefaultANGLEVulkan", "--use-angle=vulkan");
  }
} else {
  // Software, and honest about it.
  flags.push("--enable-unsafe-webgpu", "--enable-unsafe-swiftshader", "--use-angle=swiftshader");
}

if (!opt["keep-profile"]) await Bun.$`rm -rf ${profile}`.quiet().nothrow();
// Launch on `about:blank`, not the target URL. Chromium begins loading a
// command-line URL before this script can attach, so the run would either miss
// the opening requests or — if corrected with a reload — fetch the whole module
// twice, which showed up as a 135 MB transfer of a 71 MB file. Attach first,
// then navigate, and every byte is accounted for exactly once.
const chrome = opt.remote
  ? undefined
  : Bun.spawn(["chromium", ...flags, "about:blank"], {
      stdout: "ignore",
      stderr: "ignore",
    });

/** Poll the DevTools endpoint until Chromium is listening and has a page target. */
async function pageTarget(): Promise<string> {
  const deadline = Date.now() + 30_000;
  while (Date.now() < deadline) {
    try {
      const list = (await (await fetch(`http://127.0.0.1:${PORT}/json/list`)).json()) as any[];
      const page = list.find((t) => t.type === "page" && t.webSocketDebuggerUrl);
      // A forwarded browser names its own port; reach it through ours.
      if (page) return page.webSocketDebuggerUrl.replace(/^ws:\/\/[^/]+/, `ws://127.0.0.1:${PORT}`);
    } catch {
      // Not up yet. The endpoint refuses connections until the browser is ready,
      // which is indistinguishable from a crash for the first second or so.
    }
    await Bun.sleep(250);
  }
  throw new Error("Chromium never exposed a page target");
}

const ws = new WebSocket(await pageTarget());
await new Promise<void>((ok, fail) => {
  ws.onopen = () => ok();
  ws.onerror = (e) => fail(e);
});

let nextId = 1;
const pending = new Map<number, (r: any) => void>();
function send(method: string, params: Record<string, unknown> = {}): Promise<any> {
  const id = nextId++;
  ws.send(JSON.stringify({ id, method, params }));
  return new Promise((res) => pending.set(id, res));
}

/** `send`, or `undefined` once `ms` pass — a hung main thread never answers. */
function sendWithin(method: string, params: Record<string, unknown>, ms: number): Promise<any> {
  return Promise.race([send(method, params), Bun.sleep(ms).then(() => undefined)]);
}

const traceEvents: any[] = [];
let traceDone: (() => void) | undefined;

/** The next `Debugger.paused`, for the hang sampler below. */
let onPaused: ((frames: any[]) => void) | undefined;

const console_: string[] = [];
/** The client's `gpu` lines and the camera script's moves, for the GPU table. */
const gpuRows: GpuRow[] = [];
const marks: Mark[] = [];
const since = () => (Date.now() - started) / 1000;
const errors: string[] = [];
const requests = new Map<string, { url: string; bytes: number; status?: number }>();

/** Re-run the browser's own `%c`/`%s` substitution over CDP's raw argument list. */
function describe(a: any): string {
  if (a == null) return "";
  if (a.value !== undefined) return String(a.value);
  if (a.unserializableValue !== undefined) return String(a.unserializableValue);
  if (a.description !== undefined) return String(a.description);
  return "";
}
function formatConsole(args: any[]): string {
  if (!args?.length) return "";
  if (args[0]?.type !== "string") return args.map(describe).join(" ");
  const fmt: string = args[0].value;
  let out = "";
  let ai = 1;
  for (let i = 0; i < fmt.length; i++) {
    if (fmt[i] === "%" && i + 1 < fmt.length) {
      const c = fmt[i + 1];
      if (c === "c") { ai++; i++; continue; }            // a style: consume, print nothing
      if ("sdifoO".includes(c)) { out += describe(args[ai++]); i++; continue; }
      if (c === "%") { out += "%"; i++; continue; }
    }
    out += fmt[i];
  }
  for (; ai < args.length; ai++) out += " " + describe(args[ai]);
  return out.replace(/\s+/g, " ").trim();
}

ws.onmessage = (ev) => {
  const msg = JSON.parse(String(ev.data));
  if (msg.id !== undefined) {
    pending.get(msg.id)?.(msg.result);
    pending.delete(msg.id);
    return;
  }
  switch (msg.method) {
    case "Tracing.dataCollected":
      traceEvents.push(...msg.params.value);
      break;
    case "Tracing.tracingComplete":
      traceDone?.();
      break;
    case "Debugger.paused":
      onPaused?.(msg.params.callFrames ?? []);
      onPaused = undefined;
      break;
    case "Runtime.consoleAPICalled": {
      const line = formatConsole(msg.params.args);
      if (line) console_.push(line);
      const gpu = line && parseGpuLine(line);
      if (gpu) gpuRows.push({ t: since(), f: gpu });
      const heapBy = line && parseHeapBy(line);
      if (heapBy) marks.push({ t: since(), label: `heap by label: ${heapBy}` });
      const owners = line && parseOwnersLine(line);
      if (owners) marks.push({ t: since(), label: `mesh owners: ${owners}` });
      const load = line && parseMapLoad(line);
      if (load) marks.push({ t: since(), label: `map load: ${load}` });
      const pacing = line && /\bcamera pacing (frames\s*=[\s\S]*)$/.exec(line);
      if (pacing) marks.push({ t: since(), label: `camera pacing: ${pacing[1].replace(/;?\s+/g, " ").replace(/;\s*/g, " ").replace(/\s*=\s*/g, "=")}` });
      if (opt.raw) console.log("  │", line);
      break;
    }
    case "Runtime.exceptionThrown": {
      const d = msg.params.exceptionDetails;
      // When it happened and what was logged just before: a bare stack of
      // `wasm-function[N]` frames names nothing, the moment it fired often does.
      const at = ((Date.now() - started) / 1000).toFixed(1);
      const before = console_.slice(-3).map((l) => "      after: " + l.slice(0, 200)).join("\n");
      errors.push(`t=${at}s ${d?.exception?.description ?? d?.text ?? "unknown exception"}\n${before}`);
      break;
    }
    case "Log.entryAdded": {
      const e = msg.params.entry;
      if (e.level === "error") errors.push(`[${e.source}] ${e.text}`);
      if (opt.raw && e.level !== "verbose") console.log(`  ▲ [${e.level} ${e.source}] ${e.text}`);
      break;
    }
    case "Network.requestWillBeSent":
      requests.set(msg.params.requestId, { url: msg.params.request.url, bytes: 0 });
      break;
    case "Network.responseReceived": {
      const r = requests.get(msg.params.requestId);
      if (r) r.status = msg.params.response.status;
      break;
    }
    case "Network.loadingFinished": {
      // The authoritative wire total for the whole response. Summing
      // `Network.dataReceived` chunks instead double-counted badly (130 MB for a
      // 71 MB module) because its two length fields overlap in meaning.
      const r = requests.get(msg.params.requestId);
      if (r) r.bytes = msg.params.encodedDataLength ?? 0;
      break;
    }
  }
};

await send("Runtime.enable");
await send("Log.enable");
await send("Network.enable");
await send("Page.enable");
if (opt["no-webgpu"]) {
  // Runs before *any* script on the page, which is the only way to be a browser
  // that never had WebGPU — a gate in <head> would otherwise see the property
  // and pass. This is what makes the "did the 71 MB actually get fetched?"
  // question answerable, since the DOM alone cannot tell you.
  await send("Page.addScriptToEvaluateOnNewDocument", {
    source: "delete Object.getPrototypeOf(navigator).gpu; delete navigator.gpu;",
  });
}

// The trace is what sees past the page: a main thread blocked inside a
// WebGPU call waits on the GPU process, where no JS profile can follow it.
if (opt.trace) {
  await send("Tracing.start", {
    categories: [
      "toplevel", "gpu", "gpu.dawn", "disabled-by-default-gpu.dawn", "gpu.service", "cc", "viz",
      "blink", "v8", "v8.wasm", "v8.execute", "disabled-by-default-devtools.timeline", "ipc", "mojom",
    ].join(","),
    transferMode: "ReportEvents",
  });
}

// A heartbeat the page cannot fake: requestAnimationFrame stops while a task
// runs, so its age is how long the main thread has been busy.
await send("Page.addScriptToEvaluateOnNewDocument", {
  source: "(function beat(){ window.__vfBeat = performance.now(); requestAnimationFrame(beat); })();",
});

await send("Page.navigate", { url: opt.url });

/**
 * Ask the page what WebGPU actually handed it.
 *
 * Bevy's own `AdapterInfo` cannot answer this: on the browser backend WebGPU
 * deliberately masks adapter identity, so `name` comes back empty whether the
 * device is a real GPU or SwiftShader. Reading "software" out of that empty
 * string is a mistake — `GPUAdapter.info` is the field that actually carries
 * vendor/architecture, and it is the only thing here entitled to decide whether
 * the frame numbers below mean anything.
 */
async function adapterInfo(): Promise<any> {
  // Bounded: a page that hung after the hang check still owes the report.
  const r = await sendWithin("Runtime.evaluate", {
    expression: `(async () => {
      if (!navigator.gpu) return { webgpu: false };
      const a = await navigator.gpu.requestAdapter();
      if (!a) return { webgpu: true, adapter: null };
      const i = a.info ?? (a.requestAdapterInfo ? await a.requestAdapterInfo() : {});
      return { webgpu: true, adapter: {
        vendor: i.vendor, architecture: i.architecture,
        device: i.device, description: i.description,
      } };
    })()`,
    awaitPromise: true,
    returnByValue: true,
  }, 10_000);
  return r?.result?.value;
}

process.stderr.write(`probing ${opt.url} for ${opt.seconds}s …\n`);

// Sample the main thread. Worth saying why this is the interesting thread: Bevy
// builds without `multi_threaded` on wasm, so the entire ECS schedule runs
// serially here, alongside rendering and every `IoTaskPool` future — only
// `platform::workers` escapes to a real Web Worker. If a frame costs a second,
// this profile is where the second went.
//
// Give the scene time to stream in first: the opening seconds profile an empty
// world and would dilute exactly the samples worth having.
if (PROFILING) {
  // `--profile-from 0` keeps the opening seconds: the load is where the hitches are.
  const from = opt["profile-from"] !== undefined ? Number(opt["profile-from"]) * 1000 : Math.min(RUN_MS / 3, 12_000);
  await Bun.sleep(from);
  await send("Profiler.enable");
  await send("Profiler.setSamplingInterval", { interval: 1000 }); // µs
  await send("Profiler.start");
}

/**
 * The `--drive` script, started in the background so the heartbeat watch below
 * keeps running. Every event is awaited, so a stalled page slows the script down
 * rather than queueing a backlog of input it then replays all at once.
 */
async function drive(fromMs: number) {
  await Bun.sleep(Math.max(0, fromMs - (Date.now() - started)));
  const cx = 800, cy = 450;
  const mouse = (type: string, x: number, y: number, button = "none", extra: Record<string, unknown> = {}) =>
    sendWithin("Input.dispatchMouseEvent", { type, x, y, button, ...extra }, 2000);
  const drag = async (button: "left" | "middle", dx: number, dy: number, steps: number) => {
    const buttons = button === "left" ? 1 : 4;
    await mouse("mouseMoved", cx, cy);
    await mouse("mousePressed", cx, cy, button, { buttons, clickCount: 1 });
    for (let i = 1; i <= steps; i++) {
      await mouse("mouseMoved", cx + (dx * i) / steps, cy + (dy * i) / steps, button, { buttons });
      await Bun.sleep(16);
    }
    await mouse("mouseReleased", cx + dx, cy + dy, button, { buttons: 0, clickCount: 1 });
  };
  const wheel = async (deltaY: number, steps: number) => {
    for (let i = 0; i < steps; i++) {
      await mouse("mouseWheel", cx, cy, "none", { deltaX: 0, deltaY });
      await Bun.sleep(16);
    }
  };
  if (opt["drive-script"] === "flight") {
    // Altitudes, not wheel notches: the wheel's direction differs per driver
    // and per camera mode, and a run has to be read against where it was.
    for (const alt of FLIGHT_ALTITUDES) {
      if (Date.now() - started > RUN_MS - 2000) break;
      await sendWithin("Runtime.evaluate", { expression: flyExpression(alt) }, 3000);
      marks.push({ t: since(), label: `flew to ${alt} m` });
      await Bun.sleep(FLIGHT_DWELL_MS);
    }
    return;
  }
  if (opt["drive-script"] === "drag") {
    // A steady drag, back and forth: 3 px every 8 ms for 3 s each way, the same
    // as `firefox_probe --drag`, so the client's `camera pacing` lines compare.
    let dir = 1;
    while (Date.now() - started < RUN_MS - 4000) {
      const x0 = cx - dir * 560;
      await mouse("mouseMoved", x0, cy);
      await mouse("mousePressed", x0, cy, "left", { buttons: 1, clickCount: 1 });
      marks.push({ t: since(), label: `drag ${dir > 0 ? "right" : "left"}, 3 px every 8 ms` });
      for (let i = 1; i <= 375; i++) {
        await mouse("mouseMoved", x0 + dir * 3 * i, cy, "left", { buttons: 1 });
        await Bun.sleep(8);
      }
      await mouse("mouseReleased", x0 + dir * 1125, cy, "left", { buttons: 0, clickCount: 1 });
      dir = -dir;
      await Bun.sleep(500);
    }
    return;
  }
  if (opt["drive-script"] === "zoomout") {
    // Out in four steps with a pause at each, so a streaming burst and the
    // residency it leaves behind can be told apart; then all the way back in.
    for (let step = 0; step < 4 && Date.now() - started < RUN_MS - 2000; step++) {
      await wheel(-100, 40); // a negative deltaY zooms out here
      marks.push({ t: since(), label: `zoomed out, step ${step + 1}` });
      await Bun.sleep(10_000);
    }
    await wheel(100, 160);
    marks.push({ t: since(), label: "zoomed back in" });
    return;
  }
  while (Date.now() - started < RUN_MS - 2000) {
    for (let i = 0; i < 4; i++) await drag("left", -500, 0, 60); // pan east, ~4 s
    await drag("middle", 600, 0, 120); // turn, ~2 s
    await wheel(-100, 40); // zoom in
    await wheel(100, 40); // and back out
    for (let i = 0; i < 4; i++) await drag("left", 0, -300, 60); // pan north
  }
}
if (opt.drive !== undefined) drive(Number(opt.drive) * 1000);

/**
 * Watch the heartbeat for the rest of the run. A main thread that has not run a
 * frame for `--hang-after` seconds (or will not even evaluate an expression) is
 * paused in the debugger a few times, a second apart: the same frames every
 * time is a spin or a loop that will not end, different frames is a slow task.
 * The debugger is only attached at that point — attaching it moves wasm to
 * debug code, and the run before the hang should be measured without it.
 */
const hangStacks: string[][] = [];
let hungAt: number | undefined;
while (Date.now() - started < RUN_MS) {
  await Bun.sleep(2000);
  const r = await sendWithin(
    "Runtime.evaluate",
    { expression: "window.__vfBeat === undefined ? 0 : performance.now() - window.__vfBeat", returnByValue: true },
    3000,
  );
  const age = r?.result?.value;
  if (r !== undefined && (typeof age !== "number" || age < HANG_MS)) continue;
  hungAt = (Date.now() - started) / 1000;
  process.stderr.write(`main thread unresponsive at t=${hungAt.toFixed(0)}s (beat age ${age ?? "no answer"}) — sampling its stack\n`);
  await sendWithin("Debugger.enable", {}, 20_000);
  for (let i = 0; i < 4; i++) {
    const frames = new Promise<any[]>((res) => (onPaused = res));
    await sendWithin("Debugger.pause", {}, 5000);
    const got = await Promise.race([frames, Bun.sleep(10_000).then(() => undefined)]);
    if (!got) {
      hangStacks.push(["(the debugger could not pause it)"]);
      break;
    }
    hangStacks.push(got.map((f: any) => f.functionName || "(anonymous)"));
    await sendWithin("Debugger.resume", {}, 5000);
    await Bun.sleep(1000);
  }
  break;
}

if (opt.trace) {
  const done = new Promise<void>((res) => (traceDone = res));
  await sendWithin("Tracing.end", {}, 10_000);
  await Promise.race([done, Bun.sleep(60_000)]);
  await Bun.write(opt.trace, JSON.stringify({ traceEvents }));
  process.stderr.write(`wrote ${opt.trace} (${traceEvents.length} events)\n`);
}

let profileTop: { name: string; ms: number; pct: number }[] = [];
let inclusiveTop: { name: string; ms: number; pct: number }[] = [];
if (PROFILING) {
  const { profile } = (await sendWithin("Profiler.stop", {}, 30_000)) ?? {};
  if (profile) {
  const byId = new Map<number, any>(profile.nodes.map((n: any) => [n.id, n]));
  const self = new Map<string, number>();
  let total = 0;
  // `samples[i]` is the node running at `timeDeltas[i]` µs after the previous
  // sample, so charging each delta to its node gives self time directly.
  for (let i = 0; i < profile.samples.length; i++) {
    const dt = (profile.timeDeltas[i] ?? 0) / 1000;
    if (dt <= 0) continue;
    const f = byId.get(profile.samples[i])?.callFrame;
    if (!f) continue;
    const name = f.functionName || "(anonymous)";
    self.set(name, (self.get(name) ?? 0) + dt);
    total += dt;
  }
  profileTop = [...self.entries()]
    .sort((a, b) => b[1] - a[1])
    .slice(0, 25)
    .map(([name, ms]) => ({ name, ms, pct: (100 * ms) / total }));

  const tree = profileTree(profile);
  inclusiveTop = [...tree.inclusive.entries()]
    .filter(([name]) => !name.startsWith("("))
    .sort((a, b) => b[1] - a[1])
    .slice(0, 40)
    .map(([name, ms]) => ({ name, ms, pct: (100 * ms) / total }));
  if (opt["profile-out"]) {
    const out = opt["profile-out"];
    await Bun.write(`${out}.cpuprofile`, JSON.stringify(profile));
    await Bun.write(`${out}.callgrind`, callgrind(profile, tree));
    process.stderr.write(`wrote ${out}.cpuprofile (Chrome DevTools) and ${out}.callgrind (kcachegrind)\n`);
  }
  }
}

/**
 * Per-node self and inclusive time (ms) of a `Profiler.stop` profile, plus each
 * function's inclusive time counted once per stack, so recursion does not add
 * a function to itself.
 */
function profileTree(profile: any) {
  const nodes = new Map<number, any>(profile.nodes.map((n: any) => [n.id, n]));
  const selfMs = new Map<number, number>();
  for (let i = 0; i < profile.samples.length; i++) {
    const dt = (profile.timeDeltas[i] ?? 0) / 1000;
    if (dt > 0) selfMs.set(profile.samples[i], (selfMs.get(profile.samples[i]) ?? 0) + dt);
  }
  const inclMs = new Map<number, number>();
  const inclusive = new Map<string, number>();
  const onStack = new Map<string, number>();
  const nameOf = (n: any) => n.callFrame.functionName || "(anonymous)";
  // Iterative post-order: a deep Bevy stack overflows a recursive walk.
  const root = profile.nodes[0];
  const stack: { n: any; i: number }[] = [{ n: root, i: 0 }];
  onStack.set(nameOf(root), 1);
  while (stack.length) {
    const top = stack[stack.length - 1];
    const kids: number[] = top.n.children ?? [];
    if (top.i < kids.length) {
      const child = nodes.get(kids[top.i++]);
      if (!child) continue;
      onStack.set(nameOf(child), (onStack.get(nameOf(child)) ?? 0) + 1);
      stack.push({ n: child, i: 0 });
      continue;
    }
    stack.pop();
    let t = selfMs.get(top.n.id) ?? 0;
    for (const k of kids) t += inclMs.get(k) ?? 0;
    inclMs.set(top.n.id, t);
    const name = nameOf(top.n);
    const depth = onStack.get(name)! - 1;
    if (depth === 0) {
      onStack.delete(name);
      inclusive.set(name, (inclusive.get(name) ?? 0) + t);
    } else {
      onStack.set(name, depth);
    }
  }
  return { nodes, selfMs, inclMs, inclusive, nameOf };
}

/** The profile as a callgrind file, which kcachegrind/qcachegrind open as-is. */
function callgrind(profile: any, tree: ReturnType<typeof profileTree>): string {
  const us = (ms: number) => Math.round(ms * 1000);
  type Fn = { file: string; name: string; self: number; calls: Map<string, { n: number; incl: number }> };
  const fns = new Map<string, Fn>();
  const keyOf = (n: any) => `${n.callFrame.url}\u0000${tree.nameOf(n)}`;
  const fnOf = (n: any): Fn => {
    const k = keyOf(n);
    let f = fns.get(k);
    if (!f) fns.set(k, (f = { file: n.callFrame.url || "(native)", name: tree.nameOf(n), self: 0, calls: new Map() }));
    return f;
  };
  for (const n of profile.nodes) {
    const f = fnOf(n);
    f.self += tree.selfMs.get(n.id) ?? 0;
    for (const k of n.children ?? []) {
      const child = tree.nodes.get(k);
      if (!child) continue;
      const e = f.calls.get(keyOf(child)) ?? { n: 0, incl: 0 };
      e.n += 1;
      e.incl += tree.inclMs.get(k) ?? 0;
      f.calls.set(keyOf(child), e);
    }
  }
  const ids = new Map<string, number>();
  const fileIds = new Map<string, number>();
  const ref = (map: Map<string, number>, v: string) => {
    const id = map.get(v);
    if (id !== undefined) return `(${id})`;
    map.set(v, map.size + 1);
    return `(${map.size}) ${v}`;
  };
  let total = 0;
  for (const f of fns.values()) total += us(f.self);
  const lines = ["# callgrind format", "version: 1", "creator: tools/cdp_probe.ts", "positions: line", "events: us", `summary: ${total}`, ""];
  for (const f of fns.values()) {
    lines.push(`fl=${ref(fileIds, f.file)}`, `fn=${ref(ids, f.name)}`, `0 ${us(f.self)}`);
    for (const [ck, e] of f.calls) {
      const c = fns.get(ck)!;
      lines.push(`cfl=${ref(fileIds, c.file)}`, `cfn=${ref(ids, c.name)}`, `calls=${e.n} 0`, `0 ${us(e.incl)}`);
    }
    lines.push("");
  }
  return lines.join("\n");
}

// ---------------------------------------------------------------- report

const gpu = hungAt === undefined ? await adapterInfo() : undefined;
const adapter = gpu?.adapter
  ? Object.entries(gpu.adapter)
      .filter(([, v]) => v)
      .map(([k, v]) => `${k}=${v}`)
      .join("  ")
  : undefined;
const software =
  !gpu?.webgpu ||
  !gpu?.adapter ||
  /swiftshader|llvmpipe|lavapipe|software|microsoft basic/i.test(JSON.stringify(gpu.adapter));

const fps = console_
  .filter((l) => /\bfps = \d/.test(l))
  .map((l) => ({
    fps: Number(l.match(/\bfps = (\d+)/)?.[1]),
    mean: Number(l.match(/mean_frame_ms = (\d+)/)?.[1]),
    worst: Number(l.match(/worst_frame_ms = (\d+)/)?.[1]),
  }));

const wasm = [...requests.values()].filter((r) => r.url.endsWith(".wasm"));

console.log("\n══ adapter ═══════════════════════════════════════════════");
console.log(" ", adapter ?? `(GPUAdapter.info unavailable: ${JSON.stringify(gpu)})`);
if (software) {
  console.log("  ⚠ SOFTWARE RASTERISER (or no adapter at all).");
  console.log("    Frame times below are CPU rasterisation and say nothing");
  console.log("    about real GPU performance. Do not quote them as such.");
}

console.log("\n══ wasm transfer ═════════════════════════════════════════");
// Listed per request, never merged: trunk emits both a `<link rel=preload>` and
// `init()`'s own fetch for the same module, so "was it downloaded twice?" is a
// real question and a summed total would hide the answer.
if (!wasm.length) console.log("  no .wasm requested");
for (const w of wasm) {
  console.log(`  ${(w.bytes / 1e6).toFixed(1)} MB  status=${w.status ?? "-"}  ${w.url}`);
}
if (wasm.length > 1) console.log(`  ⚠ ${wasm.length} separate requests for the module`);

// The console's "Failed to load resource: … 404" names no URL; this does.
const failed = [...requests.values()].filter((r) => (r.status ?? 0) >= 400);
if (failed.length) {
  console.log("\n══ failed requests ═══════════════════════════════════════");
  for (const r of failed) console.log(`  ${r.status}  ${r.url}`);
}

console.log("\n══ frame stats ═══════════════════════════════════════════");
if (!fps.length) {
  console.log("  none — add ?fps_log to the URL (see env_flag in main.rs)");
} else {
  const sorted = [...fps].map((f) => f.fps).sort((a, b) => a - b);
  const median = sorted[Math.floor(sorted.length / 2)];
  for (const f of fps) console.log(`  fps=${f.fps}  mean=${f.mean}ms  worst=${f.worst}ms`);
  console.log(`  ── median ${median} fps over ${fps.length} samples`);
}

printGpuTable(gpuRows, marks);

if (hangStacks.length) {
  console.log(`\n══ main thread hung at t=${hungAt!.toFixed(0)}s ═════════════════════════════`);
  hangStacks.forEach((frames, i) => {
    console.log(`  pause ${i + 1}:`);
    for (const f of frames.slice(0, 30)) console.log(`    ${f.slice(0, 140)}`);
  });
}

if (PROFILING) {
  console.log("\n══ main-thread inclusive time ════════════════════════════");
  for (const p of inclusiveTop) {
    console.log(`  ${p.pct.toFixed(1).padStart(5)}%  ${p.ms.toFixed(0).padStart(6)} ms  ${p.name.slice(0, 110)}`);
  }
  console.log("\n══ main-thread self time ═════════════════════════════════");
  if (!profileTop.length) console.log("  no samples");
  for (const p of profileTop) {
    console.log(`  ${p.pct.toFixed(1).padStart(5)}%  ${p.ms.toFixed(0).padStart(6)} ms  ${p.name.slice(0, 110)}`);
  }
  if (profileTop.some((p) => /^wasm-function\[/.test(p.name))) {
    console.log("  ⚠ names are indices — this module was built with `strip`.");
    console.log("    Rebuild with CARGO_PROFILE_WEB_RELEASE_STRIP=none to get symbols.");
  }
}

const notable = console_.filter((l) =>
  /panic|error|WARN|not supported|Disabling|not loaded|limited|tile fill|render cost/i.test(l),
);
console.log("\n══ notable log lines ═════════════════════════════════════");
for (const l of [...new Set(notable)].slice(0, 40)) console.log("  " + l.slice(0, 160));

if (errors.length) {
  console.log("\n══ errors ════════════════════════════════════════════════");
  // Whole, not cut to a line: the frames below the first are the only
  // pointer there is into a stripped module.
  for (const e of [...new Set(errors)].slice(0, 15)) console.log("  " + e.slice(0, 6000));
}

console.log(`\n(${console_.length} console lines captured)`);

if (opt.remote) await sendWithin("Page.navigate", { url: "about:blank" }, 5000);
ws.close();
if (opt.remote) {
  // Someone else's browser: leave it running, on a blank page.
  process.exit(errors.length ? 1 : 0);
}
chrome!.kill();
// Chromium still holds its profile open for a moment after `kill`, and racing it
// makes the tool fail *after* it has already printed everything it was asked for.
// The temp dir is per-port and gets cleared on the next run anyway.
await chrome!.exited;
if (!opt["keep-profile"]) await Bun.$`rm -rf ${profile}`.quiet().nothrow();
process.exit(errors.length ? 1 : 0);

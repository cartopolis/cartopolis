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
 */
import { parseArgs } from "node:util";

const { values: opt } = parseArgs({
  options: {
    url: { type: "string", default: "http://127.0.0.1:8195/" },
    seconds: { type: "string", default: "30" },
    port: { type: "string", default: "9333" },
    gpu: { type: "boolean", default: false },
    profile: { type: "boolean", default: false },
    "no-webgpu": { type: "boolean", default: false },
    raw: { type: "boolean", default: false },
  },
});

const RUN_MS = Number(opt.seconds) * 1000;
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
  flags.push("--enable-unsafe-webgpu", "--enable-features=Vulkan", "--use-angle=vulkan");
} else {
  // Software, and honest about it.
  flags.push("--enable-unsafe-webgpu", "--enable-unsafe-swiftshader", "--use-angle=swiftshader");
}

await Bun.$`rm -rf ${profile}`.quiet().nothrow();
// Launch on `about:blank`, not the target URL. Chromium begins loading a
// command-line URL before this script can attach, so the run would either miss
// the opening requests or — if corrected with a reload — fetch the whole module
// twice, which showed up as a 135 MB transfer of a 71 MB file. Attach first,
// then navigate, and every byte is accounted for exactly once.
const chrome = Bun.spawn(["chromium", ...flags, "about:blank"], {
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
      if (page) return page.webSocketDebuggerUrl;
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

const console_: string[] = [];
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
    case "Runtime.consoleAPICalled": {
      const line = formatConsole(msg.params.args);
      if (line) console_.push(line);
      if (opt.raw) console.log("  │", line);
      break;
    }
    case "Runtime.exceptionThrown": {
      const d = msg.params.exceptionDetails;
      errors.push(d?.exception?.description ?? d?.text ?? "unknown exception");
      break;
    }
    case "Log.entryAdded": {
      const e = msg.params.entry;
      if (e.level === "error") errors.push(`[${e.source}] ${e.text}`);
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
  const r = await send("Runtime.evaluate", {
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
  });
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
if (opt.profile) {
  await Bun.sleep(Math.min(RUN_MS / 3, 12_000));
  await send("Profiler.enable");
  await send("Profiler.setSamplingInterval", { interval: 1000 }); // µs
  await send("Profiler.start");
}

await Bun.sleep(RUN_MS);

let profileTop: { name: string; ms: number; pct: number }[] = [];
if (opt.profile) {
  const { profile } = await send("Profiler.stop");
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
}

// ---------------------------------------------------------------- report

const gpu = await adapterInfo();
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

console.log("\n══ frame stats ═══════════════════════════════════════════");
if (!fps.length) {
  console.log("  none — add ?fps_log to the URL (see env_flag in main.rs)");
} else {
  const sorted = [...fps].map((f) => f.fps).sort((a, b) => a - b);
  const median = sorted[Math.floor(sorted.length / 2)];
  for (const f of fps) console.log(`  fps=${f.fps}  mean=${f.mean}ms  worst=${f.worst}ms`);
  console.log(`  ── median ${median} fps over ${fps.length} samples`);
}

if (opt.profile) {
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
  for (const e of [...new Set(errors)].slice(0, 15)) console.log("  " + e.slice(0, 300));
}

console.log(`\n(${console_.length} console lines captured)`);

ws.close();
chrome.kill();
// Chromium still holds its profile open for a moment after `kill`, and racing it
// makes the tool fail *after* it has already printed everything it was asked for.
// The temp dir is per-port and gets cleared on the next run anyway.
await chrome.exited;
await Bun.$`rm -rf ${profile}`.quiet().nothrow();
process.exit(errors.length ? 1 : 0);

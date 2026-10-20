#!/usr/bin/env bun
/**
 * `cdp_probe.ts` for Firefox: load the web build in a real Firefox, print its
 * console, count the frames it actually draws, optionally zoom the camera, and
 * print the client's own GPU counts (`gpu_log.ts`) as a table.
 *
 * Firefox speaks WebDriver BiDi rather than the Chrome DevTools Protocol, so this
 * is its own small driver. It drives the Firefox in `~/webprof/ff/Firefox.app`
 * (macOS) or `$FIREFOX`.
 *
 *   bun tools/firefox_probe.ts --url https://cartopolis.org/viewer/ --seconds 90
 *   bun tools/firefox_probe.ts --zoom-out 30          # from t=30 s, zoom out in four steps
 *   bun tools/firefox_probe.ts --zoom-in 30           # …or in, down to street level
 *   bun tools/firefox_probe.ts --fresh                # a first visit: delete the profile first
 *
 * Without `--fresh` the profile is kept, so a second run is a returning visitor
 * (the IndexedDB caches, the stored shaders, the remembered camera).
 *
 * Two things learnt setting it up, both silent:
 *
 * - **Headless Firefox has no WebGPU canvas**: `canvas.getContext()` returns
 *   null and the client panics creating its surface. This runs a window, so it
 *   needs a logged-in desktop session (the Mac nodes have one).
 * - **Without `browsingContext.setViewport` the page is 85 px tall** on those
 *   nodes, which renders almost nothing and drops wheel input off the page.
 *
 * The frame counter is a `requestAnimationFrame` loop the probe installs: frames
 * drawn per poll and the longest gap between two. The page answering
 * `script.evaluate` is not the same as it drawing: a long task stalls frames
 * while the page still answers between them.
 *
 * Note the wheel: in Firefox a positive `deltaY` zooms this client **out**, the
 * opposite of what Chrome's `Input.dispatchMouseEvent` does.
 */
import { parseArgs } from "node:util";
import { parseGpuLine, parseOwnersLine, printGpuTable, type GpuRow, type Mark } from "./gpu_log.ts";

const { values: opt } = parseArgs({
  options: {
    url: { type: "string", default: "https://cartopolis.org/viewer/" },
    seconds: { type: "string", default: "60" },
    profile: { type: "string", default: "/tmp/vf-firefox" },
    fresh: { type: "boolean", default: false },
    "zoom-out": { type: "string" },
    "zoom-in": { type: "string" },
    port: { type: "string", default: "9231" },
    raw: { type: "boolean", default: false },
    screenshot: { type: "string" },
  },
});

const FIREFOX = process.env.FIREFOX ?? `${process.env.HOME}/webprof/ff/Firefox.app/Contents/MacOS/firefox`;
const prof = opt.profile!;
if (opt.fresh) await Bun.$`rm -rf ${prof}`.nothrow();
await Bun.$`mkdir -p ${prof}`;
await Bun.write(
  `${prof}/user.js`,
  [
    ["dom.webgpu.enabled", true],
    ["gfx.webgpu.ignore-blocklist", true],
    ["dom.max_script_run_time", 0],
    ["browser.shell.checkDefaultBrowser", false],
    ["datareporting.policy.dataSubmissionEnabled", false],
    ["toolkit.telemetry.reportingpolicy.firstRun", false],
    ["browser.aboutwelcome.enabled", false],
    ["browser.startup.homepage_override.mstone", "ignore"],
    ["startup.homepage_welcome_url", ""],
    ["datareporting.policy.firstRunURL", ""],
    ["trailhead.firstrun.didSeeAboutWelcome", true],
  ]
    .map(([k, v]) => `user_pref(${JSON.stringify(k)}, ${JSON.stringify(v)});`)
    .join("\n"),
);

const port = Number(opt.port);
const firefox = Bun.spawn([FIREFOX, "--no-remote", "-profile", prof, "--remote-debugging-port", String(port), "about:blank"], {
  stdout: "ignore",
  stderr: "ignore",
});
const started = Date.now();
const since = () => (Date.now() - started) / 1000;
const stamp = () => since().toFixed(1).padStart(6);

let ws: WebSocket | undefined;
for (let i = 0; i < 60 && !ws; i++) {
  await Bun.sleep(500);
  try {
    const s = new WebSocket(`ws://127.0.0.1:${port}/session`);
    await new Promise<void>((ok, fail) => {
      s.onopen = () => ok();
      s.onerror = () => fail();
    });
    ws = s;
  } catch {
    // Not listening yet.
  }
}
if (!ws) throw new Error("Firefox never opened its BiDi socket");

const gpuRows: GpuRow[] = [];
const marks: Mark[] = [];
const errors: string[] = [];
let id = 0;
const pending = new Map<number, (v: any) => void>();
ws.onmessage = (m) => {
  const msg = JSON.parse(String(m.data));
  if (msg.id !== undefined) {
    pending.get(msg.id)?.(msg);
    pending.delete(msg.id);
    return;
  }
  if (msg.method !== "log.entryAdded") return;
  const e = msg.params;
  const text: string = (e.text ?? (e.args ?? []).map((a: any) => a.value ?? a.type).join(" ")).replace(/%c/g, "");
  const gpu = parseGpuLine(text);
  if (gpu) gpuRows.push({ t: since(), f: gpu });
  const owners = parseOwnersLine(text);
  if (owners) marks.push({ t: since(), label: `mesh owners: ${owners}` });
  if (e.level === "error") errors.push(`t=${since().toFixed(1)}s ${text.slice(0, 300)}`);
  if (opt.raw) console.log(`${stamp()} [${e.level}] ${text.slice(0, 400)}`);
};
const send = (method: string, params: any = {}, ms = 10_000) =>
  new Promise<any>((res) => {
    const i = ++id;
    pending.set(i, res);
    ws!.send(JSON.stringify({ id: i, method, params }));
    setTimeout(() => {
      if (pending.has(i)) {
        pending.delete(i);
        res(undefined);
      }
    }, ms);
  });

await send("session.new", { capabilities: {} });
await send("session.subscribe", { events: ["log.entryAdded"] });
const context = (await send("browsingContext.getTree", {})).result.contexts[0].context;
await send("browsingContext.setViewport", { context, viewport: { width: 1600, height: 900 } });
await send("browsingContext.navigate", { context, url: opt.url, wait: "none" });
console.log(`${stamp()} navigated ${opt.url}`);

const scroll = async (deltaY: number, notches: number) => {
  for (let i = 0; i < notches; i++) {
    await send(
      "input.performActions",
      { context, actions: [{ type: "wheel", id: "wheel", actions: [{ type: "scroll", x: 800, y: 450, deltaX: 0, deltaY }] }] },
      3000,
    );
    await Bun.sleep(16);
  }
};
const zoomFrom = opt["zoom-out"] ?? opt["zoom-in"];
const zoomDy = opt["zoom-out"] !== undefined ? 100 : -100;
let nextZoom = zoomFrom !== undefined ? started + Number(zoomFrom) * 1000 : undefined;
let zoomStep = 0;

// Frames drawn since the last poll, and the longest gap between two.
const COUNTER = `(() => {
  if (window.__ffF === undefined) {
    window.__ffF = 0; window.__ffWorst = 0; window.__ffLast = performance.now();
    const f = () => { const n = performance.now(); window.__ffWorst = Math.max(window.__ffWorst, n - window.__ffLast);
      window.__ffLast = n; window.__ffF++; requestAnimationFrame(f); };
    requestAnimationFrame(f);
  }
  const out = { frames: window.__ffF, worst: Math.round(window.__ffWorst) };
  window.__ffF = 0; window.__ffWorst = 0;
  return JSON.stringify(out);
})()`;
const frames: { t: number; frames: number; worst: number }[] = [];
const end = started + Number(opt.seconds) * 1000;
while (Date.now() < end) {
  await Bun.sleep(2000);
  if (nextZoom !== undefined && Date.now() >= nextZoom && zoomStep < 4) {
    zoomStep++;
    await scroll(zoomDy, 40);
    marks.push({ t: since(), label: `zoomed ${zoomDy > 0 ? "out" : "in"}, step ${zoomStep}` });
    nextZoom = Date.now() + 10_000;
  }
  const r = await send("script.evaluate", { expression: COUNTER, target: { context }, awaitPromise: false }, 4000);
  const v = r?.result?.result?.value;
  if (!v) {
    marks.push({ t: since(), label: "page did not answer within 4 s" });
    continue;
  }
  frames.push({ t: since(), ...JSON.parse(v) });
}

if (opt.screenshot) {
  const shot = await send("browsingContext.captureScreenshot", { context }, 15_000);
  if (shot?.result?.data) await Bun.write(opt.screenshot, Buffer.from(shot.result.data, "base64"));
}
await send("browser.close", {}, 5000);
await Bun.sleep(1500);
firefox.kill();

console.log("\n══ frames drawn ══════════════════════════════════════════");
const events = [
  ...frames.map((f) => ({ t: f.t, s: `${String(f.frames).padStart(4)} frames, longest gap ${f.worst} ms` })),
  ...marks.map((m) => ({ t: m.t, s: `── ${m.label}` })),
].sort((a, b) => a.t - b.t);
for (const e of events) console.log(`  ${e.t.toFixed(0).padStart(5)}s  ${e.s}`);
printGpuTable(gpuRows, marks);
if (errors.length) {
  console.log("\n══ errors ════════════════════════════════════════════════");
  for (const e of [...new Set(errors)].slice(0, 20)) console.log(`  ${e}`);
}
process.exit(0);

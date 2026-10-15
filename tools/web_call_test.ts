#!/usr/bin/env bun
// A call between the web client (alice, headless Chrome with its fake camera
// and microphone — a test tool, not the product) and the native client (bob,
// headless, stand-in camera and tone) through a local simulator.
// docs/notes/calls.md, "Calls in the browser".
//
// Run from a build slot that has `target/debug/{simulator,cartopolis}`, after a
// web build into $DIST:
//
//   CARGO_PROFILE_DEV_DEBUG_ASSERTIONS=false CARGO_PROFILE_DEV_OVERFLOW_CHECKS=false \
//     crates/cartopolis/serve-threads.sh build --dist /tmp/webcalltest/dist
//   bun tools/web_call_test.ts
//
// Debug assertions off, because a debug build keeps the panic on a failing
// system (`lib.rs`, `set_error_handler`) and this container's software GPU
// halts the renderer, after which one does. The Chrome flags below are the
// ones that keep a WebGPU canvas alive here. Prints the `call: received` lines
// from bob and, at the end, the browser's own RTP statistics for both
// directions — the proof that each side decoded the other.
import { spawn } from "bun";
import { mkdirSync, rmSync, readFileSync, existsSync } from "node:fs";

const T = process.env.WEBCALL_DIR ?? "/tmp/webcalltest";
const S = process.env.SLOT ?? process.cwd();
const BIN = `${S}/target/debug`;
const DIST = process.env.DIST ?? `${T}/dist`;
const RUN = `${T}/run`;
const alsa = process.env.ALSA_LIB ?? "/home/agent/opt/alsa/usr/lib/x86_64-linux-gnu";
const env = { ...process.env, PKG_CONFIG_PATH: `${alsa}/pkgconfig`, LD_LIBRARY_PATH: alsa, WGPU_BACKEND: "vulkan" };
const WAIT_S = Number(process.env.WAIT_S ?? 240);

rmSync(RUN, { recursive: true, force: true });
mkdirSync(`${RUN}/bob`, { recursive: true });
const procs: any[] = [];
const cleanup = () => procs.forEach((p) => { try { p.kill(); } catch {} });
process.on("exit", cleanup);

for (const u of ["alice", "bob"]) {
  Bun.spawnSync([`${BIN}/simulator`, "--database-path", `${RUN}/world.db`, "--create-admin", u], {
    env: { ...env, CARTO_ADMIN_PASSWORD: "pw" },
  });
}
// Alice and Bob are contacts, as two people who call each other are: a
// private chat with a stranger starts as a request (`simulator::contacts`).
{
  const { Database } = await import("bun:sqlite");
  const db = new Database(`${RUN}/world.db`);
  const id = (u: string) => (db.query("SELECT id FROM accounts WHERE username = ?").get(u) as any).id;
  for (const [a, b] of [["alice", "bob"], ["bob", "alice"]])
    db.run("INSERT INTO contacts (account, contact, since_ms) VALUES (?, ?, ?)", [id(a), id(b), Date.now()]);
  db.close();
}
procs.push(spawn([`${BIN}/simulator`, "--database-path", `${RUN}/world.db`, "--listen", "127.0.0.1:4777",
  "--http-listen", "127.0.0.1:4778", "--calls-listen", "127.0.0.1:4779"],
  { env: { ...env, RUST_LOG: "info", CARTO_ASSET_DIR: `${RUN}/assets` }, stdout: Bun.file(`${RUN}/sim.log`), stderr: Bun.file(`${RUN}/sim.err`) }));
await Bun.sleep(2000);

const login = await fetch("http://127.0.0.1:4778/api/login", {
  method: "POST", headers: { "content-type": "application/json" },
  body: JSON.stringify({ username: "alice", password: "pw" }),
}).then((r) => r.json());
console.log("alice token", String(login.token).slice(0, 8) + "…");

// The page, with the cross-origin isolation the threaded build needs.
const types: Record<string, string> = { wasm: "application/wasm", js: "text/javascript", html: "text/html", json: "application/json", css: "text/css" };
const server = Bun.serve({
  port: 8195, hostname: "127.0.0.1",
  fetch(req) {
    let path = new URL(req.url).pathname;
    if (path.endsWith("/")) path += "index.html";
    const file = `${DIST}${path}`;
    if (!existsSync(file)) return new Response("no", { status: 404 });
    return new Response(Bun.file(file), { headers: {
      "Content-Type": types[path.split(".").pop()!] ?? "application/octet-stream",
      "Cross-Origin-Opener-Policy": "same-origin", "Cross-Origin-Embedder-Policy": "require-corp",
      "Cross-Origin-Resource-Policy": "same-origin",
    } });
  },
});

const chrome = spawn([process.env.CHROME ?? `${process.env.HOME}/bin/chromium`, "--headless=new", "--no-sandbox",
  "--remote-debugging-port=9333", `--user-data-dir=${RUN}/chrome`, "--use-fake-device-for-media-stream",
  "--use-fake-ui-for-media-stream", "--autoplay-policy=no-user-gesture-required",
  "--enable-unsafe-webgpu", "--enable-features=Vulkan,VulkanFromANGLE,DefaultANGLEVulkan", "--use-angle=vulkan", "--window-size=1024,768", "about:blank"], { stderr: Bun.file(`${RUN}/chrome.err`) });
procs.push(chrome);

async function target(): Promise<string> {
  for (let i = 0; i < 100; i++) {
    try {
      const list = await fetch("http://127.0.0.1:9333/json/list").then((r) => r.json());
      const page = list.find((t: any) => t.type === "page");
      if (page) return page.webSocketDebuggerUrl;
    } catch {}
    await Bun.sleep(200);
  }
  throw new Error("no chrome page");
}
const ws = new WebSocket(await target());
await new Promise((r) => (ws.onopen = r));
let id = 0;
const pending = new Map<number, (v: any) => void>();
const consoleLog: string[] = [];
ws.onmessage = (e) => {
  const m = JSON.parse(String(e.data));
  if (m.id && pending.has(m.id)) { pending.get(m.id)!(m); pending.delete(m.id); }
  if (m.method === "Runtime.consoleAPICalled") {
    const text = m.params.args.map((a: any) => a.value ?? a.description ?? "").join(" ");
    consoleLog.push(text);
  }
  if (m.method === "Runtime.exceptionThrown") consoleLog.push("EXCEPTION " + JSON.stringify(m.params.exceptionDetails).slice(0, 400));
};
const cdp = (method: string, params: any = {}) => new Promise<any>((r) => { const i = ++id; pending.set(i, r); ws.send(JSON.stringify({ id: i, method, params })); });
await cdp("Runtime.enable");
await cdp("Page.enable");

// Remember a signed-in session, the way a returning visitor has one.
const addr = "ws://127.0.0.1:4778/ws";
const user = { version: 1, session: { kind: "remote", server: addr, username: "alice", auto_enter: true }, session_tokens: { [addr]: login.token } };
await cdp("Page.navigate", { url: "http://127.0.0.1:8195/nothing-here" });
await Bun.sleep(1000);
const seeded = await cdp("Runtime.evaluate", { awaitPromise: true, expression: `new Promise((ok, bad) => {
  const open = indexedDB.open("cartopolis", 1);
  open.onupgradeneeded = () => { open.result.createObjectStore("cache"); open.result.createObjectStore("data"); };
  open.onsuccess = () => {
    const tx = open.result.transaction("data", "readwrite");
    tx.objectStore("data").put({ b: new TextEncoder().encode(${JSON.stringify(JSON.stringify(user))}), t: Date.now() }, "user.json");
    tx.oncomplete = () => ok("seeded"); tx.onerror = () => bad(String(tx.error));
  };
  open.onerror = () => bad(String(open.error));
})` });
console.log("indexeddb:", seeded.result?.result?.value ?? JSON.stringify(seeded).slice(0, 300));
await cdp("Page.navigate", { url: "http://127.0.0.1:8195/?answer&camera" });

// Bob calls alice once alice has had a moment to sign in and publish keys.
await Bun.sleep(90000);
procs.push(spawn([`${BIN}/cartopolis`, "--mcp-headless", "--connect", "127.0.0.1:4777", "--mcp-listen", `${RUN}/bob-mcp.sock`], {
  env: { ...env, CARTO_DATA_DIR: `${RUN}/bob`, CARTO_USER: "bob", CARTO_PASSWORD: "pw",
    CARTO_QUALITY: "phone", CARTO_CALL: "alice", CARTO_FAKE_CAMERA: "1", CARTO_FAKE_MIC: "1",
    RUST_LOG: "info,cartopolis::systems::net::calls=debug,cartopolis::systems::net::private_chat=debug" },
  stdout: Bun.file(`${RUN}/bob.log`), stderr: Bun.file(`${RUN}/bob.err`),
}));

const started = Date.now();
while ((Date.now() - started) / 1000 < WAIT_S) {
  await Bun.sleep(10000);
  const bob = existsSync(`${RUN}/bob.log`) ? readFileSync(`${RUN}/bob.log`, "utf8") : "";
  const got = bob.split("\n").filter((l) => l.includes("call: received")).slice(-2);
  const web = consoleLog.filter((l) => /call|received|Call/i.test(l)).slice(-4);
  console.log(`[${Math.round((Date.now() - started) / 1000)}s] bob: ${got.join(" | ") || "-"}\n  web: ${web.join(" | ") || "-"}`);
}
const stats = await cdp("Runtime.evaluate", { awaitPromise: true, returnByValue: true, expression: `(async () => {
  const c = globalThis.cartopolisCall; if (!c) return "no call object";
  const rows = [];
  for (const [name, pc] of [["up", c.up], ["down", c.down]]) {
    if (!pc) continue;
    (await pc.getStats()).forEach((s) => {
      if (s.type === "inbound-rtp" || s.type === "outbound-rtp")
        rows.push([name, s.type, s.kind, "packets=" + (s.packetsReceived ?? s.packetsSent), "frames=" + (s.framesDecoded ?? s.framesEncoded ?? "-"),
          "samples=" + (s.totalSamplesReceived ?? "-"), "concealed=" + (s.concealedSamples ?? "-"), "energy=" + (s.totalAudioEnergy ?? "-")].join(" "));
    });
  }
  rows.push("camera=" +(c.camera ? c.camera.readyState + " " + c.camera.label : "none") + " senderTrack=" + (c.videoSender && c.videoSender.track ? c.videoSender.track.readyState : "none"));
  const video = (d) => ((d && d.sdp) || "").split("m=").find((m) => m.startsWith("video")) || "no video line";
  const pick = (d) => video(d).split("\\r\\n").filter((l) => /^video|a=(sendonly|recvonly|sendrecv|inactive)|a=ssrc:.*cname|a=msid|a=mid|a=rtpmap/.test(l)).join(" | ");
  rows.push("OFFER VIDEO: " + pick(c.up.localDescription));
  rows.push("ANSWER VIDEO: " + pick(c.up.remoteDescription));
  return rows.join("\\n");
})()` });
console.log("BROWSER STATS\n" + (stats.result?.result?.value ?? JSON.stringify(stats).slice(0, 300)));
await Bun.write(`${RUN}/console.log`, consoleLog.join("\n"));
const shot = await cdp("Page.captureScreenshot", { format: "jpeg", quality: 60 });
if (shot.result?.data) await Bun.write(`${RUN}/web.jpg`, Buffer.from(shot.result.data, "base64"));
server.stop();
cleanup();
process.exit(0);

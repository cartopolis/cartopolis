#!/usr/bin/env bun
/**
 * Serve a local web build of the client under /viewer/, the way the site does,
 * and forward every other path to cartopolis.org — so a build that is not
 * deployed can be driven by `tools/cdp_probe.ts` against the real services.
 *
 *   ROOT=crates/cartopolis/dist PORT=8297 bun tools/serve_web_build.ts
 *
 * Cross-origin isolation is not optional: the build uses shared memory, which
 * a page only gets with COOP + COEP. The forwarded request drops `Host` (and
 * friends), or the site answers a request for 127.0.0.1 with its index page and
 * every terrain tile reads as "not a terrain tile".
 */
import { file } from "bun";

const root = process.env.ROOT ?? "crates/cartopolis/dist";
const upstream = process.env.UPSTREAM ?? "https://cartopolis.org";
const isolation = {
  "Cross-Origin-Opener-Policy": "same-origin",
  "Cross-Origin-Embedder-Policy": "require-corp",
  "Cache-Control": "no-store",
};

Bun.serve({
  port: Number(process.env.PORT ?? 8297),
  hostname: "127.0.0.1",
  async fetch(req) {
    const url = new URL(req.url);
    if (url.pathname.startsWith("/viewer")) {
      let path = url.pathname.replace(/^\/viewer/, "");
      if (path === "" || path.endsWith("/")) path += "index.html";
      const local = file(root + path);
      if (await local.exists()) return new Response(local, { headers: isolation });
    }
    const headers = new Headers(req.headers);
    for (const h of ["host", "origin", "referer", "accept-encoding"]) headers.delete(h);
    const answer = await fetch(upstream + url.pathname + url.search, {
      method: req.method,
      headers,
      body: req.body,
    });
    // Bun has already decoded the body, so the encoding headers would lie.
    const out = new Headers(answer.headers);
    for (const [k, v] of Object.entries(isolation)) out.set(k, v);
    out.delete("content-encoding");
    out.delete("content-length");
    return new Response(answer.body, { status: answer.status, headers: out });
  },
});
console.log(`serving ${root} at /viewer/, the rest from ${upstream}`);

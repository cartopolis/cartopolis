#!/usr/bin/env bun
/**
 * Drive a running Cartopolis client from a shell, through its MCP server.
 *
 * The client serves the Model Context Protocol when started with `--mcp-listen`
 * (`crates/cartopolis/src/systems/dev/mcp/`), so an editor's agent can fly its
 * camera and photograph what it sees. This is the same surface without an
 * editor: a smoke test for the server, and the way an agent with no MCP client
 * of its own reaches it.
 *
 * Usage:
 *   export CARTO_MCP_TOKEN=…                 # Settings ▸ General ▸ Editor access
 *   export CARTO_MCP=http://127.0.0.1:4750/mcp    # the default
 *
 *   bun tools/mcp.ts tools                   # what this client offers
 *   bun tools/mcp.ts state                   # where it is looking, and is it loaded
 *   bun tools/mcp.ts view --at 53.2194,6.5665,300 --look=-20,180 --out /tmp/a.jpg
 *   bun tools/mcp.ts view-here --out /tmp/b.jpg    # the same view, moving nothing
 *   bun tools/mcp.ts call view '{"lat":53.2,"lng":6.5,"altitude_m":900}'
 *
 * Reaching a client on another machine is an ssh forward, not a flag — the
 * tunnel is authenticated and encrypted and this is not:
 *
 *   ssh -N -L 4750:127.0.0.1:4750 deck &
 *   bun tools/mcp.ts state
 *
 * Note the `=` in `--look=-20,180`: a leading `-20` reads as a flag otherwise,
 * exactly as it does for the client's own `--look`.
 */
import { parseArgs } from "node:util";

const DEFAULT_ENDPOINT = "http://127.0.0.1:4750/mcp";
const PROTOCOL_VERSION = "2026-07-28";
const CLIENT_NAME = "tools/mcp.ts";

type Json = Record<string, unknown>;

/**
 * One connection's worth of state: the endpoint, the token, a request id.
 *
 * The transport is Streamable HTTP with a plain JSON reply — the client serves
 * statelessly (see `server.rs`), so there is no session to establish and no
 * event stream to parse. `initialize` is still sent first, because the handshake
 * is what tells the server which protocol version we speak.
 */
class Client {
  #nextId = 0;
  #initialized = false;

  constructor(
    private endpoint: string,
    private token: string,
  ) {}

  async rpc(method: string, params?: unknown): Promise<Json> {
    const body: Json = { jsonrpc: "2.0", id: ++this.#nextId, method };
    if (params !== undefined) body.params = params;
    // Every ordinary request under 2026-07-28 restates who is asking and which
    // version they negotiated, in `_meta`. It duplicates the handshake on
    // purpose: the transport is stateless, so a request has to carry everything
    // needed to serve it. Not on `initialize` itself, which is the handshake.
    if (this.#initialized) {
      const params = (body.params ?? {}) as Json;
      params._meta = {
        ...((params._meta as Json) ?? {}),
        "io.modelcontextprotocol/protocolVersion": PROTOCOL_VERSION,
        "io.modelcontextprotocol/clientInfo": { name: CLIENT_NAME, version: "1" },
        "io.modelcontextprotocol/clientCapabilities": {},
      };
      body.params = params;
    }
    const message = await this.#send(body, true);
    if (message.error) {
      const err = message.error as { message?: string; code?: number };
      throw new Error(`${err.message ?? "refused"} (${err.code})`);
    }
    return (message.result ?? {}) as Json;
  }

  async notify(method: string, params?: unknown): Promise<void> {
    const body: Json = { jsonrpc: "2.0", method };
    if (params !== undefined) body.params = params;
    await this.#send(body, false);
  }

  async #send(body: Json, expectReply: boolean): Promise<Json> {
    const headers: Record<string, string> = {
      "Content-Type": "application/json",
      // Both, because a server is allowed to answer either way and ours picks
      // JSON only when the handler emits nothing before its result.
      Accept: "application/json, text/event-stream",
      Authorization: `Bearer ${this.token}`,
    };
    if (this.#initialized) headers["MCP-Protocol-Version"] = PROTOCOL_VERSION;

    // SEP-2243 routing headers, which 2026-07-28 requires and the server
    // validates against the body: `Mcp-Method` always, and `Mcp-Name` for the
    // methods that carry a name — `tools/call` here. They exist so a proxy can
    // route MCP traffic without parsing JSON. A mismatch is a 400, not a silent
    // difference, which is the right trade for a header that duplicates the
    // body.
    headers["Mcp-Method"] = String(body.method);
    const params = body.params as Json | undefined;
    if (body.method === "tools/call" && typeof params?.name === "string") {
      headers["Mcp-Name"] = params.name;
    }

    let response: Response;
    try {
      response = await fetch(this.endpoint, {
        method: "POST",
        headers,
        body: JSON.stringify(body),
      });
    } catch (e) {
      throw new Error(
        `cannot reach ${this.endpoint}: ${e instanceof Error ? e.message : e}. ` +
          `Is the client running with --mcp-listen?`,
      );
    }
    if (!response.ok) {
      const detail = (await response.text()).trim();
      if (response.status === 401) {
        throw new Error(
          "the client refused the token — check CARTO_MCP_TOKEN against " +
            "Settings ▸ General ▸ Editor access",
        );
      }
      throw new Error(`HTTP ${response.status}: ${detail}`);
    }
    const raw = await response.text();
    if (!expectReply || !raw) return {};
    return parseBody(raw, response.headers.get("content-type") ?? "");
  }

  async initialize(): Promise<void> {
    await this.rpc("initialize", {
      protocolVersion: PROTOCOL_VERSION,
      capabilities: {},
      clientInfo: { name: CLIENT_NAME, version: "1" },
    });
    this.#initialized = true;
    await this.notify("notifications/initialized");
  }

  call(name: string, args: Json): Promise<Json> {
    return this.rpc("tools/call", { name, arguments: args });
  }
}

/** A JSON-RPC message, whether it arrived as JSON or as one SSE event. */
function parseBody(raw: string, contentType: string): Json {
  if (!contentType.includes("text/event-stream")) return JSON.parse(raw);
  for (const line of raw.split("\n")) {
    if (line.startsWith("data:")) {
      const payload = line.slice(5).trim();
      if (payload) return JSON.parse(payload);
    }
  }
  throw new Error("the event stream carried no message");
}

/**
 * Print a tool result the way a person wants to read it: the text, the numbers,
 * and where any image went.
 */
async function show(result: Json, out: string | undefined): Promise<void> {
  const content = (result.content ?? []) as Array<Json>;
  for (const block of content) {
    if (block.type === "text") {
      console.log(block.text);
    } else if (block.type === "image") {
      const bytes = Buffer.from(block.data as string, "base64");
      const path = out ?? "/tmp/cartopolis-mcp.jpg";
      await Bun.write(path, bytes);
      console.log(`image: ${bytes.length} bytes -> ${path}`);
    }
  }
  if (result.structuredContent !== undefined) {
    console.log(JSON.stringify(result.structuredContent, null, 2));
  }
  if (result.isError) process.exit(1);
}

function numbers(text: string, want: number, what: string): number[] {
  const parts = text.split(",").map((p) => Number(p.trim()));
  if (parts.length !== want || parts.some(Number.isNaN)) {
    console.error(`${what} wants ${want} comma-separated numbers, got ${text}`);
    process.exit(2);
  }
  return parts;
}

const { values: opt, positionals } = parseArgs({
  allowPositionals: true,
  options: {
    endpoint: { type: "string" },
    token: { type: "string" },
    at: { type: "string" },
    look: { type: "string", default: "-30,0" },
    mode: { type: "string" },
    settle: { type: "string" },
    wait: { type: "string" },
    time: { type: "string" },
    "hide-ui": { type: "boolean", default: false },
    format: { type: "string" },
    "max-edge": { type: "string" },
    out: { type: "string" },
  },
});

const endpoint = opt.endpoint ?? process.env.CARTO_MCP ?? DEFAULT_ENDPOINT;
const token = opt.token ?? process.env.CARTO_MCP_TOKEN ?? "";
const [command, ...rest] = positionals;

if (!command) {
  console.error("usage: bun tools/mcp.ts <tools|state|view|view-here|call> [options]");
  process.exit(2);
}
if (!token) {
  console.error(
    "no token. Copy it from Settings ▸ General ▸ Editor access in the running " +
      "client and export CARTO_MCP_TOKEN.",
  );
  process.exit(2);
}

/** Only the options the caller actually gave: the server's defaults are better
 *  than a second set of defaults here that could drift from them. */
function present(pairs: Array<[string, unknown]>): Json {
  const out: Json = {};
  for (const [key, value] of pairs) {
    if (value !== undefined && value !== null && value !== "") out[key] = value;
  }
  return out;
}

const client = new Client(endpoint, token);
try {
  await client.initialize();
  switch (command) {
    case "tools": {
      const listed = await client.rpc("tools/list", {});
      for (const tool of (listed.tools ?? []) as Array<Json>) {
        console.log(`${tool.name}\n    ${String(tool.description ?? "").trim()}\n`);
      }
      break;
    }
    case "state":
      await show(await client.call("state", {}), undefined);
      break;
    case "view": {
      if (!opt.at) {
        console.error("view wants --at LAT,LNG,ALT");
        process.exit(2);
      }
      const [lat, lng, altitude_m] = numbers(opt.at, 3, "--at");
      const [pitch_deg, yaw_deg] = numbers(opt.look, 2, "--look");
      const args = {
        lat,
        lng,
        altitude_m,
        pitch_deg,
        yaw_deg,
        ...present([
          ["mode", opt.mode],
          ["settle_s", opt.settle === undefined ? undefined : Number(opt.settle)],
          ["wait_s", opt.wait === undefined ? undefined : Number(opt.wait)],
          ["time", opt.time === undefined ? undefined : Number(opt.time)],
          ["format", opt.format],
          ["max_edge_px", opt["max-edge"] === undefined ? undefined : Number(opt["max-edge"])],
          ["hide_ui", opt["hide-ui"] || undefined],
        ]),
      };
      await show(await client.call("view", args), opt.out);
      break;
    }
    case "view-here": {
      const args = present([
        ["settle_s", opt.settle === undefined ? undefined : Number(opt.settle)],
        ["format", opt.format],
        ["max_edge_px", opt["max-edge"] === undefined ? undefined : Number(opt["max-edge"])],
        ["hide_ui", opt["hide-ui"] || undefined],
      ]);
      await show(await client.call("view_here", args), opt.out);
      break;
    }
    case "call": {
      const [tool, args] = rest;
      if (!tool) {
        console.error("call wants a tool name, and optionally a JSON argument object");
        process.exit(2);
      }
      await show(await client.call(tool, JSON.parse(args ?? "{}")), opt.out);
      break;
    }
    default:
      console.error(`unknown command ${command}`);
      process.exit(2);
  }
} catch (e) {
  console.error(e instanceof Error ? e.message : String(e));
  process.exit(1);
}

#!/usr/bin/env bun
/**
 * Drive a running Cartopolis client from a shell, through its MCP server.
 *
 * The client serves the Model Context Protocol on a Unix socket
 * (`crates/cartopolis/src/systems/dev/mcp/`), so an editor's agent can fly its
 * camera and photograph what it sees. This is the same surface without an
 * editor: a smoke test for the server, and the way an agent with no MCP client
 * of its own reaches it.
 *
 * Usage:
 *   bun tools/mcp.ts tools                   # what this client offers
 *   bun tools/mcp.ts state                   # where it is looking, and is it loaded
 *   bun tools/mcp.ts view --at 53.2194,6.5665,300 --look=-20,180 --out /tmp/a.jpg
 *   bun tools/mcp.ts view-here --out /tmp/b.jpg    # the same view, moving nothing
 *   bun tools/mcp.ts call view '{"lat":53.2,"lng":6.5,"altitude_m":900}'
 *
 * The socket is the client's default — `$XDG_RUNTIME_DIR/cartopolis/mcp.sock`,
 * else `$TMPDIR` or `/tmp` + `cartopolis-$UID/mcp.sock` — unless `--socket` or
 * `CARTO_MCP_SOCKET` names another (a client started with `--mcp-listen PATH`).
 * There is no token: the socket's directory is its owner's alone.
 *
 * A client on another machine is reached through its own relay over ssh, which
 * an editor can launch as its MCP command directly:
 *
 *   ssh deck cartopolis mcp                  # stdio ↔ the Deck's client socket
 *
 * Note the `=` in `--look=-20,180`: a leading `-20` reads as a flag otherwise,
 * exactly as it does for the client's own `--look`.
 */
import { parseArgs } from "node:util";
import { join } from "node:path";
import { userInfo } from "node:os";

const PROTOCOL_VERSION = "2025-11-25";
const CLIENT_NAME = "tools/mcp.ts";

type Json = Record<string, unknown>;

/** The client's own rule (`cartopolis_mcp::server::default_socket_path`). */
function defaultSocket(): string {
  const runtime = process.env.XDG_RUNTIME_DIR;
  if (runtime) return join(runtime, "cartopolis", "mcp.sock");
  const tmp = process.env.TMPDIR || "/tmp";
  return join(tmp, `cartopolis-${userInfo().uid}`, "mcp.sock");
}

/**
 * One MCP session: a connection to the socket, newline-delimited JSON-RPC both
 * ways (the stdio framing), and a table of requests waiting on their answers.
 * The connection *is* the session, so `initialize` is sent once and nothing
 * else needs restating per request.
 */
class Client {
  #nextId = 0;
  #buffer = "";
  // One decoder for the stream, so a character split across two reads survives.
  #decoder = new TextDecoder();
  #waiting = new Map<number, { resolve: (m: Json) => void; reject: (e: Error) => void }>();
  #socket: Awaited<ReturnType<typeof Bun.connect>> | undefined;

  constructor(private path: string) {}

  async connect(): Promise<void> {
    try {
      this.#socket = await Bun.connect({
        unix: this.path,
        socket: {
          data: (_socket, chunk) => this.#receive(this.#decoder.decode(chunk, { stream: true })),
          close: () => this.#fail(new Error("the client closed the connection")),
          error: (_socket, e) => this.#fail(e),
        },
      });
    } catch (e) {
      throw new Error(
        `no Cartopolis is serving MCP at ${this.path} — is it running with MCP on? ` +
          `(${e instanceof Error ? e.message : e})`,
      );
    }
  }

  close(): void {
    this.#socket?.end();
  }

  #receive(text: string): void {
    this.#buffer += text;
    let newline: number;
    while ((newline = this.#buffer.indexOf("\n")) >= 0) {
      const line = this.#buffer.slice(0, newline).trim();
      this.#buffer = this.#buffer.slice(newline + 1);
      if (!line) continue;
      const message = JSON.parse(line) as Json;
      // Notifications and server-to-client requests carry no id we sent.
      const pending = this.#waiting.get(message.id as number);
      if (!pending) continue;
      this.#waiting.delete(message.id as number);
      pending.resolve(message);
    }
  }

  #fail(e: Error): void {
    for (const pending of this.#waiting.values()) pending.reject(e);
    this.#waiting.clear();
  }

  #write(body: Json): void {
    this.#socket!.write(JSON.stringify(body) + "\n");
  }

  async rpc(method: string, params?: unknown): Promise<Json> {
    const id = ++this.#nextId;
    const body: Json = { jsonrpc: "2.0", id, method };
    if (params !== undefined) body.params = params;
    const message = await new Promise<Json>((resolve, reject) => {
      this.#waiting.set(id, { resolve, reject });
      this.#write(body);
    });
    if (message.error) {
      const err = message.error as { message?: string; code?: number };
      throw new Error(`${err.message ?? "refused"} (${err.code})`);
    }
    return (message.result ?? {}) as Json;
  }

  notify(method: string, params?: unknown): void {
    const body: Json = { jsonrpc: "2.0", method };
    if (params !== undefined) body.params = params;
    this.#write(body);
  }

  async initialize(): Promise<void> {
    await this.rpc("initialize", {
      protocolVersion: PROTOCOL_VERSION,
      capabilities: {},
      clientInfo: { name: CLIENT_NAME, version: "1" },
    });
    this.notify("notifications/initialized");
  }

  call(name: string, args: Json): Promise<Json> {
    return this.rpc("tools/call", { name, arguments: args });
  }
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
    socket: { type: "string" },
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

const socketPath = opt.socket ?? process.env.CARTO_MCP_SOCKET ?? defaultSocket();
const [command, ...rest] = positionals;

if (!command) {
  console.error("usage: bun tools/mcp.ts <tools|state|view|view-here|call> [options]");
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

const client = new Client(socketPath);
try {
  await client.connect();
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
client.close();

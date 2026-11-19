#!/usr/bin/env bun
// Hold the crate graph to the rules CLAUDE.md's architecture table states.
//
//   bun tools/dep-rules.ts [--quiet]
//
// "No `bevy`", "the simulator never links e2ee outside its tests", "the viewer
// must not depend on `cartopolis`": every one of these held on 2026-11-19, and
// every one held only because each session read the table. A crate that picks
// up bevy through a new path dependency still compiles, still passes its tests,
// and quietly puts the whole engine in the server's build. This reads the
// resolved graph from `cargo metadata` (offline, no build, ~4 s) and fails on
// any forbidden package in a crate's normal or build dependencies, followed
// transitively. Dev-dependencies are allowed everything: a test may link what
// the crate may not.
//
// The graph is resolved for the whole workspace, so features are unified: an
// optional dependency another crate switches on shows up under every crate that
// reaches it. A finding prints the path, so that case reads as what it is.

type Dep = { pkg: string; dep_kinds: { kind: string | null }[] };
type Node = { id: string; deps: Dep[] };
type Pkg = { id: string; name: string };

const quiet = process.argv.includes("--quiet");

const bevy = (n: string) => n === "bevy" || n.startsWith("bevy_") || n === "big_space";
const client = (n: string) => n === "cartopolis" || n === "cartopolis_materials";

// crate → why → what it must not reach. Keep the reasons in CLAUDE.md's words.
const RULES: { crate: string; why: string; forbidden: (n: string) => boolean }[] = [
  { crate: "cartopolis_core", why: "shared with the server: no bevy", forbidden: (n) => bevy(n) || client(n) },
  { crate: "cartopolis_geo", why: "no bevy (client-build-cost.md)", forbidden: (n) => bevy(n) || client(n) },
  { crate: "cartopolis_vio", why: "no bevy (camera-tracking.md)", forbidden: (n) => bevy(n) || client(n) },
  { crate: "cartopolis_gnss", why: "no bevy (rtk-receiver.md)", forbidden: (n) => bevy(n) || client(n) },
  { crate: "cartopolis_chat", why: "no bevy (web-chat.md)", forbidden: (n) => bevy(n) || client(n) },
  { crate: "cartopolis_mcp", why: "no bevy or cartopolis", forbidden: (n) => bevy(n) || client(n) },
  { crate: "cartopolis_face", why: "no bevy (avatar-faces.md)", forbidden: (n) => bevy(n) || client(n) },
  { crate: "cartopolis_human", why: "no bevy (makehuman-avatars.md)", forbidden: (n) => bevy(n) || client(n) },
  { crate: "cartopolis_scenario", why: "no bevy (scenarios.md)", forbidden: (n) => bevy(n) || client(n) },
  { crate: "cartopolis_voice", why: "no bevy (voice-agent.md)", forbidden: (n) => bevy(n) || client(n) },
  {
    crate: "cartopolis_simulator",
    why: "headless, and never holds a private-chat key (encrypted-chat.md, web-chat.md)",
    forbidden: (n) => bevy(n) || client(n) || n === "cartopolis_e2ee" || n === "cartopolis_chat",
  },
  { crate: "cartopolis_viewer", why: "must not depend on cartopolis", forbidden: client },
  { crate: "cartopolis_materials", why: "never depends on cartopolis", forbidden: (n) => n === "cartopolis" },
];

const out = Bun.spawnSync(["cargo", "metadata", "--offline", "--locked", "--format-version", "1"], {
  stderr: "pipe",
});
if (out.exitCode !== 0) {
  console.error(`dep-rules: cargo metadata failed\n${out.stderr.toString()}`);
  process.exit(2);
}
const meta = JSON.parse(out.stdout.toString()) as { packages: Pkg[]; resolve: { nodes: Node[] } };
const name = new Map(meta.packages.map((p) => [p.id, p.name]));
const nodes = new Map(meta.resolve.nodes.map((n) => [n.id, n]));
const byName = new Map(meta.packages.map((p) => [p.name, p.id]));

// Breadth-first over normal and build edges, keeping each package's parent so a
// finding can print the chain that reached it.
function reach(root: string): Map<string, string | null> {
  const parent = new Map<string, string | null>([[root, null]]);
  const queue = [root];
  while (queue.length) {
    const id = queue.shift()!;
    for (const d of nodes.get(id)?.deps ?? []) {
      if (!d.dep_kinds.some((k) => k.kind !== "dev")) continue;
      if (parent.has(d.pkg)) continue;
      parent.set(d.pkg, id);
      queue.push(d.pkg);
    }
  }
  return parent;
}

let failures = 0;
for (const rule of RULES) {
  const root = byName.get(rule.crate);
  if (!root) {
    console.error(`dep-rules: no workspace crate named ${rule.crate} — rename the rule with the crate`);
    failures++;
    continue;
  }
  const parent = reach(root);
  const bad = [...parent.keys()].filter((id) => id !== root && rule.forbidden(name.get(id)!));
  // One finding per first forbidden package on a path, not every bevy_* under it.
  const first = bad.filter((id) => {
    for (let p = parent.get(id); p; p = parent.get(p)) if (bad.includes(p)) return false;
    return true;
  });
  for (const id of first) {
    const chain: string[] = [];
    for (let p: string | null | undefined = id; p; p = parent.get(p)) chain.unshift(name.get(p)!);
    console.error(`${rule.crate} reaches ${name.get(id)} (${rule.why}):\n    ${chain.join(" → ")}`);
    failures++;
  }
  if (!first.length && !quiet) console.log(`ok  ${rule.crate}`);
}
if (failures) {
  console.error(`\ndep-rules: ${failures} forbidden dependenc${failures === 1 ? "y" : "ies"}`);
  process.exit(1);
}

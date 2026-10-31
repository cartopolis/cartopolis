#!/usr/bin/env bun
// Hold docs/notes' front-matter to what `.claude/hooks/note-triggers` can use.
//
//   bun tools/notes-lint.ts [--quiet]
//
// A note reaches the agent working on its files only through its `triggers:`,
// and a trigger that names a file which has since been renamed or deleted fails
// silently: the note is never named again and nobody notices. On 2026-10-31,
// 63 of 784 file-shaped triggers in 39 notes named nothing (`vector_tiles.rs`
// in five notes, the Python pipeline deleted 2026-08-29 in four), four notes had
// no live trigger at all, and the one `check:` line in the notes had been
// failing for three weeks. This runs in the gate's `fmt` step, so it costs
// nothing to keep true.
//
// It fails on:
//   * an unquoted file name (`routing.rs`) or path (`crates/atlas`) in
//     `triggers:` that matches no tracked file
//   * a bare `lib.rs`, `mod.rs`, `main.rs`, `build.rs` or `Cargo.toml`, which
//     every crate has, so the note is named for files nobody meant
//   * a note none of whose file-shaped triggers (module paths included) names a
//     live file, so the hook can never name it
//   * a `check:` line whose command exits non-zero — its claim went stale
//   * a note without `triggers:` or `updated:`
//
// Quoted triggers (`"turn list"`, `"pipeline/ground.rs"`) are keywords for
// grep, and HTTP routes (`/api/tokens`) are not files: neither is held to a
// file. A `module::item` (`free_camera::setup_camera`, `std::fs`) counts as
// live when one of its last two segments is a module file, and is otherwise a
// keyword. Every finding names the note and the token, so the fix is one edit.

import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";

const quiet = process.argv.includes("--quiet");
const root = Bun.spawnSync(["git", "rev-parse", "--show-toplevel"]).stdout.toString().trim();
process.chdir(root);

const tracked = Bun.spawnSync(["git", "ls-files"]).stdout.toString().split("\n").filter(Boolean);
const bases = new Set(tracked.map((f) => f.slice(f.lastIndexOf("/") + 1)));
const stems = new Set<string>();
for (const f of tracked) {
  if (!f.endsWith(".rs")) continue;
  const parts = f.split("/");
  const base = parts[parts.length - 1].replace(/\.rs$/, "");
  stems.add(base === "mod" ? parts[parts.length - 2] : base);
}

const EXT = /\.(rs|wesl|wgsl|ts|tsx|js|sh|toml|json|py|yml|yaml|java|xml|html|css|kt)$/;
const TOO_GENERIC = new Set(["lib.rs", "mod.rs", "main.rs", "Cargo.toml", "build.rs"]);
const MODULE_SKIP = /^(mod|lib|main|plugin|tests?)$/;

function pathMatches(tok: string): boolean {
  return tracked.some(
    (f) => f === tok || f.startsWith(tok + "/") || ("/" + f).includes("/" + tok + "/") || ("/" + f).endsWith("/" + tok),
  );
}

// The hook's tokens: split on anything that cannot be in a path, `::` kept.
const split = (s: string) => s.replace(/::/g, "#").split(/[^A-Za-z0-9_.\/#-]+/).filter(Boolean);

const errors: string[] = [];
const warn = (note: string, msg: string) => errors.push(`${note}: ${msg}`);

const dir = "docs/notes";
const notes = readdirSync(dir).filter((f) => f.endsWith(".md") && f !== "README.md").sort();
for (const name of notes) {
  const path = join(dir, name);
  const text = readFileSync(path, "utf8");
  const fm = text.startsWith("---\n") ? text.slice(4, text.indexOf("\n---", 4)) : "";
  if (!fm) { warn(path, "no front-matter"); continue; }
  for (const key of ["triggers", "updated"]) {
    if (!new RegExp(`^${key}:`, "m").test(fm)) warn(path, `no \`${key}:\``);
  }

  // The triggers block runs from `triggers:` to the next `key:` line.
  const m = fm.match(/^triggers:[\s\S]*?(?=^[a-z_]+:|$(?![\s\S]))/m);
  const block = m ? m[0].replace(/^triggers:/, "") : "";
  const quoted = [...block.matchAll(/"([^"]*)"|`([^`]*)`/g)].flatMap((q) => split(q[1] ?? q[2]));
  const bare = split(block.replace(/"[^"]*"|`[^`]*`/g, " "));
  let live = 0;
  let fileShaped = 0;
  for (const [toks, strict] of [[bare, true], [quoted, false]] as const) {
    for (let tok of toks) {
      if (tok.startsWith("/")) continue; // an HTTP route
      tok = tok.replace(/^\.\//, "").replace(/\.+$/, "").replace(/\/+$/, "");
      if (!tok) continue;
      if (tok.includes("#")) {
        const cands = tok.split("#").slice(-2).filter((p) => /^[a-z][a-z0-9_]+$/.test(p) && !MODULE_SKIP.test(p));
        if (!cands.length) continue;
        fileShaped++;
        if (cands.some((c) => stems.has(c))) live++;
      } else if (tok.includes("/")) {
        fileShaped++;
        if (pathMatches(tok)) live++;
        else if (strict) warn(path, `trigger \`${tok}\` matches no tracked file`);
      } else if (EXT.test(tok)) {
        fileShaped++;
        if (TOO_GENERIC.has(tok)) {
          if (strict) warn(path, `trigger \`${tok}\` is every crate's — give it its path (crates/<crate>/…/${tok})`);
        } else if (bases.has(tok)) live++;
        else if (strict) warn(path, `trigger \`${tok}\` matches no tracked file`);
      }
    }
  }
  if (live === 0) warn(path, "no trigger names a live file, so the hook never names this note");

  // `check:` lines — a shell one-liner, in backticks, that fails when the claim
  // stops being true.
  for (const line of text.split("\n")) {
    const c = line.match(/^check:\s*`(.+)`\s*$/);
    if (!c) continue;
    const r = Bun.spawnSync(["bash", "-c", c[1]], { cwd: root });
    if (r.exitCode !== 0) warn(path, `check failed, the claim beside it is stale: ${c[1]}`);
  }
}

if (errors.length) {
  for (const e of errors) console.error(e);
  console.error(`\n${errors.length} problem(s) in ${notes.length} notes — docs/notes/README.md has the trigger shapes.`);
  process.exit(1);
}
if (!quiet) console.log(`${notes.length} notes: every trigger names a live file, every check holds`);

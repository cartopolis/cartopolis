#!/usr/bin/env bun
// The client's module graph: which modules import which, and the import cycles.
//
//   bun tools/module-graph.ts [--json out.json]
//
// Every .rs under crates/cartopolis/src is a module (a subfolder folds into its
// module: theme/*, mcp/*); `cfg(test)` blocks, tests.rs files and comments are
// left out; names re-exported from cartopolis_geo count as outside the crate.
// Prints the strongly connected components larger than one and the most-
// imported modules. docs/notes/module-graph.md has what the numbers have been
// and what moved them; measure before and after a refactor that claims to
// untangle anything.
import { readdirSync, readFileSync, statSync, writeFileSync } from "fs";
import { join, relative } from "path";
const ROOT = new URL("../crates/cartopolis/src", import.meta.url).pathname;
const files: string[] = [];
(function walk(d: string) { for (const e of readdirSync(d)) { const p = join(d, e); if (statSync(p).isDirectory()) walk(p); else if (p.endsWith(".rs") && !p.endsWith("/tests.rs")) files.push(p); } })(ROOT);
const GROUPS = new Set(["dev","gui","map","nav","net","player"]);
// strip comments + string contents
function strip(s: string): string {
  let o = ""; let i = 0; const n = s.length;
  while (i < n) {
    const c = s[i], d = s[i+1];
    if (c === "/" && d === "/") { while (i < n && s[i] !== "\n") i++; continue; }
    if (c === "/" && d === "*") { let depth = 1; i += 2; while (i < n && depth) { if (s[i]==="/"&&s[i+1]==="*"){depth++;i+=2;} else if (s[i]==="*"&&s[i+1]==="/"){depth--;i+=2;} else { if (s[i]==="\n") o+="\n"; i++; } } continue; }
    if (c === "r" && (d === "#" || d === '"') && !/\w/.test(s[i-1] ?? "")) { let j = i+1, h = 0; while (s[j]==="#"){h++;j++;} if (s[j]==='"') { const end = '"' + "#".repeat(h); const k = s.indexOf(end, j+1); o += '""'; for (const ch of s.slice(i, k+end.length)) if (ch==="\n") o+="\n"; i = k + end.length; continue; } }
    if (c === '"') { let j = i+1; while (j < n && s[j] !== '"') { if (s[j]==="\\") j++; j++; } o += '""'; for (const ch of s.slice(i, j+1)) if (ch==="\n") o+="\n"; i = j+1; continue; }
    if (c === "'") { const m = /^'(\\.[^']*|[^'\\])'/.exec(s.slice(i, i+12)); if (m) { o += "' '"; i += m[0].length; continue; } }
    o += c; i++;
  }
  return o;
}
function stripTests(s: string): string {
  const re = /#\[cfg\(test\)\]\s*(pub(\(\w+\))?\s+)?mod\s+\w+\s*\{/g; let m; let out = s;
  while ((m = re.exec(out))) { let i = m.index + m[0].length, depth = 1; while (i < out.length && depth) { if (out[i]==="{") depth++; else if (out[i]==="}") depth--; i++; } const lines = out.slice(m.index, i).split("\n").length-1; out = out.slice(0, m.index) + "\n".repeat(lines) + out.slice(i); re.lastIndex = m.index; }
  return out;
}
function modPath(f: string): string[] {
  const r = relative(ROOT, f).replace(/\.rs$/, "").split("/");
  if (r[r.length-1] === "mod") r.pop();
  if (r.length === 1 && (r[0] === "lib" || r[0] === "main")) return [];
  return r;
}
// node for an absolute module path (segments after crate)
const geoReexports = new Set<string>();
const sysMod = readFileSync(join(ROOT,"systems/mod.rs"),"utf8");
{ const m = /pub use cartopolis_geo::\{([^}]*)\}/.exec(sysMod); if (m) for (const x of m[1].split(",")) geoReexports.add(x.trim()); }
const flatSys = new Map<string,string>(); // name -> group
const filePathNode = new Map<string,string>();
function nodeOfFile(f: string): string | null {
  const p = modPath(f);
  if (p.length === 0) return null;
  if (p[0] === "systems") {
    if (p.length <= 2) return null; // systems/mod, group mod
    if (p.length === 3 && p[2] === "plugin") return null;
    return p[2];
  }
  if ((p[0] === "platform" || p[0] === "utils")) return p.length >= 2 ? `${p[0]}::${p[1]}` : p[0];
  return p[0];
}
for (const f of files) { const p = modPath(f); if (p[0]==="systems" && p.length===3 && p[2]!=="plugin") flatSys.set(p[2], p[1]); }
function resolve(abs: string[]): { node: string | null, rest: string[] } {
  if (abs.length === 0) return { node: null, rest: [] };
  if (abs[0] === "systems") {
    let i = 1; if (GROUPS.has(abs[1])) i = 2;
    const m = abs[i]; if (!m) return { node: null, rest: [] };
    if (geoReexports.has(m)) return { node: null, rest: [] };
    if (flatSys.has(m)) return { node: m, rest: abs.slice(i+1) };
    return { node: null, rest: [] }; // plugin types etc
  }
  if (abs[0] === "platform" || abs[0] === "utils") {
    const sub = abs[1]; const fsub = files.find(f => modPath(f).join("/") === `${abs[0]}/${sub}`);
    if (sub && fsub) return { node: `${abs[0]}::${sub}`, rest: abs.slice(2) };
    return { node: abs[0], rest: abs.slice(1) };
  }
  const top = files.find(f => { const p = modPath(f); return p.length===1 && p[0]===abs[0]; }) || files.find(f=>modPath(f)[0]===abs[0]);
  if (top) return { node: abs[0], rest: abs.slice(1) };
  return { node: null, rest: [] };
}
// absolute path from a relative path in file module mp
function absolutize(segs: string[], mp: string[], fileIsMod: boolean): string[] | null {
  // module path for `self`: the file's module. super = parent.
  let base: string[];
  if (segs[0] === "crate") return segs.slice(1);
  if (segs[0] === "self") { base = mp.slice(); segs = segs.slice(1); }
  else if (segs[0] === "super") { base = mp.slice(); while (segs[0] === "super") { base.pop(); segs = segs.slice(1); } }
  else if (segs[0] === "systems" && mp.length <= 1) return segs; // lib.rs or top-level `use crate::systems` then systems::
  else return null;
  if (segs[0] === "self") segs = segs.slice(1);
  return [...base, ...segs];
}
// parse use trees
function expandUse(tree: string): string[][] {
  tree = tree.replace(/\s+as\s+\w+/g, "").replace(/\s+/g, "");
  function parse(s: string, prefix: string[]): string[][] {
    const out: string[][] = []; let i = 0;
    const parts: string[] = []; let depth = 0, start = 0;
    for (; i < s.length; i++) { if (s[i]==="{") depth++; else if (s[i]==="}") depth--; else if (s[i]==="," && depth===0) { parts.push(s.slice(start,i)); start=i+1; } }
    parts.push(s.slice(start));
    for (const p of parts) { if (!p) continue;
      const b = p.indexOf("{");
      if (b >= 0) { const pre = p.slice(0,b).replace(/::$/,""); const inner = p.slice(b+1, p.lastIndexOf("}")); out.push(...parse(inner, [...prefix, ...(pre? pre.split("::"):[])])); }
      else { const q = p.split(/as/)[0]; const segs = p.split("::"); out.push([...prefix, ...segs]); }
    }
    return out;
  }
  return parse(tree, []);
}
type Edge = { names: Map<string, number> };
const edges = new Map<string, Map<string, Set<string>>>(); // from -> to -> names
const nodes = new Set<string>();
const refsAt = new Map<string, string[]>(); // "from->to" -> file:line samples
function addEdge(from: string, to: string, name: string | null, loc: string) {
  if (from === to) return;
  if (!edges.has(from)) edges.set(from, new Map());
  const m = edges.get(from)!; if (!m.has(to)) m.set(to, new Set());
  if (name) m.get(to)!.add(name);
  const k = `${from}->${to}`; if (!refsAt.has(k)) refsAt.set(k, []); const a = refsAt.get(k)!; if (a.length < 6 && !a.includes(loc)) a.push(loc + (name?`(${name})`:""));
}
const decls = new Map<string, Map<string,string>>(); // node -> name -> kind
for (const f of files) {
  const from = nodeOfFile(f); const mp = modPath(f);
  const raw = readFileSync(f, "utf8"); const src = stripTests(strip(raw));
  if (from) nodes.add(from);
  // declarations
  if (from) { const dm = decls.get(from) ?? new Map(); decls.set(from, dm);
    for (const m of src.matchAll(/pub(?:\([\w:]+\))?\s+(const|static|struct|enum|fn|type|trait)\s+(\w+)(\s*;)?/g)) dm.set(m[2], m[1]==="struct" && m[3] ? "unit" : m[1]);
  }
  if (!from) continue;
  const rel = relative(ROOT, f);
  const lineOf = (idx: number) => src.slice(0, idx).split("\n").length;
  const aliases = new Map<string,string>(); // local ident -> node (module imported whole)
  for (const m of src.matchAll(/\buse\s+([^;]+);/g)) {
    for (const segs0 of expandUse(m[1])) {
      let segs = segs0; let alias: string | null = null;
      const abs = absolutize(segs, mp, false); if (!abs) continue;
      if (abs[abs.length-1] === "self") abs.pop();
      const r = resolve(abs); if (!r.node) continue;
      const name = r.rest[0] ?? null;
      if (!name) aliases.set(abs[abs.length-1], r.node);
      addEdge(from, r.node, name === "*" ? "*" : name, `${rel}:${lineOf(m.index!)}`);
    }
  }
  // `use crate::systems;` or `use crate::systems::map;`: groups as alias prefixes
  const prefixAliases = new Map<string,string[]>();
  for (const m of src.matchAll(/\buse\s+([^;]+);/g)) for (const segs of expandUse(m[1])) { const abs = absolutize(segs, mp, false); if (!abs) continue; if (abs.at(-1)==="self") abs.pop(); if ((abs.length===1 && abs[0]==="systems") || (abs.length===2 && abs[0]==="systems" && GROUPS.has(abs[1])) || (abs.length===1 && (abs[0]==="platform"||abs[0]==="utils"))) prefixAliases.set(abs.at(-1)!, abs); }
  // inline paths outside use statements
  const body = src.replace(/\buse\s+[^;]+;/g, (s) => s.replace(/[^\n]/g, " "));
  for (const m of body.matchAll(/\b((?:crate|super|self|systems|platform|utils|[a-z_][a-z0-9_]*)(?:::[A-Za-z_]\w*)+)/g)) {
    const segs = m[1].split("::"); let abs: string[] | null = null;
    if (["crate","super","self"].includes(segs[0])) abs = absolutize(segs, mp, false);
    else if (prefixAliases.has(segs[0])) abs = [...prefixAliases.get(segs[0])!, ...segs.slice(1)];
    else if (aliases.has(segs[0])) { addEdge(from, aliases.get(segs[0])!, segs[1], `${rel}:${lineOf(m.index!)}`); continue; }
    else if (segs[0] === "systems" && mp.length <= 1) abs = segs;
    if (!abs) continue;
    const r = resolve(abs); if (!r.node) continue;
    addEdge(from, r.node, r.rest[0] ?? null, `${rel}:${lineOf(m.index!)}`);
  }
}
// output
const out = { nodes: [...nodes].sort(), edges: [] as any[], decls: Object.fromEntries([...decls].map(([k,v])=>[k,Object.fromEntries(v)])) };
for (const [f, m] of edges) for (const [t, names] of m) out.edges.push({ from: f, to: t, names: [...names], at: refsAt.get(`${f}->${t}`) });
const jsonAt = process.argv.indexOf("--json");
if (jsonAt > 0) writeFileSync(process.argv[jsonAt + 1], JSON.stringify(out, null, 1));

// ── The cycles ──────────────────────────────────────────────────────────────
{
  const adj = new Map<string, string[]>();
  for (const n of out.nodes as string[]) adj.set(n, []);
  for (const e of out.edges as { from: string; to: string }[]) adj.get(e.from)!.push(e.to);
  let i = 0;
  const index = new Map<string, number>(), low = new Map<string, number>(), on = new Set<string>();
  const stack: string[] = [], sccs: string[][] = [];
  const visit = (v: string) => {
    index.set(v, i); low.set(v, i); i++; stack.push(v); on.add(v);
    for (const w of adj.get(v)!) {
      if (!index.has(w)) { visit(w); low.set(v, Math.min(low.get(v)!, low.get(w)!)); }
      else if (on.has(w)) low.set(v, Math.min(low.get(v)!, index.get(w)!));
    }
    if (low.get(v) === index.get(v)) {
      const c: string[] = []; let w: string;
      do { w = stack.pop()!; on.delete(w); c.push(w); } while (w !== v);
      sccs.push(c);
    }
  };
  for (const n of adj.keys()) if (!index.has(n)) visit(n);
  sccs.sort((a, b) => b.length - a.length);
  console.log(`${adj.size} modules, ${out.edges.length} imports`);
  for (const c of sccs.filter((c) => c.length > 1)) console.log(`cycle of ${c.length}: ${c.sort().join(" ")}`);
  const indeg = new Map<string, number>();
  for (const e of out.edges as { to: string }[]) indeg.set(e.to, (indeg.get(e.to) ?? 0) + 1);
  console.log("most imported:", [...indeg].sort((a, b) => b[1] - a[1]).slice(0, 12).map(([m, n]) => `${m} ${n}`).join(", "));
}

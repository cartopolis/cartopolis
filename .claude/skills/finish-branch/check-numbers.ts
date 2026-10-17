#!/usr/bin/env bun
// Refuse a branch that claims a number somebody else already holds.
//
//   bun .claude/skills/finish-branch/check-numbers.ts [--base origin/main] [--no-live]
//
// Three things in this repository are numbered by hand, and git cannot see a
// clash between two of them, because two branches that each append "the next
// one" touch different lines or different files and merge clean:
//
//   * simulator migrations   crates/simulator/migrations/V<n>__<name>.sql
//   * wire revisions         PROTOCOL_HISTORY's `version: <n>` rows
//   * wire message kinds     `MessageKind`'s `<Name> = <n>,` discriminants
//
// It has happened to all three. Two branches each added a V11 and a V12, both
// merged clean, and the public simulator crash-looped for ten minutes
// (docs/notes/migration-numbers-are-claimed-by-a-deploy.md); a migration was
// renumbered V25 → V26 by hand between two sessions (41e91eb); and a branch
// claimed message kinds and a protocol revision that main had already taken
// (the global CLAUDE.md). The tests that exist — the gapless history, the
// kind ↔ wire bijection — pass on each side, because each side's numbers are
// consistent *with themselves*.
//
// So this compares the branch against `--base`, before the rebase: a number the
// branch added that main has since added *as something else* is a clash, and is
// reported with both claimants. It also reads the live simulator's migration
// history over `ssh host` when that works, because a migration number is claimed
// by a deploy, not by a merge — `deploy-sim.yml` will deploy any ref, so the live
// database can hold a version no branch on main has ever named.
//
// Exit 0: clear. Exit 1: a clash, with what to do. Exit 2: usage.

import { parseArgs } from "node:util";
import { spawnSync } from "node:child_process";

const { values: args } = parseArgs({
    options: {
        base: { type: "string", default: "origin/main" },
        "no-live": { type: "boolean", default: false },
        help: { type: "boolean", short: "h", default: false },
    },
});
if (args.help) {
    console.log("usage: check-numbers.ts [--base origin/main] [--no-live]");
    process.exit(0);
}

const MIGRATIONS = "crates/simulator/migrations";
const PROTOCOL = "crates/core/src/protocol.rs";

function git(...a: string[]): string | null {
    const r = spawnSync("git", a, { encoding: "utf8", maxBuffer: 64 << 20 });
    return r.status === 0 ? r.stdout : null;
}

const base = args.base!;
const mergeBase = git("merge-base", "HEAD", base)?.trim();
if (!mergeBase) {
    console.error(`check-numbers: no merge base between HEAD and ${base}`);
    process.exit(2);
}

// ── What each tree claims ───────────────────────────────────────────────────

/** version → migration name, for one tree. */
function migrations(rev: string): Map<number, string[]> {
    const out = new Map<number, string[]>();
    const ls = git("ls-tree", "--name-only", `${rev}:${MIGRATIONS}`) ?? "";
    for (const f of ls.split("\n")) {
        const m = /^V(\d+)__(.+)\.sql$/.exec(f.trim());
        if (!m) continue;
        const v = Number(m[1]);
        out.set(v, [...(out.get(v) ?? []), m[2]]);
    }
    return out;
}

/** The parts of protocol.rs that carry numbers, for one tree. */
function protocol(rev: string): { revisions: Map<number, string>; kinds: Map<number, string> } {
    const src = git("show", `${rev}:${PROTOCOL}`) ?? "";
    const revisions = new Map<number, string>();
    // A row is `version: N,` followed (a line or two later) by its summary; the
    // summary is the row's identity, since the number is the thing in dispute.
    const rowRe = /version:\s*(\d+),[\s\S]*?summary:\s*"((?:[^"\\]|\\.){0,80})/g;
    const hist = /PROTOCOL_HISTORY[^=]*=\s*&\[([\s\S]*?)\n\];/.exec(src)?.[1] ?? "";
    for (const m of hist.matchAll(rowRe)) revisions.set(Number(m[1]), m[2]);

    const kinds = new Map<number, string>();
    const body = /pub enum MessageKind\s*\{([\s\S]*?)\n\}/.exec(src)?.[1] ?? "";
    for (const m of body.matchAll(/^\s*([A-Z][A-Za-z0-9]*)\s*=\s*(\d+)\s*,/gm)) {
        kinds.set(Number(m[2]), m[1]);
    }
    return { revisions, kinds };
}

const problems: string[] = [];

/** Numbers `ours` added since the merge base that `theirs` also added, as something else. */
function clashes<T>(
    what: string,
    before: Map<number, T>,
    ours: Map<number, T>,
    theirs: Map<number, T>,
    same: (a: T, b: T) => boolean,
    show: (t: T) => string,
    fix: string,
) {
    for (const [n, mine] of ours) {
        if (before.has(n) && same(before.get(n)!, mine)) continue; // not ours
        const other = theirs.get(n);
        if (other === undefined || same(other, mine)) continue; // free, or the same change on both sides
        if (before.has(n) && same(before.get(n)!, other)) continue; // main kept the old one; ours changed it
        problems.push(`${what}${n}: this branch adds ${show(mine)}, but ${base} already has ${show(other)}.\n    ${fix}`);
    }
}

// ── Migrations ──────────────────────────────────────────────────────────────

const sameList = (a: string[], b: string[]) => a.join(",") === b.join(",");
const migBase = migrations(mergeBase);
const migOurs = migrations("HEAD");
const migMain = migrations(base);

for (const [v, names] of migOurs) {
    if (names.length > 1) problems.push(`migration V${v}: two files share the number on this branch (${names.join(", ")}).`);
}
clashes(
    "migration V",
    migBase,
    migOurs,
    migMain,
    sameList,
    (n) => n.join(" and "),
    "Renumber yours above main's highest — but first read the live history (below), and if your branch was ever deployed, read docs/notes/migration-numbers-are-claimed-by-a-deploy.md: renumbering after a deploy does not repair it.",
);

// ── Wire protocol ───────────────────────────────────────────────────────────

const pBase = protocol(mergeBase);
const pOurs = protocol("HEAD");
const pMain = protocol(base);

clashes(
    "protocol revision ",
    pBase.revisions,
    pOurs.revisions,
    pMain.revisions,
    (a, b) => a === b,
    (s) => `"${s}…"`,
    "Move your row after main's last one. If a client or server carrying your number has shipped, it speaks a different protocol under that number — say so in the row.",
);
clashes(
    "message kind ",
    pBase.kinds,
    pOurs.kinds,
    pMain.kinds,
    (a, b) => a === b,
    (s) => s,
    "Take the next number after main's highest. Never reuse a retired one (see the note at kind 5).",
);

// ── The live database ───────────────────────────────────────────────────────
// Best-effort: not every machine that lands has a key to the host, and a
// landing must not depend on a host it cannot reach. Silence there is reported,
// not treated as a pass.

if (!args["no-live"]) {
    const r = spawnSync(
        "ssh",
        [
            "-o", "BatchMode=yes", "-o", "ConnectTimeout=5", "host",
            `sudo sqlite3 /data/cartopolis/sim/data/world.db "select version || '|' || name from refinery_schema_history order by version;"`,
        ],
        { encoding: "utf8", timeout: 20_000 },
    );
    if (r.status !== 0) {
        console.log("check-numbers: could not read the live migration history (no `ssh host` here?) — skipped that half");
    } else {
        const live = new Map<number, string>();
        for (const line of r.stdout.split("\n")) {
            const [v, name] = line.trim().split("|");
            if (v) live.set(Number(v), name);
        }
        const liveMax = Math.max(0, ...live.keys());
        for (const [v, names] of migOurs) {
            const applied = live.get(v);
            // Main holding the applied one means the clash is already reported above.
            if (applied !== undefined && !names.includes(applied) && !migMain.get(v)?.includes(applied)) {
                problems.push(
                    `migration V${v}: the live database applied V${v}__${applied}, and this branch has V${v}__${names.join("/")}.\n` +
                        "    Deploying this would stop the simulator (refinery: divergent). Some ref carrying that migration was deployed without being on main — see docs/notes/migration-numbers-are-claimed-by-a-deploy.md for the three repairs.",
                );
            } else if (applied === undefined && v < liveMax && !migMain.has(v)) {
                problems.push(
                    `migration V${v}: unapplied in the live database, whose history already reaches V${liveMax}.\n` +
                        "    A migration below the live maximum is not the next one; renumber it above V" + liveMax + ".",
                );
            }
        }
        for (const [v, name] of live) {
            // Main having it is fine: the rebase brings it in.
            if (!migOurs.has(v) && !migMain.has(v)) {
                problems.push(
                    `migration V${v}: the live database applied V${v}__${name}, which neither this branch nor ${base} has.\n` +
                        "    A deploy of this branch would refuse to start (refinery: missing). Find the ref that was deployed and land its migration first.",
                );
            }
        }
    }
}

if (problems.length) {
    console.error(`check-numbers: ${problems.length} clash(es) against ${base}:\n`);
    for (const p of problems) console.error(`  ✗ ${p}\n`);
    process.exit(1);
}
console.log(`check-numbers: migrations, protocol revisions and message kinds are clear against ${base}`);

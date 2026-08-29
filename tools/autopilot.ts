#!/usr/bin/env bun
//
// The autopilot: keep the work moving while nobody is available to steer it.
//
//   bun tools/autopilot.ts tick     # one pass — file a ticket if the quota allows, ship what is ready
//   bun tools/autopilot.ts status   # what it would do, and why
//   bun tools/autopilot.ts ship     # only the shipping half
//   bun tools/autopilot.ts generate # only the ticket half
//
// It is two jobs with one config file, and it deliberately **runs no language
// model**. Both halves are decisions a script can make: which frontier item has
// no ticket yet (docs/direction.md), and which finished branch CI has passed.
// The thinking is done by the issue watcher's own sessions, which already
// refine a ticket into a spec and implement it.
//
// ## Why this is a separate process from the watcher
//
// The watcher's sessions take their prompt from issue text, which anyone with a
// Forgejo account can write. Its container is therefore given no credentials
// beyond a `write:issue` token — see the comment on the `config:` volume in
// docker-compose.yml. Merging to `main` needs `write:repository`, so putting
// that token where those sessions can read it would let an issue body reach a
// token that can push to `main`. This process holds it instead, takes no free
// text as input, and can only do the fixed sequence below.
//
// ## The sequence
//
// A ticket the watcher finished carries `agent:done`, which means **CI passed on
// the pushed branch** — the watcher resolves it from Forgejo's commit status
// after the session is over, so it is not a verdict the session could influence.
// `ship` on the same issue is the standing permission to land it. This then:
// opens a pull request, re-checks every Actions job on the branch head, merges,
// and hands `main` to tools/deploy-main.
//
// Nothing here is a judgement about whether the change is *good*. That is the
// trade docs/direction.md describes: the lane is limited to work whose success
// a machine can see.

import { $ } from "bun";

const API = process.env.FORGEJO_API ?? "https://code.garage44.eu/api/v1";
const REPO = process.env.FORGEJO_REPO ?? "jeroen/cartopolis";
const TOKEN = process.env.FORGEJO_TOKEN;              // write:issue
const MERGE_TOKEN = process.env.FORGEJO_MERGE_TOKEN;  // write:repository
const REPO_ROOT = process.env.AGENT_REPO_ROOT ?? "/home/agent/cartopolis";
const CONFIG_PATH = process.env.CARTO_AUTOPILOT_CONFIG ?? "/home/agent/issue-agent/autopilot.json";
const STATE_PATH = process.env.CARTO_AUTOPILOT_STATE ?? "/home/agent/issue-agent/autopilot-state.json";

const SHIP = "ship";
const DONE = "agent:done";
const SHIPPED = "agent:shipped";
const FAILED = "agent:failed";
const AUTONOMOUS = "autonomous";
const FILED_BY_AUTOPILOT = "autopilot";

// --- config ------------------------------------------------------------------
//
// Read on every tick, never cached: this file IS the control surface, and the
// point of it is that changing it takes effect without restarting anything.
//
//   { "enabled": false,        // file tickets at all
//     "ticketsPerDay": 1,      // how many new ones a day
//     "ship": false }          // merge and deploy finished ones, or stop at review

type Config = { enabled: boolean; ticketsPerDay: number; maxOpen: number; ship: boolean };

const DEFAULTS: Config = { enabled: false, ticketsPerDay: 5, maxOpen: 2, ship: false };

async function config(): Promise<Config> {
  try {
    const raw = JSON.parse(await Bun.file(CONFIG_PATH).text());
    return {
      enabled: Boolean(raw.enabled),
      // A quota of 0 is "file nothing", which is a different and useful state
      // from `enabled: false` — it keeps shipping what is already in flight.
      ticketsPerDay: Number.isFinite(raw.ticketsPerDay) ? Math.max(0, Math.floor(raw.ticketsPerDay)) : DEFAULTS.ticketsPerDay,
      // How many may be open at once, which is a different limit from the daily
      // one: the watcher builds two at a time and they all branch from the same
      // `main`, so filing five at once would be five branches racing a moving
      // base. The day's quota paces the work; this paces the collisions.
      maxOpen: Number.isFinite(raw.maxOpen) ? Math.max(1, Math.floor(raw.maxOpen)) : DEFAULTS.maxOpen,
      ship: Boolean(raw.ship),
    };
  } catch {
    return DEFAULTS;
  }
}

type State = { day: string; filed: number; keys: string[] };

async function loadState(): Promise<State> {
  try {
    const s = JSON.parse(await Bun.file(STATE_PATH).text());
    return { day: s.day ?? "", filed: s.filed ?? 0, keys: s.keys ?? [] };
  } catch {
    return { day: "", filed: 0, keys: [] };
  }
}

async function saveState(s: State) {
  await Bun.write(STATE_PATH, JSON.stringify(s, null, 2));
}

const today = () => new Date().toISOString().slice(0, 10);

// --- forgejo -----------------------------------------------------------------

async function api(path: string, init: RequestInit = {}, token = TOKEN) {
  const res = await fetch(`${API}/repos/${REPO}${path}`, {
    ...init,
    headers: { Authorization: `token ${token}`, "Content-Type": "application/json", ...(init.headers ?? {}) },
  });
  if (!res.ok) throw new Error(`${init.method ?? "GET"} ${path} → ${res.status} ${await res.text()}`);
  return res.status === 204 ? null : await res.json();
}

const log = (...a: unknown[]) => console.log(new Date().toISOString().slice(11, 19), ...a);

const labelsOf = (issue: any): string[] => (issue.labels ?? []).map((l: any) => l.name);

/** Every Actions job that ran for this commit, as [status, name]. */
async function jobsFor(sha: string): Promise<[string, string][]> {
  const d = await api(`/actions/tasks?limit=50`, {}, MERGE_TOKEN ?? TOKEN);
  return (d.workflow_runs ?? [])
    .filter((r: any) => r.head_sha === sha)
    .map((r: any) => [r.status ?? "", r.name ?? ""] as [string, string]);
}

// --- shipping ----------------------------------------------------------------

async function shipReady(cfg: Config) {
  // One switch for the whole half. The `ship` label is the per-ticket opt-in;
  // this is the standing one, and turning it off leaves finished branches
  // sitting on `agent:done` for a person to land, which is where this lane
  // stopped before the autopilot existed.
  if (!cfg.ship) return;
  const issues = await api(`/issues?state=open&type=issues&labels=${encodeURIComponent(`${DONE},${SHIP}`)}&limit=20`);
  if (!issues.length) return;

  for (const issue of issues) {
    const n = issue.number;
    const names = labelsOf(issue);
    if (!names.includes(DONE) || !names.includes(SHIP)) continue; // Forgejo's label filter is an OR

    // The refined description carries the branch the session was told to use.
    // Falling back to the watcher's default keeps an unrefined ticket shippable.
    const branch = /^\s*Branch:\s*`?([A-Za-z0-9._\/-]+)`?\s*$/im.exec(issue.body ?? "")?.[1] ?? `issue-${n}`;

    try {
      const head = await api(`/branches/${encodeURIComponent(branch)}`);
      const sha = head.commit.id;

      const jobs = await jobsFor(sha);
      if (!jobs.length) {
        log(`#${n}: no Actions job ran on ${branch} (${sha.slice(0, 8)}) — not shipping`);
        continue;
      }
      const bad = jobs.filter(([st]) => st !== "success");
      if (bad.length) {
        // `agent:done` is also how the watcher records "CI never answered", so
        // this is not redundant with it. That state is fine for handing a branch
        // to a person and is not fine for publishing.
        log(`#${n}: ${bad.map(([st, name]) => `${name}=${st}`).join(", ")} — not shipping`);
        continue;
      }

      const pr = await openOrFindPr(issue, branch);
      log(`#${n}: merging pull request ${pr.number} (${branch} → main)`);
      await mergePr(pr.number);

      await api(`/issues/${n}/labels/${await labelId(SHIP)}`, { method: "DELETE" });
      await addLabel(n, SHIPPED);
      await comment(n, `🤖 **Merged into \`main\`** as pull request #${pr.number}, on a green run of every Actions job for \`${sha.slice(0, 8)}\`.`);

      await deployMain(n);
    } catch (e) {
      log(`#${n}: ship failed —`, e instanceof Error ? e.message : e);
      await comment(n, `🤖 **Could not ship this.** ${e instanceof Error ? e.message : e}\n\nThe branch is intact; nothing was merged.`).catch(() => {});
      await addLabel(n, FAILED).catch(() => {});
    }
  }
}

async function openOrFindPr(issue: any, branch: string) {
  const open = await api(`/pulls?state=open&limit=50`, {}, MERGE_TOKEN);
  const existing = open.find((p: any) => p.head?.ref === branch);
  if (existing) return existing;
  return await api(
    `/pulls`,
    {
      method: "POST",
      body: JSON.stringify({
        head: branch,
        base: "main",
        title: issue.title,
        // Closes the issue on merge, which is also what moves its card to Done:
        // the watcher projects a closed issue onto the board, Forgejo has no
        // kanban API of its own.
        body: `Closes #${issue.number}\n\nMerged by the autopilot (docs/direction.md) after every Actions job passed on the branch head.`,
      }),
    },
    MERGE_TOKEN,
  );
}

async function mergePr(index: number) {
  // A 405 here is Forgejo still computing mergeability, not a permission
  // problem — it answers "Please try again later" in the body. Poll it.
  for (let attempt = 0; attempt < 20; attempt++) {
    const res = await fetch(`${API}/repos/${REPO}/pulls/${index}/merge`, {
      method: "POST",
      headers: { Authorization: `token ${MERGE_TOKEN}`, "Content-Type": "application/json" },
      body: JSON.stringify({ Do: "merge", delete_branch_after_merge: false }),
    });
    if (res.ok) return;
    const body = await res.text();
    if (res.status !== 405) throw new Error(`merge → ${res.status} ${body}`);
    await Bun.sleep(15_000);
  }
  throw new Error("Forgejo never reported the pull request as mergeable");
}

/** Hand `main` to the deploy script; it does its own gating and ordering. */
async function deployMain(issueNumber: number) {
  const res = await $`${REPO_ROOT}/tools/deploy-main`.cwd(REPO_ROOT).nothrow().quiet();
  const out = `${res.stdout.toString()}${res.stderr.toString()}`.trim().slice(-3000);
  if (res.exitCode === 0) {
    log(`#${issueNumber}: deployed`);
    await comment(issueNumber, `🤖 **Deployed to the public services.**\n\n\`\`\`\n${out}\n\`\`\``);
  } else {
    log(`#${issueNumber}: deploy refused or failed`);
    await comment(
      issueNumber,
      `🤖 **Merged, but not deployed.** The deploy step exited ${res.exitCode}:\n\n\`\`\`\n${out}\n\`\`\`\n\n` +
        `\`main\` has the change; the public services are still on the previous build.`,
    );
  }
}


/**
 * The newest commit on `main` that Actions ran for, and how those jobs ended.
 *
 * Not simply `main`'s head: the CI workflow ignores `docs/**` and `*.md`, so a
 * documentation merge leaves a head with no jobs at all, which is
 * indistinguishable from a commit whose run never started. Walking back to the
 * newest commit that *was* built is what "is main broken?" actually means.
 */
async function mainVerdict(): Promise<{ sha: string; failed: [string, string][] } | null> {
  const d = await api(`/actions/tasks?limit=50`, {}, MERGE_TOKEN ?? TOKEN);
  // Only the CI workflow. A deploy job that failed says something about the
  // world outside the repository — a runner without a network, a service that
  // would not restart — and blocking every ticket on it would be reading "the
  // publish did not work" as "the code is broken".
  const runs = (d.workflow_runs ?? []).filter((r: any) => r.head_branch === "main" && r.workflow_id === "ci.yml");
  if (!runs.length) return null;
  const sha = runs[0].head_sha;
  const jobs = runs.filter((r: any) => r.head_sha === sha);

  // Group by job name and let any successful run clear it: a job re-dispatched
  // after a runner timeout is the answer, and the failed attempt beside it is
  // history. Without this a re-run can never turn `main` green again.
  const byName = new Map<string, string[]>();
  for (const j of jobs) byName.set(j.name, [...(byName.get(j.name) ?? []), j.status]);

  const failed: [string, string][] = [];
  for (const [name, states] of byName) {
    if (states.includes("success")) continue;
    if (states.some((st) => ["waiting", "running", "blocked"].includes(st))) return null; // still deciding
    failed.push([states[0] ?? "", name]);
  }
  return { sha, failed };
}

/**
 * A broken `main` outranks the frontier. Nothing can be shipped from it — both
 * this and tools/deploy-main refuse — so filing anything else first is filing
 * work that cannot land.
 */
async function fileRedMainTicket(): Promise<"filed" | "known" | "green"> {
  const verdict = await mainVerdict();
  if (!verdict || !verdict.failed.length) return "green";

  const key = `main-red-${verdict.sha.slice(0, 8)}`;
  if (await alreadyFiled(key)) {
    // Asked already. Still a stop: nothing filed now could land on this `main`.
    log(`main is red on ${verdict.sha.slice(0, 8)} and a ticket for it exists — waiting`);
    return "known";
  }

  const jobs = verdict.failed.map(([st, name]) => `- \`${name}\` — ${st}`).join("\n");
  const issue = requireIssue(await api(`/issues`, {
    method: "POST",
    body: JSON.stringify({
      title: `main is red on ${verdict.sha.slice(0, 8)}`,
      labels: await labelIds([AUTONOMOUS, FILED_BY_AUTOPILOT, ...(( await config()).ship ? [SHIP] : [])]),
      body: [
        `The last commit on \`main\` that CI ran for is \`${verdict.sha.slice(0, 8)}\`, and it did not pass:`,
        "",
        jobs,
        "",
        "Nothing can be deployed while this stands — `tools/deploy-main` gates on it — so this",
        "comes before anything on the frontier.",
        "",
        "Read the failing job's log, reproduce it locally, and fix the cause. If the failure is",
        "the runner rather than the code (a flake, a cache miss, a missing tool), say so on this",
        "ticket and close it rather than changing code to suit it.",
        "",
        `<sub>Filed by the autopilot. <!-- autopilot:${key} --></sub>`,
      ].join("\n"),
    }),
  }));
  log(`filed #${issue.number}: main is red on ${verdict.sha.slice(0, 8)}`);
  return "filed";
}

// --- filing the next ticket --------------------------------------------------
//
// The frontier lives in docs/direction.md as a table, read from `main` over the
// API rather than from a checkout — this process holds the powerful token and
// has no business running git in a worktree other sessions are writing to.

async function frontier(): Promise<{ key: string; source: string; adds: string }[]> {
  const res = await fetch(`${API}/repos/${REPO}/raw/docs/direction.md?ref=main`, {
    headers: { Authorization: `token ${TOKEN}` },
  });
  if (!res.ok) throw new Error(`cannot read docs/direction.md → ${res.status}`);
  const text = await res.text();
  const rows: { key: string; source: string; adds: string }[] = [];
  for (const line of text.split("\n")) {
    const m = /^\|\s*([^|]+?)\s*\|\s*([^|]+?)\s*\|\s*$/.exec(line);
    if (!m) continue;
    const [, source, adds] = m;
    if (/^-+$/.test(source) || /^Source$/i.test(source)) continue;
    rows.push({ key: source.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, ""), source, adds });
  }
  return rows;
}

// The marker is in the body, so a ticket that has been refined — which rewrites
// the description around it — still matches. `state=all` because a closed one
// counts: this list is "what has been tried", not "what is open".
//
// Scanned locally rather than with the API's `q=`, which is a fuzzy match over
// title and body: searching it for `autopilot:pdok-aerial-imagery` returns
// unrelated issues, and a false positive here means the frontier looks fully
// filed and nothing is ever proposed again.
let issueBodies: string[] | null = null;

async function alreadyFiled(key: string): Promise<boolean> {
  if (!issueBodies) {
    const all = await api(`/issues?state=all&type=issues&limit=100`);
    issueBodies = all.map((i: any) => `${i.body ?? ""}`);
  }
  return issueBodies.some((b) => b.includes(`autopilot:${key}`));
}

async function generate(cfg: Config) {
  if (!cfg.enabled) return log("filing is off (enabled: false)");
  // A red `main` comes before everything, and it bypasses both the daily quota
  // and the one-at-a-time rule on purpose: every branch is cut from `main`, so
  // while it is broken each new ticket inherits the failure and stalls at its
  // own CI. This is not more work, it is the thing that lets work land.
  const red = await fileRedMainTicket();
  if (red !== "green") return;

  const state = await loadState();
  if (state.day !== today()) {
    state.day = today();
    state.filed = 0;
  }
  if (state.filed >= cfg.ticketsPerDay) return log(`quota reached: ${state.filed}/${cfg.ticketsPerDay} filed today`);

  // One at a time. Two tickets on the board at once is two branches being built
  // against a `main` that the other is about to move, and the watcher's own
  // concurrency limit exists for the same reason.
  //
  // Filtered here rather than in the query: Forgejo answers `labels=<name it
  // does not know>` by returning **every** open issue instead of none, so
  // before this label exists the API's own filter reports the board as full and
  // nothing is ever filed. `shipReady` re-checks its labels for the same reason.
  const open = await api(`/issues?state=open&type=issues&limit=50`);
  const inFlight = open.filter((i: any) => labelsOf(i).includes(FILED_BY_AUTOPILOT));
  if (inFlight.length >= cfg.maxOpen) {
    return log(`${inFlight.length} autopilot ticket(s) open (limit ${cfg.maxOpen}) — waiting for #${inFlight[0].number}`);
  }

  let rows: { key: string; source: string; adds: string }[];
  try {
    rows = await frontier();
  } catch (e) {
    // Until docs/direction.md is on `main` there is no frontier to pull from,
    // and that is the correct state to sit in rather than guess at work.
    return log(`no frontier to read — ${e instanceof Error ? e.message : e}`);
  }

  for (const row of rows) {
    if (state.keys.includes(row.key)) continue;
    if (await alreadyFiled(row.key)) {
      state.keys.push(row.key);
      continue;
    }

    const body = [
      `From the frontier in [\`docs/direction.md\`](../src/branch/main/docs/direction.md): **${row.source}** — ${row.adds}.`,
      "",
      "The first half of this ticket is deciding whether it is worth doing at all:",
      "",
      "- Is there an interface a client or an extractor can use, and what does it cost per cell?",
      "- Does the licence allow redistributing what we would cache?",
      "- Does it change the picture, or only the data behind it?",
      "",
      "**Not worth it is a valid answer.** Record it in `docs/direction.md` against this row",
      "with the reason, and close this ticket — that is the result, not a failure.",
      "",
      "If it is worth doing, follow the shape the surveyed layers already use:",
      "an extractor under `tools/`, a coverage manifest, a streamer that adds to what",
      "is drawn rather than replacing it, and no unbounded per-frame upload.",
      "",
      `<sub>Filed by the autopilot. <!-- autopilot:${row.key} --></sub>`,
    ].join("\n");

    const issue = await requireIssue(await api(`/issues`, {
      method: "POST",
      body: JSON.stringify({
        title: `${row.source}: is it worth wiring in?`,
        body,
        labels: await labelIds([AUTONOMOUS, FILED_BY_AUTOPILOT, ...(cfg.ship ? [SHIP] : [])]),
      }),
    }));
    log(`filed #${issue.number}: ${row.source}`);
    state.filed++;
    state.keys.push(row.key);
    await saveState(state);
    return;
  }

  // The frontier ran out. That is not "there is no work" — it is the queue
  // needing to be refilled, which is itself a ticket: read what the registries
  // publish, check what this world already draws, and propose rows.
  //
  // Keyed on how many rows the table has, so the same ask can be made again
  // after the table grows, and cannot be made twice against the same one.
  const refill = `frontier-refill-${rows.length}`;
  if (!(await alreadyFiled(refill))) {
    const issue = requireIssue(
      await api(`/issues`, {
        method: "POST",
        body: JSON.stringify({
          title: "The frontier is empty — find the next things worth importing",
          labels: await labelIds([AUTONOMOUS, FILED_BY_AUTOPILOT, ...(cfg.ship ? [SHIP] : [])]),
          body: [
            "Every candidate in the frontier table of [`docs/direction.md`](../src/branch/main/docs/direction.md)",
            "has been filed. This ticket refills it.",
            "",
            "The goal is at the top of that file, and it is the filter: a three-dimensional,",
            "augmented-reality twin of the real world, the Netherlands in the most detail,",
            "built from data somebody already publishes. So look for **sources**, not features:",
            "",
            "- What do PDOK, the Kadaster, CBS, Rijkswaterstaat, the provinces, the",
            "  municipalities and the water boards publish that this world does not draw yet?",
            "- What does OpenStreetMap carry that the client ignores — tags already in the",
            "  vector tiles it streams, layers it never reads?",
            "- What is registered but invisible: things with a position and no representation.",
            "",
            "For each one worth proposing, add a row to the candidates table with the source and",
            "one line on what it would add. Rules for what goes in:",
            "",
            "- **A source, not an idea.** Something that publishes data, with an interface.",
            "- **Not the maintainer's job.** Nothing whose success is a matter of taste — no",
            "  colour, lighting or feel work, and nothing that needs a camera or a headset.",
            "- **Say what it costs** if you can measure it: bytes per cell, or requests per view.",
            "- Do not touch the goal, the rules, or the list of what is already wired.",
            "",
            "Six to ten rows is a good outcome. Fewer, well-checked ones beat a long list of",
            "sources nobody confirmed exists — and if a candidate turns out not to be real, that",
            "is worth saying in the pull request rather than filing it as a row.",
            "",
            `<sub>Filed by the autopilot. <!-- autopilot:${refill} --></sub>`,
          ].join("\n"),
        }),
      }),
    );
    log(`filed #${issue.number}: refill the frontier (${rows.length} rows exhausted)`);
    state.filed++;
    await saveState(state);
    return;
  }

  await saveState(state);
  log("nothing left on the frontier, and the refill ticket is already open");
}


/**
 * A created issue, or a hard error.
 *
 * Worth the four lines: `FORGEJO_REPO` held the repository's **old** name, and
 * Forgejo answers an API call on a renamed repository with a redirect — which
 * turns a POST into a GET, so creating an issue returned the issue *list* with
 * status 200 and no `number` in it. The autopilot then recorded a ticket it had
 * not filed and moved on to the next frontier row. Anything that must create
 * something checks that it did.
 */
function requireIssue(res: any): any {
  if (!res || typeof res.number !== "number") {
    throw new Error(`issue was not created (got ${JSON.stringify(res).slice(0, 200)}) — is FORGEJO_REPO the current name?`);
  }
  return res;
}

// --- label plumbing ----------------------------------------------------------

let labelCache: Record<string, number> | null = null;

async function labels(): Promise<Record<string, number>> {
  if (labelCache) return labelCache;
  const list = await api(`/labels?limit=100`);
  labelCache = Object.fromEntries(list.map((l: any) => [l.name, l.id]));
  return labelCache!;
}

async function labelId(name: string): Promise<number> {
  const all = await labels();
  if (all[name] != null) return all[name];
  const made = await api(`/labels`, { method: "POST", body: JSON.stringify({ name, color: "#7c5cff", description: "autopilot" }) });
  labelCache![name] = made.id;
  return made.id;
}

const labelIds = async (names: string[]) => await Promise.all(names.map(labelId));

async function addLabel(n: number, name: string) {
  await api(`/issues/${n}/labels`, { method: "POST", body: JSON.stringify({ labels: [await labelId(name)] }) });
}

async function comment(n: number, body: string) {
  await api(`/issues/${n}/comments`, { method: "POST", body: JSON.stringify({ body }) });
}

// --- main --------------------------------------------------------------------

const cmd = process.argv[2] ?? "tick";
const cfg = await config();

if (!TOKEN) throw new Error("FORGEJO_TOKEN is not set");
if (cmd !== "status" && !MERGE_TOKEN) throw new Error("FORGEJO_MERGE_TOKEN is not set");

if (cmd === "status") {
  const state = await loadState();
  console.log(JSON.stringify({ config: cfg, configPath: CONFIG_PATH, state, repo: REPO }, null, 2));
} else if (cmd === "ship") {
  await shipReady(cfg);
} else if (cmd === "generate") {
  await generate(cfg);
} else if (cmd === "tick") {
  await shipReady(cfg);
  await generate(cfg);
} else {
  console.error(`unknown command: ${cmd}`);
  process.exit(2);
}

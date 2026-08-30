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
// deploy-main lives beside this script — in the repo both are in tools/, and
// the autopilot container mounts that directory whole at /opt/autopilot with no
// checkout anywhere (deploy-main deliberately works from the API alone). The
// old default of `$AGENT_REPO_ROOT/tools/deploy-main` pointed at a path that
// does not exist in the container, so no unattended deploy ever ran.
const DEPLOY_MAIN = process.env.AGENT_DEPLOY_MAIN ?? `${import.meta.dir}/deploy-main`;
const CONFIG_PATH = process.env.CARTO_AUTOPILOT_CONFIG ?? "/home/agent/issue-agent/autopilot.json";
const STATE_PATH = process.env.CARTO_AUTOPILOT_STATE ?? "/home/agent/issue-agent/autopilot-state.json";
/** Written hourly by the host, because this container has no claude login of its own. */
const EXPIRY_PATH = process.env.CARTO_CLAUDE_EXPIRY ?? "/home/agent/issue-agent/claude-expiry.json";

const SHIP = "ship";
/** How long `agent:running` may sit untouched before it is treated as dead. */
const STALE_RUN_MS = Number(process.env.CARTO_AUTOPILOT_STALE_HOURS ?? 3) * 3_600_000;
/** Parked tickets tolerated on top of the open limit before filing stops. */
const MAX_PARKED = 4;
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
//     "ticketsPerDay": 1,      // how many new feature tickets a day
//     "qaPerDay": 0,           // how many QA passes a day (0 = the lane is off)
//     "qaMaxOpen": 3,          // how many QA tickets may be open at once
//     "ship": false }          // merge and deploy finished ones, or stop at review

type Config = { enabled: boolean; ticketsPerDay: number; maxOpen: number; ship: boolean; qaPerDay: number; qaMaxOpen: number };

const DEFAULTS: Config = { enabled: false, ticketsPerDay: 5, maxOpen: 2, ship: false, qaPerDay: 0, qaMaxOpen: 3 };

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
      qaPerDay: Number.isFinite(raw.qaPerDay) ? Math.max(0, Math.floor(raw.qaPerDay)) : DEFAULTS.qaPerDay,
      qaMaxOpen: Number.isFinite(raw.qaMaxOpen) ? Math.max(1, Math.floor(raw.qaMaxOpen)) : DEFAULTS.qaMaxOpen,
    };
  } catch {
    return DEFAULTS;
  }
}

type State = { day: string; filed: number; keys: string[]; digestDay?: string; qaFiled: number; qaCursor: number };

async function loadState(): Promise<State> {
  try {
    const s = JSON.parse(await Bun.file(STATE_PATH).text());
    return { day: s.day ?? "", filed: s.filed ?? 0, keys: s.keys ?? [], digestDay: s.digestDay, qaFiled: s.qaFiled ?? 0, qaCursor: s.qaCursor ?? 0 };
  } catch {
    return { day: "", filed: 0, keys: [], qaFiled: 0, qaCursor: 0 };
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



const NEEDS_INPUT = "agent:needs-input";

/**
 * A ticket of ours that stopped to ask is a ticket that stops for ever: nobody
 * is reading the board, which is the whole premise. So answer it the only way
 * this lane can — point at the values, and re-arm it.
 *
 * The bare trigger label means "implement this now" and skips refinement, which
 * is right here: the spec was written, the session simply would not choose. It
 * is done **once** per ticket; a second stop is a real question and is left
 * alone, because a loop that keeps re-arming the same ticket is a loop that
 * burns a session every hour on a decision it cannot make.
 */
async function rearmParkedTickets() {
  const open = await api(`/issues?state=open&type=issues&limit=50`);
  for (const issue of open) {
    const names = labelsOf(issue);
    if (!names.includes(FILED_BY_AUTOPILOT) || !names.includes(NEEDS_INPUT)) continue;

    const comments = await api(`/issues/${issue.number}/comments?limit=50`);
    if (comments.some((c: any) => (c.body ?? "").includes("<!-- autopilot:rearmed -->"))) continue;

    await comment(
      issue.number,
      "🤖 **Nobody is going to answer this, so decide it yourself.**\n\n" +
        "The seven values at the top of [`docs/direction.md`](../src/branch/main/docs/direction.md) " +
        "are there to settle exactly this: measured over plausible, official sources first, degrade " +
        "rather than break, add rather than replace, keep the frame budget, ship the smallest thing a " +
        "machine can judge, and write down what you decided.\n\n" +
        "Pick the reading those support, record which one you applied in the commit body, and build it. " +
        "Stop again only for something no one can derive from the repository — a credential, a licence " +
        "somebody must accept, or a question about what the project is for.\n\n" +
        "<sub><!-- autopilot:rearmed --></sub>",
    );
    await api(`/issues/${issue.number}/labels/${await labelId(NEEDS_INPUT)}`, { method: "DELETE" });
    await addLabel(issue.number, "agent");
    log(`#${issue.number}: parked on a question — answered from the values and re-armed`);
  }
}

// --- what may not be shipped unattended ---------------------------------------
//
// `docs/direction.md` states these as rules; this is the part that enforces
// them, because a rule an agent reads is a rule an agent can decide it has a
// good reason to break. Each one is a change no revert of ours can undo:
//
// - **the wire protocol** — the frame header carries a version and
//   `decode_app_frame` refuses a mismatch outright, so a bump locks out every
//   installed Android app and downloaded desktop binary, and no deploy from
//   this machine reaches either.
// - **a database migration** — it runs once against the live world.
// - **the workflows** — a branch that edits CI can weaken the very check that
//   is about to judge it.
// - **the autopilot's own scripts** — same shape, one level up.
// - **`assetlinks.json`** — the Android App Link is bound to a signing
//   certificate at *install* time, so getting it wrong is not fixable by
//   publishing a correction.
//
// A ticket that touches one of these keeps its branch and loses its `ship`
// label: it becomes an ordinary finished branch waiting for a person.
const FENCED: [RegExp, string][] = [
  [/^crates\/core\/src\/protocol\.rs$/, "the wire protocol"],
  [/^crates\/simulator\/migrations\//, "a database migration"],
  [/^\.forgejo\/workflows\//, "the CI or deploy workflows"],
  [/^tools\/(autopilot\.ts|deploy-main)$/, "the autopilot's own scripts"],
  [/assetlinks\.json$/, "the Android App Link certificate"],
];

async function fencedPaths(branch: string): Promise<string[]> {
  const cmp = await api(`/compare/main...${encodeURIComponent(branch)}`);
  const files: string[] = Array.from(new Set((cmp.files ?? []).map((f: any) => f.filename as string)));
  const hits: string[] = [];
  for (const f of files) {
    const rule = FENCED.find(([re]) => re.test(f));
    if (rule) hits.push(`\`${f}\` — ${rule[1]}`);
  }
  return hits;
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

    // Whether mergePr got through — a failure after that point must not tell
    // the issue "nothing was merged" (issue #187 was told exactly that, two
    // lines under the comment announcing its own merge).
    let merged = false;
    try {
      const head = await api(`/branches/${encodeURIComponent(branch)}`);
      const sha = head.commit.id;

      const fenced = await fencedPaths(branch);
      if (fenced.length) {
        await api(`/issues/${n}/labels/${await labelId(SHIP)}`, { method: "DELETE" });
        await comment(
          n,
          `🤖 **Not shipping this one — it touches something the unattended lane may not change:**\n\n` +
            fenced.map((f) => `- ${f}`).join("\n") +
            `\n\nThe branch \`${branch}\` is finished and CI passed on it; it is waiting for you to land it. ` +
            `The reasoning is in \`docs/direction.md\` — these are the changes no revert from here undoes.`,
        );
        log(`#${n}: fenced (${fenced.length} path(s)) — left for review`);
        continue;
      }

      // The pull request comes **before** the check, not after it, and that is
      // the whole reason this lane's verdicts were empty: `ci.yml` runs on
      // `main` and on pull requests and deliberately not on a branch push, so
      // until the request exists there is nothing for CI to have said. The
      // watcher's `agent:done` inherits that — it means "CI did not answer" for
      // every ticket this lane files, which is exactly why the real gate is
      // here rather than on the label.
      const pr = await openOrFindPr(issue, branch);

      const jobs = await jobsFor(sha);
      if (!jobs.length) {
        // Either the run has not registered yet — this ticks hourly, so simply
        // look again — or the change is one CI is configured to skip. The
        // workflow ignores `docs/**` and `*.md`, and a documentation-only
        // branch that waits for a verdict waits for ever.
        const files: string[] = Array.from(
          new Set(((await api(`/compare/main...${encodeURIComponent(branch)}`)).files ?? []).map((f: any) => f.filename as string)),
        );
        const skipped = files.length > 0 && files.every((f) => f.startsWith("docs/") || f.endsWith(".md"));
        if (!skipped) {
          log(`#${n}: pull request ${pr.number} open, waiting for CI on ${sha.slice(0, 8)}`);
          continue;
        }
        log(`#${n}: documentation only (${files.length} file(s)) — CI does not run on these, shipping`);
      } else {
        const bad = jobs.filter(([st]) => st !== "success");
        if (bad.length) {
          log(`#${n}: ${bad.map(([st, name]) => `${name}=${st}`).join(", ")} — not shipping`);
          continue;
        }
      }

      log(`#${n}: merging pull request ${pr.number} (${branch} → main)`);
      await mergePr(pr.number);
      merged = true;

      await api(`/issues/${n}/labels/${await labelId(SHIP)}`, { method: "DELETE" });
      await addLabel(n, SHIPPED);
      await comment(n, `🤖 **Merged into \`main\`** as pull request #${pr.number} (\`${sha.slice(0, 8)}\`).`);

      await deployMain(n);
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      log(`#${n}: ship failed —`, msg);
      if (e instanceof MergeConflict) {
        // Retrying cannot resolve this — every autopilot ticket edits
        // docs/direction.md, so two in flight collide with whichever lands
        // first. Park it the way a fenced ticket is parked (drop `ship`, say
        // why once) instead of failing and commenting every five minutes.
        await api(`/issues/${n}/labels/${await labelId(SHIP)}`, { method: "DELETE" }).catch(() => {});
        await comment(
          n,
          `🤖 **Not shipping this one — the branch conflicts with \`main\`** (another ticket landed first).\n\n` +
            `Rebase \`${branch}\` onto \`main\`, resolve, push, and put the \`ship\` label back to land it.`,
        ).catch(() => {});
      } else {
        await comment(
          n,
          merged
            ? `🤖 **Merged, but the step after the merge failed.** ${msg}\n\n\`main\` has the change.`
            : `🤖 **Could not ship this.** ${msg}\n\nThe branch is intact; nothing was merged.`,
        ).catch(() => {});
        await addLabel(n, FAILED).catch(() => {});
      }
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

/** The branch genuinely conflicts with `main`; no amount of retrying merges it. */
class MergeConflict extends Error {}

async function mergePr(index: number) {
  // The merge endpoint answers 405 both while Forgejo is still computing
  // mergeability and for a genuine conflict, and the two are indistinguishable
  // there. The pull request itself is the tell: `mergeable` flips true within
  // seconds when computation was the only obstacle, and stays false for ever
  // on a conflict — so poll that, merge when it says yes, and after five
  // minutes of "no" call it what it is.
  for (let attempt = 0; attempt < 20; attempt++) {
    const pr = await api(`/pulls/${index}`, {}, MERGE_TOKEN);
    if (pr.mergeable) {
      const res = await fetch(`${API}/repos/${REPO}/pulls/${index}/merge`, {
        method: "POST",
        headers: { Authorization: `token ${MERGE_TOKEN}`, "Content-Type": "application/json" },
        body: JSON.stringify({ Do: "merge", delete_branch_after_merge: false }),
      });
      if (res.ok) return;
      const body = await res.text();
      // A 405 despite `mergeable: true` means main moved between the two
      // calls; anything else is a real refusal.
      if (res.status !== 405) throw new Error(`merge → ${res.status} ${body}`);
    }
    await Bun.sleep(15_000);
  }
  throw new MergeConflict(`pull request ${index} never became mergeable — its branch conflicts with \`main\``);
}

/** Hand `main` to the deploy script; it does its own gating and ordering. */
async function deployMain(issueNumber: number) {
  let res;
  try {
    res = await $`${DEPLOY_MAIN}`.nothrow().quiet();
  } catch (e) {
    // .nothrow() covers a non-zero exit, not a failure to spawn at all.
    log(`#${issueNumber}: deploy script did not run —`, e instanceof Error ? e.message : e);
    await comment(
      issueNumber,
      `🤖 **Merged, but not deployed** — the deploy script could not be run: ${e instanceof Error ? e.message : e}\n\n` +
        `\`main\` has the change; the public services are still on the previous build.`,
    ).catch(() => {});
    return;
  }
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
  // Only tickets that are still *moving* hold a slot. One that failed, or that
  // stopped to ask a question, is waiting on a person — and with a limit of two
  // that would mean two dead tickets stop the lane for good, silently, which is
  // the most likely way this whole thing quietly ends.
  //
  // A session that died mid-run leaves `agent:running` behind on purpose (the
  // watcher will not silently retry an unknown half-finished state), so that
  // label ages out here rather than counting for ever.
  const open = await api(`/issues?state=open&type=issues&limit=50`);
  // QA tickets pace themselves in generateQa and hold no feature slot here —
  // nineteen a day counted against maxOpen would stop this lane outright.
  const mine = open.filter((i: any) => labelsOf(i).includes(FILED_BY_AUTOPILOT) && !labelsOf(i).includes(QA));
  const stalled = (i: any) => {
    const names = labelsOf(i);
    if (names.includes(FAILED) || names.includes(NEEDS_INPUT)) return true;
    // Finished, and either about to be merged in this same tick or waiting for
    // a person because it was fenced. Either way it is not work in progress.
    if (names.includes(DONE)) return true;
    const age = Date.now() - new Date(i.updated_at).getTime();
    return names.includes("agent:running") && age > STALE_RUN_MS;
  };
  const inFlight = mine.filter((i: any) => !stalled(i));
  const parked = mine.filter(stalled);
  if (parked.length) log(`${parked.length} autopilot ticket(s) parked (${parked.map((i: any) => `#${i.number}`).join(", ")}) — not holding a slot`);
  if (inFlight.length >= cfg.maxOpen) {
    return log(`${inFlight.length} autopilot ticket(s) open (limit ${cfg.maxOpen}) — waiting for #${inFlight[0].number}`);
  }
  // …but they do bound the sprawl. A lane filing five a day into a board nobody
  // reads is worse than a lane that stops.
  if (mine.length >= cfg.maxOpen + MAX_PARKED) {
    return log(`${mine.length} autopilot ticket(s) open including ${parked.length} parked — filing nothing until some are dealt with`);
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
"",
      "**Decide it yourself.** This ticket is not being watched, so a question asked",
      "here is a ticket that stops. The seven values at the top of `docs/direction.md`",
      "exist to settle exactly this kind of ambiguity — pick the reading they support,",
      "say in the commit body which one you applied and why, and build. Only a decision",
      "that would need something nobody can derive from the repository — a credential, a",
      "licence somebody must agree to, a choice about what the project is *for* — is a",
      "reason to stop.",
      "",      "If it is worth doing, follow the shape the surveyed layers already use:",
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
            "**Decide it yourself** — see the values at the top of that file. A candidate you",
            "are unsure about is one to leave out, not one to ask about.",
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

// --- the QA lane -------------------------------------------------------------
//
// The build lane proves a change does what its ticket says; nothing walks the
// product the way a player does and asks what is MISSING. That class of gap is
// invisible to CI, because no test fails on a capability nobody wrote — the
// standing example: registration worked, claiming a house worked, and a
// registered account still could not claim a house. A QA ticket hands one
// functionality per session to the watcher with instructions to exercise it
// headlessly and hunt for expectation gaps; what comes back is filed `qa-gap`
// issues and a report, never code. Targets come from two queues: everything
// recently shipped is walked once as a user would meet it, then the standing
// list in docs/qa/targets.md rotates. QA tickets carry the bare `agent` label,
// not `autonomous` — refinement would rewrite these instructions into a build
// spec, which is exactly what a QA pass is not.

const QA = "qa";

async function qaInventory(): Promise<{ key: string; name: string; how: string }[]> {
  const res = await fetch(`${API}/repos/${REPO}/raw/docs/qa/targets.md?ref=main`, {
    headers: { Authorization: `token ${TOKEN}` },
  });
  if (!res.ok) return [];
  const rows: { key: string; name: string; how: string }[] = [];
  for (const line of (await res.text()).split("\n")) {
    const m = /^\|\s*([^|]+?)\s*\|\s*([^|]+?)\s*\|\s*$/.exec(line);
    if (!m) continue;
    const [, name, how] = m;
    if (/^-+$/.test(name) || /^Functionality$/i.test(name)) continue;
    rows.push({ key: name.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, ""), name, how });
  }
  return rows;
}

async function shippedRecently(): Promise<any[]> {
  const closed = await api(`/issues?state=closed&type=issues&labels=${encodeURIComponent(SHIPPED)}&limit=30`);
  const cutoff = Date.now() - 7 * 86_400_000;
  return closed.filter(
    (i: any) => labelsOf(i).includes(SHIPPED) && !labelsOf(i).includes(QA) && new Date(i.closed_at ?? i.updated_at).getTime() > cutoff,
  );
}

async function fileQaTicket(state: State, key: string, name: string, intro: string) {
  const body = [
    intro,
    "",
    "**This is a QA pass, not a build.** Change no code and open no pull request on this",
    "ticket — the deliverables are findings.",
    "",
    "1. **Exercise it for real.** Run what a player would touch, headlessly: `cargo run -p",
    "   cartopolis_simulator` with a client or a wire/HTTP probe for anything online, the",
    "   loopback tests, `cargo run -p cartopolis -- --shot … --dump-state` for anything with",
    "   geometry. CLAUDE.md lists every harness. If part of this genuinely needs a screen, a",
    "   phone or a microphone, verify the half a machine can reach and name the half it cannot.",
    "2. **Walk the user's journey, not the happy path.** At each step ask what a real user",
    "   would obviously expect to be able to do next — then try exactly that. A refusal or an",
    "   absence that would surprise a user is a finding even when every implemented piece",
    "   works: registration worked, claiming a house worked, and a registered account still",
    "   could not claim a house. No test failed; a user hit a wall.",
    "3. **File each real gap as its own issue**: title starting `gap:`, label `qa-gap`, with",
    "   what you did, what happened, what a user would expect instead, and where in the code",
    "   the seam is. Use the Forgejo API with the `FORGEJO_TOKEN` in your environment. Do not",
    "   label them `autonomous` — a person decides which gaps become work.",
    "4. **Report and close.** Your final message: what you exercised, what held, what you",
    "   filed. Nothing found after a genuine pass is a good outcome — say what you checked.",
    "   Then close this ticket through the API; a QA pass has no branch to merge.",
    "",
    "**Decide it yourself.** Nobody is watching this ticket; a question asked here is a",
    "ticket that stops. Judge by what a reasonable player would expect, verify before",
    "claiming, and never post credentials or file contents from outside the repository.",
    "",
    `<sub>Filed by the autopilot. <!-- autopilot:${key} --></sub>`,
  ].join("\n");

  const issue = requireIssue(
    await api(`/issues`, {
      method: "POST",
      body: JSON.stringify({
        title: `QA: ${name}`,
        body,
        labels: await labelIds(["agent", FILED_BY_AUTOPILOT, QA]),
      }),
    }),
  );
  log(`filed #${issue.number}: QA — ${name}`);
  state.qaFiled++;
  await saveState(state);
}

async function generateQa(cfg: Config) {
  if (!cfg.enabled || !cfg.qaPerDay) return;

  const state = await loadState();
  if (state.day !== today()) {
    state.day = today();
    state.filed = 0;
    state.qaFiled = 0;
  }
  if (state.qaFiled >= cfg.qaPerDay) return log(`qa quota reached: ${state.qaFiled}/${cfg.qaPerDay} today`);

  // Its own open-ticket gate, separate from the feature lane's: QA sessions
  // change nothing and race nothing, so they only need pacing against the
  // watcher's concurrency, not against a moving `main`.
  const open = await api(`/issues?state=open&type=issues&limit=50`);
  const openQa = open.filter((i: any) => labelsOf(i).includes(QA) && labelsOf(i).includes(FILED_BY_AUTOPILOT));
  if (openQa.length >= cfg.qaMaxOpen) {
    return log(`${openQa.length} QA ticket(s) open (limit ${cfg.qaMaxOpen}) — waiting`);
  }

  // Freshly shipped functionality first — walked once, while nobody has used it
  // yet, which is when a missing obvious next step costs the least.
  for (const shipped of await shippedRecently()) {
    const key = `qa-reverify-${shipped.number}`;
    if (await alreadyFiled(key)) continue;
    await fileQaTicket(
      state,
      key,
      `what #${shipped.number} shipped — ${shipped.title}`,
      `Issue #${shipped.number} (“${shipped.title}”) was built and merged by the unattended lane, and nobody has used it. Walk what it delivered as a player would meet it.`,
    );
    return;
  }

  // Then rotate through the standing functionality list, at most one full pass
  // per day (the key is dated, so a target repeats tomorrow, not today).
  const inventory = await qaInventory();
  if (!inventory.length) return log("no QA inventory — docs/qa/targets.md is missing or has no rows");
  for (let i = 0; i < inventory.length; i++) {
    const t = inventory[(state.qaCursor + i) % inventory.length];
    const key = `qa-${t.key}-${today()}`;
    if (await alreadyFiled(key)) continue;
    state.qaCursor = (state.qaCursor + i + 1) % inventory.length;
    await fileQaTicket(state, key, t.name, `From the standing list in \`docs/qa/targets.md\`: **${t.name}** — ${t.how}.`);
    return;
  }
  log("every QA target has been filed today");
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


// --- the daily note -----------------------------------------------------------
//
// Nobody is watching the lane, which means silence has to be readable: a loop
// with nothing to do and a loop that died on Tuesday look identical from the
// outside. So once a day it says what it did, on one long-lived issue.
//
// That issue carries `driven` and NOT the autopilot label — `driven` keeps the
// watcher off it (it would otherwise try to implement the log), and leaving the
// autopilot label off keeps it out of the open-ticket count it would otherwise
// fill by existing.

const LOG_MARKER = "<!-- autopilot:log -->";

async function logIssue(): Promise<any> {
  const all = await api(`/issues?state=all&type=issues&limit=100`);
  const found = all.find((i: any) => `${i.body ?? ""}`.includes(LOG_MARKER));
  if (found) return found;
  return requireIssue(
    await api(`/issues`, {
      method: "POST",
      body: JSON.stringify({
        title: "Autopilot log",
        labels: await labelIds(["driven"]),
        body: [
          "The unattended lane reports here once a day: what it filed, what landed, what",
          "deployed, what is stuck, and how long the login it runs on has left.",
          "",
          "It exists because silence is ambiguous — a loop with nothing to do and a loop",
          "that stopped look the same from outside. **No new comment for a day means",
          "something is wrong**, not that nothing happened.",
          "",
          "Nothing reads this issue; close it and it will be made again. The lane itself is",
          "`docs/direction.md` and the knob is `/data/agent/state/autopilot.json`.",
          "",
          `<sub>${LOG_MARKER}</sub>`,
        ].join("\n"),
      }),
    }),
  );
}

/** Days until the claude.ai login stops refreshing, written by the host timer. */
async function loginDaysLeft(): Promise<number | null> {
  try {
    const d = JSON.parse(await Bun.file(EXPIRY_PATH).text());
    const ms = Number(d.refreshTokenExpiresAt) - Date.now();
    return Math.floor(ms / 86_400_000);
  } catch {
    return null;
  }
}

async function digest(cfg: Config) {
  const state = await loadState();
  if (state.digestDay === today()) return;

  const since = Date.now() - 24 * 3_600_000;
  const recent = (i: any) => new Date(i.updated_at).getTime() >= since;

  const all = await api(`/issues?state=all&type=issues&limit=100`);
  const mine = all.filter((i: any) => labelsOf(i).includes(FILED_BY_AUTOPILOT));
  const filed = mine.filter((i: any) => new Date(i.created_at).getTime() >= since);
  const closed = mine.filter((i: any) => i.state === "closed" && recent(i));
  const openNow = mine.filter((i: any) => i.state === "open");
  // Three states, not two. A finished ticket is waiting on the machine (its
  // pull request, then the merge and the deploy) and needs nobody; a failed or
  // parked one is waiting on a person. Reporting both as "stuck" is how a note
  // meant to make silence readable would instead cry wolf every day.
  const parked = openNow.filter((i: any) => {
    const names = labelsOf(i);
    return names.includes(FAILED) || names.includes(NEEDS_INPUT);
  });
  const finished = openNow.filter((i: any) => labelsOf(i).includes(DONE) && !parked.includes(i));
  const working = openNow.filter((i: any) => !parked.includes(i) && !finished.includes(i));

  const verdict = await mainVerdict();
  const days = await loginDaysLeft();
  const list = (xs: any[]) => (xs.length ? xs.map((i: any) => `#${i.number}`).join(", ") : "none");

  const body = [
    `🤖 **${today()}**`,
    "",
    `- **Landed:** ${list(closed)}`,
    `- **Filed:** ${list(filed)}`,
    `- **Being built:** ${list(working)}`,
    `- **Finished, shipping when CI passes:** ${list(finished)}`,
    `- **Stuck, waiting for you:** ${list(parked)}`,
    `- **\`main\`:** ${
      !verdict
        ? "no completed CI run to read"
        : verdict.failed.length
          ? `red on \`${verdict.sha.slice(0, 8)}\` — ${verdict.failed.map(([st, n]) => `${n} (${st})`).join(", ")}`
          : `green on \`${verdict.sha.slice(0, 8)}\``
    }`,
    `- **Login:** ${days === null ? "unknown" : days <= 7 ? `**${days} days left — sign in on the VPS or this all stops**` : `${days} days left`}`,
    `- **Settings:** ${cfg.ticketsPerDay}/day, ${cfg.maxOpen} at once, shipping ${cfg.ship ? "on" : "off"}`,
  ].join("\n");

  const issue = await logIssue();
  await comment(issue.number, body);
  state.digestDay = today();
  await saveState(state);
  log(`posted the daily note on #${issue.number}`);
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

// Comments land on a public repository, and deployMain posts raw command
// output — a curl error can echo a URL with a token in it. Scrub the exact
// values of every secret-shaped variable this process holds before posting.
// Bare hex is deliberately not matched: commit SHAs are 40-hex too.
const SECRET_VALUES: string[] = Object.entries(process.env)
  .filter(([k, v]) => /TOKEN|SECRET|PASSWORD|_KEY/i.test(k) && (v ?? "").length >= 8)
  .map(([, v]) => v as string);

async function comment(n: number, body: string) {
  for (const v of SECRET_VALUES) body = body.split(v).join("[redacted]");
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
} else if (cmd === "digest") {
  await digest(cfg);
} else if (cmd === "ship") {
  await shipReady(cfg);
} else if (cmd === "generate") {
  await rearmParkedTickets();
  await generate(cfg);
  await generateQa(cfg);
} else if (cmd === "tick") {
  await digest(cfg);
  await shipReady(cfg);
  await rearmParkedTickets();
  await generate(cfg);
  await generateQa(cfg);
} else {
  console.error(`unknown command: ${cmd}`);
  process.exit(2);
}

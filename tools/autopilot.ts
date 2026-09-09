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
// token that can push to `main`. This process holds it instead and can only do
// the fixed sequence below.
//
// It does read one piece of free text now: `focus`, typed into the admin console
// by a signed-in operator and quoted into the tickets it files. That is not a
// way in — the text reaches a *session's* prompt, which is somewhere anyone who
// can open an issue already reaches, and it reaches nothing here but a string
// interpolation. What it must never become is an instruction this process acts
// on itself.
//
// ## The sequence
//
// A ticket the watcher finished carries `agent:done`; `ship` on the same issue
// is the standing permission to land it. This then fast-forwards `main` to the
// branch head and hands `main` to tools/deploy-main.
//
// **It opens no pull request** (2026-08-30). It used to, and the request was
// never a review — it existed to *start a build*, because `ci.yml` has no push
// trigger on branches and there was otherwise nothing for CI to have said about
// one. Landing then cost two full runs of one tree on a runner with capacity 3:
// the request's, and the merge commit's.
//
// The branch is still required to be green, by the direct route — a
// `workflow_dispatch` on the branch ref (`dispatchCi`). One run, not two, and
// what it judges is exactly what lands, because a fast-forward makes no merge
// commit. A red branch is parked with `agent:failed` and the failing job named
// on the ticket, rather than retried hourly for ever with the reason only in
// this process's log.
//
// Nothing here is a judgement about whether the change is *good*. That is the
// trade docs/direction.md describes: the lane is limited to work whose success
// a machine can see.

import { $ } from "bun";
import { rename } from "node:fs/promises";

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
// Since 2026-08-30 the admin console writes it too — cartopy's
// `server/autopilot.ts` (the Autopilot tab's sliders) — which makes this Config
// a hand-synced seam like `AccountRow`: add a field here and add it there, or
// the console silently strips it on the next save.
//
//   { "enabled": false,        // file tickets at all
//     "ticketsPerDay": 1,      // how many new feature tickets a day
//     "qaPerDay": 0,           // how many QA passes a day (0 = the lane is off)
//     "qaMaxOpen": 3,          // how many QA tickets may be open at once
//     "retroEveryDays": 0,     // file a retrospective every N days (0 = never)
//     "ship": false,           // merge and deploy finished ones, or stop at review
//     "focus": "",             // an area to work in; empty means the frontier
//     "focusPlan": [],         // the ordered queue under that focus (see below)
//     "planRevision": 0 }      // bumped by the console to ask for a fresh plan

type PlanItem = {
  /** Stable key, and the ticket's dedupe marker (`autopilot:focus-<id>`). */
  id: string;
  /** One line: the piece of work. This becomes the ticket title. */
  title: string;
  /** Where in the code, and how it would be judged. Empty is allowed. */
  note: string;
  /** The ticket it became, or 0 while it has not been filed. */
  issue: number;
};

type Config = {
  enabled: boolean;
  ticketsPerDay: number;
  maxOpen: number;
  ship: boolean;
  qaPerDay: number;
  qaMaxOpen: number;
  retroEveryDays: number;
  focus: string;
  focusPlan: PlanItem[];
  planRevision: number;
};

const DEFAULTS: Config = {
  enabled: false,
  ticketsPerDay: 5,
  maxOpen: 2,
  ship: false,
  qaPerDay: 0,
  qaMaxOpen: 3,
  retroEveryDays: 0,
  focus: "",
  focusPlan: [],
  planRevision: 0,
};

/** Long enough for a paragraph of intent, short enough that a ticket still reads as one. */
const MAX_FOCUS_CHARS = 2000;
/** A plan is a queue a person reads. Past twenty items nobody reorders it, they rewrite it. */
const MAX_PLAN_ITEMS = 20;

/**
 * Whatever is in the file, as a plan.
 *
 * Applied to the console's list and to a session's posted block alike — the
 * second is JSON a language model wrote, so nothing here may assume a field is
 * present, a string, or short.
 */
function planItems(raw: unknown): PlanItem[] {
  if (!Array.isArray(raw)) return [];
  const out: PlanItem[] = [];
  const seen = new Set<string>();
  for (const entry of raw) {
    if (!entry || typeof entry !== "object") continue;
    const e = entry as Record<string, unknown>;
    const title = String(e.title ?? "").trim().replace(/\s+/g, " ").slice(0, 200);
    if (!title) continue;
    const given = String(e.id ?? "").toLowerCase().replace(/[^a-z0-9-]+/g, "-").replace(/^-|-$/g, "").slice(0, 48);
    const id = given && !seen.has(given) ? given : freshId(title, seen);
    seen.add(id);
    const issue = Math.floor(Number(e.issue));
    out.push({ id, title, note: String(e.note ?? "").trim().slice(0, 1000), issue: Number.isFinite(issue) ? Math.max(0, issue) : 0 });
    if (out.length >= MAX_PLAN_ITEMS) break;
  }
  return out;
}

/** A slug of the title, made unique against the ids already taken. */
function freshId(title: string, seen: Set<string>): string {
  const slug = title.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "").slice(0, 40) || "item";
  let id = slug;
  for (let n = 2; seen.has(id); n++) id = `${slug}-${n}`;
  return id;
}

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
      retroEveryDays: Number.isFinite(raw.retroEveryDays) ? Math.max(0, Math.floor(raw.retroEveryDays)) : DEFAULTS.retroEveryDays,
      // An area to work in, in the maintainer's own words. Empty — which is the
      // default and the state to return to — hands the queue back to the
      // frontier in docs/direction.md.
      focus: String(raw.focus ?? "").trim().slice(0, MAX_FOCUS_CHARS),
      focusPlan: planItems(raw.focusPlan),
      planRevision: Number.isFinite(raw.planRevision) ? Math.max(0, Math.floor(raw.planRevision)) : 0,
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

/**
 * Change named fields of the control file, keeping everything else in it.
 *
 * This process is a *reader* of that file everywhere else — it is the console's
 * knob, not this lane's state, which is why there are two files at all. There
 * are exactly two things it writes back, and both are answers to a question the
 * console asked: the plan a planning session posted, and the issue number
 * stamped onto a plan item when it is filed.
 *
 * Merged into the file as it stands at that instant and written through a temp
 * file and a rename, because cartopy's `server/autopilot.ts` may be saving an
 * operator's edit in the same second — and a torn parse over here falls back to
 * DEFAULTS, i.e. reads as the whole lane switching itself off. Neither write
 * ever reorders the plan or rewrites a title: those belong to the person editing
 * the list, so the worst a collision can cost is one stamp, which `filedIssue`
 * recovers on the next tick.
 *
 * `edit` returns false to abandon the write, which is not an error — it is how
 * "the list is no longer empty, leave it alone" is said.
 */
async function writeConfig(edit: (raw: Record<string, unknown>) => boolean): Promise<boolean> {
  // Re-read rather than reuse the tick's Config: that one is normalised, and
  // writing it back would silently delete any field the console has that this
  // version has not heard of.
  const raw = JSON.parse(await Bun.file(CONFIG_PATH).text()) as Record<string, unknown>;
  if (!edit(raw)) return false;
  const tmp = `${CONFIG_PATH}.autopilot-${process.pid}.tmp`;
  await Bun.write(tmp, `${JSON.stringify(raw, null, 2)}\n`);
  await rename(tmp, CONFIG_PATH);
  return true;
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
/**
 * Every Actions task recorded against a commit, as `[status, job name]`.
 *
 * Deduped by job name, keeping the newest — the list is newest-first, and a
 * re-dispatched run adds a second task under the same name whose predecessor's
 * `failure` is not a verdict on it. `main` currently carries exactly that shape,
 * so reading every row would call a fixed branch red for ever.
 */
async function jobsFor(sha: string): Promise<[string, string][]> {
  const d = await api(`/actions/tasks?limit=50`, {}, MERGE_TOKEN ?? TOKEN);
  const seen = new Set<string>();
  const out: [string, string][] = [];
  for (const r of d.workflow_runs ?? []) {
    if (r.head_sha !== sha) continue;
    const name = r.name ?? "";
    if (seen.has(name)) continue;
    seen.add(name);
    out.push([r.status ?? "", name]);
  }
  return out;
}

/**
 * Start CI on a branch without opening anything.
 *
 * `ci.yml` carries `workflow_dispatch` for an unrelated reason (re-warming the
 * cache after a skipped `main` run), and it is what makes a branch verdict
 * reachable at all now that the pull request is gone: `ci.yml` has no push
 * trigger on branches, so a pushed branch is otherwise never built.
 *
 * A dispatched run is **stronger** than the pull-request lane's ever was, in two
 * ways. It runs everything — the scope step declines to narrow itself for a hand
 * started run, so both scene smokes and both cross-target lanes run, where a
 * pull request only ever got the suite and rustfmt. And because the landing is a
 * fast-forward, the tree that was tested *is* the tree that ends up on `main`; a
 * merge commit is a commit nobody built, which is why `ci.yml` has a step
 * comparing its tree against its second parent's.
 *
 * Not awaited to completion. A run is 15-25 minutes and this tick has other
 * tickets to move, so the answer is read on a later one. Re-dispatching is
 * harmless if it ever happens: `ci.yml`'s `concurrency` cancels a branch's
 * previous run when a new one starts.
 */
async function dispatchCi(branch: string) {
  await api(`/actions/workflows/ci.yml/dispatches`, { method: "POST", body: JSON.stringify({ ref: branch }) }, MERGE_TOKEN);
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

    // Whether the landing got through — a failure after that point must not tell
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
            `\n\nThe branch \`${branch}\` is finished; it is waiting for you to land it. ` +
            `The reasoning is in \`docs/direction.md\` — these are the changes no revert from here undoes.`,
        );
        log(`#${n}: fenced (${fenced.length} path(s)) — left for review`);
        continue;
      }

      // **The branch has to be green, and a pull request is not how this lane
      // finds that out any more.** `ci.yml` has no push trigger on branches, so
      // a pushed branch is never built on its own — which is the entire reason
      // a request used to be opened here. `workflow_dispatch` starts the same
      // run against the branch ref with one call and nothing to merge.
      //
      // Reading the answer is a *later tick's* job. A run is 15-25 minutes on a
      // runner with capacity 3; blocking here would hold this pass open for one
      // ticket while every other one waited.
      const files: string[] = Array.from(
        new Set(((await api(`/compare/main...${encodeURIComponent(branch)}`)).files ?? []).map((f: any) => f.filename as string)),
      );
      // The same paths `ci.yml` ignores on its triggers. A branch inside that
      // set would wait for ever for a run that a dispatch *would* start (
      // `paths-ignore` does not apply to a dispatch) and that would compile the
      // world to prove prose.
      const docsOnly =
        files.length > 0 && files.every((f) => f.startsWith("docs/") || f.startsWith(".claude/") || f.endsWith(".md"));

      if (docsOnly) {
        log(`#${n}: documentation only (${files.length} file(s)) — CI does not run on these, landing`);
      } else {
        const jobs = await jobsFor(sha);
        if (!jobs.length) {
          await dispatchCi(branch);
          log(`#${n}: asked CI to build ${branch} (${sha.slice(0, 8)}) — landing it when that is green`);
          continue;
        }
        const pending = jobs.filter(([st]) => ["running", "waiting", "queued", "blocked"].includes(st));
        if (pending.length) {
          log(`#${n}: CI still building ${sha.slice(0, 8)} (${pending.map(([, name]) => name).join(", ")})`);
          continue;
        }
        const bad = jobs.filter(([st]) => st !== "success");
        if (bad.length) {
          // Parked, not retried. The old lane logged this and looked again in an
          // hour, for ever — a red branch became a ticket that simply stopped
          // moving, with the reason only in this process's log, which is the one
          // place nobody is reading.
          await api(`/issues/${n}/labels/${await labelId(SHIP)}`, { method: "DELETE" });
          await addLabel(n, FAILED);
          await comment(
            n,
            `🤖 **Not landing this — CI is red on \`${branch}\`** (\`${sha.slice(0, 8)}\`):\n\n` +
              bad.map(([st, name]) => `- \`${name}\` — ${st}`).join("\n") +
              `\n\nFix it on the branch and put \`${SHIP}\` back to land it.`,
          );
          log(`#${n}: ${bad.map(([st, name]) => `${name}=${st}`).join(", ")} — parked`);
          continue;
        }
        log(`#${n}: CI is green on ${sha.slice(0, 8)}`);
      }

      // Landing is a ref update, so it is done as one. No pull request is opened
      // and no merge commit is made: `landOnMain` fast-forwards `main` to the
      // very commit the run above passed on, which is a stronger guarantee than
      // the old path gave — a merge commit is a commit nobody built.
      log(`#${n}: fast-forwarding main to ${sha.slice(0, 8)} (${branch})`);
      await landOnMain(branch, sha);
      merged = true;

      await api(`/issues/${n}/labels/${await labelId(SHIP)}`, { method: "DELETE" });
      await addLabel(n, SHIPPED);
      await comment(n, `🤖 **Landed on \`main\`** as \`${sha.slice(0, 8)}\` — fast-forwarded from \`${branch}\`, no pull request.`);
      // The pull request body's `Closes #N` used to do this, and the card only
      // moves when the issue is closed: the watcher projects a closed issue onto
      // the board, Forgejo having no kanban API of its own.
      await api(`/issues/${n}`, { method: "PATCH", body: JSON.stringify({ state: "closed" }) });

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

/**
 * Move `main` to `sha`, with no pull request and no checkout.
 *
 * Forgejo's API has no endpoint that fast-forwards a branch — `git/refs` is
 * read-only and `branches` only creates — so this is done with git, which needs
 * neither a working tree nor the objects. The commits are already on the server:
 * a shallow fetch of the branch into a throwaway bare repository is enough to
 * name the sha, and the push that follows sends an empty pack because the remote
 * has everything in it already. Measured against this server on 2026-08-30: a
 * bare, `--depth=1` repository can move a ref this way.
 *
 * **Not forced.** A plain push is a fast-forward or a rejection, and a rejection
 * means either that `main` moved or that this branch was never based on it.
 * Both are the same instruction to a caller with no checkout — rebase it
 * somewhere that has one — so both arrive as `MergeConflict`.
 */
async function landOnMain(branch: string, sha: string) {
  const host = API.replace(/\/api\/v1\/?$/, "");
  const url = `${host}/${REPO}.git`;
  const dir = `/tmp/autopilot-ff-${sha.slice(0, 12)}`;
  // Through `http.extraHeader` rather than a token in the URL: a remote URL is
  // echoed back by git on every error and lands in this process's log.
  const auth = `Authorization: token ${MERGE_TOKEN}`;
  await $`rm -rf ${dir}`.quiet();
  try {
    await $`git init --bare -q ${dir}`.quiet();
    await $`git --git-dir=${dir} -c http.extraHeader=${auth} fetch -q --depth=1 ${url} refs/heads/${branch}:refs/landing`.quiet();
    const got = (await $`git --git-dir=${dir} rev-parse refs/landing`.text()).trim();
    if (got !== sha) {
      // The branch moved between reading its head and fetching it. Landing the
      // newer commit would ship something this tick never looked at.
      throw new Error(`${branch} moved while landing: expected ${sha.slice(0, 8)}, fetched ${got.slice(0, 8)}`);
    }
    const push = await $`git --git-dir=${dir} -c http.extraHeader=${auth} push ${url} ${sha}:refs/heads/main`.quiet().nothrow();
    if (push.exitCode !== 0) {
      const err = push.stderr.toString() + push.stdout.toString();
      if (/non-fast-forward|fetch first|rejected/i.test(err)) {
        throw new MergeConflict(`\`${branch}\` does not fast-forward onto \`main\` — it needs a rebase`);
      }
      throw new Error(`push → ${err.trim()}`);
    }
  } finally {
    await $`rm -rf ${dir}`.quiet().nothrow();
  }
}

/** The branch genuinely conflicts with `main`; no amount of retrying merges it. */
class MergeConflict extends Error {}

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
let issueCache: any[] | null = null;

async function knownIssues(): Promise<any[]> {
  if (!issueCache) issueCache = (await api(`/issues?state=all&type=issues&limit=100`)) as any[];
  return issueCache!;
}

/** The ticket filed under this key, or null. The focus lane needs the issue, not the fact. */
async function filedIssue(key: string): Promise<any | null> {
  return (await knownIssues()).find((i: any) => `${i.body ?? ""}`.includes(`autopilot:${key}`)) ?? null;
}

async function alreadyFiled(key: string): Promise<boolean> {
  return (await filedIssue(key)) !== null;
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
  const mine = open.filter(
    (i: any) =>
      labelsOf(i).includes(FILED_BY_AUTOPILOT) &&
      !labelsOf(i).includes(QA) &&
      !labelsOf(i).includes(RETRO) &&
      !labelsOf(i).includes(PLAN),
  );
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

  // A focus, while one is set, *replaces* the frontier as this lane's queue —
  // it does not decorate it. The frontier answers "what is the next source
  // nobody has wired in?", which is a fine standing question and the wrong one
  // to answer while the maintainer has said what this fortnight is for.
  if (cfg.focus) return await generateFocus(cfg, state);

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

// --- the focus ----------------------------------------------------------------
//
// The frontier is a standing queue: sources somebody publishes, filed one at a
// time for ever. It answers "what next?" and it cannot answer "spend the next
// fortnight on X" — the maintainer's own priority, which is usually not a row in
// that table at all ("get this solid on the Steam Deck runner" is not a data
// source). So there is a second queue, and the difference that matters is that a
// **person orders it**: the console carries a focus sentence and a plan under it,
// this lane files that plan top down, and the frontier stands down until the
// focus is cleared.
//
// Turning a sentence into six pieces of work is the one part that needs
// judgement, and this process still runs no language model — so it is done the
// way every other judgement in this lane is done, by a session on a ticket. The
// planning ticket posts its answer back as a fenced `autopilot-plan` block and
// this reads it into the control file, where the console can reorder, rewrite,
// extend and delete it before most of it has been built. At one ticket a day
// there is a day between a plan arriving and its second item being started.
//
// Three things are deliberate. The plan is stored **only while the console's
// list is empty**, so a second planning session can never overwrite an operator's
// edits. A filed item is remembered by its **issue number in the file**, not by
// its position, so reordering the list after some of it is built refiles
// nothing. And **nothing about the fences changes**: a focus asking for a
// protocol bump files tickets that are refused at the ship gate like any other.
//
// The block is read from any comment on the ticket, by anybody. That is not a
// privilege — a Forgejo account can already open an issue, which is a prompt to
// a session by a shorter route — and what it produces is a list a person sees.

const PLAN = "plan";

/** 32-bit FNV-1a, hex. Only ever used to key a ticket to the focus text that asked for it. */
function textKey(text: string): string {
  let h = 0x811c9dc5;
  for (let i = 0; i < text.length; i++) {
    h ^= text.charCodeAt(i);
    h = Math.imul(h, 0x01000193);
  }
  return (h >>> 0).toString(16).padStart(8, "0");
}

// Keyed on the focus text *and* a revision the console bumps: editing the focus
// asks a new question, and pressing "plan again" asks the same one twice, and
// alreadyFiled has to be able to tell those from a tick that already ran.
const planKey = (cfg: Config) => `focus-plan-${textKey(cfg.focus)}-${cfg.planRevision}`;

const quoted = (text: string) => `> ${text.split("\n").join("\n> ")}`;

const PLAN_BLOCK = /```autopilot-plan\s*\n([\s\S]*?)```/g;

/**
 * The plan a planning session posted, or null while it has not.
 *
 * The **last** valid block wins: a session that posts a draft and then a
 * correction means the correction, and taking the first would file work it
 * withdrew. A block that will not parse is logged and skipped rather than
 * failing the tick — the ticket is still open, and a session can post again.
 */
async function planFromTicket(n: number): Promise<PlanItem[] | null> {
  const comments = await api(`/issues/${n}/comments?limit=50`);
  let found: PlanItem[] | null = null;
  for (const c of comments) {
    for (const m of `${c.body ?? ""}`.matchAll(PLAN_BLOCK)) {
      try {
        const items = planItems(JSON.parse(m[1]));
        if (items.length) found = items;
      } catch {
        log(`#${n}: a plan block did not parse as JSON — ignoring it`);
      }
    }
  }
  return found;
}

/** Store a posted plan, unless the operator's list has meanwhile stopped being empty. */
async function storePlan(items: PlanItem[]): Promise<boolean> {
  return await writeConfig((raw) => {
    if (planItems(raw.focusPlan).length) return false;
    raw.focusPlan = items;
    return true;
  });
}

/**
 * Record which ticket a plan item became.
 *
 * Matched by `id` against the file as it stands, so an operator who reordered
 * the list while this tick was filing keeps their order and gets the stamp.
 */
async function stampPlanItem(id: string, issue: number): Promise<boolean> {
  return await writeConfig((raw) => {
    const list = Array.isArray(raw.focusPlan) ? (raw.focusPlan as Record<string, unknown>[]) : [];
    const item = list.find((e) => e && typeof e === "object" && String((e as any).id) === id);
    if (!item) return false;
    item.issue = issue;
    return true;
  });
}

async function filePlanningTicket(cfg: Config, key: string) {
  const body = [
    "The maintainer has set a focus for this lane, and it needs turning into a queue of work.",
    "",
    quoted(cfg.focus),
    "",
    "**This is a planning pass, not a build.** Change no code and open no branch. What comes",
    "back is a list, which the maintainer reorders and edits in the console before any of it",
    "is filed — so propose the work, do not start it.",
    "",
    "1. **Read before proposing.** `docs/direction.md` for the goal and the seven values,",
    "   `CLAUDE.md` for how this tree is built and what it already does, and whatever part of",
    "   the code the focus names. Half of a good plan is noticing the thing is already there.",
    "2. **Propose six to ten pieces of work.** Each one a ticket a session could finish in a",
    "   sitting, ordered so the earlier ones unblock the later ones. A title that says what",
    "   changes, and a note saying where in the code it lands and how a machine would judge",
    "   it — that last part is what this lane can land unattended.",
    "3. **Leave out what it may not do.** The wire protocol, database migrations, the CI",
    "   workflows, `tools/autopilot.ts` itself and `assetlinks.json` are fenced: a ticket",
    "   touching one is built and then parked for a person. Anything whose success is a",
    "   matter of taste belongs to a person too.",
    "4. **Post the plan in one comment on this ticket**, as exactly this block:",
    "",
    "````",
    "```autopilot-plan",
    '[{"title": "…", "note": "…"}, {"title": "…", "note": "…"}]',
    "```",
    "````",
    "",
    "   JSON, an array, `title` and `note` both strings. The autopilot reads the last such",
    "   block on this ticket and stores it as the queue; everything else in the comment is",
    "   ignored, so write whatever reasoning you want around it.",
    "5. **Then close this ticket.** There is no branch to merge.",
    "",
    "**Decide it yourself.** Nobody is watching this ticket, so a question asked here is a",
    "ticket that stops. The values at the top of `docs/direction.md` settle the ambiguity, and",
    "a piece of work you are unsure about is one to leave off the list rather than ask about.",
    "",
    `<sub>Filed by the autopilot. <!-- autopilot:${key} --></sub>`,
  ].join("\n");

  const issue = requireIssue(
    await api(`/issues`, {
      method: "POST",
      body: JSON.stringify({
        title: `Plan the focus: ${cfg.focus.split("\n")[0].slice(0, 80)}`,
        body,
        labels: await labelIds(["agent", FILED_BY_AUTOPILOT, PLAN]),
      }),
    }),
  );
  log(`filed #${issue.number}: plan the focus`);
}

async function fileFocusTicket(cfg: Config, item: PlanItem, position: string): Promise<any> {
  const body = [
    "From the maintainer's focus for this lane:",
    "",
    quoted(cfg.focus),
    "",
    `**${item.title}**`,
    ...(item.note ? ["", item.note] : []),
    "",
    `This is ${position} of the plan under that focus. The queue is ordered by a person in the`,
    "admin console, so build this one rather than the one you would have picked — and keep it",
    "the size it is. A ticket that turns out to be three tickets is one ticket: finish this",
    "part and say on it what the other two are.",
    "",
    "If the item is simply wrong — already done, impossible, or a bad idea against the values",
    "— say so here and close it. That is a result, not a failure.",
    "",
    "**Decide it yourself.** This ticket is not being watched, so a question asked here is a",
    "ticket that stops. The seven values at the top of `docs/direction.md` exist to settle",
    "exactly that kind of ambiguity: pick the reading they support, say in the commit body",
    "which one you applied, and build. Only a decision needing something nobody can derive",
    "from the repository — a credential, a licence somebody must accept, a choice about what",
    "the project is *for* — is a reason to stop.",
    "",
    `<sub>Filed by the autopilot. <!-- autopilot:focus-${item.id} --></sub>`,
  ].join("\n");

  return requireIssue(
    await api(`/issues`, {
      method: "POST",
      body: JSON.stringify({
        title: item.title,
        body,
        labels: await labelIds([AUTONOMOUS, FILED_BY_AUTOPILOT, ...(cfg.ship ? [SHIP] : [])]),
      }),
    }),
  );
}

/**
 * The planning lane: get a queue under the focus, or wait for the session that
 * is writing one.
 *
 * A step of the tick in its own right, beside the QA and retrospective lanes,
 * rather than the first half of `generateFocus` — which is where it started and
 * where it deadlocked. Everything guarding the feature lane guards *code being
 * built*: a red `main` every new branch would inherit, a daily quota, one
 * ticket at a time. This files no branch and writes no code, and behind those
 * gates it was unreachable in exactly the state that produces it — a focus set
 * on a busy board answered "no plan yet" in the console for as long as the
 * board stayed busy, with nothing said anywhere about why. It is still one
 * session per (focus, revision), self-limited by its key, and it still holds no
 * feature slot.
 *
 * `enabled` does gate it. That knob means "file no new tickets", and this files
 * a ticket.
 */
async function planFocus(cfg: Config): Promise<void> {
  if (!cfg.enabled || !cfg.focus || cfg.focusPlan.length) return;

  const key = planKey(cfg);
  const ticket = await filedIssue(key);
  if (!ticket) return await filePlanningTicket(cfg, key);

  const posted = await planFromTicket(ticket.number);
  if (!posted) return log(`#${ticket.number} is planning the focus — no plan posted yet`);
  // A write that cannot happen must not take the tick with it: the feature, QA
  // and retrospective lanes all run after this one, and an unwritable control
  // file (a volume mounted read-only) would otherwise stop all three every hour
  // with a stack trace as the only explanation.
  let stored: boolean;
  try {
    stored = await storePlan(posted);
  } catch (e) {
    return log(`could not store the plan from #${ticket.number} — is ${CONFIG_PATH} writable from here? ${e instanceof Error ? e.message : e}`);
  }
  if (!stored) {
    return log(`#${ticket.number} posted a plan but the console's list is no longer empty — keeping the console's`);
  }
  log(`stored ${posted.length} plan item(s) from #${ticket.number}`);
  // The tick's own copy, so the first item can be filed in this same pass
  // rather than an hour later.
  cfg.focusPlan = posted;
}

/**
 * The feature lane, while a focus is set. Called from `generate` behind the same
 * quota and open-ticket gates the frontier goes through — a focus changes where
 * the work comes from, never how fast it arrives.
 */
async function generateFocus(cfg: Config, state: State): Promise<void> {
  const plan = cfg.focusPlan;
  if (!plan.length) return log("a focus is set and there is no plan under it yet — nothing to file");

  const next = plan.find((i) => !i.issue);
  if (!next) {
    return log(`the focus plan is fully filed (${plan.length} item(s)) — extend it in the console, or clear the focus`);
  }

  // Filed on an earlier tick whose stamp did not land (the console saved over it,
  // or the control file is not writable from here). The marker is the authority,
  // not the stamp: filing it twice is the one mistake this lane cannot take back.
  const already = await filedIssue(`focus-${next.id}`);
  if (already) {
    await stampPlanItem(next.id, already.number).catch(() => {});
    return log(`plan item “${next.title}” is already #${already.number} — recorded it`);
  }

  const done = plan.filter((i) => i.issue).length;
  const issue = await fileFocusTicket(cfg, next, `item ${done + 1} of ${plan.length}`);
  log(`filed #${issue.number}: ${next.title}`);
  state.filed++;
  await saveState(state);
  await stampPlanItem(next.id, issue.number).catch((e) =>
    log(`could not record #${issue.number} on the plan — ${e instanceof Error ? e.message : e}`),
  );
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

async function fileQaTicket(state: State, key: string, name: string, intro: string, focus = "") {
  const body = [
    intro,
    "",
    // A steer, never a substitute for the target: a QA pass whose target the
    // focus does not touch is still worth walking, and narrowing it to the
    // focus would silently stop covering everything else.
    ...(focus
      ? [
          "The maintainer's current focus for this lane is:",
          "",
          `> ${focus.split("\n").join("\n> ")}`,
          "",
          "Walk the whole target regardless, and weight the depth toward that focus where the",
          "two meet.",
          "",
        ]
      : []),
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
      cfg.focus,
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
    await fileQaTicket(state, key, t.name, `From the standing list in \`docs/qa/targets.md\`: **${t.name}** — ${t.how}.`, cfg.focus);
    return;
  }
  log("every QA target has been filed today");
}

// --- branch sweeping ---------------------------------------------------------
//
// A merged ticket used to leave its branch behind for ever: merges passed
// delete_branch_after_merge: false and nothing else ever deleted one, so the
// branch list only grew. Merges delete their branch now; this sweeps what
// predates that, plus anything merged by hand. The safety rule is absolute —
// a branch is deleted only when `main` already contains everything on it
// (compare answers zero commits ahead), so nothing unmerged can be lost. A
// branch whose head moved in the last day is left alone: one just cut from
// `main` for new work is zero ahead too, and must not vanish mid-session.

async function sweepMergedBranches() {
  const branches: any[] = [];
  for (let page = 1; ; page++) {
    const b = await api(`/branches?page=${page}&limit=50`, {}, MERGE_TOKEN);
    branches.push(...b);
    if (b.length < 50) break;
  }
  for (const b of branches) {
    if (b.name === "main" || b.protected) continue;
    const age = Date.now() - new Date(b.commit?.timestamp ?? Date.now()).getTime();
    if (age < 24 * 3_600_000) continue;
    try {
      const cmp = await api(`/compare/main...${encodeURIComponent(b.name)}`);
      if ((cmp.total_commits ?? (cmp.commits ?? []).length) > 0) continue;
      await api(`/branches/${encodeURIComponent(b.name)}`, { method: "DELETE" }, MERGE_TOKEN);
      log(`swept fully-merged branch ${b.name}`);
    } catch (e) {
      log(`could not sweep ${b.name} — ${e instanceof Error ? e.message : e}`);
    }
  }
}

// --- the retrospective -------------------------------------------------------
//
// Both other lanes execute; neither ever steps back. Every `retroEveryDays`
// days one session reads the whole period — what shipped, what stalled, what
// the QA passes found — and steers, inside hard bounds. The one real authority
// it holds is promotion: it may move up to three `qa-gap` findings into the
// build lane, which closes the loop QA opened (a gap is found, judged against
// the values, and built, with no person in between). Direction itself stays
// out of reach — docs/direction.md's own rule is that a session proposes
// changes as a ticket and never rewrites it — and the gap-label requirement
// matters: labels need write access, so `qa-gap` marks a finding as the lane's
// own, not text a stranger filed.

const RETRO = "retro";

async function generateRetro(cfg: Config) {
  if (!cfg.enabled || !cfg.retroEveryDays) return;

  // Deterministic period index instead of a state field: every tick in the
  // same period computes the same key, and alreadyFiled dedupes.
  const period = Math.floor(Date.now() / (cfg.retroEveryDays * 86_400_000));
  const key = `retro-p${period}`;
  if (await alreadyFiled(key)) return;

  const days = cfg.retroEveryDays;
  const body = [
    `Step back and read the last ${days} day(s) of this lane, then steer it. You are not building anything on this ticket.`,
    "",
    ...(cfg.focus
      ? [
          "The maintainer has set a focus for this lane, so judge the period against it as well",
          "as against the values — including whether the plan under it is still the right queue:",
          "",
          `> ${cfg.focus.split("\n").join("\n> ")}`,
          "",
        ]
      : []),
    "1. **Gather.** `docs/direction.md` (the goal and the seven values); every issue closed or",
    `   filed in the last ${days} day(s) — shipped, failed, parked — and the reports on closed`,
    "   `qa` tickets; the open `qa-gap` backlog. All of it is on the Forgejo API with the",
    "   `FORGEJO_TOKEN` in your environment.",
    "2. **Analyse against the goal.** Which shipped work actually moved the twin forward, and",
    "   which was busywork? Where did tickets stall, and is the pattern a machinery problem",
    "   (conflicts, refusals, flakes) rather than a hard ticket? What do the QA findings say",
    "   players keep hitting? Is the frontier still the highest-value queue, or is the gap",
    "   backlog now worth more than the next data source?",
    "3. **Act, inside these bounds and no further:**",
    "   - **Promote at most three open `qa-gap` issues into the build lane** by adding the",
    "     `autonomous` label" + (cfg.ship ? " and the `ship` label" : "") + ". Only gaps whose success a machine",
    "     can judge, and nothing the fences cover — no wire protocol, no database schema, no",
    "     taste. Only issues already carrying `qa-gap`: that label is the lane vouching for",
    "     its own finding. Say on each one why it was promoted.",
    "   - **File a ticket for each machinery problem** you can name precisely (the autopilot,",
    "     the watcher, CI, the deploy). These are code under `tools/`, which the unattended",
    "     lane may build but never merge — they land for review, which is the point.",
    "   - **Propose direction changes as a ticket** titled `proposal: …`, argued from the",
    "     period you just read. Never edit `docs/direction.md` here and never label a proposal",
    "     `autonomous` — the direction is the maintainer's, by that file's own first rule.",
    "4. **Report and close.** Final message: the period in five lines, what you promoted and",
    "   why, what you filed, what you propose. Then close this ticket through the API.",
    "",
    "**Decide it yourself.** Nobody is watching this ticket. Judge by the seven values; a",
    "promotion you are unsure about is one to leave for the maintainer, not to ask about.",
    "",
    `<sub>Filed by the autopilot. <!-- autopilot:${key} --></sub>`,
  ].join("\n");

  const issue = requireIssue(
    await api(`/issues`, {
      method: "POST",
      body: JSON.stringify({
        title: `Retrospective: the last ${days} day(s), and what to do differently`,
        body,
        labels: await labelIds(["agent", FILED_BY_AUTOPILOT, RETRO]),
      }),
    }),
  );
  log(`filed #${issue.number}: retrospective for period ${period}`);
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
    // Which queue is driving is the single most surprising thing about this
    // lane's output — a note listing tickets nobody recognises, with no line
    // saying a focus is set, is a note that raises more questions than it settles.
    `- **Queue:** ${
      cfg.focus
        ? `focus — ${cfg.focusPlan.filter((i) => i.issue).length}/${cfg.focusPlan.length} plan item(s) filed — “${cfg.focus.split("\n")[0].slice(0, 90)}”`
        : "the frontier in `docs/direction.md`"
    }`,
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
  await planFocus(cfg);
  await generate(cfg);
  await generateQa(cfg);
  await generateRetro(cfg);
} else if (cmd === "tick") {
  await digest(cfg);
  await shipReady(cfg);
  await sweepMergedBranches();
  await rearmParkedTickets();
  await planFocus(cfg);
  await generate(cfg);
  await generateQa(cfg);
  await generateRetro(cfg);
} else {
  console.error(`unknown command: ${cmd}`);
  process.exit(2);
}

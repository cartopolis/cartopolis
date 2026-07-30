---
name: branch-context
description: Retrieve the agent chat digest for a branch from refs/agent-context/<branch> — the reasoning, dead ends and open questions from the session(s) that built it. Use before continuing or reviewing someone else's unfinished branch.
---

# branch-context

Fetch and read the chat digest published for a branch. Sessions run through
`claude.ai/code` execute in the `viberfox-agent` container on the VPS, and a
`Stop` hook publishes a mechanical digest of each session to
`refs/agent-context/<branch>` in a **separate, private repo**. That ref carries
the *why* — the
alternatives that lost, the dead ends, the things the user still has to decide —
none of which survives in the diff.

See [docs/notes/agent-context-refs.md](../../../docs/notes/agent-context-refs.md)
for why it lives on a ref outside `refs/heads/*` in a private repo.

Digests are **kept after a branch lands**, so this works on merged work too — it
is often the only surviving answer to "why is this like this?" once the branch
name is gone.

## Retrieve

The digests live in `jeroen/viberfox-agent-context`, **not** in this repo — it
is private, because a transcript is never reviewed for credentials before it is
published and viberfox itself is public. Fetch from there explicitly:

```bash
git fetch ssh://git@code.garage44.eu:2222/jeroen/viberfox-agent-context.git \
    '+refs/agent-context/*:refs/agent-context/*'
git for-each-ref 'refs/agent-context/**' --format='%(refname:lstrip=2)'
```

You need read access to that repo; there is no anonymous path to it.

Then list and read. One file per session, named for the session id:

```bash
git ls-tree -r --name-only refs/agent-context/<branch>
git show refs/agent-context/<branch>:<session-id>.md
```

**A branch may have several sessions, and the earliest is often the important
one.** `feat/shared-http-agent-overpass-fallback` is the worked example: the
design was settled in a session that was still on `main`, and the branch session
after it only executed. Read every file on the ref, oldest first, before
concluding what the branch was for.

## Read it for what a diff cannot say

The digest is a transcript reduction, not a summary — it is long, and skimming it
for prose is the right approach. What to pull out:

- the user's original request, verbatim, near the top
- decisions and the alternatives that were rejected
- subagent reports (they arrive as `task-notification` turns and are usually the
  densest findings in the session)
- anything left explicitly unfinished or unverified

Tool calls appear as one `· ToolName {…}` line each. They are there for the shape
of the work, not to be read closely.

## When there is no ref

Then no session published one — the branch predates the hook, or the session was
killed before `Stop` fired. Say so rather than inventing intent, and fall back to
the commit bodies and the diff.

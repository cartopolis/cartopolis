---
name: finish-branch
description: Land a finished branch into main after reading the agent chat context that explains it. Use when a branch is done and being merged. The chat digest is kept, not destroyed.
---

# finish-branch

Land a branch, having first read the reasoning behind it.

The digest on `refs/agent-context/<branch>` **stays** after the merge. Keeping it
is the point: "why is this code like this?" outlives the branch by years, while a
commit body only ever holds what the author thought to write down at the time.

> This skill used to destroy the digest at merge time, from when digests were
> pushed into the public viberfox repo where anyone could read them. Moving them
> to the private `viberfox-agent-context` repo removed that reason. See
> [docs/notes/agent-context-refs.md](../../../docs/notes/agent-context-refs.md)
> for what the destruction machinery was and why it is now only a manual escape
> hatch.

## Order

**1. Read the context.** Run `branch-context` and confirm the branch actually is
finished: nothing left stubbed, the open questions answered, the verification the
session claimed actually run. A digest header saying `uncommitted at stop: 12
file(s)` means you are not looking at all the work that existed.

**2. Check what the branch was *for*.** A branch often drifts across several
sessions. The oldest session file usually holds the design decision; the later
ones only execute it. If what you are about to merge no longer matches that
intent, this is a conversation, not a merge.

**3. Merge.** Follow the `commit` skill's conventions for any merge or fixup
commit. Rebase or merge as the branch's shape warrants; get it into `main` and
pushed.

**4. Leave the context ref alone.** There is nothing to clean up. The ref stays
keyed on the branch name, which remains meaningful long after the branch itself
is deleted.

## Deleting one anyway

If a digest genuinely has to go — a credential slipped past the filter's
redaction, or something was pasted that should never have been typed — it takes
both halves, and neither alone is enough:

```bash
CTX=ssh://git@code.garage44.eu:2222/jeroen/viberfox-agent-context.git
git push "$CTX" --delete refs/agent-context/<branch>   # server prunes on receipt
git update-ref -d refs/agent-context/<branch>          # then this clone
git reflog expire --expire-unreachable=now --all
git gc --prune=now
```

The server's `post-receive.d/prune-agent-context` hook runs `git gc --prune=now`
when it sees such a deletion, which is what makes the objects unretrievable
rather than merely unreachable. Measured: it completes within about 2 s.

**Do not** delete the digest *file* in a new commit and call it removed — the
content stays fully readable in the ref's history. And do not skip the local
half: two stores, two prunes.

If the reason is a leaked credential, **rotate it first**. Deleting a digest does
not un-leak anything that was already fetched.

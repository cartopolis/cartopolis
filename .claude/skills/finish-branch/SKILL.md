---
name: finish-branch
description: Merge a finished branch into main and destroy its agent chat context — delete refs/agent-context/<branch> so the server prunes it, and prune the local clone too. Use when a branch is done and being landed.
---

# finish-branch

Land a branch and make its chat digest unretrievable in the same operation. The
digest is fine to read while the branch is open; once the work is in `main` it
has to go, and "gone" means pruned, not merely unreachable.

Read [docs/notes/agent-context-refs.md](../../../docs/notes/agent-context-refs.md)
first if anything below looks like it could be skipped — each step is there
because the cheaper version was measured and did not work.

## Order

**1. Read the context before destroying it.** Run `branch-context` and confirm
the branch actually is finished: nothing left stubbed, the open questions
answered, the verification the session claimed actually run. If it is not
finished, stop here — this skill deletes the reasoning.

**2. Merge.** Follow the `commit` skill's conventions for any merge or fixup
commit. Rebase or merge as the branch's shape warrants; get it into `main` and
pushed.

**3. Delete the context ref.** This is the step that triggers destruction — the
server's `post-receive.d/prune-agent-context` hook detaches a
`git gc --prune=now` when it sees an `refs/agent-context/*` deletion:

```bash
git push ssh://git@code.garage44.eu:2222/jeroen/viberfox-agent-context.git \
    --delete refs/agent-context/<branch>
```

Note the remote: the digests live in the private `viberfox-agent-context` repo,
not in viberfox. Deleting from `origin` would silently succeed at nothing.

**4. Prune this clone too.** The server hook cannot reach your objects. If this
clone ever fetched the namespace, it still holds them:

```bash
git update-ref -d refs/agent-context/<branch>
git reflog expire --expire-unreachable=now --all
git gc --prune=now
```

The `reflog expire` is only needed locally — a working clone does have reflogs
that a bare server repo does not.

**5. Verify, don't assume.** Confirm the ref is gone from the context repo, and that a
digest blob you noted in step 1 is no longer readable:

```bash
git ls-remote ssh://git@code.garage44.eu:2222/jeroen/viberfox-agent-context.git \
    'refs/agent-context/*'
git cat-file -e <blob-from-step-1> && echo "STILL PRESENT" || echo gone
```

Measured, the server prune completes within about 2 s. If the blob is still
present after ~10 s, the hook did not fire — check
`journalctl -t agent-context-prune` on the VPS before concluding it worked.

## Do not

- **Do not** delete the digest *file* in a new commit and call it removed. The
  content stays fully readable in the ref's history; measured.
- **Do not** rewrite `main`'s history to remove it. It was never in `main` —
  that is the entire point of the separate ref, and a rewrite would change SHAs
  and force everyone to re-clone for no benefit.
- **Do not** skip step 4 because the server said gone. Two stores, two prunes.

## What this cannot undo

Backups taken while the branch was open still contain the digest. That holds for
any storage choice and is accepted, not solved.

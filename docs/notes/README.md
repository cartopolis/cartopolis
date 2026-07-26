# Notes

Standing facts about this codebase that neither the code nor `git log` holds.

**Not** a decision log. Replaced the numbered ADRs — see
[retired/adr-system.md](retired/adr-system.md) for why, and don't re-propose them.

## Commit or note?

Ask: *will this still be true after the next ten commits?*

- **No — it explains one diff** → commit body. Frozen with the code it describes.
  The `commit` skill enforces this.
- **Yes — it constrains all future work** → a note here. Edited in place when it
  stops being true.

Four things belong here and nowhere else: **measurements** (with date and method),
**rejected options** (with the reason they lost), **external-world constraints**
(upstream policy, licence, platform limit), and **directions abandoned without a
diff** — the last have no commit to attach to at all.

## What does not belong

- What the code says.
- What a commit says — why a change was made.
- Plans and proposals. Those are issues until they land.
- **Anything a code change would falsify.** That is a comment, next to the thing.
  Example: the wasm `IoTaskPool` rule lives at `lod22.rs:349`, not here.

## Format

One topic per file, named for the topic. Free prose — no mandatory sections; a
template is a shape you fill when you have nothing to say. The only structure:

```yaml
---
topic: routing
triggers: [routing.rs, navigation.rs, "A*", "turn list"]   # when to read this
updated: 2026-07-26
---
```

A claim may carry a `check:` line — a shell one-liner that fails when the claim
goes stale (e.g. `check: ! grep -rqi "osrm" Cargo.toml`). **No runner exists
yet**; they are written for the CI job that will run them.

If a note passes ~100 lines it is probably narrating. Cut it.

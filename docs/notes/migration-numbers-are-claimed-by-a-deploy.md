---
topic: migration-numbers-are-claimed-by-a-deploy
triggers: [refinery, migrations, "V1", "add a migration", deploy-sim, "refinery migrate", "divergent", "MissingVersion", world.db, schema, crates/simulator/migrations, deploy-sim.yml, check-numbers.ts, crates/simulator/src/db.rs]
updated: 2026-10-17
---

# A migration number is claimed by a deploy, not by a merge

The next free `Vnn__` is **not** the one after the highest file in
`crates/simulator/migrations`. It is the one after the highest version in the
*production database*, and those two numbers can differ — because
`deploy-sim.yml` is a `workflow_dispatch` and can be dispatched from any ref, so
a branch that is nowhere near `main` can migrate the live schema.

This took the public simulator down for about ten minutes on 2026-08-29, and the
sequence is worth reading once because git gives no warning at any step:

1. A branch adding `V11__prim_particles` and `V12__prim_origin` was deployed to
   production while unmerged. `refinery` applied both to `world.db`.
2. A different branch, cut earlier and carrying its own `V11__account_email` and
   `V12__house_claims`, merged to `main` and deployed.
3. `refinery` compared applied version 11 against the file on disk, found two
   different migrations wearing one number, and refused to start. `Restart=always`
   crash-looped the service and every client got a 502 from Caddy.

Both branches merged clean in git, because the filenames differ and neither
touched the other's file. It is the same shape as the `PROTOCOL_HISTORY`
collision (`wire-protocol-versions.md`), with one extra twist: the wire
version collides at *merge*, and this one collides at *deploy*.

## Renumbering afterwards does not fix it

The obvious repair — move the losing migration to `V13`/`V14` — does not work,
and the reason is worth knowing before trying it under pressure. `refinery`'s
`Runner` defaults to **both** `abort_divergent: true` and `abort_missing: true`
(`refinery-core/src/runner.rs`), and `verify_migrations` walks the applied list
first: an applied version with *no* file at that number is `Kind::MissingVersion`
and a hard error. So renumbering only trades "applied migration V11 is different
than filesystem one V11" for "V11 is missing from the filesystem". The service
stays down.

What that leaves, once two migrations already share a number in a live database:

- **Roll the loser out of the database** — delete its `refinery_schema_history`
  rows and reverse its DDL — so the winner's files apply cleanly at those
  numbers. Only safe when the loser is additive (`ADD COLUMN`, `CREATE TABLE`)
  and nothing has written to what it added. This is what was done here.
- **Put the loser's migration files on the winner's branch, byte for byte**, and
  renumber the winner above them. The checksums must match what was applied or
  `abort_divergent` fires. Costs `main` a schema change whose code is elsewhere,
  which is harmless for a nullable column and is not harmless in general.
- **Redeploy the loser's ref** to restore service, and sort the numbering out
  afterwards. The fastest way back up, and it leaves the same trap armed.

## So, before adding a migration

Read the live history, not the directory listing:

    ssh host 'sudo sqlite3 /data/cartopolis/sim/data/world.db \
      "select version, name from refinery_schema_history order by version desc limit 5;"'

A gap in the on-disk numbering is fine — `refinery` applies whatever it finds
with a version above the current one, so a fresh database is happy with
`1..10, 13, 14`. Two files sharing a number never is.

## What now catches it (2026-10-07)

Two guards, one at each end, because the clash can be made at either:

- **Landing.** The landing runs `tools/check-numbers.ts` (cartopolis's
  `pre_land` at the midas hub) before it rebases. It refuses a branch that added a `Vnn__` main has since
  added as something else, and — when `ssh host` works — one whose `Vnn__`
  disagrees with the live `refinery_schema_history` or sits below its highest
  version. The same script checks `PROTOCOL_HISTORY` revisions and
  `MessageKind` numbers, which collide the same way.
- **Deploy.** `deploy-sim.yml` migrates a *copy* of the live database with the
  new binary before publishing it. Divergent or missing fails the job there, and
  the old binary keeps serving instead of crash-looping.

Neither stops somebody deploying an unmerged branch; they make the cost of
having done so a refused landing or a refused deploy, not an outage.

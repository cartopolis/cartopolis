# ADR-016: Authentication and Server Session Model (v0)

---
**Metadata:**
- **ID**: ADR-016
- **Status**: Proposed
- **Date**: 2025-03-21
- **Tags**: [auth, session, security, networking, protocol, cli, ops]
- **Decision Type**: architecture_pattern
- **Aligns with**: [G-05, G-03, P-04, P-02]
- **Roadmap tier**: P0
- **Doc resolution**: R2
- **Related**: [ADR-007, ADR-008, ADR-009, ADR-013, ADR-014]
- **Supersedes**: []
- **Superseded By**: []
---

## Context

**Problem**: [Vision](../abstract/vision.md) ties **governance** (G-05) and **mutating security** (P-04) to **known identity** before uploads, world edits, or social sends. ADR-009 already gates **world traffic** on a **Handshake → Ack**, but it does not define **what “proof” means**, **who stores sessions**, or how a **principal** (stable account id) is bound to a TCP connection. Without that, implementers cannot wire auth consistently across `vibe_sim` and clients.

**Requirements**:
- **Single authority**: the simulation server **issues and validates** sessions for v0 (aligns with P-01 / authoritative server story).
- **Composable with ADR-009**: handshake carries **credentials**; Ack carries **session id** and **principal id** (or error).
- **Simple v0 UX**: the **client binary** accepts **username and password** (CLI flags or equivalent) and uses them to populate the handshake — no separate login UI required for early iteration.
- **Server-side account admin**: the **sim binary** (or dedicated **`vibe_sim` subcommand**) exposes **CLI** operations to **create**, **edit**, and **delete** local users against the same credential store as the handshake — no separate admin UI for v0.
- **Rust CLI stack**: use **mainstream ecosystem crates** for parsing and operator UX (**`clap`**, companions below)—no ad hoc `std::env::args` parsing.
- **Simple v0 authorization**: after successful auth, **all principals share the same mutation rights** (no roles, ACLs, or per-resource policy yet). A later ADR can introduce **AuthZ** without changing the session model.
- **Versioning**: auth handshake fields evolve under **protocol_version** (ADR-009, P-02). **OIDC, API keys, mutual TLS** stay out of scope until a follow-up ADR.

## Decision

**v0 model**: After TCP connect (ADR-008), the client completes an **authentication handshake** inside the ADR-009 envelope. The server verifies **credentials**, creates a **server-side session**, and returns an **opaque session id** plus a **stable principal id**. Until Ack succeeds, the server **must not** send world snapshot or accept **mutating** application messages (read-only probes if any are defined stay explicitly whitelisted in spec).

**Approach**:
- **Client CLI**: parse args with **`clap`** (derive API, same as ADR-014). Expose **username** and **password** via flags (e.g. `--user`, `--password`) before opening the TCP connection; exact names in generated help. **Note:** passwords on argv are visible in OS process listings; acceptable for **lab/dev** only — prefer **`VIBE_PASSWORD`** (or similar) **environment variable** when you need slightly better hygiene on shared machines; use **`dialoguer`** (e.g. `Password` prompt) when neither flag nor env is set, if you want a safe default path.
- **Wire**: the handshake payload carries **username** plus **password** (or a derived proof if we add challenge–response later). Server verifies against a **local credential store** (hashed password in SQLite per ADR-002/013, or a minimal server config file for smoke tests — pick one path in implementation and document it). Prefer **`argon2`** (or **`bcrypt`**) via the **`password-hash`** / **`argon2`** crates for storage; **`zeroize`** on structures that hold plaintext passwords **in memory** until hashing or send completes.
- **Server CLI (user CRUD)**: implement **create**, **edit**, and **delete** as **`clap`** subcommands (e.g. `user add`, `user passwd`, `user rm` — exact spelling in derive + `--help`). Same **dialoguer** / env / argv pattern for secrets as the client. **`clap_complete`** (optional) can generate shell completions for packaged binaries. Operator-facing errors at the **binary** boundary may use **`miette`** or **`color-eyre`** for readable reports; keep **`thiserror`** in libraries (ADR-008 layering). Commands **mutate the credential store only** through the **single server writer** path (ADR-013): no hand-editing the DB file while the sim is running. **Create**: username + password. **Edit**: at minimum **password change**; optional **username rename** if the schema supports it without breaking foreign keys. **Delete**: remove the account record; document behavior for **active sessions** (v0: connection remains until disconnect; new handshakes fail) and for **owned world data** (defer detailed cascade rules unless already modeled).
- **Principal**: stable internal identifier derived from the account record (e.g. UUID or row id), not the display username string alone if usernames can change later.
- **Session**: binds **one TCP connection** to **one principal** for its lifetime. **v0**: session ends when the connection closes; **no** mid-connection re-auth or token refresh in this ADR.
- **Session id**: opaque (UUID per ADR-009 recommendation); **server-side** only (in-memory table for dev; persistent sessions optional under ADR-013).
- **Authorization (v0)**: **no per-user policy** — if the handshake succeeds, the connection is **trusted for all v0 mutating message kinds** the server exposes. **Roles, asset ACLs, region powers** are explicitly deferred.

**Rust ecosystem (CLI)** — use **`clap`** (derive) on **both** client and sim; **`dialoguer`** for interactive secrets and destructive confirms; optional **`clap_complete`**; **`miette`** or **`color-eyre`** at binaries; **`zeroize`** for sensitive in-memory buffers; **`argon2`** + **`password-hash`** (or **`bcrypt`**) for stored password hashes. **Config and logging** stay on **ADR-014** (**`figment`**, **`tracing`**).

**Explicit non-decisions (v0)**:
- **OAuth/OIDC** browser redirects and token exchange — defer to a follow-up ADR when a client shell needs it.
- **Self-service account lifecycle** (email verify, forgot-password flows, MFA enrollment) — not required for v0; **operator-driven CLI** covers bootstrap and password rotation.
- **Fine-grained authorization** — separate ADR when mutating surface grows beyond “one trust level”.

## Solo/dev mode and process model

Adding auth must not change the dev DX: `cargo run -p viberfox --profile dev-bevy`
still launches a working sim+viewer with one command. This is achieved without a
solo-only code path:

- **One client path.** The client's only entry point is `connect(addr, credentials)`.
  Solo mode supplies its inputs (a loopback address + a dev account); `--connect`
  supplies a remote address + `--login` credentials. Everything downstream —
  handshake, auth, snapshot apply — is identical, so external connect stays
  first-class and always-tested.
- **In-process sim over loopback TCP.** `vibe_sim` is split into a **lib**
  (`serve(config) -> SimHandle`, `ensure_account(...)`) + a thin bin (ADR-015). In
  solo mode `viberfox` runs `serve` on a background tokio runtime bound to
  `127.0.0.1:0` (ephemeral), reads back `SimHandle::local_addr`, and connects to it
  as a normal authenticated client. **No in-memory bypass** of the socket or the
  auth handshake — that would create a divergent, rotting path. `tokio`/`rusqlite`
  are already client deps, so the added weight is small.
- **Feature-gated.** A default-on `solo` cargo feature pulls the `vibe_sim` lib into
  `viberfox`. Release/lean client builds turn it off and require `--connect`.
- **Auto-provisioned dev account.** In solo mode the client calls the sim lib's
  `ensure_account` (idempotent) at startup, so the real argon2 handshake runs every
  launch with no manual `user add` and no "skip auth in solo" shortcut.
- **Modes.** Default = solo (spawn + auth + connect); `--connect host:port` =
  external sim; `--offline` = today's no-sim local-SQLite path (no auth); `--shot`
  implies `--offline` so the screenshot harness stays sim-free.
- **`--login`.** Credential source that avoids an interactive prompt: `--login <FILE>`
  (gitignored TOML: `server?`, `user`, `password`|`token`) or inline `--login user:pass`;
  a default path is checked when the flag is omitted. Combined with **session-token
  persistence** in the user store, the password is entered at most once — later runs
  reuse the token.
- **Lifecycle.** In-process teardown is tied to Bevy `AppExit`: dropping `SimHandle`
  aborts the accept/tick tasks. (The `SimHandle` Drop already does this.)
- **Identity feedback.** `ServerHelloAck` echoes the authoritative `username`
  alongside `principal_id`; the client stores it in an `AuthIdentity` resource and
  the HUD shows a profile chip (avatar initial + name). This is the seed the chat
  presence UI (ADR-028) builds on.
- **Token persistence + fallback.** On a successful ack the client persists the
  issued `session_token` per server in the user store (external sims only — solo's
  loopback port is ephemeral). Reconnects present the token first and fall back to
  the password if it is stale (e.g. the sim restarted and dropped its in-memory
  sessions), so a stale token never locks the user out.

## Rationale

**Primary Reasoning**:
1. **Roadmap order** ([roadmap.md](../abstract/roadmap.md)): authentication before **asset upload** and **authorization** reduces rework on hot paths.
2. **Fits existing gate**: ADR-009 already promises “no world snapshot before successful handshake”; this ADR defines **successful** in security terms.
3. **P-04**: Mutating actions attach to **principal** at the sim; **v0 policy** is binary (authenticated or not), which is enough until asset and social paths need **separation of duties**.
4. **CLI-first** matches **headless sim + developer client** (ADR-007) before graphical login or admin UIs exist.
5. **Operator ergonomics**: scripts and deploys can **provision users** without SQL or a separate tool.

**Alternatives Considered**:

| Alternative | Pros | Cons | Rejected Because |
|------------|------|------|------------------|
| Stateless JWT only, sim verifies signature | Horizontally scalable | Weak **revocation** and **session invalidation** for v0 | Sim is single writer for v0; server-side session is simpler and safer to reason about |
| Mutual TLS as sole identity | Strong transport binding | Heavy **ops** for desktop/lab clients | Defer; optional later as additional proof kind |
| Per-message HMAC with shared secret | No session store | Key rotation and UX cost; easy to get wrong | Session + handshake is clearer for game-style clients |
| Piggyback on external IdP only (no local principals) | Enterprise fit | Blocks **offline/lab** and early iteration | v0 needs a **local** path; federated auth layers on top |

## Consequences

**Positive**:
- Clear hook for **rate limits** and **audit logs** (principal id on each mutating handler).
- Client and sim share one **envelope story** (ADR-009); no parallel “side channel” auth protocol.
- **User CRUD on the sim** keeps deployment self-contained (ADR-014 CLI stack).

**Negative**:
- **Server memory / DB** for session store (mitigation: TTLs, bounded table size, SQLite for restarts if needed).
- Handshake payload **must be versioned** carefully (mitigation: feature bits in handshake per ADR-009 discipline).

## Related

- [ADR-007](./007-simulation-vs-client-process-model.md): Sim owns connection lifecycle
- [ADR-013](./013-sqlite-migrations-and-server-writer.md): Persistent accounts / sessions follow single-writer SQLite discipline when enabled
- [ADR-014](./014-runtime-configuration-and-operations.md): `clap` + config layering for sim CLI surface
- [ADR-008](./008-network-transport-layer.md): TCP session scope
- [ADR-009](./009-application-protocol-envelope-v0.md): Envelope, handshake ordering, UUID session ids — **this ADR specializes** the auth semantics of that handshake

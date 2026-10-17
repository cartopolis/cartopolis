---
topic: wire-protocol-versions
triggers: [crates/core/src/protocol.rs, PROTOCOL_HISTORY, PROTOCOL_VERSION, ProtocolRevision, MessageKind, UnsupportedVersion, check-numbers.ts, "protocol version", "wire revision", "protocol bump"]
updated: 2026-10-17
---

# The wire version is the last row of a changelog, and nothing else

`cartopolis_core::protocol::PROTOCOL_HISTORY` is the wire's changelog: one
`ProtocolRevision { version, adr, summary }` per revision. `PROTOCOL_VERSION` is
*derived* from its last entry, so **bumping the version is appending a row** —
the number cannot move without a sentence saying why. It travels in every
frame's 8-byte header (`protocol_version` u16 LE, `message_kind` u16 LE,
`request_id` u32 LE, then the postcard payload; ADR-008/009), and
`decode_app_frame` refuses any other version with
`ProtocolError::UnsupportedVersion`.

**Do not write the number anywhere else.** It was, and it rotted: three
documents said "currently 8" while the wire had moved to 11, which is why the
constant became derived. Prose names `PROTOCOL_HISTORY`, never a value. The log
starts at v8 (ADR-018), the oldest revision with a written record; no client
speaking v1–v7 exists.

`protocol_history_is_contiguous_and_current` keeps the run gapless and
ascending and every row summarised — a duplicated or skipped number would let two
schemas answer to one version.

What is and is not a revision:

- **A new carrier is not.** The WebSocket carrier moves the same frame body, so
  `PROTOCOL_HISTORY` gained no row for it (`hosted-simulator.md`).
- **Operator actions are not on the wire at all** — they are HTTP, so the admin
  surface changing never moves every client's version (`hosted-simulator.md`).
- **Anything a player's client must send or understand is.** Account creation is
  absent from the wire, so offering it in the app would be a revision, not a UI
  change (`session-modes.md`).

**Two branches each appending the next number collide at merge**, the way
migrations collide at deploy (`migration-numbers-are-claimed-by-a-deploy.md`).
`.claude/skills/finish-branch/check-numbers.ts` checks `PROTOCOL_HISTORY`
versions and `MessageKind` discriminants against `main` before landing.

**A deployed server behind `main` refuses newer clients.** Deploys are manual
(`midas deploy sim`), so the server drifts; a client one revision ahead fails to
sign in with "Connection reset without closing handshake", and the server's
journal says `unsupported protocol version`.

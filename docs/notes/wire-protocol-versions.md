---
topic: wire-protocol-versions
triggers: [crates/core/src/protocol, wire_shape.txt, CARTO_BLESS_WIRE, PROTOCOL_HISTORY, PROTOCOL_VERSION, ProtocolRevision, MessageKind, UnsupportedVersion, check-numbers.ts, "protocol version", "wire revision", "protocol bump"]
updated: 2026-11-19
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

**The payload's shape is pinned, the header's numbers are not enough.**
postcard sends a `NetMessage` variant as its position in the enum and a struct
as its fields in order, so moving a variant or swapping two fields compiles,
round-trips in every same-build test, and makes every deployed peer misread the
message. Nothing caught that until 2026-11-19: `MessageKind`'s explicit numbers
guard the header only. `protocol::wire_shape` now traces every type reachable
from `NetMessage` (and `AvatarLook`, which the server keeps as postcard) into
`crates/core/src/protocol/wire_shape.txt`, one line per numbered variant or
field, and fails on any difference. A meant change re-pins it with
`CARTO_BLESS_WIRE=1 cargo test -p cartopolis_core wire_shape`, beside a
`PROTOCOL_HISTORY` row. A new nested enum fails the trace as
`MissingVariants([...])`; add it to the `enums!` list there.

**Two branches each appending the next number collide at merge**, the way
migrations collide at deploy (`migration-numbers-are-claimed-by-a-deploy.md`).
`tools/check-numbers.ts` checks `PROTOCOL_HISTORY`
versions and `MessageKind` discriminants against `main` before landing.

**A deployed server behind `main` refuses newer clients.** Deploys are manual
(`midas deploy sim`), so the server drifts; a client one revision ahead fails to
sign in with "Connection reset without closing handshake", and the server's
journal says `unsupported protocol version`.

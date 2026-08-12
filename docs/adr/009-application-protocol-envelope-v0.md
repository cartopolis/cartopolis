# ADR-009: Application Protocol Envelope (v0)

---
**Metadata:**
- **ID**: ADR-009
- **Status**: Accepted
- **Date**: 2025-03-21
- **Tags**: [protocol, networking, api]
- **Decision Type**: architecture_pattern
- **Related**: [ADR-008, ADR-010, ADR-011, ADR-016]
- **Supersedes**: []
- **Superseded By**: []
---

## Context

**Problem**: Framed TCP (ADR-008) needs a **versioned, extensible** inner format so client and sim can evolve.

**Requirements**:
- **Protocol version** negotiation or rejection
- **Message kind** (discriminant) + **correlation** where needed
- **Session gate**: no world snapshot before successful handshake
- **Small v0 surface**: not a full Entity–Component–Attribute schema

## Decision

Every TCP frame body (ADR-008 length-delimited) is an **app frame**: an 8-byte header + postcard payload.

**Header** (`crates/cartopolis_core/src/protocol.rs`, all little-endian):
- `protocol_version: u16` — `PROTOCOL_VERSION`, derived from the `PROTOCOL_HISTORY` changelog in `crates/cartopolis_core/src/protocol.rs` (**the** record of what each revision changed — this doc deliberately does not repeat the number); `decode_app_frame` rejects mismatches (`ProtocolError::UnsupportedVersion`).
- `message_kind: u16` — discriminant of the `MessageKind` enum (`#[repr(u16)]`, `ClientHello`…); mismatch vs the decoded body is rejected.
- `request_id: u32` — correlation id (echoed in `ServerError`); 0 when unused.

**Body**: postcard-encoded `NetMessage` (the rest of the frame — the transport already delimits length, so no inner length prefix). `serde` derives on every message; `uuid::Uuid` for session ids; `thiserror` (`ProtocolError`) for typed decode errors.

**Handshake / gate**: `ClientHello { protocol_version, client_token }` → `ServerHelloAck { session_id, tick_hz, your_avatar_id, osm_tile_url_template }` (or `ServerError`) before any `WorldSnapshot`/`ClientIntent`. Auth (real credential verification) is **not built** — `client_token` is an opaque placeholder for v0.

## Rationale

**Primary Reasoning**:
1. **Evolution**: Version + kind allow adding messages without new transports.
2. **Debugging**: `request_id` links errors to client actions.
3. **Simple gate**: handshake-before-scene, without a full EC-attribute schema.

**Alternatives Considered**:

| Alternative | Pros | Cons | Rejected Because |
|------------|------|------|------------------|
| JSON-only messages | Human-readable | Bandwidth + speed | Poor fit for snapshots |
| Protobuf / gRPC | Schema tooling, multi-lang | Heavier codegen, HTTP/2 stack | Defer until multi-language clients |
| Bincode instead of postcard | Slightly faster | Less `no_std`-friendly, larger | Postcard is compact and deterministic enough |

## Consequences

**Positive**:
- Clear extension point for ADR-010/011 message kinds
- Testable with golden vectors per message kind

**Negative**:
- Every new kind needs versioning discipline (mitigation: append a `ProtocolRevision` to `PROTOCOL_HISTORY`, which *is* the version bump — a test enforces the run stays gapless)
- Wire schema lives in one `cartopolis_core` module; roundtrip tests per kind guard it

## Related

- [ADR-008](./008-network-transport-layer.md): Frames carry this envelope
- [ADR-010](./010-authoritative-avatar-state-v0.md): Intent message kinds
- [ADR-011](./011-static-world-replication-v0.md): Snapshot / delta kinds

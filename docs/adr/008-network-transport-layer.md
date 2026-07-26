# ADR-008: Network Transport Layer (v0)

---
**Metadata:**
- **ID**: ADR-008
- **Status**: Accepted
- **Date**: 2025-03-21
- **Tags**: [networking, transport, server]
- **Decision Type**: technology_choice
- **Related**: [ADR-007, ADR-009]
- **Supersedes**: []
- **Superseded By**: []
---

## Context

**Problem**: Client and simulation processes (ADR-007) need a **reliable, debuggable** byte pipe for application messages (ADR-009).

**Requirements**:
- Works **headless** on server
- **Framed** messages (length-delimited or stream-safe)
- Rust ecosystem fit; no heavyweight networking framework dependency
- Room to add **TLS** or **WebSocket** front-end later without rewriting app messages

## Decision

Use **TCP** with **length-prefixed frames** (little-endian `u32` length + payload) as the **v0 transport** between `vibe_sim` and `viberfox`.

**Approach**:
- One connection per client session; sim is listener.
- Frame = `length || payload`; payload interpreted by ADR-009 envelope.
- Document max frame size to bound memory.
- **Later**: Optional QUIC or WebSocket **terminator** in front of the same logical messages (not v0).

**Rust ecosystem (preferred stack)**:
- **`tokio`** (`net`, `io-util`, `macros`, `rt-multi-thread`) for async accept/read/write on the sim and client network tasks.
- **`tokio-util::codec::LengthDelimitedCodec`** (configure **max frame length** and length field width/endianness to match the written spec) for **battle-tested framing** instead of hand-rolled `read_exact` loops.
- **`bytes::BytesMut`** (pulled in via tokio/tokio-util) for zero-copy-ish buffer reuse where it matters.
- Errors: **`thiserror`** in `vibe_core` / library boundaries; **`anyhow`** (or `eyre`) only at **binary** `main` for top-level reporting—standard Rust layering.
- Future **TLS** on TCP: **`tokio-rustls`** + **`rustls`** (ring/aws-lc per platform policy), not OpenSSL via FFI unless required.

## Rationale

**Primary Reasoning**:
1. **Reliability**: World snapshots and ordered setup match TCP semantics for v0.
2. **Simplicity**: No custom ACK layer in the first iteration.
3. **Framing**: Same *spirit* as discrete messages in [TundraMessages.h](https://github.com/realXtend/tundra/blob/master/src/Core/TundraProtocolModule/TundraMessages.h) (opcode lives inside payload per ADR-009).

**Alternatives Considered**:

| Alternative | Pros | Cons | Rejected Because |
|------------|------|------|------------------|
| UDP + custom reliability | Lower latency | More code | Defer until profiling says TCP is bottleneck |
| QUIC (**`quinn`**) | Modern, built on **tokio** | Heavier dep for v0 | Revisit when TLS + NAT traversal matter |
| WebSocket v0 (**`tokio-tungstenite`**) | Browser-friendly | Extra framing + deps in sim | Defer to dedicated gateway ADR |

## Consequences

**Positive**:
- Easy to inspect with tcpdump / simple test harness
- Aligns with the mainstream **async Rust** stack (**tokio** + **tokio-util** codecs)

**Negative**:
- Head-of-line blocking under packet loss (mitigation: acceptable for v0; measure later)
- No browser client without a bridge (mitigation: future WebSocket adapter)

## Transport evolution (TCP+TLS → QUIC)

Decided path, so the trigger is on record before auth (ADR-016) builds on it:

1. **v0 — TCP + TLS.** `tokio-rustls` wraps the stream; TLS 1.3 is ~free (1-RTT
   handshake, ~22 B/record, AES-NI crypto). TLS is **not** the efficiency cost —
   TCP **head-of-line blocking under packet loss** is, and it only bites at
   `entity_count × update_rate × loss_rate`, all small for a capacity-bounded sim.
   Fine for loopback/LAN/healthy internet.
2. **Envelope tags reliability class per message kind** (ADR-009) now, at no cost:
   `ClientIntent`/`WorldSnapshot` = *unreliable-capable* (latest-wins, obsolete on
   supersede); prim CRUD / chat / textures = *reliable-ordered*. This is the seam
   that lets the position stream move off TCP later without touching message defs.
3. **Instrument** observed RTT + loss. That measurement — not a guess — is the
   trigger.
4. **When loss-latency bites → QUIC (`quinn`).** Independent streams (no cross-
   stream HoL), unreliable datagrams (RFC 9221) for the position stream, TLS 1.3
   intrinsic (retires the TLS question) — a standardised transport, so we do
   **not** hand-roll UDP reliability.

Bigger efficiency levers come first and are transport-agnostic: AoI (have it),
delta/dirty-field updates (ADR-011), position/rotation quantization. Transport is
not where the efficiency story starts.

## Related

- [ADR-007](./007-simulation-vs-client-process-model.md): Sim owns the listener
- [ADR-009](./009-application-protocol-envelope-v0.md): Payload format inside frames
- Tundra-Urho3D [User and Programmer's Guide](https://github.com/realXtend/tundra-urho3d/blob/master/doc/User_and_Programmers_guide.md) — WebSocket binary framing as contrast

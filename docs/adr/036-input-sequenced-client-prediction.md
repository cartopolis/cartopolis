# ADR-036: Input-Sequenced Client Prediction and Reconciliation

---
**Metadata:**
- **ID**: ADR-036
- **Status**: Proposed
- **Date**: 2026-07-23
- **Tags**: [networking, protocol, prediction, avatar, latency, replication]
- **Decision Type**: architecture_pattern
- **Aligns with**: [G-01, P-02]
- **Roadmap tier**: P1
- **Doc resolution**: R2
- **Related**: [ADR-008, ADR-009, ADR-012, ADR-016]
- **Supersedes**: []
- **Superseded By**: []
---

## Context

**Problem**: the client is server-authoritative for avatar movement (ADR-016): it
sends `ClientIntent`, the sim integrates at `tick_hz` (20 Hz) and broadcasts
`WorldSnapshot`. The client predicts locally between snapshots, but has no way to
know *which* of its inputs a given snapshot already accounts for. Reconciliation
therefore compares a prediction that has consumed inputs up to now against an
authoritative position that has consumed inputs up to `now − (RTT + tick phase)`.

The first-order symptom — reconciling against the raw 20 Hz snapshot, so the
correction fought the prediction on every frame between ticks and produced a
20 Hz sawtooth in apparent speed — was fixed without a protocol change by
extrapolating the reconcile target by its own age
(`avatar::smooth_online_avatar_display`). That removes the pulsing but not the
underlying error: the extrapolation guesses with the *current* velocity, so every
direction change, wall slide, or server-side correction still resolves as a blend
rather than a replay. On loopback (solo mode) the residual is small; over a real
network it grows linearly with RTT.

**Requirements**:
- The client's displayed avatar responds to input in the same frame, at any RTT.
- A server correction (collision, teleport, anti-cheat clamp) converges without a
  visible slide, and without discarding inputs the player has made since.
- No new authority on the client: the sim stays authoritative (ADR-016).
- Per-connection rate limits (ADR-012) must not silently drop an input edge.

## Decision

Number client inputs, have the sim echo the last one it applied, and reconcile by
**replaying the unacknowledged tail** from the authoritative state.

**Approach**:

1. **Wire (`cartopolis_core`)** — a new revision appended to `PROTOCOL_HISTORY`
   (**v12**; the number is derived from that log, never written by hand):
   - `ClientIntent` gains `sequence: u32` — monotonic per connection, and
     `dt_ms: u16`, the client frame duration the intent covers.
   - `WorldSnapshot` gains `last_input_sequence: u32` (per receiving client;
     0 before the first intent).
   The ADR-009 envelope is unchanged: no new message kinds, two added postcard
   fields, both `#[serde(default)]`.

2. **Sim (`cartopolis_simulator`)** — `SimWorld` records the highest `sequence` applied per
   avatar and integrates each intent over its own `dt_ms` (clamped to a sane
   ceiling so a lying client can't buy speed), instead of holding one velocity
   until the next tick. `net.rs` stamps `last_input_sequence` per connection when
   encoding the snapshot — so the broadcast becomes a per-connection encode for
   avatars, or the field is carried in the local-avatar `AvatarStateDto`.

3. **Client (`cartopolis`)** — a ring buffer of `(sequence, velocity, dt)` for
   inputs sent and not yet acknowledged. On snapshot: drop everything at or below
   `last_input_sequence`, reset the predicted position to the authoritative one,
   then re-integrate the remaining tail. The result is the position the sim
   *will* report once it has caught up, so the residual error is real divergence
   only, and the existing smoothing handles it.

4. **Rate limits** — `MIN_INTENT_INTERVAL` must coalesce (keep the newest
   pending intent and apply it when the window opens) rather than `continue`
   past it: with sequence numbers a dropped intent leaves a permanent gap the
   client will keep replaying.

## Rationale

**Primary Reasoning**:
1. Replay is the only reconciliation that is *correct* rather than plausible: it
   reproduces the sim's own integration over exactly the inputs the sim has not
   seen yet, so at steady state the error is zero regardless of RTT.
2. The pieces are already in place — the envelope carries a `request_id`, the
   client already mirrors the sim's `apply_intent` scaling to predict, and the
   snapshot already goes out per tick. Sequencing formalises what the code is
   informally assuming.
3. It makes server corrections expressible. Today a sim-side clamp is
   indistinguishable from prediction drift; after this, a mismatch that survives
   replay is by definition a correction and can be snapped and logged.

**Alternatives Considered**:

| Alternative | Pros | Cons | Rejected Because |
|------------|------|------|------------------|
| Age-extrapolated reconcile target only (shipped) | No protocol change; kills the 20 Hz sawtooth | Guesses with the current velocity; error grows with RTT; can't distinguish drift from correction | Necessary but not sufficient — keep it as the smoothing layer under replay |
| Client-authoritative position | Trivially smooth, zero perceived latency | Abandons ADR-016's authority model; unfixable cheating | Contradicts the server-authoritative decision |
| Interpolate in the past (render the avatar behind the snapshot stream) | Simple, no prediction at all | Adds a full RTT + buffer of input lag to the *local* avatar | Acceptable for remote avatars (what `smooth_remote_avatars` does), not for the one the player drives |
| Raise `tick_hz` to hide it | No design work | Linear cost in CPU and bandwidth for every connected client; doesn't fix RTT | Treats the symptom, and the sawtooth was never really about tick rate |

## Consequences

**Positive**:
- Local avatar responds within a frame at any RTT; corrections converge exactly.
- Wall sliding (client-side collision, no building geometry in the sim) stops
  being a source of permanent predicted/authoritative divergence.
- A measurable divergence metric falls out for free — useful in `--dump-state`.

**Negative**:
- Breaking wire change: `PROTOCOL_VERSION` 11 → 12, and `decode_app_frame`
  rejects mismatches outright, so client and sim must be deployed together.
  Mitigation: solo mode embeds the sim in-process, so only external `--connect`
  deployments are affected.
- The snapshot becomes per-connection for the local-avatar field, weakening the
  single-encode broadcast. Mitigation: encode the shared body once and stamp the
  per-client field, or carry the ack in a small separate message.
- The client must keep the replay integration byte-for-byte in step with
  `SimWorld::apply_intent`/`step`. Mitigation: move the scaling constants into
  `cartopolis_core` so both sides read one definition — they are duplicated today
  (`network::SIM_WALK_SPEED` mirrors `state::WALK_SPEED` by comment only).

## Related

- [ADR-008](./008-network-transport-layer.md): Transport unchanged.
- [ADR-009](./009-application-protocol-envelope-v0.md): Envelope unchanged; this
  adds two postcard fields and appends a `PROTOCOL_HISTORY` revision.
- [ADR-012](./012-interest-management-and-osm-tiles.md): Per-connection rate limits must
  coalesce rather than drop once inputs are sequenced.
- [ADR-016](./016-authentication-and-session-model-v0.md): Preserves server
  authority; prediction is display-only.

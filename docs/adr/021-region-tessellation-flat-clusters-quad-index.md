# ADR-021: Region Tessellation — Flat Metric Clusters with a Global Quad Index

---
**Metadata:**
- **ID**: ADR-021
- **Status**: Superseded (direction retired)
- **Date**: 2026-06-19
- **Tags**: [coordinates, regions, addressing, sim, large-world, architecture]
- **Decision Type**: architecture_pattern
- **Aligns with**: [G-04, G-02, P-03, P-05]
- **Roadmap tier**: P2 — planet-scale world model
- **Doc resolution**: R2
- **Related**: [ADR-020, ADR-019, ADR-006, ADR-010, ADR-012]
- **Supersedes**: []
- **Superseded By**: [forthcoming capacity-bounded asset ADR]
---

> **Superseded direction.** The fixed-region-grid + cluster + S2-index model below is
> **being retired**. The sim tier is moving to a **capacity-bounded set of assets placed
> at any real-world position** (a simulator caps *how many* assets it hosts, not *where*
> they sit) — no fixed 256 m squares, no cluster tessellation, no per-region `(i,j)`
> addressing. The floating-origin frame (ADR-019) and the map/content separation
> (ADR-020) still hold. Retained for history and for the addressing ideas a future
> spatial-index/discovery design may reuse. A forthcoming ADR specifies the new model.

## Context

ADR-020 splits the world into a **map tier** (read-only globe) and a **sim tier** (discrete regions), and ADR-019 gives the precise global frame. One question was left open: **how exactly are sim regions tessellated and addressed on the globe?** Two candidates emerged:

- **(A) One global quad sphere** — cube-sphere/S2 cells stuck together worldwide *are* the regions.
- **(B) Flat metric clusters** — regions are true flat squares tiled in real meters within a locally-anchored area, with a global quad grid used only for *addressing*.

The ambiguity created downstream fuzziness: do regions warp (to wrap the sphere) or stay exact squares (leaving gaps)? Do region IDs come from geometry or from an index? How do on-demand ("activate a region") and a global grid coexist?

A sphere is not flat-developable (*Theorema Egregium*): you cannot have **flat + exact-square + globally-seamless** at once. (A) keeps seamlessness by warping cells; (B) keeps flat exact squares and accepts that seams fall *between* clusters.

## Decision

Adopt **(B)**: sim regions are **flat, fixed-size real-meter squares** tiled inside locally-anchored **clusters**; a **global quad index (S2-style) is used only for addressing, discovery, and neighbor lookup — never for geometry.**

This cleanly separates two concerns that (A) conflated:

| Layer | Job | Property |
|-------|-----|----------|
| **Geometry / simulation** | where things are, physics, movement | flat metric grid in a cluster's tangent plane |
| **Addressing / index** | stable IDs, "what's near here", neighbors | global quad cells (S2-style) |

### Definitions
- **Cluster** — a contiguous play area pinned to the globe by **one** lat/lng anchor + ENU tangent frame (ADR-020). Bounded in size so curvature is invisible (sag = (½·width)²/2R; ≤ ~7 km keeps sag < 1 m). The cluster is the unit of "perfectly flat and seamless".
- **Region** — a fixed real-meter square (e.g. 256 m) addressed by **integer (i, j) offsets** in its cluster's flat plane: `region(i,j)` origin = anchor + i·256·East + j·256·North. Neighbours share edges **exactly** because they're computed in one shared metric grid.
- **Global quad index** — each region (and cluster) is tagged with an **S2-style cell ID** derived from its anchor lat/lng. Used as the database key / network address, for spatial discovery ("regions near this point"), and for the rare **cross-cluster** neighbour lookup. It does **not** define region size, shape, or coordinates.

### Resolutions to the open questions
1. **Warp vs gaps → neither, inside a cluster.** Regions are exact squares and seamless *within* a cluster (shared metric grid). The unavoidable sphere seam lives **between clusters** — open water / unsimulated map — where a hairline discontinuity is irrelevant.
2. **IDs come from the index, not geometry.** Region geometry is the flat grid; identity/addressing is the S2 cell. The two are linked (anchor → cell) but independent in purpose.
3. **On-demand activation.** Quad cells are **virtual indices** until a region is activated ("convert a building" → instantiate the region). The global index gives every potential region a stable address *without* instantiating it, so sparse, on-demand activation and a global addressing grid coexist naturally.
4. **Cross-region movement.** Within a cluster: pure flat translation between `(i,j)` frames. Between clusters: a small frame handoff (tilt ≤ ~0.0023°/region) via the S2 index — the same handoff path multiplayer AOI needs (ADR-010/012).

### Worked example — positioning a sim and its neighbour

Anchor a Groningen cluster at the Grote Markt (**53.2194°N, 6.5665°E**):
- **cluster id** = S2 cell at L12 (~1.9 km) → token `47a1d3` *(illustrative)*; **frame** = ENU tangent plane at that anchor.
- **region (i,j) origin** = `anchor + i·256·East + j·256·North`; **region key** = `47a1d3:i:j`.

A sim over part of Groningen = a block of regions, e.g. **Sim A = (0,0)…(3,3)**.

**Adjacent sim, same cluster (the common case).** A neighbouring **Sim B = (4,0)…(7,3)** to the east is just the next block. Both sims share the cluster's *one* metric grid, so B's west edge (`i=4`) coincides with A's east edge to the millimetre — **no transform, no rotation, no handoff**; positioning is pure integer arithmetic:
```
adjacent(A, B)  ⇔  A.i_max + 1 == B.i_min   (and j-ranges overlap)
B region (4,j) origin = anchor + 4·256·East + j·256·North   ← continues A's grid exactly
```

**Adjacent sim, different cluster (only past the sag bound ~7 km, or another town).** Sim B gets **its own anchor + ENU frame** and its own S2 cluster cell (e.g. `47a1d4`). A and B no longer share a plane, so they're related by a **rigid transform `T_AB`** derived from the two anchors (translation + a tilt of ≤ ~0.0023° per region of separation), discovered via **S2 cell adjacency**. Crossing the border is the AOI handoff (ADR-010/012): the avatar's local coords map `A-frame → B-frame`.

**Rule of thumb:** keep extending **one** cluster (trivial integer adjacency) until the sag budget forces a new one; place cluster borders on water/unsimulated terrain so the only non-flat seam is where nothing is simulated.

## Rationale

1. **Keeps the sim flat and globe-agnostic.** No geodesy or sphere math in `vibe_sim`; it simulates flat squares in local meters, exactly as today (P-05).
2. **True squares preserve the SL/OpenSim model** — fixed-size rectangular regions/parcels with consistent X/Y axes and north, which authoring and gameplay assume.
3. **Matches islands-of-simulation reality.** Viberfox visits cities with ocean between; global seamless tiling buys little, and (B) puts the only seams where nothing is simulated.
4. **Global index gives the benefits of (A) without its costs.** Stable worldwide addressing, discovery, and neighbour queries — without warped cells or sphere math in the sim.

**Alternatives Considered**:

| Alternative | Pros | Cons | Rejected Because |
|------------|------|------|------------------|
| (A) Global quad sphere as regions | Globally seamless; intrinsic neighbours | Warped/non-uniform cells; sphere math in sim; breaks square-parcel model | Cost not justified for islands-of-sim |
| lat/lng grid as regions | Simple addressing | Curvilinear → unequal, non-aligning squares | Seams everywhere (ADR-020 anti-pattern) |
| Flat clusters, no global index | Simplest | No stable worldwide IDs / discovery / cross-cluster neighbours | Needed for persistence & multiplayer |

## Consequences

**Positive**:
- Regions are exact, seamless squares where it matters (inside a cluster); flat sim math is always valid.
- Stable global addressing + discovery + neighbour lookup via the quad index.
- On-demand activation works without a pre-instantiated global grid.
- `vibe_sim` stays flat and unchanged in spirit.

**Negative**:
- **Two identifiers per region** (cluster-local `(i,j)` + global S2 cell): must keep the mapping consistent (mitigation: derive the cell from the anchor; store both; one is canonical for the DB key).
- **Cluster size is bounded** by the sag budget; very large contiguous play areas need multiple clusters with handoffs at their borders (mitigation: clusters ≥ a city; borders fall on unsimulated terrain).
- **Cross-cluster seams** carry a tiny tilt (mitigation: place cluster borders on water/unsimulated map; reuse the AOI handoff).

## Related

- [ADR-020](./020-map-tier-and-sim-tier-world-model.md): Defines the two tiers; this fixes the sim tier's tessellation.
- [ADR-019](./019-planet-scale-floating-origin.md): Precise global frame the clusters are pinned into.
- [ADR-006](./006-world-coordinate-and-osm-anchor.md): Refines/replaces its "region == tile" model; region size is now a fixed real-meter value, identity is the quad cell.
- [ADR-010](./010-authoritative-avatar-state-v0.md) / [ADR-012](./012-interest-management-and-osm-tiles.md): Cross-region/cluster movement reuses the AOI/handoff path.

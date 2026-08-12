# Vision

## Mission

Cartopolis is a **networked 3D place** where people **author, share, and visit** spatial environments—**grounded in real geography** (e.g. map tiles / OSM-backed context)—so that proposing and experiencing **layouts at civic or public scale** feels **social and immediate**, not like a solo modelling tool.

We want to **shorten the authoring loop**: 3D work is traditionally slow; we use **modern tooling (Rust, Bevy, the current crate ecosystem)** and **LLM-assisted development** to move faster, and over time **embed LLM capability in the product** so **world building** can keep pace with ideas—without abandoning **permissions, safety, and reproducibility**.

## Lineage (context, not requirements)

Experience with **Second Life, OpenSim, and realXtend (Tundra)** shaped the mental model: **regions, assets, social presence**, and **map-adjacent** virtual space. Those stacks also showed **limits** (performance, toolchain weight, ecosystem sustainability).

This project is a **clean-room architecture** in **Rust / Bevy**. It does **not** require wire or feature parity with legacy virtual-world platforms unless we add an explicit goal and ADR for that.

## Goals (`G-*`)

Stable IDs for traceability. New ADRs should cite relevant goals in **Aligns with**.

| ID | Goal |
|----|------|
| **G-01** | **Authoring:** Creators can define or evolve a **shared 3D environment** in line with the world model faster than a purely manual asset-only pipeline. |
| **G-02** | **Visiting:** Others can **enter the same environment** and **see a consistent world** (replication, interest, persistence as needed). |
| **G-03** | **Social:** Users can **interact as people** in-world (e.g. chat early; voice and shared media as later layers on the same trust model). |
| **G-04** | **Geography as substrate:** The world **is** the real planet — one **continuous space from street to orbit**, with **map-derived geometry and live real-world data** (buildings, layers, transit, weather) as the ground everything else is authored on top of, not as a backdrop. |
| **G-05** | **Governance:** **Authentication and authorization** prevent **unauthorized edits** and support **evolving roles**. |
| **G-06** | **LLM in the product:** The system can **use LLMs to assist creation or operation** of worlds under **explicit constraints** (scoped actions, validation, oversight as deployments require). *Partially delivered:* an in-client copilot authors prims through scoped tool calls; the constraint and oversight model is not yet recorded in an ADR. |
| **G-07** | **Real-world coupling (roadmap):** **Headless or companion clients** can relate **real-world position** (e.g. GNSS) to **in-world or map representation** where privacy and accuracy policies allow. |
| **G-08** | **Integrations:** **Defined behaviors** (e.g. scripting) can **connect to external systems** under **capability and permission** rules. |
| **G-09** | **Reach:** Visiting should cost **as little as opening a link** — the client runs on the **web** as well as natively, and an **anonymous Viewer tier** can look around before any account exists, with **login** gating the parts that mutate shared state. |

## Non-goals (current)

These are **explicit deferrals** to reduce scope creep unless a new goal and ADR say otherwise.

- **Full protocol or feature parity** with Second Life / OpenSim / legacy stacks.
- **Unbounded in-world LLM** actions with **no validation** on geometry, permissions, or resource use.
- **Production WebRTC / voice / video** before **session identity, auth, and abuse basics** are in place.
- Treating **GNSS** as perfect ground truth **without** a stated **accuracy, spoofing, and privacy** model.

## Related

- [principles.md](principles.md) — `P-*`
- [roadmap.md](roadmap.md) — **P0/P1/P2** priorities and ADR sequencing
- [adr-resolution.md](adr-resolution.md) — how deep to document (R0–R3)
- [capability-map.md](capability-map.md) — themes, priority, ADR coverage
- [../adr/index.md](../adr/index.md) — decisions

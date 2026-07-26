# Principles

Durable engineering rules. They **outlive** individual ADRs. If a decision **contradicts** a principle, update this file with rationale or document a **time-boxed exception** in the ADR.

New ADRs should cite relevant principles in **Aligns with** as `P-*`.

| ID | Principle |
|----|-----------|
| **P-01** | **Authoritative simulation** for shared state: what every client must agree on is **owned by the server** (or a defined authority), not invented only on clients. |
| **P-02** | **Explicit protocol and versioning** for cross-wire behavior; clients and sim stay **evolvable** without silent breakage. |
| **P-03** | **Performance and operational cost** are design inputs—especially for **many users** and **large worlds**—not late polish. |
| **P-04** | **Security by default for mutating actions**: edits, asset uploads, script execution, and **LLM-driven world changes** go through **identity, policy, and audit** appropriate to the deployment. |
| **P-05** | **Modular boundaries**: protocol, simulation, client, and asset paths stay **separable** with **clear ownership**—avoid an accidental monolith. |
| **P-06** | **LLM actions are constrained**: any automated build or change path defines **inputs, outputs, limits, and failure modes**; prompts do not replace engineering discipline. |
| **P-07** | **Privacy for sensitive channels**: location (e.g. GNSS), voice, video, and chat need an **explicit threat model** per feature, even when prototypes are lab-only. |

## Related

- [vision.md](vision.md) — `G-*` goals
- [../adr/index.md](../adr/index.md) — decisions that implement these

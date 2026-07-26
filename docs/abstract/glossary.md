# Glossary

Shared vocabulary for docs and ADRs. Refine as the domain stabilizes.

| Term | Meaning |
|------|---------|
| **Avatar** | A participant’s embodiment in the world; movement and representation may be **server-authoritative** (see ADR-010). |
| **Client** | The interactive **Bevy** application (or future thin clients) that renders the world and sends intents to the sim. |
| **Interest / AOI** | Rules for **which** world data a client receives based on position or other criteria (see ADR-012). |
| **Prim** | A **placed 3D primitive** or object instance in stored world data (see storage ADRs). |
| **Region** | A **logical slice of world** with geographic or grid context (see ADR-006, ADR-002). |
| **Sim** | The **headless simulation** process that owns authoritative state and speaks the wire protocol (`vibe_sim`). |
| **World model** | The **data and rules** that define what exists in the space (geometry, regions, prims, replicated state). |

## Related

- [vision.md](vision.md)
- [../adr/index.md](../adr/index.md)

# ADR Index

| ID | Title | Status | Tags |
|----|-------|--------|------|
| [001](./001-bevy-game-engine.md) | Bevy as Game Engine | Accepted | technology, rendering, ecs |
| [002](./002-sqlite-storage.md) | SQLite for Region and Prim Storage | Accepted | storage, database |
| [003](./003-ecs-architecture.md) | ECS Architecture with Bevy | Accepted | architecture, ecs |
| [004](./004-osm-tile-integration.md) | OSM Tile Integration for Region Textures | Accepted | rendering, tiles, osm |
| [005](./005-sky-lighting-system.md) | Sky and Lighting System | Accepted | rendering, lighting, sky, atmosphere |
| [006](./006-world-coordinate-and-osm-anchor.md) | World Coordinate Model and OSM Anchor | Superseded | coordinates, osm, regions, networking |
| [007](./007-simulation-vs-client-process-model.md) | Simulation vs Client Process Model | Accepted | server, client, headless, architecture |
| [008](./008-network-transport-layer.md) | Network Transport Layer (v0) | Accepted | networking, transport, server |
| [009](./009-application-protocol-envelope-v0.md) | Application Protocol Envelope (v0) | Accepted | protocol, networking, api |
| [010](./010-authoritative-avatar-state-v0.md) | Authoritative Avatar State (v0) | Accepted | avatar, networking, authority, simulation |
| [011](./011-static-world-replication-v0.md) | Static World Replication (v0) | Accepted | replication, networking, regions, prims |
| [012](./012-interest-management-and-osm-tiles.md) | Interest Management and OSM Tiles | Accepted | interest, aoi, osm, networking |
| [013](./013-sqlite-migrations-and-server-writer.md) | SQLite Migrations and Single Server Writer | Accepted | sqlite, storage, migrations, server |
| [014](./014-runtime-configuration-and-operations.md) | Runtime Configuration and Operations | Accepted | config, ops, deployment |
| [015](./015-workspace-module-boundaries.md) | Workspace Module and Crate Boundaries | Accepted | workspace, crates, architecture |
| [016](./016-authentication-and-session-model-v0.md) | Authentication and Server Session Model (v0) | Proposed | auth, session, security, networking, protocol, cli, ops |
| [017](./017-prim-authoring-and-editing.md) | Prim Authoring and Editing (v0) | Accepted | authoring, prims, ui, protocol, persistence, textures, replication |
| [018](./018-prim-texture-surface-params.md) | Prim Texture Surface Parameters | Accepted | prims, textures, ui, protocol, persistence, rendering |
| [019](./019-planet-scale-floating-origin.md) | Planet-Scale Floating-Origin Coordinate Model | Accepted | coordinates, precision, rendering, osm, large-world |
| [020](./020-map-tier-and-sim-tier-world-model.md) | Two-Tier World — Continuous Map Surface and Sim Content | Accepted | coordinates, map, regions, osm, large-world, architecture |
| [021](./021-region-tessellation-flat-clusters-quad-index.md) | Region Tessellation — Flat Metric Clusters with a Global Quad Index | Superseded | coordinates, regions, addressing, sim, large-world, architecture |
| [022](./022-osm-building-import.md) | OSM 3D Building Import (Overpass → Extruded Footprints) | Accepted | osm, buildings, rendering, import, dependency |
| [023](./023-data-layers.md) | Data Layers (Togglable OSM Point-of-Interest Overlays) | Accepted | osm, data-layers, rendering, ui, import |
| [024](./024-transit-live-boards.md) | Live Transit Departure Boards (OVapi) | Accepted | data-layers, transit, live-data, ui, dependency |
| [025](./025-planetary-globe-and-space-tier.md) | Planetary Globe and Space Tier | Accepted | rendering, coordinates, globe, osm, large-world, camera |
| [026](./026-live-transit-vehicles.md) | Live Transit Vehicle Positions (OVapi GTFS-Realtime) | Accepted | data-layers, transit, live-data, rendering, gtfs-rt |
| [027](./027-3dbag-buildings.md) | Self-Hosted 3DBAG Buildings for Covered Areas | Accepted | buildings, osm, 3dbag, rendering, import, dependency, ops |
| [028](./028-proximity-chat.md) | Proximity Chat with Place-Derived Channels | Proposed | chat, social, geo, places, protocol, networking, privacy, persistence |
| [029](./029-geo-anchored-prim-storage.md) | Geo-Anchored Prim Storage | Proposed | prims, coordinates, storage, protocol, precision, large-world, persistence |
| [029](./029-platform-capability-model.md) | Platform Capability Model (Substitutable Providers + Additive Plugins) | Proposed | architecture, platform, wasm, web, capabilities, portability, plugins |

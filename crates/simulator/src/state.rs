use cartopolis_core::{
    snap_yaw_continuation, AvatarStateDto, Emote, EmoteState, GeoAnchor, NetMessage, PrimDto,
    PrimGeometry, PrimSurface, RegionDto, EMOTE_REPLICATE_SECS,
};
use glam::Vec3;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use crate::db::RegionCaps;
use crate::scripting::{PrimView, ScriptCommand};
use cartopolis_core::motion;

struct AvatarSim {
    position: Vec3,
    yaw: f32,
    velocity: Vec3,
    /// Flight mode (`ClientIntent::flying`): hovers at altitude, and only then do
    /// `fly_vertical` and the client's vertical keys mean anything. An avatar that isn't flying
    /// falls — see [`SimWorld::step`].
    flying: bool,
    fly_vertical: f32,
    /// Vertical velocity (m/s, up positive) for the jump/fall arc. Flight ignores
    /// it and holds altitude; everything else integrates it under
    /// [`motion::GRAVITY`]. It replaced a constant descent rate, which meant a
    /// fall ran at full speed from its first frame and a jump was impossible to
    /// express at all.
    vertical_velocity: f32,
    /// Set for one tick by a `ClientIntent` whose `jump` bit is set. Consumed by
    /// [`SimWorld::step`], which only honours it when the avatar is on the ground —
    /// the authority decides whether a jump happens, so a client cannot fly by
    /// spamming the bit.
    jump_requested: bool,
    /// The emote this avatar is currently broadcasting, and how long it has been
    /// doing so. Held for [`EMOTE_REPLICATE_SECS`] rather than forwarded once, so
    /// a client whose first snapshot of this avatar lands mid-wave still sees it.
    emote: Option<EmoteState>,
    emote_age: f32,
    /// Monotonic per-avatar counter. The same emote twice running is two events,
    /// and a receiver that diffed on the emote alone would play only the first.
    emote_sequence: u32,
}

pub struct SimWorld {
    regions: Vec<RegionDto>,
    prims: Vec<PrimDto>,
    /// Region id -> approximate sim origin (for AOI); v0 single region at origin.
    region_sim_origin: HashMap<i64, Vec3>,
    avatars: HashMap<u64, AvatarSim>,
    next_avatar_id: u64,
    observer: Vec3,
    aoi_radius_sq: f32,
    /// Database connection for prim mutations (Phase 3).
    conn: Arc<Mutex<rusqlite::Connection>>,
    /// Per-region resource budget (schema V7). A region absent from this map has
    /// no recorded budget and is treated as unlimited — that only happens for a
    /// row inserted outside the migration path.
    caps: HashMap<i64, RegionCaps>,
    /// Prims carrying a script, so the script budget can be counted without
    /// touching the database on every create.
    scripted: HashSet<i64>,
    /// Prims mutated by a script since the last flush.
    ///
    /// Script writes land in memory and replicate through the ordinary
    /// `WorldSnapshot` — prims are already in it — and are written back to SQLite
    /// on a timer instead. A moving prim would otherwise be an UPDATE at the tick
    /// rate, which is 20 disk writes a second per script for state that is
    /// re-derivable and changes again immediately.
    dirty: HashSet<i64>,
}

/// A region's budget and what is currently drawn against it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapacityUsage {
    pub region_id: i64,
    pub prims_used: u32,
    pub prims_max: u32,
    pub scripts_used: u32,
    pub scripts_max: u32,
}

impl SimWorld {
    pub fn new(
        mut regions: Vec<RegionDto>,
        prims: Vec<PrimDto>,
        aoi_radius: f32,
        conn: Arc<Mutex<rusqlite::Connection>>,
        caps: HashMap<i64, RegionCaps>,
        scripted: HashSet<i64>,
    ) -> Self {
        let mut region_sim_origin = HashMap::new();
        let n = regions.len().max(1);
        let grid_size = (n as f32).sqrt().ceil() as usize;
        let spacing = 300.0_f32;
        let grid_f = grid_size as f32;
        // Same ordering as the offline client: sort by region id so grid indices match.
        let mut ordered_indices: Vec<usize> = (0..regions.len()).collect();
        ordered_indices.sort_by_key(|&i| regions[i].id);
        for (grid_idx, &idx) in ordered_indices.iter().enumerate() {
            let row = grid_idx / grid_size;
            let col = grid_idx % grid_size;
            let pos = Vec3::new(
                (col as f32 - grid_f / 2.0) * spacing,
                0.0,
                (row as f32 - grid_f / 2.0) * spacing,
            );
            let r = &mut regions[idx];
            region_sim_origin.insert(r.id, pos);
            r.sim_x = pos.x;
            r.sim_y = pos.y;
            r.sim_z = pos.z;
        }
        Self {
            regions,
            prims,
            region_sim_origin,
            avatars: HashMap::new(),
            next_avatar_id: 1,
            observer: Vec3::ZERO,
            aoi_radius_sq: aoi_radius * aoi_radius,
            conn,
            caps,
            scripted,
            dirty: HashSet::new(),
        }
    }

    pub fn spawn_avatar(&mut self) -> u64 {
        let id = self.next_avatar_id;
        self.next_avatar_id += 1;
        let mut ids: Vec<i64> = self.regions.iter().map(|r| r.id).collect();
        ids.sort();
        let start = ids
            .first()
            .and_then(|rid| self.region_sim_origin.get(rid).copied())
            .unwrap_or(Vec3::ZERO);
        self.avatars.insert(
            id,
            AvatarSim {
                // Feet on the ground: the client avatar models are authored with
                // their origin at the feet, so the standing height is y=0 (the sim
                // clamps y>=0 below). The old +1.0 floated the humanoids a metre up.
                position: start,
                // Match client tank convention: yaw π ↔ tank 0 ↔ travel −Z when pressing W.
                yaw: std::f32::consts::PI,
                velocity: Vec3::ZERO,
                flying: false,
                fly_vertical: 0.0,
                vertical_velocity: 0.0,
                jump_requested: false,
                emote: None,
                emote_age: 0.0,
                emote_sequence: 0,
            },
        );
        id
    }

    pub fn remove_avatar(&mut self, id: u64) {
        self.avatars.remove(&id);
    }

    /// Current position of an avatar (proximity chat, ADR-028).
    pub fn avatar_position(&self, id: u64) -> Option<Vec3> {
        self.avatars.get(&id).map(|a| a.position)
    }

    pub fn set_observer(&mut self, p: Vec3) {
        self.observer = p;
    }

    /// Returns a reference to the regions (for testing).
    #[cfg(test)]
    pub fn regions(&self) -> &[RegionDto] {
        &self.regions
    }

    /// Returns a reference to the prims (for testing).
    #[cfg(test)]
    pub fn prims(&self) -> &[PrimDto] {
        &self.prims
    }

    #[allow(clippy::too_many_arguments)]
    pub fn apply_intent(
        &mut self,
        avatar_id: u64,
        move_x: f32,
        move_z: f32,
        display_yaw: f32,
        flying: bool,
        fly_up: bool,
        fly_down: bool,
        jump: bool,
        emote: Option<Emote>,
    ) {
        // Every float in an intent must be a real number, or the whole intent is
        // discarded.
        //
        // postcard decodes an `f32` from four raw little-endian bytes, so a client
        // can put any bit pattern it likes on the wire, including infinity — and
        // infinity is the one that gets through. The magnitude test below rejects
        // NaN by accident (`NaN > 1e-4` is false) but *accepts* `inf`, and
        // `v / inf` is NaN, which lands in `velocity`, then in `position`, then in
        // every `WorldSnapshot` to every client, permanently: no later intent can
        // recover a position that is already NaN, because NaN + anything is NaN.
        //
        // Discarding the whole intent rather than sanitising the bad component is
        // deliberate. A frame like this is not a client having a rounding problem,
        // it is a client sending something no encoder here produces; there is no
        // part of it worth honouring, and picking one would mean deciding what a
        // malformed jump bit means. `display_yaw` is guarded downstream by
        // `snap_yaw_continuation`, but it is checked here too so the rule is one
        // rule rather than a note about which fields happen to be safe.
        if !move_x.is_finite() || !move_z.is_finite() || !display_yaw.is_finite() {
            tracing::debug!(avatar_id, "discarded an intent with a non-finite field");
            return;
        }
        let Some(av) = self.avatars.get_mut(&avatar_id) else {
            return;
        };
        // Horizontal speed is the intent *magnitude* scaled, capped
        // (`motion::sim_speed_for_wish` — the client predicts with the same
        // function). The magnitude is the throttle: a walk, a jog, a sprint and
        // flight are four magnitudes, which is why none of them needed a
        // protocol field of its own.
        let mut v = Vec3::new(move_x, 0.0, move_z);
        let mag = v.length();
        if mag > 1e-4 {
            v = v / mag * motion::sim_speed_for_wish(mag);
        } else {
            v = Vec3::ZERO;
        }
        av.velocity.x = v.x;
        av.velocity.z = v.z;
        // Always apply facing so remotes see orbit-camera rotation while idle (not only when moving).
        av.yaw = snap_yaw_continuation(av.yaw, display_yaw);
        // Vertical thrust only means something in flight: a walker holding the
        // climb key used to levitate and — nothing pulling it back down — stay up.
        av.flying = flying;
        av.fly_vertical = if !flying {
            0.0
        } else if fly_up {
            motion::FLY_VERTICAL_SPEED
        } else if fly_down {
            -motion::FLY_VERTICAL_SPEED
        } else {
            0.0
        };
        // Latch rather than assign: intents arrive faster than ticks and are rate
        // limited on top (`net::MIN_INTENT_INTERVAL`), so a jump that shared a tick
        // with a later "not jumping" intent would be silently dropped. `step`
        // clears it.
        av.jump_requested |= jump;
        // Taking off cancels a pending jump — otherwise it fires on landing, long
        // after the key that asked for it.
        if flying {
            av.jump_requested = false;
        }
        // An emote is a one-shot: stamp it with a fresh sequence so receivers can
        // tell "waved again" from "still waving".
        if let Some(e) = emote {
            av.emote_sequence = av.emote_sequence.wrapping_add(1);
            av.emote = Some(EmoteState {
                emote: e,
                sequence: av.emote_sequence,
            });
            av.emote_age = 0.0;
        }
    }

    pub fn step(&mut self, dt: f32) {
        for av in self.avatars.values_mut() {
            av.position.x += av.velocity.x * dt;
            av.position.z += av.velocity.z * dt;
            // Flight holds altitude; everything else is on the vertical arc, which
            // is `motion::step_fall` — the same function the client predicts with
            // and the `--offline` path integrates, so the three cannot disagree.
            if av.flying {
                av.position.y += av.fly_vertical * dt;
                av.vertical_velocity = 0.0;
                av.jump_requested = false;
                if av.position.y < motion::GROUND_Y {
                    av.position.y = motion::GROUND_Y;
                }
            } else {
                // A jump is only granted from the ground: the authority decides,
                // so holding the key cannot climb.
                if av.jump_requested && av.position.y <= motion::GROUND_Y + 1e-3 {
                    av.vertical_velocity = motion::JUMP_SPEED;
                }
                av.jump_requested = false;
                let s =
                    motion::step_fall(av.position.y, av.vertical_velocity, motion::GROUND_Y, dt);
                av.position.y = s.y;
                av.vertical_velocity = s.vertical_velocity;
            }
            // Age out the replicated emote so it stops being re-broadcast (and
            // re-played by anyone who arrives) long after it finished.
            if av.emote.is_some() {
                av.emote_age += dt;
                if av.emote_age > EMOTE_REPLICATE_SECS {
                    av.emote = None;
                }
            }
            // Horizontal yaw comes from [`SimWorld::apply_intent`] (`display_yaw`); do not derive from
            // `atan2(velocity)` (branch cuts + integration drift caused client flip-flops).
        }
    }

    /// ADR-012: filter regions/prims by distance from observer to region origin (v0 heuristic).
    pub fn snapshot(&self, tick: u64) -> NetMessage {
        let regions: Vec<RegionDto> = self
            .regions
            .iter()
            .filter(|r| {
                let Some(origin) = self.region_sim_origin.get(&r.id) else {
                    return true;
                };
                (*origin - self.observer).length_squared() <= self.aoi_radius_sq
            })
            .cloned()
            .collect();

        let region_ids: std::collections::HashSet<i64> = regions.iter().map(|r| r.id).collect();
        let prims: Vec<PrimDto> = self
            .prims
            .iter()
            .filter(|p| region_ids.contains(&p.region_id))
            .cloned()
            .collect();

        // v0 multiplayer: replicate every avatar to all clients. (Regions/prims still use observer AOI.)
        let avatars: Vec<AvatarStateDto> = self
            .avatars
            .iter()
            .map(|(&id, a)| AvatarStateDto {
                id,
                position: a.position,
                yaw: a.yaw,
                emote: a.emote,
            })
            .collect();

        NetMessage::WorldSnapshot {
            tick,
            regions,
            prims,
            avatars,
        }
    }

    /// Create a new prim in the world (Phase 3).
    /// Persists to database and updates in-memory list.
    /// Returns the created PrimDto on success, or ServerError on failure.
    /// (ADR-017 Phase 3)
    pub fn add_prim(
        &mut self,
        region_id: i64,
        position: Vec3,
        shape: &str,
        geo: GeoAnchor,
    ) -> Result<PrimDto, String> {
        // Capacity is checked *before* the insert, so the refusal costs no write
        // and the id sequence does not advance on a rejected create.
        if let Some(caps) = self.caps.get(&region_id) {
            let used = self.region_prim_count(region_id);
            if used >= caps.max_prims {
                return Err(format!(
                    "region is full: {used} of {} prims. Delete something or ask the operator to raise the budget.",
                    caps.max_prims
                ));
            }
        }
        // Call db::insert_prim with a minimal lock
        let prim = {
            let conn_guard = self
                .conn
                .lock()
                .map_err(|e| format!("failed to acquire db lock: {}", e))?;
            crate::db::insert_prim(&conn_guard, region_id, position, shape, geo)
                .map_err(|e| format!("insert_prim failed: {}", e))?
        };
        // Add to in-memory list
        self.prims.push(prim.clone());
        Ok(prim)
    }

    /// Update an existing prim in the world (Phase 3).
    /// Persists to database and updates in-memory list.
    /// Returns the updated PrimDto on success, or ServerError if prim not found.
    /// (ADR-017 Phase 3)
    pub fn update_prim(
        &mut self,
        prim_id: i64,
        position: Vec3,
        rotation: Vec3,
        scale: Vec3,
        color: [f32; 3],
        texture_id: Option<String>,
        name: &str,
        surface: PrimSurface,
        geometry: PrimGeometry,
        geo: GeoAnchor,
    ) -> Result<PrimDto, String> {
        // Call db::update_prim with a minimal lock
        let prim = {
            let conn_guard = self
                .conn
                .lock()
                .map_err(|e| format!("failed to acquire db lock: {}", e))?;
            crate::db::update_prim(
                &conn_guard,
                prim_id,
                position,
                rotation,
                scale,
                color,
                texture_id,
                name,
                surface,
                geometry,
                geo,
            )
            .map_err(|e| format!("update_prim failed: {}", e))?
            .ok_or_else(|| format!("prim {} not found", prim_id))?
        };
        // Update in-memory list
        if let Some(pos) = self.prims.iter().position(|p| p.id == prim_id) {
            self.prims[pos] = prim.clone();
        }
        Ok(prim)
    }

    /// Delete a prim from the world (Phase 3).
    /// Persists deletion to database and removes from in-memory list.
    /// Returns true if the prim was deleted, false if not found.
    /// (ADR-017 Phase 3)
    pub fn remove_prim(&mut self, prim_id: i64) -> Result<bool, String> {
        // Call db::delete_prim with a minimal lock
        let deleted = {
            let conn_guard = self
                .conn
                .lock()
                .map_err(|e| format!("failed to acquire db lock: {}", e))?;
            crate::db::delete_prim(&conn_guard, prim_id)
                .map_err(|e| format!("delete_prim failed: {}", e))?
        };
        // Remove from in-memory list if it was deleted
        if deleted {
            if let Some(pos) = self.prims.iter().position(|p| p.id == prim_id) {
                self.prims.remove(pos);
            }
            // `db::delete_prim` removed the script row too, so release its budget.
            self.scripted.remove(&prim_id);
            self.dirty.remove(&prim_id);
        }
        Ok(deleted)
    }

    fn region_prim_count(&self, region_id: i64) -> u32 {
        self.prims
            .iter()
            .filter(|p| p.region_id == region_id)
            .count() as u32
    }

    /// A prim by id, for the lookups a script command needs (its name and where
    /// it is standing when it speaks).
    pub fn prim(&self, prim_id: i64) -> Option<&PrimDto> {
        self.prims.iter().find(|p| p.id == prim_id)
    }

    /// Whether a prim carries a script. Cheap enough to ask per touch — the set is
    /// in memory precisely so this doesn't become a query.
    pub fn is_scripted(&self, prim_id: i64) -> bool {
        self.scripted.contains(&prim_id)
    }

    /// The region a prim belongs to, if it exists.
    pub fn prim_region(&self, prim_id: i64) -> Option<i64> {
        self.prims
            .iter()
            .find(|p| p.id == prim_id)
            .map(|p| p.region_id)
    }

    /// Budget and usage for a region, for [`NetMessage::RegionCapacity`].
    pub fn capacity(&self, region_id: i64) -> Option<CapacityUsage> {
        let caps = self.caps.get(&region_id)?;
        let scripts_used = self
            .prims
            .iter()
            .filter(|p| p.region_id == region_id && self.scripted.contains(&p.id))
            .count() as u32;
        Some(CapacityUsage {
            region_id,
            prims_used: self.region_prim_count(region_id),
            prims_max: caps.max_prims,
            scripts_used,
            scripts_max: caps.max_scripts,
        })
    }

    /// Capacity for every region, for the connect-time push.
    pub fn all_capacity(&self) -> Vec<CapacityUsage> {
        self.regions
            .iter()
            .filter_map(|r| self.capacity(r.id))
            .collect()
    }

    /// Record that a prim gained or lost a script, after the script budget has
    /// been checked with [`SimWorld::can_add_script`].
    pub fn set_scripted(&mut self, prim_id: i64, scripted: bool) {
        if scripted {
            self.scripted.insert(prim_id);
        } else {
            self.scripted.remove(&prim_id);
        }
    }

    /// Whether `prim_id` may take a script. Replacing an existing one is always
    /// allowed — it consumes no additional budget.
    pub fn can_add_script(&self, prim_id: i64) -> Result<(), String> {
        if self.scripted.contains(&prim_id) {
            return Ok(());
        }
        let Some(region_id) = self.prim_region(prim_id) else {
            return Err(format!("prim {prim_id} not found"));
        };
        let Some(usage) = self.capacity(region_id) else {
            return Ok(());
        };
        if usage.scripts_used >= usage.scripts_max {
            return Err(format!(
                "region is at its script limit: {} of {}",
                usage.scripts_used, usage.scripts_max
            ));
        }
        Ok(())
    }

    /// Read-only views of every scripted prim, handed to the host each tick.
    pub fn script_views(&self) -> Vec<(i64, PrimView)> {
        self.prims
            .iter()
            .filter(|p| self.scripted.contains(&p.id))
            .map(|p| {
                (
                    p.id,
                    PrimView {
                        pos: p.position,
                        rot: p.rotation,
                        color: p.color,
                        scale: p.scale,
                        alpha: p.surface.alpha,
                        glow: p.surface.glow,
                        full_bright: p.surface.full_bright,
                        name: p.name.clone(),
                    },
                )
            })
            .collect()
    }

    /// Apply a script's transform/appearance write in memory.
    ///
    /// Returns the mutated prim so the caller can broadcast a `PrimUpsert`.
    /// Nothing is persisted here — see [`SimWorld::flush_dirty`] and the note on
    /// the `dirty` field.
    pub fn apply_script_command(&mut self, cmd: &ScriptCommand) -> Option<PrimDto> {
        let (prim_id, mutate): (i64, Box<dyn FnOnce(&mut PrimDto)>) = match cmd {
            ScriptCommand::SetPos { prim_id, pos } => {
                let pos = *pos;
                (*prim_id, Box::new(move |p: &mut PrimDto| p.position = pos))
            }
            ScriptCommand::SetColor { prim_id, color } => {
                let color = *color;
                (*prim_id, Box::new(move |p: &mut PrimDto| p.color = color))
            }
            ScriptCommand::SetScale { prim_id, scale } => {
                let scale = *scale;
                (*prim_id, Box::new(move |p: &mut PrimDto| p.scale = scale))
            }
            ScriptCommand::SetRot { prim_id, rot } => {
                let rot = *rot;
                (*prim_id, Box::new(move |p: &mut PrimDto| p.rotation = rot))
            }
            ScriptCommand::SetAlpha { prim_id, alpha } => {
                let alpha = *alpha;
                (
                    *prim_id,
                    Box::new(move |p: &mut PrimDto| p.surface.alpha = alpha),
                )
            }
            ScriptCommand::SetGlow { prim_id, glow } => {
                let glow = *glow;
                (
                    *prim_id,
                    Box::new(move |p: &mut PrimDto| p.surface.glow = glow),
                )
            }
            ScriptCommand::SetFullBright { prim_id, on } => {
                let on = *on;
                (
                    *prim_id,
                    Box::new(move |p: &mut PrimDto| p.surface.full_bright = on),
                )
            }
            ScriptCommand::SetName { prim_id, name } => {
                let name = name.clone();
                (
                    *prim_id,
                    Box::new(move |p: &mut PrimDto| p.name = name.clone()),
                )
            }
            // Not a field write: the caller routes these to chat, to the editor,
            // and to `remove_prim` respectively.
            ScriptCommand::Say { .. }
            | ScriptCommand::Output { .. }
            | ScriptCommand::Die { .. } => return None,
        };
        let prim = self.prims.iter_mut().find(|p| p.id == prim_id)?;
        mutate(prim);
        let out = prim.clone();
        self.dirty.insert(prim_id);
        Some(out)
    }

    /// Persist prims that scripts have moved since the last call.
    ///
    /// Best-effort: a failed write leaves the prim dirty for the next flush, and
    /// the in-memory value — which is what clients see — is unaffected.
    pub fn flush_dirty(&mut self) {
        if self.dirty.is_empty() {
            return;
        }
        let ids: Vec<i64> = self.dirty.drain().collect();
        let Ok(conn) = self.conn.lock() else {
            tracing::warn!("db lock poisoned; script writes not persisted");
            return;
        };
        for id in ids {
            let Some(p) = self.prims.iter().find(|p| p.id == id) else {
                continue;
            };
            if let Err(e) = crate::db::update_prim(
                &conn,
                p.id,
                p.position,
                p.rotation,
                p.scale,
                p.color,
                p.texture_id.clone(),
                &p.name,
                p.surface,
                prim_geometry(p),
                p.geo,
            ) {
                tracing::warn!(prim_id = id, "persisting script write failed: {e:#}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A world on a throwaway database — `SimWorld` writes prim mutations through
    /// its connection, so an in-memory stub would not exercise the same code.
    fn test_world() -> (SimWorld, tempfile::TempDir) {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let path = dir.path().join("test.db");
        let conn = crate::db::open_and_migrate(path.to_str().unwrap()).expect("open db");
        let (regions, prims) = crate::db::load_world(&conn).expect("load world");
        let caps = crate::db::load_region_caps(&conn).expect("load caps");
        let world = SimWorld::new(
            regions,
            prims,
            50.0,
            Arc::new(Mutex::new(conn)),
            caps,
            HashSet::new(),
        );
        (world, dir)
    }

    /// A hostile client must not be able to write a non-number into the
    /// authoritative avatar state.
    ///
    /// Infinity is the case that got through: the wire is raw `f32` bytes, the
    /// magnitude test only rejects NaN, and `v / inf` is NaN. Once the position is
    /// NaN it stays NaN — every subsequent intent adds to it — and it is
    /// replicated to every client for the life of the process.
    #[test]
    fn a_non_finite_intent_cannot_poison_the_avatar() {
        let (mut world, _dir) = test_world();
        let id = world.spawn_avatar();
        let start = world.avatar_position(id).expect("spawned");

        for (x, z, yaw) in [
            (f32::INFINITY, 0.0, 0.0),
            (f32::NEG_INFINITY, 0.0, 0.0),
            (0.0, f32::INFINITY, 0.0),
            (f32::NAN, f32::NAN, 0.0),
            (1.0, 0.0, f32::NAN),
        ] {
            world.apply_intent(id, x, z, yaw, false, false, false, false, None);
            world.step(0.05);
            let p = world.avatar_position(id).expect("still there");
            assert!(
                p.is_finite(),
                "intent ({x}, {z}, yaw {yaw}) put {p:?} into the authoritative position"
            );
        }
        assert_eq!(
            world.avatar_position(id),
            Some(start),
            "a rejected intent should move nothing at all"
        );

        // And an ordinary intent still moves, so the guard is not simply refusing
        // everything.
        world.apply_intent(id, 0.0, 1.0, 0.0, false, false, false, false, None);
        world.step(0.05);
        let p = world.avatar_position(id).expect("still there");
        assert!(p.is_finite());
        assert!(
            (p.z - start.z).abs() > 1e-4,
            "a valid intent should have moved the avatar"
        );
    }
}

/// Re-assemble the flattened geometry fields of a [`PrimDto`] into the grouped
/// [`PrimGeometry`] that the write path takes.
fn prim_geometry(p: &PrimDto) -> PrimGeometry {
    PrimGeometry {
        path_cut_begin: p.path_cut_begin,
        path_cut_end: p.path_cut_end,
        hollow: p.hollow,
        twist_begin: p.twist_begin,
        twist_end: p.twist_end,
        taper_x: p.taper_x,
        taper_y: p.taper_y,
        top_shear_x: p.top_shear_x,
        top_shear_y: p.top_shear_y,
        slice_begin: p.slice_begin,
        slice_end: p.slice_end,
    }
}

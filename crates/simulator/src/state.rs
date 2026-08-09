use glam::Vec3;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use viberfox_core::{
    snap_yaw_continuation, AvatarStateDto, Emote, EmoteState, GeoAnchor, NetMessage, PrimDto,
    PrimGeometry, PrimSurface, RegionDto, EMOTE_REPLICATE_SECS,
};

use viberfox_core::motion;

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
}

impl SimWorld {
    pub fn new(
        mut regions: Vec<RegionDto>,
        prims: Vec<PrimDto>,
        aoi_radius: f32,
        conn: Arc<Mutex<rusqlite::Connection>>,
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
                let s = motion::step_fall(
                    av.position.y,
                    av.vertical_velocity,
                    motion::GROUND_Y,
                    dt,
                );
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
        }
        Ok(deleted)
    }
}

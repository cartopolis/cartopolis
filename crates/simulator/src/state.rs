use cartopolis_core::{
    snap_yaw_continuation, AvatarStateDto, Emote, EmoteState, GeoAnchor, NetMessage,
    ParticleEffect, PrimDto, PrimGeometry, PrimSurface, EMOTE_REPLICATE_SECS, MAX_SEAT_OFFSET_M,
};
use glam::Vec3;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

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
    /// Climb/descent the client is *asking* for (m/s): ±[`motion::FLY_VERTICAL_SPEED`]
    /// or zero, written straight from the intent's `fly_up`/`fly_down` bits.
    fly_vertical_target: f32,
    /// What the thrust has actually built to, ramped toward the target by
    /// [`motion::step_fly_vertical`] in [`SimWorld::step`]. The wire carries two
    /// booleans, so this ramp is the one part of the vertical model the authority
    /// has to reproduce rather than receive — the client predicts it with the same
    /// function.
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
    /// The account this avatar is signed in as, for anything that has to *name*
    /// it — today that is `vfSensor`, which hands a script the name so a prim can
    /// greet whoever walked up. Set once at the handshake; "Someone" until then,
    /// which is what a connection that has not authenticated yet is.
    name: String,
    /// Where this avatar is on Earth, as its own client last said (protocol 20).
    ///
    /// The sim cannot compute this and does not use it to move anything: it is
    /// the only frame in which "is this avatar near that prim?" has an answer
    /// (see [`cartopolis_core::NetMessage::ClientIntent`]'s `geo`), and
    /// `vfSensor` is the one consumer. `None` until a client that knows says so —
    /// an avatar with no fix is invisible to a sensor, which is the honest
    /// answer.
    geo: Option<GeoAnchor>,
    /// Where this avatar's body is drawn relative to [`Self::position`], while it
    /// is sitting on something (`AvatarStateDto::seated`).
    ///
    /// The sim holds it without understanding it: it has no map, so it cannot
    /// check that there is a bench there. What it *can* do is the two things a
    /// client must not be trusted with — bound it (`MAX_SEAT_OFFSET_M`) and drop
    /// it the moment the same avatar asks to move, so "sitting" cannot become a
    /// way to walk around displaced from your own position.
    seated: Option<[f32; 2]>,
}

pub struct SimWorld {
    prims: Vec<PrimDto>,
    avatars: HashMap<u64, AvatarSim>,
    next_avatar_id: u64,
    /// Database connection for prim mutations (Phase 3).
    conn: Arc<Mutex<rusqlite::Connection>>,
    /// What this world may hold at once, from `cartopolis.toml`.
    ///
    /// One budget for the server. It used to be a row per region, on the
    /// reasoning that a region is the unit an operator hands out — but a
    /// simulator only ever ran one region, so the ceiling was already the
    /// server's, and it now sits with the rest of an operator's decisions
    /// instead of in a table.
    caps: WorldCaps,
    /// Prim id -> the account that built it (schema V15), for the edit and delete
    /// permission. Absent means nobody owns it, which is how everything built
    /// before V15 and everything a content pack installed reads.
    ///
    /// In memory rather than queried, because `UpdatePrim` arrives at the gizmo's
    /// drag rate: a permission check that read SQLite twenty times a second per
    /// dragging client would put the database on the movement path. Loaded once
    /// at startup and maintained here, exactly as `scripted` is.
    owners: HashMap<i64, String>,
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

/// Furthest a `vfSensor` may see, whatever a script asks for.
///
/// A cap on the *gathering*, not on the question: [`SimWorld::script_views`]
/// measures every located avatar against every scripted prim once a tick, and
/// without a ceiling a script could ask about a continent and get one. It is
/// generous next to any plausible in-world use and stingy next to a planet.
const SENSOR_MAX_RANGE_M: f32 = 4_096.0;

/// Most neighbours carried to one script per tick. `vfSensor` returns the nearest
/// one, so this only bounds what a future "list everyone" call could ask for —
/// and bounds the per-tick allocation now, which is the reason it exists.
const SENSOR_MAX_RESULTS: usize = 16;

/// Ground distance between two geo anchors, in metres, as `f32` for the script
/// API. Non-finite inputs come back as infinity, which every caller filters out —
/// there is no distance that means "unknown", so the answer has to be one no
/// range test accepts.
fn ground_distance_m(a: GeoAnchor, b: GeoAnchor) -> f32 {
    if !a.latitude.is_finite()
        || !a.longitude.is_finite()
        || !b.latitude.is_finite()
        || !b.longitude.is_finite()
    {
        return f32::INFINITY;
    }
    cartopolis_core::ground_distance_m(a.latitude, a.longitude, b.latitude, b.longitude) as f32
}

/// What the world may hold at once ([`SimWorld::caps`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorldCaps {
    pub max_prims: u32,
    pub max_scripts: u32,
}

/// The world's budget and what is currently drawn against it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapacityUsage {
    pub prims_used: u32,
    pub prims_max: u32,
    pub scripts_used: u32,
    pub scripts_max: u32,
}

impl SimWorld {
    pub fn new(
        prims: Vec<PrimDto>,
        conn: Arc<Mutex<rusqlite::Connection>>,
        caps: WorldCaps,
        scripted: HashSet<i64>,
        owners: HashMap<i64, String>,
    ) -> Self {
        Self {
            prims,
            avatars: HashMap::new(),
            next_avatar_id: 1,
            conn,
            caps,
            scripted,
            owners,
            dirty: HashSet::new(),
        }
    }

    pub fn spawn_avatar(&mut self) -> u64 {
        let id = self.next_avatar_id;
        self.next_avatar_id += 1;
        // The origin of the sim's flat frame, which is where every client's map
        // is anchored when it connects. There is nothing else to spawn relative
        // to: the sim holds no map (see `AvatarSim::geo`).
        let start = Vec3::ZERO;
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
                fly_vertical_target: 0.0,
                fly_vertical: 0.0,
                vertical_velocity: 0.0,
                jump_requested: false,
                emote: None,
                emote_age: 0.0,
                emote_sequence: 0,
                name: "Someone".to_string(),
                geo: None,
                seated: None,
            },
        );
        id
    }

    pub fn remove_avatar(&mut self, id: u64) {
        self.avatars.remove(&id);
    }

    /// Record who an avatar is, once the handshake knows.
    pub fn set_avatar_name(&mut self, id: u64, name: &str) {
        if let Some(a) = self.avatars.get_mut(&id) {
            a.name = name.to_string();
        }
    }

    /// Place an avatar (protocol 17, `NetMessage::Teleport`): the counterpart of
    /// [`Self::apply_intent`], which can only ask to *move*. Every jump in the
    /// client — a searched place, a bookmark, a shared link, a distant GPS fix —
    /// funnels here when a session is online; without it the client moved only
    /// its local entity and the next snapshot pulled the avatar straight back.
    ///
    /// Motion state is cleared along with the position, for the same reason the
    /// client's own teleport lands the camera at rest: velocity carried across a
    /// jump slides the avatar off the place it asked for, and a jump arc from
    /// the origin means landing mid-fall at the destination. Flight survives —
    /// it is a mode, not motion — but the position's own `y` is clamped like
    /// [`Self::step`] clamps it, so a malformed teleport cannot put an avatar
    /// underground.
    pub fn teleport_avatar(&mut self, id: u64, position: Vec3) {
        // Same rule as `apply_intent`, and the same reason: postcard decodes an
        // `f32` from raw bytes, a NaN position poisons every later snapshot, and
        // there is no honest partial application of a malformed placement.
        if !position.x.is_finite() || !position.y.is_finite() || !position.z.is_finite() {
            tracing::debug!(id, "discarded a teleport with a non-finite position");
            return;
        }
        let Some(av) = self.avatars.get_mut(&id) else {
            return;
        };
        av.position = Vec3::new(position.x, position.y.max(0.0), position.z);
        av.velocity = Vec3::ZERO;
        av.vertical_velocity = 0.0;
        av.jump_requested = false;
        // `debug`, not `info`: this stopped being one message per deliberate jump
        // when device tracking started sending a placement per GNSS fix, i.e. at
        // ~1 Hz for as long as somebody is driving or has the locate button lit.
        // A line a second per tracked client is a log nobody can read the rest of.
        tracing::debug!(id, x = position.x, z = position.z, "avatar teleported");
    }

    /// Current position of an avatar (proximity chat, ADR-028).
    pub fn avatar_position(&self, id: u64) -> Option<Vec3> {
        self.avatars.get(&id).map(|a| a.position)
    }

    /// How many avatars are in the world — one per connected client, so this is
    /// the population reported by `/api/health`.
    pub fn avatar_count(&self) -> usize {
        self.avatars.len()
    }

    /// Every prim in the world, in id order.
    ///
    /// The AoI filter in [`SimWorld::snapshot`] is what a *client* sees; this is
    /// the whole set, which is what the authoring API lists and what the tests
    /// assert against. An operator listing the world's contents is not standing
    /// anywhere in it.
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
        seated: Option<[f32; 2]>,
        geo: Option<GeoAnchor>,
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
        // The geographic fix, filtered rather than trusted: a non-finite or
        // off-globe coordinate is dropped and the last good one kept, because a
        // sensor reading a NaN latitude would compute a NaN distance and then see
        // nobody at all, everywhere, for as long as that client stayed connected.
        if let Some(g) = geo {
            if g.latitude.is_finite()
                && g.longitude.is_finite()
                && g.latitude.abs() <= 90.0
                && g.longitude.abs() <= 180.0
            {
                av.geo = Some(g);
            }
        }
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
        av.fly_vertical_target = if !flying {
            0.0
        } else if fly_up {
            motion::FLY_VERTICAL_SPEED
        } else if fly_down {
            -motion::FLY_VERTICAL_SPEED
        } else {
            0.0
        };
        // Leaving flight drops the thrust outright rather than ramping it down:
        // an avatar that isn't flying is on the gravity arc, and coasting upward
        // out of it for half a second is the levitation this branch exists to
        // prevent.
        if !flying {
            av.fly_vertical = 0.0;
        }
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
        // The seat. A *level*, unlike the jump and the emote beside it: it holds
        // until the client says otherwise, so an avatar that comes into range
        // mid-sit is drawn sitting.
        //
        // Three ways to lose it, and each is the sim refusing to take the client's
        // word for something. A non-finite or over-long offset is rejected outright
        // — the client is describing a body somewhere its position is not, and
        // there is a limit to how far that may be. Asking to move ends the sit, so
        // sitting cannot become a way to walk around displaced. And flight ends it
        // for the same reason gravity does: nothing is under you.
        av.seated = seated.filter(|o| {
            o[0].is_finite()
                && o[1].is_finite()
                && (o[0] * o[0] + o[1] * o[1]) <= MAX_SEAT_OFFSET_M * MAX_SEAT_OFFSET_M
        });
        if mag > 1e-4 || flying || jump {
            av.seated = None;
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
            // Flight rides altitude (`motion::fly_altitude_factor`), applied here
            // at integration rather than to the stored velocity: the factor moves
            // with `y` between intents, and the wire's wish encoding saturates at
            // `MAX_SPEED` so it could never carry the ramped speed anyway. The
            // client predicts through the same function.
            let lift = if av.flying {
                motion::fly_altitude_factor(av.position.y)
            } else {
                1.0
            };
            av.position.x += av.velocity.x * lift * dt;
            av.position.z += av.velocity.z * lift * dt;
            // Flight holds altitude; everything else is on the vertical arc, which
            // is `motion::step_fall` — the same function the client predicts with
            // and the `--offline` path integrates, so the three cannot disagree.
            if av.flying {
                // Ramp the thrust rather than assigning it: a key press used to be
                // a step in the world velocity, which at altitude (where `lift`
                // multiplies it) is a discontinuity of hundreds of metres per
                // second. `motion::step_fly_vertical` is the shared definition, so
                // the client predicts the same curve.
                av.fly_vertical =
                    motion::step_fly_vertical(av.fly_vertical, av.fly_vertical_target, dt);
                av.position.y += av.fly_vertical * lift * dt;
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

    /// Every prim and every avatar, to every client.
    ///
    /// **There is no area-of-interest filter here, and removing it was the
    /// honest outcome of retiring regions rather than a regression.** ADR-012's
    /// v0 filter excluded whole *regions* by the distance from `self.observer`
    /// to a synthetic grid origin — and `observer` was one field on a world
    /// shared by every connection, written by whichever client sent the last
    /// `ObserverUpdate`, while one snapshot is broadcast to all. So what it did
    /// in practice was blank every prim for *everybody* as soon as one client
    /// walked 500 m from the origin of a grid that no longer corresponds to
    /// anything. A real filter has to be computed per connection, which is a
    /// different piece of work: the snapshot would have to stop being a
    /// broadcast.
    pub fn snapshot(&self, tick: u64) -> NetMessage {
        let prims: Vec<PrimDto> = self.prims.clone();

        // v0 multiplayer: replicate every avatar to all clients.
        let avatars: Vec<AvatarStateDto> = self
            .avatars
            .iter()
            .map(|(&id, a)| AvatarStateDto {
                id,
                position: a.position,
                yaw: a.yaw,
                emote: a.emote,
                seated: a.seated,
            })
            .collect();

        NetMessage::WorldSnapshot {
            tick,
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
        position: Vec3,
        shape: &str,
        geo: GeoAnchor,
        particles: Option<&ParticleEffect>,
        model: Option<&str>,
    ) -> Result<PrimDto, String> {
        // Capacity is checked *before* the insert, so the refusal costs no write
        // and the id sequence does not advance on a rejected create.
        let used = self.prims.len() as u32;
        if used >= self.caps.max_prims {
            return Err(format!(
                "the world is full: {used} of {} prims. Delete something or ask the operator to raise the budget.",
                self.caps.max_prims
            ));
        }
        // Call db::insert_prim with a minimal lock
        let prim = {
            let conn_guard = self
                .conn
                .lock()
                .map_err(|e| format!("failed to acquire db lock: {}", e))?;
            crate::db::insert_prim(&conn_guard, position, shape, geo, particles, model)
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
        particles: Option<&ParticleEffect>,
        model: Option<&str>,
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
                particles,
                model,
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
            self.owners.remove(&prim_id);
            self.dirty.remove(&prim_id);
        }
        Ok(deleted)
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

    /// Does a prim by this id exist?
    #[must_use]
    pub fn has_prim(&self, prim_id: i64) -> bool {
        self.prims.iter().any(|p| p.id == prim_id)
    }

    /// Budget and usage for the world, for [`NetMessage::WorldCapacity`].
    #[must_use]
    pub fn capacity(&self) -> CapacityUsage {
        let scripts_used = self
            .prims
            .iter()
            .filter(|p| self.scripted.contains(&p.id))
            .count() as u32;
        CapacityUsage {
            prims_used: self.prims.len() as u32,
            prims_max: self.caps.max_prims,
            scripts_used,
            scripts_max: self.caps.max_scripts,
        }
    }

    /// Who owns a prim, if anybody does.
    #[must_use]
    pub fn prim_owner(&self, prim_id: i64) -> Option<&str> {
        self.owners.get(&prim_id).map(String::as_str)
    }

    /// Record a prim's owner, after it has been written to the database.
    pub fn set_owner(&mut self, prim_id: i64, owner: &str) {
        self.owners.insert(prim_id, owner.to_string());
    }

    /// May `principal` change or delete this prim?
    ///
    /// Three rules, and the middle one is the reason this exists at all.
    ///
    /// An **admin always may** — the override, and the thing that keeps a build
    /// whose author has gone from being permanently unmaintainable.
    ///
    /// An **owned prim is its owner's**, and nobody else's. Before this,
    /// `UpdatePrim` and `DeletePrim` were open to any connected client, so
    /// anything anybody built could be moved, recoloured, rescripted or deleted
    /// by anybody else.
    ///
    /// An **unowned prim belongs to the server**, so only an admin may touch it.
    /// That is everything built before schema V15 and everything a content pack
    /// installs, and it is the deliberate side of a real trade: the alternative —
    /// treating "no owner" as "anybody's" — would leave the firebird and the
    /// beacon deletable by any passer-by. An operator re-homes a genuinely
    /// orphaned build with `POST /api/content/transfer`.
    #[must_use]
    pub fn may_edit(&self, prim_id: i64, principal: &str, is_admin: bool) -> bool {
        if is_admin {
            return true;
        }
        self.owners.get(&prim_id).is_some_and(|o| o == principal)
    }

    /// Record that a prim gained or lost a script, after the script budget has
    /// been checked with [`SimWorld::can_add_script`].
    pub fn set_scripted(&mut self, prim_id: i64, scripted: bool) {
        if scripted {
            self.scripted.insert(prim_id);
        } else {
            self.scripted.remove(&prim_id);
        }
        // And on the prim itself, which is what clients are sent (protocol 21).
        // `db::row_to_prim` reads the same fact out of `prim_scripts` on every
        // load, so this is the in-memory copy catching up rather than a second
        // source of truth — but without it a freshly scripted prim would not be
        // pressable until the server restarted.
        if let Some(p) = self.prims.iter_mut().find(|p| p.id == prim_id) {
            p.scripted = scripted;
        }
    }

    /// Whether `prim_id` may take a script. Replacing an existing one is always
    /// allowed — it consumes no additional budget.
    pub fn can_add_script(&self, prim_id: i64) -> Result<(), String> {
        if self.scripted.contains(&prim_id) {
            return Ok(());
        }
        if !self.has_prim(prim_id) {
            return Err(format!("prim {prim_id} not found"));
        }
        let usage = self.capacity();
        if usage.scripts_used >= usage.scripts_max {
            return Err(format!(
                "the world is at its script limit: {} of {}",
                usage.scripts_used, usage.scripts_max
            ));
        }
        Ok(())
    }

    /// Read-only views of every scripted prim, handed to the host each tick.
    ///
    /// The neighbours are computed here rather than answered on demand from the
    /// script thread, and that is the whole shape of `vfSensor`: the host thread
    /// holds no reference to `SimWorld` (that is what keeps a script off the tick's
    /// write lock — see `scripting`'s module note), so anything a script can ask
    /// about has to have been pushed to it at the barrier. A sensor therefore
    /// reads a snapshot that is at most one tick old, which is the same freshness
    /// `vfGetPos` has always had.
    pub fn script_views(&self) -> Vec<(i64, PrimView)> {
        // Every avatar that has told us where on Earth it is. Gathered once for
        // the whole pass: a world with 200 scripted prims and 20 avatars would
        // otherwise rebuild this list 200 times a tick.
        let located: Vec<(u64, GeoAnchor, String)> = self
            .avatars
            .iter()
            .filter_map(|(id, a)| a.geo.map(|g| (*id, g, a.name.clone())))
            .collect();
        self.prims
            .iter()
            .filter(|p| self.scripted.contains(&p.id))
            .map(|p| {
                let mut nearby: Vec<(u64, String, f32)> = located
                    .iter()
                    .map(|(id, g, name)| (*id, name.clone(), ground_distance_m(p.geo, *g)))
                    .filter(|(_, _, d)| d.is_finite() && *d <= SENSOR_MAX_RANGE_M)
                    .collect();
                // Nearest first, so `vfSensor(range)` is a prefix scan and the
                // truncation below drops the ones a script is least likely to
                // have been asking about.
                nearby.sort_by(|a, b| a.2.total_cmp(&b.2));
                nearby.truncate(SENSOR_MAX_RESULTS);
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
                        particles: p.particles,
                        geo: p.geo,
                        nearby,
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
            ScriptCommand::SetGeo {
                prim_id,
                latitude,
                longitude,
            } => {
                let geo = GeoAnchor {
                    latitude: *latitude,
                    longitude: *longitude,
                };
                (*prim_id, Box::new(move |p: &mut PrimDto| p.geo = geo))
            }
            ScriptCommand::SetParticles { prim_id, effect } => {
                let effect = effect.map(|e| e.sanitized());
                (
                    *prim_id,
                    Box::new(move |p: &mut PrimDto| p.particles = effect),
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
        // Lock before draining: a poisoned lock must leave the set intact for
        // the next flush, exactly as the doc above promises.
        let Ok(conn) = self.conn.lock() else {
            tracing::warn!("db lock poisoned; script writes not persisted");
            return;
        };
        let ids: Vec<i64> = self.dirty.drain().collect();
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
                p.particles.as_ref(),
                p.model.as_deref(),
            ) {
                tracing::warn!(prim_id = id, "persisting script write failed: {e:#}");
                // Keep the promise above: a failed write stays dirty, so a
                // transiently locked or full database is retried on the next
                // flush instead of silently rewinding the prim on restart.
                self.dirty.insert(id);
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
        let prims = crate::db::load_world(&conn).expect("load world");
        let world = SimWorld::new(
            prims,
            Arc::new(Mutex::new(conn)),
            crate::config::SimConfig::default().world_caps(),
            HashSet::new(),
            HashMap::new(),
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
            world.apply_intent(id, x, z, yaw, false, false, false, false, None, None, None);
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
        world.apply_intent(
            id, 0.0, 1.0, 0.0, false, false, false, false, None, None, None,
        );
        world.step(0.05);
        let p = world.avatar_position(id).expect("still there");
        assert!(p.is_finite());
        assert!(
            (p.z - start.z).abs() > 1e-4,
            "a valid intent should have moved the avatar"
        );
    }

    /// The seat is the one thing a client may use to draw its body away from the
    /// position this sim holds for it, so the sim polices it: it must be a real,
    /// short offset, and it does not survive the same client asking to move.
    ///
    /// Without the second rule "sitting" is a way to walk around a metre or two
    /// displaced from your own position — invisible to anything that reasons
    /// about where an avatar is, because that is still the honest number.
    #[test]
    fn a_seat_is_bounded_and_does_not_survive_moving() {
        let (mut world, _dir) = test_world();
        let id = world.spawn_avatar();
        let seated = |w: &SimWorld| {
            let NetMessage::WorldSnapshot { avatars, .. } = w.snapshot(0) else {
                panic!("snapshot")
            };
            avatars.iter().find(|a| a.id == id).expect("in view").seated
        };

        // An ordinary seat: still, and within reach of a bench.
        world.apply_intent(
            id,
            0.0,
            0.0,
            0.0,
            false,
            false,
            false,
            false,
            None,
            Some([0.4, -0.9]),
            None,
        );
        assert_eq!(seated(&world), Some([0.4, -0.9]));

        // Asking to move ends it, in the same intent that asks.
        world.apply_intent(
            id,
            0.0,
            1.0,
            0.0,
            false,
            false,
            false,
            false,
            None,
            Some([0.4, -0.9]),
            None,
        );
        assert_eq!(seated(&world), None, "walking away should end the sit");

        // So does taking off, and so does a jump.
        world.apply_intent(
            id,
            0.0,
            0.0,
            0.0,
            false,
            false,
            false,
            false,
            None,
            Some([0.4, -0.9]),
            None,
        );
        world.apply_intent(
            id,
            0.0,
            0.0,
            0.0,
            true,
            false,
            false,
            false,
            None,
            Some([0.4, -0.9]),
            None,
        );
        assert_eq!(seated(&world), None, "flying should end the sit");
        world.apply_intent(
            id,
            0.0,
            0.0,
            0.0,
            false,
            false,
            false,
            false,
            None,
            Some([0.4, -0.9]),
            None,
        );
        world.apply_intent(
            id,
            0.0,
            0.0,
            0.0,
            false,
            false,
            false,
            true,
            None,
            Some([0.4, -0.9]),
            None,
        );
        assert_eq!(seated(&world), None, "jumping should end the sit");

        // Nothing a client can put on the wire draws its body across the street,
        // and nothing it can put there is not a number.
        for bad in [
            [50.0, 0.0],
            [0.0, -50.0],
            [f32::INFINITY, 0.0],
            [f32::NAN, f32::NAN],
        ] {
            world.apply_intent(
                id,
                0.0,
                0.0,
                0.0,
                false,
                false,
                false,
                false,
                None,
                Some(bad),
                None,
            );
            assert_eq!(seated(&world), None, "{bad:?} should have been refused");
        }

        // And the position itself never moved through any of it.
        assert!(world.avatar_position(id).expect("spawned").is_finite());
    }

    /// A teleport (protocol 17) *places* the avatar: position set, motion
    /// cleared, and the placement survives a tick — the whole point is that the
    /// next snapshot confirms the jump instead of pulling the avatar back.
    #[test]
    fn a_teleport_places_the_avatar_and_the_placement_survives_a_tick() {
        let (mut world, _dir) = test_world();
        let id = world.spawn_avatar();
        // Walking when the jump lands: the carried velocity must not slide the
        // avatar off the destination.
        world.apply_intent(
            id, 0.0, 1.0, 0.0, false, false, false, false, None, None, None,
        );
        world.step(0.05);

        let dest = Vec3::new(1234.5, 0.05, -987.0);
        world.teleport_avatar(id, dest);
        assert_eq!(world.avatar_position(id), Some(dest));
        world.step(0.05);
        let p = world.avatar_position(id).expect("still there");
        assert!(
            (p.x - dest.x).abs() < 1e-3 && (p.z - dest.z).abs() < 1e-3,
            "carried velocity slid the avatar to {p:?} after a teleport to {dest:?}"
        );

        // The same hostility rule as intents: a non-finite placement is
        // discarded whole, and a below-ground one is clamped to the floor.
        world.teleport_avatar(id, Vec3::new(f32::NAN, 0.0, 0.0));
        assert_eq!(world.avatar_position(id), Some(p), "NaN must move nothing");
        world.teleport_avatar(id, Vec3::new(0.0, -50.0, 0.0));
        let sunk = world.avatar_position(id).expect("still there");
        assert!(sunk.y >= 0.0, "teleported underground: {sunk:?}");
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

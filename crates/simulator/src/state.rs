use cartopolis_core::{
    snap_yaw_continuation, AvatarStateDto, Emote, EmoteState, GeoAnchor, HandPose, NetMessage,
    PrimBody, PrimDto, PrimSound, ToneSpec, EMOTE_REPLICATE_SECS, MAX_SEAT_OFFSET_M,
};
use glam::Vec3;
use std::collections::{HashMap, HashSet};

use crate::physics::{AvatarProbe, Kick, Physics, MAX_PHYSICAL_PRIMS};
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
    /// What [`SimWorld::step`] actually moved this avatar by, per second, on its
    /// last tick — replicated as `AvatarStateDto::velocity`. Measured from the
    /// position change rather than assembled from `velocity`, `fly_vertical` and
    /// `vertical_velocity`, so the flight lift and the ground clamp are already
    /// in it: an avatar landing from a jump reports zero, not the fall rate it
    /// had before the floor stopped it.
    moved: Vec3,
    /// The account this avatar is signed in as, for anything that has to *name*
    /// it — today that is `sensor`, which hands a script the name so a prim can
    /// greet whoever walked up. Set once at the handshake; "Someone" until then,
    /// which is what a connection that has not authenticated yet is.
    name: String,
    /// Where this avatar is on Earth, as its own client last said (protocol 20).
    ///
    /// The sim cannot compute this and does not use it to move anything: it is
    /// the only frame in which "is this avatar near that prim?" has an answer
    /// (see [`cartopolis_core::NetMessage::ClientIntent`]'s `geo`), and
    /// `sensor` is the one consumer. `None` until a client that knows says so —
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
    /// Every prim, in id order.
    prims: Vec<PrimDto>,
    /// Prim id -> position in `prims`.
    ///
    /// Every edit, permission check, script write and touch looks a prim up by
    /// id, and a linear scan of a 1,000-prim world is a thousand comparisons per
    /// gizmo frame per dragging client, under the write lock. Kept in step by the
    /// three methods that change the list's shape — [`Self::commit_prim`],
    /// [`Self::forget_prim`] and [`Self::new`] — and by nothing else, because
    /// nothing else inserts or removes.
    index: HashMap<i64, usize>,
    avatars: HashMap<u64, AvatarSim>,
    next_avatar_id: u64,
    /// Creates that have claimed room against the budget and are inserting
    /// their row with this lock released ([`Self::reserve_prim`]).
    pending_prims: u32,
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
    /// Prims a script deleted (`die`) whose rows are still to be removed —
    /// the deletion half of `dirty` ([`Self::retire_prim`]).
    retired: HashSet<i64>,
    /// Who has been near whom, by lowercase username pair (sorted), for the
    /// "people you have been near" request setting (`contacts`). Pruned to
    /// [`MET_WINDOW_MS`] as it is written. In memory on purpose: it is an hour of
    /// who-stood-where, which is exactly the kind of thing not to keep on disk.
    met: HashMap<(String, String), cartopolis_core::MetDto>,
    /// Seconds since the last pass over [`Self::met`].
    meet_clock: f32,
    /// The rigid-body step for physical prims (`physics.rs`).
    physics: Physics,
    /// What the last [`Self::step`] moved, for the tick loop to send as one
    /// `PrimMotion` ([`Self::take_motion`]).
    motion: Vec<PrimBody>,
}

/// How far past a prim's `hand_range` a reported hand is still accepted: the
/// client tests the range against its own copy of the prim, which can lag the
/// script's by a tick, and a hand on the boundary should not flicker.
const HAND_RANGE_SLACK_M: f32 = 0.25;

/// Two avatars this close count as having met (`contacts`). Twice the Here
/// chat's say radius: close enough to have been talking, loose enough that
/// walking past each other on a square counts.
pub const MEET_RADIUS_M: f32 = 2.0 * crate::chat::SAY_RADIUS_M;

/// How long having been near somebody lets them send you a request.
pub const MET_WINDOW_MS: u64 = 60 * 60 * 1000;

/// How often the meetings are measured. Every avatar against every other is
/// quadratic, and nobody meets anybody in under a couple of seconds.
const MEET_SCAN_SECS: f32 = 2.0;

fn wall_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn pair(a: &str, b: &str) -> (String, String) {
    let (a, b) = (a.to_lowercase(), b.to_lowercase());
    if a <= b {
        (a, b)
    } else {
        (b, a)
    }
}

/// Furthest a `sensor` may see, whatever a script asks for.
///
/// A cap on the *gathering*, not on the question: [`SimWorld::script_views`]
/// measures every located avatar against every scripted prim once a tick, and
/// without a ceiling a script could ask about a continent and get one. It is
/// generous next to any plausible in-world use and stingy next to a planet.
const SENSOR_MAX_RANGE_M: f32 = 4_096.0;

/// Most neighbours carried to one script per tick. `sensor` returns the nearest
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
    /// A world holding `prims`, as loaded from the database.
    ///
    /// Takes no connection, and that is deliberate: nothing in here does I/O, so
    /// nothing done under the world lock can wait on the disk. The database is
    /// `crate::prims::PrimService`'s.
    pub fn new(
        mut prims: Vec<PrimDto>,
        caps: WorldCaps,
        scripted: HashSet<i64>,
        owners: HashMap<i64, String>,
    ) -> Self {
        // `db::load_world` already orders by id; sorting again is what keeps the
        // `partition_point` in `commit_prim` right for any caller.
        prims.sort_by_key(|p| p.id);
        let index = prims.iter().enumerate().map(|(i, p)| (p.id, i)).collect();
        Self {
            prims,
            index,
            avatars: HashMap::new(),
            next_avatar_id: 1,
            pending_prims: 0,
            caps,
            scripted,
            owners,
            dirty: HashSet::new(),
            retired: HashSet::new(),
            met: HashMap::new(),
            meet_clock: 0.0,
            physics: Physics::new(),
            motion: Vec::new(),
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
                moved: Vec3::ZERO,
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

    /// Who an avatar is signed in as, once it has said.
    #[must_use]
    pub fn avatar_name(&self, id: u64) -> Option<String> {
        self.avatars
            .get(&id)
            .filter(|a| a.name != "Someone")
            .map(|a| a.name.clone())
    }

    /// Where and when `a` and `b` were last near each other, within the last
    /// [`MET_WINDOW_MS`].
    #[must_use]
    pub fn met(&self, a: &str, b: &str) -> Option<cartopolis_core::MetDto> {
        let m = self.met.get(&pair(a, b))?;
        (wall_ms().saturating_sub(m.at_ms) <= MET_WINDOW_MS).then_some(*m)
    }

    /// Metres between two signed-in people's avatars now, if both are in the
    /// world and located.
    #[must_use]
    pub fn distance_between(&self, a: &str, b: &str) -> Option<f32> {
        let geo_of = |n: &str| {
            self.avatars
                .values()
                .find(|av| av.name.eq_ignore_ascii_case(n))
                .and_then(|av| av.geo)
        };
        let d = ground_distance_m(geo_of(a)?, geo_of(b)?);
        d.is_finite().then_some(d)
    }

    /// Where each of `names` has an avatar in the world with a location: the
    /// avatar's id and where it is on Earth. A name with no located avatar is
    /// left out; one signed in on two devices answers with the first found.
    #[must_use]
    pub fn whereabouts(&self, names: &[String]) -> Vec<(String, u64, GeoAnchor)> {
        names
            .iter()
            .filter_map(|n| {
                self.avatars
                    .iter()
                    .filter(|(_, av)| av.name.eq_ignore_ascii_case(n))
                    .find_map(|(id, av)| Some((n.clone(), *id, av.geo?)))
            })
            .collect()
    }

    /// Everybody within [`MEET_RADIUS_M`] of `name`'s avatar, nearest first.
    #[must_use]
    pub fn near(&self, name: &str) -> Vec<(String, f32)> {
        let Some(me) = self
            .avatars
            .values()
            .find(|av| av.name.eq_ignore_ascii_case(name))
            .and_then(|av| av.geo)
        else {
            return Vec::new();
        };
        let mut out: Vec<(String, f32)> = self
            .avatars
            .values()
            .filter(|av| av.name != "Someone" && !av.name.eq_ignore_ascii_case(name))
            .filter_map(|av| Some((av.name.clone(), ground_distance_m(me, av.geo?))))
            .filter(|(_, d)| *d <= MEET_RADIUS_M)
            .collect();
        out.sort_by(|a, b| a.1.total_cmp(&b.1));
        out.dedup_by(|a, b| a.0.eq_ignore_ascii_case(&b.0));
        out
    }

    /// Record every pair of located avatars within [`MEET_RADIUS_M`] of each
    /// other, and forget meetings older than the window.
    fn note_meetings(&mut self) {
        let now = wall_ms();
        let located: Vec<(&str, GeoAnchor)> = self
            .avatars
            .values()
            .filter(|a| a.name != "Someone")
            .filter_map(|a| Some((a.name.as_str(), a.geo?)))
            .collect();
        for (i, (a, ga)) in located.iter().enumerate() {
            for (b, gb) in &located[i + 1..] {
                if a.eq_ignore_ascii_case(b) || ground_distance_m(*ga, *gb) > MEET_RADIUS_M {
                    continue;
                }
                self.met.insert(
                    pair(a, b),
                    cartopolis_core::MetDto {
                        at_ms: now,
                        latitude: (ga.latitude + gb.latitude) / 2.0,
                        longitude: (ga.longitude + gb.longitude) / 2.0,
                    },
                );
            }
        }
        self.met
            .retain(|_, m| now.saturating_sub(m.at_ms) <= MET_WINDOW_MS);
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
        // A placement is not travel: a receiver extrapolating the pre-jump walk
        // would draw the avatar strolling on from where it landed.
        av.moved = Vec3::ZERO;
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
            // The kick emote is also a kick: a physical prim at the avatar's feet
            // is sent the way it faces (`physics::Physics::kick`).
            if e == Emote::Kick {
                if let Some(g) = av.geo {
                    self.physics.kick(Kick {
                        geo: g,
                        feet_y: av.position.y,
                        yaw: av.yaw,
                    });
                }
            }
        }
    }

    pub fn step(&mut self, dt: f32) {
        self.meet_clock += dt;
        if self.meet_clock >= MEET_SCAN_SECS {
            self.meet_clock = 0.0;
            self.note_meetings();
        }
        for av in self.avatars.values_mut() {
            let before = av.position;
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
            if dt > 0.0 {
                av.moved = (av.position - before) / dt;
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

        // Physical prims, after the avatars have moved so a push uses where they
        // are now. Avatars take part by their geographic fix, the only frame they
        // share with a prim; one that has not sent one cannot push anything.
        let probes: Vec<AvatarProbe> = self
            .avatars
            .iter()
            .filter_map(|(&id, a)| {
                a.geo.map(|geo| AvatarProbe {
                    id,
                    geo,
                    feet_y: a.position.y,
                })
            })
            .collect();
        let moved = self.physics.step(dt, &mut self.prims, &probes);
        // Persisted on the same timer as a script's writes, so a ball left in the
        // goal is still in the goal after a restart.
        self.dirty.extend(moved.iter().map(|b| b.id));
        self.motion.extend(moved);
    }

    /// The physical prims the steps since the last call moved, one entry per
    /// prim (its latest pose), for one `PrimMotion`.
    pub fn take_motion(&mut self) -> Vec<PrimBody> {
        let mut out = std::mem::take(&mut self.motion);
        // Two steps between takes leave two entries for one prim; keep the last.
        out.reverse();
        let mut seen = HashSet::new();
        out.retain(|b| seen.insert(b.id));
        out.sort_unstable_by_key(|b| b.id);
        out
    }

    /// How many prims are physical now.
    #[must_use]
    pub fn physical_count(&self) -> usize {
        self.prims.iter().filter(|p| p.physical).count()
    }

    /// Switch a prim's physics on or off in memory. Refused (`Err`) when turning
    /// it on would put the world over [`MAX_PHYSICAL_PRIMS`].
    pub fn set_physical(&mut self, prim_id: i64, on: bool) -> Result<Option<PrimDto>, String> {
        let Some(&pos) = self.index.get(&prim_id) else {
            return Ok(None);
        };
        if on && !self.prims[pos].physical && self.physical_count() >= MAX_PHYSICAL_PRIMS {
            return Err(format!(
                "this world already has {MAX_PHYSICAL_PRIMS} physical prims, the most it may hold"
            ));
        }
        self.prims[pos].physical = on;
        Ok(Some(self.prims[pos].clone()))
    }

    /// How fast a physical prim is moving (`get_vel`).
    #[must_use]
    pub fn prim_velocity(&self, prim_id: i64) -> Vec3 {
        self.physics.velocity(prim_id)
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
    /// The full world: every prim and every avatar.
    ///
    /// Sent **once per connection**, at the handshake. See
    /// [`Self::avatar_snapshot`] for why the repeating one carries no prims.
    pub fn snapshot(&self, tick: u64) -> NetMessage {
        NetMessage::WorldSnapshot {
            tick,
            prims: self.prims.clone(),
            avatars: self.avatar_dtos(),
        }
    }

    /// The repeating snapshot: avatars only, with an empty prim list.
    ///
    /// # Why the prims are not in here
    ///
    /// The client applies a snapshot's prim list on the *first* one it receives
    /// and ignores it on every one after that (`network.rs`, the
    /// `received_initial_world` branch) — prims change through `PrimUpsert` and
    /// `PrimRemoved`, which is what the tick loop already sends. So the whole
    /// list was being cloned out of the world, postcard-encoded, copied once per
    /// connected client and then discarded on arrival, twenty times a second,
    /// for as long as anyone was connected. At the default 1,000-prim ceiling
    /// that is the largest single thing this server did per tick and none of it
    /// reached a screen.
    ///
    /// This is deliberately **not** a wire change: `WorldSnapshot` still carries
    /// a `prims` field and a connection still receives a full one. It moved from
    /// the broadcast to the handshake, where the only client that needs it is.
    /// An older client sees exactly what it saw before, because "the prim list in
    /// a repeat snapshot" is a thing no client has ever read.
    ///
    /// One consequence to know, and it predates this: a client whose broadcast
    /// receiver falls behind drops frames as `Lagged`, and a dropped `PrimUpsert`
    /// is never resent. The repeating full prim list looked like it would repair
    /// that and never did, since the client discards it. If prim resync becomes a
    /// real problem, the fix is to notice the lag and resend, not to go back to
    /// broadcasting the world.
    pub fn avatar_snapshot(&self, tick: u64) -> NetMessage {
        NetMessage::WorldSnapshot {
            tick,
            prims: Vec::new(),
            avatars: self.avatar_dtos(),
        }
    }

    /// v0 multiplayer: replicate every avatar to all clients.
    fn avatar_dtos(&self) -> Vec<AvatarStateDto> {
        self.avatars
            .iter()
            .map(|(&id, a)| AvatarStateDto {
                id,
                position: a.position,
                yaw: a.yaw,
                emote: a.emote,
                seated: a.seated,
                velocity: a.moved,
            })
            .collect()
    }

    // -----------------------------------------------------------------------
    // Prim storage — in memory only
    //
    // Nothing below touches SQLite, and that is the point of the shape: the world
    // is behind one write lock that the tick loop takes twenty times a second, so
    // a write that waited on the disk while holding it stalled every connected
    // client at once. The database half of each operation lives in
    // `crate::prims::PrimService`, which does it with this lock released and then
    // comes back here to make the in-memory copy agree.
    // -----------------------------------------------------------------------

    /// Claim room for one prim that is about to be inserted into the database.
    ///
    /// Capacity is checked *before* the insert, so the refusal costs no write and
    /// the id sequence does not advance on a rejected create. It is a
    /// reservation rather than a bare check because the insert now happens with
    /// the world lock released: two creates that both checked `len()` and then
    /// both inserted would put the world one prim over its budget. Every
    /// reservation ends in exactly one [`Self::commit_prim`] or
    /// [`Self::release_prim_reservation`].
    pub fn reserve_prim(&mut self) -> Result<(), String> {
        let used = self.prims.len() as u32 + self.pending_prims;
        if used >= self.caps.max_prims {
            return Err(format!(
                "the world is full: {used} of {} prims. Delete something or ask the operator to raise the budget.",
                self.caps.max_prims
            ));
        }
        self.pending_prims += 1;
        Ok(())
    }

    /// Give back a reservation whose insert did not happen.
    pub fn release_prim_reservation(&mut self) {
        self.pending_prims = self.pending_prims.saturating_sub(1);
    }

    /// Add a prim the database has just stored, spending its reservation.
    ///
    /// Inserted in id order rather than pushed: [`Self::prims`] promises id
    /// order, and two creates that finished their inserts in one order may reach
    /// here in the other.
    pub fn commit_prim(&mut self, mut prim: PrimDto, owner: Option<&str>) {
        self.release_prim_reservation();
        prim.scripted = self.scripted.contains(&prim.id);
        if let Some(owner) = owner {
            self.owners.insert(prim.id, owner.to_string());
        }
        if let Some(&pos) = self.index.get(&prim.id) {
            // Cannot happen — the id is the database's and was never here — but
            // replacing is the answer that leaves the index honest if it does.
            self.prims[pos] = prim;
            return;
        }
        let pos = self.prims.partition_point(|p| p.id < prim.id);
        self.prims.insert(pos, prim);
        self.reindex_from(pos);
    }

    /// Replace a prim's in-memory copy with what the database now holds for it.
    ///
    /// `false` when the prim is no longer in the world — a script can retire one
    /// (`die`) between an edit's database write and this call, and a caller
    /// that broadcast the edit anyway would hand every client a resurrected copy.
    pub fn replace_prim(&mut self, mut prim: PrimDto) -> bool {
        let Some(&pos) = self.index.get(&prim.id) else {
            return false;
        };
        // The in-memory flag is the one maintained beside the script store, so it
        // wins over whatever the row read back said.
        prim.scripted = self.scripted.contains(&prim.id);
        // A looping sound is never in the row at all (it is not persisted), so an
        // edit carries the one playing across rather than silencing it.
        prim.sound_loop = self.prims[pos].sound_loop;
        // Nor is a hand range or a tone: an edit to a prim's colour must not
        // stop it sensing hands or silence it.
        prim.hand_range = self.prims[pos].hand_range;
        prim.tone = self.prims[pos].tone;
        // Physics is switched in memory first and written after (a script's
        // `set_physical` reaches the row at the next flush), so memory wins here
        // as it does for `scripted`.
        prim.physical = self.prims[pos].physical;
        self.prims[pos] = prim;
        true
    }

    /// Drop a prim the database has already deleted.
    ///
    /// Returns whether it was here. Releases everything held against it — its
    /// script budget (`db::delete_prim` removed the script row too), its owner
    /// and any unflushed script write.
    pub fn forget_prim(&mut self, prim_id: i64) -> bool {
        let Some(pos) = self.index.remove(&prim_id) else {
            return false;
        };
        self.prims.remove(pos);
        self.reindex_from(pos);
        self.scripted.remove(&prim_id);
        self.owners.remove(&prim_id);
        self.dirty.remove(&prim_id);
        true
    }

    /// Whether a reported hand is one the prim it names wants: the prim senses
    /// hands, and this one is within its range.
    #[must_use]
    pub fn hand_in_range(&self, pose: &HandPose) -> bool {
        self.prim(pose.prim_id).is_some_and(|p| {
            p.hand_range > 0.0 && pose.distance() <= p.hand_range + HAND_RANGE_SLACK_M
        })
    }

    /// Remove a prim now and delete its row at the next flush.
    ///
    /// For `die`, which is applied inside the tick while this lock is held and
    /// so cannot wait on the database the way a person's delete does. The world
    /// is the authority — the prim is gone for every client at once — and the row
    /// follows on the same cadence script writes do; a failed delete stays queued
    /// and is retried, exactly as a failed script write stays dirty.
    pub fn retire_prim(&mut self, prim_id: i64) -> bool {
        if !self.forget_prim(prim_id) {
            return false;
        }
        self.retired.insert(prim_id);
        true
    }

    /// Re-point the id index at every prim from `from` onwards, after an insert
    /// or removal shifted them.
    fn reindex_from(&mut self, from: usize) {
        for (i, p) in self.prims.iter().enumerate().skip(from) {
            self.index.insert(p.id, i);
        }
    }

    /// A prim by id, for the lookups a script command needs (its name and where
    /// it is standing when it speaks).
    pub fn prim(&self, prim_id: i64) -> Option<&PrimDto> {
        self.index.get(&prim_id).map(|&i| &self.prims[i])
    }

    /// Whether a prim carries a script. Cheap enough to ask per touch — the set is
    /// in memory precisely so this doesn't become a query.
    pub fn is_scripted(&self, prim_id: i64) -> bool {
        self.scripted.contains(&prim_id)
    }

    /// Does a prim by this id exist?
    #[must_use]
    pub fn has_prim(&self, prim_id: i64) -> bool {
        self.index.contains_key(&prim_id)
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

    /// Is this prim `principal`'s?
    ///
    /// An **owned prim is its owner's**, and nobody else's. Before owners,
    /// `UpdatePrim` and `DeletePrim` were open to any connected client, so
    /// anything anybody built could be moved, recoloured, rescripted or deleted
    /// by anybody else.
    ///
    /// An **unowned prim belongs to the server**: nobody owns it, so only an
    /// account allowed to change anybody's prims (`prim_edit_any`; an admin)
    /// may touch it. That is everything built before schema V15 and everything a
    /// content pack installs, and it is the deliberate side of a real trade: the
    /// alternative — treating "no owner" as "anybody's" — would leave the
    /// firebird and the beacon deletable by any passer-by. An operator re-homes a
    /// genuinely orphaned build with `POST /api/content/transfer`.
    ///
    /// What an actor may *do* with a prim is its permissions' business
    /// (`prims::Actor::may_edit`, `docs/notes/permissions.md`); this answers only
    /// whose the prim is.
    #[must_use]
    pub fn owns(&self, prim_id: i64, principal: &str) -> bool {
        !principal.is_empty() && self.owners.get(&prim_id).is_some_and(|o| o == principal)
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
        if let Some(&pos) = self.index.get(&prim_id) {
            self.prims[pos].scripted = scripted;
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
    /// script thread, and that is the whole shape of `sensor`: the host thread
    /// holds no reference to `SimWorld` (that is what keeps a script off the tick's
    /// write lock — see `scripting`'s module note), so anything a script can ask
    /// about has to have been pushed to it at the barrier. A sensor therefore
    /// reads a snapshot that is at most one tick old, which is the same freshness
    /// `get_pos` has always had.
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
                // Nearest first, so `sensor(range)` is a prefix scan and the
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
                        physical: p.physical,
                        velocity: self.physics.velocity(p.id),
                        mass: crate::physics::mass_of(p),
                    },
                )
            })
            .collect()
    }

    /// Apply a script's transform/appearance write in memory.
    ///
    /// Returns the mutated prim so the caller can broadcast a `PrimUpsert`.
    /// Nothing is persisted here — see [`SimWorld::take_dirty`] and the note on
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
            ScriptCommand::SetSoundLoop { prim_id, sound } => {
                let sound = sound.map(PrimSound::sanitized);
                (
                    *prim_id,
                    Box::new(move |p: &mut PrimDto| p.sound_loop = sound),
                )
            }
            ScriptCommand::SenseHands { prim_id, range } => {
                let range = range.clamp(0.0, cartopolis_core::tone::MAX_HAND_RANGE_M);
                (
                    *prim_id,
                    Box::new(move |p: &mut PrimDto| p.hand_range = range),
                )
            }
            // Written here but broadcast by the tick loop as a `PrimTone`, not
            // an upsert: a script following a hand sets it twenty times a second.
            ScriptCommand::SetTone { prim_id, tone } => {
                let pos = *self.index.get(prim_id)?;
                self.prims[pos].tone = tone.map(ToneSpec::sanitized);
                return None;
            }
            ScriptCommand::SetPhysical { prim_id, on } => {
                return match self.set_physical(*prim_id, *on) {
                    Ok(Some(p)) => {
                        self.dirty.insert(*prim_id);
                        Some(p)
                    }
                    Ok(None) => None,
                    Err(why) => {
                        tracing::debug!(prim_id, "script set_physical refused: {why}");
                        None
                    }
                };
            }
            // Motion, not a field write: the physics step applies it, and the
            // pose it produces goes out in `PrimMotion`.
            ScriptCommand::SetVel { prim_id, velocity } => {
                self.physics.set_velocity(*prim_id, *velocity);
                return None;
            }
            ScriptCommand::ApplyImpulse { prim_id, impulse } => {
                self.physics.apply_impulse(*prim_id, *impulse);
                return None;
            }
            // Not a field write: the caller routes these to chat, to every
            // connection, to the editor, and to `remove_prim` respectively.
            ScriptCommand::Say { .. }
            | ScriptCommand::PlaySound { .. }
            | ScriptCommand::Output { .. }
            | ScriptCommand::Die { .. } => return None,
        };
        let pos = *self.index.get(&prim_id)?;
        let prim = &mut self.prims[pos];
        mutate(prim);
        let out = prim.clone();
        self.dirty.insert(prim_id);
        Some(out)
    }

    /// Hand over everything scripts have changed since the last call, for
    /// [`crate::prims::PrimService::flush`] to write with this lock released.
    ///
    /// A *copy* of each dirty prim as it stands now, not a reference to it: the
    /// write happens after the lock is gone, while scripts carry on moving the
    /// same prims, and a later change simply marks the prim dirty again for the
    /// next flush. What the batch carries is therefore always a state the world
    /// really was in, which is all a crash-recovery copy has to be.
    ///
    /// The ids are drained. A batch that does not land must be handed back with
    /// [`Self::restore_dirty`], or those writes are lost until the prim next
    /// changes — the promise `flush` makes is that a failed write is retried.
    #[must_use]
    pub fn take_dirty(&mut self) -> DirtyBatch {
        let mut rows = Vec::with_capacity(self.dirty.len());
        for id in self.dirty.drain() {
            if let Some(&pos) = self.index.get(&id) {
                rows.push(self.prims[pos].clone());
            }
        }
        DirtyBatch {
            rows,
            retired: self.retired.drain().collect(),
        }
    }

    /// Mark a batch that did not reach the database as unwritten again.
    ///
    /// An updated prim that has since left the world is not re-marked — its row
    /// is being deleted, and writing it back would be the resurrection the
    /// delete exists to prevent. A retired id always is: its row still exists.
    pub fn restore_dirty(&mut self, batch: &DirtyBatch) {
        for p in &batch.rows {
            if self.index.contains_key(&p.id) {
                self.dirty.insert(p.id);
            }
        }
        self.retired.extend(batch.retired.iter().copied());
    }
}

/// What one flush writes: prims scripts have changed, and prims scripts have
/// deleted (`die`). See [`SimWorld::take_dirty`].
#[derive(Debug, Default)]
pub struct DirtyBatch {
    pub rows: Vec<PrimDto>,
    pub retired: Vec<i64>,
}

impl DirtyBatch {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty() && self.retired.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A world loaded from a throwaway database, so the prims it holds are ones
    /// the real schema produced rather than hand-built stand-ins.
    fn test_world() -> (SimWorld, tempfile::TempDir) {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let path = dir.path().join("test.db");
        let conn = crate::db::open_and_migrate(path.to_str().unwrap()).expect("open db");
        let prims = crate::db::load_world(&conn).expect("load world");
        let world = SimWorld::new(
            prims,
            crate::config::SimConfig::default().world_caps(),
            HashSet::new(),
            HashMap::new(),
        );
        (world, dir)
    }

    /// Store a prim and add it to the world, the two halves
    /// `prims::PrimService::create` performs with the lock released between them.
    fn stored_prim(world: &mut SimWorld, dir: &tempfile::TempDir) -> PrimDto {
        let path = dir.path().join("test.db");
        let conn = crate::db::open_and_migrate(path.to_str().unwrap()).expect("open db");
        let prim =
            crate::db::insert_prim(&conn, Vec3::ZERO, "box", GeoAnchor::default(), None, None)
                .expect("insert prim");
        world.reserve_prim().expect("room for a prim");
        world.commit_prim(prim.clone(), None);
        prim
    }

    /// The id index has to follow every change to the list's shape, including a
    /// removal from the middle, which shifts every prim after it. A stale index
    /// would answer `prim(id)` with a *different* prim — the edit lands on the
    /// wrong object with nothing to say so.
    #[test]
    fn the_prim_index_follows_inserts_and_removals() {
        let (mut world, dir) = test_world();
        let a = stored_prim(&mut world, &dir);
        let b = stored_prim(&mut world, &dir);
        let c = stored_prim(&mut world, &dir);
        assert!(world.forget_prim(b.id));
        assert!(!world.has_prim(b.id));
        assert_eq!(world.prim(a.id).map(|p| p.id), Some(a.id));
        assert_eq!(world.prim(c.id).map(|p| p.id), Some(c.id));
        assert!(!world.forget_prim(b.id), "a second forget finds nothing");

        // A retirement is gone from the world at once and queued for the row.
        assert!(world.retire_prim(c.id));
        assert!(world.prim(c.id).is_none());
        let batch = world.take_dirty();
        assert_eq!(batch.retired, vec![c.id]);
        // And a failed flush hands the retirement back for the next one.
        world.restore_dirty(&batch);
        assert_eq!(world.take_dirty().retired, vec![c.id]);
        assert_eq!(world.prims().len(), 1);
    }

    /// A looping sound is never in the database, so an edit — which replaces the
    /// prim with the row read back — must carry it across rather than silence it.
    #[test]
    fn an_edit_keeps_the_loop_a_script_started() {
        use crate::scripting::ScriptCommand;
        use cartopolis_core::SoundName;
        let (mut world, dir) = test_world();
        let prim = stored_prim(&mut world, &dir);
        let sound = PrimSound {
            sound: SoundName::Bell,
            volume: 0.5,
            range_m: 40.0,
        };
        world
            .apply_script_command(&ScriptCommand::SetSoundLoop {
                prim_id: prim.id,
                sound: Some(sound),
            })
            .expect("prim is there");
        assert!(world.replace_prim(prim.clone()));
        assert_eq!(world.prim(prim.id).unwrap().sound_loop, Some(sound));
    }

    /// A hand is accepted only for a prim that senses hands and only within its
    /// range; an edit keeps the range and the tone; a tone is written to the
    /// prim but is not an upsert.
    #[test]
    fn hands_are_taken_within_range_and_tones_survive_an_edit() {
        use crate::scripting::ScriptCommand;
        let (mut world, dir) = test_world();
        let prim = stored_prim(&mut world, &dir);
        let hand = |offset| HandPose {
            prim_id: prim.id,
            offset,
            precise: true,
        };
        assert!(
            !world.hand_in_range(&hand([0.3, 0.0, 0.0])),
            "not sensing yet"
        );
        world
            .apply_script_command(&ScriptCommand::SenseHands {
                prim_id: prim.id,
                range: 2.0,
            })
            .expect("prim is there");
        assert!(world.hand_in_range(&hand([0.3, 0.0, 0.0])));
        assert!(!world.hand_in_range(&hand([3.0, 0.0, 0.0])));
        assert!(!world.hand_in_range(&hand([f32::NAN, 0.0, 0.0])));

        let tone = ToneSpec::default();
        assert!(world
            .apply_script_command(&ScriptCommand::SetTone {
                prim_id: prim.id,
                tone: Some(tone),
            })
            .is_none());
        assert_eq!(world.prim(prim.id).unwrap().tone, Some(tone));
        assert!(world.replace_prim(prim.clone()));
        let after = world.prim(prim.id).unwrap();
        assert_eq!(after.tone, Some(tone), "an edit keeps the tone");
        assert_eq!(after.hand_range, 2.0, "and the range");
    }

    /// A reservation counts against the budget before its row exists, so two
    /// creates racing through the database cannot both fit into the last slot.
    #[test]
    fn a_reservation_holds_its_place_in_the_budget() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let mut world = SimWorld::new(
            Vec::new(),
            WorldCaps {
                max_prims: 1,
                max_scripts: 1,
            },
            HashSet::new(),
            HashMap::new(),
        );
        world.reserve_prim().expect("the first create fits");
        assert!(world.reserve_prim().is_err(), "the second must not");
        world.release_prim_reservation();
        let _ = stored_prim(&mut world, &dir);
        assert!(world.reserve_prim().is_err(), "the world is full");
    }

    /// The repeating snapshot must not carry the prim list, and the handshake one
    /// must.
    ///
    /// This is the property the whole split rests on: the client builds its prim
    /// entities from the first `WorldSnapshot` and ignores the list in every one
    /// after it, so putting the world in all of them was work that reached no
    /// screen. If someone ever "fixes" `avatar_snapshot` by having it call
    /// `snapshot`, this fails rather than quietly restoring twenty full world
    /// serialisations a second.
    #[test]
    fn only_the_first_snapshot_carries_the_world() {
        let (mut world, dir) = test_world();
        let _ = stored_prim(&mut world, &dir);
        let id = world.spawn_avatar();

        let NetMessage::WorldSnapshot { prims, avatars, .. } = world.snapshot(0) else {
            panic!("snapshot")
        };
        assert_eq!(prims.len(), 1, "the handshake snapshot carries the world");
        assert!(avatars.iter().any(|a| a.id == id));

        let NetMessage::WorldSnapshot { prims, avatars, .. } = world.avatar_snapshot(1) else {
            panic!("snapshot")
        };
        assert!(
            prims.is_empty(),
            "the repeating snapshot carries no prims; they travel as PrimUpsert"
        );
        assert!(
            avatars.iter().any(|a| a.id == id),
            "it does still carry avatars — that is the whole reason it repeats"
        );
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

    /// The replicated velocity is what the tick actually moved the avatar by —
    /// so it matches the walk, is zero once a teleport has placed it, and never
    /// reports travel into the floor.
    #[test]
    fn the_snapshot_carries_the_velocity_the_tick_moved_the_avatar_by() {
        let (mut world, _dir) = test_world();
        let id = world.spawn_avatar();
        let velocity = |world: &SimWorld| match world.avatar_snapshot(0) {
            NetMessage::WorldSnapshot { avatars, .. } => {
                avatars
                    .iter()
                    .find(|a| a.id == id)
                    .expect("in the snapshot")
                    .velocity
            }
            other => panic!("not a snapshot: {other:?}"),
        };
        assert_eq!(velocity(&world), Vec3::ZERO, "a fresh avatar is at rest");

        world.apply_intent(
            id, 0.0, 1.0, 0.0, false, false, false, false, None, None, None,
        );
        let before = world.avatar_position(id).expect("spawned");
        world.step(0.05);
        let after = world.avatar_position(id).expect("spawned");
        let v = velocity(&world);
        assert!(v.length() > 0.5, "walking but reported {v:?}");
        assert!(
            ((after - before) / 0.05 - v).length() < 1e-3,
            "reported {v:?} for a move of {:?} in 50 ms",
            after - before
        );
        assert!(
            v.y.abs() < 1e-6,
            "standing on the floor, yet vertical {v:?}"
        );

        world.teleport_avatar(id, Vec3::new(500.0, 0.0, 500.0));
        assert_eq!(velocity(&world), Vec3::ZERO, "a placement is not travel");
    }
}

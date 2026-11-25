//! How a car the avatar drives moves: a bicycle model with tyres that slip.
//!
//! Pure arithmetic like [`crate::motion`], and for the same reason it lives in
//! core rather than in the client: the client runs it, the tests here pin its
//! behaviour, and nothing about it needs Bevy. The sim does **not** run it — the
//! sim has no map and so no walls to stop a car — it receives the velocity this
//! produces as an intent and integrates that, capped at
//! [`crate::motion::MAX_DRIVE_SPEED`] (`docs/notes/driving.md`).
//!
//! The model is the one every driving game that feels like a car starts from:
//!
//! - **Two axles, one wheel each** (the bicycle model). Each axle has a slip
//!   angle — the angle between where its wheel points and where it is actually
//!   going — and a tyre turns that into a sideways force that rises with the
//!   angle, peaks around 6°, and falls off past it ([`tyre_lateral`]).
//! - **The grip an axle has is the weight on it**, and braking moves weight onto
//!   the front, accelerating onto the rear. So braking into a corner tightens it
//!   and lifting off mid-corner can step the rear out.
//! - **An axle's grip is shared between going and turning** (the friction
//!   circle): a front-wheel-drive car under full throttle in a corner has less
//!   grip left to turn with, and runs wide — understeer, which is what pushing a
//!   road car too fast into a bend feels like.
//! - **An engine with gears**, a torque curve and an automatic box, so the push
//!   fades as the car gathers speed and the engine note has an rpm to follow.
//!
//! Below walking pace slip angles mean nothing (the wheel's velocity is tiny and
//! its direction is noise), so the model blends to the kinematic bicycle there:
//! the car goes exactly where its wheels point.
//!
//! Axes: the car's yaw is the avatar's (`facing = (sin yaw, cos yaw)` in world
//! XZ), so a positive yaw rate turns it toward its **left**, and a positive
//! steer, lateral velocity or lateral force is to the left too.

use std::f32::consts::PI;

/// Standard gravity (m/s²). Not [`crate::motion::GRAVITY`]: that is a game
/// constant chosen for a jump that feels right, and tyre grip wants the real one.
pub const G: f32 = 9.81;

/// Physics steps per second. The tyre forces are stiff (a slip angle of a few
/// degrees is the whole range), so a frame-rate step would ring at 30 fps.
pub const STEP_HZ: f32 = 240.0;

/// Which wheels the engine drives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Drive {
    Front,
    Rear,
}

/// A car's build: everything about it that does not change while it moves.
#[derive(Debug, Clone, PartialEq)]
pub struct CarParams {
    /// Kerb weight with a driver, kg.
    pub mass: f32,
    /// Yaw moment of inertia, kg·m².
    pub inertia: f32,
    /// Centre of mass to the front axle, m.
    pub cg_to_front: f32,
    /// Centre of mass to the rear axle, m.
    pub cg_to_rear: f32,
    /// Height of the centre of mass, m: how much weight moves under braking.
    pub cg_height: f32,
    /// Tyre rolling radius, m.
    pub wheel_radius: f32,
    /// Peak tyre friction coefficient on dry tarmac.
    pub mu: f32,
    /// Front and rear tyre stiffness: the Pacejka `B` of each axle. A larger
    /// rear than front is the road car's built-in understeer.
    pub stiffness_front: f32,
    pub stiffness_rear: f32,
    pub drive: Drive,
    /// Torque at the peak of the curve, N·m, and the rpm it is reached at.
    pub peak_torque: f32,
    pub peak_rpm: f32,
    pub idle_rpm: f32,
    pub redline_rpm: f32,
    /// Forward gear ratios, first first.
    pub gears: Vec<f32>,
    pub reverse_ratio: f32,
    pub final_drive: f32,
    /// Drag as `F = drag · v²` (½ρ·Cd·A), N·s²/m².
    pub drag: f32,
    /// Rolling resistance as `F = rolling · v`, N·s/m.
    pub rolling: f32,
    /// Most brake force the pedal can ask for, as a share of the car's weight.
    pub brake_g: f32,
    /// Front wheel lock, rad.
    pub max_steer: f32,
    /// Bumper to bumper and side to side, m: what hits walls.
    pub length: f32,
    pub width: f32,
}

impl CarParams {
    /// A small front-driven hatchback: a Polo, a Clio, a 205.
    #[must_use]
    pub fn hatchback() -> Self {
        Self {
            mass: 1150.0,
            inertia: 1500.0,
            cg_to_front: 1.0,
            cg_to_rear: 1.55,
            cg_height: 0.52,
            wheel_radius: 0.30,
            mu: 1.0,
            stiffness_front: 20.0,
            stiffness_rear: 26.0,
            drive: Drive::Front,
            peak_torque: 160.0,
            peak_rpm: 3800.0,
            idle_rpm: 850.0,
            redline_rpm: 6500.0,
            gears: vec![3.45, 1.95, 1.30, 1.00, 0.80],
            reverse_ratio: 3.3,
            final_drive: 4.1,
            drag: 0.40,
            rolling: 12.0,
            brake_g: 1.1,
            max_steer: 0.6,
            length: 4.0,
            width: 1.72,
        }
    }

    /// A rear-driven saloon: heavier, stronger, and its tail can be provoked.
    #[must_use]
    pub fn saloon() -> Self {
        Self {
            mass: 1500.0,
            inertia: 2400.0,
            cg_to_front: 1.30,
            cg_to_rear: 1.50,
            cg_height: 0.50,
            wheel_radius: 0.32,
            stiffness_front: 20.0,
            stiffness_rear: 24.0,
            drive: Drive::Rear,
            peak_torque: 300.0,
            peak_rpm: 4200.0,
            gears: vec![4.1, 2.4, 1.6, 1.2, 1.0, 0.82],
            final_drive: 3.5,
            drag: 0.36,
            length: 4.6,
            width: 1.80,
            ..Self::hatchback()
        }
    }

    /// An estate or compact SUV: a hatchback's layout, taller and heavier.
    #[must_use]
    pub fn estate() -> Self {
        Self {
            mass: 1450.0,
            inertia: 2200.0,
            cg_to_front: 1.15,
            cg_to_rear: 1.55,
            cg_height: 0.62,
            wheel_radius: 0.33,
            peak_torque: 240.0,
            gears: vec![3.6, 2.1, 1.4, 1.05, 0.85, 0.70],
            final_drive: 3.9,
            drag: 0.46,
            length: 4.5,
            width: 1.82,
            ..Self::hatchback()
        }
    }

    /// A delivery van: slow, tall and soft.
    #[must_use]
    pub fn van() -> Self {
        Self {
            mass: 2100.0,
            inertia: 4200.0,
            cg_to_front: 1.35,
            cg_to_rear: 1.85,
            cg_height: 0.85,
            wheel_radius: 0.34,
            mu: 0.95,
            stiffness_front: 18.0,
            stiffness_rear: 24.0,
            peak_torque: 330.0,
            peak_rpm: 2200.0,
            redline_rpm: 4800.0,
            gears: vec![4.5, 2.5, 1.5, 1.1, 0.85, 0.7],
            final_drive: 3.9,
            drag: 0.75,
            rolling: 18.0,
            brake_g: 0.95,
            length: 5.2,
            width: 2.0,
            ..Self::hatchback()
        }
    }

    #[must_use]
    pub fn wheelbase(&self) -> f32 {
        self.cg_to_front + self.cg_to_rear
    }

    /// Engine torque (N·m) at `rpm` on full throttle: a parabola through the
    /// peak, falling to two thirds of it at idle and at the redline.
    #[must_use]
    pub fn torque_at(&self, rpm: f32) -> f32 {
        let span = if rpm < self.peak_rpm {
            self.peak_rpm - self.idle_rpm
        } else {
            self.redline_rpm - self.peak_rpm
        };
        let x = ((rpm - self.peak_rpm) / span.max(1.0)).clamp(-1.0, 1.0);
        self.peak_torque * (1.0 - x * x / 3.0)
    }

    /// The steer lock the driver can reach at `speed` (m/s).
    ///
    /// A keyboard is on or off, so the lock has to come down with speed or a
    /// tap of A at 100 km/h would put the full 35° on and spin the car. The
    /// curve still leaves more than the tyres can take at speed — pushed hard,
    /// the car runs wide rather than turning, which is the understeer it should
    /// have, not a lock that forbids asking.
    #[must_use]
    pub fn steer_limit(&self, speed: f32) -> f32 {
        let s = speed.abs() / 14.0;
        self.max_steer / (1.0 + s * s)
    }
}

/// What the driver is doing this frame.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct CarInput {
    /// Accelerator, 0..1.
    pub throttle: f32,
    /// Brake pedal, 0..1. Held at a standstill, it engages reverse.
    pub brake: f32,
    /// Steering wish, −1 (right) .. 1 (left).
    pub steer: f32,
    /// The handbrake: locks the rear wheels.
    pub handbrake: bool,
}

/// A car in motion: everything the step changes.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct CarState {
    /// Heading, the avatar's yaw convention.
    pub yaw: f32,
    /// Velocity in the car's own frame: forward, and to the left (m/s).
    pub forward: f32,
    pub lateral: f32,
    /// Turn rate, rad/s, positive to the left.
    pub yaw_rate: f32,
    /// The front wheels' angle, rad, positive to the left.
    pub steer: f32,
    /// Gear: 1.. forward, −1 reverse.
    pub gear: i8,
    pub rpm: f32,
    /// Longitudinal acceleration last step (m/s²), for the weight transfer.
    pub accel: f32,
    /// How far each axle is past its grip, 0 (holding) .. 1 (sliding): what the
    /// tyres sound and the skid marks are drawn from.
    pub slide_front: f32,
    pub slide_rear: f32,
    /// Whether the brake lights are on.
    pub braking: bool,
    /// Seconds the car has stood still with the brake held: reverse engages
    /// past [`REVERSE_AFTER`], so a stop is not a reversal.
    pub held: f32,
}

impl CarState {
    /// A car standing at `yaw`, in first, engine idling.
    #[must_use]
    pub fn parked(yaw: f32, params: &CarParams) -> Self {
        Self {
            yaw,
            gear: 1,
            rpm: params.idle_rpm,
            ..Self::default()
        }
    }

    /// Velocity in world XZ, m/s.
    #[must_use]
    pub fn world_velocity(&self) -> [f32; 2] {
        let (s, c) = self.yaw.sin_cos();
        // forward = (sin, cos), left = (cos, −sin)
        [
            self.forward * s + self.lateral * c,
            self.forward * c - self.lateral * s,
        ]
    }

    /// Speed over the ground, m/s.
    #[must_use]
    pub fn speed(&self) -> f32 {
        self.forward.hypot(self.lateral)
    }

    /// Put a world velocity back into the car's frame — after a wall took
    /// some of it away.
    pub fn set_world_velocity(&mut self, v: [f32; 2]) {
        let (s, c) = self.yaw.sin_cos();
        self.forward = v[0] * s + v[1] * c;
        self.lateral = v[0] * c - v[1] * s;
    }
}

/// Sideways force of one axle's tyres (N), positive to the left, for a slip
/// angle `alpha` (rad) under a vertical load `load` (N) with friction `mu`.
///
/// A simplified Pacejka "magic formula", `D·sin(C·atan(B·α))`: linear for small
/// angles, peaking near 7° for a `B` of 20, and falling to about 90 % past it — a
/// tyre that has let go grips less than one that is about to, which is what
/// makes a slide a thing to catch rather than a ceiling to lean on.
#[must_use]
pub fn tyre_lateral(alpha: f32, load: f32, mu: f32, stiffness: f32) -> f32 {
    const C: f32 = 1.3;
    -mu * load * (C * (stiffness * alpha).atan()).sin()
}

/// The slip angle at which [`tyre_lateral`] peaks, rad.
#[must_use]
pub fn peak_slip(stiffness: f32) -> f32 {
    const C: f32 = 1.3;
    (PI / (2.0 * C)).tan() / stiffness
}

/// Below this forward speed (m/s) the car follows its wheels exactly; above
/// twice it, the tyres alone decide. Between, a blend.
const KINEMATIC_BELOW: f32 = 1.5;

/// Seconds the wheels take to reach the lock the speed allows: a hand on a
/// wheel, not a switch. In proportion to the lock, so at speed the small lock
/// is reached as gently as the full one in a car park.
const STEER_SECS: f32 = 0.3;
/// …and back to straight when let go, a bit quicker — the caster's pull.
const RETURN_SECS: f32 = 0.2;

/// Shift up past this share of the redline, down below this rpm.
const UPSHIFT_AT: f32 = 0.88;
const DOWNSHIFT_RPM: f32 = 1700.0;

/// Below this speed a held brake with no throttle brings the car to rest, and
/// at rest it engages reverse.
const STOPPED: f32 = 0.4;

/// How much a tyre's friction falls per unit of load over its share of the
/// car's weight.
const LOAD_SENSITIVITY: f32 = 0.25;

/// Seconds at rest with the brake held before it selects reverse.
pub const REVERSE_AFTER: f32 = 0.35;

/// Advance `state` by `dt` seconds of `input`, on a surface of friction
/// `grip` (1 on dry tarmac, ~0.6 on grass).
pub fn step(state: &mut CarState, params: &CarParams, input: CarInput, grip: f32, dt: f32) {
    if !(dt > 0.0) {
        return;
    }
    let substeps = (dt * STEP_HZ).ceil().clamp(1.0, 64.0) as usize;
    let h = dt / substeps as f32;
    for _ in 0..substeps {
        substep(state, params, input, grip, h);
    }
}

fn substep(state: &mut CarState, p: &CarParams, input: CarInput, grip: f32, h: f32) {
    let throttle = input.throttle.clamp(0.0, 1.0);
    let brake = input.brake.clamp(0.0, 1.0);
    let wish = input.steer.clamp(-1.0, 1.0);

    // ── The wheel ────────────────────────────────────────────────────────────
    let limit = p.steer_limit(state.forward);
    let target = wish * limit;
    let rate = limit
        / if wish.abs() < 1e-3 {
            RETURN_SECS
        } else {
            STEER_SECS
        };
    let d = target - state.steer;
    state.steer += d.clamp(-rate * h, rate * h);

    // ── The gearbox ──────────────────────────────────────────────────────────
    // Reverse is the brake held at rest, like every arcade-and-sim car since
    // Out Run: one pedal less to find on a phone.
    if state.forward.abs() >= STOPPED {
        state.held = 0.0;
    }
    if state.forward.abs() < STOPPED && throttle < 0.05 {
        if brake > 0.5 && state.gear > 0 {
            state.held += h;
            if state.held >= REVERSE_AFTER {
                state.gear = -1;
            }
        } else {
            state.held = 0.0;
        }
    } else if state.gear < 0 && throttle > 0.05 && state.forward > -STOPPED {
        state.gear = 1;
    }
    // In reverse the pedals swap: the brake drives it backwards, the
    // accelerator stops it.
    let (drive_pedal, brake_pedal) = if state.gear < 0 {
        (brake, throttle)
    } else {
        (throttle, brake)
    };
    let ratio = if state.gear < 0 {
        p.reverse_ratio
    } else {
        p.gears[(state.gear.max(1) as usize - 1).min(p.gears.len() - 1)]
    };
    let wheel_rpm = state.forward.abs() / p.wheel_radius * 60.0 / (2.0 * PI);
    let engine_rpm = wheel_rpm * ratio * p.final_drive;
    if state.gear > 0 {
        let top = p.gears.len() as i8;
        if engine_rpm > p.redline_rpm * UPSHIFT_AT && state.gear < top {
            state.gear += 1;
        } else if engine_rpm < DOWNSHIFT_RPM && state.gear > 1 {
            state.gear -= 1;
        }
    }
    // The clutch slips below idle: the engine never stalls and pulls away.
    let idle_target = p.idle_rpm + drive_pedal * (p.peak_rpm * 0.6 - p.idle_rpm);
    state.rpm = engine_rpm.max(idle_target).min(p.redline_rpm);
    let engine_force = if engine_rpm >= p.redline_rpm {
        0.0
    } else {
        drive_pedal * p.torque_at(state.rpm) * ratio * p.final_drive * 0.9 / p.wheel_radius
    };
    let engine_force = if state.gear < 0 {
        -engine_force
    } else {
        engine_force
    };

    // ── Loads: weight moves with the acceleration ────────────────────────────
    let wb = p.wheelbase();
    let weight = p.mass * G;
    let shift = p.mass * state.accel * p.cg_height / wb;
    let load_f = (weight * p.cg_to_rear / wb - shift).max(weight * 0.15);
    let load_r = (weight * p.cg_to_front / wb + shift).max(weight * 0.15);
    // Load sensitivity: a tyre pressed harder grips less per newton. It is
    // what makes a nose-heavy car understeer at the limit — with one friction
    // coefficient for both axles the front and rear let go together, and every
    // bend taken too fast became a four-wheel drift instead of running wide.
    let per_wheel = weight / 2.0;
    let mu_f = p.mu * grip * (1.0 - LOAD_SENSITIVITY * (load_f / per_wheel - 1.0));
    let mu_r = p.mu * grip * (1.0 - LOAD_SENSITIVITY * (load_r / per_wheel - 1.0));

    // ── Longitudinal forces per axle ─────────────────────────────────────────
    // The brakes are biased 65/35 front to rear, as a road car's are, and never
    // ask an axle for more than it has — ABS.
    let vx = state.forward;
    let dir = if vx.abs() > 1e-3 { vx.signum() } else { 0.0 };
    let brake_force = brake_pedal * p.brake_g * weight;
    let mut fx_f = -dir * (brake_force * 0.65).min(mu_f * load_f * 0.95);
    let mut fx_r = -dir * (brake_force * 0.35).min(mu_r * load_r * 0.95);
    match p.drive {
        Drive::Front => fx_f += engine_force,
        Drive::Rear => fx_r += engine_force,
    }
    // The handbrake locks the rear: it skids, so it brakes at sliding friction
    // and has almost nothing left to hold the tail with.
    let rear_locked = input.handbrake && vx.abs() > STOPPED;
    if input.handbrake {
        fx_r = -dir * mu_r * load_r * 0.8;
    }

    // ── Lateral forces per axle: slip angles ─────────────────────────────────
    // Each axle's slip angle is between where its wheel points and where the
    // contact patch is going, taken in the wheel's own frame so it reads the
    // same driving backwards. The 0.5 m/s floor keeps it finite at rest, where
    // the kinematic blend below has the say anyway.
    fn slip(long: f32, lat: f32) -> f32 {
        lat.atan2(long.abs().max(0.5))
    }
    let (ss, cs) = state.steer.sin_cos();
    let front_lat = state.lateral + p.cg_to_front * state.yaw_rate;
    let alpha_f = slip(vx * cs + front_lat * ss, -vx * ss + front_lat * cs);
    let alpha_r = slip(vx, state.lateral - p.cg_to_rear * state.yaw_rate);
    let mut fy_f = tyre_lateral(alpha_f, load_f, mu_f, p.stiffness_front);
    let mut fy_r = if rear_locked {
        tyre_lateral(alpha_r, load_r, mu_r * 0.35, p.stiffness_rear * 0.5)
    } else {
        tyre_lateral(alpha_r, load_r, mu_r, p.stiffness_rear)
    };

    // ── The friction circle: going and turning share one grip ────────────────
    fn circle(fx: &mut f32, fy: &mut f32, limit: f32) -> f32 {
        let total = fx.hypot(*fy);
        if total > limit && total > 1e-3 {
            // Traction is the driver's ask; the tyre gives up the side force
            // first, which is why a spinning wheel steers badly.
            let fx_max = fx.abs().min(limit);
            *fx = fx.signum() * fx_max;
            let fy_max = (limit * limit - fx_max * fx_max).max(0.0).sqrt();
            *fy = fy.clamp(-fy_max, fy_max);
            1.0
        } else {
            0.0
        }
    }
    let over_f = circle(&mut fx_f, &mut fy_f, mu_f * load_f);
    let over_r = circle(&mut fx_r, &mut fy_r, mu_r * load_r);
    let peak_f = peak_slip(p.stiffness_front);
    let peak_r = peak_slip(p.stiffness_rear);
    state.slide_front = over_f.max(((alpha_f.abs() - peak_f) / peak_f).clamp(0.0, 1.0));
    state.slide_rear = over_r
        .max(((alpha_r.abs() - peak_r) / peak_r).clamp(0.0, 1.0))
        .max(if rear_locked { 1.0 } else { 0.0 });
    state.braking = brake_pedal > 0.05 || input.handbrake;

    // ── Resistance ───────────────────────────────────────────────────────────
    let resist = -p.drag * vx * vx.abs() - p.rolling * vx;

    // ── Integrate the dynamic model ──────────────────────────────────────────
    let (sd, cd) = (ss, cs);
    let fx = fx_f * cd - fy_f * sd + fx_r + resist;
    let fy = fy_f * cd + fx_f * sd + fy_r;
    let torque = p.cg_to_front * (fy_f * cd + fx_f * sd) - p.cg_to_rear * fy_r;
    let ax = fx / p.mass + state.lateral * state.yaw_rate;
    let ay = fy / p.mass - vx * state.yaw_rate;
    let mut forward = vx + ax * h;
    let mut lateral = state.lateral + ay * h;
    let mut yaw_rate = state.yaw_rate + torque / p.inertia * h;

    // ── At walking pace, the wheels lead ─────────────────────────────────────
    let kin = 1.0 - ((vx.abs() - KINEMATIC_BELOW) / KINEMATIC_BELOW).clamp(0.0, 1.0);
    if kin > 0.0 {
        let k_yaw = forward * state.steer.tan() / wb;
        let k_lat = k_yaw * p.cg_to_rear;
        yaw_rate = yaw_rate * (1.0 - kin) + k_yaw * kin;
        lateral = lateral * (1.0 - kin) + k_lat * kin;
    }
    // Braking to rest stops rather than oscillating about zero.
    if forward.abs() < STOPPED && throttle < 0.05 && (brake_pedal > 0.05 || input.handbrake) {
        if state.gear > 0 || brake < 0.05 {
            forward = 0.0;
            lateral *= 0.5;
            yaw_rate *= 0.5;
        }
    }
    // A sign change under braking alone is a stop, not a reversal.
    if vx != 0.0
        && forward.signum() != vx.signum()
        && (engine_force == 0.0 || engine_force.signum() != forward.signum())
    {
        forward = 0.0;
    }
    // The weight moves with what the tyres and the air push the body by, not
    // with `forward`'s rate of change: that also holds the turning frame's
    // `lateral · yaw_rate`, which in a slide is tens of m/s² of "braking" that
    // no tyre applied — and moved the weight onto the nose, unloaded the rear,
    // and spun the car out of every slide it entered.
    state.accel += (fx / p.mass - state.accel) * (h * 12.0).min(1.0);
    state.forward = forward;
    state.lateral = lateral;
    state.yaw_rate = yaw_rate;
    state.yaw = crate::yaw::wrap_angle_pi(state.yaw + yaw_rate * h);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(state: &mut CarState, p: &CarParams, input: CarInput, secs: f32) {
        for _ in 0..(secs * 60.0) as usize {
            step(state, p, input, 1.0, 1.0 / 60.0);
        }
    }

    fn floor_it() -> CarInput {
        CarInput {
            throttle: 1.0,
            ..CarInput::default()
        }
    }

    #[test]
    fn a_hatchback_reaches_100_in_a_hatchbacks_time() {
        let p = CarParams::hatchback();
        let mut s = CarState::parked(0.0, &p);
        let mut t = 0.0;
        while s.forward < 100.0 / 3.6 && t < 30.0 {
            step(&mut s, &p, floor_it(), 1.0, 1.0 / 60.0);
            t += 1.0 / 60.0;
        }
        assert!((7.0..14.0).contains(&t), "0–100 km/h in {t:.1} s");
        assert!(s.gear >= 3, "still in gear {} at 100 km/h", s.gear);
    }

    #[test]
    fn top_speed_is_set_by_the_air_and_stays_under_the_sims_cap() {
        for p in [
            CarParams::hatchback(),
            CarParams::saloon(),
            CarParams::estate(),
            CarParams::van(),
        ] {
            let mut s = CarState::parked(0.0, &p);
            run(&mut s, &p, floor_it(), 120.0);
            let kmh = s.forward * 3.6;
            assert!(kmh > 130.0, "tops out at {kmh:.0} km/h");
            assert!(
                s.forward < crate::motion::MAX_DRIVE_SPEED,
                "{kmh:.0} km/h outruns the sim"
            );
        }
    }

    #[test]
    fn braking_from_100_stops_in_a_road_cars_distance() {
        let p = CarParams::hatchback();
        let mut s = CarState::parked(0.0, &p);
        s.forward = 100.0 / 3.6;
        let brake = CarInput {
            brake: 1.0,
            ..CarInput::default()
        };
        let mut dist = 0.0;
        for _ in 0..600 {
            step(&mut s, &p, brake, 1.0, 1.0 / 60.0);
            dist += s.forward / 60.0;
            if s.forward == 0.0 {
                break;
            }
        }
        assert_eq!(s.forward, 0.0);
        assert!((32.0..50.0).contains(&dist), "stopped in {dist:.1} m");
        assert!(s.gear > 0, "a stop under braking is not a reversal");
    }

    #[test]
    fn the_brake_held_at_rest_reverses() {
        let p = CarParams::hatchback();
        let mut s = CarState::parked(0.0, &p);
        let brake = CarInput {
            brake: 1.0,
            ..CarInput::default()
        };
        run(&mut s, &p, brake, 3.0);
        assert_eq!(s.gear, -1);
        assert!(s.forward < -2.0, "reversing at {}", s.forward);
        // The accelerator stops it and then drives it forward again.
        run(&mut s, &p, floor_it(), 4.0);
        assert!(s.gear > 0 && s.forward > 2.0);
    }

    #[test]
    fn it_turns_where_its_wheels_point_at_parking_speed() {
        let p = CarParams::hatchback();
        let mut s = CarState::parked(0.0, &p);
        s.forward = 1.0;
        s.steer = p.max_steer;
        let input = CarInput {
            steer: 1.0,
            throttle: 0.0,
            ..CarInput::default()
        };
        step(&mut s, &p, input, 1.0, 1.0 / 60.0);
        let kinematic = s.forward * s.steer.tan() / p.wheelbase();
        assert!(
            (s.yaw_rate - kinematic).abs() < 0.05,
            "{} vs {}",
            s.yaw_rate,
            kinematic
        );
        assert!(s.yaw_rate > 0.0, "positive steer turns left");
    }

    #[test]
    fn pushed_into_a_bend_too_fast_it_runs_wide() {
        // Full lock at 90 km/h asks for far more than the tyres have: the car
        // must turn less than its wheels point, its nose must be the end that
        // lets go, and it must not spin.
        let p = CarParams::hatchback();
        let mut s = CarState::parked(0.0, &p);
        s.forward = 25.0;
        s.gear = 4;
        let input = CarInput {
            steer: 1.0,
            throttle: 0.5,
            ..CarInput::default()
        };
        for _ in 0..180 {
            let before = s.lateral;
            step(&mut s, &p, input, 1.0, 1.0 / 60.0);
            let lateral_g = ((s.lateral - before) * 60.0 + s.forward * s.yaw_rate) / G;
            assert!(lateral_g < 1.1 * p.mu, "{lateral_g:.2} g of cornering");
            let slip = s.lateral.atan2(s.forward).abs();
            assert!(slip < 0.2, "the car is sideways: {slip:.2} rad");
        }
        let kinematic = s.speed() * s.steer.tan() / p.wheelbase();
        assert!(
            s.yaw_rate < kinematic * 0.8,
            "{} vs {}",
            s.yaw_rate,
            kinematic
        );
        assert!(
            s.slide_front > s.slide_rear,
            "the front should be the end past its grip"
        );
    }

    #[test]
    fn a_gentle_bend_is_taken_without_sliding() {
        let p = CarParams::hatchback();
        let mut s = CarState::parked(0.0, &p);
        s.forward = 14.0;
        s.gear = 3;
        let input = CarInput {
            steer: 0.3,
            throttle: 0.3,
            ..CarInput::default()
        };
        run(&mut s, &p, input, 3.0);
        assert!(s.slide_front == 0.0 && s.slide_rear == 0.0);
        assert!(s.yaw_rate > 0.1);
        assert!(s.lateral.abs() < 1.0);
    }

    #[test]
    fn the_handbrake_swings_the_tail() {
        let p = CarParams::hatchback();
        let mut s = CarState::parked(0.0, &p);
        s.forward = 15.0;
        s.gear = 3;
        let mut plain = s;
        let turn = CarInput {
            steer: 1.0,
            ..CarInput::default()
        };
        let yank = CarInput {
            handbrake: true,
            ..turn
        };
        run(&mut s, &p, yank, 0.8);
        run(&mut plain, &p, turn, 0.8);
        assert!(s.yaw > plain.yaw + 0.2, "{} vs {}", s.yaw, plain.yaw);
        assert!(s.slide_rear > 0.5);
    }

    #[test]
    fn grass_holds_less_than_tarmac() {
        let p = CarParams::hatchback();
        let mut road = CarState::parked(0.0, &p);
        road.forward = 20.0;
        road.gear = 4;
        let mut grass = road;
        let input = CarInput {
            steer: 1.0,
            ..CarInput::default()
        };
        for _ in 0..90 {
            step(&mut road, &p, input, 1.0, 1.0 / 60.0);
            step(&mut grass, &p, input, 0.55, 1.0 / 60.0);
        }
        assert!(road.yaw > grass.yaw);
    }

    #[test]
    fn nothing_turns_into_nan() {
        let p = CarParams::van();
        let mut s = CarState::parked(0.0, &p);
        let inputs = [
            CarInput {
                throttle: 1.0,
                steer: -1.0,
                handbrake: true,
                ..CarInput::default()
            },
            CarInput {
                brake: 1.0,
                steer: 1.0,
                ..CarInput::default()
            },
            CarInput::default(),
        ];
        for i in 0..3000 {
            step(&mut s, &p, inputs[(i / 200) % 3], 0.6, 1.0 / 30.0);
            assert!(s.forward.is_finite() && s.lateral.is_finite() && s.yaw.is_finite());
        }
        // A zero or negative step does nothing.
        let before = s;
        step(&mut s, &p, CarInput::default(), 1.0, 0.0);
        assert_eq!(before, s);
    }

    #[test]
    fn the_world_velocity_round_trips() {
        let mut s = CarState {
            yaw: 0.7,
            forward: 12.0,
            lateral: -1.5,
            ..CarState::default()
        };
        let v = s.world_velocity();
        s.forward = 0.0;
        s.lateral = 0.0;
        s.set_world_velocity(v);
        assert!((s.forward - 12.0).abs() < 1e-4 && (s.lateral + 1.5).abs() < 1e-4);
        // Forward at yaw 0 is +z.
        let at_zero = CarState {
            forward: 1.0,
            ..CarState::default()
        }
        .world_velocity();
        assert!((at_zero[1] - 1.0).abs() < 1e-6 && at_zero[0].abs() < 1e-6);
    }
}

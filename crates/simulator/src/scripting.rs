//! In-world scripting: prim scripts in JavaScript, run in a Bun process this
//! module's thread supervises (`tools/script-host`, `docs/notes/javascript-prim-scripts.md`).
//!
//! # Threading
//!
//! None of this belongs on the tick's critical path. The host lives on one
//! dedicated OS thread and talks to the simulation over channels — [`HostMsg`]
//! in, [`ScriptCommand`] out. The tick loop never blocks on a script and never
//! holds `SimWorld`'s write lock while one runs; commands are applied at the tick
//! barrier. That thread in turn speaks JSON with the script process
//! ([`bun`]), which it kills and restarts when a tick hangs or the process
//! grows past its memory ceiling.
//!
//! # What a script sees
//!
//! Handlers are plain functions defined at the top level of the script:
//!
//! ```js
//! function on_start() { log("hello"); set_timer_event(1.0) }
//! function on_touch(id, name) { say(name + " touched me") }
//! function on_timer() { const [x, y, z] = get_pos(); set_pos(x, y + 0.1, z) }
//! ```
//!
//! Until 2026-11-16 the same API also ran as Luau, one VM per prim through mlua;
//! `docs/notes/prim-scripting.md` keeps what that engine taught.

use cartopolis_core::{GeoAnchor, ParticleEffect, PrimSound, ScriptLevel, ToneSpec};
use glam::Vec3;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::Arc;

/// Longest script source accepted. Not a security boundary (the process's
/// memory ceiling is); it stops a paste accident from becoming a database row
/// nobody wants. `LIMITS.maxSourceBytes` in `tools/script-host/src/limits.ts`.
pub const MAX_SOURCE_BYTES: usize = 64 * 1024;

/// How far one script may grow the heap in one slice before it is stopped
/// (`LIMITS.sliceHeap`): the per-script memory ceiling the simulator enforces,
/// told to clients and shown in the editor. The host's heap audit
/// (`LIMITS.scriptRetained`, what a script holds *between* ticks) is not run by
/// [`bun`]'s supervisor; the process ceiling covers that case.
pub const SCRIPT_MEMORY_BYTES: usize = 16 << 20;

/// Longest string a script may say or log in one call (`LIMITS.maxText`).
const MAX_TEXT: usize = 500;

/// Something for a script to react to.
#[derive(Debug, Clone)]
pub enum ScriptEvent {
    /// Fired once when the script is compiled (on store, and at server startup).
    Start,
    Touch {
        avatar_id: u64,
        avatar_name: String,
    },
    Timer,
    /// Another script sent on a channel this one listens to.
    Message {
        channel: i64,
        text: String,
        sender: i64,
    },
    /// Where somebody's hand is near this prim (`sense_hands`), metres in the
    /// prim's own frame. Coalesced per person: a script that falls behind gets
    /// each person's newest hand, not a backlog of old ones (`queue` in
    /// `tools/script-host/src/host.ts`).
    Hand {
        avatar_id: u64,
        avatar_name: String,
        offset: [f32; 3],
        precise: bool,
    },
}

/// A script's request of the world, applied by the tick loop at the barrier.
///
/// Deliberately a small closed enum rather than direct access to `SimWorld`: it is
/// the authorisation surface (nothing here can touch a prim other than the
/// script's own), it keeps the VM off the write lock, and it is the seam that lets
/// the engine be swapped without the simulation noticing.
#[derive(Debug, Clone)]
pub enum ScriptCommand {
    SetPos {
        prim_id: i64,
        pos: Vec3,
    },
    /// Euler rotation in **radians**, converted at the API boundary from the
    /// degrees an author writes (`set_rot`) — the wire and the client's
    /// `Quat::from_euler` are both radians, and only one of the two conventions
    /// should ever be in flight past this point.
    SetRot {
        prim_id: i64,
        rot: Vec3,
    },
    SetColor {
        prim_id: i64,
        color: [f32; 3],
    },
    SetScale {
        prim_id: i64,
        scale: Vec3,
    },
    SetAlpha {
        prim_id: i64,
        alpha: f32,
    },
    SetGlow {
        prim_id: i64,
        glow: f32,
    },
    SetFullBright {
        prim_id: i64,
        on: bool,
    },
    SetName {
        prim_id: i64,
        name: String,
    },
    /// Move the prim's **geographic anchor** — the durable half of its placement
    /// (ADR-029). `set_pos`'s x/z are re-derived from this by every client, so a
    /// script that wants a prim to *go* somewhere writes this and a script that
    /// wants it higher writes `set_pos`.
    SetGeo {
        prim_id: i64,
        latitude: f64,
        longitude: f64,
    },
    /// Attach, replace or (with `None`) remove the prim's particle emitter.
    /// Already sanitised at the API boundary.
    SetParticles {
        prim_id: i64,
        effect: Option<ParticleEffect>,
    },
    /// Local chat from the prim, heard within `radius` metres.
    Say {
        prim_id: i64,
        text: String,
        radius: f32,
    },
    Output {
        prim_id: i64,
        level: ScriptLevel,
        text: String,
    },
    /// A one-shot sound at the prim (`play_sound`). Not a field write: the tick
    /// loop sends it to every connection as `NetMessage::PrimSound`, and each
    /// client decides from its own distance whether it is in range.
    PlaySound {
        prim_id: i64,
        sound: PrimSound,
    },
    /// Start or replace (`loop_sound`), or with `None` stop (`stop_sound`), the
    /// prim's looping sound. A field write on `PrimDto::sound_loop`.
    SetSoundLoop {
        prim_id: i64,
        sound: Option<PrimSound>,
    },
    /// Ask for the hands within `range` metres (`sense_hands`), 0 for none. A
    /// field write on `PrimDto::hand_range`, which is how clients learn which
    /// prims to report a hand to; already clamped.
    SenseHands {
        prim_id: i64,
        range: f32,
    },
    /// Sound (`set_tone`) or with `None` silence (`stop_tone`) the prim's
    /// tone. Not a field write the tick loop broadcasts as an upsert: it goes
    /// out as `NetMessage::PrimTone`, coalesced to one per prim per tick.
    SetTone {
        prim_id: i64,
        tone: Option<ToneSpec>,
    },
    /// Delete the prim and everything of it, `die`.
    Die {
        prim_id: i64,
    },
    /// Switch the prim's physics on or off (`set_physical`). A field write on
    /// `PrimDto::physical`, refused past `physics::MAX_PHYSICAL_PRIMS`.
    SetPhysical {
        prim_id: i64,
        on: bool,
    },
    /// Set a physical prim's velocity, m/s (`set_vel`). Handed to the physics
    /// step, not written to the prim.
    SetVel {
        prim_id: i64,
        velocity: Vec3,
    },
    /// Shove a physical prim, kg·m/s (`apply_impulse`).
    ApplyImpulse {
        prim_id: i64,
        impulse: Vec3,
    },
}

impl ScriptCommand {
    /// The prim this command acts on. Every variant carries one — a script can
    /// only ever address its own — so this is total rather than an `Option`.
    #[must_use]
    pub fn prim_id(&self) -> i64 {
        match self {
            Self::SetPos { prim_id, .. }
            | Self::SetRot { prim_id, .. }
            | Self::SetColor { prim_id, .. }
            | Self::SetScale { prim_id, .. }
            | Self::SetAlpha { prim_id, .. }
            | Self::SetGlow { prim_id, .. }
            | Self::SetFullBright { prim_id, .. }
            | Self::SetName { prim_id, .. }
            | Self::SetGeo { prim_id, .. }
            | Self::SetParticles { prim_id, .. }
            | Self::Say { prim_id, .. }
            | Self::PlaySound { prim_id, .. }
            | Self::SetSoundLoop { prim_id, .. }
            | Self::SenseHands { prim_id, .. }
            | Self::SetTone { prim_id, .. }
            | Self::Output { prim_id, .. }
            | Self::SetPhysical { prim_id, .. }
            | Self::SetVel { prim_id, .. }
            | Self::ApplyImpulse { prim_id, .. }
            | Self::Die { prim_id } => *prim_id,
        }
    }
}

/// What a script may read about its own prim. Refreshed once per tick from the
/// authoritative world, so a handler sees a consistent snapshot for its whole run
/// rather than a value that shifts under it mid-slice.
///
/// Not `Copy` since it grew a name, which is why it is behind a `RefCell` rather
/// than a `Cell`.
#[derive(Debug, Clone, Default)]
pub struct PrimView {
    pub pos: Vec3,
    /// Radians, as stored on the wire. `get_rot` converts on the way out.
    pub rot: Vec3,
    pub color: [f32; 3],
    pub scale: Vec3,
    pub alpha: f32,
    pub glow: f32,
    pub full_bright: bool,
    pub name: String,
    /// The emitter currently on the prim, so a script can read what it set and so
    /// the sequence below has something to count from.
    pub particles: Option<ParticleEffect>,
    /// The prim's geographic anchor (ADR-029) — where it actually is, as opposed
    /// to [`Self::pos`]'s x/z, which are a client-frame echo.
    pub geo: GeoAnchor,
    /// Avatars near this prim, nearest first: `(id, name, metres)`.
    ///
    /// Pushed with the view rather than queried, because the host thread has no
    /// access to the world (see the module note on threading) — a sensor is a
    /// *read of last tick's neighbours*, which is exactly what `get_pos` is too.
    pub nearby: Vec<(u64, String, f32)>,
    /// Whether the physics step moves this prim (`get_physical`).
    pub physical: bool,
    /// Metres per second, last tick (`get_vel`). Zero unless physical.
    pub velocity: Vec3,
    /// Kilograms, from the prim's size and shape (`get_mass`).
    pub mass: f32,
}

/// Tick-side → host-thread messages.
enum HostMsg {
    Load {
        prim_id: i64,
        source: String,
    },
    Unload {
        prim_id: i64,
    },
    Event {
        prim_id: i64,
        event: ScriptEvent,
    },
    Tick {
        dt: f32,
        views: Vec<(i64, PrimView)>,
    },
    Shutdown,
}

/// The write end of the host, cheap to clone and safe to share.
///
/// Split from [`ScriptHost`] because the two ends have different sharing needs:
/// every connection task wants to load a script or post a touch, while exactly
/// one place — the tick loop — drains the commands. `Receiver` is `Send` but not
/// `Sync`, so keeping it here would make the whole handle unshareable.
#[derive(Clone)]
pub struct ScriptHandle {
    tx: Sender<HostMsg>,
    /// Scripts handed to the host so far, for [`ScriptHost::requested`].
    requested: Arc<AtomicUsize>,
}

impl ScriptHandle {
    fn send(&self, msg: HostMsg) {
        // A closed channel means the host thread is gone. That is a bug, but not
        // one worth killing the simulation over — scripts stop, the world runs on.
        if self.tx.send(msg).is_err() {
            tracing::error!("script host thread is gone; scripts are not running");
        }
    }

    /// Compile (or recompile) a prim's script. Replaces any existing instance,
    /// discarding its state — storing a script is a restart, as it is in-world.
    pub fn load(&self, prim_id: i64, source: String) {
        self.requested.fetch_add(1, Ordering::Relaxed);
        self.send(HostMsg::Load { prim_id, source });
    }

    /// Drop a prim's script and its VM.
    pub fn unload(&self, prim_id: i64) {
        self.send(HostMsg::Unload { prim_id });
    }

    /// Queue an event for a script. Silently ignored if the prim has none.
    pub fn event(&self, prim_id: i64, event: ScriptEvent) {
        self.send(HostMsg::Event { prim_id, event });
    }
}

/// Owner of the script host thread. Held by the tick loop, which is the only
/// place that drains commands. `Drop` shuts the thread down and joins it.
pub struct ScriptHost {
    handle: ScriptHandle,
    rx: Receiver<ScriptCommand>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl ScriptHost {
    /// Start the host thread with this checkout's script host and the `bun` on
    /// the path ([`BunEngine::development`]), as the tests and `cargo run` want.
    pub fn spawn() -> Self {
        Self::spawn_with(BunEngine::development())
    }

    /// Start the host thread on the script process `engine` describes.
    pub fn spawn_with(engine: BunEngine) -> Self {
        let (host_tx, host_rx) = mpsc::channel::<HostMsg>();
        let (cmd_tx, cmd_rx) = mpsc::channel::<ScriptCommand>();
        let thread = std::thread::Builder::new()
            .name("cartopolis-scripts".into())
            .spawn(move || bun::run(host_rx, cmd_tx, engine))
            .expect("spawn script host thread");
        Self {
            handle: ScriptHandle {
                tx: host_tx,
                requested: Arc::new(AtomicUsize::new(0)),
            },
            rx: cmd_rx,
            thread: Some(thread),
        }
    }

    /// A shareable write end for the connection tasks.
    pub fn handle(&self) -> ScriptHandle {
        self.handle.clone()
    }

    /// Number of scripts the host has been *asked* to run. A hint, not a count of
    /// live VMs: compilation happens on the host thread, so a script in this total
    /// may still turn out to be a syntax error. Enough for a startup log line.
    pub fn requested(&self) -> usize {
        self.requested.load(Ordering::Relaxed)
    }

    /// Advance every script by one tick, publishing the current prim state.
    pub fn tick(&self, dt: f32, views: Vec<(i64, PrimView)>) {
        self.handle.send(HostMsg::Tick { dt, views });
    }

    /// Take everything the scripts have asked for since the last call. Never
    /// blocks: whatever has arrived by now is applied this tick, the rest next.
    pub fn drain(&self) -> Vec<ScriptCommand> {
        let mut out = Vec::new();
        loop {
            match self.rx.try_recv() {
                Ok(cmd) => out.push(cmd),
                Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => break,
            }
        }
        out
    }
}

/// The owner is also a handle: `load`, `unload` and `event` mean the same thing
/// whether you hold the host or a clone of its write end, and spelling them out
/// twice would be two places to keep in step.
impl std::ops::Deref for ScriptHost {
    type Target = ScriptHandle;

    fn deref(&self) -> &ScriptHandle {
        &self.handle
    }
}

impl Drop for ScriptHost {
    fn drop(&mut self) {
        // An explicit message, not "drop the last sender": connection tasks hold
        // clones of the handle, and a task that outlives this one by a moment would
        // otherwise keep the channel open and make the join below hang forever.
        self.handle.send(HostMsg::Shutdown);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

mod bun;
mod rules;

pub(crate) use bun::host_command;
pub use bun::{BunEngine, COMPILED_HOST};

#[cfg(test)]
mod bun_tests;
#[cfg(test)]
mod js_tests;

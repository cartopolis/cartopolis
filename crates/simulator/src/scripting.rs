//! In-world scripting: one sandboxed Luau VM per scripted prim, scheduled against
//! a per-region fuel bank.
//!
//! # Why Luau, and what the capacity model rests on
//!
//! Three primitives, and the engine was chosen for having all three:
//!
//! * [`mlua::Lua::sandbox`] — libraries and metatables read-only, `safeenv` on. A
//!   script cannot reach the host through a patched `string.format`.
//! * [`mlua::Lua::set_memory_limit`] — a hard byte ceiling per script, enforced by
//!   the allocator. This is the *memory* half of the capacity model, and it is a
//!   VM invariant rather than accounting code we could get wrong.
//! * [`mlua::Lua::set_interrupt`] returning [`VmState::Yield`] — the *time* half.
//!   Standard Lua's `set_hook` can only raise an error, which would make "kill the
//!   script" the sole answer to an over-budget slice. Being able to **suspend and
//!   resume** is what makes a fuel bank a scheduler instead of a killer: a script
//!   that runs long is descheduled and continues next tick, exactly as a long
//!   loop behaves on a busy region rather than as a user-visible failure.
//!
//! # Threading
//!
//! `Lua` is `!Send`, and it should be: none of this belongs on the tick's critical
//! path. The whole host lives on one dedicated OS thread and talks to the
//! simulation over channels — [`HostMsg`] in, [`ScriptCommand`] out. The tick loop
//! never blocks on a script and never holds `SimWorld`'s write lock while one
//! runs; commands are applied at the tick barrier. That boundary is also what
//! keeps the VM replaceable: nothing outside this module names `mlua`.
//!
//! # What a script sees
//!
//! Handlers are plain functions defined at the top level of the chunk:
//!
//! ```lua
//! function on_start()  vfLog("hello")  vfSetTimerEvent(1.0)  end
//! function on_touch(id, name)  vfSay(name .. " touched me")  end
//! function on_timer()  local x, y, z = vfGetPos()  vfSetPos(x, y + 0.1, z)  end
//! ```
//!
//! The chunk runs in its own environment table whose `__index` is the (read-only)
//! sandboxed globals, so `function on_touch()` defines into the script's own
//! space. Without that, `sandbox(true)` would make the global table read-only and
//! every script would fail on its first `function` statement.

use cartopolis_core::{GeoAnchor, ParticleEffect, ScriptLevel};
use glam::Vec3;
use mlua::thread::ThreadStatus;
use mlua::{Lua, LuaOptions, StdLib, Thread, VmState};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::Arc;

/// Per-script memory ceiling, over and above the interpreter's own baseline (see
/// [`Slot::compile`]). Enforced by the Lua allocator, so a script that asks for
/// more gets a catchable error rather than the host growing without bound.
///
/// Second Life's Mono scripts get 64 KiB and that is the obvious number to copy,
/// but it does not survive contact with this allocator: **Luau hands out memory in
/// ~32 KiB pages**, and loading a chunk takes one or two of them before the script
/// has run a line (measured 2026-08-10: 32,720 B for a one-line handler, 65,440 B
/// for a two-line one). At 64 KiB the *compile* was the whole budget, and scripts
/// failed with "not enough memory" — sometimes, depending on which side of a page
/// boundary the source landed, which is the worst way for a limit to behave.
/// 256 KiB is eight pages: two for the chunk, six for it to work in.
///
/// The cost that is not here: each script is a whole Luau state, ~386 KiB of
/// interpreter before the ceiling applies at all. A region at its default 200
/// scripts is therefore ~75 MiB of baseline — the price of one VM per prim, and
/// the number to revisit if a region ever wants thousands.
pub const SCRIPT_MEMORY_BYTES: usize = 256 * 1024;

/// Interrupt allowance shared by every script in the world, refilled each tick.
///
/// The unit is "interrupt callbacks", which Luau fires on a fixed instruction
/// cadence — deterministic, unlike wall-clock, so a region's capacity is
/// reproducible across hosts and a slow disk cannot make a script look expensive.
pub const REGION_FUEL_PER_TICK: i64 = 200_000;

/// Ceiling on what any single script may draw from the bank in one tick, so one
/// busy script cannot starve the rest before round-robin comes back around.
pub const SCRIPT_FUEL_SLICE: i64 = 20_000;

/// One-off budget for the top level of a chunk at load time. Larger than a tick
/// slice because setup legitimately does more work than an event handler, but
/// still bounded: `while true do end` at the top level must not hang the host.
///
/// The bound is only real because the chunk runs as a **coroutine** — see
/// [`Slot::compile`]. Running it with `Chunk::exec` instead (a `lua_pcall` on the
/// main state) made this constant decorative: the interrupt fired on cadence and
/// asked to yield, mlua discarded the request because a `pcall` body is not a
/// yieldable point in Luau, and the loop ran forever on the host thread.
const LOAD_FUEL: i64 = 200_000;

/// The standard libraries a script gets, spelled out rather than taken from
/// `StdLib::ALL_SAFE`.
///
/// Under the `luau` feature `ALL_SAFE` is `u32::MAX` — it includes `DEBUG`, which
/// [`Lua::new`] therefore opens. Today that is harmless (Luau's `debug` is only
/// `{info, traceback}`, and `sandbox(true)` freezes it), but "the sandbox has no
/// debug library" was the stated model and it was not true, and a future Luau that
/// grows `debug.setupvalue` — as standard Lua has — would hand every script a way
/// into the host closures' upvalues with nothing here to stop it. Listing the set
/// makes an addition upstream arrive switched *off*, which is the same rule
/// `Slot::compile` applies to `require`.
///
/// Everything else Luau ships stays: the note in `docs/notes/prim-scripting.md`
/// says there is no `vfFrand` and no `vfGetUnixTime` *because* `math.random` and
/// `os.time` are there, so trimming the list further would quietly take the
/// documented answer away. A function rather than a `const` because `StdLib`'s
/// `BitOr` is a plain trait impl, not a `const fn`.
fn script_stdlib() -> StdLib {
    StdLib::COROUTINE
        | StdLib::TABLE
        | StdLib::OS
        | StdLib::STRING
        | StdLib::UTF8
        | StdLib::BIT
        | StdLib::BUFFER
        | StdLib::VECTOR
        | StdLib::INTEGER
        | StdLib::MATH
}

/// Longest script source accepted. Not a security boundary (the memory limit is);
/// it stops a paste accident from becoming a database row nobody wants.
pub const MAX_SOURCE_BYTES: usize = 64 * 1024;

/// Events queued per script before the oldest is dropped ([`Slot::queue`] decides
/// *which* oldest). A script that cannot keep up with its own event rate is
/// misbehaving; unbounded queueing would turn that into host memory growth.
const MAX_PENDING_EVENTS: usize = 32;

/// Commands one script may emit per tick. Caps `vfLog` in a loop, which is the
/// most likely way an author floods the wire by accident.
const MAX_COMMANDS_PER_TICK: usize = 64;

/// Shortest repeating timer a script may ask for — one tick at 20 Hz.
const MIN_TIMER_INTERVAL: f32 = 0.05;

/// Messages one script may send per tick (`vfSend`). Separate from
/// [`MAX_COMMANDS_PER_TICK`] because it bounds a different thing: a command costs
/// one wire frame, while a message costs an event queued on *every* listening
/// script in the region, so the same number would mean very different work.
const MAX_MESSAGES_PER_TICK: usize = 8;

/// Channels one script may listen on. A ceiling rather than a policy — a script
/// that wants a thousand channels is describing a design problem, and the set is
/// walked once per message per slot.
const MAX_LISTEN_CHANNELS: usize = 16;

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
    /// Another script in the same region sent on a channel this one listens to.
    Message {
        channel: i64,
        text: String,
        sender: i64,
    },
}

impl ScriptEvent {
    /// The handler name this event dispatches to.
    fn handler(&self) -> &'static str {
        match self {
            Self::Start => "on_start",
            Self::Touch { .. } => "on_touch",
            Self::Timer => "on_timer",
            Self::Message { .. } => "on_message",
        }
    }
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
    /// degrees an author writes (`vfSetRot`) — the wire and the client's
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
    /// (ADR-029). `vfSetPos`'s x/z are re-derived from this by every client, so a
    /// script that wants a prim to *go* somewhere writes this and a script that
    /// wants it higher writes `vfSetPos`.
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
    /// Delete the prim and everything of it, `vfDie`.
    Die {
        prim_id: i64,
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
            | Self::Output { prim_id, .. }
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
    /// Radians, as stored on the wire. `vfGetRot` converts on the way out.
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
    /// Which region the prim is in. Not readable by a script; it is what scopes
    /// `vfSend` (see [`Slot::region_id`]).
    pub region_id: i64,
    /// Avatars near this prim, nearest first: `(id, name, metres)`.
    ///
    /// Pushed with the view rather than queried, because the host thread has no
    /// access to the world (see the module note on threading) — a sensor is a
    /// *read of last tick's neighbours*, which is exactly what `vfGetPos` is too.
    pub nearby: Vec<(u64, String, f32)>,
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
    /// Start the host thread.
    pub fn spawn() -> Self {
        let (host_tx, host_rx) = mpsc::channel::<HostMsg>();
        let (cmd_tx, cmd_rx) = mpsc::channel::<ScriptCommand>();
        let thread = std::thread::Builder::new()
            .name("cartopolis-scripts".into())
            .spawn(move || run(host_rx, cmd_tx))
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

/// Per-slot state shared with the `vf*` host closures. Everything lives on the
/// host thread, so `Rc`/`Cell` rather than `Arc`/`Mutex`.
struct Shared {
    prim_id: i64,
    out: RefCell<Vec<ScriptCommand>>,
    view: RefCell<PrimView>,
    /// Set by `vfSetTimerEvent(seconds)`; `Some(0.0)` cancels.
    timer_request: Cell<Option<f32>>,
    /// Commands dropped this tick because the script exceeded its output budget.
    dropped: Cell<u32>,
    /// `vfSend` calls made this slice, drained by the scheduler and fanned out to
    /// the other scripts in the region.
    ///
    /// **Not a [`ScriptCommand`]**, and that is the whole design: a message never
    /// leaves the host thread. It is script-to-script, the simulation has nothing
    /// to do with it, and routing it out to the tick loop and back would put a
    /// button's press behind the world's write lock for no reason.
    outbox: RefCell<Vec<(i64, String)>>,
    /// `vfListen` calls made this slice — `(channel, on)`, applied by the
    /// scheduler for the same reason `timer_request` is.
    listen_changes: RefCell<Vec<(i64, bool)>>,
    /// Simulated seconds since the script started, advanced by the scheduler and
    /// zeroed by `vfResetTime`. Simulated rather than wall-clock so `vfGetTime`
    /// and `vfSleep` agree with the tick that drives them — and so two hosts
    /// running the same script see the same numbers, as they do for fuel.
    time: Cell<f32>,
}

impl Shared {
    /// The next value for [`ParticleEffect::sequence`] — one past whatever the
    /// prim is carrying.
    ///
    /// Derived from the prim rather than counted in a private field, so it keeps
    /// rising across a script restart: a stored effect outlives the VM that set
    /// it, and a counter that went back to 1 on every recompile would hand a
    /// client a sequence it had already played, i.e. a blast that does not go off.
    fn next_sequence(&self) -> u32 {
        self.view
            .borrow()
            .particles
            .map_or(1, |p| p.sequence.wrapping_add(1))
    }

    fn push(&self, cmd: ScriptCommand) {
        let mut out = self.out.borrow_mut();
        if out.len() >= MAX_COMMANDS_PER_TICK {
            self.dropped.set(self.dropped.get() + 1);
            return;
        }
        out.push(cmd);
    }
}

/// One scripted prim: its VM, its compiled handlers, and its scheduling state.
struct Slot {
    lua: Lua,
    shared: Rc<Shared>,
    /// Remaining interrupt allowance for the current slice. The interrupt closure
    /// decrements this and yields at zero.
    ///
    /// `Arc<AtomicI64>` rather than `Rc<Cell>` only because `set_interrupt`'s bound
    /// is `MaybeSend`, which is a real `Send` when mlua's `send` feature is on
    /// somewhere in the graph. Uncontended, so the atomics are free in practice.
    fuel: Arc<AtomicI64>,
    /// The chunk's environment table, where handlers were defined.
    env: mlua::Table,
    pending: VecDeque<ScriptEvent>,
    /// The suspended handler call, if one is mid-flight across ticks.
    running: Option<Thread>,
    /// Arguments for [`Slot::running`]'s *first* resume. A resumed coroutine takes
    /// the values `coroutine.yield` returns, not the handler's parameters again, so
    /// this is taken once and the thread continues bare afterwards.
    first_args: Option<ScriptEvent>,
    /// `(interval, remaining)` for a repeating `on_timer`.
    timer: Option<(f32, f32)>,
    /// Channels this script has asked to hear (`vfListen`).
    ///
    /// Opt-in, as Second Life's `llListen` is: without it every `vfSend` would be
    /// queued on every script in the region, so one chatty prim would cost every
    /// other one an event per tick whether or not it cared.
    listening: std::collections::HashSet<i64>,
    /// The region this prim is in, refreshed with the view each tick.
    ///
    /// Held on the slot rather than read through `shared` at fan-out time because
    /// the fan-out happens with several slots borrowed at once; it is also the
    /// only thing that scopes a channel, and a channel that silently crossed
    /// regions would be a surprising thing to debug.
    region_id: i64,
    /// A faulted script keeps its source (so the author can see and fix it) but
    /// runs nothing until it is stored again.
    faulted: bool,
}

impl Slot {
    /// Compile `source` into a fresh sandboxed VM.
    fn compile(prim_id: i64, source: &str) -> Result<Self, String> {
        let lua = Lua::new_with(script_stdlib(), LuaOptions::default())
            .map_err(|e| format!("could not create the interpreter: {e}"))?;

        // Two globals mlua adds that `sandbox(true)` does *not* take away, removed
        // here — before the sandbox freezes the global table and puts them out of
        // reach of any later edit.
        //
        // `require` is the serious one: mlua wires Luau's require-by-string to a
        // `FsRequirer`, i.e. **the host filesystem**, so it is a read primitive on
        // the server's disk handed to every script by default. "Luau is sandboxed"
        // is true of the language and not of this binding, which is the kind of gap
        // a test has to hold shut (`sandbox_denies_escape_routes`).
        //
        // `loadstring` is a judgement call rather than a hole — compiled code stays
        // inside the same sandbox, fuel and memory ceiling. It goes because the
        // editor's source is meant to *be* what runs: a script that assembles code
        // at runtime is one nobody can read, review, or hand back an error line for.
        for global in ["require", "loadstring"] {
            lua.globals()
                .set(global, mlua::Value::Nil)
                .map_err(|e| format!("could not remove `{global}`: {e}"))?;
        }

        // Read-only libraries and metatables, and `safeenv`. Must come before the
        // environment table is built, or the metatable we attach is frozen too.
        lua.sandbox(true)
            .map_err(|e| format!("could not sandbox: {e}"))?;

        let fuel = Arc::new(AtomicI64::new(LOAD_FUEL));
        let interrupt_fuel = fuel.clone();
        lua.set_interrupt(move |_| {
            // `fetch_sub` then test: the callback fires on Luau's own instruction
            // cadence, so this is the whole cost of metering.
            if interrupt_fuel.fetch_sub(1, Ordering::Relaxed) <= 0 {
                Ok(VmState::Yield)
            } else {
                Ok(VmState::Continue)
            }
        });

        let shared = Rc::new(Shared {
            prim_id,
            out: RefCell::new(Vec::new()),
            view: RefCell::new(PrimView::default()),
            timer_request: Cell::new(None),
            dropped: Cell::new(0),
            outbox: RefCell::new(Vec::new()),
            listen_changes: RefCell::new(Vec::new()),
            time: Cell::new(0.0),
        });

        // The chunk's own global space: writes land here, reads fall through to
        // the frozen stdlib. `sandbox(true)` makes the real globals read-only, so
        // without this every `function on_touch()` would fail to assign.
        let env = lua
            .create_table()
            .map_err(|e| format!("could not create environment: {e}"))?;
        let meta = lua
            .create_table()
            .map_err(|e| format!("could not create environment: {e}"))?;
        meta.set("__index", lua.globals())
            .map_err(|e| format!("could not create environment: {e}"))?;
        env.set_metatable(Some(meta))
            .map_err(|e| format!("could not create environment: {e}"))?;
        install_api(&lua, &env, &shared).map_err(|e| format!("could not install vf API: {e}"))?;
        // The prelude is host code, so it runs before the ceiling below applies to
        // the author's chunk.
        lua.load(PRELUDE)
            .set_name("=vfprelude")
            .set_environment(env.clone())
            .exec()
            .map_err(|e| format!("could not install the prelude: {e}"))?;

        // The ceiling is measured from the *baseline*, and set last, so it means
        // "what this script's own code and data may cost". A fresh Luau state with
        // its stdlib loaded is already a couple of hundred KiB before a line of
        // script exists; charging that to the author's 64 KiB made every script in
        // the world fail to compile with "not enough memory", including the empty
        // one. The interpreter's fixed cost is the host's, not the script's.
        let baseline = lua.used_memory();
        lua.set_memory_limit(baseline + SCRIPT_MEMORY_BYTES)
            .map_err(|e| format!("could not set memory limit: {e}"))?;

        // The author's chunk runs as a **coroutine**, exactly as a handler does,
        // and for the same reason: it is the only shape in which the fuel bank can
        // stop it.
        //
        // `Chunk::exec` is a `lua_pcall` on the main state, and a `pcall` body is
        // not a yieldable point in Luau (`lua_isyieldable` is `nCcalls <=
        // baseCcalls`, and only `lua_resume` raises the base). mlua's interrupt
        // proc checks exactly that and *silently ignores* a `VmState::Yield` it
        // cannot honour — so `LOAD_FUEL` was counted, exhausted, and then had
        // nothing to do about it. A one-line `while true do end` at the top level
        // therefore never returned: the host thread stopped draining `HostMsg`,
        // every other prim's script stopped compiling and running, and the tick
        // loop kept pushing an unbounded `Tick { views }` at it 20 times a second
        // until the process ran out of memory. Storing the source succeeds before
        // any of this, so a restart replayed it and hung again.
        //
        // Resuming once is deliberate rather than a scheduling decision: unlike a
        // handler, an unfinished chunk has not defined its handlers yet, so there
        // is nothing coherent to keep in `slots` while it catches up. Overrunning
        // the load budget is an author error with a message, not a slow prim.
        let chunk = lua
            .load(source)
            .set_name(format!("=prim{prim_id}"))
            .set_environment(env.clone())
            .into_function()
            .map_err(|e| clean_error(&e))?;
        let top_level = lua
            .create_thread(chunk)
            .map_err(|e| format!("could not start the script: {e}"))?;
        fuel.store(LOAD_FUEL, Ordering::Relaxed);
        top_level
            .resume::<mlua::MultiValue>(())
            .map_err(|e| clean_error(&e))?;
        if top_level.status() == ThreadStatus::Resumable {
            return Err(format!(
                "the top level of the script did not finish within its load budget \
                 ({LOAD_FUEL} instructions). Long-running work belongs in a handler, \
                 where it is descheduled and resumed instead of refused."
            ));
        }

        Ok(Self {
            lua,
            shared,
            fuel,
            env,
            pending: VecDeque::new(),
            running: None,
            first_args: None,
            timer: None,
            listening: std::collections::HashSet::new(),
            region_id: 0,
            faulted: false,
        })
    }

    /// The handler for an event, if the script defined one.
    ///
    /// **Raw**, and that is load-bearing. `Table::get` respects `__index`, and the
    /// environment's metatable is the one thing in here a script can replace —
    /// `setmetatable(getfenv(), {__index = function() while true do end end})` is
    /// a legal top-level statement, and every lookup of `on_start` afterwards
    /// would run that metamethod on the host thread from Rust, i.e. at a
    /// non-yieldable point where the fuel bank cannot suspend it (see
    /// [`Slot::compile`]). A handler is a plain field of the chunk's own
    /// environment; there is nothing for `__index` to legitimately answer here.
    fn handler(&self, name: &str) -> Option<mlua::Function> {
        self.env.raw_get::<mlua::Function>(name).ok()
    }

    /// Queue an event, keeping the queue bounded without losing the one event a
    /// script cannot do without.
    ///
    /// Two rules, both learned from what the plain ring buffer did. **Timers
    /// coalesce**: `on_timer` means "the interval elapsed", so a second pending
    /// one only asks for the same handler twice in a row later — a script whose
    /// handler is slower than its own `vfSetTimerEvent` would otherwise fill the
    /// queue in a couple of seconds and stay full forever. And **`Start` is never
    /// evicted**: it is queued once, and a script that loses it runs with none of
    /// its own setup — no timer, no state — which looks like a script that ran but
    /// did nothing rather than a script that was dropped.
    fn queue(&mut self, event: ScriptEvent) {
        if matches!(event, ScriptEvent::Timer)
            && self.pending.iter().any(|e| matches!(e, ScriptEvent::Timer))
        {
            return;
        }
        if self.pending.len() >= MAX_PENDING_EVENTS {
            match self
                .pending
                .iter()
                .position(|e| !matches!(e, ScriptEvent::Start))
            {
                Some(i) => {
                    self.pending.remove(i);
                }
                // Only `Start`s left, which cannot happen (one is ever queued) —
                // drop the arrival rather than the initialisation.
                None => return,
            }
            tracing::debug!(
                prim_id = self.shared.prim_id,
                "script event queue full; an event was dropped"
            );
        }
        self.pending.push_back(event);
    }

    /// Run for at most `budget` interrupts. Returns what was actually consumed.
    ///
    /// Resumes a suspended handler if there is one, otherwise starts the next
    /// queued event. Either way it stops at the first yield — a handler that runs
    /// past its slice is descheduled, not killed, and picks up next tick.
    fn run_slice(&mut self, budget: i64) -> i64 {
        if self.faulted || budget <= 0 {
            return 0;
        }
        if self.running.is_none() {
            // Skip events with no handler rather than spending a slice on them.
            loop {
                let Some(event) = self.pending.pop_front() else {
                    return 0;
                };
                let Some(func) = self.handler(event.handler()) else {
                    continue;
                };
                match self.lua.create_thread(func) {
                    Ok(t) => {
                        self.running = Some(t);
                        self.first_args = Some(event);
                        break;
                    }
                    Err(e) => {
                        self.fault(&format!("could not start handler: {e}"));
                        return 0;
                    }
                }
            }
        }

        self.fuel.store(budget, Ordering::Relaxed);
        let Some(thread) = self.running.clone() else {
            return 0;
        };
        let result: mlua::Result<mlua::MultiValue> = match self.first_args.take() {
            Some(ScriptEvent::Touch {
                avatar_id,
                avatar_name,
            }) => thread.resume((avatar_id, avatar_name)),
            Some(ScriptEvent::Message {
                channel,
                text,
                sender,
            }) => thread.resume((channel, text, sender)),
            Some(_) | None => thread.resume(()),
        };
        let spent = (budget - self.fuel.load(Ordering::Relaxed)).max(0);

        match result {
            Ok(_) => match thread.status() {
                // Yielded — out of fuel, or an explicit `coroutine.yield`. Either
                // way it continues next tick with a fresh slice.
                ThreadStatus::Resumable => {}
                _ => self.running = None,
            },
            Err(e) => {
                self.running = None;
                self.fault(&clean_error(&e));
            }
        }
        spent
    }

    /// Stop the script and tell the author why. Faulting rather than unloading
    /// keeps the source in place so the editor can show it next to the error.
    fn fault(&mut self, message: &str) {
        self.faulted = true;
        self.pending.clear();
        self.timer = None;
        // A stopped script must also stop *receiving*: it cannot answer, and the
        // events would accumulate against its queue until it was stored again.
        self.listening.clear();
        self.shared.push(ScriptCommand::Output {
            prim_id: self.shared.prim_id,
            level: ScriptLevel::Error,
            text: message.to_string(),
        });
    }
}

/// Luau prefixes runtime errors with the chunk name and line; keep that (it is
/// what makes an error actionable) but drop the trailing Rust-side traceback mlua
/// appends, which names host frames an author cannot act on.
fn clean_error(e: &mlua::Error) -> String {
    let s = e.to_string();
    match s.split_once("\nstack traceback:") {
        Some((head, _)) => head.trim_end().to_string(),
        None => s,
    }
}
/// Lua-side half of the API, loaded into every script's environment before the
/// author's chunk.
///
/// `vfSleep` lives here rather than in Rust because a host function cannot yield
/// the coroutine it is called from — but Lua code can, and the scheduler already
/// resumes a yielded handler on the next tick. So sleeping is "yield until the
/// clock says otherwise", which needs no new machinery in the scheduler at all:
/// a sleeping script costs one resume and a compare per tick, and is descheduled
/// by exactly the mechanism that deschedules a script which ran too long.
const PRELUDE: &str = r#"
function vfSleep(seconds)
    local wake = vfGetTime() + seconds
    while vfGetTime() < wake do
        coroutine.yield()
    end
end
"#;

/// Longest string a script may say or log in one call.
const MAX_TEXT: usize = 500;

/// Chat radii, matching `crate::chat`'s for say and shout. Whisper is Second
/// Life's 10 m and has no avatar-side equivalent here yet — a prim is the only
/// thing that whispers.
const WHISPER_RADIUS_M: f32 = 10.0;

/// Install the `vf*` functions into a script's environment.
///
/// This is the entire authority a script has. Every entry acts on the script's
/// own prim — `prim_id` is captured, never passed in — so there is no argument an
/// author can supply that reaches another object.
///
/// The names are flat and `vf`-prefixed rather than methods on a `vf` table, which
/// is Second Life's shape (`llSetPos`, `llSay`) and therefore the one the people
/// most likely to write these already know. `cartopolis_core::SCRIPT_API` is the
/// catalogue the editor shows, and a test asserts the two agree exactly — add a
/// function here without listing it there and the suite fails.
fn install_api(lua: &Lua, env: &mlua::Table, shared: &Rc<Shared>) -> mlua::Result<()> {
    // ── Movement ──

    let s = shared.clone();
    env.set(
        "vfGetPos",
        lua.create_function(move |_, ()| {
            let v = s.view.borrow().pos;
            Ok((v.x, v.y, v.z))
        })?,
    )?;

    let s = shared.clone();
    env.set(
        "vfSetPos",
        lua.create_function(move |_, (x, y, z): (f32, f32, f32)| {
            let pos = finite_vec3(x, y, z)?;
            s.push(ScriptCommand::SetPos {
                prim_id: s.prim_id,
                pos,
            });
            // Reflect the write locally so a handler that sets and then reads
            // within one slice sees its own value, instead of the stale tick
            // snapshot. The authoritative value still arrives next tick.
            s.view.borrow_mut().pos = pos;
            Ok(())
        })?,
    )?;

    let s = shared.clone();
    env.set(
        "vfGetGeo",
        lua.create_function(move |_, ()| {
            let g = s.view.borrow().geo;
            Ok((g.latitude, g.longitude))
        })?,
    )?;

    let s = shared.clone();
    env.set(
        "vfSetGeo",
        lua.create_function(move |_, (lat, lng): (f64, f64)| {
            // The globe's own bounds, not a taste: a latitude past a pole names no
            // point, and everything downstream (the projection, the distance, the
            // client's placement) would answer with a number anyway.
            if !lat.is_finite() || !lng.is_finite() || lat.abs() > 90.0 || lng.abs() > 180.0 {
                return Err(mlua::Error::RuntimeError(
                    "expected a latitude in -90..90 and a longitude in -180..180".to_string(),
                ));
            }
            s.push(ScriptCommand::SetGeo {
                prim_id: s.prim_id,
                latitude: lat,
                longitude: lng,
            });
            s.view.borrow_mut().geo = GeoAnchor {
                latitude: lat,
                longitude: lng,
            };
            Ok(())
        })?,
    )?;

    env.set(
        "vfGeoScale",
        lua.create_function(move |_, lat: f64| {
            if !lat.is_finite() {
                return Err(mlua::Error::RuntimeError(
                    "expected a finite latitude".to_string(),
                ));
            }
            Ok(cartopolis_core::degree_scale_m(lat.clamp(-90.0, 90.0)))
        })?,
    )?;

    let s = shared.clone();
    env.set(
        "vfGetRot",
        lua.create_function(move |_, ()| {
            let v = s.view.borrow().rot;
            Ok((v.x.to_degrees(), v.y.to_degrees(), v.z.to_degrees()))
        })?,
    )?;

    let s = shared.clone();
    env.set(
        "vfSetRot",
        lua.create_function(move |_, (rx, ry, rz): (f32, f32, f32)| {
            // Degrees in, radians out: an author reads the numbers off the edit
            // dialog, which shows degrees, and the wire is radians.
            let deg = finite_vec3(rx, ry, rz)?;
            let rot = Vec3::new(deg.x.to_radians(), deg.y.to_radians(), deg.z.to_radians());
            s.push(ScriptCommand::SetRot {
                prim_id: s.prim_id,
                rot,
            });
            s.view.borrow_mut().rot = rot;
            Ok(())
        })?,
    )?;

    let s = shared.clone();
    env.set(
        "vfGetScale",
        lua.create_function(move |_, ()| {
            let v = s.view.borrow().scale;
            Ok((v.x, v.y, v.z))
        })?,
    )?;

    let s = shared.clone();
    env.set(
        "vfSetScale",
        lua.create_function(move |_, (x, y, z): (f32, f32, f32)| {
            let scale = finite_vec3(x, y, z)?;
            // A zero or negative scale is a degenerate mesh, and a huge one is a
            // grief vector; clamp rather than refuse so a script keeps running.
            let scale = scale.clamp(Vec3::splat(0.01), Vec3::splat(64.0));
            s.push(ScriptCommand::SetScale {
                prim_id: s.prim_id,
                scale,
            });
            s.view.borrow_mut().scale = scale;
            Ok(())
        })?,
    )?;

    // ── Appearance ──

    let s = shared.clone();
    env.set(
        "vfGetColor",
        lua.create_function(move |_, ()| {
            let c = s.view.borrow().color;
            Ok((c[0], c[1], c[2]))
        })?,
    )?;

    let s = shared.clone();
    env.set(
        "vfSetColor",
        lua.create_function(move |_, (r, g, b): (f32, f32, f32)| {
            // Check *then* clamp. `f32::clamp` returns NaN for NaN — it is a
            // range limit, not a validator — so `vfSetColor(0/0, 0, 0)` used to
            // put a NaN straight into the prim's colour, from there into SQLite
            // and into every client's material.
            let color = [
                finite(r)?.clamp(0.0, 1.0),
                finite(g)?.clamp(0.0, 1.0),
                finite(b)?.clamp(0.0, 1.0),
            ];
            s.push(ScriptCommand::SetColor {
                prim_id: s.prim_id,
                color,
            });
            s.view.borrow_mut().color = color;
            Ok(())
        })?,
    )?;

    let s = shared.clone();
    env.set(
        "vfGetAlpha",
        lua.create_function(move |_, ()| Ok(s.view.borrow().alpha))?,
    )?;

    let s = shared.clone();
    env.set(
        "vfSetAlpha",
        lua.create_function(move |_, a: f32| {
            let alpha = finite(a)?.clamp(0.0, 1.0);
            s.push(ScriptCommand::SetAlpha {
                prim_id: s.prim_id,
                alpha,
            });
            s.view.borrow_mut().alpha = alpha;
            Ok(())
        })?,
    )?;

    let s = shared.clone();
    env.set(
        "vfGetGlow",
        lua.create_function(move |_, ()| Ok(s.view.borrow().glow))?,
    )?;

    let s = shared.clone();
    env.set(
        "vfSetGlow",
        lua.create_function(move |_, g: f32| {
            let glow = finite(g)?.clamp(0.0, 1.0);
            s.push(ScriptCommand::SetGlow {
                prim_id: s.prim_id,
                glow,
            });
            s.view.borrow_mut().glow = glow;
            Ok(())
        })?,
    )?;

    let s = shared.clone();
    env.set(
        "vfSetFullBright",
        lua.create_function(move |_, on: bool| {
            s.push(ScriptCommand::SetFullBright {
                prim_id: s.prim_id,
                on,
            });
            s.view.borrow_mut().full_bright = on;
            Ok(())
        })?,
    )?;

    // Two entry points, one implementation. The only difference between a fire and
    // an explosion is `burst` and which preset the unspecified fields come from —
    // spelling them out twice would be two tables of defaults to keep in step.
    for (name, burst) in [("vfParticles", false), ("vfParticleBurst", true)] {
        let s = shared.clone();
        env.set(
            name,
            lua.create_function(move |_, rules: Option<mlua::Table>| {
                let effect = match rules {
                    // `vfParticles(nil)` puts it out. There is no `vfParticlesOff`
                    // because "no rules" already means "no emitter", and a second
                    // name for it is a second thing to document.
                    None => None,
                    Some(t) => Some(effect_from_table(&t, burst, s.next_sequence())?),
                };
                s.push(ScriptCommand::SetParticles {
                    prim_id: s.prim_id,
                    effect,
                });
                s.view.borrow_mut().particles = effect;
                Ok(())
            })?,
        )?;
    }

    // ── Chat ──

    let s = shared.clone();
    env.set(
        "vfLog",
        lua.create_function(move |_, text: String| {
            s.push(ScriptCommand::Output {
                prim_id: s.prim_id,
                level: ScriptLevel::Info,
                text: truncate(&text, MAX_TEXT),
            });
            Ok(())
        })?,
    )?;

    for (name, radius) in [
        ("vfWhisper", WHISPER_RADIUS_M),
        ("vfSay", crate::chat::SAY_RADIUS_M),
        ("vfShout", crate::chat::SHOUT_RADIUS_M),
    ] {
        let s = shared.clone();
        env.set(
            name,
            lua.create_function(move |_, text: String| {
                s.push(ScriptCommand::Say {
                    prim_id: s.prim_id,
                    text: truncate(&text, MAX_TEXT),
                    radius,
                });
                Ok(())
            })?,
        )?;
    }

    // ── Timing ──

    let s = shared.clone();
    env.set(
        "vfSetTimerEvent",
        lua.create_function(move |_, seconds: f32| {
            s.timer_request.set(Some(seconds.max(0.0)));
            Ok(())
        })?,
    )?;

    let s = shared.clone();
    env.set(
        "vfGetTime",
        lua.create_function(move |_, ()| Ok(s.time.get()))?,
    )?;

    let s = shared.clone();
    env.set(
        "vfResetTime",
        lua.create_function(move |_, ()| {
            s.time.set(0.0);
            Ok(())
        })?,
    )?;

    // ── Sensing ──

    let s = shared.clone();
    env.set(
        "vfSensor",
        lua.create_function(move |_, range: f32| {
            let range = finite(range)?.max(0.0);
            // `nearby` is already nearest-first, so this is the head of the list
            // and not a scan. Returning nothing at all — rather than a sentinel
            // distance — is what lets an author write `if id then`.
            let view = s.view.borrow();
            match view.nearby.iter().find(|(_, _, d)| *d <= range) {
                Some((id, name, d)) => Ok((Some(*id), Some(name.clone()), Some(*d))),
                None => Ok((None, None, None)),
            }
        })?,
    )?;

    // ── Messaging ──

    let s = shared.clone();
    env.set(
        "vfSend",
        lua.create_function(move |_, (channel, text): (i64, String)| {
            let mut out = s.outbox.borrow_mut();
            if out.len() >= MAX_MESSAGES_PER_TICK {
                // Counted with the dropped commands rather than raised as an
                // error: a script that overruns its own send budget is busy, not
                // broken, and the notice at the end of the tick says so once
                // instead of faulting it mid-handler.
                s.dropped.set(s.dropped.get() + 1);
                return Ok(());
            }
            out.push((channel, truncate(&text, MAX_TEXT)));
            Ok(())
        })?,
    )?;

    let s = shared.clone();
    env.set(
        "vfListen",
        lua.create_function(move |_, (channel, on): (i64, Option<bool>)| {
            s.listen_changes
                .borrow_mut()
                .push((channel, on.unwrap_or(true)));
            Ok(())
        })?,
    )?;

    // ── Prim / lifecycle ──

    let s = shared.clone();
    env.set(
        "vfGetPrimId",
        lua.create_function(move |_, ()| Ok(s.prim_id))?,
    )?;

    let s = shared.clone();
    env.set(
        "vfGetName",
        lua.create_function(move |_, ()| Ok(s.view.borrow().name.clone()))?,
    )?;

    let s = shared.clone();
    env.set(
        "vfSetName",
        lua.create_function(move |_, name: String| {
            // The same cap the edit dialog's field has; a name is a label, and an
            // unbounded one is a way to put a kilobyte in every snapshot.
            let name = truncate(&name, 64);
            s.push(ScriptCommand::SetName {
                prim_id: s.prim_id,
                name: name.clone(),
            });
            s.view.borrow_mut().name = name;
            Ok(())
        })?,
    )?;

    let s = shared.clone();
    env.set(
        "vfDie",
        lua.create_function(move |_, ()| {
            s.push(ScriptCommand::Die { prim_id: s.prim_id });
            Ok(())
        })?,
    )?;

    Ok(())
}

/// Read a `vfParticles` rules table into an effect.
///
/// Unspecified fields keep the preset's value, which is what makes
/// `vfParticles{ color_start = {0,0.4,1} }` a blue flame rather than an error
/// message about the eleven fields it did not mention. An **unknown** key is an
/// error, though, and deliberately so: `vfParticles{ lifetime = 3 }` silently
/// emitting the default 1.4 s is the kind of bug an author cannot see, and this
/// is the same rule the shot manifest applies to its own keys.
///
/// The names are **exactly** `authoring::ParticleSpec`'s, and
/// `the_two_authoring_surfaces_use_one_vocabulary` is the test that keeps them
/// so. They were shorter here once — `spread`, `radius`, `size`, `color` — which
/// meant the same emitter was spelled two ways depending on whether a script or
/// an HTTP body set it, and a person moving between them had to translate. The
/// longer names won because they are symmetric (`size_start` beside `size_end`,
/// where `size` beside `end_size` is not) and because `spread_deg` carries its
/// unit, which this codebase has reason to care about.
fn effect_from_table(t: &mlua::Table, burst: bool, sequence: u32) -> mlua::Result<ParticleEffect> {
    const KEYS: &[&str] = &[
        "count",
        "life",
        "speed",
        "spread_deg",
        "gravity",
        "source_radius",
        "size_start",
        "size_end",
        "color_start",
        "color_end",
        "alpha",
        "turbulence",
    ];
    for pair in t.clone().pairs::<mlua::Value, mlua::Value>() {
        let (k, _) = pair?;
        let name = k
            .as_string()
            .and_then(|s| s.to_str().ok().map(|v| v.to_string()))
            .unwrap_or_default();
        if !KEYS.contains(&name.as_str()) {
            // An old spelling gets its replacement by name rather than the whole
            // list. These six were the script API's own names until the two
            // authoring surfaces were reconciled, so a script written against
            // them is somebody's real work and deserves better than "not one of:
            // twelve things".
            let hint = match name.as_str() {
                "spread" => Some("spread_deg"),
                "radius" => Some("source_radius"),
                "size" => Some("size_start"),
                "end_size" => Some("size_end"),
                "color" => Some("color_start"),
                "end_color" => Some("color_end"),
                _ => None,
            };
            return Err(mlua::Error::RuntimeError(match hint {
                Some(now) => format!("particle rule `{name}` is called `{now}` now"),
                None => format!("particle rule `{name}` is not one of: {}", KEYS.join(", ")),
            }));
        }
    }

    let base = if burst {
        ParticleEffect::blast()
    } else {
        ParticleEffect::fire()
    };
    let num = |key: &str, fallback: f32| -> mlua::Result<f32> {
        match t.get::<Option<f32>>(key)? {
            Some(v) => finite(v),
            None => Ok(fallback),
        }
    };
    let rgb = |key: &str, fallback: [f32; 3]| -> mlua::Result<[f32; 3]> {
        let Some(c) = t.get::<Option<mlua::Table>>(key)? else {
            return Ok(fallback);
        };
        // Positional, `{1.0, 0.5, 0.1}`, because that is how a colour is written
        // everywhere else in this API (`vfSetColor(r, g, b)`).
        Ok([
            finite(c.get::<Option<f32>>(1)?.unwrap_or(fallback[0]))?,
            finite(c.get::<Option<f32>>(2)?.unwrap_or(fallback[1]))?,
            finite(c.get::<Option<f32>>(3)?.unwrap_or(fallback[2]))?,
        ])
    };

    Ok(ParticleEffect {
        count: num("count", f32::from(base.count))?.clamp(0.0, 65_535.0) as u16,
        life: num("life", base.life)?,
        speed: num("speed", base.speed)?,
        spread_deg: num("spread_deg", base.spread_deg)?,
        gravity: num("gravity", base.gravity)?,
        source_radius: num("source_radius", base.source_radius)?,
        size_start: num("size_start", base.size_start)?,
        size_end: num("size_end", base.size_end)?,
        color_start: rgb("color_start", base.color_start)?,
        color_end: rgb("color_end", base.color_end)?,
        alpha: num("alpha", base.alpha)?,
        turbulence: num("turbulence", base.turbulence)?,
        burst,
        sequence,
    }
    // Clamped here, at the boundary, exactly as `vfSetScale` clamps rather than
    // refuses: a script that asks for a thousand particles gets the ceiling and
    // keeps running.
    .sanitized())
}

/// Reject NaN/infinity at the boundary. A script can produce one with `0/0`, and
/// letting it reach a `Transform` puts a prim at an undefined position that no
/// later write can recover.
fn finite_vec3(x: f32, y: f32, z: f32) -> mlua::Result<Vec3> {
    Ok(Vec3::new(finite(x)?, finite(y)?, finite(z)?))
}

/// The scalar half of [`finite_vec3`], for the setters that take one number.
///
/// Every `f32` a script hands the host goes through one of these two. The rule is
/// the boundary's, not the field's: a clamp downstream cannot stand in for it,
/// because `f32::clamp` propagates NaN rather than rejecting it, and a NaN that
/// gets past here is persisted and replicated to every observer.
fn finite(v: f32) -> mlua::Result<f32> {
    if !v.is_finite() {
        return Err(mlua::Error::RuntimeError(
            "expected finite numbers".to_string(),
        ));
    }
    Ok(v)
}

fn truncate(s: &str, max_chars: usize) -> String {
    s.chars().take(max_chars).collect()
}

/// The host thread's main loop.
fn run(rx: Receiver<HostMsg>, tx: Sender<ScriptCommand>) {
    let mut slots: HashMap<i64, Slot> = HashMap::new();
    // Round-robin cursor, so the same script is not always first at the bank.
    let mut cursor: usize = 0;

    while let Ok(msg) = rx.recv() {
        match msg {
            HostMsg::Load { prim_id, source } => {
                slots.remove(&prim_id);
                match Slot::compile(prim_id, &source) {
                    Ok(mut slot) => {
                        let handlers: Vec<&str> = ["on_start", "on_touch", "on_timer"]
                            .into_iter()
                            .filter(|n| slot.handler(n).is_some())
                            .collect();
                        let text = if handlers.is_empty() {
                            "compiled, but defines no handler (on_start / on_touch / on_timer)"
                                .to_string()
                        } else {
                            format!("compiled — {}", handlers.join(", "))
                        };
                        let _ = tx.send(ScriptCommand::Output {
                            prim_id,
                            level: ScriptLevel::Notice,
                            text,
                        });
                        slot.queue(ScriptEvent::Start);
                        slots.insert(prim_id, slot);
                    }
                    Err(e) => {
                        let _ = tx.send(ScriptCommand::Output {
                            prim_id,
                            level: ScriptLevel::Error,
                            text: e,
                        });
                    }
                }
            }
            HostMsg::Unload { prim_id } => {
                if slots.remove(&prim_id).is_some() {
                    let _ = tx.send(ScriptCommand::Output {
                        prim_id,
                        level: ScriptLevel::Notice,
                        text: "stopped".to_string(),
                    });
                }
            }
            HostMsg::Event { prim_id, event } => {
                if let Some(slot) = slots.get_mut(&prim_id) {
                    slot.queue(event);
                }
            }
            HostMsg::Tick { dt, views } => {
                for (prim_id, view) in views {
                    if let Some(slot) = slots.get_mut(&prim_id) {
                        slot.region_id = view.region_id;
                        *slot.shared.view.borrow_mut() = view;
                    }
                }
                step(&mut slots, &mut cursor, dt, &tx);
            }
            HostMsg::Shutdown => break,
        }
    }
}

/// One scheduling pass: advance timers, then spend the region's fuel bank on
/// whoever has work, round-robin from a rotating start.
fn step(slots: &mut HashMap<i64, Slot>, cursor: &mut usize, dt: f32, tx: &Sender<ScriptCommand>) {
    let mut ids: Vec<i64> = slots.keys().copied().collect();
    if ids.is_empty() {
        return;
    }
    ids.sort_unstable();

    for id in &ids {
        if let Some(slot) = slots.get_mut(id) {
            // The script clock advances with simulated time, for every script,
            // whether or not it gets a slice this tick — `vfGetTime` measures the
            // world going by, not the script's own share of the CPU.
            let t = &slot.shared.time;
            t.set(t.get() + dt);
            if let Some((interval, remaining)) = slot.timer.as_mut() {
                *remaining -= dt;
                if *remaining <= 0.0 {
                    *remaining = *interval;
                    slot.queue(ScriptEvent::Timer);
                }
            }
        }
    }

    // Everything `vfSend` produced this tick, gathered across the slice loop and
    // delivered *after* it. Two reasons it is not delivered inline. The loop holds
    // one slot mutably, and a listener is another slot. And delivering mid-loop
    // would make "does this listener see the message this tick or next?" depend on
    // where the round-robin cursor happened to be — a script's behaviour would
    // then change with the number of other scripts in the region, which is the
    // kind of thing that reproduces once a week.
    let mut posted: Vec<(i64, i64, i64, String)> = Vec::new();

    let mut bank = REGION_FUEL_PER_TICK;
    let start = *cursor % ids.len();
    for i in 0..ids.len() {
        if bank <= 0 {
            break;
        }
        let id = ids[(start + i) % ids.len()];
        let Some(slot) = slots.get_mut(&id) else {
            continue;
        };
        if slot.faulted || (slot.running.is_none() && slot.pending.is_empty()) {
            continue;
        }
        let budget = bank.min(SCRIPT_FUEL_SLICE);
        bank -= slot.run_slice(budget);

        // A `vfSetTimerEvent` call inside that slice takes effect now.
        if let Some(seconds) = slot.shared.timer_request.take() {
            slot.timer = if seconds <= 0.0 {
                None
            } else {
                let interval = seconds.max(MIN_TIMER_INTERVAL);
                Some((interval, interval))
            };
        }

        // `vfListen` calls made in that slice take effect now, as
        // `vfSetTimerEvent` does.
        for (channel, on) in slot.shared.listen_changes.borrow_mut().drain(..) {
            if on {
                if slot.listening.len() < MAX_LISTEN_CHANNELS {
                    slot.listening.insert(channel);
                }
            } else {
                slot.listening.remove(&channel);
            }
        }
        let region = slot.region_id;
        for (channel, text) in slot.shared.outbox.borrow_mut().drain(..) {
            posted.push((id, region, channel, text));
        }

        let dropped = slot.shared.dropped.replace(0);
        let mut out = std::mem::take(&mut *slot.shared.out.borrow_mut());
        if dropped > 0 {
            out.push(ScriptCommand::Output {
                prim_id: id,
                level: ScriptLevel::Notice,
                text: format!("{dropped} command(s) dropped — over the per-tick output budget"),
            });
        }
        for cmd in out {
            if tx.send(cmd).is_err() {
                return;
            }
        }
    }
    *cursor = cursor.wrapping_add(1);

    // Fan out. Scoped to the sender's region, because a channel that silently
    // crossed regions would let one estate's button work another's door — and
    // **never back to the sender**, which is the one deviation from Second Life
    // here and is deliberate: a script listening on the channel it sends on is the
    // ordinary shape (the beacon and the firebird both do it), and hearing
    // yourself makes that an immediate feedback loop the author did not write.
    for (sender, region, channel, text) in posted {
        for id in &ids {
            if *id == sender {
                continue;
            }
            let Some(slot) = slots.get_mut(id) else {
                continue;
            };
            if slot.faulted || slot.region_id != region || !slot.listening.contains(&channel) {
                continue;
            }
            slot.queue(ScriptEvent::Message {
                channel,
                text: text.clone(),
                sender,
            });
        }
    }
}

#[cfg(test)]
mod tests;

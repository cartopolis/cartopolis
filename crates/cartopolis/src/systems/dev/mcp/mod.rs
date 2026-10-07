//! **The client as something a model can drive.**
//!
//! A [Model Context Protocol](https://modelcontextprotocol.io) server hosted by
//! this client, so an editor's agent can move the camera, photograph what it
//! sees, and read the scene's numbers — over loopback HTTP, with a bearer token.
//!
//! **On by default on the desktop, switched in Settings ▸ General ▸ Editor
//! access.** The thing it enables is an editor attaching to a client that is
//! *already running*, and a launch flag cannot serve that: the moment somebody
//! wants it is the moment something is on screen and they want to ask about it.
//! So the listener is started and stopped while the app runs
//! ([`reconcile_listener`]), and the flags only decide *where* — or take a run
//! out of it entirely (`--no-mcp`, a scripted capture).
//!
//! # Why this and not the panel it replaces
//!
//! There used to be an AI panel in here: a chat box, an API key in `.env`, and
//! four tools that made one prim per call. It had the model in the wrong process
//! (the editor could not reach it), it shipped a credential, and its tool surface
//! was a prim at a time — the granularity nobody can show working. Agents that
//! build well elsewhere apply a *batch* and then render, inspect and revise.
//!
//! The half that was missing here is the second one. Until now every look at the
//! world cost a whole process: `--shot` starts the binary, loads the assets,
//! compiles the shaders and then fills the tiles — a settled ground-level fill is
//! ~25 s on its own (`docs/notes/tile-raster-cost.md`), and on a software
//! rasteriser the frames alone are seconds each. A client that is *already up*
//! pays none of that twice, and re-photographing a view it has already streamed
//! costs about a frame.
//!
//! # The three seams
//!
//! **Thread.** [`server`] runs a tokio runtime on a thread of its own, the same
//! arrangement `systems::network` uses for the sim transport. Nothing there
//! touches the `World`.
//!
//! **Frame.** A tool call becomes a [`Job`] on a channel with a oneshot reply.
//! [`pump`] drains the queue in `Update` and answers from the ECS. Calls that
//! take time do not block anything: they become a viewpoint for the camera
//! operator below and are answered when it lands.
//!
//! **Capture.** There is no second screenshot path. `view` pushes a
//! [`ShotSpec`](crate::systems::shot_harness::ShotSpec) onto the *shot harness*,
//! which already owns the only cycle that knows how to do this properly — place
//! the camera, wait for every streamer to go quiet, quiesce the CPU pool so the
//! GPU readback is not queued behind a rasterise backlog, fire the shutter, and
//! measure the world it photographed. A live harness differs from `--shot` in one
//! respect: running off the end of its list parks the camera operator instead of
//! quitting (`ShotHarness::live`).

pub mod agent;
pub mod overlay;
pub mod surroundings;

/// The job protocol — what crosses the channel between that crate's server
/// thread and [`pump`]. Defined there, since the server builds and reads it.
pub use cartopolis_mcp::protocol::{
    ImageFormat, Job, Reply, Request, ViewReply, ViewRequest, WorldState,
};
/// The listener and the tool surface live in their own Bevy-free crate
/// (`crates/mcp_server`), so the async/tower generic code is compiled outside
/// this one. Named `server` here as it always was.
pub use cartopolis_mcp::server;

use bevy::prelude::*;
use futures_channel::mpsc::UnboundedReceiver;
use futures_channel::oneshot;
use std::collections::VecDeque;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use crate::grid::AnchoredCamera;
use crate::resources::{CameraState, FrameStats, MapAnchor};
use crate::systems::globe::GlobeState;
use crate::systems::shot_harness::{ShotHarness, ShotProgress, ShotSpec, StreamWatch};

/// Where a bare `--mcp-listen` binds.
///
/// Loopback, and a port nothing else here uses (the sim is 4747, its HTTP
/// surface 4748). Reaching a client on another machine is an `ssh -L` away,
/// which is both how everything else already reaches the Steam Deck and a
/// meaningfully better answer than binding a LAN address: the tunnel is
/// authenticated and encrypted, and this is not.
pub const DEFAULT_LISTEN: &str = "127.0.0.1:4750";

/// How long a `view` may sit unanswered before the caller is told something is
/// wrong.
///
/// Generously above the harness's own budgets — `--wait` caps the settle and
/// `CAPTURE_GRACE_S` caps the readback — because this is not a deadline anything
/// is expected to approach. It is the backstop for the one case those cannot
/// cover: a harness that never advances at all, which would otherwise leave the
/// editor's request hanging until the process ends.
const JOB_TIMEOUT_S: f64 = 300.0;

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Where this run would serve, and how, if it serves at all.
///
/// A `Resource`, because whether it *is* serving is decided while the app runs
/// (`AppPrefs::mcp_enabled`, the Settings switch) and [`reconcile_listener`]
/// needs to know where to bind when it is switched back on.
#[derive(Resource, Clone, Debug)]
pub struct Config {
    pub listen: SocketAddr,
    /// No window: render offscreen at [`Config::size`] and stay up serving calls.
    pub headless: bool,
    /// The offscreen surface, when there is no window to be the surface.
    pub size: UVec2,
    /// **Serve regardless of the preference.** `--mcp-headless` has no Settings
    /// panel to switch anything from, so the flag is the whole decision; a
    /// `user.json` carried from a desktop with the switch off would otherwise
    /// leave a capture server answering nothing, with no way to say so.
    pub forced: bool,
    /// More `Host` header values to accept besides the listen address itself
    /// (`--mcp-host`). A client reached through a forward — the Steam Deck's,
    /// behind a port on the VPS — is asked for by the *forward's* name, which the
    /// DNS-rebinding guard would otherwise refuse as a stranger.
    pub hosts: Vec<String>,
}

impl Config {
    /// Resolve the flags, or `Ok(None)` when this run has no business serving.
    ///
    /// `excluded` is a run that is out of it entirely — a scripted capture, or
    /// `--no-mcp`. **A scripted capture is the interesting one**: `--shot` /
    /// `--shots` / `--perf` run a list and quit, so binding a port would collide
    /// with whatever client is already up on this machine, for a run no editor
    /// could attach to in the seconds it exists. Every other desktop run resolves
    /// to a config, and then the *preference* decides whether it is listening.
    ///
    /// `--mcp-headless` implies a listener: a windowless client that serves
    /// nothing would render to nobody and answer to nobody.
    pub fn resolve(
        listen: Option<&str>,
        headless: bool,
        size: Option<&str>,
        excluded: bool,
    ) -> Result<Option<Self>, String> {
        if excluded {
            // Reachable only for a scripted capture: clap already refuses
            // `--no-mcp` beside either of these.
            if listen.is_some() || headless {
                return Err(
                    "--mcp-listen and a scripted capture are alternatives: a `--shot` \
                            run photographs its list and quits, so there is nothing for an \
                            editor to drive."
                        .into(),
                );
            }
            return Ok(None);
        }
        let addr = listen.unwrap_or(DEFAULT_LISTEN);
        let parsed: SocketAddr = addr
            .parse()
            .map_err(|e| format!("--mcp-listen wants host:port, got {addr:?}: {e}"))?;
        let size = match size {
            Some(s) => crate::systems::shot_harness::parse_size(s)
                .ok_or_else(|| format!("--mcp-size wants WxH, got {s:?}"))?,
            None => {
                crate::systems::shot_harness::parse_size(crate::systems::shot_harness::DEFAULT_SIZE)
                    .expect("the default size parses")
            }
        };
        Ok(Some(Self {
            listen: parsed,
            headless,
            size,
            // A flag is an instruction for this launch and outranks the
            // remembered preference — the same rule the sky flags follow
            // (`user_store::SkyCliOverrides`).
            forced: headless || listen.is_some(),
            hosts: Vec::new(),
        }))
    }
}

/// The token the server checks, shared with the thread that checks it — as a
/// resource. The token itself, and the comparison, are
/// [`cartopolis_mcp::protocol::BearerToken`]; this wrapper exists because a type
/// from another crate cannot derive `Resource` here.
#[derive(Resource, Clone, Deref)]
pub struct McpAuth(pub cartopolis_mcp::protocol::BearerToken);

impl McpAuth {
    pub fn new(token: String) -> Self {
        Self(cartopolis_mcp::protocol::BearerToken(Arc::new(
            RwLock::new(token),
        )))
    }
}

/// A live listener, as a resource; see [`server::Running`], which it wraps for
/// the same reason [`McpAuth`] wraps the token. **Dropping it stops the server.**
#[derive(Resource)]
pub struct Running(pub server::Running);

/// Mint a token. 128 bits of v4 UUID, hex, no dashes — long enough that it is
/// not guessable and short enough to copy off a screen.
///
/// `uuid` is already in this crate's graph, and this is desktop-only code, so the
/// wasm trap in that crate's note (v4 needs its `js` feature) cannot apply here.
pub fn mint_token() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// Why the client is not serving, when it is not.
///
/// Kept so Settings can say *which* — "you switched it off" and "the port was
/// already taken" want different things from the reader, and a row that only
/// said "not serving" would send somebody looking for a setting they had not
/// touched.
#[derive(Resource, Default)]
pub struct McpTrouble(pub Option<String>);

/// Start or stop the listener to match [`AppPrefs::mcp_enabled`].
///
/// **The switch is the whole interface.** An editor attaches to a client that is
/// already running, so the decision to allow that is one somebody makes *while*
/// it runs — which a launch flag cannot serve. This system is what makes the
/// checkbox mean something: tick it and a socket opens, untick it and the
/// listener is dropped, which cancels its token and ends its thread.
///
/// It is change-detected on the preference rather than run every frame, and it
/// does not retry a failed bind: a port in use is a state a person resolves (by
/// closing the other client), and a system that kept trying would fill the log
/// and hide the reason. Untick and re-tick to try again.
pub fn reconcile_listener(
    mut commands: Commands,
    config: Res<Config>,
    prefs: Res<crate::resources::AppPrefs>,
    auth: Res<McpAuth>,
    sender: Res<JobSender>,
    running: Option<Res<Running>>,
    mut trouble: ResMut<McpTrouble>,
    mut exit: MessageWriter<AppExit>,
    mut was: Local<Option<bool>>,
) {
    let want = config.forced || prefs.mcp_enabled;
    if *was == Some(want) && running.is_some() == want {
        return;
    }
    *was = Some(want);
    match (want, running.is_some()) {
        (true, false) => match server::spawn(
            config.listen,
            config.hosts.clone(),
            auth.0.clone(),
            sender.0.clone(),
        ) {
            Ok(handle) => {
                tracing::info!(listen = %config.listen, "mcp: serving");
                trouble.0 = None;
                commands.insert_resource(Running(handle));
            }
            // **Fatal only when serving is the whole job.** A windowed client
            // that cannot open the socket is still a perfectly good client and
            // says so in Settings; `--mcp-headless` with no socket is a process
            // rendering for nobody, and the usual cause — another client already
            // on the port — is one a person fixes rather than waits out.
            Err(e) if config.headless => {
                tracing::error!(listen = %config.listen, error = %e, "mcp: headless run cannot serve");
                exit.write(AppExit::error());
            }
            Err(e) => {
                tracing::warn!(error = %e, "mcp: not serving");
                trouble.0 = Some(e);
            }
        },
        (false, true) => {
            tracing::info!("mcp: stopped — switched off in Settings");
            trouble.0 = None;
            commands.remove_resource::<Running>();
        }
        _ => {}
    }
}

/// The end of the job channel the server thread writes into.
///
/// A resource of its own because the *receiver* lives in [`McpBridge`] for the
/// life of the process while the sender is cloned into each listener — the
/// queue outlives any particular server, so switching the listener off and on
/// again does not lose a call that was already in flight.
#[derive(Resource)]
pub struct JobSender(pub futures_channel::mpsc::UnboundedSender<Job>);

// ---------------------------------------------------------------------------
// The bridge
// ---------------------------------------------------------------------------

/// One entry of the activity readout — see [`overlay`].
#[derive(Clone)]
pub struct Entry {
    pub label: String,
    pub detail: String,
    pub outcome: Outcome,
    /// `Time<Real>` seconds when this was last written, for the fade.
    pub at: f64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Working,
    Done,
    Failed,
}

/// The queue of calls waiting on the schedule, and the log the overlay draws.
#[derive(Resource)]
pub struct McpBridge {
    jobs: UnboundedReceiver<Job>,
    waiting: Vec<Waiting>,
    /// Agent requests drained this frame, for [`agent::serve`] — a queue of
    /// their own so the pump does not need that system's dozen parameters.
    agent: Vec<(agent::AgentRequest, oneshot::Sender<Reply>)>,
    /// Newest last. Bounded — this is a readout, not a record.
    pub log: VecDeque<Entry>,
    pub listen: SocketAddr,
    /// Calls answered since the process started, for the overlay's heading.
    pub served: u64,
}

/// A `view` that has been handed to the camera operator and not yet landed.
struct Waiting {
    shot_index: usize,
    started: f64,
    out: PathBuf,
    format: ImageFormat,
    max_edge: u32,
    reply: oneshot::Sender<Reply>,
    /// Index into [`McpBridge::log`]'s backing entry, by label, so the overlay
    /// can be updated when this lands.
    log_label: String,
}

impl McpBridge {
    pub fn new(jobs: UnboundedReceiver<Job>, listen: SocketAddr) -> Self {
        Self {
            jobs,
            waiting: Vec::new(),
            agent: Vec::new(),
            log: VecDeque::new(),
            listen,
            served: 0,
        }
    }

    /// Is the camera operator working for somebody right now?
    pub fn busy(&self) -> bool {
        !self.waiting.is_empty()
    }

    fn note(&mut self, label: String, detail: String, outcome: Outcome, at: f64) {
        const KEPT: usize = 8;
        if let Some(existing) = self.log.iter_mut().find(|e| e.label == label) {
            existing.detail = detail;
            existing.outcome = outcome;
            existing.at = at;
            return;
        }
        self.log.push_back(Entry {
            label,
            detail,
            outcome,
            at,
        });
        while self.log.len() > KEPT {
            self.log.pop_front();
        }
    }
}

/// Everything the Settings panel's Editor access row needs, as one parameter.
///
/// **One rather than three, because Bevy caps a system at sixteen** and
/// `render_settings_panel` was already at thirteen — the same cap
/// `free_camera::camera_controls` and `picking::prim_picking` each hit and
/// answered the same way. Every field is optional: the two resources exist only
/// on a build that was asked to serve, and the store is absent in a scripted run.
///
/// The user store is in here, rather than beside it, because the token *is* store
/// data: showing it and regenerating it are one row's worth of work and there is
/// no version of this row that reads one without the other.
#[derive(bevy::ecs::system::SystemParam)]
pub struct EditorAccess<'w> {
    pub config: Option<Res<'w, Config>>,
    pub auth: Option<Res<'w, McpAuth>>,
    pub running: Option<Res<'w, Running>>,
    pub trouble: Option<Res<'w, McpTrouble>>,
    pub store: Option<ResMut<'w, crate::systems::user_store::UserStore>>,
    pub audience: Option<ResMut<'w, agent::Audience>>,
}

/// What the Editor access row has to say about the listener right now.
pub enum Serving {
    /// Open, at this address.
    At(SocketAddr),
    /// Switched off here.
    Off,
    /// Wanted, and could not. The string is the reason, written for a reader —
    /// almost always another client already holding the port.
    Failed(String),
}

impl EditorAccess<'_> {
    /// The token an editor must present, or `None` when this build has no
    /// listener at all (a phone, a browser, a scripted capture).
    pub fn token(&self) -> Option<String> {
        self.config.as_deref()?;
        self.store.as_deref()?.data.mcp_token.clone()
    }

    /// Where it is serving, or why it is not.
    pub fn serving(&self) -> Option<Serving> {
        self.config.as_deref()?;
        Some(match (&self.running, &self.trouble) {
            (Some(running), _) => Serving::At(running.0.listen),
            (None, Some(trouble)) => match &trouble.0 {
                Some(why) => Serving::Failed(why.clone()),
                None => Serving::Off,
            },
            (None, None) => Serving::Off,
        })
    }

    /// Mint a new token, into the store *and* the lock the listener reads — so
    /// whatever an editor is holding stops working now rather than next launch.
    pub fn regenerate(&mut self) {
        let (Some(store), Some(auth)) = (self.store.as_deref_mut(), self.auth.as_deref()) else {
            return;
        };
        let minted = mint_token();
        auth.set(&minted);
        store.data.mcp_token = Some(minted);
        store.dirty = true;
    }

    /// Whose chat the agent may read, as the person typed it — the list is
    /// stored raw so a half-typed `jeroen,` keeps its comma — and whether a flag
    /// fixed it for this run.
    pub fn allow(&self) -> (String, bool) {
        match self.audience.as_deref() {
            Some(a) if a.pinned => (a.to_list(), true),
            _ => (
                self.store
                    .as_deref()
                    .and_then(|s| s.data.mcp_allow.clone())
                    .unwrap_or_default(),
                false,
            ),
        }
    }

    /// Change the list, in the store and in the filter at once, so the agent
    /// stops hearing somebody the moment they are taken off.
    pub fn set_allow(&mut self, list: String) {
        if self.audience.as_deref().is_some_and(|a| a.pinned) {
            return;
        }
        if let Some(audience) = self.audience.as_deref_mut() {
            *audience = agent::Audience::parse(&list);
        }
        if let Some(store) = self.store.as_deref_mut() {
            store.data.mcp_allow = Some(list);
            store.dirty = true;
        }
    }
}

// ---------------------------------------------------------------------------
// The pump
// ---------------------------------------------------------------------------

/// Drain the call queue and answer it from the ECS.
///
/// The only system in this module that touches the world. Everything it cannot
/// answer at once becomes a viewpoint on the harness and is answered by
/// [`land_captures`] when the shutter lands.
#[allow(clippy::too_many_arguments)]
pub fn pump(
    mut bridge: ResMut<McpBridge>,
    mut harness: ResMut<ShotHarness>,
    progress: Res<ShotProgress>,
    watch: StreamWatch,
    camera: AnchoredCamera,
    cam_state: Res<CameraState>,
    anchor: Res<MapAnchor>,
    globe: Res<GlobeState>,
    frames: Res<FrameStats>,
    real: Res<Time<Real>>,
    audience: Res<agent::Audience>,
    session: Res<crate::systems::session::SessionState>,
) {
    let now = real.elapsed_secs_f64();
    // **A picture is text the allow-list cannot filter.** The chrome carries the
    // chat overlay — speech bubbles and the transcript, i.e. every line from
    // everybody — so a capture with it on is a way round `agent::Audience`.
    // Online, and with a list narrower than everyone, the chrome is left out
    // whatever the call asked for. Offline there is no chat to leak.
    let chrome_leaks = session.is_online() && !audience.everyone;
    loop {
        // `Err` is both "nothing queued" and "the sender is gone"; neither has
        // anything more to give this frame.
        let Ok(job) = bridge.jobs.try_recv() else {
            break;
        };
        bridge.served += 1;
        match job.request {
            Request::State => {
                let state = measure_state(
                    &watch, &camera, &cam_state, &anchor, &globe, &frames, &progress,
                );
                let detail = if state.busy.is_empty() {
                    format!("settled at {:.5}, {:.5}", state.lat, state.lng)
                } else {
                    format!("streaming {}", state.busy.join(", "))
                };
                bridge.note("state".into(), detail, Outcome::Done, now);
                let _ = job.reply.send(Reply::State(Box::new(state)));
            }
            Request::Agent(req) => bridge.agent.push((req, job.reply)),
            Request::View(mut req) => {
                if chrome_leaks {
                    req.hide_ui = Some(true);
                }
                let label = format!("view #{}", bridge.served);
                match push_view(&mut harness, &req, &camera, &cam_state, &anchor, &globe) {
                    Ok(spec_index) => {
                        let out = harness.shots[spec_index]
                            .out
                            .clone()
                            .unwrap_or_else(|| default_out(spec_index));
                        bridge.note(
                            label.clone(),
                            if req.in_place {
                                "capturing in place…".to_string()
                            } else {
                                format!(
                                    "flying to {:.5}, {:.5} at {:.0} m…",
                                    req.lat, req.lng, req.alt
                                )
                            },
                            Outcome::Working,
                            now,
                        );
                        bridge.waiting.push(Waiting {
                            shot_index: spec_index,
                            started: now,
                            out,
                            format: req.format,
                            max_edge: req.max_edge,
                            reply: job.reply,
                            log_label: label,
                        });
                    }
                    Err(why) => {
                        bridge.note(label, why.clone(), Outcome::Failed, now);
                        let _ = job.reply.send(Reply::Error(why));
                    }
                }
            }
        }
    }
}

/// Answer every `view` whose capture has landed.
///
/// Separate from [`pump`] so it can run *after* `drive_capture` in the same
/// frame: the shot it is waiting on can then be answered on the frame it
/// completes rather than on the next one.
pub fn land_captures(
    mut bridge: ResMut<McpBridge>,
    progress: Res<ShotProgress>,
    real: Res<Time<Real>>,
) {
    if bridge.waiting.is_empty() {
        return;
    }
    let now = real.elapsed_secs_f64();
    // The harness advances its index past a viewpoint once that viewpoint is
    // finished with — saved, or given up on. Either way there is an answer to
    // give, and the metrics row distinguishes them.
    let landed: Vec<Waiting> = {
        let (done, still): (Vec<_>, Vec<_>) = std::mem::take(&mut bridge.waiting)
            .into_iter()
            .partition(|w| progress.shot_index() > w.shot_index || now - w.started > JOB_TIMEOUT_S);
        bridge.waiting = still;
        done
    };
    for w in landed {
        let timed_out = progress.shot_index() <= w.shot_index;
        let metrics = progress
            .metrics()
            .iter()
            .find(|m| m.shot == w.shot_index + 1)
            .and_then(|m| serde_json::to_value(m).ok());
        let took = now - w.started;
        let (reply, detail, outcome) = if timed_out {
            (
                Reply::Error(format!(
                    "the capture did not finish within {JOB_TIMEOUT_S:.0} s"
                )),
                "timed out".to_string(),
                Outcome::Failed,
            )
        } else {
            match encode_capture(&w.out, w.format, w.max_edge) {
                Ok((bytes, width, height)) => {
                    // **`busy`, not `settled`.** `settled` answers "was the quiet
                    // requirement met", which a call that asked for no patience —
                    // `view_here`'s default — meets trivially while the map is
                    // still arriving. What the caller needs to know is whether
                    // anything was still working, and that is this list.
                    let busy = metrics
                        .as_ref()
                        .and_then(|m| m.get("busy"))
                        .and_then(serde_json::Value::as_array)
                        .map(|layers| {
                            layers
                                .iter()
                                .filter_map(serde_json::Value::as_str)
                                .collect::<Vec<_>>()
                                .join(", ")
                        })
                        .filter(|s| !s.is_empty());
                    (
                        Reply::View(Box::new(ViewReply {
                            image: Some((w.format, bytes)),
                            width,
                            height,
                            path: w.out.clone(),
                            metrics: metrics.clone(),
                            took_s: took,
                            note: busy.as_ref().map(|layers| {
                                format!(
                                    "the world had not finished loading when this was taken: \
                                     {layers} still working"
                                )
                            }),
                        })),
                        match &busy {
                            Some(layers) => {
                                format!("{width}×{height} in {took:.1} s, still loading {layers}")
                            }
                            None => format!("{width}×{height} in {took:.1} s"),
                        },
                        Outcome::Done,
                    )
                }
                // The metrics row is pushed at the shutter, so a capture that
                // never read back still has its numbers and lacks only its
                // pixels. Hand those over rather than an error with nothing in
                // it — they are what explains the failure.
                Err(why) => (
                    Reply::View(Box::new(ViewReply {
                        image: None,
                        width: 0,
                        height: 0,
                        path: w.out.clone(),
                        metrics: metrics.clone(),
                        took_s: took,
                        note: Some(format!("no image: {why}")),
                    })),
                    format!("no image: {why}"),
                    Outcome::Failed,
                ),
            }
        };
        bridge.note(w.log_label, detail, outcome, now);
        let _ = w.reply.send(reply);
    }
}

// ---------------------------------------------------------------------------
// Idling (`--mcp-headless` only)
// ---------------------------------------------------------------------------

/// How often a headless client wakes while it has nothing to do.
///
/// A call that arrives while it sleeps waits at most this long to be picked up,
/// which is nothing beside a model's own turn. Everything that has to move
/// smoothly — a walk, a capture's settle — runs at the full rate, because it is
/// not idle (`pace_idle`).
const IDLE_HZ: f64 = 2.0;

/// When this frame started, for [`pace_idle`].
#[derive(Resource)]
pub struct FrameStart(std::time::Instant);

impl Default for FrameStart {
    fn default() -> Self {
        Self(std::time::Instant::now())
    }
}

pub fn mark_frame_start(mut start: ResMut<FrameStart>) {
    start.0 = std::time::Instant::now();
}

/// **A headless client slows right down between calls.** It renders into an
/// image nobody looks at except through `view`, and it used to draw the whole
/// city into it at the run loop's full rate (`MCP_IDLE_HZ` in `lib.rs`) while
/// waiting. That rate is kept for a capture, a walk or a call still being
/// carried out; with nothing in flight the frame is stretched to [`IDLE_HZ`]
/// here, at the end of it.
///
/// **Not drawing at all while idle was tried, and Bevy 0.19 cannot do it.**
/// Setting the camera's `is_active` to false panics on the next frame in
/// `bevy_pbr`'s `prepare_atmosphere_probe_bind_groups`, which builds the sky's
/// environment-light bind group with no active view and `unwrap`s the empty
/// transform buffer. The types involved are private to `bevy_pbr`, so there is
/// nothing on this side to switch off instead.
pub fn pace_idle(
    start: Res<FrameStart>,
    harness: Option<Res<ShotHarness>>,
    progress: Res<ShotProgress>,
    bridge: Res<McpBridge>,
    agent: Res<agent::AgentState>,
    streams: Res<crate::systems::stream_watch::StreamRegistry>,
) {
    // **Not while the map is still arriving.** Every streamer is bounded per
    // *frame*, so at the idle rate a teleport took minutes to fill in: the Deck
    // bot's `look_around` found buildings (which stream without a frame cap)
    // and no shops, benches or anything to sit on, for several minutes after
    // starting. The same `busy` list the capture's settle test waits on.
    let idle = !crate::systems::shot_harness::shot_in_progress(harness, progress)
        && !bridge.busy()
        && !agent.is_working()
        && streams.busy().is_empty();
    if !idle {
        return;
    }
    let frame = std::time::Duration::from_secs_f64(1.0 / IDLE_HZ);
    if let Some(rest) = frame.checked_sub(start.0.elapsed()) {
        std::thread::sleep(rest);
    }
}

/// Where a capture goes when the caller did not say.
fn default_out(index: usize) -> PathBuf {
    std::env::temp_dir().join(format!("cartopolis-view-{index:04}.png"))
}

/// Turn a `ViewRequest` into a viewpoint on the harness, returning its index.
fn push_view(
    harness: &mut ShotHarness,
    req: &ViewRequest,
    camera: &AnchoredCamera,
    cam_state: &CameraState,
    anchor: &MapAnchor,
    globe: &GlobeState,
) -> Result<usize, String> {
    let index = harness.shots.len();
    let mut spec = ShotSpec {
        lat: req.lat,
        lng: req.lng,
        alt: req.alt,
        pitch_deg: req.pitch_deg,
        yaw_deg: req.yaw_deg,
        name: None,
        description: None,
        // An editor aims with the camera it can see; `--look-at` is for a
        // scripted run that cannot.
        look_at: None,
        out: Some(req.out.clone().unwrap_or_else(|| default_out(index))),
        mode: Some(req.mode.clone()),
        hide_ui: req.hide_ui,
        ui: None,
        in_place: req.in_place,
        settle: req.settle,
        wait: req.wait,
        env: req.env.clone(),
    };
    if req.in_place {
        // Record where "here" actually is, so the reply and the metrics row say
        // what was photographed rather than repeating the zeros the call sent.
        let (lat, lng, alt) = where_now(camera, anchor, globe)
            .ok_or_else(|| "the world is not anchored yet; there is no `here`".to_string())?;
        spec.lat = lat;
        spec.lng = lng;
        spec.alt = alt;
        let (pitch, yaw) = cam_state.free_camera_rotation.into();
        spec.pitch_deg = pitch.to_degrees();
        spec.yaw_deg = yaw.to_degrees();
    } else if !(-90.0..=90.0).contains(&req.lat) || !(-180.0..=180.0).contains(&req.lng) {
        return Err(format!(
            "lat/lng out of range: {:.5}, {:.5}",
            req.lat, req.lng
        ));
    }
    harness.shots.push(spec);
    Ok(index)
}

/// The camera's position on Earth, and how high it is over the ground there.
fn where_now(
    camera: &AnchoredCamera,
    anchor: &MapAnchor,
    globe: &GlobeState,
) -> Option<(f64, f64, f64)> {
    let geo = anchor.geo?;
    let (cell, tf) = camera.get()?;
    let (lat, lng) = crate::systems::geo_nav::camera_geo(geo, globe, cell, tf);
    Some((lat, lng, globe.altitude_m(cell, tf)))
}

#[allow(clippy::too_many_arguments)]
fn measure_state(
    watch: &StreamWatch,
    camera: &AnchoredCamera,
    cam_state: &CameraState,
    anchor: &MapAnchor,
    globe: &GlobeState,
    frames: &FrameStats,
    progress: &ShotProgress,
) -> WorldState {
    let busy = watch.busy();
    let (lat, lng, altitude_m) = where_now(camera, anchor, globe).unwrap_or((0.0, 0.0, 0.0));
    let (pitch, yaw) = cam_state.free_camera_rotation.into();
    let (in_space, space_factor) = camera
        .get()
        .map(|(cell, tf)| (globe.in_space(cell, tf), globe.space_factor(cell, tf)))
        .unwrap_or((false, 0.0));
    WorldState {
        lat,
        lng,
        altitude_m,
        pitch_deg: pitch.to_degrees(),
        yaw_deg: yaw.to_degrees(),
        camera_mode: crate::systems::geo_nav::mode_name(cam_state.mode).to_string(),
        anchor: anchor.geo.map(|(lat, lng, _)| [lat, lng]),
        settled: busy.is_empty(),
        busy,
        in_space,
        space_factor,
        fps: frames.fps,
        frame_ms: frames.mean_ms,
        worst_frame_ms: frames.worst_ms,
        captures: progress.shot_index(),
    }
}

/// Read back the PNG the harness wrote, shrink it to the caller's budget and
/// encode it.
///
/// The file is the harness's output and stays on disk; this is a copy for the
/// wire. Downscaling here rather than rendering smaller is deliberate: the
/// capture should be the picture the client actually draws, and what a context
/// window can afford is a separate question from what the renderer is doing.
fn encode_capture(
    path: &std::path::Path,
    format: ImageFormat,
    max_edge: u32,
) -> Result<(Vec<u8>, u32, u32), String> {
    let img = image::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let (w, h) = (img.width(), img.height());
    let img = if max_edge > 0 && w.max(h) > max_edge {
        img.resize(max_edge, max_edge, image::imageops::FilterType::Lanczos3)
    } else {
        img
    };
    let (w, h) = (img.width(), img.height());
    let mut bytes = Vec::new();
    match format {
        ImageFormat::Jpeg => {
            let rgb = img.to_rgb8();
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, 85)
                .encode_image(&rgb)
                .map_err(|e| e.to_string())?;
        }
        ImageFormat::Png => {
            img.write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .map_err(|e| e.to_string())?;
        }
    }
    Ok((bytes, w, h))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An ordinary desktop run resolves to a loopback config it did not ask for
    /// — that is what "on by default" means — and leaves the decision to the
    /// preference rather than forcing it.
    #[test]
    fn an_ordinary_run_can_serve_without_being_asked() {
        let cfg = Config::resolve(None, false, None, false).unwrap().unwrap();
        assert!(cfg.listen.ip().is_loopback());
        assert_eq!(cfg.listen.port(), 4750);
        assert!(!cfg.headless);
        assert!(
            !cfg.forced,
            "nothing on the command line said so, so Settings decides"
        );
    }

    /// A flag is an instruction for this launch and outranks what Settings
    /// remembers — the rule the sky flags already follow.
    #[test]
    fn naming_an_address_forces_it_on() {
        let cfg = Config::resolve(Some("127.0.0.1:4790"), false, None, false)
            .unwrap()
            .unwrap();
        assert_eq!(cfg.listen.port(), 4790);
        assert!(cfg.forced);
    }

    /// A scripted capture binds nothing: it would collide with a client already
    /// running here, for a process no editor can attach to before it exits.
    #[test]
    fn a_scripted_capture_never_serves() {
        assert!(Config::resolve(None, false, None, true).unwrap().is_none());
        assert!(
            Config::resolve(Some(DEFAULT_LISTEN), false, None, true).is_err(),
            "and asking for both is a startup error rather than a silent winner"
        );
    }

    #[test]
    fn headless_implies_a_forced_listener() {
        let cfg = Config::resolve(None, true, None, false).unwrap().unwrap();
        assert!(cfg.headless);
        assert!(
            cfg.forced,
            "there is no Settings panel to switch it on from"
        );
        assert_eq!(cfg.listen.to_string(), DEFAULT_LISTEN);
    }

    #[test]
    fn a_bad_address_is_a_startup_error() {
        assert!(Config::resolve(Some("not-an-address"), false, None, false).is_err());
        assert!(Config::resolve(Some(DEFAULT_LISTEN), false, Some("big"), false).is_err());
    }

    /// The comparison must not accept a prefix, a suffix, or the empty token —
    /// the last one being the case that matters, since an unset token would
    /// otherwise let every caller in.
    #[test]
    fn the_token_is_compared_whole() {
        let auth = McpAuth::new(String::new());
        assert!(!auth.accepts(""), "an unset token accepts nothing");
        auth.set("0123456789abcdef");
        assert!(auth.accepts("0123456789abcdef"));
        assert!(!auth.accepts("0123456789abcde"));
        assert!(!auth.accepts("0123456789abcdef0"));
        assert!(!auth.accepts("0123456789abcdee"));
        assert!(!auth.accepts(""));
    }

    #[test]
    fn a_minted_token_is_long_and_hex() {
        let t = mint_token();
        assert_eq!(t.len(), 32);
        assert!(t.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(t, mint_token());
    }
}

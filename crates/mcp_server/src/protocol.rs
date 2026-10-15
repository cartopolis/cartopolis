//! What crosses the channel between the server thread and the client's
//! schedule: a [`Job`] carrying a [`Request`] one way and a [`Reply`] back on its
//! oneshot. Plain data, so the server can build and read it without the ECS.

use futures_channel::oneshot;
use std::path::PathBuf;

use crate::args::{AgentReply, AgentRequest};
use cartopolis_core::ShotEnv;

/// The longest edge of a returned image, unless the call asks for another.
///
/// A picture is the expensive part of an answer — a 1920×1080 PNG is a large
/// fraction of a context window — and the questions this is for (is the building
/// there, is the ground flat, did that change anything) are all legible at this
/// size. Ask for more when the question is about pixels.
pub const DEFAULT_MAX_EDGE: u32 = 1024;

// ---------------------------------------------------------------------------
// The job protocol
// ---------------------------------------------------------------------------

/// A tool call, on its way from the server thread to the schedule.
pub struct Job {
    pub request: Request,
    pub reply: oneshot::Sender<Reply>,
}

#[derive(Debug)]
pub enum Request {
    /// Everything about the world that can be answered without drawing it.
    State,
    /// Place the camera, wait for the world, photograph it.
    View(Box<ViewRequest>),
    /// Something the signed-in account does: talk, walk, build
    /// (the client's `mcp::agent`).
    Agent(AgentRequest),
}

/// One `view` / `view_here` call.
#[derive(Debug, Clone)]
pub struct ViewRequest {
    pub lat: f64,
    pub lng: f64,
    pub alt: f64,
    pub pitch_deg: f32,
    pub yaw_deg: f32,
    /// `free` (exact placement), `earth` (the orbit camera) or `avatar` (the
    /// follow camera, framing the body).
    pub mode: String,
    /// Photograph from where the camera already is; see the client's `ShotSpec::in_place`.
    pub in_place: bool,
    pub settle: Option<f32>,
    pub wait: Option<f32>,
    pub hide_ui: Option<bool>,
    pub env: ShotEnv,
    /// Where the PNG is written. The reply carries the path either way, because
    /// a file that outlives the answer is worth having: it can be opened, kept,
    /// or diffed against the next one.
    pub out: Option<PathBuf>,
    pub format: ImageFormat,
    pub max_edge: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFormat {
    /// The default. The map is photographic — imagery, terrain, thousands of
    /// small surfaces — and PNG holds that badly.
    Jpeg,
    /// For a question about exact pixels, where the codec must not have an
    /// opinion.
    Png,
}

impl ImageFormat {
    pub fn mime(self) -> &'static str {
        match self {
            Self::Jpeg => "image/jpeg",
            Self::Png => "image/png",
        }
    }
}

pub enum Reply {
    State(Box<WorldState>),
    View(Box<ViewReply>),
    Agent(Box<AgentReply>),
    /// The call could not be carried out. The string is written to be shown to
    /// whoever asked, because it is.
    Error(String),
}

/// What the world is doing right now, with nothing drawn.
///
/// The cheapest verification there is, and often the whole answer: "is the world
/// finished, and where am I looking?" needs no picture at all.
#[derive(Debug, Clone, serde::Serialize)]
pub struct WorldState {
    pub lat: f64,
    pub lng: f64,
    pub altitude_m: f64,
    pub pitch_deg: f32,
    pub yaw_deg: f32,
    pub camera_mode: String,
    /// The map's anchor, which is the origin every world coordinate here is
    /// measured from.
    pub anchor: Option<[f64; 2]>,
    /// Which map layers are still working; empty means every streamer has gone
    /// quiet — see
    /// the client's `shot_harness::StreamWatch::busy`.
    ///
    /// **`busy`, the same name `--dump-state` gives it**, and deliberately not a
    /// clearer one. An agent reading `busy` from a capture's metrics and
    /// `streaming` from this would reasonably assume they were different
    /// measurements of different things.
    pub busy: Vec<&'static str>,
    pub settled: bool,
    pub in_space: bool,
    pub space_factor: f32,
    pub fps: u32,
    pub frame_ms: f32,
    pub worst_frame_ms: f32,
    /// How many viewpoints this client has photographed since it started.
    pub captures: usize,
}

/// The answer to a `view`: the picture, where it was taken from, and every
/// number the harness measured about the world in it.
pub struct ViewReply {
    /// The encoded image, ready to be handed over as an MCP image block. `None`
    /// when the capture itself failed — the metrics are still worth having, and
    /// saying which half is missing beats an error with nothing in it.
    pub image: Option<(ImageFormat, Vec<u8>)>,
    pub width: u32,
    pub height: u32,
    pub path: PathBuf,
    /// The full `ShotMetrics` row, as `--dump-state` would write it.
    pub metrics: Option<serde_json::Value>,
    /// Seconds from the call arriving to the picture existing.
    pub took_s: f64,
    pub note: Option<String>,
}

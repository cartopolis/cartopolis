//! A voice agent for a call: what the people in it say goes to OpenAI's
//! Realtime API, what the model says comes back as speech, and the model's
//! function calls are the client's MCP tools (`docs/mockups/voice-agent/`).
//!
//! ```text
//! call session ──(who, 16 kHz PCM)──▶ mix ─▶ 24 kHz ─▶ input_audio_buffer.append ─┐
//!                                                                                  ▼
//! call session ◀──16 kHz, 50 frames/s── playout ◀─ 16 kHz ◀─ output_audio.delta ◀─ model
//!                                                                                  │
//!                                              ToolHost::call ◀── function call ◀──┘
//! ```
//!
//! [`VoiceAgent`] owns one thread per call with a runtime of its own. The
//! client hands it the call's decoded audio, a closure that sends a frame into
//! the call, and a [`ToolHost`]; it knows nothing else about Cartopolis.
//!
//! **Who may make it act is decided here, per tool call.** Read-only tools
//! answer anybody in the call. The rest answer only while the person the
//! model is answering is one of [`Config::trusted`], judged by who spoke last
//! ([`bridge`]'s speaker attribution). That rule, not a confirmation, is what
//! keeps a stranger the agent was sent to call from talking it into walking
//! off, building, or calling somebody else.

mod bridge;
pub mod playout;
pub mod realtime;
pub mod resample;

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

/// The call side's rate: what the client's session decodes to and encodes from.
pub const RATE: u32 = 16_000;
/// 20 ms at [`RATE`]: one Opus frame.
pub const FRAME: usize = (RATE as usize * 20) / 1000;

/// The one tool the bridge answers itself: the call's end belongs to the
/// bridge, which waits for the goodbye to be heard before hanging up.
pub const HANG_UP: &str = "hang_up";

/// In a meeting, what the model calls instead of speaking when nobody
/// addressed it: the bridge answers it and asks for nothing more.
pub const KEEP_LISTENING: &str = "keep_listening";

/// A tool as the model is offered it.
#[derive(Debug, Clone)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    /// JSON Schema for the arguments.
    pub parameters: serde_json::Value,
}

/// A tool's answer.
#[derive(Debug, Clone, Default)]
pub struct ToolOutput {
    pub text: String,
    /// A JPEG, base64, when the tool returned a picture.
    pub image: Option<String>,
    pub error: bool,
}

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Where the tools are. Called on the agent's own runtime.
pub trait ToolHost: Send + Sync + 'static {
    /// Every tool there is; [`Config`] chooses which the model sees.
    fn list(&self) -> BoxFuture<'_, Result<Vec<ToolSpec>, String>>;
    fn call(&self, name: String, arguments: serde_json::Value) -> BoxFuture<'_, ToolOutput>;
}

/// One call's agent.
#[derive(Debug, Clone)]
pub struct Config {
    /// `wss://api.openai.com/v1/realtime?model=…`.
    pub url: String,
    /// The bearer token: an API key, or a short-lived client secret.
    pub key: String,
    pub voice: String,
    /// Everything the model is told: who it is, whom it is talking to, why.
    pub instructions: String,
    /// Tools anybody in the call may make it use.
    pub read_only: Vec<String>,
    /// Tools only [`Self::trusted`] may make it use. Offered at all only when
    /// somebody trusted is in the call.
    pub full: Vec<String>,
    /// The configured users in this call, lower case.
    pub trusted: Vec<String>,
    /// Put "[Anna is speaking]" in front of the model when the speaker
    /// changes: for a meeting, where several voices reach it as one stream.
    pub announce_speakers: bool,
    /// Hand pictures from tools to the model.
    pub images: bool,
    /// The agent speaks first: it answered, or it called.
    pub speak_first: bool,
    /// Silence this long and the agent says goodbye and hangs up.
    pub idle_hangup: Duration,
    /// When the call ends, ask the model for one line in text with these
    /// instructions ([`Event::Outcome`]): what an outbound call established,
    /// for whoever asked for it.
    pub outcome: Option<String>,
}

/// What the agent tells the client.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// It said goodbye, or the line has been silent too long: hang up.
    HangUp,
    /// The line [`Config::outcome`] asked for.
    Outcome(String),
    /// It cannot go on. The text is for a log, and for the person who asked.
    Failed(String),
    /// The thread has ended; the agent can be dropped.
    Closed,
}

enum Cmd {
    Tell { text: String, respond: bool },
    Names(HashMap<u64, String>),
    Finish,
}

/// A running voice agent. Dropping it ends the conversation at once; call
/// [`VoiceAgent::finish`] and wait for [`Event::Closed`] to let it write its
/// outcome first.
pub struct VoiceAgent {
    cmds: tokio::sync::mpsc::UnboundedSender<Cmd>,
    events: Mutex<mpsc::Receiver<Event>>,
}

impl VoiceAgent {
    /// Start talking. `hear` carries what each person in the call says (their
    /// participant number and 20 ms at [`RATE`]); `speak` sends one 20 ms
    /// frame into the call, called 50 times a second, silence included.
    #[must_use]
    pub fn start(
        cfg: Config,
        tools: Arc<dyn ToolHost>,
        hear: mpsc::Receiver<(u64, Vec<f32>)>,
        speak: Box<dyn FnMut(Vec<f32>) + Send>,
    ) -> Self {
        let (cmds, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
        let (ev_tx, events) = mpsc::channel();
        bridge::spawn(cfg, tools, hear, speak, cmd_rx, ev_tx);
        Self {
            cmds,
            events: Mutex::new(events),
        }
    }

    /// Put a line in front of the model ("You arrived at the station"), and
    /// with `respond`, let it speak about it.
    pub fn tell(&self, text: &str, respond: bool) {
        let _ = self.cmds.send(Cmd::Tell {
            text: text.to_string(),
            respond,
        });
    }

    /// Who is who in the call, by participant number.
    pub fn names(&self, names: HashMap<u64, String>) {
        let _ = self.cmds.send(Cmd::Names(names));
    }

    /// The call is over: write the outcome if one was asked for, then close.
    pub fn finish(&self) {
        let _ = self.cmds.send(Cmd::Finish);
    }

    /// Whatever it has said since last asked.
    pub fn events(&self) -> Vec<Event> {
        self.events
            .lock()
            .map(|rx| rx.try_iter().collect())
            .unwrap_or_default()
    }
}

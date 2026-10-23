//! The conversation loop: one WebSocket to the model, a 10 ms tick that moves
//! audio both ways, and the tool calls in between.
//!
//! **Speaker attribution.** The model hears everybody as one stream. The
//! bridge keeps who was last heard above a speaking level, holds it for a
//! moment between words, and treats that person as the one the model is
//! answering: for [`Config::announce_speakers`] and for deciding whether a
//! tool from [`Config::full`] may run. Two people talking at once defeat it,
//! which is why everything that is not read-only waits for a trusted speaker.

use std::collections::{HashMap, HashSet};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use futures_util::{SinkExt as _, StreamExt as _};
use serde_json::{json, Value};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
use tokio_tungstenite::tungstenite::Message;

use crate::playout::Playout;
use crate::realtime::{self, Incoming, MODEL_RATE};
use crate::resample::{from_pcm16, to_pcm16, Resampler};
use crate::{
    Cmd, Config, Event, ToolHost, ToolOutput, ToolSpec, FRAME, HANG_UP, KEEP_LISTENING, RATE,
};

/// How often the loop moves audio. Half a frame, so a frame is never late by
/// more than that.
const TICK: Duration = Duration::from_millis(10);
const FRAME_TIME: Duration = Duration::from_millis(20);
/// Behind by more frames than this (a stall): skip ahead rather than talk late
/// for the rest of the call.
const MAX_BEHIND: u64 = 10;
/// A frame this loud (RMS, full scale 1) is somebody speaking: the call
/// screen's own level.
const SPEAKING_RMS: f32 = 0.02;
/// How long the speaker is held after their last loud frame.
const SPEAKER_HOLD: Duration = Duration::from_millis(800);
/// The pause after the goodbye has been played before the line is dropped.
const HANG_UP_AFTER: Duration = Duration::from_millis(400);
/// Asked to say goodbye after a long silence, and still there this long after.
const IDLE_GRACE: Duration = Duration::from_secs(20);
/// The longest a tool may take before the model is told it failed.
const TOOL_TIMEOUT: Duration = Duration::from_secs(90);
/// How long the outcome line may take once the call has ended.
const OUTCOME_WAIT: Duration = Duration::from_secs(20);

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;
type WsTx = futures_util::stream::SplitSink<Ws, Message>;

pub(crate) fn spawn(
    cfg: Config,
    tools: Arc<dyn ToolHost>,
    hear: mpsc::Receiver<(u64, Vec<f32>)>,
    speak: Box<dyn FnMut(Vec<f32>) + Send>,
    cmds: UnboundedReceiver<Cmd>,
    events: mpsc::Sender<Event>,
) {
    let spawned = std::thread::Builder::new()
        .name("voice".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    let _ = events.send(Event::Failed(format!("no runtime: {e}")));
                    let _ = events.send(Event::Closed);
                    return;
                }
            };
            let r = rt.block_on(run(cfg, tools, hear, speak, cmds, &events));
            if let Err(why) = r {
                tracing::warn!(%why, "voice: ended");
                let _ = events.send(Event::Failed(why));
            }
            let _ = events.send(Event::Closed);
        });
    if let Err(e) = spawned {
        tracing::warn!(error = %e, "voice: cannot start its thread");
    }
}

/// The tools the model may see: the configured ones that exist, plus the
/// bridge's own hang-up.
fn offered(cfg: &Config, all: Vec<ToolSpec>) -> Vec<ToolSpec> {
    let full = !cfg.trusted.is_empty();
    let mut out: Vec<ToolSpec> = all
        .into_iter()
        .filter(|t| t.name != HANG_UP && t.name != KEEP_LISTENING)
        .filter(|t| cfg.read_only.contains(&t.name) || (full && cfg.full.contains(&t.name)))
        .map(|mut t| {
            t.parameters = clean_schema(t.parameters);
            t
        })
        .collect();
    if cfg.announce_speakers {
        out.push(ToolSpec {
            name: KEEP_LISTENING.into(),
            description: "Say nothing this turn: call this instead of speaking when what was \
                          said was not meant for you."
                .into(),
            parameters: json!({ "type": "object", "properties": {} }),
        });
    }
    out.push(ToolSpec {
        name: HANG_UP.into(),
        description: "End this call. Say goodbye first: the line stays open until \
                      everything you said has been heard."
            .into(),
        parameters: json!({ "type": "object", "properties": {} }),
    });
    out
}

/// An MCP tool's schema as a function's parameters: without the keys that
/// describe the schema rather than the arguments.
fn clean_schema(mut v: Value) -> Value {
    if let Some(o) = v.as_object_mut() {
        o.remove("$schema");
        o.remove("title");
        o.entry("properties").or_insert_with(|| json!({}));
    }
    v
}

struct State {
    playout: Playout,
    up: Resampler,
    down: Resampler,
    /// A reply is being generated: no other may be asked for.
    responding: bool,
    /// Tool answers are in and the model should speak about them.
    respond_wanted: bool,
    /// Tool calls not answered yet.
    outstanding: usize,
    /// The model called `hang_up`; the line drops once its goodbye is played.
    hang_up: Option<Option<Instant>>,
    hung_up: bool,
    last_activity: Instant,
    idle_warned: Option<Instant>,
    /// Replies cut off by an interruption: their late audio is dropped.
    cut: HashSet<String>,
    /// Calls already handled, by id.
    calls_seen: HashSet<String>,
    started: Instant,
    frames_sent: u64,
    names: HashMap<u64, String>,
    loud_at: HashMap<u64, Instant>,
    speaker: Option<u64>,
    /// The call has ended and the outcome is being written: since when, and
    /// whether it has been asked for yet.
    finishing: Option<(Instant, bool)>,
}

impl State {
    /// Who the model is answering: whoever spoke last, or with one other
    /// person in the call and nobody heard yet, them.
    fn speaker_name(&self) -> Option<&str> {
        match self.speaker {
            Some(p) => self.names.get(&p).map(String::as_str),
            None if self.names.len() == 1 => self.names.values().next().map(String::as_str),
            None => None,
        }
    }
}

async fn run(
    cfg: Config,
    tools: Arc<dyn ToolHost>,
    hear: mpsc::Receiver<(u64, Vec<f32>)>,
    mut speak: Box<dyn FnMut(Vec<f32>) + Send>,
    mut cmds: UnboundedReceiver<Cmd>,
    events: &mpsc::Sender<Event>,
) -> Result<(), String> {
    let specs = offered(&cfg, tools.list().await?);
    let names: HashSet<String> = specs.iter().map(|t| t.name.clone()).collect();

    let mut req = cfg
        .url
        .as_str()
        .into_client_request()
        .map_err(|e| format!("bad voice service address {}: {e}", cfg.url))?;
    let auth = format!("Bearer {}", cfg.key)
        .parse()
        .map_err(|_| "the voice key is not a valid header".to_string())?;
    req.headers_mut().insert("Authorization", auth);
    let (ws, _) = tokio_tungstenite::connect_async(req)
        .await
        .map_err(|e| format!("cannot reach the voice service: {e}"))?;
    let (mut tx, mut rx) = ws.split();
    tracing::info!(tools = specs.len(), "voice: connected");

    send(
        &mut tx,
        realtime::session_update(&cfg.instructions, &cfg.voice, &specs),
    )
    .await?;

    let now = Instant::now();
    let mut s = State {
        playout: Playout::default(),
        up: Resampler::new(RATE, MODEL_RATE),
        down: Resampler::new(MODEL_RATE, RATE),
        responding: false,
        respond_wanted: cfg.speak_first,
        outstanding: 0,
        hang_up: None,
        hung_up: false,
        last_activity: now,
        idle_warned: None,
        cut: HashSet::new(),
        calls_seen: HashSet::new(),
        started: now,
        frames_sent: 0,
        names: HashMap::new(),
        loud_at: HashMap::new(),
        speaker: None,
        finishing: None,
    };
    try_respond(&mut tx, &mut s).await?;

    let (tool_tx, mut tool_rx) = tokio::sync::mpsc::unbounded_channel::<(String, ToolOutput)>();
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            _ = tick.tick() => {
                if on_tick(&cfg, &mut tx, &mut s, &hear, &mut speak, events).await? {
                    return Ok(());
                }
            }
            msg = rx.next() => {
                let text = match msg {
                    Some(Ok(Message::Text(t))) => t,
                    Some(Ok(Message::Close(_))) | None => {
                        return if s.finishing.is_some() {
                            Ok(())
                        } else {
                            Err("the voice service closed the connection".into())
                        };
                    }
                    Some(Ok(_)) => continue,
                    Some(Err(e)) => return Err(format!("the voice connection failed: {e}")),
                };
                let done = on_message(
                    &cfg, &mut tx, &mut s, &tools, &names, &tool_tx, realtime::parse(&text), events,
                )
                .await?;
                if done {
                    return Ok(());
                }
            }
            cmd = cmds.recv() => {
                match cmd {
                    // Dropped: end at once.
                    None => return Ok(()),
                    Some(Cmd::Names(n)) => s.names = n,
                    Some(Cmd::Tell { text, respond }) => {
                        send(&mut tx, realtime::system_note(&text)).await?;
                        s.last_activity = Instant::now();
                        if respond {
                            s.respond_wanted = true;
                            try_respond(&mut tx, &mut s).await?;
                        }
                    }
                    Some(Cmd::Finish) => {
                        if cfg.outcome.is_none() {
                            return Ok(());
                        }
                        if s.finishing.is_none() {
                            s.finishing = Some((Instant::now(), false));
                            s.playout.interrupt(RATE);
                            if s.responding {
                                send(&mut tx, json!({ "type": "response.cancel" })).await?;
                            }
                        }
                    }
                }
            }
            Some((call_id, out)) = tool_rx.recv() => {
                s.outstanding = s.outstanding.saturating_sub(1);
                if s.finishing.is_some() {
                    continue;
                }
                let text = if out.error {
                    format!("That did not work: {}", out.text)
                } else {
                    out.text
                };
                send(&mut tx, realtime::function_output(&call_id, &text)).await?;
                if let (true, Some(img)) = (cfg.images, out.image.as_deref()) {
                    send(&mut tx, realtime::image(img)).await?;
                }
                s.last_activity = Instant::now();
                s.respond_wanted = true;
                try_respond(&mut tx, &mut s).await?;
            }
        }
    }
}

async fn send(tx: &mut WsTx, v: Value) -> Result<(), String> {
    tx.send(Message::Text(v.to_string().into()))
        .await
        .map_err(|e| format!("the voice connection failed: {e}"))
}

async fn try_respond(tx: &mut WsTx, s: &mut State) -> Result<(), String> {
    if s.respond_wanted && !s.responding && s.outstanding == 0 && s.finishing.is_none() {
        s.respond_wanted = false;
        // Set before the server says so, or a second request in the meantime
        // would be refused for overlapping this one.
        s.responding = true;
        send(tx, realtime::respond()).await?;
    }
    Ok(())
}

/// Audio both ways, and the timers. `Ok(true)` ends the conversation.
async fn on_tick(
    cfg: &Config,
    tx: &mut WsTx,
    s: &mut State,
    hear: &mpsc::Receiver<(u64, Vec<f32>)>,
    speak: &mut Box<dyn FnMut(Vec<f32>) + Send>,
    events: &mpsc::Sender<Event>,
) -> Result<bool, String> {
    let now = Instant::now();
    if let Some((since, asked)) = s.finishing {
        if !asked && !s.responding {
            let instructions = cfg.outcome.clone().unwrap_or_default();
            send(tx, realtime::respond_in_text(&instructions)).await?;
            s.responding = true;
            s.finishing = Some((since, true));
        }
        if now.duration_since(since) > OUTCOME_WAIT {
            tracing::warn!("voice: no outcome line in time");
            return Ok(true);
        }
        // The call is gone: nothing to hear and nobody to speak to.
        hear.try_iter().for_each(drop);
        return Ok(false);
    }

    // In: everybody's frames since the last tick, mixed, and who is talking.
    let mut by_sender: HashMap<u64, Vec<f32>> = HashMap::new();
    for (from, pcm) in hear.try_iter() {
        let rms = (pcm.iter().map(|x| x * x).sum::<f32>() / pcm.len().max(1) as f32).sqrt();
        if rms > SPEAKING_RMS {
            s.loud_at.insert(from, now);
        }
        by_sender.entry(from).or_default().extend(pcm);
    }
    if !by_sender.is_empty() {
        let len = by_sender.values().map(Vec::len).max().unwrap_or(0);
        let mut mix = vec![0.0f32; len];
        for pcm in by_sender.values() {
            for (m, x) in mix.iter_mut().zip(pcm) {
                *m += x;
            }
        }
        let mut at24 = Vec::with_capacity(len * 3 / 2 + 8);
        s.up.push(&mix, &mut at24);
        if !at24.is_empty() {
            send(tx, realtime::append_audio(&to_pcm16(&at24))).await?;
        }
    }
    let speaker = s
        .loud_at
        .iter()
        .filter(|(_, at)| now.duration_since(**at) < SPEAKER_HOLD)
        .max_by_key(|(_, at)| **at)
        .map(|(p, _)| *p);
    if speaker.is_some() && speaker != s.speaker {
        s.speaker = speaker;
        if cfg.announce_speakers {
            if let Some(name) = s.speaker_name() {
                let note = format!("[{name} is speaking]");
                send(tx, realtime::system_note(&note)).await?;
            }
        }
    }

    // Out: the frames due by now, speech or silence.
    let due = (now.duration_since(s.started).as_millis() / FRAME_TIME.as_millis()) as u64;
    if due > s.frames_sent + MAX_BEHIND {
        s.frames_sent = due - 1;
    }
    while s.frames_sent < due {
        let frame = s.playout.next_frame().unwrap_or_else(|| vec![0.0; FRAME]);
        speak(frame);
        s.frames_sent += 1;
    }

    // Hanging up, once the goodbye has been heard.
    if let Some(at) = s.hang_up {
        if !s.responding && s.playout.is_empty() {
            match at {
                None => s.hang_up = Some(Some(now + HANG_UP_AFTER)),
                Some(at) if now >= at => {
                    s.hang_up = None;
                    s.hung_up = true;
                    let _ = events.send(Event::HangUp);
                }
                Some(_) => {}
            }
        }
    }

    // A silent line: ask for a goodbye, then drop it if none comes.
    let quiet = !s.responding && s.playout.is_empty() && s.outstanding == 0;
    if quiet && !s.hung_up && s.hang_up.is_none() {
        match s.idle_warned {
            None if now.duration_since(s.last_activity) > cfg.idle_hangup => {
                s.idle_warned = Some(now);
                send(
                    tx,
                    realtime::system_note(
                        "Nobody has said anything for a while. Say a short goodbye and hang up.",
                    ),
                )
                .await?;
                s.respond_wanted = true;
                try_respond(tx, s).await?;
            }
            Some(at) if now.duration_since(at) > IDLE_GRACE => {
                s.hung_up = true;
                let _ = events.send(Event::HangUp);
            }
            _ => {}
        }
    }
    Ok(false)
}

/// One message from the model. `Ok(true)` ends the conversation.
#[allow(clippy::too_many_arguments)]
async fn on_message(
    cfg: &Config,
    tx: &mut WsTx,
    s: &mut State,
    tools: &Arc<dyn ToolHost>,
    offered: &HashSet<String>,
    tool_tx: &UnboundedSender<(String, ToolOutput)>,
    msg: Incoming,
    events: &mpsc::Sender<Event>,
) -> Result<bool, String> {
    let now = Instant::now();
    match msg {
        Incoming::Ready | Incoming::Other => {}
        Incoming::Audio { item, pcm16 } => {
            if s.finishing.is_none() && !s.cut.contains(&item) {
                let mut at16 = Vec::new();
                s.down.push(&from_pcm16(&pcm16), &mut at16);
                s.playout.push(&item, at16);
                s.last_activity = now;
            }
        }
        Incoming::SpeechStarted => {
            s.last_activity = now;
            s.idle_warned = None;
            if let Some((item, ms)) = s.playout.interrupt(RATE) {
                send(tx, realtime::truncate(&item, ms)).await?;
                s.cut.insert(item);
            }
        }
        Incoming::ResponseStarted => s.responding = true,
        Incoming::ResponseDone => {
            s.responding = false;
            try_respond(tx, s).await?;
        }
        Incoming::Text(text) => {
            if s.finishing.is_some() {
                let line = text.trim().to_string();
                if !line.is_empty() {
                    let _ = events.send(Event::Outcome(line));
                }
                return Ok(true);
            }
        }
        Incoming::Error(why) => {
            // Most are about one request (a reply asked for while another
            // ran), not the session; a fatal one is followed by the close.
            tracing::warn!(%why, "voice: the service refused something");
        }
        Incoming::Call {
            call_id,
            name,
            arguments,
        } => {
            if !s.calls_seen.insert(call_id.clone()) || s.finishing.is_some() {
                return Ok(false);
            }
            s.last_activity = now;
            if name == HANG_UP {
                s.hang_up = Some(None);
                let said = "Hanging up as soon as what you said has been heard.";
                send(tx, realtime::function_output(&call_id, said)).await?;
                return Ok(false);
            }
            if name == KEEP_LISTENING {
                send(tx, realtime::function_output(&call_id, "Listening.")).await?;
                return Ok(false);
            }
            if let Some(refusal) = refusal(cfg, s, offered, &name) {
                tracing::info!(tool = %name, "voice: refused a tool");
                send(tx, realtime::function_output(&call_id, &refusal)).await?;
                s.respond_wanted = true;
                return Ok(false);
            }
            let args: Value = serde_json::from_str(&arguments).unwrap_or_else(|_| json!({}));
            tracing::info!(tool = %name, "voice: tool");
            s.outstanding += 1;
            let tools = Arc::clone(tools);
            let tool_tx = tool_tx.clone();
            tokio::spawn(async move {
                let out = tokio::time::timeout(TOOL_TIMEOUT, tools.call(name, args))
                    .await
                    .unwrap_or_else(|_| ToolOutput {
                        text: "it took too long and was given up".into(),
                        image: None,
                        error: true,
                    });
                let _ = tool_tx.send((call_id, out));
            });
        }
    }
    Ok(false)
}

/// Why a tool may not run now, said to the model.
fn refusal(cfg: &Config, s: &State, offered: &HashSet<String>, name: &str) -> Option<String> {
    if !offered.contains(name) {
        return Some(format!("There is no tool called {name} on this call."));
    }
    if cfg.read_only.iter().any(|t| t == name) {
        return None;
    }
    let speaker = s.speaker_name().map(str::to_lowercase);
    match speaker {
        Some(who) if cfg.trusted.contains(&who) => None,
        _ => Some(format!(
            "Refused: only {} can ask you to do that, and the person you are answering \
             is not one of them. Say you can't do that on this call.",
            cfg.trusted.join(" or ")
        )),
    }
}

#[cfg(test)]
mod tests;

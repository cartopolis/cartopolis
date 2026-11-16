//! The script engine: prim scripts in a Bun process (`tools/script-host`),
//! behind [`ScriptHost`](super::ScriptHost).
//!
//! The host thread takes `HostMsg`s and answers with `ScriptCommand`s by
//! speaking newline-delimited JSON with a child process, and it is that
//! process's supervisor as well:
//!
//! - **A hung tick is killed.** JavaScriptCore cannot stop a built-in mid-call (a
//!   regular expression that backtracks, a string doubled to a gigabyte), so a
//!   tick that has not answered in [`BunEngine::tick_timeout`] ends the process.
//! - **So is one past its memory ceiling**, read from the operating system while
//!   the tick runs, since one realm has no per-script allocator to refuse with.
//! - **The prim that was running is blamed** — the child writes its id into a
//!   file both processes map, which can still be read after the kill — and
//!   stays stopped; every other script is loaded again into a fresh process.
//!   That restart loses their state: they begin again at `on_start`.
//!
//! The child gets an empty environment: it holds untrusted code, and nothing the
//! simulator was started with (an admin password, a token) belongs in it.

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use cartopolis_core::ScriptLevel;
use glam::Vec3;
use serde_json::{json, Map, Value};

use super::{rules, HostMsg, PrimView, ScriptCommand, ScriptEvent};

/// Which script process to run and how it is held to account.
#[derive(Debug, Clone)]
pub struct BunEngine {
    /// What is started: the compiled `cartopolis-script-host`
    /// (`deploy/build/script-host.sh`, Bun inside it), or `bun` 1.4 or later
    /// running a checkout's `tools/script-host`.
    pub program: PathBuf,
    /// `tools/script-host` when [`Self::program`] is `bun`, holding
    /// `src/child.ts` and its `node_modules`; `None` for the compiled host.
    pub host_dir: Option<PathBuf>,
    /// A tick that has not answered by then is a hung script.
    pub tick_timeout: Duration,
    /// Resident memory past which the process is killed outright.
    pub memory_limit_bytes: u64,
}

/// The compiled script host's file name, looked for beside the simulator's
/// own binary.
pub const COMPILED_HOST: &str = "cartopolis-script-host";

impl BunEngine {
    pub const DEFAULT_TICK_TIMEOUT: Duration = Duration::from_millis(1000);
    pub const DEFAULT_MEMORY_LIMIT_BYTES: u64 = 1 << 30;

    /// The compiled host at `program`.
    #[must_use]
    pub fn compiled(program: PathBuf) -> Self {
        Self {
            program,
            host_dir: None,
            tick_timeout: Self::DEFAULT_TICK_TIMEOUT,
            memory_limit_bytes: Self::DEFAULT_MEMORY_LIMIT_BYTES,
        }
    }

    /// `bun` running the script host's source in `host_dir`.
    #[must_use]
    pub fn source(bun: PathBuf, host_dir: PathBuf) -> Self {
        Self {
            program: bun,
            host_dir: Some(host_dir),
            ..Self::compiled(PathBuf::new())
        }
    }

    /// This checkout's `tools/script-host` under the `bun` on the path: right
    /// for the tests and `cargo run`, wrong for an installed server, which
    /// finds [`COMPILED_HOST`] beside itself instead (`SimConfig::script_host`).
    #[must_use]
    pub fn development() -> Self {
        Self::source(
            "bun".into(),
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/script-host"),
        )
    }
}

struct Process {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
}

struct Supervisor {
    engine: BunEngine,
    blame: PathBuf,
    process: Process,
    /// Every loaded source, so a restart can load them again.
    sources: HashMap<i64, String>,
    /// Prims blamed for a kill: not loaded again until their script is stored again.
    blamed: HashSet<i64>,
    /// The last particle sequence each prim was seen with, for the next one.
    sequences: HashMap<i64, u32>,
}

impl Supervisor {
    fn start(engine: BunEngine) -> std::io::Result<Self> {
        // One file per host, not per process: two hosts in one process (the
        // test suite runs several at once) blamed each other's scripts.
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let blame = std::env::temp_dir().join(format!(
            "cartopolis-script-blame-{}-{n}",
            std::process::id()
        ));
        std::fs::write(&blame, [0u8; 8])?;
        let process = spawn(&engine, &blame)?;
        Ok(Self {
            engine,
            blame,
            process,
            sources: HashMap::new(),
            blamed: HashSet::new(),
            sequences: HashMap::new(),
        })
    }

    fn send(&mut self, value: &Value) {
        let mut line = value.to_string();
        line.push('\n');
        // A dead child shows up as a missing answer to the next tick, where the
        // restart happens; a failed write here has nothing better to do.
        let _ = self.process.stdin.write_all(line.as_bytes());
        let _ = self.process.stdin.flush();
    }

    /// The child's next line, or `None` if it had to be killed (or died) first.
    fn answer(&mut self, tx: &Sender<ScriptCommand>) -> Option<String> {
        let started = Instant::now();
        let reason = loop {
            match self.process.lines.recv_timeout(Duration::from_millis(20)) {
                Ok(line) => return Some(line),
                Err(RecvTimeoutError::Disconnected) => {
                    break match self.process.child.try_wait() {
                        Ok(Some(status)) => format!("the script process exited ({status})"),
                        _ => "the script process closed its output".to_string(),
                    };
                }
                Err(RecvTimeoutError::Timeout) => {
                    if started.elapsed() > self.engine.tick_timeout {
                        break format!(
                            "did not finish within {} ms",
                            self.engine.tick_timeout.as_millis()
                        );
                    }
                    if resident_bytes(self.process.child.id()) > self.engine.memory_limit_bytes {
                        break format!(
                            "took the script process past {} MiB",
                            self.engine.memory_limit_bytes >> 20
                        );
                    }
                }
            }
        };
        let _ = self.process.child.kill();
        let _ = self.process.child.wait();
        self.restart(&reason, tx);
        None
    }

    fn restart(&mut self, reason: &str, tx: &Sender<ScriptCommand>) {
        let blamed = std::fs::read(&self.blame)
            .ok()
            .and_then(|b| {
                b.get(..8)
                    .map(|b| f64::from_le_bytes(b.try_into().unwrap_or([0; 8])))
            })
            .unwrap_or(0.0) as i64;
        tracing::warn!(blamed, reason, "script process killed; restarting it");
        if blamed != 0 {
            self.blamed.insert(blamed);
            let _ = tx.send(ScriptCommand::Output {
                prim_id: blamed,
                level: ScriptLevel::Error,
                text: format!("{reason} while this script was running; stopped"),
            });
        }
        let _ = std::fs::write(&self.blame, [0u8; 8]);
        match spawn(&self.engine, &self.blame) {
            Ok(p) => self.process = p,
            Err(e) => {
                tracing::error!(error = %e, "could not restart the script process");
                return;
            }
        }
        let reload: Vec<(i64, String)> = self
            .sources
            .iter()
            .filter(|(id, _)| !self.blamed.contains(id))
            .map(|(id, s)| (*id, s.clone()))
            .collect();
        for (prim_id, source) in reload {
            self.send(&json!({"op": "load", "prim_id": prim_id, "source": source}));
            let _ = tx.send(ScriptCommand::Output {
                prim_id,
                level: ScriptLevel::Notice,
                text: "restarted: the script process was restarted because of another script"
                    .into(),
            });
        }
    }

    /// One command from the child, checked again: everything in the realm is a
    /// script's to replace, the API functions included.
    fn command(&mut self, raw: &Map<String, Value>) -> Result<Option<ScriptCommand>, String> {
        let prim_id = raw
            .get("prim_id")
            .and_then(Value::as_i64)
            .ok_or("a command without a prim")?;
        let (kind, v) = raw
            .iter()
            .find(|(k, _)| k.as_str() != "prim_id")
            .ok_or("an empty command")?;
        let f = |key: &str| -> Result<f32, String> {
            v.get(key)
                .and_then(Value::as_f64)
                .map(|n| n as f32)
                .ok_or_else(|| format!("{kind}: `{key}` is not a number"))
                .and_then(rules::finite)
        };
        let vec3 = |key: &str| -> Result<Vec3, String> {
            let a = v
                .get(key)
                .and_then(Value::as_array)
                .ok_or_else(|| format!("{kind}: `{key}` is not a list"))?;
            let at = |i: usize| {
                a.get(i)
                    .and_then(Value::as_f64)
                    .map(|n| n as f32)
                    .ok_or_else(|| format!("{kind}: `{key}` needs three numbers"))
                    .and_then(rules::finite)
            };
            Ok(Vec3::new(at(0)?, at(1)?, at(2)?))
        };
        let on = || v.get("on").and_then(Value::as_bool).unwrap_or(false);
        let text = |key: &str| {
            v.get(key)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        let opt = |key: &str| v.get(key).and_then(Value::as_f64).map(|n| n as f32);
        Ok(Some(match kind.as_str() {
            "SetPos" => ScriptCommand::SetPos {
                prim_id,
                pos: vec3("pos")?,
            },
            "SetRot" => ScriptCommand::SetRot {
                prim_id,
                rot: vec3("rot")?,
            },
            "SetScale" => ScriptCommand::SetScale {
                prim_id,
                scale: vec3("scale")?.clamp(Vec3::splat(0.01), Vec3::splat(64.0)),
            },
            "SetColor" => {
                let c = vec3("color")?.clamp(Vec3::ZERO, Vec3::ONE);
                ScriptCommand::SetColor {
                    prim_id,
                    color: c.to_array(),
                }
            }
            "SetAlpha" => ScriptCommand::SetAlpha {
                prim_id,
                alpha: f("alpha")?.clamp(0.0, 1.0),
            },
            "SetGlow" => ScriptCommand::SetGlow {
                prim_id,
                glow: f("glow")?.clamp(0.0, 1.0),
            },
            "SetFullBright" => ScriptCommand::SetFullBright { prim_id, on: on() },
            "SetPhysical" => ScriptCommand::SetPhysical { prim_id, on: on() },
            "SetName" => ScriptCommand::SetName {
                prim_id,
                name: text("name").chars().take(64).collect(),
            },
            "SetGeo" => {
                let lat = v
                    .get("latitude")
                    .and_then(Value::as_f64)
                    .filter(|l| l.is_finite() && l.abs() <= 90.0);
                let lng = v
                    .get("longitude")
                    .and_then(Value::as_f64)
                    .filter(|l| l.is_finite() && l.abs() <= 180.0);
                let (Some(latitude), Some(longitude)) = (lat, lng) else {
                    return Err(
                        "set_geo: expected a latitude in -90..90 and a longitude in -180..180"
                            .into(),
                    );
                };
                ScriptCommand::SetGeo {
                    prim_id,
                    latitude,
                    longitude,
                }
            }
            "SetVel" => ScriptCommand::SetVel {
                prim_id,
                velocity: vec3("velocity")?,
            },
            "ApplyImpulse" => ScriptCommand::ApplyImpulse {
                prim_id,
                impulse: vec3("impulse")?,
            },
            "SenseHands" => ScriptCommand::SenseHands {
                prim_id,
                range: f("range")?.clamp(0.0, cartopolis_core::tone::MAX_HAND_RANGE_M),
            },
            "Die" => ScriptCommand::Die { prim_id },
            "Say" => ScriptCommand::Say {
                prim_id,
                text: text("text").chars().take(super::MAX_TEXT).collect(),
                radius: f("radius")?,
            },
            "Output" => ScriptCommand::Output {
                prim_id,
                level: match v.get("level").and_then(Value::as_str) {
                    Some("Error") => ScriptLevel::Error,
                    Some("Notice") => ScriptLevel::Notice,
                    _ => ScriptLevel::Info,
                },
                text: text("text").chars().take(super::MAX_TEXT).collect(),
            },
            "SetParticles" => {
                let effect = match v.get("rules") {
                    None | Some(Value::Null) => None,
                    Some(Value::Object(r)) => {
                        let burst = v.get("burst").and_then(Value::as_bool).unwrap_or(false);
                        let sequence = self
                            .sequences
                            .get(&prim_id)
                            .map_or(1, |s| s.wrapping_add(1));
                        self.sequences.insert(prim_id, sequence);
                        Some(rules::effect(r, burst, sequence)?)
                    }
                    Some(_) => return Err("particles: the rules must be an object".into()),
                };
                ScriptCommand::SetParticles { prim_id, effect }
            }
            "PlaySound" => ScriptCommand::PlaySound {
                prim_id,
                sound: rules::sound("play_sound", &text("name"), opt("volume"), opt("range"))?,
            },
            "SetSoundLoop" => ScriptCommand::SetSoundLoop {
                prim_id,
                sound: match v {
                    Value::Null => None,
                    _ => Some(rules::sound(
                        "loop_sound",
                        &text("name"),
                        opt("volume"),
                        opt("range"),
                    )?),
                },
            },
            "SetTone" => ScriptCommand::SetTone {
                prim_id,
                tone: match v {
                    Value::Object(r) => Some(rules::tone(r)?),
                    _ => None,
                },
            },
            other => return Err(format!("an unknown command `{other}`")),
        }))
    }
}

/// `bin` as a path the child can be started from. A bare name is looked up on
/// this process's `PATH` here, because `Command` looks it up on the *child's*,
/// which [`spawn`] clears to `/usr/bin:/bin` — where a Homebrew `bun` is not, so
/// every script failed to start on the Mac build nodes (2026-11-15).
fn resolve_bin(bin: &std::path::Path) -> PathBuf {
    if bin.components().count() > 1 {
        return bin.to_path_buf();
    }
    std::env::var_os("PATH")
        .and_then(|paths| {
            std::env::split_paths(&paths)
                .map(|dir| dir.join(bin))
                .find(|candidate| candidate.is_file())
        })
        .unwrap_or_else(|| bin.to_path_buf())
}

/// A checkout's script host needs its packages; a fresh one has none. Installed
/// from the lockfile once, the first time a host starts there.
fn install_packages(bun: &std::path::Path, host_dir: &std::path::Path) -> std::io::Result<()> {
    if host_dir.join("node_modules/acorn").exists() {
        return Ok(());
    }
    let status = Command::new(bun)
        .args(["install", "--frozen-lockfile"])
        .current_dir(host_dir)
        .stdout(Stdio::null())
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "`bun install` in {} failed ({status})",
            host_dir.display()
        )))
    }
}

fn spawn(engine: &BunEngine, blame: &std::path::Path) -> std::io::Result<Process> {
    let program = resolve_bin(&engine.program);
    let mut command = Command::new(&program);
    match &engine.host_dir {
        Some(dir) => {
            install_packages(&program, dir)?;
            command.arg(dir.join("src/child.ts")).current_dir(dir);
        }
        // Nothing to find beside it: it carries its sources and Bun.
        None => {
            command.current_dir(std::env::temp_dir());
        }
    }
    let mut child = command
        .arg(blame)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()?;
    let stdin = child.stdin.take().expect("piped stdin");
    let stdout = child.stdout.take().expect("piped stdout");
    let (tx, lines) = mpsc::channel();
    std::thread::Builder::new()
        .name("cartopolis-scripts-read".into())
        .spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        })?;
    Ok(Process {
        child,
        stdin,
        lines,
    })
}

/// Resident memory of `pid`: /proc on Linux, `ps` elsewhere.
fn resident_bytes(pid: u32) -> u64 {
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string(format!("/proc/{pid}/status"))
            .ok()
            .and_then(|s| {
                s.lines()
                    .find_map(|l| l.strip_prefix("VmRSS:"))
                    .and_then(|v| v.trim().trim_end_matches("kB").trim().parse::<u64>().ok())
            })
            .map_or(0, |kb| kb * 1024)
    }
    #[cfg(not(target_os = "linux"))]
    {
        Command::new("ps")
            .args(["-o", "rss=", "-p", &pid.to_string()])
            .output()
            .ok()
            .and_then(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .trim()
                    .parse::<u64>()
                    .ok()
            })
            .map_or(0, |kb| kb * 1024)
    }
}

fn view_json(v: &PrimView) -> Value {
    json!({
        "pos": [v.pos.x, v.pos.y, v.pos.z],
        "rot": [v.rot.x, v.rot.y, v.rot.z],
        "color": v.color,
        "scale": [v.scale.x, v.scale.y, v.scale.z],
        "alpha": v.alpha,
        "glow": v.glow,
        "full_bright": v.full_bright,
        "name": v.name,
        "geo": [v.geo.latitude, v.geo.longitude],
        "nearby": v.nearby.iter().map(|(id, name, d)| json!([id, name, d])).collect::<Vec<_>>(),
        "physical": v.physical,
        "velocity": [v.velocity.x, v.velocity.y, v.velocity.z],
        "mass": v.mass,
    })
}

fn event_json(e: &ScriptEvent) -> Value {
    match e {
        ScriptEvent::Start => json!({"kind": "Start"}),
        ScriptEvent::Timer => json!({"kind": "Timer"}),
        ScriptEvent::Touch {
            avatar_id,
            avatar_name,
        } => {
            json!({"kind": "Touch", "avatar_id": avatar_id, "avatar_name": avatar_name})
        }
        ScriptEvent::Message {
            channel,
            text,
            sender,
        } => {
            json!({"kind": "Message", "channel": channel, "text": text, "sender": sender})
        }
        ScriptEvent::Hand {
            avatar_id,
            avatar_name,
            offset,
            precise,
        } => {
            json!({"kind": "Hand", "avatar_id": avatar_id, "avatar_name": avatar_name, "offset": offset, "precise": precise})
        }
    }
}

/// The host thread's main loop for the JavaScript engine.
pub(super) fn run(rx: Receiver<HostMsg>, tx: Sender<ScriptCommand>, engine: BunEngine) {
    let mut sup = match Supervisor::start(engine) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = %e, "could not start the script process; no script will run");
            // Keep draining, so the tick loop's sends never block or pile up.
            while let Ok(msg) = rx.recv() {
                if let HostMsg::Shutdown = msg {
                    break;
                }
            }
            return;
        }
    };
    while let Ok(msg) = rx.recv() {
        match msg {
            HostMsg::Load { prim_id, source } => {
                sup.blamed.remove(&prim_id);
                sup.sources.insert(prim_id, source.clone());
                sup.send(&json!({"op": "load", "prim_id": prim_id, "source": source}));
            }
            HostMsg::Unload { prim_id } => {
                sup.sources.remove(&prim_id);
                sup.sequences.remove(&prim_id);
                sup.send(&json!({"op": "unload", "prim_id": prim_id}));
            }
            HostMsg::Event { prim_id, event } => {
                sup.send(&json!({"op": "event", "prim_id": prim_id, "event": event_json(&event)}));
            }
            HostMsg::Tick { dt, views } => {
                for (id, view) in &views {
                    if let Some(p) = view.particles {
                        sup.sequences.insert(*id, p.sequence);
                    }
                }
                let views: Vec<Value> = views
                    .iter()
                    .map(|(id, v)| json!([id, view_json(v)]))
                    .collect();
                sup.send(&json!({"op": "tick", "dt": dt, "views": views}));
                let Some(line) = sup.answer(&tx) else {
                    continue;
                };
                let commands = match serde_json::from_str::<Value>(&line) {
                    Ok(Value::Object(mut reply)) => match reply.remove("commands") {
                        Some(Value::Array(c)) => c,
                        _ => Vec::new(),
                    },
                    _ => {
                        tracing::warn!(
                            "the script process answered a tick with something that is not JSON"
                        );
                        Vec::new()
                    }
                };
                for raw in commands {
                    let Value::Object(raw) = raw else { continue };
                    let prim_id = raw.get("prim_id").and_then(Value::as_i64).unwrap_or(0);
                    match sup.command(&raw) {
                        Ok(Some(cmd)) => {
                            if tx.send(cmd).is_err() {
                                return;
                            }
                        }
                        Ok(None) => {}
                        Err(message) => {
                            // The check is the simulator's, after the slice (see
                            // `rules`), so the script is stopped but the message
                            // has no line.
                            sup.send(
                                &json!({"op": "fault", "prim_id": prim_id, "message": message}),
                            );
                        }
                    }
                }
            }
            HostMsg::Shutdown => break,
        }
    }
    let _ = sup.process.child.kill();
    let _ = sup.process.child.wait();
    let _ = std::fs::remove_file(&sup.blame);
}

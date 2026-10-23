//! The whole conversation loop against a stand-in for the Realtime API on
//! loopback: greeting, hearing, a tool, a refused tool, an interruption, a
//! hang-up and the outcome line.

use super::*;
use crate::{realtime::MODEL_RATE, Config, Event, ToolHost, ToolOutput, ToolSpec, VoiceAgent};
use base64::Engine as _;
use std::sync::Mutex;
use tokio::net::TcpListener;

struct FakeTools {
    called: Mutex<Vec<String>>,
}

impl ToolHost for FakeTools {
    fn list(&self) -> crate::BoxFuture<'_, Result<Vec<ToolSpec>, String>> {
        Box::pin(async {
            Ok(["look_around", "walk_to", "build"]
                .iter()
                .map(|n| ToolSpec {
                    name: (*n).into(),
                    description: format!("{n} does things"),
                    parameters: json!({
                        "$schema": "http://json-schema.org/draft-07/schema#",
                        "title": "Args",
                        "type": "object",
                    }),
                })
                .collect())
        })
    }

    fn call(&self, name: String, _arguments: Value) -> crate::BoxFuture<'_, ToolOutput> {
        self.called.lock().unwrap().push(name);
        Box::pin(async {
            ToolOutput {
                text: "You are on the Grote Markt.".into(),
                image: None,
                error: false,
            }
        })
    }
}

fn tone(rate: u32, ms: u32) -> Vec<f32> {
    let n = (rate * ms / 1000) as usize;
    (0..n)
        .map(|i| 0.4 * (2.0 * std::f32::consts::PI * 300.0 * i as f32 / rate as f32).sin())
        .collect()
}

fn audio_delta(item: &str, ms: u32) -> Value {
    let pcm = to_pcm16(&tone(MODEL_RATE, ms));
    json!({
        "type": "response.output_audio.delta",
        "item_id": item,
        "delta": base64::engine::general_purpose::STANDARD.encode(pcm),
    })
}

fn call_item(call_id: &str, name: &str) -> Value {
    json!({
        "type": "response.output_item.done",
        "item": { "type": "function_call", "call_id": call_id, "name": name, "arguments": "{}" },
    })
}

/// The stand-in server's view of the conversation, for the assertions.
#[derive(Default, Debug)]
struct Seen {
    tools: Vec<String>,
    appends: usize,
    /// Milliseconds of caller audio received.
    appended_ms: usize,
    outputs: Vec<(String, String)>,
    truncated: Option<(String, u64)>,
    text_reply_asked: bool,
}

#[test]
fn a_whole_call_greets_hears_acts_refuses_is_interrupted_and_reports() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap();
    let listener = rt.block_on(TcpListener::bind("127.0.0.1:0")).unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(Seen::default()));
    let (step_tx, step_rx) = mpsc::channel::<&'static str>();
    let (go_tx, go_rx) = tokio::sync::mpsc::unbounded_channel::<&'static str>();

    // The server, scripted: it waits for the test to say when each step may
    // start, so the test can feed audio between them.
    let server_seen = Arc::clone(&seen);
    rt.spawn(async move {
        let mut go_rx = go_rx;
        let (stream, _) = listener.accept().await.unwrap();
        let ws = tokio_tungstenite::accept_async(stream).await.unwrap();
        let (mut tx, mut rx) = ws.split();
        let seen = server_seen;
        // Read until a message of `kind`, noting everything on the way.
        macro_rules! until {
            ($kind:expr) => {
                loop {
                    let Some(Ok(Message::Text(t))) = rx.next().await else {
                        panic!("closed waiting for {}", $kind);
                    };
                    let v: Value = serde_json::from_str(&t).unwrap();
                    let kind = v["type"].as_str().unwrap_or_default().to_string();
                    let mut s = seen.lock().unwrap();
                    match kind.as_str() {
                        "session.update" => {
                            s.tools = v["session"]["tools"]
                                .as_array()
                                .unwrap()
                                .iter()
                                .map(|t| t["name"].as_str().unwrap().to_string())
                                .collect();
                            assert!(v["session"]["tools"][0]["parameters"]["$schema"].is_null());
                        }
                        "input_audio_buffer.append" => {
                            s.appends += 1;
                            let b = base64::engine::general_purpose::STANDARD
                                .decode(v["audio"].as_str().unwrap())
                                .unwrap();
                            s.appended_ms += b.len() / 2 * 1000 / MODEL_RATE as usize;
                        }
                        "conversation.item.create"
                            if v["item"]["type"] == "function_call_output" =>
                        {
                            s.outputs.push((
                                v["item"]["call_id"].as_str().unwrap().into(),
                                v["item"]["output"].as_str().unwrap().into(),
                            ));
                        }
                        "conversation.item.truncate" => {
                            s.truncated = Some((
                                v["item_id"].as_str().unwrap().into(),
                                v["audio_end_ms"].as_u64().unwrap(),
                            ));
                        }
                        "response.create" if v["response"]["output_modalities"][0] == "text" => {
                            s.text_reply_asked = true;
                        }
                        _ => {}
                    }
                    drop(s);
                    if kind == $kind {
                        break v;
                    }
                }
            };
        }
        macro_rules! say {
            ($v:expr) => {
                tx.send(Message::Text($v.to_string().into())).await.unwrap()
            };
        }

        until!("session.update");
        say!(json!({"type": "session.updated"}));
        // It speaks first.
        until!("response.create");
        say!(json!({"type": "response.created"}));
        say!(audio_delta("greet", 600));
        say!(json!({"type": "response.done"}));
        step_tx.send("greeted").unwrap();

        // A tool the caller may ask for, in a reply of its own.
        assert_eq!(go_rx.recv().await, Some("look"));
        say!(json!({"type": "response.created"}));
        say!(call_item("c1", "look_around"));
        say!(json!({"type": "response.done"}));
        until!("response.create");
        say!(json!({"type": "response.created"}));
        say!(json!({"type": "response.done"}));
        step_tx.send("looked").unwrap();

        // A tool only the trusted may ask for, while somebody else speaks.
        assert_eq!(go_rx.recv().await, Some("walk"));
        say!(json!({"type": "response.created"}));
        say!(call_item("c2", "walk_to"));
        say!(json!({"type": "response.done"}));
        until!("response.create");
        say!(json!({"type": "response.created"}));
        // A long reply, talked over.
        say!(audio_delta("long", 2000));
        step_tx.send("talking").unwrap();
        assert_eq!(go_rx.recv().await, Some("interrupt"));
        say!(json!({"type": "input_audio_buffer.speech_started"}));
        until!("conversation.item.truncate");
        say!(json!({"type": "response.done"}));
        step_tx.send("cut").unwrap();

        // Goodbye.
        assert_eq!(go_rx.recv().await, Some("bye"));
        say!(json!({"type": "response.created"}));
        say!(audio_delta("bye", 200));
        say!(call_item("c3", HANG_UP));
        say!(json!({"type": "response.done"}));

        // The outcome, once the client says the call is over.
        until!("response.create");
        say!(json!({"type": "response.created"}));
        say!(json!({"type": "response.output_text.done", "text": " Anna can make Saturday. "}));
        say!(json!({"type": "response.done"}));
        let _ = rx.next().await;
    });

    let tools = Arc::new(FakeTools {
        called: Mutex::new(Vec::new()),
    });
    let (hear_tx, hear) = mpsc::channel();
    let (spoke_tx, spoke) = mpsc::channel::<bool>();
    let cfg = Config {
        url: format!("ws://127.0.0.1:{port}/v1/realtime"),
        key: "test".into(),
        voice: "marin".into(),
        instructions: "Be brief.".into(),
        read_only: vec!["look_around".into()],
        full: vec!["walk_to".into()],
        trusted: vec!["jeroen".into()],
        announce_speakers: false,
        images: true,
        speak_first: true,
        idle_hangup: Duration::from_secs(60),
        outcome: Some("One line for jeroen.".into()),
    };
    let agent = VoiceAgent::start(
        cfg,
        tools.clone(),
        hear,
        Box::new(move |frame: Vec<f32>| {
            assert_eq!(frame.len(), FRAME);
            let _ = spoke_tx.send(frame.iter().any(|s| s.abs() > 0.01));
        }),
    );
    agent.names(HashMap::from([(7, "jeroen".to_string()), (9, "anna".to_string())]));
    let wait = |what: &str| {
        let got = step_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        assert_eq!(got, what);
    };
    // Frames spoken until this many seconds pass, counting those with sound.
    let spoken_for = |secs: f32| {
        let until = Instant::now() + Duration::from_secs_f32(secs);
        let (mut all, mut loud) = (0, 0);
        while Instant::now() < until {
            if let Ok(l) = spoke.recv_timeout(Duration::from_millis(50)) {
                all += 1;
                loud += usize::from(l);
            }
        }
        (all, loud)
    };
    let feed = |who: u64, ms: u32| {
        for frame in tone(RATE, ms).chunks(FRAME) {
            hear_tx.send((who, frame.to_vec())).unwrap();
        }
    };

    wait("greeted");
    // The silence spoken while connecting.
    let _ = spoke.try_iter().count();
    // 600 ms of greeting comes out as ~30 frames with sound, paced.
    let (all, loud) = spoken_for(1.0);
    assert!((40..=60).contains(&all), "{all} frames in a second");
    assert!((25..=32).contains(&loud), "{loud} frames of greeting");

    feed(7, 200);
    std::thread::sleep(Duration::from_millis(100));
    go_tx.send("look").unwrap();
    wait("looked");
    assert_eq!(tools.called.lock().unwrap().as_slice(), ["look_around"]);

    // Anna speaks, then the model tries to walk for her.
    feed(9, 200);
    std::thread::sleep(Duration::from_millis(100));
    go_tx.send("walk").unwrap();
    wait("talking");
    let (_, loud) = spoken_for(0.3);
    assert!(loud > 5, "the long reply is playing");
    go_tx.send("interrupt").unwrap();
    wait("cut");
    let (_, loud) = spoken_for(0.3);
    assert!(loud <= 1, "{loud} frames of sound after the cut");

    go_tx.send("bye").unwrap();
    let until = Instant::now() + Duration::from_secs(5);
    let mut events = Vec::new();
    while Instant::now() < until && !events.contains(&Event::HangUp) {
        let _ = spoke.try_iter().count();
        events.extend(agent.events());
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(events.contains(&Event::HangUp), "{events:?}");
    agent.finish();
    let until = Instant::now() + Duration::from_secs(5);
    while Instant::now() < until && !events.contains(&Event::Closed) {
        events.extend(agent.events());
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        events.contains(&Event::Outcome("Anna can make Saturday.".into())),
        "{events:?}"
    );
    assert!(events.contains(&Event::Closed));

    let s = seen.lock().unwrap();
    assert_eq!(s.tools, ["look_around", "walk_to", "hang_up"], "build is not offered");
    // 400 ms fed, less the resampler's lag.
    assert!(s.appends >= 2, "{} appends", s.appends);
    assert!((380..=400).contains(&s.appended_ms), "{} ms heard", s.appended_ms);
    assert_eq!(s.outputs[0].0, "c1");
    assert!(s.outputs[0].1.contains("Grote Markt"));
    assert_eq!(s.outputs[1].0, "c2");
    assert!(s.outputs[1].1.starts_with("Refused"), "{}", s.outputs[1].1);
    assert_eq!(tools.called.lock().unwrap().len(), 1, "walk_to never ran");
    let (item, ms) = s.truncated.clone().unwrap();
    assert_eq!(item, "long");
    assert!((200..1500).contains(&ms), "cut after {ms} ms");
    assert!(s.text_reply_asked);
}

#[test]
fn a_call_with_nobody_trusted_offers_only_the_read_only_tools() {
    let cfg = Config {
        url: String::new(),
        key: String::new(),
        voice: String::new(),
        instructions: String::new(),
        read_only: vec!["look_around".into()],
        full: vec!["walk_to".into()],
        trusted: vec![],
        announce_speakers: false,
        images: false,
        speak_first: true,
        idle_hangup: Duration::from_secs(60),
        outcome: None,
    };
    let all = ["look_around", "walk_to", "hang_up"]
        .iter()
        .map(|n| ToolSpec {
            name: (*n).into(),
            description: String::new(),
            parameters: json!({}),
        })
        .collect();
    let names: Vec<String> = offered(&cfg, all).into_iter().map(|t| t.name).collect();
    assert_eq!(names, ["look_around", "hang_up"]);
}

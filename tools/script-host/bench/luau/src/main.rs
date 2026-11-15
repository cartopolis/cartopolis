//! The Luau host under the same scenarios as `../bench.ts`.
//!
//! `ScriptHost::tick` only posts a message to the host thread and returns, so a
//! tick's cost is read with a sentinel: after the tick, unload an empty sentinel
//! script and wait for its "stopped" notice. The channel is FIFO, so that notice
//! arrives exactly when the tick before it has finished.
use std::time::{Duration, Instant};

use cartopolis_core::{GeoAnchor, ScriptLevel};
use cartopolis_simulator::scripting::{PrimView, ScriptCommand, ScriptEvent, ScriptHost};

const SENTINEL: i64 = 1_000_000_000;
const CONTENT: [&str; 4] = ["beacon", "firebird", "football", "theremin"];

fn rss() -> f64 {
    let out = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .expect("ps");
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse::<f64>()
        .unwrap_or(0.0)
        * 1024.0
}

/// Everything that arrives until `want` says yes, or two seconds pass.
fn wait_for(
    host: &ScriptHost,
    sink: &mut Vec<ScriptCommand>,
    mut want: impl FnMut(&ScriptCommand) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        for c in host.drain() {
            let hit = want(&c);
            sink.push(c);
            if hit {
                return;
            }
        }
        assert!(Instant::now() < deadline, "the Luau host stopped answering");
        std::thread::yield_now();
    }
}

fn is_output(c: &ScriptCommand, prim: i64, needle: &str) -> bool {
    matches!(c, ScriptCommand::Output { prim_id, text, .. } if *prim_id == prim && text.contains(needle))
}

fn load_all(host: &ScriptHost, scripts: &[(i64, String)]) {
    let mut sink = Vec::new();
    for (id, src) in scripts {
        host.load(*id, src.clone());
    }
    let last = scripts.last().map(|(id, _)| *id).unwrap_or(0);
    wait_for(
        host,
        &mut sink,
        |c| matches!(c, ScriptCommand::Output { prim_id, level, .. } if *prim_id == last && *level != ScriptLevel::Info),
    );
    let errors: Vec<_> = sink
        .iter()
        .filter_map(|c| match c {
            ScriptCommand::Output {
                prim_id,
                level: ScriptLevel::Error,
                text,
            } => Some(format!("{prim_id}: {text}")),
            _ => None,
        })
        .collect();
    assert!(errors.is_empty(), "scripts failed to load: {errors:?}");
}

fn view(i: i64) -> PrimView {
    PrimView {
        name: format!("prim {i}"),
        geo: GeoAnchor {
            latitude: 53.2194,
            longitude: 6.5665,
        },
        alpha: 1.0,
        scale: glam::Vec3::ONE,
        color: [1.0; 3],
        ..Default::default()
    }
}

/// One tick, timed to its end; returns milliseconds and what it produced.
fn timed_tick(host: &ScriptHost, ids: &[i64]) -> (f64, Vec<ScriptCommand>) {
    let views: Vec<(i64, PrimView)> = ids.iter().map(|&i| (i, view(i))).collect();
    let t = Instant::now();
    host.tick(0.05, views);
    host.unload(SENTINEL);
    let mut sink = Vec::new();
    wait_for(host, &mut sink, |c| is_output(c, SENTINEL, "stopped"));
    let ms = t.elapsed().as_secs_f64() * 1000.0;
    host.load(SENTINEL, "function on_touch() end".into());
    wait_for(host, &mut sink, |c| is_output(c, SENTINEL, "compiled"));
    (ms, sink)
}

fn stats(mut ms: Vec<f64>) -> (f64, f64) {
    ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mean = ms.iter().sum::<f64>() / ms.len() as f64;
    let p99 = ms[((ms.len() as f64) * 0.99) as usize - 1];
    (mean, p99)
}

fn main() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../..");
    let scenarios: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.join("tools/script-host/bench/scenarios.json")).unwrap(),
    )
    .unwrap();
    let content: Vec<String> = CONTENT
        .iter()
        .map(|n| {
            std::fs::read_to_string(root.join(format!("crates/simulator/content/{n}.lua"))).unwrap()
        })
        .collect();

    // ── Memory per script ──
    for n in [200i64, 2000] {
        for (label, sources) in [
            (
                "idle",
                vec![scenarios["idle"]["lua"].as_str().unwrap().to_string()],
            ),
            ("content", content.clone()),
            // One pack each, as `../bench.ts` does: timers, no message storm.
            ("firebird", vec![content[1].clone()]),
            ("football", vec![content[2].clone()]),
        ] {
            let host = ScriptHost::spawn();
            let before = rss();
            let scripts: Vec<(i64, String)> = (1..=n)
                .map(|i| (i, sources[(i as usize) % sources.len()].clone()))
                .collect();
            let t = Instant::now();
            load_all(&host, &scripts);
            let load_ms = t.elapsed().as_secs_f64() * 1000.0;
            host.load(SENTINEL, "function on_touch() end".into());
            let ids: Vec<i64> = (1..=n).collect();
            let mut ms = Vec::new();
            for tick in 0..200 {
                if tick % 20 == 5 {
                    host.event(
                        1,
                        ScriptEvent::Touch {
                            avatar_id: 42,
                            avatar_name: "Anna".into(),
                        },
                    );
                }
                ms.push(timed_tick(&host, &ids).0);
            }
            let after = rss();
            let (mean, p99) = stats(ms);
            println!(
                "{}",
                serde_json::json!({"engine": "luau", "scenario": label, "scripts": n,
                    "bytes_per_script": ((after - before) / n as f64).round(),
                    "load_ms": load_ms.round(), "tick_mean_ms": (mean * 1000.0).round() / 1000.0, "tick_p99_ms": (p99 * 1000.0).round() / 1000.0})
            );
        }
    }

    // ── CPU: ten scripts that never stop, so the world's fuel bank is spent every tick ──
    for label in ["busy", "calls"] {
        let host = ScriptHost::spawn();
        let src = scenarios[label]["lua"].as_str().unwrap().to_string();
        let scripts: Vec<(i64, String)> = (1..=10).map(|i| (i, src.clone())).collect();
        load_all(&host, &scripts);
        host.load(SENTINEL, "function on_touch() end".into());
        let ids: Vec<i64> = (1..=10).collect();
        let mut ms = Vec::new();
        let mut most = 0u64;
        for _ in 0..100 {
            let (t, out) = timed_tick(&host, &ids);
            ms.push(t);
            for c in out {
                if let ScriptCommand::Output {
                    level: ScriptLevel::Info,
                    text,
                    ..
                } = c
                {
                    most = most.max(text.parse().unwrap_or(0));
                }
            }
        }
        let (mean, p99) = stats(ms);
        // `most` is one script's count after 100 ticks; all ten run alike.
        println!(
            "{}",
            serde_json::json!({"engine": "luau", "scenario": label, "scripts": 10,
                "tick_mean_ms": (mean * 1000.0).round() / 1000.0, "tick_p99_ms": (p99 * 1000.0).round() / 1000.0,
                "work_per_tick_all_scripts": (most as f64 / 100.0 * 10.0).round()})
        );
    }
}

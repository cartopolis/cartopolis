//! The JavaScript engine end to end: a real Bun process behind a real
//! [`ScriptHost`], fed ticks the way the tick loop feeds them.
//!
//! Needs Bun 1.4 or later. It is found as `CARTO_TEST_BUN`, then
//! `~/.cache/bun-1.4.4/bun` (where `tools/script-host/bench/run-on-node.sh`
//! puts it), then `bun` on the path; without one these tests say so and pass,
//! because a build node without Bun is not a broken engine. The engine's own
//! guarantees — the sandbox, fuel, memory — are tested in Bun itself
//! (`tools/script-host/test`); these test the seam.

use super::*;
use std::path::PathBuf;
use std::time::{Duration, Instant};

fn bun() -> Option<PathBuf> {
    let candidates = [
        std::env::var_os("CARTO_TEST_BUN").map(PathBuf::from),
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache/bun-1.4.4/bun")),
        Some(PathBuf::from("bun")),
    ];
    for path in candidates.into_iter().flatten() {
        let Ok(out) = std::process::Command::new(&path).arg("--version").output() else {
            continue;
        };
        let version = String::from_utf8_lossy(&out.stdout);
        let mut parts = version
            .trim()
            .split('.')
            .map(|p| p.parse::<u32>().unwrap_or(0));
        if (parts.next().unwrap_or(0), parts.next().unwrap_or(0)) >= (1, 4) {
            return Some(path);
        }
    }
    None
}

fn host_dir() -> PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/script-host")
}

/// A host on the Bun engine, or `None` (with a note) where there is no Bun 1.4
/// or the script host's packages are not installed.
fn host() -> Option<ScriptHost> {
    let Some(bun) = bun() else {
        eprintln!("skipped: no Bun 1.4 (set CARTO_TEST_BUN)");
        return None;
    };
    if !host_dir().join("node_modules/acorn").exists() {
        let status = std::process::Command::new(&bun)
            .args(["install", "--frozen-lockfile"])
            .current_dir(host_dir())
            .status();
        if !matches!(status, Ok(s) if s.success()) {
            eprintln!("skipped: `bun install` in tools/script-host failed");
            return None;
        }
    }
    Some(ScriptHost::spawn_with(Engine::Bun(BunEngine {
        bun,
        host_dir: host_dir(),
        tick_timeout: Duration::from_millis(1500),
        memory_limit_bytes: 1 << 30,
    })))
}

fn view(lat: f64, lng: f64) -> PrimView {
    PrimView {
        geo: GeoAnchor {
            latitude: lat,
            longitude: lng,
        },
        alpha: 1.0,
        scale: Vec3::ONE,
        ..Default::default()
    }
}

/// Tick until `done` says so or `limit` passes; everything that came out.
fn tick_until(
    host: &ScriptHost,
    ids: &[i64],
    limit: Duration,
    mut done: impl FnMut(&[ScriptCommand]) -> bool,
) -> Vec<ScriptCommand> {
    let start = Instant::now();
    let mut out = Vec::new();
    while start.elapsed() < limit {
        host.tick(
            0.05,
            ids.iter().map(|&i| (i, view(53.2194, 6.5665))).collect(),
        );
        std::thread::sleep(Duration::from_millis(20));
        out.extend(host.drain());
        if done(&out) {
            break;
        }
    }
    out
}

fn errors(out: &[ScriptCommand]) -> Vec<(i64, String)> {
    out.iter()
        .filter_map(|c| match c {
            ScriptCommand::Output {
                prim_id,
                level: ScriptLevel::Error,
                text,
            } => Some((*prim_id, text.clone())),
            _ => None,
        })
        .collect()
}

#[test]
fn the_content_packs_run_on_bun() {
    let Some(host) = host() else { return };
    host.load(1, include_str!("../../content/firebird.js").into());
    host.load(2, include_str!("../../content/football.js").into());
    let out = tick_until(&host, &[1, 2], Duration::from_secs(20), |out| {
        out.iter()
            .filter(|c| matches!(c, ScriptCommand::SetGeo { prim_id: 1, .. }))
            .count()
            >= 5
    });
    assert_eq!(errors(&out), vec![], "the packs should compile and run");
    let flights = out
        .iter()
        .filter(|c| matches!(c, ScriptCommand::SetGeo { prim_id: 1, .. }))
        .count();
    assert!(
        flights >= 5,
        "the firebird should fly, every tick: {flights} moves"
    );
    assert!(
        out.iter().any(|c| matches!(
            c,
            ScriptCommand::SetParticles {
                prim_id: 1,
                effect: Some(_)
            }
        )),
        "the firebird lights its flame through the shared particle rules"
    );
}

#[test]
fn a_bad_particle_rule_stops_the_script_with_the_luau_message() {
    let Some(host) = host() else { return };
    host.load(1, "function on_start() { particles({lifetime: 3}) }".into());
    let out = tick_until(&host, &[1], Duration::from_secs(10), |out| {
        !errors(out).is_empty()
    });
    let errs = errors(&out);
    assert_eq!(errs.len(), 1, "{out:?}");
    assert!(
        errs[0].1.contains("particle rule `lifetime`"),
        "{}",
        errs[0].1
    );
}

#[test]
fn a_hung_script_is_killed_blamed_and_the_rest_come_back() {
    let Some(host) = host() else { return };
    host.load(
        1,
        "let n = 0; function on_start() { set_timer_event(0.05) } function on_timer() { n++; say(String(n)) }".into(),
    );
    host.load(
        2,
        r#"function on_touch() { for (let i = 0; i < 10; i++) /^(a+)+$/.test("a".repeat(40) + "!") }"#.into(),
    );
    tick_until(&host, &[1, 2], Duration::from_secs(10), |out| {
        out.iter()
            .any(|c| matches!(c, ScriptCommand::Say { prim_id: 1, .. }))
    });
    host.event(
        2,
        ScriptEvent::Touch {
            avatar_id: 7,
            avatar_name: "ada".into(),
        },
    );
    let out = tick_until(&host, &[1, 2], Duration::from_secs(30), |out| {
        let killed = !errors(out).is_empty();
        let back = out
            .iter()
            .rev()
            .take_while(|c| {
                !matches!(
                    c,
                    ScriptCommand::Output {
                        level: ScriptLevel::Error,
                        ..
                    }
                )
            })
            .any(|c| matches!(c, ScriptCommand::Say { prim_id: 1, .. }));
        killed && back
    });
    for c in out.iter().filter(|c| matches!(c, ScriptCommand::Output { .. })) {
        eprintln!("DEBUG {c:?}");
    }
    let errs = errors(&out);
    assert_eq!(errs.len(), 1, "exactly the hung script is blamed: {errs:?}");
    assert_eq!(errs[0].0, 2);
    assert!(errs[0].1.contains("did not finish"), "{}", errs[0].1);
    let after = out
        .iter()
        .skip_while(|c| {
            !matches!(
                c,
                ScriptCommand::Output {
                    level: ScriptLevel::Error,
                    ..
                }
            )
        })
        .any(|c| matches!(c, ScriptCommand::Say { prim_id: 1, .. }));
    assert!(after, "the other script runs again in the new process");
}

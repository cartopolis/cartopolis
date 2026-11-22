//! The behaviour the Luau engine's tests guarded, held against the JavaScript
//! engine: a real Bun process behind a real [`ScriptHost`], driven by ticks.
//!
//! These are the author-facing promises — what a call does, what it refuses,
//! what a budget lets through — so they are asserted on the `ScriptCommand`s
//! that leave the host, which is all the tick loop ever sees. Like `bun_tests`,
//! every test needs Bun 1.4 (`bun_tests::host`).
//!
//! Deliberately not ported from the Luau suite:
//! - `runaway_loop_yields_and_stays_resumable` — Bun's `sandbox.test.ts` "a handler that never ends is paused each tick, and the others still run" (and `one_busy_script_does_not_starve_another` below, through the seam).
//! - `a_runaway_top_level_returns_instead_of_hanging_the_host` — `sandbox.test.ts` "an endless top level is refused at load, not left running".
//! - `a_handler_lookup_cannot_run_script_code` — Luau-only: `getfenv`/`__index` metamethods and mlua's `raw_get`; the JS rewrite finds handlers with `typeof` on bindings, and getters are refused (`sandbox.test.ts` "getters and setters").
//! - `memory_ceiling_faults_the_script_only` — `sandbox.test.ts` "a slice that allocates without end is stopped" and the audit test.
//! - `sandbox_denies_escape_routes` — `sandbox.test.ts` "the sandbox" (no `process`/`Bun`/`fetch`/`eval`/import), "one realm, scripts apart" (frozen intrinsics) and "replacing an API call breaks only the script that did it".
//! - `the_standard_libraries_an_author_is_promised_are_present` — Luau-only: the explicit Luau stdlib list (`math.random`, `os.time`, …); the realm's allowlist is `realm-runtime.js`'s `__keep`.
//! - `the_catalogue_matches_the_installed_api`'s Luau half (walking the Luau environment) — replaced by `the_catalogue_matches_the_installed_api` below, which compares `realm.ts`'s `API_NAMES` with `SCRIPT_API` both ways; `sandbox.test.ts` "every call in the catalogue … is there" checks each name is a function in the realm.

use super::*;
use cartopolis_core::tone::Voice;
use cartopolis_core::{ScriptLevel, SoundName, ToneSpec};
use std::time::{Duration, Instant};

use super::bun_tests::{errors, view};

/// A prim id no test uses, whose script marks the end of a run (see [`Bench::run`]).
const SENTINEL: i64 = 1_000_000;

/// A host and the prims it is ticked with, run in counted ticks.
///
/// `tick_until` waits for something to appear, which cannot assert that
/// something *did not*. This knows when a run is over: after the ticks it loads
/// a sentinel whose top level logs a fresh token, and ticks once more. The child
/// handles its input in order, so the token's line comes after every command of
/// the counted ticks (and every fault the simulator sent back for them), and
/// before anything of the extra tick. Whatever of the extra tick arrives is kept
/// for the next run, so nothing is lost — but the script clock has advanced one
/// tick more than was asked for.
struct Bench {
    host: ScriptHost,
    views: Vec<(i64, PrimView)>,
    dt: f32,
    runs: u32,
    carry: Vec<ScriptCommand>,
}

impl Bench {
    /// Wait for the Bun process to be up before any test script is loaded, so a
    /// slow first start (a busy node) is never mistaken for a hung test script.
    fn new(host: ScriptHost, ids: &[i64]) -> Self {
        let token = "settled 0";
        host.load(SENTINEL, format!("log({token:?})"));
        let start = Instant::now();
        'warm: loop {
            // Ticked again while waiting: a first tick that timed out restarted
            // the process, and the reloaded sentinel speaks only at a tick.
            host.tick(0.05, Vec::new());
            for _ in 0..20 {
                std::thread::sleep(Duration::from_millis(10));
                if host.drain().iter().any(|c| is_token(c, token)) {
                    break 'warm;
                }
            }
            assert!(
                start.elapsed() < Duration::from_secs(30),
                "the script process never started"
            );
        }
        Self {
            host,
            views: ids.iter().map(|&i| (i, view(53.2194, 6.5665))).collect(),
            dt: 0.05,
            runs: 0,
            carry: Vec::new(),
        }
    }

    /// Tick `ticks` times and return everything the test's prims emitted.
    fn run(&mut self, ticks: u32) -> Vec<ScriptCommand> {
        for _ in 0..ticks {
            self.host.tick(self.dt, self.views.clone());
        }
        self.runs += 1;
        let token = format!("settled {}", self.runs);
        self.host.load(SENTINEL, format!("log({token:?})"));
        self.host.tick(self.dt, self.views.clone());
        let mut out = std::mem::take(&mut self.carry);
        let start = Instant::now();
        loop {
            out.extend(self.host.drain());
            if let Some(i) = out.iter().position(|c| is_token(c, &token)) {
                self.carry = out.split_off(i + 1);
                out.retain(|c| c.prim_id() != SENTINEL);
                return out;
            }
            assert!(
                start.elapsed() < Duration::from_secs(20),
                "the script host never finished the run: {out:?}"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Set what `prim_id` is told about itself from the next tick on.
    fn set_view(&mut self, prim_id: i64, v: PrimView) {
        for (id, slot) in &mut self.views {
            if *id == prim_id {
                *slot = v.clone();
            }
        }
    }
}

fn is_token(c: &ScriptCommand, token: &str) -> bool {
    matches!(c, ScriptCommand::Output { prim_id: SENTINEL, text, .. } if text == token)
}

/// Info lines, in order, per prim.
fn logs(out: &[ScriptCommand], prim: i64) -> Vec<String> {
    out.iter()
        .filter_map(|c| match c {
            ScriptCommand::Output {
                prim_id,
                level: ScriptLevel::Info,
                text,
            } if *prim_id == prim => Some(text.clone()),
            _ => None,
        })
        .collect()
}

fn notices(out: &[ScriptCommand], prim: i64) -> Vec<String> {
    out.iter()
        .filter_map(|c| match c {
            ScriptCommand::Output {
                prim_id,
                level: ScriptLevel::Notice,
                text,
            } if *prim_id == prim => Some(text.clone()),
            _ => None,
        })
        .collect()
}

fn errors_of(out: &[ScriptCommand], prim: i64) -> Vec<String> {
    errors(out)
        .into_iter()
        .filter(|(id, _)| *id == prim)
        .map(|(_, t)| t)
        .collect()
}

fn said(out: &[ScriptCommand]) -> Vec<String> {
    out.iter()
        .filter_map(|c| match c {
            ScriptCommand::Say { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

fn tones(out: &[ScriptCommand]) -> Vec<Option<ToneSpec>> {
    out.iter()
        .filter_map(|c| match c {
            ScriptCommand::SetTone { tone, .. } => Some(*tone),
            _ => None,
        })
        .collect()
}

fn touch(id: u64) -> ScriptEvent {
    ScriptEvent::Touch {
        avatar_id: id,
        avatar_name: "ada".into(),
    }
}

fn hand(id: u64, name: &str, offset: [f32; 3]) -> ScriptEvent {
    ScriptEvent::Hand {
        avatar_id: id,
        avatar_name: name.into(),
        offset,
        precise: true,
    }
}

/// The smallest thing that has to work: a script compiles and its handler runs,
/// once.
#[test]
fn compiles_and_runs_a_handler() {
    let host = super::bun_tests::host();
    let mut b = Bench::new(host, &[1]);
    b.host.load(1, "function on_start() { log('hi') }".into());
    let out = b.run(3);
    assert_eq!(logs(&out, 1), vec!["hi"], "{out:?}");
    assert_eq!(errors(&out), vec![]);
}

/// A syntax error reaches the author as an Error line that says where, and
/// costs the host nothing: the next script loads and runs.
#[test]
fn syntax_error_is_reported_not_panicked() {
    let host = super::bun_tests::host();
    let mut b = Bench::new(host, &[1, 2]);
    b.host.load(1, "function on_start( {".into());
    b.host.load(2, "function on_start() { log('fine') }".into());
    let out = b.run(2);
    let errs = errors_of(&out, 1);
    assert_eq!(errs.len(), 1, "{out:?}");
    assert!(
        errs[0].contains("line 1"),
        "the error should point at the line so the editor can: {}",
        errs[0]
    );
    assert_eq!(logs(&out, 2), vec!["fine"]);
}

/// `on_touch` is told who touched it: their id and their name.
#[test]
fn touch_handler_receives_the_toucher() {
    let host = super::bun_tests::host();
    let mut b = Bench::new(host, &[1]);
    b.host.load(
        1,
        "function on_touch(id, name) { say(name + '/' + id) }".into(),
    );
    b.host.event(1, touch(7));
    assert_eq!(said(&b.run(3)), vec!["ada/7"]);
}

/// A runtime error stops that script with its own message, and it stays
/// stopped: a later event runs nothing.
#[test]
fn runtime_error_faults_with_a_message() {
    let host = super::bun_tests::host();
    let mut b = Bench::new(host, &[1]);
    b.host.load(
        1,
        "function on_start() { throw new Error('boom') } function on_touch() { log('alive') }"
            .into(),
    );
    let out = b.run(2);
    let errs = errors_of(&out, 1);
    assert!(
        errs.iter().any(|t| t.contains("boom")),
        "expected the script's own message, got {out:?}"
    );
    b.host.event(1, touch(1));
    let after = b.run(3);
    assert!(
        logs(&after, 1).is_empty(),
        "a faulted script ran: {after:?}"
    );
}

/// NaN must not reach a transform: a prim at an undefined position cannot be
/// recovered by a later write.
#[test]
fn non_finite_positions_are_refused() {
    let host = super::bun_tests::host();
    let mut b = Bench::new(host, &[1]);
    b.host
        .load(1, "function on_start() { set_pos(0/0, 0, 0) }".into());
    let out = b.run(2);
    assert_eq!(
        errors_of(&out, 1).len(),
        1,
        "a NaN position should be a script error: {out:?}"
    );
    assert!(!out
        .iter()
        .any(|c| matches!(c, ScriptCommand::SetPos { .. })));
}

/// The same rule for the scalar setters, where NaN once got through: a range
/// clamp propagates NaN, so it has to be refused before it, and a NaN colour
/// would be persisted and sent to every client's material. The valid range
/// still clamps.
#[test]
fn non_finite_appearance_values_are_refused() {
    let host = super::bun_tests::host();
    let calls = [
        "set_color(0/0, 0, 0)",
        "set_color(0, 1/0, 0)",
        "set_alpha(0/0)",
        "set_glow(0/0)",
    ];
    let mut b = Bench::new(host, &[1, 2, 3, 4, 5]);
    for (i, call) in calls.iter().enumerate() {
        b.host
            .load(i as i64 + 1, format!("function on_start() {{ {call} }}"));
    }
    b.host
        .load(5, "function on_start() { set_color(2, -1, 0.5) }".into());
    let out = b.run(2);
    for (i, call) in calls.iter().enumerate() {
        let prim = i as i64 + 1;
        assert_eq!(
            errors_of(&out, prim).len(),
            1,
            "`{call}` should be a script error: {out:?}"
        );
        assert!(
            !out.iter().any(|c| c.prim_id() == prim
                && matches!(
                    c,
                    ScriptCommand::SetColor { .. }
                        | ScriptCommand::SetAlpha { .. }
                        | ScriptCommand::SetGlow { .. }
                )),
            "`{call}` reached the world anyway: {out:?}"
        );
    }
    let color = out
        .iter()
        .find_map(|c| match c {
            ScriptCommand::SetColor { prim_id: 5, color } => Some(*color),
            _ => None,
        })
        .expect("a colour command");
    assert_eq!(color, [1.0, 0.0, 0.5]);
}

/// A logging loop gets the per-tick output budget — 64 commands — and the
/// rest are counted and reported, not delivered and not a fault.
#[test]
fn output_budget_caps_a_logging_loop() {
    let host = super::bun_tests::host();
    let mut b = Bench::new(host, &[1]);
    // 1000 iterations finish within one slice's fuel, so all of them land in
    // one tick's budget.
    b.host.load(
        1,
        "function on_start() { for (let i = 0; i < 1000; i++) log('spam') }".into(),
    );
    let out = b.run(2);
    assert_eq!(
        logs(&out, 1).len(),
        64,
        "per-tick output budget was not applied"
    );
    assert!(
        notices(&out, 1)
            .iter()
            .any(|t| t.contains("936 command(s) dropped")),
        "drops should be counted: {:?}",
        notices(&out, 1)
    );
    assert_eq!(errors(&out), vec![]);
}

/// The event queue drops the right things when it is full: never `Start` (a
/// script that loses it runs forever uninitialised, with no error anywhere),
/// and a flood of timers is one pending timer, not a full queue.
#[test]
fn a_flood_of_events_keeps_start_and_coalesces_timers() {
    let host = super::bun_tests::host();
    let src = "function on_start() { log('started') } \
               function on_timer() { log('timer') } \
               function on_touch(id) { log('touch ' + id) }";
    let mut b = Bench::new(host, &[1]);
    b.host.load(1, src.into());
    for _ in 0..128 {
        b.host.event(1, ScriptEvent::Timer);
    }
    let out = b.run(5);
    assert_eq!(
        logs(&out, 1),
        vec!["started", "timer"],
        "timers should collapse to one pending fire, after the Start"
    );

    // A flood of distinct events still evicts — the oldest — and still not Start.
    b.host.load(1, src.into());
    for i in 0..64 {
        b.host.event(1, touch(i));
    }
    let out = b.run(40);
    let lines = logs(&out, 1);
    assert_eq!(
        lines.first().map(String::as_str),
        Some("started"),
        "the Start event was evicted by a touch flood: {lines:?}"
    );
    let touches: Vec<&String> = lines.iter().filter(|l| l.starts_with("touch")).collect();
    assert_eq!(
        touches.len(),
        31,
        "the queue holds 32 events, Start among them: {lines:?}"
    );
    assert_eq!(
        touches[0], "touch 33",
        "the oldest touches are the ones dropped"
    );
}

/// A script asking for a faster timer than the tick gets the tick's 0.05 s, not
/// a busy loop. Ticked at 0.01 s so a clamped timer fires every fifth tick and
/// an unclamped one every tick.
#[test]
fn timer_interval_is_clamped_to_the_tick() {
    let host = super::bun_tests::host();
    let mut b = Bench::new(host, &[1]);
    b.dt = 0.01;
    b.host.load(
        1,
        "function on_start() { set_timer_event(0.001) } function on_timer() { log('fire') }".into(),
    );
    let fires = logs(&b.run(20), 1).len();
    assert!(
        (2..=5).contains(&fires),
        "20 ticks of 0.01 s fired the timer {fires} times; clamped to 0.05 s it is three or four"
    );
}

/// One script's endless loop is paused at its slice, and another still runs in
/// the same tick — and the supervisor does not take the paused loop for a hung
/// process.
#[test]
fn one_busy_script_does_not_starve_another() {
    let host = super::bun_tests::host();
    let mut b = Bench::new(host, &[1, 2]);
    b.host
        .load(1, "function on_start() { while (true) {} }".into());
    b.host.load(2, "function on_start() { log('ran') }".into());
    let out = b.run(5);
    assert_eq!(
        logs(&out, 2),
        vec!["ran"],
        "the quiet script was starved: {out:?}"
    );
    assert_eq!(
        errors(&out),
        vec![],
        "running out of fuel is scheduling, not a fault"
    );
}

/// Storing a script is a restart: the old instance and its state are gone.
#[test]
fn reload_replaces_the_instance() {
    let host = super::bun_tests::host();
    let src = "let counter = 0; function on_touch() { counter++; log(String(counter)) }";
    let mut b = Bench::new(host, &[1]);
    b.host.load(1, src.into());
    b.host.event(1, touch(1));
    let mut out = b.run(2);
    b.host.load(1, src.into());
    b.host.event(1, touch(1));
    out.extend(b.run(3));
    assert_eq!(
        logs(&out, 1),
        vec!["1", "1"],
        "the reloaded script kept state from the old instance"
    );
}

/// The installed API and the editor's reference are the same set of names.
///
/// `SCRIPT_API` is what an author reads in the panel; `realm.ts`'s `API_NAMES`
/// is what a script is handed. A name in one and not the other is a function
/// nobody can find or a documented call that does not exist. Needs no Bun.
#[test]
fn the_catalogue_matches_the_installed_api() {
    use std::collections::BTreeSet;
    let realm = include_str!("../../../../tools/script-host/src/realm.ts");
    let list = realm
        .split("API_NAMES = [")
        .nth(1)
        .and_then(|rest| rest.split(']').next())
        .expect("realm.ts should declare API_NAMES as an array");
    let installed: BTreeSet<&str> = list.split('\'').skip(1).step_by(2).collect();
    let catalogued: BTreeSet<&str> = cartopolis_core::SCRIPT_API.iter().map(|f| f.name).collect();
    let undocumented: Vec<_> = installed.difference(&catalogued).collect();
    let missing: Vec<_> = catalogued.difference(&installed).collect();
    assert!(
        undocumented.is_empty(),
        "installed but not in SCRIPT_API (the editor will not show these): {undocumented:?}"
    );
    assert!(
        missing.is_empty(),
        "in SCRIPT_API but not installed (the reference is lying): {missing:?}"
    );
}

/// Authors write the degrees the edit dialog shows them; the wire is radians,
/// and what a script reads back is degrees again.
#[test]
fn rotation_is_degrees_in_and_radians_out() {
    let host = super::bun_tests::host();
    let mut b = Bench::new(host, &[1, 2]);
    b.set_view(
        2,
        PrimView {
            rot: Vec3::new(0.0, std::f32::consts::PI, 0.0),
            ..view(53.2194, 6.5665)
        },
    );
    b.host
        .load(1, "function on_start() { set_rot(0, 180, 0) }".into());
    b.host.load(
        2,
        "function on_start() { const [, y] = get_rot(); log(y.toFixed(1)) }".into(),
    );
    let out = b.run(2);
    let rot = out
        .iter()
        .find_map(|c| match c {
            ScriptCommand::SetRot { rot, .. } => Some(*rot),
            _ => None,
        })
        .expect("a rotation command");
    assert!(
        (rot.y - std::f32::consts::PI).abs() < 1e-5,
        "180 degrees should reach the wire as pi radians, got {rot:?}"
    );
    assert_eq!(logs(&out, 2), vec!["180.0"]);
}

/// Each speaking verb carries its own radius, so the tick loop can deliver by
/// distance without knowing which one was called.
#[test]
fn each_chat_verb_has_its_own_radius() {
    let host = super::bun_tests::host();
    let mut b = Bench::new(host, &[1]);
    b.host.load(
        1,
        "function on_start() { whisper('a'); say('b'); shout('c') }".into(),
    );
    let radii: Vec<f32> = b
        .run(2)
        .iter()
        .filter_map(|c| match c {
            ScriptCommand::Say { radius, .. } => Some(*radius),
            _ => None,
        })
        .collect();
    assert_eq!(
        radii,
        vec![10.0, crate::chat::SAY_RADIUS_M, crate::chat::SHOUT_RADIUS_M]
    );
}

/// `sleep` suspends the handler and lets the rest of the tick happen, then
/// finishes it once the script clock has passed — the descheduling an overrun
/// gets, but asked for.
#[test]
fn sleep_suspends_the_handler_and_resumes_it() {
    let host = super::bun_tests::host();
    let mut b = Bench::new(host, &[1]);
    b.host.load(
        1,
        "function on_start() { log('before'); sleep(0.2); log('after') }".into(),
    );
    // Ticks 1–2 (0.1 s): it ran up to the sleep and stopped there.
    assert_eq!(logs(&b.run(1), 1), vec!["before"]);
    // Ticks 3–4 (0.2 s since it began at 0.05 s, so not yet).
    assert_eq!(logs(&b.run(2), 1), Vec::<String>::new(), "it woke early");
    // By tick 9 the clock is well past.
    assert_eq!(logs(&b.run(4), 1), vec!["after"]);
}

/// The clock `get_time` reads is simulated time, advanced for every script by
/// the scheduler — including ones that had nothing to run.
#[test]
fn the_script_clock_advances_with_the_tick() {
    let host = super::bun_tests::host();
    let mut b = Bench::new(host, &[1]);
    b.host.load(
        1,
        "function on_touch() { log(get_time().toFixed(2)) }".into(),
    );
    b.run(3); // four idle ticks, counting the run's closing one
    b.host.event(1, touch(1));
    assert_eq!(logs(&b.run(1), 1), vec!["0.25"]);
}

/// A script ends its own prim through a command for the tick loop, not as a
/// direct delete.
#[test]
fn die_is_a_command_not_a_side_effect() {
    let host = super::bun_tests::host();
    let mut b = Bench::new(host, &[1]);
    b.host.load(1, "function on_touch() { die() }".into());
    b.host.event(1, touch(1));
    let out = b.run(2);
    assert!(
        out.iter()
            .any(|c| matches!(c, ScriptCommand::Die { prim_id: 1 })),
        "expected a Die for prim 1, got {out:?}"
    );
}

// ---------------------------------------------------------------------------
// Script-to-script messaging
// ---------------------------------------------------------------------------

/// Load a pair of scripts as prims 1 and 2 and run the real scheduler over them:
/// who hears a message, and when, is decided there and nowhere else.
fn talk(host: ScriptHost, a: &str, b: &str, ticks: u32) -> Vec<ScriptCommand> {
    let mut bench = Bench::new(host, &[1, 2]);
    bench.host.load(1, a.into());
    bench.host.load(2, b.into());
    bench.run(ticks)
}

/// A message reaches a listener, and carries the channel and the sender's id.
#[test]
fn a_script_can_talk_to_another_script() {
    let host = super::bun_tests::host();
    let out = talk(
        host,
        "function on_start() { send(7, 'ping') }",
        "function on_start() { listen(7) } \
         function on_message(c, t, from) { log(t + ' ' + c + ' ' + from) }",
        6,
    );
    assert_eq!(logs(&out, 2), vec!["ping 7 1"], "{out:?}");
}

/// A script does not hear itself: listening on the channel you send on is the
/// ordinary shape, and hearing yourself turns it into a feedback loop.
#[test]
fn a_script_does_not_hear_its_own_message() {
    let host = super::bun_tests::host();
    let out = talk(
        host,
        "function on_start() { listen(7); send(7, 'hello') } \
         function on_message() { log('heard myself') }",
        "function on_start() {}",
        6,
    );
    assert!(logs(&out, 1).is_empty(), "{out:?}");
}

/// Listening is opt-in, or one chatty prim would cost every other one work.
#[test]
fn a_script_hears_nothing_until_it_listens() {
    let host = super::bun_tests::host();
    let out = talk(
        host,
        "function on_start() { send(7, 'ping') }",
        "function on_start() {} function on_message() { log('heard') }",
        6,
    );
    assert!(logs(&out, 2).is_empty(), "{out:?}");
}

/// A listener on 7 does not hear 8.
#[test]
fn channels_are_separate() {
    let host = super::bun_tests::host();
    let out = talk(
        host,
        "function on_start() { send(8, 'ping') }",
        "function on_start() { listen(7) } function on_message() { log('heard') }",
        6,
    );
    assert!(logs(&out, 2).is_empty(), "{out:?}");
}

/// `listen(channel, false)` stops it again.
#[test]
fn listening_can_be_stopped() {
    let host = super::bun_tests::host();
    let out = talk(
        host,
        "function on_start() { set_timer_event(0.05) } function on_timer() { send(7, 'ping') }",
        "function on_start() { listen(7); listen(7, false) } \
         function on_message() { log('heard') }",
        10,
    );
    assert!(logs(&out, 2).is_empty(), "{out:?}");
}

/// A script cannot flood the world: eight sends a tick get through, and the rest
/// are reported as dropped rather than faulting the script mid-handler.
#[test]
fn the_send_budget_is_bounded() {
    let host = super::bun_tests::host();
    let out = talk(
        host,
        "function on_start() { for (let i = 0; i < 100; i++) send(7, 'ping') }",
        "function on_start() { listen(7) } function on_message() { log('heard') }",
        12,
    );
    assert_eq!(
        logs(&out, 2).len(),
        8,
        "the send budget is 8 a tick: {out:?}"
    );
    assert_eq!(errors(&out), vec![]);
    assert!(
        notices(&out, 1)
            .iter()
            .any(|t| t.contains("92 command(s) dropped")),
        "{:?}",
        notices(&out, 1)
    );
}

// ---------------------------------------------------------------------------
// Particles, sounds, tones
// ---------------------------------------------------------------------------

/// **One vocabulary for an emitter**, whether a script or an HTTP body sets it.
///
/// The script side's list is read out of the refusal the simulator itself gives
/// for an unknown rule, and the admin side's out of `ParticleSpec`'s own
/// `Serialize`, so neither is a second hand-written list. `burst` is which
/// function was called and `sequence` is derived from the prim, so those two
/// are the admin API's alone.
#[test]
fn the_two_authoring_surfaces_use_one_vocabulary() {
    use std::collections::BTreeSet;
    let host = super::bun_tests::host();
    let http: BTreeSet<String> =
        match serde_json::to_value(crate::authoring::ParticleSpec::default()).expect("serialise") {
            serde_json::Value::Object(map) => map.keys().cloned().collect(),
            other => panic!("ParticleSpec should serialise to an object, got {other:?}"),
        };

    let mut b = Bench::new(host, &[1]);
    b.host.load(
        1,
        "function on_start() { particles({definitely_not_a_field: 1}) }".into(),
    );
    let out = b.run(2);
    let errs = errors_of(&out, 1);
    let text = errs
        .first()
        .unwrap_or_else(|| panic!("an unknown rule must be refused: {out:?}"));
    let script: BTreeSet<String> = text
        .split("is not one of: ")
        .nth(1)
        .expect("the refusal should list the valid keys")
        .lines()
        .next()
        .unwrap_or_default()
        .split(", ")
        .map(|k| k.trim().to_string())
        .collect();

    let only_http: BTreeSet<String> = http.difference(&script).cloned().collect();
    assert_eq!(
        only_http,
        ["burst".to_string(), "sequence".to_string()]
            .into_iter()
            .collect::<BTreeSet<_>>(),
        "the admin API has fields a script does not, and these are the two that are meant to be"
    );
    let only_script: BTreeSet<&String> = script.difference(&http).collect();
    assert!(
        only_script.is_empty(),
        "a script can set {only_script:?}, which the admin API has no name for"
    );
}

/// The six renamed particle rules answer with their new name rather than the
/// whole list: a script in the old spelling is somebody's real work.
#[test]
fn an_old_particle_rule_name_says_what_it_is_called_now() {
    let host = super::bun_tests::host();
    let renamed = [
        ("spread", "spread_deg"),
        ("radius", "source_radius"),
        ("size", "size_start"),
        ("end_size", "size_end"),
        ("color", "color_start"),
        ("end_color", "color_end"),
    ];
    let ids: Vec<i64> = (1..=renamed.len() as i64).collect();
    let mut b = Bench::new(host, &ids);
    for (id, (old, _)) in ids.iter().zip(renamed) {
        b.host.load(
            *id,
            format!("function on_start() {{ particles({{{old}: 1}}) }}"),
        );
    }
    let out = b.run(2);
    for (id, (old, now)) in ids.iter().zip(renamed) {
        let errs = errors_of(&out, *id);
        assert!(
            errs.iter().any(|t| t.contains(now)),
            "`{old}` should point at `{now}`, said: {errs:?}"
        );
    }
}

/// `play_sound` emits a one-shot with the defaults filled in, and the numbers
/// an author passes are clamped rather than trusted.
#[test]
fn play_sound_emits_a_clamped_one_shot() {
    use cartopolis_core::{DEFAULT_SOUND_RANGE_M, MAX_SOUND_RANGE_M};
    let host = super::bun_tests::host();
    let mut b = Bench::new(host, &[1]);
    b.host.load(
        1,
        "function on_start() { play_sound('firework'); play_sound('bell', 4, 9000) }".into(),
    );
    let sounds: Vec<_> = b
        .run(2)
        .into_iter()
        .filter_map(|c| match c {
            ScriptCommand::PlaySound { prim_id, sound } => Some((prim_id, sound)),
            _ => None,
        })
        .collect();
    assert_eq!(sounds.len(), 2, "{sounds:?}");
    assert_eq!(sounds[0].0, 1);
    assert_eq!(sounds[0].1.sound, SoundName::Firework);
    assert_eq!(sounds[0].1.volume, 1.0);
    assert_eq!(sounds[0].1.range_m, DEFAULT_SOUND_RANGE_M);
    assert_eq!(sounds[1].1.sound, SoundName::Bell);
    assert_eq!(sounds[1].1.volume, 1.0, "volume is clamped to 1");
    assert_eq!(sounds[1].1.range_m, MAX_SOUND_RANGE_M, "range is clamped");
}

/// An unknown sound is an error naming the library — never a silent no-op,
/// which an author cannot tell from a speaker that is off.
#[test]
fn an_unknown_sound_is_an_error_naming_the_library() {
    let host = super::bun_tests::host();
    let mut b = Bench::new(host, &[1]);
    b.host
        .load(1, "function on_start() { play_sound('fireworks') }".into());
    let out = b.run(2);
    let errs = errors_of(&out, 1);
    let error = errs
        .first()
        .unwrap_or_else(|| panic!("expected an error, got {out:?}"));
    assert!(error.contains("fireworks"), "{error}");
    assert!(error.contains("firework, crackle"), "{error}");
    assert!(!out
        .iter()
        .any(|c| matches!(c, ScriptCommand::PlaySound { .. })));
}

/// A sound in a tight loop is capped at two a tick and counted as dropped, not
/// faulted: a busy script, not a broken one.
#[test]
fn play_sound_is_capped_per_tick() {
    let host = super::bun_tests::host();
    let mut b = Bench::new(host, &[1]);
    b.host.load(
        1,
        "function on_start() { for (let i = 0; i < 10; i++) play_sound('pop') }".into(),
    );
    let out = b.run(2);
    let played = out
        .iter()
        .filter(|c| matches!(c, ScriptCommand::PlaySound { .. }))
        .count();
    assert_eq!(played, 2);
    assert_eq!(errors(&out), vec![]);
    assert!(
        notices(&out, 1)
            .iter()
            .any(|t| t.contains("8 command(s) dropped")),
        "{:?}",
        notices(&out, 1)
    );
}

/// `loop_sound` sets the prim's loop and `stop_sound` clears it.
#[test]
fn loop_and_stop_set_the_prims_loop() {
    let host = super::bun_tests::host();
    let mut b = Bench::new(host, &[1]);
    b.host.load(
        1,
        "function on_start() { loop_sound('whoosh', 0.5, 20); stop_sound() }".into(),
    );
    let loops: Vec<_> = b
        .run(2)
        .into_iter()
        .filter_map(|c| match c {
            ScriptCommand::SetSoundLoop { sound, .. } => Some(sound),
            _ => None,
        })
        .collect();
    assert_eq!(loops.len(), 2, "{loops:?}");
    let first = loops[0].expect("a loop");
    assert_eq!(first.sound, SoundName::Whoosh);
    assert_eq!(first.volume, 0.5);
    assert_eq!(first.range_m, 20.0);
    assert_eq!(loops[1], None);
}

/// Pitches are hertz or a note as a musician writes it, everything else has a
/// default, and `stop_tone` silences.
#[test]
fn set_tone_reads_notes_and_fills_defaults() {
    let host = super::bun_tests::host();
    let mut b = Bench::new(host, &[1]);
    b.host.load(
        1,
        "function on_start() { \
           set_tone({pitch: 'A3'}); \
           set_tone({pitch: 880, gain: 0.2, voice: 'square', glide: 0.2, vibrato: 0.5, range: 40}); \
           stop_tone() }"
            .into(),
    );
    let set = tones(&b.run(2));
    assert_eq!(set.len(), 3, "{set:?}");
    let first = set[0].expect("a tone");
    assert!((first.pitch_hz - 220.0).abs() < 0.01, "{}", first.pitch_hz);
    assert_eq!(first.gain, ToneSpec::default().gain);
    let second = set[1].expect("a tone");
    assert_eq!(second.voice, Voice::Square);
    assert_eq!(
        (
            second.pitch_hz,
            second.gain,
            second.glide_s,
            second.vibrato,
            second.range_m
        ),
        (880.0, 0.2, 0.2, 0.5, 40.0)
    );
    assert_eq!(set[2], None);
}

/// A typo is an error that says what was meant, never a tone that quietly
/// ignores it — which an author would only find by ear.
#[test]
fn set_tone_typos_are_errors() {
    let host = super::bun_tests::host();
    let cases = [
        ("set_tone({pitch: 440, vibratto: 1})", "vibratto"),
        ("set_tone({pitch: 440, voice: 'kazoo'})", "sine, organ"),
        ("set_tone({pitch: 'H2'})", "H2"),
        ("set_tone({gain: 1})", "pitch"),
    ];
    let ids: Vec<i64> = (1..=cases.len() as i64).collect();
    let mut b = Bench::new(host, &ids);
    for (id, (source, _)) in ids.iter().zip(cases) {
        b.host
            .load(*id, format!("function on_start() {{ {source} }}"));
    }
    let out = b.run(2);
    for (id, (source, expect)) in ids.iter().zip(cases) {
        let errs = errors_of(&out, *id);
        let error = errs
            .first()
            .unwrap_or_else(|| panic!("{source}: expected an error, got {out:?}"));
        assert!(error.contains(expect), "{source}: {error}");
        assert!(
            !out.iter()
                .any(|c| c.prim_id() == *id && matches!(c, ScriptCommand::SetTone { .. })),
            "{source} sounded anyway"
        );
    }
}

// ---------------------------------------------------------------------------
// Hands
// ---------------------------------------------------------------------------

/// `sense_hands` sets the range (clamped), and `on_hand` is told whose hand it
/// is and where.
#[test]
fn a_hand_reaches_the_script_with_whose_it_is() {
    let host = super::bun_tests::host();
    let mut b = Bench::new(host, &[1]);
    b.host.load(
        1,
        "function on_start() { sense_hands(2); sense_hands(50) } \
         function on_hand(id, name, x, y, z, precise) { \
           say(name + '/' + id + ' ' + x + ' ' + y + ' ' + z + ' ' + precise) }"
            .into(),
    );
    let ranges: Vec<f32> = b
        .run(2)
        .into_iter()
        .filter_map(|c| match c {
            ScriptCommand::SenseHands { range, .. } => Some(range),
            _ => None,
        })
        .collect();
    assert_eq!(ranges, vec![2.0, cartopolis_core::tone::MAX_HAND_RANGE_M]);
    b.host.event(1, hand(7, "ada", [0.5, 0.25, -1.0]));
    assert_eq!(said(&b.run(2)), vec!["ada/7 0.5 0.25 -1 true"]);
}

/// A hand is a level: a newer one replaces the same person's still waiting, so
/// a slow handler is handed where the hand is now — and one person's hand never
/// replaces another's.
#[test]
fn hands_waiting_for_the_script_coalesce_per_person() {
    let host = super::bun_tests::host();
    let mut b = Bench::new(host, &[1]);
    b.host
        .load(1, "function on_hand(id, name, x) { say(name + x) }".into());
    for x in [1.0, 2.0, 3.0] {
        b.host.event(1, hand(1, "a", [x, 0.0, 0.0]));
        b.host.event(1, hand(2, "b", [x * 10.0, 0.0, 0.0]));
    }
    assert_eq!(said(&b.run(5)), vec!["a3", "b30"]);
}

/// The theremin pack's script, end to end: the first hand plays and a second
/// is ignored; the hand starts on the middle note, moving it toward the rod
/// raises the note, past the top the range follows it, and lowering it fades
/// the note out; and a player whose hand stops arriving is let go of.
#[test]
fn the_theremin_script_turns_a_hand_into_a_tone() {
    let host = super::bun_tests::host();
    let mut b = Bench::new(host, &[1]);
    b.host
        .load(1, include_str!("../../content/theremin.js").into());
    let started = b.run(2);
    assert!(
        started
            .iter()
            .any(|c| matches!(c, ScriptCommand::SenseHands { range, .. } if *range > 1.0)),
        "{started:?}"
    );

    let play = |b: &mut Bench, id: u64, at: [f32; 3]| {
        b.host
            .event(1, hand(id, if id == 7 { "ada" } else { "bob" }, at));
        let out = b.run(2);
        assert_eq!(errors(&out), vec![]);
        let set = tones(&out);
        assert_eq!(set.len(), 1, "{set:?}");
        set[0].expect("a tone")
    };
    let near = |hz: f32, want: f32| (hz - want).abs() < 1.0;

    // Ada's first hand, metres from the cabinet as tracking may put it, is
    // the middle note at full volume; Bob arriving after her is ignored.
    let first = play(&mut b, 7, [1.5, 0.3, 2.0]);
    assert!(near(first.pitch_hz, 370.0), "{}", first.pitch_hz);
    assert!(first.gain > 0.99, "{}", first.gain);
    b.host.event(1, hand(8, "bob", [0.0, 0.0, 0.0]));
    assert_eq!(tones(&b.run(2)), vec![], "only the player's hand plays");

    // 30 cm toward the rod is the top; 30 cm more is still the top, and from
    // there 30 cm back is the middle again.
    assert!(near(play(&mut b, 7, [1.8, 0.3, 2.0]).pitch_hz, 1046.5));
    assert!(near(play(&mut b, 7, [2.1, 0.3, 2.0]).pitch_hz, 1046.5));
    assert!(near(play(&mut b, 7, [1.8, 0.3, 2.0]).pitch_hz, 370.0));

    // Lowered 30 cm: silent.
    assert!(play(&mut b, 7, [1.8, 0.0, 2.0]).gain < 0.01);

    // Ada's hand stops arriving: within a few seconds a timer lets her go...
    b.dt = 0.5;
    assert_eq!(tones(&b.run(6)), vec![None]);
    // ...and Bob plays, from the middle of his own range.
    b.dt = 0.05;
    let bob = play(&mut b, 8, [0.35, 0.45, 0.0]);
    assert!(near(bob.pitch_hz, 370.0), "{}", bob.pitch_hz);
}

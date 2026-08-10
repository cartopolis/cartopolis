//! The capacity model's guarantees, asserted against the real VM.
//!
//! These are the claims the engine was chosen for — a sandbox a script cannot
//! write through, a memory ceiling it cannot exceed, and a runaway loop that is
//! *suspended* rather than killed. Each is a property of Luau's configuration
//! that a version bump could silently take away, which is exactly what a test is
//! for.

use super::*;

/// Compile a slot and run it to completion (or until it stops making progress),
/// returning everything it emitted. Bounded so a test can never hang.
fn run_to_quiet(slot: &mut Slot, max_ticks: u32) -> Vec<ScriptCommand> {
    let mut out = Vec::new();
    for _ in 0..max_ticks {
        if slot.faulted || (slot.running.is_none() && slot.pending.is_empty()) {
            break;
        }
        slot.run_slice(SCRIPT_FUEL_SLICE);
        out.append(&mut slot.shared.out.borrow_mut());
    }
    out.append(&mut slot.shared.out.borrow_mut());
    out
}

fn outputs(cmds: &[ScriptCommand]) -> Vec<(ScriptLevel, String)> {
    cmds.iter()
        .filter_map(|c| match c {
            ScriptCommand::Output { level, text, .. } => Some((*level, text.clone())),
            _ => None,
        })
        .collect()
}

#[test]
fn compiles_and_runs_a_handler() {
    let mut slot = Slot::compile(1, "function on_start() vf.log('hi') end").expect("compile");
    slot.queue(ScriptEvent::Start);
    let out = run_to_quiet(&mut slot, 10);
    assert_eq!(
        outputs(&out),
        vec![(ScriptLevel::Info, "hi".to_string())],
        "on_start should have logged once"
    );
}

#[test]
fn syntax_error_is_reported_not_panicked() {
    let Err(err) = Slot::compile(1, "function on_start( end") else {
        panic!("a syntax error should not compile");
    };
    assert!(
        err.contains("prim1"),
        "the error should name the chunk so the editor can point at it: {err}"
    );
}

#[test]
fn touch_handler_receives_the_toucher() {
    let mut slot = Slot::compile(
        1,
        "function on_touch(id, name) vf.say(name .. '/' .. id) end",
    )
    .expect("compile");
    slot.queue(ScriptEvent::Touch {
        avatar_id: 7,
        avatar_name: "ada".into(),
    });
    let out = run_to_quiet(&mut slot, 10);
    let said: Vec<&String> = out
        .iter()
        .filter_map(|c| match c {
            ScriptCommand::Say { text, .. } => Some(text),
            _ => None,
        })
        .collect();
    assert_eq!(said, vec!["ada/7"]);
}

/// The load-bearing one: an infinite loop must be *descheduled*, keep its place,
/// and still be resumable — not aborted, and not a hung host thread.
#[test]
fn runaway_loop_yields_and_stays_resumable() {
    let mut slot = Slot::compile(1, "function on_start() while true do end end").expect("compile");
    slot.queue(ScriptEvent::Start);

    let spent = slot.run_slice(SCRIPT_FUEL_SLICE);
    assert!(
        spent > 0,
        "the slice should have consumed fuel, spent {spent}"
    );
    assert!(
        !slot.faulted,
        "running out of fuel is scheduling, not a fault"
    );
    assert!(
        slot.running.is_some(),
        "the handler should still be suspended and resumable"
    );

    // And it keeps being resumable, tick after tick, without ever finishing.
    for _ in 0..5 {
        slot.run_slice(SCRIPT_FUEL_SLICE);
        assert!(slot.running.is_some() && !slot.faulted);
    }
}

/// A script that exceeds its byte ceiling faults with a memory error; the host
/// survives and every other script is unaffected.
#[test]
fn memory_ceiling_faults_the_script_only() {
    let mut hog = Slot::compile(
        1,
        "function on_start() local t = {} for i = 1, 1e9 do t[i] = tostring(i) end end",
    )
    .expect("compile");
    hog.queue(ScriptEvent::Start);
    // Give it many slices: it must fault on memory, not simply run out of fuel.
    for _ in 0..500 {
        if hog.faulted {
            break;
        }
        hog.run_slice(SCRIPT_FUEL_SLICE);
    }
    assert!(hog.faulted, "the allocation should have been refused");
    let errs = outputs(&hog.shared.out.borrow());
    assert!(
        errs.iter()
            .any(|(l, t)| *l == ScriptLevel::Error && t.to_lowercase().contains("memory")),
        "expected a memory error, got {errs:?}"
    );

    let mut neighbour =
        Slot::compile(2, "function on_start() vf.log('fine') end").expect("compile");
    neighbour.queue(ScriptEvent::Start);
    let out = run_to_quiet(&mut neighbour, 10);
    assert_eq!(outputs(&out), vec![(ScriptLevel::Info, "fine".to_string())]);
}

/// A script must not be able to reach another prim, the host, or the filesystem.
#[test]
fn sandbox_denies_escape_routes() {
    // Neither Luau nor this host provides these; the point is that they are
    // *absent*, so a script referencing one gets nil rather than a capability.
    for expr in ["io", "require", "loadstring", "dofile", "package"] {
        let src = format!("function on_start() if {expr} ~= nil then vf.log('LEAK') end end");
        let mut slot = Slot::compile(1, &src).expect("compile");
        slot.queue(ScriptEvent::Start);
        let out = run_to_quiet(&mut slot, 10);
        assert!(
            outputs(&out).is_empty(),
            "`{expr}` is reachable from a script"
        );
    }

    // The stdlib is frozen: a script cannot patch a function the host might call.
    let mut slot =
        Slot::compile(1, "function on_start() string.format = nil end").expect("compile");
    slot.queue(ScriptEvent::Start);
    run_to_quiet(&mut slot, 10);
    assert!(
        slot.faulted,
        "writing to a sandboxed stdlib table should fault the script"
    );

    // `vf` itself is read-only, so one handler cannot rewrite the API another uses.
    let mut slot = Slot::compile(1, "function on_start() vf.set_pos = nil end").expect("compile");
    slot.queue(ScriptEvent::Start);
    run_to_quiet(&mut slot, 10);
    assert!(slot.faulted, "the vf table should be read-only");
}

/// A runtime error stops that script and explains itself, rather than unwinding
/// into the host.
#[test]
fn runtime_error_faults_with_a_message() {
    let mut slot = Slot::compile(1, "function on_start() error('boom') end").expect("compile");
    slot.queue(ScriptEvent::Start);
    // `run_to_quiet` *drains* the slot's outbox, so the fault line is in what it
    // returns — re-reading `shared.out` afterwards finds an empty vec and asserts
    // nothing.
    let errs = outputs(&run_to_quiet(&mut slot, 10));
    assert!(slot.faulted);
    assert!(
        errs.iter()
            .any(|(l, t)| *l == ScriptLevel::Error && t.contains("boom")),
        "expected the script's own message, got {errs:?}"
    );
}

/// NaN must not reach a transform: a prim at an undefined position cannot be
/// recovered by a later write.
#[test]
fn non_finite_positions_are_refused() {
    let mut slot =
        Slot::compile(1, "function on_start() vf.set_pos(0/0, 0, 0) end").expect("compile");
    slot.queue(ScriptEvent::Start);
    run_to_quiet(&mut slot, 10);
    assert!(slot.faulted, "a NaN position should be a script error");
    assert!(!slot
        .shared
        .out
        .borrow()
        .iter()
        .any(|c| matches!(c, ScriptCommand::SetPos { .. })));
}

#[test]
fn output_budget_caps_a_logging_loop() {
    let src = "function on_start() for i = 1, 10000 do vf.log('spam') end end";
    let mut slot = Slot::compile(1, src).expect("compile");
    slot.queue(ScriptEvent::Start);
    slot.run_slice(SCRIPT_FUEL_SLICE);
    assert!(
        slot.shared.out.borrow().len() <= MAX_COMMANDS_PER_TICK,
        "per-tick output budget was not applied"
    );
    assert!(slot.shared.dropped.get() > 0, "drops should be counted");
}

/// A script asking for a faster timer than the tick rate gets the tick rate, not
/// a busy loop.
#[test]
fn timer_interval_is_clamped_to_the_tick() {
    let mut slots = HashMap::new();
    let mut slot = Slot::compile(1, "function on_start() vf.timer(0.001) end").expect("compile");
    slot.queue(ScriptEvent::Start);
    slots.insert(1i64, slot);
    let (tx, _rx) = mpsc::channel();
    let mut cursor = 0;
    step(&mut slots, &mut cursor, 0.05, &tx);
    let (interval, _) = slots[&1].timer.expect("timer should be armed");
    assert!(
        (interval - MIN_TIMER_INTERVAL).abs() < f32::EPSILON,
        "interval {interval} was not clamped to {MIN_TIMER_INTERVAL}"
    );
}

/// One script's fuel slice must not consume the whole region bank.
#[test]
fn one_busy_script_does_not_starve_another() {
    let (tx, rx) = mpsc::channel();
    let mut slots = HashMap::new();
    let mut hog = Slot::compile(1, "function on_start() while true do end end").expect("compile");
    hog.queue(ScriptEvent::Start);
    slots.insert(1i64, hog);
    let mut quiet = Slot::compile(2, "function on_start() vf.log('ran') end").expect("compile");
    quiet.queue(ScriptEvent::Start);
    slots.insert(2i64, quiet);

    let mut cursor = 0;
    step(&mut slots, &mut cursor, 0.05, &tx);

    let cmds: Vec<ScriptCommand> = rx.try_iter().collect();
    assert!(
        outputs(&cmds)
            .iter()
            .any(|(l, t)| *l == ScriptLevel::Info && t == "ran"),
        "the quiet script was starved by the busy one: {:?}",
        outputs(&cmds)
    );
    assert!(
        slots[&1].running.is_some(),
        "the busy script should be suspended, still holding its place"
    );
}

/// Storing a script is a restart: the old instance and its state are gone.
#[test]
fn reload_replaces_the_instance() {
    let host = ScriptHost::spawn();
    host.load(
        1,
        "counter = 0 function on_touch() counter = counter + 1 vf.log(tostring(counter)) end"
            .into(),
    );
    host.event(
        1,
        ScriptEvent::Touch {
            avatar_id: 1,
            avatar_name: "a".into(),
        },
    );
    host.tick(0.05, vec![(1, PrimView::default())]);
    // Reload, then touch once more: the counter must start over at 1.
    host.load(
        1,
        "counter = 0 function on_touch() counter = counter + 1 vf.log(tostring(counter)) end"
            .into(),
    );
    host.event(
        1,
        ScriptEvent::Touch {
            avatar_id: 1,
            avatar_name: "a".into(),
        },
    );
    for _ in 0..10 {
        host.tick(0.05, vec![(1, PrimView::default())]);
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let logs: Vec<String> = outputs(&host.drain())
        .into_iter()
        .filter(|(l, _)| *l == ScriptLevel::Info)
        .map(|(_, t)| t)
        .collect();
    assert!(
        logs.iter().all(|l| l == "1"),
        "the reloaded script kept state from the old instance: {logs:?}"
    );
}

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
    let mut slot = Slot::compile(1, "function on_start() log('hi') end").expect("compile");
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
    let mut slot = Slot::compile(1, "function on_touch(id, name) say(name .. '/' .. id) end")
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

/// The other half of the runaway loop, and the one that was never tested: the
/// **top level** of the chunk, which does not run in a handler.
///
/// `Slot::compile` used to `exec()` the chunk — a `lua_pcall` on the main state,
/// where Luau cannot yield, so mlua discarded the interrupt's yield and this
/// one-liner never returned. The host thread stopped serving every other script,
/// its unbounded inbox kept filling at the tick rate, and because the source is
/// stored before it is compiled, restarting the server replayed it.
///
/// The test is "it returns at all". `#[should_panic]` would not do: a hang has no
/// panic, and the harness's own timeout is what a reader would have to know about.
#[test]
fn a_runaway_top_level_returns_instead_of_hanging_the_host() {
    let result = Slot::compile(1, "while true do end");
    let Err(err) = result else {
        panic!("an unbounded top level should not compile into a running script");
    };
    assert!(
        err.contains("load budget"),
        "the author should be told which limit stopped them: {err}"
    );

    // The interpreter is still usable afterwards — the budget stops the chunk, it
    // does not poison the host.
    let mut ok = Slot::compile(2, "function on_start() log('fine') end").expect("compile");
    ok.queue(ScriptEvent::Start);
    assert_eq!(
        outputs(&run_to_quiet(&mut ok, 10)),
        vec![(ScriptLevel::Info, "fine".to_string())]
    );
}

/// The second route to the same hang: a metamethod on the chunk's environment.
///
/// `Slot::handler` is called from Rust, on the host thread, at a point where the
/// fuel bank cannot suspend anything. With a metamethod-respecting `get` a script
/// could put its own `__index` on `getfenv()` and have every handler lookup run
/// arbitrary Lua there — including a loop. `raw_get` reads the field and never
/// asks the metatable, so the script simply has no handler.
#[test]
fn a_handler_lookup_cannot_run_script_code() {
    let mut slot = Slot::compile(
        1,
        "setmetatable(getfenv(), {__index = function(_, k) while true do end end})",
    )
    .expect("compile");

    // The lookup must answer "no handler" rather than executing the metamethod.
    assert!(
        slot.handler("on_start").is_none(),
        "a handler lookup resolved through the script's own metatable"
    );

    // And the scheduler agrees: an event for a handler that does not exist is
    // skipped, not run through `__index`.
    slot.queue(ScriptEvent::Start);
    let out = run_to_quiet(&mut slot, 10);
    assert!(outputs(&out).is_empty(), "expected nothing, got {out:?}");

    // The host functions are still reachable, so the raw read is finding the real
    // fields and not simply failing everything.
    let mut normal = Slot::compile(2, "function on_start() log('hi') end").expect("compile");
    normal.queue(ScriptEvent::Start);
    assert_eq!(
        outputs(&run_to_quiet(&mut normal, 10)),
        vec![(ScriptLevel::Info, "hi".to_string())]
    );
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

    let mut neighbour = Slot::compile(2, "function on_start() log('fine') end").expect("compile");
    neighbour.queue(ScriptEvent::Start);
    let out = run_to_quiet(&mut neighbour, 10);
    assert_eq!(outputs(&out), vec![(ScriptLevel::Info, "fine".to_string())]);
}

/// A script must not be able to reach another prim, the host, or the filesystem.
#[test]
fn sandbox_denies_escape_routes() {
    // Neither Luau nor this host provides these; the point is that they are
    // *absent*, so a script referencing one gets nil rather than a capability.
    //
    // `debug` is the one that was *present*: under the `luau` feature
    // `StdLib::ALL_SAFE` is `u32::MAX`, so `Lua::new()` opened it. Luau's version
    // is only `{info, traceback}` and `sandbox(true)` freezes it, so nothing was
    // exploitable — but the security model said it was not there, and this is the
    // assertion that would have said otherwise (and will, if a future Luau grows
    // the library while the stdlib set is taken from `ALL_SAFE` again).
    for expr in ["io", "require", "loadstring", "dofile", "package", "debug"] {
        let src = format!("function on_start() if {expr} ~= nil then log('LEAK') end end");
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

    // A script *can* shadow a host function, and that is not a hole. The functions
    // live in the script's own environment — they have to, because the same table
    // is where `function on_start()` lands, and a read-only one could not hold a
    // handler. Since every script is its own Luau state, the blast radius of
    // `say = nil` is the script that wrote it. What it must not do is take the
    // host with it.
    let mut slot = Slot::compile(1, "set_pos = nil function on_start() log('still here') end")
        .expect("compile");
    slot.queue(ScriptEvent::Start);
    let out = run_to_quiet(&mut slot, 10);
    assert!(
        !slot.faulted,
        "shadowing a host name should not fault the host"
    );
    assert_eq!(
        outputs(&out),
        vec![(ScriptLevel::Info, "still here".to_string())]
    );

    // And the neighbour is untouched, because it is a different VM entirely.
    let mut neighbour =
        Slot::compile(2, "function on_start() set_pos(1, 2, 3) end").expect("compile");
    neighbour.queue(ScriptEvent::Start);
    let out = run_to_quiet(&mut neighbour, 10);
    assert!(
        out.iter()
            .any(|c| matches!(c, ScriptCommand::SetPos { .. })),
        "another script's set_pos should still work"
    );
}

/// The other side of the explicit stdlib list: what an author is promised is
/// still there.
///
/// `docs/notes/prim-scripting.md` justifies the *absence* of `frand` and
/// `get_unix_time` by the presence of `math.random` and `os.time`, so narrowing
/// the library set is a documented-API change, not a tidy-up.
#[test]
fn the_standard_libraries_an_author_is_promised_are_present() {
    for expr in ["math.random", "string.format", "table.insert", "os.time"] {
        let src = format!("function on_start() if {expr} == nil then log('MISSING') end end");
        let mut slot = Slot::compile(1, &src).expect("compile");
        slot.queue(ScriptEvent::Start);
        let out = run_to_quiet(&mut slot, 10);
        assert!(!slot.faulted, "`{expr}` is not reachable from a script");
        assert!(outputs(&out).is_empty(), "`{expr}` is missing");
    }
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
    let mut slot = Slot::compile(1, "function on_start() set_pos(0/0, 0, 0) end").expect("compile");
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

/// The same rule for the scalar setters, which is where NaN actually got through.
///
/// `non_finite_positions_are_refused` tested the neighbouring call: `set_pos`
/// went through `finite_vec3`, while colour, alpha and glow went through
/// `f32::clamp`, which *propagates* NaN — it is a range limit, not a validator. A
/// NaN colour is persisted to SQLite and sent to every client's material.
#[test]
fn non_finite_appearance_values_are_refused() {
    for call in [
        "set_color(0/0, 0, 0)",
        "set_color(0, 1/0, 0)",
        "set_alpha(0/0)",
        "set_glow(0/0)",
    ] {
        let src = format!("function on_start() {call} end");
        let mut slot = Slot::compile(1, &src).expect("compile");
        slot.queue(ScriptEvent::Start);
        let out = run_to_quiet(&mut slot, 10);
        assert!(slot.faulted, "`{call}` should be a script error");
        assert!(
            !out.iter().any(|c| matches!(
                c,
                ScriptCommand::SetColor { .. }
                    | ScriptCommand::SetAlpha { .. }
                    | ScriptCommand::SetGlow { .. }
            )),
            "`{call}` reached the world anyway: {out:?}"
        );
    }

    // The valid range still works, and still clamps.
    let mut slot =
        Slot::compile(1, "function on_start() set_color(2, -1, 0.5) end").expect("compile");
    slot.queue(ScriptEvent::Start);
    let out = run_to_quiet(&mut slot, 10);
    let color = out
        .iter()
        .find_map(|c| match c {
            ScriptCommand::SetColor { color, .. } => Some(*color),
            _ => None,
        })
        .expect("a colour command");
    assert_eq!(color, [1.0, 0.0, 0.5]);
}

#[test]
fn output_budget_caps_a_logging_loop() {
    let src = "function on_start() for i = 1, 10000 do log('spam') end end";
    let mut slot = Slot::compile(1, src).expect("compile");
    slot.queue(ScriptEvent::Start);
    slot.run_slice(SCRIPT_FUEL_SLICE);
    assert!(
        slot.shared.out.borrow().len() <= MAX_COMMANDS_PER_TICK,
        "per-tick output budget was not applied"
    );
    assert!(slot.shared.dropped.get() > 0, "drops should be counted");
}

/// The event queue drops the right things when it is full.
///
/// The plain ring buffer evicted the *front*, and the front is `Start` — so a
/// script whose handler is slower than its own timer lost its initialisation and
/// then ran forever without it, which reads as "the script does nothing" with no
/// error anywhere. Timers also coalesce: a second pending `on_timer` only asks for
/// the same handler twice in a row later, and queueing them is what filled the
/// queue in the first place.
#[test]
fn a_flood_of_events_keeps_start_and_coalesces_timers() {
    let mut slot =
        Slot::compile(1, "function on_start() end function on_timer() end").expect("compile");
    slot.queue(ScriptEvent::Start);
    for _ in 0..MAX_PENDING_EVENTS * 4 {
        slot.queue(ScriptEvent::Timer);
    }
    let timers = slot
        .pending
        .iter()
        .filter(|e| matches!(e, ScriptEvent::Timer))
        .count();
    assert_eq!(timers, 1, "timers should collapse to one pending fire");
    assert!(
        slot.pending.iter().any(|e| matches!(e, ScriptEvent::Start)),
        "the Start event was evicted, so the script would run uninitialised"
    );

    // A flood of *distinct* events still evicts, and still not the Start.
    for i in 0..MAX_PENDING_EVENTS * 2 {
        slot.queue(ScriptEvent::Touch {
            avatar_id: i as u64,
            avatar_name: "ada".into(),
        });
    }
    assert!(
        slot.pending.len() <= MAX_PENDING_EVENTS,
        "queue is unbounded"
    );
    assert!(
        slot.pending.iter().any(|e| matches!(e, ScriptEvent::Start)),
        "the Start event was evicted by a touch flood"
    );
}

/// A script asking for a faster timer than the tick rate gets the tick rate, not
/// a busy loop.
#[test]
fn timer_interval_is_clamped_to_the_tick() {
    let mut slots = HashMap::new();
    let mut slot =
        Slot::compile(1, "function on_start() set_timer_event(0.001) end").expect("compile");
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

/// One script's fuel slice must not consume the whole world's bank.
#[test]
fn one_busy_script_does_not_starve_another() {
    let (tx, rx) = mpsc::channel();
    let mut slots = HashMap::new();
    let mut hog = Slot::compile(1, "function on_start() while true do end end").expect("compile");
    hog.queue(ScriptEvent::Start);
    slots.insert(1i64, hog);
    let mut quiet = Slot::compile(2, "function on_start() log('ran') end").expect("compile");
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
        "counter = 0 function on_touch() counter = counter + 1 log(tostring(counter)) end".into(),
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
        "counter = 0 function on_touch() counter = counter + 1 log(tostring(counter)) end".into(),
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

/// The editor's reference and the installed API must be the same set.
///
/// `cartopolis_core::SCRIPT_API` is what an author reads in the panel; this is what
/// they can actually call. A name in one and not the other is either a function
/// nobody can find or a documented call that errors when used — and both are the
/// kind of thing that survives review because each half looks right on its own.
#[test]
fn the_catalogue_matches_the_installed_api() {
    use std::collections::BTreeSet;

    let slot = Slot::compile(1, "").expect("compile");
    let installed: BTreeSet<String> = slot
        .env
        .pairs::<String, mlua::Value>()
        .filter_map(Result::ok)
        // An empty script defines nothing, so everything in its environment is
        // the host's: the installed functions and the prelude's `sleep`.
        .map(|(k, _)| k)
        .collect();
    let catalogued: BTreeSet<String> = cartopolis_core::SCRIPT_API
        .iter()
        .map(|f| f.name.to_string())
        .collect();

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

/// Authors write the degrees the edit dialog shows them; the wire is radians.
#[test]
fn rotation_is_degrees_in_and_radians_out() {
    let mut slot = Slot::compile(1, "function on_start() set_rot(0, 180, 0) end").expect("compile");
    slot.queue(ScriptEvent::Start);
    let out = run_to_quiet(&mut slot, 10);
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

    // And it round-trips: what a script reads back is degrees again.
    let mut slot = Slot::compile(
        1,
        "function on_start() local _, y, _ = get_rot() log(string.format('%.1f', y)) end",
    )
    .expect("compile");
    slot.shared.view.borrow_mut().rot = glam::Vec3::new(0.0, std::f32::consts::PI, 0.0);
    slot.queue(ScriptEvent::Start);
    let out = run_to_quiet(&mut slot, 10);
    assert_eq!(
        outputs(&out),
        vec![(ScriptLevel::Info, "180.0".to_string())]
    );
}

/// Each speaking verb carries its own radius, so the tick loop can deliver by
/// distance without knowing which one was called.
#[test]
fn each_chat_verb_has_its_own_radius() {
    let mut slot = Slot::compile(
        1,
        "function on_start() whisper('a') say('b') shout('c') end",
    )
    .expect("compile");
    slot.queue(ScriptEvent::Start);
    let out = run_to_quiet(&mut slot, 10);
    let radii: Vec<f32> = out
        .iter()
        .filter_map(|c| match c {
            ScriptCommand::Say { radius, .. } => Some(*radius),
            _ => None,
        })
        .collect();
    assert_eq!(radii.len(), 3);
    assert!(
        radii[0] < radii[1] && radii[1] < radii[2],
        "whisper < say < shout, got {radii:?}"
    );
    assert_eq!(radii[1], crate::chat::SAY_RADIUS_M);
    assert_eq!(radii[2], crate::chat::SHOUT_RADIUS_M);
}

/// `sleep` suspends the handler and lets the rest of the tick happen, then
/// finishes on its own — the same descheduling a script that overruns gets, but
/// asked for.
#[test]
fn sleep_suspends_the_handler_and_resumes_it() {
    let mut slot = Slot::compile(
        1,
        "function on_start() log('before') sleep(0.2) log('after') end",
    )
    .expect("compile");
    slot.queue(ScriptEvent::Start);

    slot.run_slice(SCRIPT_FUEL_SLICE);
    let first = outputs(&slot.shared.out.borrow());
    assert_eq!(
        first,
        vec![(ScriptLevel::Info, "before".to_string())],
        "the handler should have run up to the sleep and stopped there"
    );
    assert!(
        slot.running.is_some(),
        "it should be suspended, not finished"
    );

    // Nothing more happens until enough simulated time has passed.
    for _ in 0..3 {
        slot.shared.time.set(slot.shared.time.get() + 0.05);
        slot.run_slice(SCRIPT_FUEL_SLICE);
    }
    assert!(
        outputs(&slot.shared.out.borrow())
            .iter()
            .all(|(_, t)| t != "after"),
        "it woke early"
    );

    slot.shared.time.set(slot.shared.time.get() + 0.05);
    slot.run_slice(SCRIPT_FUEL_SLICE);
    let all = outputs(&slot.shared.out.borrow());
    assert!(
        all.iter().any(|(_, t)| t == "after"),
        "the handler should have finished once the clock passed 0.2s: {all:?}"
    );
    assert!(slot.running.is_none(), "and then be done");
}

/// The clock `get_time` reads is simulated time, advanced for every script by
/// the scheduler — including ones that got no slice.
#[test]
fn the_script_clock_advances_with_the_tick() {
    let (tx, _rx) = mpsc::channel();
    let mut slots = HashMap::new();
    slots.insert(
        1i64,
        Slot::compile(1, "function on_timer() end").expect("compile"),
    );
    let mut cursor = 0;
    for _ in 0..4 {
        step(&mut slots, &mut cursor, 0.05, &tx);
    }
    let t = slots[&1].shared.time.get();
    assert!((t - 0.2).abs() < 1e-5, "expected 0.2s of sim time, got {t}");
}

/// A script can end its own prim, and that is a world mutation like any other —
/// it leaves the host as a command for the tick loop, not as a direct delete.
#[test]
fn die_is_a_command_not_a_side_effect() {
    let mut slot = Slot::compile(1, "function on_touch() die() end").expect("compile");
    slot.queue(ScriptEvent::Touch {
        avatar_id: 1,
        avatar_name: "ada".into(),
    });
    let out = run_to_quiet(&mut slot, 10);
    assert!(
        out.iter()
            .any(|c| matches!(c, ScriptCommand::Die { prim_id } if *prim_id == 1)),
        "expected a Die for prim 1, got {out:?}"
    );
}

// ---------------------------------------------------------------------------
// Script-to-script messaging
// ---------------------------------------------------------------------------

/// Compile a pair of scripts, queue `on_start` for each and run the real
/// scheduler over them for a few ticks.
///
/// It goes through [`step`] rather than delivering an event by hand, because the
/// fan-out *is* what these tests are about: who hears a message, when, and in
/// what order is decided there and nowhere else.
fn talk(a: &str, b: &str, ticks: u32) -> Vec<ScriptCommand> {
    let (tx, rx) = mpsc::channel();
    let mut slots: HashMap<i64, Slot> = HashMap::new();
    for (id, source) in [(1, a), (2, b)] {
        let mut slot = Slot::compile(id, source).expect("compile");
        slot.queue(ScriptEvent::Start);
        slots.insert(id, slot);
    }
    let mut cursor = 0usize;
    for _ in 0..ticks {
        step(&mut slots, &mut cursor, 0.05, &tx);
    }
    drop(tx);
    rx.into_iter().collect()
}

/// Whether any script logged `text`.
fn logged(out: &[ScriptCommand], text: &str) -> bool {
    out.iter().any(|c| {
        matches!(c, ScriptCommand::Output { level, text: t, .. }
            if *level == ScriptLevel::Info && t == text)
    })
}

/// A message reaches a listener, and carries the sender's id.
#[test]
fn a_script_can_talk_to_another_script() {
    let out = talk(
        "function on_start() send(7, 'ping') end",
        "function on_start() listen(7) end \
         function on_message(c, t, from) log(t .. ' ' .. c .. ' ' .. from) end",
        8,
    );
    let lines: Vec<String> = out
        .iter()
        .filter_map(|c| match c {
            ScriptCommand::Output { level, text, .. } if *level == ScriptLevel::Info => {
                Some(text.clone())
            }
            _ => None,
        })
        .collect();
    assert_eq!(lines, vec!["ping 7 1".to_string()], "{out:?}");
}

/// **A script does not hear itself**, which is the one place this deviates from
/// Second Life and is deliberate: listening on the channel you send on is the
/// ordinary shape (the beacon and the firebird both do it), and hearing yourself
/// turns it into a feedback loop the author did not write.
#[test]
fn a_script_does_not_hear_its_own_message() {
    let out = talk(
        "function on_start() listen(7) send(7, 'hello') end \
         function on_message(c, t, from) log('heard myself') end",
        "function on_start() end",
        8,
    );
    assert!(!logged(&out, "heard myself"), "{out:?}");
}

/// Listening is opt-in. Without it every `send` would queue an event on every
/// script in the world, so one chatty prim would cost every other one work it
/// never asked for.
#[test]
fn a_script_hears_nothing_until_it_listens() {
    let out = talk(
        "function on_start() send(7, 'ping') end",
        "function on_start() end function on_message(c, t, f) log('heard') end",
        8,
    );
    assert!(!logged(&out, "heard"), "{out:?}");
}

/// A channel is not a public address: a listener on 7 does not hear 8.
#[test]
fn channels_are_separate() {
    let out = talk(
        "function on_start() send(8, 'ping') end",
        "function on_start() listen(7) end \
         function on_message(c, t, f) log('heard') end",
        8,
    );
    assert!(!logged(&out, "heard"), "{out:?}");
}

/// `listen(channel, false)` stops it again.
#[test]
fn listening_can_be_stopped() {
    let out = talk(
        "function on_start() set_timer_event(0.05) end \
         function on_timer() send(7, 'ping') end",
        "function on_start() listen(7) listen(7, false) end \
         function on_message(c, t, f) log('heard') end",
        12,
    );
    assert!(!logged(&out, "heard"), "{out:?}");
}

/// A script cannot flood the world: the send budget is per tick, and overrunning
/// it is reported the way an overrun command budget is rather than faulting the
/// script mid-handler.
#[test]
fn the_send_budget_is_bounded() {
    let out = talk(
        "function on_start() for i = 1, 100 do send(7, 'ping') end end",
        "function on_start() listen(7) end \
         function on_message(c, t, f) log('heard') end",
        6,
    );
    let heard = out
        .iter()
        .filter(|c| matches!(c, ScriptCommand::Output { text, .. } if text == "heard"))
        .count();
    assert!(
        heard <= MAX_MESSAGES_PER_TICK,
        "100 sends in one handler delivered {heard} messages; the cap is {MAX_MESSAGES_PER_TICK}"
    );
    assert!(heard > 0, "the first few should still have gone through");
}

/// **One vocabulary for an emitter, across both places one can be written.**
///
/// A script says `particles{ spread_deg = 20 }` and an HTTP body says
/// `{"spread_deg": 20}`, and this is the test that keeps that true. They were
/// spelled differently for a day — the script API took `spread`, `radius`,
/// `size`, `end_size`, `color`, `end_color` — which meant the same emitter had
/// two names for six of its twelve fields depending on which surface set it, and
/// nothing anywhere would have noticed them drifting further apart.
///
/// Read out of `ParticleSpec`'s own `Serialize` rather than a second hand-written
/// list, on the same rule `the_catalogue_matches_the_installed_api` follows: a
/// list that has to be maintained beside the thing it describes is a list that
/// will stop describing it.
#[test]
fn the_two_authoring_surfaces_use_one_vocabulary() {
    use std::collections::BTreeSet;

    let http: BTreeSet<String> =
        match serde_json::to_value(crate::authoring::ParticleSpec::default()).expect("serialise") {
            serde_json::Value::Object(map) => map.keys().cloned().collect(),
            other => panic!("ParticleSpec should serialise to an object, got {other:?}"),
        };

    // What a script may write. Taken from the error the host itself produces for
    // an unknown key, so it cannot drift from the parser.
    let slot = Slot::compile(1, "function on_start() end").expect("compile");
    let lua: BTreeSet<String> = {
        let err = slot
            .lua
            .load("particles({ definitely_not_a_field = 1 })")
            .set_environment(slot.env.clone())
            .exec()
            .expect_err("an unknown rule must be refused");
        let text = err.to_string();
        let list = text
            .split("is not one of: ")
            .nth(1)
            .expect("the refusal should list the valid keys")
            .lines()
            .next()
            .unwrap_or_default();
        list.split(", ").map(|k| k.trim().to_string()).collect()
    };

    // The two the script API deliberately does not take, and why. `burst` is
    // decided by *which function* was called (`particles` against
    // `particle_burst`), and `sequence` is derived from the prim so a re-armed
    // blast cannot reuse a number a client has already played.
    let only_http: BTreeSet<String> = http.difference(&lua).cloned().collect();
    assert_eq!(
        only_http,
        ["burst".to_string(), "sequence".to_string()]
            .into_iter()
            .collect::<BTreeSet<_>>(),
        "the admin API has fields a script does not, and these are the two that \
         are meant to be"
    );

    let only_lua: BTreeSet<&String> = lua.difference(&http).collect();
    assert!(
        only_lua.is_empty(),
        "a script can set {only_lua:?}, which the admin API has no name for"
    );
}

/// The six renamed rules answer with their new name rather than the whole list.
///
/// A script written against the old spelling is somebody's real work, and it
/// fails loudly either way — the point is that the failure says what to do.
#[test]
fn an_old_particle_rule_name_says_what_it_is_called_now() {
    let slot = Slot::compile(1, "function on_start() end").expect("compile");
    for (old, now) in [
        ("spread", "spread_deg"),
        ("radius", "source_radius"),
        ("size", "size_start"),
        ("end_size", "size_end"),
        ("color", "color_start"),
        ("end_color", "color_end"),
    ] {
        let err = slot
            .lua
            .load(format!("particles({{ {old} = 1 }})"))
            .set_environment(slot.env.clone())
            .exec()
            .expect_err("an old name must be refused");
        let text = err.to_string();
        assert!(
            text.contains(now),
            "`{old}` should point at `{now}`, said: {text}"
        );
    }
}

/// `play_sound` emits a one-shot with the defaults filled in, and the numbers an
/// author passes are clamped rather than trusted.
#[test]
fn play_sound_emits_a_clamped_one_shot() {
    use cartopolis_core::{SoundName, DEFAULT_SOUND_RANGE_M, MAX_SOUND_RANGE_M};
    let mut slot = Slot::compile(
        1,
        "function on_start() play_sound('firework') play_sound('bell', 4, 9000) end",
    )
    .expect("compile");
    slot.queue(ScriptEvent::Start);
    let sounds: Vec<_> = run_to_quiet(&mut slot, 10)
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

/// An unknown sound is an error that names the library — never a silent no-op,
/// which an author cannot tell apart from a speaker that is off.
#[test]
fn an_unknown_sound_is_an_error_naming_the_library() {
    let mut slot =
        Slot::compile(1, "function on_start() play_sound('fireworks') end").expect("compile");
    slot.queue(ScriptEvent::Start);
    let out = outputs(&run_to_quiet(&mut slot, 10));
    let error = out
        .iter()
        .find(|(level, _)| *level == ScriptLevel::Error)
        .unwrap_or_else(|| panic!("expected an error, got {out:?}"));
    assert!(error.1.contains("fireworks"), "{}", error.1);
    assert!(error.1.contains("firework, crackle"), "{}", error.1);
}

/// A sound in a tight loop is capped per tick and counted as dropped, not
/// faulted: a busy script, not a broken one.
#[test]
fn play_sound_is_capped_per_tick() {
    let mut slot = Slot::compile(
        1,
        "function on_start() for i = 1, 10 do play_sound('pop') end end",
    )
    .expect("compile");
    slot.queue(ScriptEvent::Start);
    let cmds = run_to_quiet(&mut slot, 10);
    let played = cmds
        .iter()
        .filter(|c| matches!(c, ScriptCommand::PlaySound { .. }))
        .count();
    assert_eq!(played, MAX_SOUNDS_PER_TICK as usize);
    assert!(!slot.faulted);
    assert_eq!(slot.shared.dropped.get(), 10 - MAX_SOUNDS_PER_TICK);
}

/// `loop_sound` sets the prim's loop and `stop_sound` clears it.
#[test]
fn loop_and_stop_set_the_prims_loop() {
    use cartopolis_core::SoundName;
    let mut slot = Slot::compile(
        1,
        "function on_start() loop_sound('whoosh', 0.5, 20) stop_sound() end",
    )
    .expect("compile");
    slot.queue(ScriptEvent::Start);
    let loops: Vec<_> = run_to_quiet(&mut slot, 10)
        .into_iter()
        .filter_map(|c| match c {
            ScriptCommand::SetSoundLoop { sound, .. } => Some(sound),
            _ => None,
        })
        .collect();
    assert_eq!(loops.len(), 2);
    let first = loops[0].expect("a loop");
    assert_eq!(first.sound, SoundName::Whoosh);
    assert_eq!(first.volume, 0.5);
    assert_eq!(first.range_m, 20.0);
    assert_eq!(loops[1], None);
}

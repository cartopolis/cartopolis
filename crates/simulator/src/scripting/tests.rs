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
    let mut slot = Slot::compile(1, "function on_start() vfLog('hi') end").expect("compile");
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
        "function on_touch(id, name) vfSay(name .. '/' .. id) end",
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
    let mut ok = Slot::compile(2, "function on_start() vfLog('fine') end").expect("compile");
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

    // The `vf` functions are still reachable, so the raw read is finding the real
    // fields and not simply failing everything.
    let mut normal = Slot::compile(2, "function on_start() vfLog('hi') end").expect("compile");
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

    let mut neighbour = Slot::compile(2, "function on_start() vfLog('fine') end").expect("compile");
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
        let src = format!("function on_start() if {expr} ~= nil then vfLog('LEAK') end end");
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

    // A script *can* shadow a vf function, and that is not a hole. The functions
    // live in the script's own environment — they have to, because the same table
    // is where `function on_start()` lands, and a read-only one could not hold a
    // handler. Since every script is its own Luau state, the blast radius of
    // `vfSay = nil` is the script that wrote it. What it must not do is take the
    // host with it.
    let mut slot = Slot::compile(
        1,
        "vfSetPos = nil function on_start() vfLog('still here') end",
    )
    .expect("compile");
    slot.queue(ScriptEvent::Start);
    let out = run_to_quiet(&mut slot, 10);
    assert!(
        !slot.faulted,
        "shadowing a vf name should not fault the host"
    );
    assert_eq!(
        outputs(&out),
        vec![(ScriptLevel::Info, "still here".to_string())]
    );

    // And the neighbour is untouched, because it is a different VM entirely.
    let mut neighbour =
        Slot::compile(2, "function on_start() vfSetPos(1, 2, 3) end").expect("compile");
    neighbour.queue(ScriptEvent::Start);
    let out = run_to_quiet(&mut neighbour, 10);
    assert!(
        out.iter()
            .any(|c| matches!(c, ScriptCommand::SetPos { .. })),
        "another script's vfSetPos should still work"
    );
}

/// The other side of the explicit stdlib list: what an author is promised is
/// still there.
///
/// `docs/notes/prim-scripting.md` justifies the *absence* of `vfFrand` and
/// `vfGetUnixTime` by the presence of `math.random` and `os.time`, so narrowing
/// the library set is a documented-API change, not a tidy-up.
#[test]
fn the_standard_libraries_an_author_is_promised_are_present() {
    for expr in ["math.random", "string.format", "table.insert", "os.time"] {
        let src = format!("function on_start() if {expr} == nil then vfLog('MISSING') end end");
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
    let mut slot =
        Slot::compile(1, "function on_start() vfSetPos(0/0, 0, 0) end").expect("compile");
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
/// `non_finite_positions_are_refused` tested the neighbouring call: `vfSetPos`
/// went through `finite_vec3`, while colour, alpha and glow went through
/// `f32::clamp`, which *propagates* NaN — it is a range limit, not a validator. A
/// NaN colour is persisted to SQLite and sent to every client's material.
#[test]
fn non_finite_appearance_values_are_refused() {
    for call in [
        "vfSetColor(0/0, 0, 0)",
        "vfSetColor(0, 1/0, 0)",
        "vfSetAlpha(0/0)",
        "vfSetGlow(0/0)",
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
        Slot::compile(1, "function on_start() vfSetColor(2, -1, 0.5) end").expect("compile");
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
    let src = "function on_start() for i = 1, 10000 do vfLog('spam') end end";
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
        Slot::compile(1, "function on_start() vfSetTimerEvent(0.001) end").expect("compile");
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
    let mut quiet = Slot::compile(2, "function on_start() vfLog('ran') end").expect("compile");
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
        "counter = 0 function on_touch() counter = counter + 1 vfLog(tostring(counter)) end".into(),
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
        "counter = 0 function on_touch() counter = counter + 1 vfLog(tostring(counter)) end".into(),
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
/// `viberfox_core::SCRIPT_API` is what an author reads in the panel; this is what
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
        .map(|(k, _)| k)
        .filter(|k| k.starts_with("vf"))
        .collect();
    let catalogued: BTreeSet<String> = viberfox_core::SCRIPT_API
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
    let mut slot =
        Slot::compile(1, "function on_start() vfSetRot(0, 180, 0) end").expect("compile");
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
        "function on_start() local _, y, _ = vfGetRot() vfLog(string.format('%.1f', y)) end",
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
        "function on_start() vfWhisper('a') vfSay('b') vfShout('c') end",
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

/// `vfSleep` suspends the handler and lets the rest of the tick happen, then
/// finishes on its own — the same descheduling a script that overruns gets, but
/// asked for.
#[test]
fn sleep_suspends_the_handler_and_resumes_it() {
    let mut slot = Slot::compile(
        1,
        "function on_start() vfLog('before') vfSleep(0.2) vfLog('after') end",
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

/// The clock `vfGetTime` reads is simulated time, advanced for every script by
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
    let mut slot = Slot::compile(1, "function on_touch() vfDie() end").expect("compile");
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

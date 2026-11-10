//! CLAUDE.md's "Rules that hold everywhere", the ones a source scan can hold.
//!
//! Each rule here bit someone silently, and a line of prose only reaches the
//! agent that happens to read it; a red test reaches every one. These read the
//! sources rather than the types because none of them is a type error: a
//! `Res<EguiManager>` compiles everywhere but the web, and a `NoFrustumCulling`
//! without `NotShadowCaster` compiles and draws, just into every shadow cascade.
//! The panel chain (`egui_manager::every_root_ui_panel_is_in_the_chain`) and the
//! one tile fixture (`cartopolis_geo::testdata`) are held the same way, where
//! they live.
//!
//! Comment lines are skipped, so the comments explaining a rule can name what
//! it forbids.

use std::path::{Path, PathBuf};

fn sources() -> Vec<(PathBuf, String)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut stack = vec![root];
    let mut out = Vec::new();
    while let Some(dir) = stack.pop() {
        for e in std::fs::read_dir(&dir).expect("src/ is readable").flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "rs") && !p.ends_with("source_rules.rs") {
                let text = std::fs::read_to_string(&p).expect("source is readable");
                out.push((p, text));
            }
        }
    }
    out.sort();
    out
}

fn is_comment(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with("//") || t.starts_with("* ") || t.starts_with("/*")
}

/// `file:line: text` of every code line containing `needle`.
fn code_lines_with(needle: &str) -> Vec<String> {
    let mut hits = Vec::new();
    for (path, text) in sources() {
        let rel = path
            .strip_prefix(env!("CARGO_MANIFEST_DIR"))
            .unwrap_or(&path);
        for (i, line) in text.lines().enumerate() {
            if !is_comment(line) && line.contains(needle) {
                hits.push(format!("{}:{}: {}", rel.display(), i + 1, line.trim()));
            }
        }
    }
    hits
}

/// `EguiManager` is non-send on wasm, so `Res<EguiManager>` compiles and
/// runs on every native target and breaks only the web build
/// (`docs/notes/egui-shared-root-ui.md`).
#[test]
fn egui_is_reached_through_egui_ref_and_egui_mut() {
    let hits: Vec<_> = ["Res<EguiManager>", "ResMut<EguiManager>"]
        .iter()
        .flat_map(|n| code_lines_with(n))
        .collect();
    assert!(
        hits.is_empty(),
        "reach egui through `EguiRef` / `EguiMut`, never `Res<EguiManager>`:\n{}",
        hits.join("\n")
    );
}

/// `egui_wants_keyboard_input()` is true for any focused widget, a button
/// included, so game keys died after every click on the UI. Only a focused
/// text field owns the keyboard (`docs/notes/egui-shared-root-ui.md`).
#[test]
fn game_keys_yield_only_to_a_focused_text_field() {
    let hits = code_lines_with("egui_wants_keyboard_input");
    assert!(
        hits.is_empty(),
        "use `EguiManager::ui_owns_keyboard`, never `egui_wants_keyboard_input()`:\n{}",
        hits.join("\n")
    );
}

/// Bevy's shader compiles and screenshot readbacks share
/// `AsyncComputeTaskPool`, so blocking work there stalls them; it goes through
/// `platform::workers` (`docs/notes/headless-shots-software-renderer.md`).
/// Asking the pool its size is not work.
#[test]
fn blocking_work_goes_through_platform_workers() {
    let hits: Vec<_> = code_lines_with("AsyncComputeTaskPool")
        .into_iter()
        .filter(|l| !l.contains("AsyncComputeTaskPool::get().thread_num()"))
        .collect();
    assert!(
        hits.is_empty(),
        "blocking CPU work goes through `platform::workers`, never `AsyncComputeTaskPool`:\n{}",
        hits.join("\n")
    );
}

/// An entity that opts out of frustum culling is pushed into every cascade of
/// every shadow-casting light, so a flat ribbon in the road was rasterised into
/// eight shadow maps (`docs/notes/render-performance-survey.md`). The bundle
/// that names `NoFrustumCulling` names `NotShadowCaster` too — within twenty
/// lines of it — or says `casts shadows` in a comment beside it if it really
/// does.
#[test]
fn no_frustum_culling_comes_with_not_shadow_caster() {
    const REACH: usize = 20;
    let mut bad = Vec::new();
    for (path, text) in sources() {
        let lines: Vec<&str> = text.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            if is_comment(line)
                || !line.contains("NoFrustumCulling")
                || line.trim_start().starts_with("use ")
            {
                continue;
            }
            let window = &lines[i.saturating_sub(REACH)..(i + REACH + 1).min(lines.len())];
            if !window
                .iter()
                .any(|l| l.contains("NotShadowCaster") || l.contains("casts shadows"))
            {
                let rel = path
                    .strip_prefix(env!("CARGO_MANIFEST_DIR"))
                    .unwrap_or(&path);
                bad.push(format!("{}:{}", rel.display(), i + 1));
            }
        }
    }
    assert!(
        bad.is_empty(),
        "`NoFrustumCulling` without `NotShadowCaster` beside it (or a `casts shadows` comment):\n{}",
        bad.join("\n")
    );
}

/// A control painted by hand — a rect given a click sense and drawn over —
/// carries no name of its own, so the client's `ui_tree` tool (egui's AccessKit
/// tree, `systems/dev/mcp/screen.rs`) lists it as "clickable area (no label)"
/// and nobody driving the client can tell it from the next one. Every such
/// rect under `src/systems` is named where it is made: `theme::named(ui, resp,
/// "What it does")`, or `resp.widget_info(..)` for a switch or a choice that
/// has an on state. One that is not a control (a backdrop that only swallows
/// touches) says so with an `// unnamed: <why>` comment on the line above.
///
/// Widgets that carry their own text (`Label`, `Button`, a `UiBuilder::sense`
/// row, all `.sense(..)`) are left to it; a sense picked into a variable
/// first is out of the scan's reach, so name those by hand all the same.
/// `#[cfg(test)] mod`s are not scanned.
#[test]
fn hand_drawn_controls_are_named() {
    const CALLS: &[&str] = &[
        "allocate_exact_size(",
        "allocate_rect(",
        "interact(",
        "map_disc(",
    ];
    const SENSES: &[&str] = &["Sense::click()", "Sense::click_and_drag()"];
    // A call split over lines names its sense up to this many lines after itself.
    const BACK: usize = 3;
    // The name may come this many lines after the sense.
    const AHEAD: usize = 10;
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/systems");
    let mut bad = Vec::new();
    for (path, text) in sources() {
        if !path.starts_with(&root) {
            continue;
        }
        let lines: Vec<&str> = text.lines().collect();
        // Everything from the test module on is tests.
        let end = (1..lines.len())
            .find(|&j| {
                lines[j - 1].trim_start().starts_with("#[cfg(test)]")
                    && lines[j].trim_start().starts_with("mod ")
            })
            .unwrap_or(lines.len());
        for (i, line) in lines.iter().enumerate().take(end) {
            if is_comment(line)
                || line.contains(".sense(")
                || !SENSES.iter().any(|s| line.contains(s))
            {
                continue;
            }
            let Some(call) = (i.saturating_sub(BACK)..=i)
                .rev()
                .find(|&j| !is_comment(lines[j]) && CALLS.iter().any(|c| lines[j].contains(c)))
            else {
                continue;
            };
            let near = &lines[i.saturating_sub(BACK)..(i + AHEAD + 1).min(lines.len())];
            let named = near
                .iter()
                .any(|l| l.contains("named(") || l.contains("widget_info"));
            let marked = lines[call.saturating_sub(1)..=i]
                .iter()
                .any(|l| l.contains("// unnamed:"));
            if !named && !marked {
                let rel = path
                    .strip_prefix(env!("CARGO_MANIFEST_DIR"))
                    .unwrap_or(&path);
                bad.push(format!(
                    "{}:{}: {}",
                    rel.display(),
                    call + 1,
                    lines[call].trim()
                ));
            }
        }
    }
    assert!(
        bad.is_empty(),
        "a hand-drawn control has no name, so `ui_tree` shows it as \"clickable area (no \
         label)\". Name it right after it is made, with the words a person sees on it or a \
         short phrase for what it does: `let resp = theme::named(ui, resp, \"Send\");` (or \
         `resp.widget_info(|| egui::WidgetInfo::selected(..))` for a switch or a choice). If \
         it is not a control at all, put `// unnamed: <why>` on the line above it:\n{}",
        bad.join("\n")
    );
}

/// `std::time::Instant::now()` and `SystemTime::now()` panic on wasm32 ("time
/// not implemented on this platform"), and a panic is `abort` there, so the
/// browser client dies: the watchdog's clock did it to every browser at its
/// first frame on 2026-08-21, and a breakout room's countdown carried another
/// from 2026-10-17 until it was found by reading on 2026-11-01. Use
/// `web_time` (or `bevy::platform::time`), which natively *is* std.
///
/// A scan cannot read `cfg`, so the files listed here are the ones that never
/// compile on wasm (their `mod` is gated off it) or gate the call inside them;
/// a new file that needs std's clock joins the list with its reason.
/// `#[cfg(test)]` items are not scanned.
#[test]
fn the_clock_is_web_time_where_the_web_runs() {
    const NATIVE_ONLY: &[&str] = &[
        "platform/android.rs",
        "platform/camera.rs",      // `cfg(not(wasm32))` in platform/mod.rs
        "platform/hw_video.rs",    // `cfg(target_os = "android")`
        "platform/orientation.rs", // its `mod android`
        "platform/http.rs",        // inside its native `mod`
        "platform/workers.rs",     // each call `cfg`-gated beside it
        "systems/dev/aerial_reference.rs", // `cfg(not(wasm32))`
        "systems/dev/mcp/",        // `cfg(mcp)`, Unix (and Android) only
        "systems/dev/system_timings.rs", // feature `perf`, never on the web
        "systems/gui/camera_screen.rs", // its `mod native`
        "systems/gui/camera_screen/recorder.rs", // `cfg(not(wasm32))`
        "systems/player/face_tracking.rs", // `cfg(not(wasm32))`
    ];
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut bad = Vec::new();
    for (path, text) in sources() {
        let rel = path
            .strip_prefix(&root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        if NATIVE_ONLY.iter().any(|n| rel.starts_with(n)) {
            continue;
        }
        // The `#[cfg(test)]` item being skipped: its brace depth, once opened.
        let mut in_test: Option<i32> = None;
        for (i, line) in text.lines().enumerate() {
            if is_comment(line) {
                continue;
            }
            let t = line.trim_start();
            if in_test.is_none()
                && (t.starts_with("#[cfg(test)]") || t.starts_with("#[cfg(all(test,"))
            {
                in_test = Some(0);
                continue;
            }
            if let Some(depth) = in_test.as_mut() {
                let opened = *depth > 0 || line.contains('{');
                *depth += line.matches('{').count() as i32 - line.matches('}').count() as i32;
                // Done when its braces close, or a brace-less item (`use …;`) ends.
                if (opened && *depth <= 0) || (!opened && line.trim_end().ends_with(';')) {
                    in_test = None;
                }
                continue;
            }
            let import = line.trim_start().starts_with("use std::time")
                && (line.contains("Instant") || line.contains("SystemTime"));
            if import
                || line.contains("std::time::Instant::now")
                || line.contains("std::time::SystemTime::now")
            {
                bad.push(format!("src/{rel}:{}: {}", i + 1, line.trim()));
            }
        }
    }
    assert!(
        bad.is_empty(),
        "std's clock panics on wasm32; use `web_time::{{Instant, SystemTime}}`:\n{}",
        bad.join("\n")
    );
}

/// Time and curve live in `systems::motion` (`docs/mockups/fluid-ui/`): a
/// panel says what kind of change it draws and takes the duration from there,
/// so the five durations stay five and the reduced-motion setting reaches
/// everything. egui's `animate_*` is called in that one module only.
#[test]
fn every_animation_goes_through_motion() {
    let hits: Vec<_> = ["animate_bool", "animate_value"]
        .iter()
        .flat_map(|n| code_lines_with(n))
        .filter(|hit| !hit.contains("systems/gui/motion.rs"))
        .collect();
    assert!(
        hits.is_empty(),
        "animate through `motion::presence` / `toggle` / `value` / `press`, never egui's `animate_*` directly:\n{}",
        hits.join("\n")
    );
}

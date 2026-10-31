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

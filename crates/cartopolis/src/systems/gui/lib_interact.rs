//! The avatar's prompts over the map drawn by the component library
//! (`lib_host`), beside egui's: the interaction prompt near something to
//! use (`interact::render_interaction_prompt` — "Sit at Bench", "Route
//! here", a greeting, Throw and Put down, Climb up), and the chip that says
//! what the camera follows (`camera_attach::render_attach_chip`).
//!
//! On with `CARTO_UI_LIB=interact` and `CARTO_UI_LIB=attach_chip` (or `1`),
//! on the web `?ui=lib`. egui's systems step aside (`InteractState::lib`,
//! `CameraAttach::lib`) but keep every decision — whether the prompt shows
//! (driving, the camera, the map's rect), which controls it carries and
//! their key hints, where the map's bottom centre is — and publish it in
//! `InteractState::lib_view` and `CameraAttach::lib_view`. A tap here goes
//! back (`InteractState::lib_acts`, `CameraAttach::lib_release`) and is done
//! by the code egui's buttons run, next frame.
//!
//! **Placement.** As egui's. The prompt's bottom centre is the map area's
//! centre, `PROMPT_BOTTOM_MARGIN` above its bottom: a band centred there,
//! listening nowhere, holds the frame. Under the panels and cards, as egui's
//! `Order::Background` area is (`z-index: -1`, as `lib_chrome`). The chip
//! is centred over the top edge, `dock::reserved_top` down.

use std::cell::RefCell;
use std::rc::Rc;

use bevy::ecs::system::NonSendMarker;
use bevy::prelude::*;
// The library's, not Bevy's: an explicit import outranks the two globs.
use mithril::prelude::*;
use mithril::prelude::{Button, Event};

use crate::systems::camera_attach::CameraAttach;
use crate::systems::interact::{InteractState, PromptAct, PromptItem, PromptView};
use crate::systems::lib_place_card::icon;
use crate::systems::lib_settings::LibScreens;
use crate::systems::theme;
use crate::systems::ui_input::UiMetrics;

/// The prompt's name in the library's host.
const PROMPT_KEY: &str = "interact_prompt";

/// The chip's name in the library's host.
const CHIP_KEY: &str = "attach_chip";

/// egui's `overlay_frame`, as a style: the surface, its rim, 14 × 12 inside.
const FRAME: &str = "box-sizing: border-box; padding: 12px 14px; \
     background: var(--m-surface); border: 1px solid var(--m-border); \
     border-radius: var(--m-radius-lg); box-shadow: var(--m-shadow)";

/// The library's px per egui point: 1 on the Bevy backend, the UI scale as
/// DOM (`lib_place_card::Placement::over_the_dock` says why).
fn px_per_point(ui_scale: f32) -> f32 {
    if cfg!(all(feature = "ui-dom", target_arch = "wasm32")) {
        ui_scale
    } else {
        1.0
    }
}

// ─── The interaction prompt ─────────────────────────────────────────────────

/// The viewport the prompt is placed in.
#[derive(Clone, Copy, PartialEq, Default)]
struct Viewport {
    w: f32,
    h: f32,
    k: f32,
}

type PromptActs = Rc<RefCell<Vec<PromptAct>>>;

#[derive(Clone)]
struct PromptLive {
    view: Signal<PromptView>,
    viewport: Signal<Viewport>,
    acts: PromptActs,
}

thread_local! {
    static PROMPT: RefCell<Option<PromptLive>> = const { RefCell::new(None) };
    static CHIP: RefCell<Option<ChipLive>> = const { RefCell::new(None) };
}

/// Mounts, bridges and unmounts the interaction prompt. `PostUpdate`,
/// before `DriveUi`, as every library screen.
pub fn drive_lib_interact(
    _main: NonSendMarker,
    lib: Res<LibScreens>,
    mut state: ResMut<InteractState>,
    metrics: Res<UiMetrics>,
) {
    if !lib.interact {
        return;
    }
    if !state.lib {
        state.lib = true;
    }
    let size = metrics.viewport_pts();
    let viewport = Viewport {
        w: size.x,
        h: size.y,
        k: px_per_point(metrics.ui_scale()),
    };
    PROMPT.with(|m| {
        let mut m = m.borrow_mut();
        // The taps of the frame, for egui's system to do next frame.
        if let Some(live) = m.as_ref() {
            let acts: Vec<PromptAct> = std::mem::take(&mut *live.acts.borrow_mut());
            if !acts.is_empty() {
                state.lib_acts.extend(acts);
            }
        }
        let Some(view) = state.lib_view.clone() else {
            if m.take().is_some() {
                crate::systems::lib_host::clear_keyed(PROMPT_KEY);
            }
            return;
        };
        match m.as_mut() {
            None => {
                let live = PromptLive {
                    view: signal(view),
                    viewport: signal(viewport),
                    acts: Rc::new(RefCell::new(Vec::new())),
                };
                let p = theme::p();
                crate::systems::lib_host::mount_keyed(
                    PROMPT_KEY,
                    prompt_view(&live),
                    p.ink == theme::DARK.ink,
                    theme::touch_layout(),
                );
                if let Some(path) = std::env::var_os("CARTO_UI_LIB_HTML") {
                    if let Err(e) = std::fs::write(&path, html(prompt_view(&live))) {
                        warn!(?path, error = %e, "lib interact: could not write the HTML");
                    }
                }
                *m = Some(live);
            }
            Some(live) => {
                if live.view.peek(|now| *now != view) {
                    live.view.set(view);
                }
                if live.viewport.get_untracked() != viewport {
                    live.viewport.set(viewport);
                }
            }
        }
    });
}

/// The HTML from the library's string renderer, for the browser beside a
/// capture.
fn html(v: View) -> String {
    use mithril::view::{mount, StringRenderer};
    let r = Rc::new(RefCell::new(StringRenderer::new()));
    let root = r.borrow().root();
    let mounted = mount(v, &r, &root, None);
    let out = r.borrow().html();
    drop(mounted);
    out
}

/// The prompt: a band centred on the map's bottom centre, listening
/// nowhere, and in it the frame with the controls, or the receipt.
fn prompt_view(l: &PromptLive) -> View {
    let (view, viewport, acts) = (l.view.clone(), l.viewport.clone(), l.acts.clone());
    let body = move || -> View {
        let v = view.get();
        let vp = viewport.get();
        // Wide enough each side of the centre for the frame, wherever the
        // map's centre is: the band may run off one edge, the frame does not.
        let half = v.x.max(vp.w - v.x).max(0.0);
        let band = format!(
            "position: absolute; left: {:.0}px; bottom: {:.0}px; width: {:.0}px; \
             display: flex; justify-content: center; pointer-events: none",
            (v.x - half) * vp.k,
            (vp.h - v.y).max(0.0) * vp.k,
            2.0 * half * vp.k
        );
        if v.items.is_empty() {
            // A receipt, not a control: it does not listen.
            let Some(last) = v.receipt.clone() else {
                return View::Empty;
            };
            let frame = format!(
                "{}; color: var(--m-text); font-size: {}px; font-weight: 600; white-space: nowrap",
                FRAME,
                theme::SIZE_BODY
            );
            return view! {
                <div style={band}>
                    <div style={frame}>{last}</div>
                </div>
            };
        }
        let controls: Vec<View> = v
            .items
            .iter()
            .map(|item| match item {
                PromptItem::Button { glyph, label, act } => {
                    let tap = tap(&acts, *act);
                    let glyph: &'static str = *glyph;
                    let glyph: View = if glyph.is_empty() {
                        View::Empty
                    } else {
                        icon(glyph)
                    };
                    let label = label.clone();
                    view! {
                        <Button variant=Variant::Secondary icon={glyph} onclick={Some(tap)}>{label}</Button>
                    }
                }
                PromptItem::Icon { glyph, hint, act } => {
                    let tap = tap(&acts, *act);
                    let (glyph, hint): (&'static str, &'static str) = (*glyph, *hint);
                    view! {
                        <Button variant=Variant::Ghost size=Size::Sm icon={icon(glyph)} icon_only=true onclick={Some(tap)} title={hint} />
                    }
                }
            })
            .collect();
        let controls = View::Fragment(controls);
        let frame = format!(
            "{}; pointer-events: auto; display: flex; align-items: center; gap: 8px",
            FRAME
        );
        // The frame's own node listens (a click that does nothing), so the
        // pointer over its padding is the library's too.
        view! {
            <div style={band}>
                <div style={frame} on:click={|_: &Event| {}}>{controls}</div>
            </div>
        }
    };
    view! {
        <div style="position: absolute; left: 0; top: 0; right: 0; bottom: 0; z-index: -1; pointer-events: none">
            {body}
        </div>
    }
}

/// A tap that hands `a` back.
fn tap(acts: &PromptActs, a: PromptAct) -> Handler {
    let acts = acts.clone();
    Rc::new(move |_: &Event| acts.borrow_mut().push(a)) as Handler
}

// ─── The camera's chip ──────────────────────────────────────────────────────

/// Where the chip's band stands, px in the library's root.
#[derive(Clone, Copy, PartialEq, Default)]
struct ChipPlacement {
    top: f32,
    width: f32,
}

#[derive(Clone)]
struct ChipLive {
    label: Signal<String>,
    placement: Signal<ChipPlacement>,
    release: Rc<RefCell<bool>>,
}

/// Mounts, bridges and unmounts the "Following X" chip. `PostUpdate`,
/// before `DriveUi`, as every library screen.
pub fn drive_lib_attach_chip(
    _main: NonSendMarker,
    lib: Res<LibScreens>,
    mut attach: ResMut<CameraAttach>,
    metrics: Res<UiMetrics>,
) {
    if !lib.attach_chip {
        return;
    }
    if !attach.lib {
        attach.lib = true;
    }
    let k = px_per_point(metrics.ui_scale());
    let placement = ChipPlacement {
        top: crate::systems::dock::reserved_top() * k,
        width: metrics.viewport_pts().x * k,
    };
    CHIP.with(|m| {
        let mut m = m.borrow_mut();
        // The ✕ of the frame, for egui's system to do next frame.
        if let Some(live) = m.as_ref() {
            if std::mem::take(&mut *live.release.borrow_mut()) {
                attach.lib_release = true;
            }
        }
        let Some(label) = attach.lib_view.clone() else {
            if m.take().is_some() {
                crate::systems::lib_host::clear_keyed(CHIP_KEY);
            }
            return;
        };
        match m.as_mut() {
            None => {
                let live = ChipLive {
                    label: signal(label),
                    placement: signal(placement),
                    release: Rc::new(RefCell::new(false)),
                };
                let p = theme::p();
                crate::systems::lib_host::mount_keyed(
                    CHIP_KEY,
                    chip_view(&live),
                    p.ink == theme::DARK.ink,
                    theme::touch_layout(),
                );
                *m = Some(live);
            }
            Some(live) => {
                if live.label.peek(|now| *now != label) {
                    live.label.set(label);
                }
                if live.placement.get_untracked() != placement {
                    live.placement.set(placement);
                }
            }
        }
    });
}

/// The chip: "Following", the name, and ✕ to let go — egui's popup frame,
/// centred over the top edge.
fn chip_view(l: &ChipLive) -> View {
    let band = {
        let placement = l.placement.clone();
        move || {
            let p = placement.get();
            format!(
                "position: absolute; left: 0; top: {:.0}px; width: {:.0}px; \
                 display: flex; justify-content: center; pointer-events: none",
                p.top, p.width
            )
        }
    };
    let name = {
        let label = l.label.clone();
        move || label.get()
    };
    let release = {
        let flag = l.release.clone();
        Some(Rc::new(move |_: &Event| *flag.borrow_mut() = true) as Handler)
    };
    let following = format!(
        "flex: none; color: var(--m-text-dim); font-size: {}px",
        theme::SIZE_SUB
    );
    let strong = format!(
        "min-width: 0; overflow: hidden; white-space: nowrap; text-overflow: ellipsis; \
         color: var(--m-text-strong); font-size: {}px; font-weight: 600",
        theme::SIZE_BODY
    );
    // The chip's own node listens (a click that does nothing), so the
    // pointer over its padding is the library's too.
    view! {
        <div style={band}>
            <div on:click={|_: &Event| {}} style="pointer-events: auto; display: flex; align-items: center; gap: 8px; padding: 6px 8px 6px 14px; background: var(--m-surface); border: 1px solid var(--m-border); border-radius: var(--m-radius-lg); box-shadow: var(--m-shadow-lg)">
                <span style={following}>"Following"</span>
                <span style={strong}>{name}</span>
                <span style="flex: none; width: 6px"></span>
                <CloseButton onclick={release} label="Let go (Esc)" />
            </div>
        </div>
    }
}

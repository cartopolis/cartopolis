//! The ring of actions drawn by the component library (`lib_host`): round
//! buttons around what was tapped, the primary one in brand orange at the
//! top, ✕ in the middle, and a pill underneath saying what was tapped. This
//! module draws the map's ring (`ui::drive_context_menu`: a point, an object,
//! a place's name); `lib_satellite_menu` draws the satellite's through the
//! same [`RingMount`].
//!
//! The caller keeps every decision — which actions, where the ring stands
//! (`action_ring::clamp_centre`), and whether a press off it or Escape closes
//! it (`action_ring::run`) — and publishes the open ring in `RingLib::view`.
//! A tap here goes back in `RingLib::acts`, tagged with the ring it was on,
//! and is acted on by the caller next frame.
//!
//! **Built from boxes**: the orbit a bordered circle, each button a circle
//! with its icon and word (6 above and 12 below the button's centre), the ✕
//! two thin boxes turned ±45°, the pill centred in a band under the ring that
//! does not listen. The ring's colours are the theme's (`theme::p()`,
//! `theme::BRAND`, `theme::LABEL_CHIP`), set as custom properties on the root
//! when it mounts.
//!
//! The library has no hover, so a button's hint is its `title` and its fill
//! does not change under the pointer; a ring does not pop as it arrives.

use std::cell::RefCell;
use std::rc::Rc;

use bevy::ecs::system::NonSendMarker;
use bevy::prelude::{Commands, Res, ResMut};
// The library's, not Bevy's.
use mithril::prelude::Event;
use mithril::prelude::*;

use crate::systems::action_ring::{
    self, MapRingLib, RingId, RingLib, RingOutcome, RingView, BUTTON_R, CLOSE_R, PILL_GAP, RADIUS,
};
use crate::systems::lib_pins::{boxed, css, px_scale};
use crate::systems::lib_settings::LibScreens;
use crate::systems::theme;
use crate::systems::ui_input::UiMetrics;

/// The map's ring's name in the library's host.
const KEY: &str = "action_ring";

/// The arm of the ✕, from its centre to a tip along each axis, points.
const CROSS_K: f32 = 5.5;
/// The ✕'s stroke, points.
const CROSS_W: f32 = 1.8;

type Acts = Rc<RefCell<Vec<(RingId, RingOutcome)>>>;

/// One ring mounted in the library's host: what it was mounted for, the ring
/// it shows, and its taps.
pub(crate) struct RingMount {
    dark: bool,
    k: f32,
    view: Signal<RingView>,
    acts: Acts,
}

thread_local! {
    static MOUNTED: RefCell<Option<RingMount>> = const { RefCell::new(None) };
}

/// Mounts, bridges and unmounts the map's ring. `PostUpdate`, before
/// `DriveUi`, as every library screen; the bridge is inserted here, the
/// first frame.
pub fn drive_lib_action_ring(
    _main: NonSendMarker,
    lib: Res<LibScreens>,
    ring: Option<ResMut<MapRingLib>>,
    metrics: bevy::prelude::Res<UiMetrics>,
    mut commands: Commands,
) {
    if !lib.action_ring {
        return;
    }
    let Some(mut ring) = ring else {
        commands.insert_resource(MapRingLib(RingLib::default()));
        return;
    };
    let k = px_scale(metrics.ui_scale());
    MOUNTED.with(|m| sync_ring(m, KEY, &mut ring.0, k));
}

/// Mirror `bridge` into the ring mounted under `key`: hand its taps back,
/// mount it when a ring opens (or the theme or scale changed), update it
/// when the ring changed, take it down when it closed.
pub(crate) fn sync_ring(
    slot: &RefCell<Option<RingMount>>,
    key: &str,
    bridge: &mut RingLib,
    k: f32,
) {
    let mut m = slot.borrow_mut();
    // The taps of the frame, for the caller to do next frame.
    if let Some(live) = m.as_ref() {
        let acts: Vec<(RingId, RingOutcome)> = std::mem::take(&mut *live.acts.borrow_mut());
        if !acts.is_empty() {
            bridge.acts.extend(acts);
        }
    }
    let dark = theme::p().ink == theme::DARK.ink;
    let stale = m
        .as_ref()
        .is_some_and(|l| bridge.view.is_none() || l.dark != dark || l.k != k);
    if stale {
        *m = None;
        crate::systems::lib_host::clear_keyed(key);
    }
    let Some(view) = bridge.view.clone() else {
        return;
    };
    match m.as_ref() {
        None => {
            let live = RingMount {
                dark,
                k,
                view: signal(view),
                acts: Rc::new(RefCell::new(Vec::new())),
            };
            crate::systems::lib_host::mount_keyed(
                key,
                ring_root(&live),
                dark,
                theme::touch_layout(),
            );
            write_html(&live);
            *m = Some(live);
        }
        Some(live) => {
            if live.view.peek(|now| *now != view) {
                live.view.set(view);
            }
        }
    }
}

/// `CARTO_UI_LIB_HTML=<path>`: the ring as HTML, for the browser beside a
/// capture.
fn write_html(l: &RingMount) {
    let Some(path) = std::env::var_os("CARTO_UI_LIB_HTML") else {
        return;
    };
    use mithril::view::{mount, StringRenderer};
    let r = Rc::new(RefCell::new(StringRenderer::new()));
    let root = r.borrow().root();
    let mounted = mount(ring_root(l), &r, &root, None);
    let out = r.borrow().html();
    drop(mounted);
    if let Err(e) = std::fs::write(&path, out) {
        bevy::log::warn!(?path, error = %e, "lib action ring: could not write the HTML");
    }
}

/// egui's ring colours as custom properties, read when the ring mounts (a
/// new theme mounts it again).
fn ring_vars() -> String {
    let p = theme::p();
    format!(
        "--ring-brand: {}; --ring-on-brand: {}; --ring-glass: {}; --ring-line: {}; \
         --ring-ink: {}; --ring-cream: {}; --ring-dim: {}; --ring-orbit: {}; \
         --ring-chip: {}; --ring-cross: {}",
        css(theme::BRAND),
        css(theme::Rgba8::WHITE),
        css(p.glass),
        css(p.overlay_line),
        css(p.text),
        css(p.cream),
        css(p.text_dim),
        // The orbit's faint white, as `action_ring::show` strokes it.
        css(theme::Rgba8::WHITE.with_alpha(40)),
        css(theme::LABEL_CHIP),
        css(theme::DARK.text_dim),
    )
}

/// The root: the whole window, listening nowhere, the ring placed in it.
fn ring_root(l: &RingMount) -> View {
    let (view, acts, k) = (l.view.clone(), l.acts.clone(), l.k);
    let root = format!(
        "position: absolute; left: 0; top: 0; right: 0; bottom: 0; pointer-events: none; {}",
        ring_vars()
    );
    el("div")
        .attr("style", root)
        .child(move || ring(&view.get(), k, &acts))
        .into_view()
}

/// `v` points as the library's px.
fn px(v: f32, k: f32) -> f32 {
    v * k
}

/// A tap that hands `o` back for the ring `id`.
fn tap(acts: &Acts, id: RingId, o: RingOutcome) -> impl Fn(&Event) + 'static {
    let acts = acts.clone();
    move |_: &Event| acts.borrow_mut().push((id, o))
}

/// One ring: a 0 × 0 node at its centre, everything placed around it.
fn ring(v: &RingView, k: f32, acts: &Acts) -> View {
    let anchor = format!(
        "position: absolute; left: {:.1}px; top: {:.1}px; width: 0; height: 0",
        px(v.centre.x, k),
        px(v.centre.y, k)
    );
    // The orbit: a 1-point line on the radius, as egui's `circle_stroke`.
    let orbit_r = RADIUS + 0.5;
    let orbit = boxed(
        px(-orbit_r, k),
        px(-orbit_r, k),
        px(2.0 * orbit_r, k),
        px(2.0 * orbit_r, k),
        &format!(
            "box-sizing: border-box; border-radius: 50%; border: {:.1}px solid var(--ring-orbit)",
            px(1.0, k)
        ),
    );
    let buttons: Vec<View> = v
        .actions
        .iter()
        .enumerate()
        .map(|(i, a)| {
            let p = action_ring::slot(i, v.actions.len());
            button(
                p.x,
                p.y,
                a.icon,
                a.label,
                a.hint,
                i == 0,
                k,
                tap(acts, v.id, RingOutcome::Chosen(a.key)),
            )
        })
        .collect();
    el("div")
        .attr("style", anchor)
        .child(el("div").attr("style", orbit))
        .children(buttons)
        .child(close(k, tap(acts, v.id, RingOutcome::Dismissed)))
        .child(pill(v, k))
        .into_view()
}

/// A button centred `(x, y)` points from the ring's centre: the disc, the
/// icon 6 points above its centre and the word 12 below, as egui sets them.
#[allow(clippy::too_many_arguments)]
fn button(
    x: f32,
    y: f32,
    icon: &'static str,
    label: &'static str,
    hint: &'static str,
    primary: bool,
    k: f32,
    on: impl Fn(&Event) + 'static,
) -> View {
    let (fill, ink) = if primary {
        ("var(--ring-brand)", "var(--ring-on-brand)")
    } else {
        ("var(--ring-glass)", "var(--ring-ink)")
    };
    let d = 2.0 * BUTTON_R;
    let disc = boxed(
        px(x - BUTTON_R, k),
        px(y - BUTTON_R, k),
        px(d, k),
        px(d, k),
        &format!(
            "box-sizing: border-box; border-radius: 50%; border: {:.1}px solid var(--ring-line); \
             background: {fill}; color: {ink}; cursor: pointer; pointer-events: auto",
            px(1.0, k)
        ),
    );
    // Placed in the disc's padding box, which starts inside its 1-point rim.
    let rim = 1.0;
    let (icon_size, word_size) = (19.0, 9.5);
    let icon_box = boxed(
        px(-rim, k),
        px(BUTTON_R - 6.0 - icon_size / 2.0 - rim, k),
        px(d, k),
        px(icon_size, k),
        &format!(
            "display: flex; align-items: center; justify-content: center; \
             font-family: 'Material Symbols Rounded'; font-size: {:.1}px; line-height: 1",
            px(icon_size, k)
        ),
    );
    let word_h = 12.0;
    let word_box = boxed(
        px(-rim, k),
        px(BUTTON_R + 12.0 - word_h / 2.0 - rim, k),
        px(d, k),
        px(word_h, k),
        &format!(
            "display: flex; align-items: center; justify-content: center; \
             font-size: {:.2}px; line-height: 1; white-space: nowrap",
            px(word_size, k)
        ),
    );
    el("div")
        .attr("style", disc)
        .attr("title", hint)
        .attr("aria-label", label)
        .on("click", on)
        .child(el("div").attr("style", icon_box).child(icon))
        .child(el("div").attr("style", word_box).child(label))
        .into_view()
}

/// ✕ in the middle: the dark chip, and two strokes turned ±45°.
fn close(k: f32, on: impl Fn(&Event) + 'static) -> View {
    let d = 2.0 * CLOSE_R;
    let chip = boxed(
        px(-CLOSE_R, k),
        px(-CLOSE_R, k),
        px(d, k),
        px(d, k),
        "border-radius: 50%; background: var(--ring-chip); cursor: pointer; pointer-events: auto",
    );
    // Each stroke runs tip to tip across the chip's centre.
    let len = 2.0 * CROSS_K * std::f32::consts::SQRT_2;
    let stroke = |deg: f32| {
        boxed(
            px(CLOSE_R - len / 2.0, k),
            px(CLOSE_R - CROSS_W / 2.0, k),
            px(len, k),
            px(CROSS_W, k),
            &format!(
                "border-radius: {:.1}px; background: var(--ring-cross); transform: rotate({deg:.0}deg)",
                px(CROSS_W / 2.0, k)
            ),
        )
    };
    el("div")
        .attr("style", chip)
        .attr("title", "Close")
        .attr("aria-label", "Close")
        .on("click", on)
        .child(el("div").attr("style", stroke(45.0)))
        .child(el("div").attr("style", stroke(-45.0)))
        .into_view()
}

/// The pill under the ring: what it is about, and a line of detail. Centred
/// on the ring in a band wide enough to stay on screen, which listens
/// nowhere, so a press on it is a press off the ring, as on egui's.
fn pill(v: &RingView, k: f32) -> View {
    let half = v.centre.x.max(v.screen_w - v.centre.x).max(0.0);
    let band = boxed(
        px(-half, k),
        px(RADIUS + BUTTON_R + PILL_GAP, k),
        px(2.0 * half, k),
        0.0,
        // 0 tall, the pill hanging out of it: not stretched to the band.
        "display: flex; justify-content: center; align-items: flex-start; pointer-events: none",
    );
    // egui's pill: 12 across and 6 down round the words, its 1-point rim
    // inside that.
    let frame = format!(
        "flex: none; display: flex; flex-direction: column; align-items: center; \
         box-sizing: border-box; padding: {:.1}px {:.1}px; border-radius: {:.1}px; \
         background: var(--ring-glass); border: {:.1}px solid var(--ring-line); white-space: nowrap",
        px(5.0, k),
        px(11.0, k),
        px(12.0, k),
        px(1.0, k)
    );
    let title = format!(
        "color: var(--ring-cream); font-size: {:.1}px",
        px(theme::SIZE_BODY, k)
    );
    let detail = (!v.detail.is_empty()).then(|| {
        let style = format!(
            "color: var(--ring-dim); font-size: {:.1}px",
            px(theme::SIZE_CHIP, k)
        );
        el("span").attr("style", style).child(v.detail.clone())
    });
    el("div")
        .attr("style", band)
        .child(
            el("div")
                .attr("style", frame)
                .child(el("span").attr("style", title).child(v.title.clone()))
                .child(detail),
        )
        .into_view()
}

//! The map's chrome drawn by the component library (`lib_host`), beside
//! egui's: the World | Community switch and the search bar with your face
//! (`dock`), the round buttons down both edges (`dock`), the compass, the
//! locate and light discs and the scale bar (`hud`), and the floor picker
//! (`floor_picker`). One module because they share one bridge
//! (`dock::ChromeLib`), one placement arithmetic (`dock::right_column`,
//! `left_slot`, `reserved_top`/`reserved_bottom`) and one disc.
//!
//! On with `CARTO_UI_LIB=chrome` (or `1`), on the web `?ui=lib`. egui's
//! systems step aside (`MapChrome::lib.on`) but keep every decision — what
//! shows (a panel, the keyboard, driving, the camera, the design room), where
//! (the map's rect, lying or standing), in which state — and publish it each
//! frame in `MapChrome::lib`; they also keep the work only they can do
//! (`place_pins::set_chrome_holes`, `ui::MainMenu::face`, `MapChrome::
//! right_bottom`), so `dock::reserved_top`/`reserved_bottom` and the slots
//! other screens read are the same numbers in both editions. A tap here is a
//! `dock::ChromeAct`, handed back and acted on next frame by the same code
//! egui's buttons run.
//!
//! **Drawn from boxes.** What egui paints — the discs, the compass needle,
//! the locate arrow, the light icons, the scale bar — is built from nodes:
//! circles by a full radius, lines by thin boxes, the needle turned by
//! `transform: rotate()` (`UiTransform` on the Bevy backend), the locate
//! disc's searching arc a bordered circle with one coloured side turning on
//! `m-spin`.
//!
//! **Placement.** Every piece is absolutely placed in one full-window root
//! that does not listen (on the web, `pointer-events: none`), each control
//! listening on its own node, so a world click under the chrome yields
//! (`lib_host::bridge_input`) and one beside it reaches the map.

use std::cell::RefCell;
use std::rc::Rc;

use bevy::ecs::system::NonSendMarker;
use bevy::prelude::{Res, ResMut};
// The library's, not Bevy's.
use mithril::prelude::*;
use mithril::prelude::{Button, Event};
use mithril::view::El;

use crate::resources::{RightPanel, SunMode};
use crate::systems::discs::{disc_look, ChatKind};
use crate::systems::dock::{
    BarView, ChromeAct, FaceView, LeftView, MapChrome, RightView, Space, SwitchView, FACE,
    MODE_WORD_H, SEARCH_H, SWITCH_H,
};
use crate::systems::egui_manager::EguiRef;
use crate::systems::floor_picker::FloorsView;
use crate::systems::hud::{
    light_hint, CompassView, LightView, LocateTone, LocateView, ScaleView, LIGHTS,
};
use crate::systems::lib_settings::LibScreens;
use crate::systems::theme::tokens::hex;
use crate::systems::{icons, theme};

/// The chrome's name in the library's host.
const KEY: &str = "chrome";

/// A disc's diameter: the one disc the map wears (`theme::ROUND_BUTTON_SIZE`).
const DISC: f32 = theme::ROUND_BUTTON_SIZE;

type Acts = Rc<RefCell<Vec<ChromeAct>>>;

/// What is mounted: one signal per piece, and what it was mounted for.
struct Live {
    dark: bool,
    /// The library's px per egui point: 1 on the Bevy backend, the UI scale
    /// as DOM (`lib_place_card::Placement::over_the_dock` says why).
    k: f32,
    acts: Acts,
    switch: Signal<Option<SwitchView>>,
    bar: Signal<Option<BarView>>,
    right: Signal<Option<RightView>>,
    left: Signal<Option<LeftView>>,
    /// The compass with its heading at zero: a turn of the view moves only
    /// [`Live::heading`], not the rose's nodes.
    compass: Signal<Option<CompassView>>,
    heading: Signal<f32>,
    locate: Signal<Option<LocateView>>,
    light: Signal<Option<LightView>>,
    scale: Signal<Option<ScaleView>>,
    floors: Signal<Option<FloorsView>>,
}

thread_local! {
    static MOUNTED: RefCell<Option<Live>> = const { RefCell::new(None) };
}

/// Write a signal only when the value moved.
fn put<T: Clone + PartialEq + 'static>(s: &Signal<T>, v: T) {
    if s.peek(|now| *now != v) {
        s.set(v);
    }
}

/// Mounts the chrome, bridges it and hands its taps back. `PostUpdate`,
/// before `DriveUi`, as every library screen.
pub fn drive_lib_chrome(
    _main: NonSendMarker,
    lib: Res<LibScreens>,
    mut chrome: ResMut<MapChrome>,
    egui: EguiRef,
) {
    if !lib.chrome {
        return;
    }
    if !chrome.lib.on {
        chrome.lib.on = true;
    }
    let dark = theme::p().ink == theme::DARK.ink;
    let k = if cfg!(all(feature = "ui-dom", target_arch = "wasm32")) {
        egui.ui_scale()
    } else {
        1.0
    };
    MOUNTED.with(|m| {
        let mut m = m.borrow_mut();
        // The taps of the frame, for egui's systems to act on next frame.
        if let Some(live) = m.as_ref() {
            let acts: Vec<ChromeAct> = std::mem::take(&mut *live.acts.borrow_mut());
            chrome.lib.acts.extend(acts);
        }
        // The light follows the sun (`ui_theme`): a new theme, or a new
        // scale, mounts the chrome again.
        if m.as_ref().is_some_and(|l| l.dark != dark || l.k != k) {
            *m = None;
            crate::systems::lib_host::clear_keyed(KEY);
        }
        let live = m.get_or_insert_with(|| {
            let live = Live {
                dark,
                k,
                acts: Rc::new(RefCell::new(Vec::new())),
                switch: signal(None),
                bar: signal(None),
                right: signal(None),
                left: signal(None),
                compass: signal(None),
                heading: signal(0.0),
                locate: signal(None),
                light: signal(None),
                scale: signal(None),
                floors: signal(None),
            };
            crate::systems::lib_host::mount_keyed(KEY, view_of(&live), dark, theme::touch_layout());
            live
        });
        let dock = &chrome.lib.dock;
        put(&live.switch, dock.switch.clone());
        put(&live.bar, dock.bar.clone());
        put(&live.right, dock.right.clone());
        put(&live.left, dock.left.clone());
        let hud = &chrome.lib.hud;
        if let Some(c) = &hud.compass {
            put(&live.heading, c.heading);
        }
        put(
            &live.compass,
            hud.compass.clone().map(|mut c| {
                c.heading = 0.0;
                c
            }),
        );
        put(&live.locate, hud.locate.clone());
        put(&live.light, hud.light.clone());
        put(&live.scale, hud.scale.clone());
        put(&live.floors, chrome.lib.floors.clone());
    });
}

/// `v` points as the library's px.
fn px(v: f32, k: f32) -> String {
    format!("{:.1}px", v * k)
}

/// A tap that hands `a` back.
fn tap(acts: &Acts, a: ChromeAct) -> impl Fn(&Event) + 'static {
    let acts = acts.clone();
    move |_: &Event| acts.borrow_mut().push(a)
}

/// The same, as a component's handler.
fn handler(acts: &Acts, a: ChromeAct) -> Handler {
    Rc::new(tap(acts, a)) as Handler
}

/// A glyph of the icon face at `size` px.
fn glyph(g: &'static str, size: f32) -> View {
    el("span")
        .attr(
            "style",
            format!(
                "font-family: 'Material Symbols Rounded'; font-size: {size:.1}px; line-height: 1"
            ),
        )
        .child(g)
        .into_view()
}

/// The root: the whole window, listening nowhere, each piece placed in it.
fn view_of(l: &Live) -> View {
    let k = l.k;
    let acts = l.acts.clone();
    // Under every other screen, as egui's panels and cards cover the map
    // controls: drawn level with them, the scale bar lay over the person
    // card and the chat chip.
    let root = format!(
        "position: absolute; left: 0; top: 0; right: 0; bottom: 0; z-index: -1; pointer-events: none; {}",
        theme::map_vars()
    );
    // Painted in this order, the floor picker last.
    el("div")
        .attr("style", root)
        .child({
            let s = l.scale.clone();
            move || s.get().map(|v| scale_view(&v, k))
        })
        .child({
            let (s, a) = (l.switch.clone(), acts.clone());
            move || s.get().map(|v| switch_view(&v, k, &a))
        })
        .child({
            let (s, a) = (l.bar.clone(), acts.clone());
            move || s.get().map(|v| bar_view(&v, k, &a))
        })
        .child({
            let (s, a) = (l.right.clone(), acts.clone());
            move || s.get().map(|v| right_view(&v, k, &a))
        })
        .child({
            let (s, a) = (l.left.clone(), acts.clone());
            move || s.get().map(|v| left_view(&v, k, &a))
        })
        .child({
            let (s, h, a) = (l.compass.clone(), l.heading.clone(), acts.clone());
            move || s.get().map(|v| compass_view(&v, &h, k, &a))
        })
        .child({
            let (s, a) = (l.locate.clone(), acts.clone());
            move || s.get().map(|v| locate_view(&v, k, &a))
        })
        .child({
            let (s, a) = (l.light.clone(), acts.clone());
            move || s.get().map(|v| light_view(&v, k, &a))
        })
        .child({
            let (s, a) = (l.floors.clone(), acts);
            move || s.get().map(|v| floors_view(&v, k, &a))
        })
        .into_view()
}

// ─── The disc ────────────────────────────────────────────────────────────────

/// A map disc's plate (`theme::map_disc`): the surface with its rim, or —
/// engaged — the accent with `ring`.
fn plate_style(size: f32, k: f32, on: bool, ring: &str) -> String {
    let (fill, rim, ink) = if on {
        ("var(--m-accent)", ring, "var(--m-on-accent)")
    } else {
        ("var(--map-fill)", "var(--map-rim)", "var(--map-ink)")
    };
    format!(
        "position: absolute; left: 0; top: 0; width: {s}; height: {s}; box-sizing: border-box; \
         display: flex; align-items: center; justify-content: center; \
         border-radius: var(--m-radius-full); border: 1.5px solid {rim}; background: {fill}; \
         color: {ink}; box-shadow: var(--m-shadow); cursor: pointer; pointer-events: auto",
        s = px(size, k)
    )
}

/// A spot `DISC` square (and `extra` taller, for words under it), its
/// right or left edge `x` in from the screen's.
fn spot(x: f32, top: f32, from_right: bool, extra: f32, k: f32) -> String {
    format!(
        "position: absolute; {side}: {x}; top: {top}; width: {s}; height: {h}",
        side = if from_right { "right" } else { "left" },
        x = px(x, k),
        top = px(top, k),
        s = px(DISC, k),
        h = px(DISC + extra, k),
    )
}

/// A round button with an icon (`theme::round_button`): a tap hands `act`
/// back.
fn round_button(
    icon: &'static str,
    hint: &str,
    on: bool,
    act: ChromeAct,
    k: f32,
    acts: &Acts,
) -> View {
    el("div")
        .attr("style", plate_style(DISC, k, on, "var(--m-accent)"))
        .attr("title", hint.to_owned())
        .attr("aria-label", hint.to_owned())
        .on("click", tap(acts, act))
        .child(glyph(icon, 17.0 * k))
        .into_view()
}

// ─── The dock ────────────────────────────────────────────────────────────────

/// The World | Community switch: a pill track, the chosen side the accent
/// plate, an unread count as a badge on Community.
fn switch_view(v: &SwitchView, k: f32, acts: &Acts) -> View {
    let frame = format!(
        "position: absolute; left: {}; bottom: {}; width: {}; height: {}; box-sizing: border-box; \
         padding: {p}; display: flex; gap: {g}; background: var(--map-fill); \
         border: 1px solid var(--map-rim); border-radius: var(--m-radius-full); \
         box-shadow: var(--m-shadow); pointer-events: auto",
        px(v.left, k),
        px(theme::OSD_MARGIN, k),
        px(v.width, k),
        px(SWITCH_H, k),
        p = px(4.0, k),
        g = px(2.0, k),
    );
    let sides = [
        (Space::World, icons::CAT_MAP, "World"),
        (Space::Community, icons::GROUP, "Community"),
    ];
    let segments: Vec<View> = sides
        .into_iter()
        .map(|(side, icon, label)| {
            let chosen = side == v.on;
            let style = format!(
                "flex: 1 1 0; min-width: 0; display: flex; align-items: center; justify-content: center; \
                 gap: {g}; border-radius: var(--m-radius-full); background: {bg}; color: {ink}; \
                 font-size: {fs}; font-weight: {fw}; white-space: nowrap; overflow: hidden; cursor: pointer",
                g = px(6.0, k),
                bg = if chosen { "var(--m-accent)" } else { "transparent" },
                ink = if chosen {
                    "var(--m-on-accent)"
                } else {
                    "var(--map-ink)"
                },
                fs = px(theme::SIZE_BODY + 0.5, k),
                fw = if chosen { 600 } else { 400 },
            );
            let badge: View = if side == Space::Community && v.unread > 0 {
                let n = if v.unread > 99 {
                    "99+".to_string()
                } else {
                    v.unread.to_string()
                };
                Badge {
                    tone: Tone::Warning,
                    children: n.into_view(),
                    ..Default::default()
                }
                .render()
            } else {
                View::Empty
            };
            el("div")
                .attr("style", style)
                .attr("title", label)
                .attr("aria-label", label)
                .on("click", tap(acts, ChromeAct::Space(side)))
                .child(glyph(icon, 18.0 * k))
                .child(el("span").child(label))
                .child(badge)
                .into_view()
        })
        .collect();
    el("div")
        .attr("style", frame)
        // The track's padding is the switch's too.
        .on("click", |_: &Event| {})
        .children(segments)
        .into_view()
}

/// Your face, `size` across: your initial on your colour signed in, a grey
/// person signed out; ringed in the accent while its sheet is open, an
/// orange dot while something waits. A picture shows as the initial (the
/// faces are egui textures, not data URLs yet).
fn face(f: &FaceView, size: f32, k: f32, acts: &Acts) -> View {
    let s = px(size, k);
    let ring = if f.open {
        format!("border: {} solid var(--m-accent); ", px(2.0, k))
    } else {
        String::new()
    };
    let (bg, ink, text, font_size, icon) = match &f.me {
        Some(name) => {
            let look = disc_look(name, ChatKind::Private);
            (
                hex(look.tone),
                "#ffffff".to_string(),
                look.text,
                size * look.scale,
                look.icon,
            )
        }
        None => (
            "var(--m-border)".to_string(),
            "var(--m-text-dim)".to_string(),
            icons::APPEARANCE.to_string(),
            size * 0.55,
            true,
        ),
    };
    let family = if icon {
        "font-family: 'Material Symbols Rounded'; "
    } else {
        ""
    };
    let disc = format!(
        "position: absolute; left: 0; top: 0; width: {s}; height: {s}; box-sizing: border-box; {ring}\
         display: flex; align-items: center; justify-content: center; border-radius: var(--m-radius-full); \
         background: {bg}; color: {ink}; {family}font-size: {}; font-weight: 600; line-height: 1",
        px(font_size, k)
    );
    let dot: View = if f.waiting {
        let r = size / 2.0;
        let d = 12.0;
        let style = format!(
            "position: absolute; left: {}; top: {}; width: {}; height: {}; box-sizing: border-box; \
             border-radius: var(--m-radius-full); border: {} solid var(--map-fill); background: {}",
            px(r + r * 0.72 - d / 2.0, k),
            px(r - r * 0.72 - d / 2.0, k),
            px(d, k),
            px(d, k),
            px(1.5, k),
            hex(theme::BRAND),
        );
        el("span").attr("style", style).into_view()
    } else {
        View::Empty
    };
    el("div")
        .attr(
            "style",
            format!("position: relative; flex: none; width: {s}; height: {s}; cursor: pointer"),
        )
        .attr("title", "You: your profile, settings and more")
        .attr("aria-label", "You")
        .on("click", tap(acts, ChromeAct::Face))
        .child(el("span").attr("style", disc).child(text))
        .child(dot)
        .into_view()
}

/// The search pill: a magnifier and "Search places", your face at its end —
/// or, after Show in world, the way back with your face beside it.
fn bar_view(v: &BarView, k: f32, acts: &Acts) -> View {
    let frame = format!(
        "position: absolute; right: {}; top: {}; width: {}; height: {}; display: flex; \
         align-items: center; gap: {}; pointer-events: auto",
        px(v.right, k),
        px(theme::OSD_MARGIN, k),
        px(v.width, k),
        px(SEARCH_H, k),
        px(8.0, k),
    );
    let pill = |border: String| {
        format!(
            "flex: 1 1 0; min-width: 0; height: 100%; box-sizing: border-box; display: flex; \
             align-items: center; gap: {}; padding: 0 {} 0 {}; background: var(--map-fill); \
             border: {border}; border-radius: var(--m-radius-full); box-shadow: var(--m-shadow)",
            px(10.0, k),
            px(4.0, k),
            px(16.0, k),
        )
    };
    if let Some(back) = &v.back {
        let chip = el("div")
            .attr("style", pill(format!("{} solid var(--map-rim)", px(1.0, k))))
            .attr("title", back.clone())
            .on("click", tap(acts, ChromeAct::Back))
            .child(el("span").attr("style", "color: var(--map-ink)").child(glyph(icons::BACK, 19.0 * k)))
            .child(
                el("span")
                    .attr(
                        "style",
                        format!(
                            "flex: 1 1 auto; min-width: 0; overflow: hidden; white-space: nowrap; \
                             text-overflow: ellipsis; color: var(--m-text-strong); font-size: {}; font-weight: 600",
                            px(theme::SIZE_BODY, k)
                        ),
                    )
                    .child(back.clone()),
            );
        return el("div")
            .attr("style", frame)
            .on("click", |_: &Event| {})
            .child(chip)
            .child(v.face.as_ref().map(|f| face(f, SEARCH_H, k, acts)))
            .into_view();
    }
    let border = if v.open {
        format!("{} solid var(--m-accent)", px(1.5, k))
    } else {
        format!("{} solid var(--map-rim)", px(1.0, k))
    };
    // The field and the face side by side in the pill, so a tap on the face
    // is not also a tap on the field.
    let field = el("div")
        .attr(
            "style",
            "flex: 1 1 auto; min-width: 0; height: 100%; display: flex; align-items: center; cursor: pointer",
        )
        .attr("title", "Search places (Ctrl+L)")
        .attr("aria-label", "Search places")
        .on("click", tap(acts, ChromeAct::Search))
        .child(
            el("span")
                .attr(
                    "style",
                    format!(
                        "flex: none; margin-right: {}; color: var(--map-ink)",
                        px(10.0, k)
                    ),
                )
                .child(glyph(icons::SEARCH, 19.0 * k)),
        )
        .child(
            el("span")
                .attr(
                    "style",
                    format!(
                        "flex: 1 1 auto; min-width: 0; overflow: hidden; white-space: nowrap; \
                         text-overflow: ellipsis; color: var(--m-text-dim); font-size: {}",
                        px(theme::SIZE_VALUE, k)
                    ),
                )
                .child("Search places"),
        );
    el("div")
        .attr("style", frame)
        .child(
            el("div")
                .attr("style", pill(border))
                .on("click", |_: &Event| {})
                .child(field)
                .child(v.face.as_ref().map(|f| face(f, FACE, k, acts))),
        )
        .into_view()
}

/// The right set: Layers, the view mode with its word under it, Camera.
fn right_view(v: &RightView, k: f32, acts: &Acts) -> View {
    let layers = el("div")
        .attr("style", spot(v.layers.x, v.layers.y, true, 0.0, k))
        .child(round_button(
            RightPanel::Layers.icon(),
            RightPanel::Layers.label(),
            false,
            ChromeAct::Layers,
            k,
            acts,
        ));
    let word = format!(
        "position: absolute; left: {}; top: {}; width: {}; display: flex; justify-content: center; \
         white-space: nowrap; color: var(--map-ink); font-size: {}; font-weight: 600; line-height: 1.2",
        px(-DISC / 2.0, k),
        px(DISC + 1.0, k),
        px(DISC * 2.0, k),
        px(theme::SIZE_SUB, k),
    );
    let mode = el("div")
        .attr("style", spot(v.mode.x, v.mode.y, true, MODE_WORD_H, k))
        .child(round_button(
            v.mode_icon,
            &v.mode_hint,
            false,
            ChromeAct::Mode,
            k,
            acts,
        ))
        .child(el("span").attr("style", word).child(v.mode_word));
    let camera = v.camera.map(|at| {
        el("div")
            .attr("style", spot(at.x, at.y, true, 0.0, k))
            .child(round_button(
                icons::CAMERA,
                "Camera",
                v.camera_on,
                ChromeAct::Camera,
                k,
                acts,
            ))
            .into_view()
    });
    fragment([layers.into_view(), mode.into_view(), camera.into_view()])
}

/// The left set this module draws: the lens and Say.
fn left_view(v: &LeftView, k: f32, acts: &Acts) -> View {
    let lens = el("div")
        .attr("style", spot(v.lens.x, v.lens.y, false, 0.0, k))
        .child(round_button(
            icons::POLL,
            "Lens: figures about places",
            v.lens_on,
            ChromeAct::Lens,
            k,
            acts,
        ));
    let say = v.say.map(|at| {
        el("div")
            .attr("style", spot(at.x, at.y, false, 0.0, k))
            .child(round_button(
                icons::CHAT,
                "Say something to the people near you",
                v.say_on,
                ChromeAct::Say,
                k,
                acts,
            ))
            .into_view()
    });
    fragment([lens.into_view(), say.into_view()])
}

// ─── The view controls ───────────────────────────────────────────────────────

/// A box at (`x`, `y`) in its parent, `w` × `h`, in `bg`, rounded by `radius`.
fn bar_box(x: f32, y: f32, w: f32, h: f32, bg: &str, radius: f32, k: f32) -> View {
    el("span")
        .attr(
            "style",
            format!(
                "position: absolute; left: {}; top: {}; width: {}; height: {}; background: {bg}; \
                 border-radius: {}",
                px(x, k),
                px(y, k),
                px(w, k),
                px(h, k),
                px(radius, k),
            ),
        )
        .into_view()
}

/// A layer the size of `s` over its parent, turned `deg` about its middle.
fn turned(s: f32, deg: f32, k: f32) -> El {
    el("span").attr(
        "style",
        format!(
            "position: absolute; left: 0; top: 0; width: {sz}; height: {sz}; transform: rotate({deg:.1}deg)",
            sz = px(s, k)
        ),
    )
}

/// The compass rose: the needle (north the accent, south the body tone) and
/// the "N" over its tip, turned with the view; held north-up, a heavier ring
/// and a padlock.
fn compass_view(v: &CompassView, heading: &Signal<f32>, k: f32, acts: &Acts) -> View {
    let r = DISC / 2.0 - 1.0;
    let c = DISC / 2.0;
    let light = theme::is_light();
    let (north, south, n_ink) = if light {
        (
            "var(--m-accent-text)",
            "var(--m-text-faint)",
            "var(--m-on-accent)",
        )
    } else {
        ("var(--m-accent)", "var(--m-text-dim)", "var(--map-ink)")
    };
    // Not the engaged plate even when locked: the needle needs the surface
    // (`hud::compass_overlay`). Held north-up is a heavier ring.
    let plate = el("div").attr("style", {
        let mut s = plate_style(DISC, k, false, "var(--m-accent)");
        if v.locked {
            s.push_str(&format!(
                "; border: {} solid var(--m-accent-text)",
                px(2.0, k)
            ));
        }
        if !v.interactive {
            s.push_str("; cursor: default");
        }
        s
    });
    // The needle's two halves are triangles, as egui's polygons: a box of
    // borders alone, three sides clear and the fourth inked, meets its
    // neighbours on the diagonals as CSS's does (`bevy_ui`'s border shader
    // splits a box's edge by distance over width).
    let half = r * 0.24;
    let tri = |top: f32, h: f32, up: bool, ink: &str| -> View {
        let widths = if up {
            format!("0 {} {} {}", px(half, k), px(h, k), px(half, k))
        } else {
            format!("{} {} 0 {}", px(h, k), px(half, k), px(half, k))
        };
        el("span")
            .attr(
                "style",
                format!(
                    "position: absolute; left: {}; top: {}; width: {}; height: {}; box-sizing: border-box; \
                     border-style: solid; border-width: {widths}; border-color: transparent; \
                     border-{}-color: {ink}",
                    px(c - half, k),
                    px(top, k),
                    px(half * 2.0, k),
                    px(h, k),
                    if up { "bottom" } else { "top" },
                ),
            )
            .into_view()
    };
    let needle = {
        let h = heading.clone();
        el("span")
            .attr("style", move || {
                format!(
                    "position: absolute; left: 0; top: 0; width: {s}; height: {s}; transform: rotate({:.1}deg)",
                    -h.get(),
                    s = px(DISC, k)
                )
            })
            .child(tri(c - r * 0.86, r * 0.86, true, north))
            .child(tri(c, r * 0.64, false, south))
            .child({
                // The "N" rides the needle and stays upright.
                let h = heading.clone();
                let n = 12.0;
                el("span")
                    .attr("style", move || {
                        format!(
                            "position: absolute; left: {}; top: {}; width: {n}; height: {n}; display: flex; \
                             align-items: center; justify-content: center; color: {n_ink}; font-size: {}; \
                             font-weight: 600; line-height: 1; transform: rotate({:.1}deg)",
                            px(c - 6.0, k),
                            px(c - r * 0.47 - 6.0, k),
                            px(theme::SIZE_TINY, k),
                            h.get(),
                            n = px(n, k),
                        )
                    })
                    .child("N")
            })
    };
    let lock: View = if v.locked {
        el("span")
            .attr(
                "style",
                format!(
                    "position: absolute; left: {}; top: {}; width: {}; display: flex; justify-content: center; \
                     color: var(--m-accent-text)",
                    px(c - 6.0, k),
                    px(c + r - 13.0, k),
                    px(12.0, k),
                ),
            )
            .child(glyph(icons::LOCK, 10.0 * k))
            .into_view()
    } else {
        View::Empty
    };
    let caption: View = match v.caption {
        Some(t) => el("span")
            .attr(
                "style",
                format!(
                    "position: absolute; left: {}; top: {}; width: {}; display: flex; justify-content: center; \
                     white-space: nowrap; color: var(--m-text-dim); font-size: {}",
                    px(-DISC / 2.0, k),
                    px(DISC + 3.0, k),
                    px(DISC * 2.0, k),
                    px(theme::SIZE_TINY, k),
                ),
            )
            .child(t)
            .into_view(),
        None => View::Empty,
    };
    // The spot listens, so the needle and the padlock over the plate are the
    // rose's too.
    let spot = el("div")
        .attr(
            "style",
            format!(
                "position: absolute; left: {}; top: {}; width: {s}; height: {s}; pointer-events: auto",
                px(v.left, k),
                px(v.top, k),
                s = px(DISC, k)
            ),
        )
        .attr("aria-label", "Compass");
    let spot = if v.interactive {
        spot.attr("title", v.hover)
            .on("click", tap(acts, ChromeAct::Compass))
    } else {
        // A passive rose still holds the pointer, as egui's did.
        spot.on("click", |_: &Event| {})
    };
    spot.child(plate)
        .child(needle)
        .child(lock)
        .child(caption)
        .into_view()
}

/// The locate disc: the location arrow (filled once a fix is in hand), the
/// arc that travels round the ring while searching, the caption beside it
/// (under it in a lying row).
fn locate_view(v: &LocateView, k: f32, acts: &Acts) -> View {
    let tone = match v.tone {
        LocateTone::Ring => "var(--m-accent)",
        LocateTone::Bright => "var(--m-accent-text)",
        LocateTone::Warn => "var(--m-warning)",
    };
    let ink = if v.on {
        "var(--m-on-accent)"
    } else {
        "var(--m-accent-text)"
    };
    let behind = if v.on {
        "var(--m-accent)"
    } else {
        "var(--map-fill)"
    };
    let c = DISC / 2.0;
    // The arrow: a diamond, its lower half notched by a second one in the
    // plate's colour — a kite pointing up.
    let a = DISC * 0.30;
    let diamond = |cy: f32, fill: &str, border: Option<&str>| {
        let border = border.map_or(String::new(), |b| {
            format!("; box-sizing: border-box; border: {} solid {b}", px(1.6, k))
        });
        el("span")
            .attr(
                "style",
                format!(
                    "position: absolute; left: {}; top: {}; width: {a}; height: {a}; background: {fill}; \
                     border-radius: {}; transform: rotate(45deg){border}",
                    px(c - a / 2.0, k),
                    px(cy - a / 2.0, k),
                    px(1.0, k),
                    a = px(a, k),
                ),
            )
            .into_view()
    };
    let arrow = if v.filled {
        diamond(c - 1.0, ink, None)
    } else {
        diamond(c - 1.0, "transparent", Some(ink))
    };
    let notch = diamond(c - 1.0 + a * 0.75, behind, None);
    let sweep: View = if v.searching {
        el("span")
            .attr(
                "style",
                format!(
                    "position: absolute; left: 0; top: 0; width: {s}; height: {s}; box-sizing: border-box; \
                     border-radius: var(--m-radius-full); border: {} solid transparent; \
                     border-top-color: var(--m-accent); animation: m-spin 2.4s linear infinite",
                    px(2.2, k),
                    s = px(DISC, k)
                ),
            )
            .into_view()
    } else {
        View::Empty
    };
    let caption: View = match &v.caption {
        Some(t) => {
            let at = if v.lying {
                format!(
                    "left: {}; top: {}; width: {}; justify-content: center",
                    px(-DISC, k),
                    px(DISC + 2.0, k),
                    px(DISC * 3.0, k)
                )
            } else {
                format!(
                    "right: {}; top: {}; justify-content: flex-end",
                    px(DISC + 6.0, k),
                    px(c - 8.0, k)
                )
            };
            el("span")
                .attr(
                    "style",
                    format!(
                        "position: absolute; {at}; display: flex; white-space: nowrap; color: {tone}; \
                         font-size: {}; font-weight: 600; line-height: 1.3",
                        px(theme::SIZE_SUB, k)
                    ),
                )
                .child(t.clone())
                .into_view()
        }
        None => View::Empty,
    };
    // The spot listens, so the arrow over the plate is the button's too.
    el("div")
        .attr(
            "style",
            format!(
                "{}; pointer-events: auto; cursor: pointer",
                spot(v.right, v.top, true, 0.0, k)
            ),
        )
        .attr("title", v.hover.clone())
        .attr("aria-label", "Locate me")
        .on("click", tap(acts, ChromeAct::Locate))
        .child(el("div").attr("style", plate_style(DISC, k, v.on, tone)))
        .child(arrow)
        .child(notch)
        .child(sweep)
        .child(caption)
        .into_view()
}

/// A light's icon in a box `size` across, from boxes as egui paints it from
/// primitives (the icon font has no sun or moon): Auto a half-filled ring,
/// Light a sun, Dark a crescent, a set time a clock face. `behind` is what
/// the crescent is cut with.
fn light_icon(mode: SunMode, size: f32, ink: &str, behind: &str, k: f32) -> View {
    let r = size / 2.0;
    let c = r;
    let stroke = 1.6;
    let ring = |d: f32| {
        el("span")
            .attr(
                "style",
                format!(
                    "position: absolute; left: {}; top: {}; width: {d}; height: {d}; box-sizing: border-box; \
                     border-radius: var(--m-radius-full); border: {} solid {ink}",
                    px(c - d / 2.0, k),
                    px(c - d / 2.0, k),
                    px(stroke, k),
                    d = px(d, k),
                ),
            )
            .into_view()
    };
    let disc = |cx: f32, cy: f32, d: f32, fill: &str| {
        el("span")
            .attr(
                "style",
                format!(
                    "position: absolute; left: {}; top: {}; width: {d}; height: {d}; \
                     border-radius: var(--m-radius-full); background: {fill}",
                    px(cx - d / 2.0, k),
                    px(cy - d / 2.0, k),
                    d = px(d, k),
                ),
            )
            .into_view()
    };
    let parts: Vec<View> = match mode {
        SunMode::Auto => {
            let d = r * 1.24;
            // The right half filled: a whole disc, clipped to that half.
            let half = el("span")
                .attr(
                    "style",
                    format!(
                        "position: absolute; left: {}; top: {}; width: {}; height: {}; overflow: hidden",
                        px(c, k),
                        px(c - d / 2.0, k),
                        px(d / 2.0, k),
                        px(d, k),
                    ),
                )
                .child(disc(0.0, d / 2.0, d, ink))
                .into_view();
            vec![ring(d), half]
        }
        SunMode::Day => {
            let mut v = vec![disc(c, c, r * 0.68, ink)];
            for i in 0..8 {
                v.push(
                    turned(size, i as f32 * 45.0, k)
                        .child(bar_box(
                            c - stroke / 2.0,
                            c - r * 0.78,
                            stroke,
                            r * 0.23,
                            ink,
                            stroke / 2.0,
                            k,
                        ))
                        .into_view(),
                );
            }
            v
        }
        SunMode::Night => vec![
            disc(c, c, r * 1.2, ink),
            disc(c + r * 0.3, c - r * 0.24, r * 1.0, behind),
        ],
        SunMode::Manual => vec![
            ring(r * 1.24),
            bar_box(
                c - stroke / 2.0,
                c - r * 0.42,
                stroke,
                r * 0.42,
                ink,
                0.0,
                k,
            ),
            bar_box(c, c - stroke / 2.0, r * 0.3, stroke, ink, 0.0, k),
        ],
    };
    el("span")
        .attr(
            "style",
            format!(
                "position: relative; flex: none; display: block; width: {s}; height: {s}",
                s = px(size, k)
            ),
        )
        .children(parts)
        .into_view()
}

/// The light disc and, open, Auto · Light · Dark beside it (under it in a
/// lying row).
fn light_view(v: &LightView, k: f32, acts: &Acts) -> View {
    let (ink, behind) = if v.open {
        ("var(--m-on-accent)", "var(--m-accent)")
    } else {
        ("var(--map-ink)", "var(--map-fill)")
    };
    let disc = el("div")
        .attr("style", plate_style(DISC, k, v.open, "var(--m-accent)"))
        .attr("title", light_hint(v.mode))
        .attr("aria-label", "Light")
        .on("click", tap(acts, ChromeAct::LightDisc))
        .child(light_icon(v.mode, DISC - 4.0, ink, behind, k));
    let choices: View = if v.open {
        let pill_h = DISC - 8.0;
        let at = if v.lying {
            format!("left: 0; top: {}", px(DISC + theme::OSD_GAP, k))
        } else {
            format!(
                "left: {}; top: {}",
                px(DISC + theme::OSD_GAP, k),
                px(1.0, k)
            )
        };
        let pills: Vec<View> = LIGHTS
            .into_iter()
            .map(|(choice, label)| {
                let on = v.mode == choice;
                let (bg, ink, behind) = if on {
                    ("var(--m-text-strong)", "var(--map-fill)", "var(--m-text-strong)")
                } else {
                    ("transparent", "var(--m-text-dim)", "var(--map-fill)")
                };
                el("div")
                    .attr(
                        "style",
                        format!(
                            "height: {}; display: flex; align-items: center; gap: {}; padding: 0 {} 0 {}; \
                             border-radius: var(--m-radius-full); background: {bg}; color: {ink}; \
                             font-size: {}; white-space: nowrap; cursor: pointer",
                            px(pill_h, k),
                            px(2.0, k),
                            px(10.0, k),
                            px(2.0, k),
                            px(theme::SIZE_SUB, k),
                        ),
                    )
                    .attr("title", label)
                    .on("click", tap(acts, ChromeAct::Light(choice)))
                    .child(light_icon(choice, pill_h * 0.72, ink, behind, k))
                    .child(el("span").child(label))
                    .into_view()
            })
            .collect();
        el("div")
            .attr(
                "style",
                format!(
                    "position: absolute; {at}; display: flex; gap: {}; padding: {}; background: var(--map-fill); \
                     border: 1px solid var(--map-rim); border-radius: var(--m-radius-full); \
                     box-shadow: var(--m-shadow); pointer-events: auto",
                    px(2.0, k),
                    px(3.0, k),
                ),
            )
            .on("click", |_: &Event| {})
            .children(pills)
            .into_view()
    } else {
        View::Empty
    };
    el("div")
        .attr("style", spot(v.left, v.top, false, 0.0, k))
        .child(disc)
        .child(choices)
        .into_view()
}

/// The scale bar: a bracket and its length, over a halo; it takes no taps.
fn scale_view(v: &ScaleView, k: f32) -> View {
    let h = theme::SIZE_TINY + 10.0;
    let ink = "var(--m-text-strong)";
    let halo = "var(--m-backdrop)";
    let rule = |x: f32, y: f32, w: f32, hh: f32| -> [View; 2] {
        [
            bar_box(x - 0.75, y - 0.75, w + 1.5, hh + 1.5, halo, 1.0, k),
            bar_box(x, y, w, hh, ink, 0.0, k),
        ]
    };
    let mut parts: Vec<View> = Vec::new();
    parts.extend(rule(0.0, h - 1.5, v.pts, 1.5));
    parts.extend(rule(0.0, h - 5.0, 1.5, 5.0));
    parts.extend(rule(v.pts - 1.5, h - 5.0, 1.5, 5.0));
    parts.push(
        el("span")
            .attr(
                "style",
                format!(
                    "position: absolute; left: 0; bottom: {}; white-space: nowrap; color: {ink}; font-size: {}; \
                     line-height: 1",
                    px(6.0, k),
                    px(theme::SIZE_TINY, k)
                ),
            )
            .child(v.label.clone())
            .into_view(),
    );
    el("div")
        .attr(
            "style",
            format!(
                "position: absolute; left: {}; bottom: {}; width: {}; height: {}",
                px(v.left, k),
                px(v.bottom, k),
                px(v.pts, k),
                px(h, k),
            ),
        )
        .children(parts)
        .into_view()
}

// ─── The floor picker ────────────────────────────────────────────────────────

/// The storeys of the building being looked into, top first, under the
/// right column.
fn floors_view(v: &FloorsView, k: f32, acts: &Acts) -> View {
    let head = el("div")
        .attr("style", "display: flex; align-items: center; gap: 8px")
        .child(
            el("span")
                .attr(
                    "style",
                    format!(
                        "flex: 1 1 auto; color: var(--m-text-strong); font-size: {}; font-weight: 600",
                        px(theme::SIZE_SUB, k)
                    ),
                )
                .child("Floors"),
        )
        .child(
            CloseButton {
                onclick: Some(handler(acts, ChromeAct::FloorClose)),
                ..Default::default()
            }
            .render(),
        );
    let room: View = match &v.room {
        Some(room) => el("span")
            .attr(
                "style",
                format!(
                    "color: var(--m-accent-text); font-size: {}",
                    px(theme::SIZE_SUB, k)
                ),
            )
            .child(format!("In the {room}"))
            .into_view(),
        None => View::Empty,
    };
    let rows: Vec<View> = v
        .storeys
        .iter()
        .map(|(i, name, current, here)| {
            let mut label = name.clone();
            if *here {
                label.push_str("  ●");
            }
            let mut row = el("div")
                .attr(
                    "style",
                    format!(
                        "min-height: {}; display: flex; align-items: center; padding: 0 {}; \
                         border-radius: var(--m-radius); background: {}; color: {}; font-size: {}; \
                         white-space: nowrap; overflow: hidden; cursor: pointer",
                        px(32.0, k),
                        px(10.0, k),
                        if *current {
                            "var(--m-accent-soft)"
                        } else {
                            "transparent"
                        },
                        if *current {
                            "var(--m-accent-text)"
                        } else {
                            "var(--m-text)"
                        },
                        px(theme::SIZE_BODY, k),
                    ),
                )
                .child(label);
            // A tap on the storey shown does nothing, as egui's.
            if !*current {
                row = row.on("click", tap(acts, ChromeAct::FloorPick(Some(*i))));
            }
            row.into_view()
        })
        .collect();
    let follow: View = if v.follow {
        Button {
            variant: Variant::Secondary,
            size: Size::Sm,
            onclick: Some(handler(acts, ChromeAct::FloorPick(None))),
            children: "Follow me".into_view(),
            ..Default::default()
        }
        .render()
    } else {
        View::Empty
    };
    let source = el("span")
        .attr(
            "style",
            format!(
                "margin-top: {}; color: var(--m-text-dim); font-size: {}",
                px(4.0, k),
                px(theme::SIZE_TINY, k)
            ),
        )
        .child(if v.designed {
            "Drawn by its owner"
        } else {
            "Generated from the register"
        });
    let plan = Button {
        variant: Variant::Secondary,
        size: Size::Sm,
        icon: glyph(icons::OPEN_EXTERNAL, 16.0 * k),
        title: Some("Open this building in the floor-plan editor".to_owned()),
        disabled: (!v.plan).into(),
        onclick: Some(handler(acts, ChromeAct::FloorPlan)),
        children: "Floor plan".into_view(),
        ..Default::default()
    }
    .render();
    el("div")
        .attr(
            "style",
            format!(
                "position: absolute; right: {}; top: {}; width: {}; display: flex; flex-direction: column; \
                 gap: {}; padding: {}; background: var(--map-fill); border: 1px solid var(--map-rim); \
                 border-radius: var(--m-radius-lg); box-shadow: var(--m-shadow); pointer-events: auto",
                px(v.right, k),
                px(v.top, k),
                px(v.width, k),
                px(4.0, k),
                px(10.0, k),
            ),
        )
        // The frame's padding is the picker's too.
        .on("click", |_: &Event| {})
        .child(head)
        .child(room)
        .children(rows)
        .child(follow)
        .child(source)
        .child(plan)
        .into_view()
}

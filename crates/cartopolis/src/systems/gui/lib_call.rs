//! The call screen drawn by the component library (`lib_host`): a call
//! ringing over the whole screen, the call you are in with its video tiles,
//! the sheets over it (More, People, Leave, Notes), and at the top the
//! folded call (the green bar, or the row of tiles over the map), ringing
//! banners and notices.
//!
//! `call_screen::drive_call_screen` keeps every decision — what rings, when
//! the screen unfolds, when the controls fade over a picture, who is large
//! in Speaker view, what a host may do to whom — and publishes it in
//! `CallScreenLib::view`. A tap here is a `CallAct`, handed back in
//! `CallScreenLib::acts` and done by `call_screen::apply`.
//!
//! **Pictures.** Each video tile is `<img src="bevy:call_tile_<id>">` (a
//! shared screen `bevy:call_tile_screen_<id>`): this system writes each
//! newest picture into a Bevy `Image` of its own — one per tile, reused,
//! made again only when the picture's size changes — and names it in
//! `mithril_bevy::LiveImages` before the view that shows it; a tile whose
//! pictures stop loses its name. The web's DOM backend has no `bevy:`
//! images, so there a tile with a picture shows the dark call screen.
//!
//! **Placement.** As egui's, in egui points times the library's px per
//! point: the full screen is absolutely laid out from the window's size
//! with egui's own figures (`EDGE`, `BOTTOM_EDGE`, the stage and grid
//! rules); the bar and banners stack down from `TopView::top`. The full
//! screen is dark in both themes, as a phone's call screen is (its colours
//! are the dark palette's, named outright); the bar follows the theme, as
//! egui's does.
//!
//! **Not as egui's.** The sheet over the call follows the app's theme where
//! egui's is dark in both: the library's host has one `dark` for every
//! screen, and mounting this one dark would leave the rest dark after it.
//! A person's profile picture is their coloured initial (`lib_chats::disc`);
//! no hover tints; the room chip's "Back to the call" sits beside the chip
//! with the pair centred, where egui centres the chip alone; Enter in the
//! note field is a new line (Add adds it).

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use bevy::ecs::system::NonSendMarker;
use bevy::prelude::*;
// The library's, not Bevy's: an explicit import outranks the two globs.
use mithril::prelude::*;
use mithril::prelude::{Button, Event};

use crate::systems::call_screen::{
    BannerView, CallAct, CallScreenLib, CallView, FoldedView, FullView, InCallView, MoreView,
    NotesView, PeopleView, PersonView, Sheet, SheetView, Tint, TopView, BOTTOM_EDGE, EDGE,
    REACTIONS,
};
use crate::systems::calls::{CallRequest, Calls, SELF_TILE};
use crate::systems::discs::ChatKind;
use crate::systems::icons;
use crate::systems::lib_chats::{disc, group as heading, note};
use crate::systems::lib_pins::css;
use crate::systems::lib_place_card::{cell, icon};
use crate::systems::lib_settings::LibScreens;
use crate::systems::theme::tokens::alpha;
use crate::systems::theme::{self, CallTone, Palette, CALL_BUTTON_LARGE, CALL_BUTTON_SMALL};
use crate::systems::ui_input::UiMetrics;
use cartopolis_core::meeting::NoteKind;
use cartopolis_core::{CallAction, CallSettings};

// Bevy's, named outright: the library's prelude is a glob too.
type BevyImage = bevy::prelude::Image;
#[cfg(all(
    feature = "ui-probe",
    not(all(feature = "ui-dom", target_arch = "wasm32"))
))]
type BevyAssets<A> = bevy::prelude::Assets<A>;
type BevyHandle<A> = bevy::prelude::Handle<A>;

/// The full screen's name in the library's host.
const KEY_SCREEN: &str = "call_screen";
/// The bar's, the banners' and the notice's.
const KEY_BAR: &str = "call_bar";

/// A person's tile picture's name in `LiveImages`.
pub(crate) fn tile_name(id: u64) -> String {
    format!("call_tile_{id}")
}

/// A shared screen's picture's name in `LiveImages`.
pub(crate) fn screen_name(id: u64) -> String {
    format!("call_tile_screen_{id}")
}

type Acts = Rc<RefCell<Vec<CallAct>>>;

/// A tap that hands `list` back, in order.
fn on(acts: &Acts, list: Vec<CallAct>) -> Handler {
    let acts = acts.clone();
    Rc::new(move |_: &Event| acts.borrow_mut().extend(list.iter().cloned())) as Handler
}

/// A handler as an `on:click` listener.
fn tap(h: Handler) -> impl Fn(&Event) + 'static {
    move |e: &Event| h(e)
}

/// The full screen's signals.
#[derive(Clone)]
struct ScreenLive {
    /// The library's px per egui point: 1 on the Bevy backend, the UI scale
    /// as DOM (`lib_place_card::Placement::over_the_dock` says why).
    k: f32,
    /// The theme it was mounted in.
    dark: bool,
    size: Signal<(f32, f32)>,
    /// The screen with its status line blanked (`split_full`): the stage is
    /// rebuilt when this moves, and a stage rebuilt is not laid out in the
    /// frame it is built — with the timer in it, every second, a capture in
    /// that frame showed the screen empty.
    full: Signal<Option<FullView>>,
    /// The status line (the call's timer), written into its text in place.
    status: Signal<String>,
    /// Which sheet is open: its frame and the note field follow only this.
    sheet_kind: Signal<Option<Sheet>>,
    sheet: Signal<Option<SheetView>>,
    note_kind: Signal<NoteKind>,
    /// The note being written; kept while the sheet is closed, as egui's.
    draft: Signal<String>,
    /// Seconds, for the ringing pulse.
    clock: Signal<f32>,
    started: web_time::Instant,
    acts: Acts,
}

/// The top's signals.
#[derive(Clone)]
struct BarLive {
    k: f32,
    dark: bool,
    /// The top with the folded bar's timer blanked (`split_top`), for the
    /// reason `ScreenLive::full` has.
    top: Signal<Option<(TopView, f32)>>,
    /// The folded bar's timer, written into its text in place.
    status: Signal<String>,
    acts: Acts,
}

/// The screen without its status line, and the line: the timer moves every
/// second and must not rebuild the stage.
fn split_full(mut full: FullView) -> (FullView, String) {
    let status = match &mut full {
        FullView::InCall(v) => std::mem::take(&mut v.status),
        _ => String::new(),
    };
    (full, status)
}

/// The top without the folded bar's timer, and the timer.
fn split_top(mut top: TopView) -> (TopView, String) {
    let status = match &mut top.folded {
        Some(FoldedView::Bar { status, .. }) => std::mem::take(status),
        _ => String::new(),
    };
    (top, status)
}

thread_local! {
    static ACTS: Acts = Rc::new(RefCell::new(Vec::new()));
    static SCREEN: RefCell<Option<ScreenLive>> = const { RefCell::new(None) };
    static BAR: RefCell<Option<BarLive>> = const { RefCell::new(None) };
}

/// The pictures written so far: each name's image and its size.
#[derive(Default)]
pub struct CallPictures {
    #[cfg_attr(
        not(all(
            feature = "ui-probe",
            not(all(feature = "ui-dom", target_arch = "wasm32"))
        )),
        allow(dead_code)
    )]
    held: HashMap<String, (BevyHandle<BevyImage>, u32, u32)>,
}

/// Mounts, bridges and unmounts the call screen and the bar, and writes the
/// call's pictures into their images. `PostUpdate`, before `DriveUi`, as
/// every library screen.
#[allow(clippy::too_many_arguments)]
pub fn drive_lib_call(
    _main: NonSendMarker,
    lib: Res<LibScreens>,
    bridge: Option<ResMut<CallScreenLib>>,
    mut calls: ResMut<Calls>,
    metrics: Res<UiMetrics>,
    mut pictures: Local<CallPictures>,
    #[cfg(all(
        feature = "ui-probe",
        not(all(feature = "ui-dom", target_arch = "wasm32"))
    ))]
    mut images: ResMut<BevyAssets<BevyImage>>,
    #[cfg(all(
        feature = "ui-probe",
        not(all(feature = "ui-dom", target_arch = "wasm32"))
    ))]
    mut live_images: Option<ResMut<mithril_bevy::LiveImages>>,
) {
    if !lib.call {
        return;
    }
    let Some(mut bridge) = bridge else {
        return;
    };
    let k = if cfg!(all(feature = "ui-dom", target_arch = "wasm32")) {
        metrics.ui_scale()
    } else {
        1.0
    };
    // The taps of the frame, for egui's system to do next frame.
    let acts = ACTS.with(Rc::clone);
    {
        let taken: Vec<CallAct> = std::mem::take(&mut *acts.borrow_mut());
        if !taken.is_empty() {
            bridge.acts.extend(taken);
        }
    }
    let view: Option<CallView> = bridge.view.clone();

    // The pictures before the views that show them: an `<img>` reads its
    // source when it is styled.
    #[cfg(all(
        feature = "ui-probe",
        not(all(feature = "ui-dom", target_arch = "wasm32"))
    ))]
    if let Some(live_images) = live_images.as_mut() {
        write_pictures(
            &mut calls,
            &mut pictures.held,
            &mut images,
            live_images,
            view.as_ref().is_some_and(|v| v.pictures),
            view.is_some(),
        );
    }
    #[cfg(not(all(
        feature = "ui-probe",
        not(all(feature = "ui-dom", target_arch = "wasm32"))
    )))]
    let _ = (&mut calls, &mut pictures);

    let dark = theme::p().ink == theme::DARK.ink;
    SCREEN.with(|m| {
        let mut m = m.borrow_mut();
        let full = view.as_ref().and_then(|v| v.full.clone());
        let stale = m
            .as_ref()
            .is_some_and(|l| full.is_none() || l.k != k || l.dark != dark);
        if stale {
            *m = None;
            crate::systems::lib_host::clear_keyed(KEY_SCREEN);
        }
        let (Some(v), Some(full)) = (view.as_ref(), full) else {
            return;
        };
        let (full, status) = split_full(full);
        let size = (v.w, v.h);
        let sheet = v.sheet.clone();
        let kind = sheet.as_ref().map(SheetView::kind);
        let note_kind = match &sheet {
            Some(SheetView::Notes(n)) => Some(n.kind),
            _ => None,
        };
        let ringing = matches!(full, FullView::Incoming { .. });
        match m.as_ref() {
            None => {
                let live = ScreenLive {
                    k,
                    dark,
                    size: signal(size),
                    full: signal(Some(full)),
                    status: signal(status),
                    sheet_kind: signal(kind),
                    sheet: signal(sheet),
                    note_kind: signal(note_kind.unwrap_or(NoteKind::Note)),
                    draft: signal(String::new()),
                    clock: signal(0.0),
                    started: web_time::Instant::now(),
                    acts: acts.clone(),
                };
                // In the app's theme: the host's `dark` is one for every
                // screen and stays after this one goes. The screen itself is
                // drawn in the dark palette's colours outright; only the
                // sheet's controls follow the theme.
                crate::systems::lib_host::mount_keyed(
                    KEY_SCREEN,
                    screen_view(&live),
                    dark,
                    theme::touch_layout(),
                );
                write_html(screen_view(&live));
                *m = Some(live);
            }
            Some(live) => {
                if live.size.get_untracked() != size {
                    live.size.set(size);
                }
                if live.status.peek(|now| *now != status) {
                    live.status.set(status);
                }
                if live.full.peek(|now| now.as_ref() != Some(&full)) {
                    live.full.set(Some(full));
                }
                if live.sheet_kind.get_untracked() != kind {
                    live.sheet_kind.set(kind);
                }
                if live.sheet.peek(|now| *now != sheet) {
                    live.sheet.set(sheet);
                }
                if let Some(nk) = note_kind {
                    if live.note_kind.get_untracked() != nk {
                        live.note_kind.set(nk);
                    }
                }
                // The pulse moves only while something rings.
                if ringing {
                    live.clock.set(live.started.elapsed().as_secs_f32());
                }
            }
        }
    });

    BAR.with(|m| {
        let mut m = m.borrow_mut();
        let top = view.as_ref().and_then(|v| v.top.clone().map(|t| (t, v.w)));
        let stale = m
            .as_ref()
            .is_some_and(|l| top.is_none() || l.k != k || l.dark != dark);
        if stale {
            *m = None;
            crate::systems::lib_host::clear_keyed(KEY_BAR);
        }
        let Some(top) = top else {
            return;
        };
        let (top, w) = top;
        let (top, status) = split_top(top);
        match m.as_ref() {
            None => {
                let live = BarLive {
                    k,
                    dark,
                    top: signal(Some((top, w))),
                    status: signal(status),
                    acts: acts.clone(),
                };
                crate::systems::lib_host::mount_keyed(
                    KEY_BAR,
                    bar_view(&live),
                    dark,
                    theme::touch_layout(),
                );
                *m = Some(live);
            }
            Some(live) => {
                if live.status.peek(|now| *now != status) {
                    live.status.set(status);
                }
                let top = (top, w);
                if live.top.peek(|now| now.as_ref() != Some(&top)) {
                    live.top.set(Some(top));
                }
            }
        }
    });
}

/// Writes each newest picture into its image, names new ones and forgets
/// the names of pictures no longer held. `upload` off (folded to the green
/// bar) leaves the newest `fresh` for when the screen comes back, as egui's
/// does; `any` off (nothing of a call up) lets every image go.
#[cfg(all(
    feature = "ui-probe",
    not(all(feature = "ui-dom", target_arch = "wasm32"))
))]
fn write_pictures(
    calls: &mut Calls,
    held: &mut HashMap<String, (BevyHandle<BevyImage>, u32, u32)>,
    images: &mut BevyAssets<BevyImage>,
    live_images: &mut mithril_bevy::LiveImages,
    upload: bool,
    any: bool,
) {
    let mut present: std::collections::HashSet<String> = std::collections::HashSet::new();
    if any {
        for (id, p) in calls.pictures_mut() {
            let name = tile_name(id);
            write_one(&name, p, held, images, live_images, upload);
            present.insert(name);
        }
        for (id, p) in calls.screens_mut() {
            let name = screen_name(id);
            write_one(&name, p, held, images, live_images, upload);
            present.insert(name);
        }
    }
    let gone: Vec<String> = held
        .keys()
        .filter(|n| !present.contains(*n))
        .cloned()
        .collect();
    for name in gone {
        held.remove(&name);
        live_images.0.remove(&name);
    }
}

/// One picture into its image: written in place at the same size, made
/// again (under the same handle) at a new one, made and named the first
/// time. Bounded by the frame's upload allowance; a picture refused stays
/// `fresh` for the next frame.
#[cfg(all(
    feature = "ui-probe",
    not(all(feature = "ui-dom", target_arch = "wasm32"))
))]
fn write_one(
    name: &str,
    p: &mut crate::systems::calls::Picture,
    held: &mut HashMap<String, (BevyHandle<BevyImage>, u32, u32)>,
    images: &mut BevyAssets<BevyImage>,
    live_images: &mut mithril_bevy::LiveImages,
    upload: bool,
) {
    use bevy::asset::RenderAssetUsages;
    use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
    let known = held.get(name).cloned();
    if !upload || (!p.fresh && known.is_some()) || p.width == 0 || p.height == 0 {
        return;
    }
    if p.rgba.len() != (p.width * p.height * 4) as usize {
        return;
    }
    if !crate::systems::stream_ledger::admit(p.rgba.len()) {
        return;
    }
    p.fresh = false;
    let image = |p: &crate::systems::calls::Picture| {
        BevyImage::new(
            Extent3d {
                width: p.width,
                height: p.height,
                depth_or_array_layers: 1,
            },
            TextureDimension::D2,
            p.rgba.clone(),
            TextureFormat::Rgba8UnormSrgb,
            RenderAssetUsages::default(),
        )
    };
    match known {
        Some((handle, w, h)) if w == p.width && h == p.height => {
            let written = match images.get_mut(&handle) {
                Some(mut img) => {
                    img.data = Some(p.rgba.clone());
                    true
                }
                None => false,
            };
            if !written {
                let _ = images.insert(&handle, image(&*p));
            }
        }
        Some((handle, _, _)) => {
            let _ = images.insert(&handle, image(&*p));
            held.insert(name.to_string(), (handle, p.width, p.height));
        }
        None => {
            let handle = images.add(image(&*p));
            live_images.0.insert(name.to_string(), handle.clone());
            held.insert(name.to_string(), (handle, p.width, p.height));
        }
    }
}

/// `CARTO_UI_LIB_HTML=<path>`: the screen as HTML, for the browser beside a
/// capture.
fn write_html(view: View) {
    if let Some(path) = std::env::var_os("CARTO_UI_LIB_HTML") {
        use mithril::view::{mount, StringRenderer};
        let r = Rc::new(RefCell::new(StringRenderer::new()));
        let root = r.borrow().root();
        let mounted = mount(view, &r, &root, None);
        let out = r.borrow().html();
        drop(mounted);
        if let Err(e) = std::fs::write(&path, out) {
            warn!(?path, error = %e, "lib call: could not write the HTML");
        }
    }
}

// ─── Pieces ──────────────────────────────────────────────────────────────────

fn pos2(x: f32, y: f32) -> Vec2 {
    Vec2::new(x, y)
}

/// The rect builders the placement was written with (it was egui's), over
/// Bevy's `Rect`. `from_min_max` does not reorder its corners.
trait RectExt {
    fn from_min_max(min: Vec2, max: Vec2) -> Rect;
    fn from_min_size(min: Vec2, size: Vec2) -> Rect;
    /// `by` in from every side.
    fn shrink(self, by: f32) -> Rect;
    /// `by` out on every side.
    fn expand(self, by: f32) -> Rect;
}

impl RectExt for Rect {
    fn from_min_max(min: Vec2, max: Vec2) -> Rect {
        Rect { min, max }
    }

    fn from_min_size(min: Vec2, size: Vec2) -> Rect {
        Rect {
            min,
            max: min + size,
        }
    }

    fn shrink(self, by: f32) -> Rect {
        self.expand(-by)
    }

    fn expand(self, by: f32) -> Rect {
        Rect {
            min: self.min - Vec2::splat(by),
            max: self.max + Vec2::splat(by),
        }
    }
}

/// The largest rect of shape `aspect` inside `rect`, centred.
fn contain(aspect: f32, rect: Rect) -> Rect {
    let target = rect.width() / rect.height().max(1.0);
    let size = if aspect > target {
        Vec2::new(rect.width(), rect.width() / aspect)
    } else {
        Vec2::new(rect.height() * aspect, rect.height())
    };
    Rect::from_center_size(rect.center(), size)
}

/// An absolutely placed box over `r`, in px, then the rest.
fn bx(k: f32, r: Rect, rest: &str) -> String {
    format!(
        "position: absolute; left: {:.1}px; top: {:.1}px; width: {:.1}px; height: {:.1}px; {rest}",
        r.min.x * k,
        r.min.y * k,
        r.width() * k,
        r.height() * k
    )
}

/// Black at a percentage, for the shades over a picture.
fn black(pct: f32) -> String {
    alpha(theme::Rgba8::BLACK, pct)
}

/// White at a percentage, for the frosted plates over a picture.
fn white(pct: f32) -> String {
    alpha(theme::Rgba8::WHITE, pct)
}

/// A filled box, `radius` round, listening nowhere.
fn plate(k: f32, r: Rect, radius: f32, fill: &str) -> View {
    let style = bx(
        k,
        r,
        &format!(
            "border-radius: {:.1}px; background: {fill}; pointer-events: none",
            radius * k
        ),
    );
    view! { <div style={style}></div> }
}

/// A circle's outline: `radius` to the middle of a `width` stroke, as egui's
/// `circle_stroke`.
fn ring(k: f32, c: Vec2, radius: f32, width: f32, colour: &str) -> View {
    let outer = radius + width * 0.5;
    let r = Rect::from_center_size(c, Vec2::splat(2.0 * outer));
    let style = bx(
        k,
        r,
        &format!(
            "box-sizing: border-box; border-radius: {:.1}px; border: {:.1}px solid {colour}; pointer-events: none",
            outer * k,
            width * k
        ),
    );
    view! { <div style={style}></div> }
}

/// A run of words and glyphs: an icon in the icon face, words in the body's.
enum Seg {
    I(&'static str),
    T(String),
    /// Words that follow a signal in place (the timer).
    D(Signal<String>),
}

fn line(segs: Vec<Seg>, size: f32) -> View {
    View::Fragment(
        segs.into_iter()
            .map(|s| match s {
                Seg::I(g) => {
                    let style = format!(
                        "flex: none; font-family: 'Material Symbols Rounded'; font-size: {:.0}px; line-height: 1",
                        (size * 1.15).round()
                    );
                    view! { <span style={style}>{g}</span> }
                }
                Seg::T(t) => {
                    let style = format!(
                        "min-width: 0; font-size: {size}px; line-height: 1.25; white-space: nowrap; overflow: hidden; text-overflow: ellipsis"
                    );
                    view! { <span style={style}>{t}</span> }
                }
                Seg::D(s) => {
                    let style = format!(
                        "min-width: 0; font-size: {size}px; line-height: 1.25; white-space: nowrap; overflow: hidden; text-overflow: ellipsis"
                    );
                    let t = View::DynText(Rc::new(move || s.get()));
                    view! { <span style={style}>{t}</span> }
                }
            })
            .collect(),
    )
}

/// Text centred on `(x, y)` in a band `band` wide, one line, listening
/// nowhere.
#[allow(clippy::too_many_arguments)]
fn text_at(k: f32, x: f32, y: f32, band: f32, segs: Vec<Seg>, size: f32, colour: &str) -> View {
    let r = Rect::from_center_size(pos2(x, y), Vec2::new(band, size * 2.0 + 8.0));
    let style = bx(
        k,
        r,
        &format!(
            "display: flex; align-items: center; justify-content: center; gap: 4px; overflow: hidden; \
             white-space: nowrap; pointer-events: none; color: {colour}"
        ),
    );
    view! { <div style={style}>{line(segs, size)}</div> }
}

/// A person's disc, `d` across, centred on `c`: their coloured initial.
fn avatar(k: f32, c: Vec2, name: &str, d: f32) -> View {
    let r = Rect::from_center_size(c, Vec2::splat(d));
    let style = bx(
        k,
        r,
        "display: flex; align-items: center; justify-content: center; pointer-events: none",
    );
    view! { <div style={style}>{disc(name, ChatKind::Private, (d * k).round() as u32)}</div> }
}

/// A picture named `name` (`LiveImages`) of shape `aspect` drawn over
/// `rect`: cropped to fill it, centred, or whole inside it (`fill` off).
fn image(k: f32, name: &str, aspect: f32, rect: Rect, radius: f32, fill: bool) -> View {
    let src = format!("bevy:{name}");
    let aspect = aspect.max(0.01);
    let frame = if fill { rect } else { contain(aspect, rect) };
    let (iw, ih) = if !fill {
        (frame.width(), frame.height())
    } else if aspect > frame.width() / frame.height().max(1.0) {
        (frame.height() * aspect, frame.height())
    } else {
        (frame.width(), frame.width() / aspect)
    };
    let style = bx(
        k,
        frame,
        &format!(
            "overflow: hidden; border-radius: {:.1}px; pointer-events: none",
            radius * k
        ),
    );
    let img = format!(
        "position: absolute; left: {:.1}px; top: {:.1}px; width: {:.1}px; height: {:.1}px; border-radius: {:.1}px",
        (frame.width() - iw) * 0.5 * k,
        (frame.height() - ih) * 0.5 * k,
        iw * k,
        ih * k,
        radius * k
    );
    let picture = el("img")
        .attr("src", src)
        .attr("alt", "")
        .attr("style", img)
        .into_view();
    view! { <div style={style}>{picture}</div> }
}

/// `paint_picture`: cropped to fill, except an upright picture on a wide
/// rect (a phone calling a desktop), which is letterboxed.
fn picture(k: f32, name: &str, aspect: f32, rect: Rect, radius: f32, always_fill: bool) -> View {
    let tall_on_wide = aspect < 1.0 && rect.width() > rect.height();
    image(k, name, aspect, rect, radius, always_fill || !tall_on_wide)
}

/// A vertical shade over a picture's edge, so the words and keys read.
fn shade(k: f32, r: Rect, dark_at_top: bool) -> View {
    let g = if dark_at_top {
        format!("linear-gradient(to bottom, {}, transparent)", black(59.0))
    } else {
        format!("linear-gradient(to bottom, transparent, {})", black(59.0))
    };
    let style = bx(
        k,
        r,
        &format!("background-image: {g}; pointer-events: none"),
    );
    view! { <div style={style}></div> }
}

/// A round call control centred on `c`, `d` across: `theme::call_button`.
#[allow(clippy::too_many_arguments)]
fn call_button(
    acts: &Acts,
    k: f32,
    pal: &Palette,
    c: Vec2,
    d: f32,
    glyph: &'static str,
    tip: &str,
    tone: CallTone,
    list: Vec<CallAct>,
) -> View {
    let (fill, rim, ink) = match tone {
        CallTone::HangUp => (
            css(theme::CALL_END_RED),
            css(theme::CALL_END_RED),
            css(theme::DARK.cream),
        ),
        CallTone::Accept => (
            css(theme::CALL_ACCEPT_GREEN),
            css(theme::CALL_ACCEPT_GREEN),
            css(theme::DARK.cream),
        ),
        CallTone::Off => (css(pal.cream), css(pal.accent_ring), css(pal.ink)),
        // The frosted plate: white over the dark call screen, black over a
        // light banner.
        CallTone::On => {
            if pal.dark {
                (white(15.0), white(18.0), css(pal.cream))
            } else {
                (black(7.5), black(9.0), css(pal.cream))
            }
        }
    };
    let r = Rect::from_center_size(c, Vec2::splat(d - 2.0));
    let style = bx(
        k,
        r,
        &format!(
            "box-sizing: border-box; border-radius: {:.1}px; background: {fill}; border: 1px solid {rim}; \
             color: {ink}; display: flex; align-items: center; justify-content: center; \
             font-family: 'Material Symbols Rounded'; font-size: {:.0}px; line-height: 1; cursor: pointer",
            (d - 2.0) * 0.5 * k,
            (d * 0.44).round()
        ),
    );
    let tip = tip.to_string();
    view! {
        <div title={tip} style={style} on:click={tap(on(acts, list))}>{glyph}</div>
    }
}

/// A chip of words on `bg`: a name on a tile, a hand's place.
fn chip(segs: Vec<Seg>, bg: &str, ink: &str) -> View {
    let style = format!(
        "flex: none; max-width: 100%; display: flex; align-items: center; gap: 4px; padding: 3px 8px; \
         border-radius: 999px; background: {bg}; color: {ink}; overflow: hidden; white-space: nowrap"
    );
    view! { <div style={style}>{line(segs, theme::SIZE_SUB)}</div> }
}

/// `child` held in a corner of `r`: top (`flex-start`) or bottom
/// (`flex-end`), left.
fn corner(k: f32, r: Rect, justify: &str, child: View) -> View {
    let style = bx(
        k,
        r,
        &format!(
            "display: flex; flex-direction: column; justify-content: {justify}; align-items: flex-start; pointer-events: none"
        ),
    );
    view! { <div style={style}>{child}</div> }
}

/// The lock line every call screen carries, centred on `(x, y)`.
fn encrypted_line(k: f32, x: f32, y: f32, band: f32, agent: bool) -> View {
    let (words, colour) = if agent {
        // What it hears goes on to a model provider, and the person talking
        // to it should see that before saying anything.
        (
            "AI agent · what you say goes to OpenAI",
            css(theme::DARK.warn),
        )
    } else {
        ("End-to-end encrypted", css(theme::DARK.text_dim))
    };
    text_at(
        k,
        x,
        y,
        band,
        vec![Seg::I(icons::LOCK), Seg::T(words.into())],
        theme::SIZE_SUB,
        &colour,
    )
}

/// The fold-down arrow in the top-left corner.
fn fold_button(acts: &Acts, k: f32) -> View {
    let d = 40.0;
    let r = Rect::from_min_size(pos2(EDGE, EDGE), Vec2::splat(d));
    let style = bx(
        k,
        r,
        &format!(
            "border-radius: {:.1}px; display: flex; align-items: center; justify-content: center; \
             color: {}; font-family: 'Material Symbols Rounded'; font-size: 24px; line-height: 1; cursor: pointer",
            d * 0.5 * k,
            css(theme::DARK.cream)
        ),
    );
    view! {
        <div title="Back to the app — the call carries on" style={style} on:click={tap(on(acts, vec![CallAct::Fold]))}>{icons::BACK}</div>
    }
}

// ─── The full screen ─────────────────────────────────────────────────────────

/// The root: the whole window, dark, taking every touch the controls do not
/// (a tap on the picture brings the controls up or down), with the ringing
/// pulse, the stage and the sheet over it.
fn screen_view(l: &ScreenLive) -> View {
    let root = {
        let (size, k) = (l.size.clone(), l.k);
        move || -> String {
            let (w, h) = size.get();
            format!(
                "position: absolute; left: 0; top: 0; width: {:.1}px; height: {:.1}px; overflow: hidden; background: {}",
                w * k,
                h * k,
                css(theme::DARK.ink)
            )
        }
    };
    let backdrop = {
        let (full, acts) = (l.full.clone(), l.acts.clone());
        move |_: &Event| {
            if full.peek(|f| matches!(f, Some(FullView::InCall(_)))) {
                acts.borrow_mut().push(CallAct::ToggleControls);
            }
        }
    };
    let pulse = {
        let l = l.clone();
        move || -> View { pulse_view(&l) }
    };
    let stage = {
        let l = l.clone();
        move || -> View {
            let (w, h) = l.size.get();
            match l.full.get() {
                Some(FullView::Incoming {
                    group,
                    name,
                    what,
                    agent,
                }) => incoming(&l, &group, &name, &what, agent, w, h),
                Some(FullView::Waiting { title, own, agent }) => {
                    waiting(&l, &title, own, agent, w, h)
                }
                Some(FullView::InCall(v)) => in_call(&l, &v, w, h),
                None => View::Empty,
            }
        }
    };
    let sheet = {
        let l = l.clone();
        move || -> View { sheet_frame(&l) }
    };
    view! {
        <div style={root} on:click={backdrop}>
            {pulse}
            {stage}
            {sheet}
        </div>
    }
}

/// Where the incoming screen's disc stands, and how wide it is.
fn incoming_disc(w: f32, h: f32) -> (Vec2, f32) {
    let disc = (w.min(h) * 0.34).clamp(96.0, 150.0);
    (pos2(w * 0.5, h * 0.30), disc)
}

/// A slow pulse around the ringing initial: the screen is ringing, not
/// frozen. Only this follows the clock.
fn pulse_view(l: &ScreenLive) -> View {
    let ringing = l
        .full
        .peek(|f| matches!(f, Some(FullView::Incoming { .. })));
    let t = l.clock.get();
    if !ringing {
        return View::Empty;
    }
    let (w, h) = l.size.get_untracked();
    let (c, disc) = incoming_disc(w, h);
    View::Fragment(
        (0..2)
            .map(|i| {
                let phase = ((t * 0.6) + i as f32 * 0.5).fract();
                let radius = disc * 0.5 * (1.0 + phase * 0.55);
                let a = (1.0 - phase) * 60.0 / 2.55;
                ring(l.k, c, radius, 2.0, &white(a))
            })
            .collect(),
    )
}

/// A call coming in, over the whole screen: who, large; what; Decline in
/// red on the left and Answer in green on the right, at thumb height.
#[allow(clippy::too_many_arguments)]
fn incoming(
    l: &ScreenLive,
    group: &[u8],
    name: &str,
    what: &str,
    agent: bool,
    w: f32,
    h: f32,
) -> View {
    let k = l.k;
    let pal = &theme::DARK;
    let x = w * 0.5;
    let (c, disc) = incoming_disc(w, h);
    let below = c.y + disc * 0.5;
    let band = w - 2.0 * EDGE;
    let mut out = vec![
        avatar(k, c, name, disc),
        text_at(
            k,
            x,
            below + 40.0,
            band,
            vec![Seg::T(name.into())],
            28.0,
            &css(pal.cream),
        ),
        text_at(
            k,
            x,
            below + 72.0,
            band,
            vec![Seg::T(what.into())],
            theme::SIZE_BODY,
            &css(pal.text),
        ),
        encrypted_line(k, x, below + 96.0, band, agent),
        // Folded, it keeps ringing as a banner at the top.
        fold_button(&l.acts, k),
    ];
    let row_y = h - BOTTOM_EDGE - CALL_BUTTON_LARGE * 0.5 - 28.0;
    let spread = (w * 0.26).clamp(70.0, 150.0);
    let decline = pos2(x - spread, row_y);
    let answer = pos2(x + spread, row_y);
    out.push(call_button(
        &l.acts,
        k,
        pal,
        decline,
        CALL_BUTTON_LARGE,
        icons::CALL_END,
        "Decline",
        CallTone::HangUp,
        vec![CallAct::Request(CallRequest::Dismiss(group.to_vec()))],
    ));
    out.push(call_button(
        &l.acts,
        k,
        pal,
        answer,
        CALL_BUTTON_LARGE,
        icons::CALL,
        "Answer",
        CallTone::Accept,
        vec![CallAct::Request(CallRequest::Join(group.to_vec()))],
    ));
    let label_y = row_y + CALL_BUTTON_LARGE * 0.5 + 16.0;
    for (at, word) in [(decline.x, "Decline"), (answer.x, "Answer")] {
        out.push(text_at(
            k,
            at,
            label_y,
            120.0,
            vec![Seg::T(word.into())],
            theme::SIZE_SUB,
            &css(pal.text),
        ));
    }
    View::Fragment(out)
}

/// Waiting for a host to let this device in: your own picture, if any,
/// behind who you wait for, and Leave.
fn waiting(l: &ScreenLive, title: &str, own: Option<f32>, agent: bool, w: f32, h: f32) -> View {
    let k = l.k;
    let pal = &theme::DARK;
    let full = Rect::from_min_size(Vec2::ZERO, Vec2::new(w, h));
    let x = w * 0.5;
    let band = w - 2.0 * EDGE;
    let mut out = Vec::new();
    if let Some(a) = own {
        out.push(picture(k, &tile_name(SELF_TILE), a, full, 0.0, true));
        out.push(plate(k, full, 0.0, &black(59.0)));
    }
    let y = h * 0.38;
    // The spinner: a faint ring with the gold arc turning on it.
    let r = 18.0;
    let spin = Rect::from_center_size(pos2(x, y), Vec2::splat(2.0 * r + 3.0));
    let spin_style = bx(
        k,
        spin,
        &format!(
            "box-sizing: border-box; border-radius: {:.1}px; border: {:.1}px solid {}; \
             border-top-color: {}; animation: m-spin 1s linear infinite; pointer-events: none",
            (r + 1.5) * k,
            3.0 * k,
            white(16.0),
            css(pal.warn)
        ),
    );
    out.push(view! { <div style={spin_style}></div> });
    out.push(text_at(
        k,
        x,
        y + 54.0,
        band,
        vec![Seg::T(title.into())],
        26.0,
        &css(pal.cream),
    ));
    out.push(text_at(
        k,
        x,
        y + 88.0,
        band,
        vec![Seg::T("Waiting for a host to let you in".into())],
        theme::SIZE_BODY,
        &css(pal.text),
    ));
    out.push(encrypted_line(k, x, EDGE + 20.0, band, agent));
    out.push(fold_button(&l.acts, k));
    let c = pos2(x, h - BOTTOM_EDGE - CALL_BUTTON_LARGE * 0.5 - 8.0);
    out.push(call_button(
        &l.acts,
        k,
        pal,
        c,
        CALL_BUTTON_LARGE,
        icons::CALL_END,
        "Stop waiting",
        CallTone::HangUp,
        vec![CallAct::Request(CallRequest::Leave)],
    ));
    out.push(text_at(
        k,
        c.x,
        c.y + CALL_BUTTON_LARGE * 0.5 + 12.0,
        120.0,
        vec![Seg::T("Leave".into())],
        theme::SIZE_TINY,
        &css(pal.text),
    ));
    View::Fragment(out)
}

/// What a tile says about its person besides the picture: the name with the
/// microphone's state, a raised hand's place, a reaction.
fn tile_badges(out: &mut Vec<View>, k: f32, pal: &Palette, rect: Rect, p: &PersonView) {
    let inner = rect.shrink(8.0);
    let mut segs = Vec::new();
    if p.held {
        segs.push(Seg::I(icons::LOCK));
    } else if p.muted {
        segs.push(Seg::I(icons::MIC_OFF));
    }
    segs.push(Seg::T(p.name.clone()));
    if p.host {
        segs.push(Seg::I(icons::HOST));
    }
    out.push(corner(
        k,
        inner,
        "flex-end",
        chip(segs, &black(63.0), &css(pal.cream)),
    ));
    if let Some(n) = p.hand {
        out.push(corner(
            k,
            inner,
            "flex-start",
            chip(
                vec![Seg::I(icons::HAND), Seg::T(n.to_string())],
                &css(pal.warn),
                &css(pal.ink),
            ),
        ));
    }
    if let Some(r) = &p.reaction {
        out.push(text_at(
            k,
            rect.max.x - 26.0,
            rect.max.y - 30.0,
            80.0,
            vec![Seg::T(r.clone())],
            34.0,
            &css(pal.cream),
        ));
    }
}

/// One person in a grid: their picture, or their initial on a raised tile;
/// the badges; a ring while they talk.
fn grid_tile(out: &mut Vec<View>, k: f32, pal: &Palette, rect: Rect, p: &PersonView) {
    let radius = 14.0;
    match p.picture {
        Some(a) => out.push(picture(k, &tile_name(p.id), a, rect, radius, true)),
        None => {
            out.push(plate(k, rect, radius, &css(pal.surface_raised)));
            let d = (rect.width().min(rect.height()) * 0.42).clamp(32.0, 120.0);
            out.push(avatar(k, rect.center(), &p.name, d));
        }
    }
    tile_badges(out, k, pal, rect, p);
    if p.speaking {
        let style = bx(
            k,
            rect,
            &format!(
                "box-sizing: border-box; border-radius: {:.1}px; border: {:.1}px solid {}; pointer-events: none",
                radius * k,
                3.0 * k,
                css(pal.accent_ring)
            ),
        );
        out.push(view! { <div style={style}></div> });
    }
}

/// `tiles` in an even grid of `cols` columns over `area`.
fn grid(
    out: &mut Vec<View>,
    k: f32,
    pal: &Palette,
    area: Rect,
    tiles: &[&PersonView],
    cols: usize,
) {
    let n = tiles.len().max(1);
    let cols = cols.clamp(1, n);
    let rows = n.div_ceil(cols);
    let gap = 8.0;
    let cell = Vec2::new(
        (area.width() - gap * (cols - 1) as f32) / cols as f32,
        (area.height() - gap * (rows - 1) as f32) / rows as f32,
    );
    for (i, person) in tiles.iter().enumerate() {
        let (r, c) = (i / cols, i % cols);
        let rect = Rect::from_min_size(
            area.min + Vec2::new(c as f32 * (cell.x + gap), r as f32 * (cell.y + gap)),
            cell,
        );
        grid_tile(out, k, pal, rect, person);
    }
}

/// The call you are in, over the whole screen, as `in_call_screen` lays it
/// out.
fn in_call(l: &ScreenLive, v: &InCallView, w: f32, h: f32) -> View {
    let k = l.k;
    let pal = &theme::DARK;
    let full = Rect::from_min_size(Vec2::ZERO, Vec2::new(w, h));
    let x = w * 0.5;
    let mut out: Vec<View> = Vec::new();

    // ── The stage ──────────────────────────────────────────────────────────
    let top_room = 72.0;
    let bottom_room = CALL_BUTTON_LARGE + BOTTOM_EDGE + 30.0;
    let stage = Rect::from_min_max(pos2(8.0, top_room), pos2(w - 8.0, h - bottom_room));
    let mut pip = v.own;
    let mut centre_title = false;
    let big = v.big.and_then(|b| v.people.iter().find(|p| p.id == b));
    if let Some((who, aspect, name)) = &v.screen {
        // A shared screen is the stage: drawn whole, never cropped, with
        // everybody (you too) in a strip beside it or under it.
        pip = None;
        let portrait = stage.height() > stage.width();
        let mut tiles: Vec<&PersonView> = v.people.iter().collect();
        tiles.push(&v.me);
        let (screen_rect, strip) = if portrait {
            let sh = (stage.width() / aspect.max(0.1)).min(stage.height() * 0.55);
            let screen_rect = Rect::from_min_size(stage.min, Vec2::new(stage.width(), sh));
            let strip = Rect::from_min_max(pos2(stage.min.x, screen_rect.max.y + 8.0), stage.max);
            (screen_rect, strip)
        } else {
            let sw = (stage.width() * 0.78).min(stage.width() - 160.0);
            let screen_rect = Rect::from_min_size(stage.min, Vec2::new(sw, stage.height()));
            let strip = Rect::from_min_max(pos2(screen_rect.max.x + 8.0, stage.min.y), stage.max);
            (screen_rect, strip)
        };
        out.push(plate(k, screen_rect, 14.0, &black(63.0)));
        out.push(image(
            k,
            &screen_name(*who),
            *aspect,
            screen_rect,
            10.0,
            false,
        ));
        out.push(corner(
            k,
            screen_rect.shrink(8.0),
            "flex-start",
            chip(
                vec![
                    Seg::I(icons::SCREEN_SHARE),
                    Seg::T(format!(" {name} is presenting")),
                ],
                &black(63.0),
                &css(pal.cream),
            ),
        ));
        let cols = if portrait { 2 } else { 1 };
        grid(&mut out, k, pal, strip, &tiles, cols);
    } else if let Some(big) = big {
        // One person large (the spotlight, or whoever talks), everybody else
        // in a row under them.
        pip = None;
        let strip_h = (stage.height() * 0.22).clamp(70.0, 150.0);
        let big_rect =
            Rect::from_min_max(stage.min, pos2(stage.max.x, stage.max.y - strip_h - 8.0));
        let strip = Rect::from_min_max(pos2(stage.min.x, stage.max.y - strip_h), stage.max);
        grid(&mut out, k, pal, big_rect, &[big], 1);
        let mut rest: Vec<&PersonView> = v.people.iter().filter(|p| p.id != big.id).collect();
        rest.push(&v.me);
        let cols = rest.len();
        grid(&mut out, k, pal, strip, &rest, cols);
    } else {
        match v.people.len() {
            0 => {
                // Still ringing them: your own camera is the backdrop.
                if let Some(a) = v.own {
                    out.push(picture(k, &tile_name(SELF_TILE), a, full, 0.0, true));
                    out.push(plate(k, full, 0.0, &black(27.0)));
                    pip = None;
                }
                centre_title = true;
            }
            1 => {
                let person = &v.people[0];
                match person.picture {
                    Some(a) => {
                        out.push(picture(k, &tile_name(person.id), a, full, 0.0, false));
                        tile_badges(&mut out, k, pal, stage, person);
                    }
                    None => centre_title = true,
                }
            }
            _ => {
                // You are one of the tiles in a group, last.
                pip = None;
                let mut tiles: Vec<&PersonView> = v.people.iter().collect();
                tiles.push(&v.me);
                let n = tiles.len();
                let portrait = stage.height() > stage.width();
                let cols = if portrait {
                    if n <= 3 {
                        1
                    } else {
                        2
                    }
                } else {
                    (n as f32).sqrt().ceil() as usize
                };
                grid(&mut out, k, pal, stage, &tiles, cols);
            }
        }
    }
    let band = w - 2.0 * EDGE;
    if centre_title {
        // No picture to look at: the person, large, in the middle.
        let disc = (w.min(h) * 0.34).clamp(96.0, 150.0);
        let c = pos2(x, h * 0.34);
        if v.people.first().is_some_and(|p| p.speaking) {
            out.push(ring(k, c, disc * 0.5 + 6.0, 3.0, &css(pal.accent_ring)));
        }
        out.push(avatar(k, c, &v.title, disc));
        if let Some(r) = v.people.first().and_then(|p| p.reaction.clone()) {
            out.push(text_at(
                k,
                c.x + disc * 0.42,
                c.y + disc * 0.36,
                80.0,
                vec![Seg::T(r)],
                40.0,
                &css(pal.cream),
            ));
        }
        out.push(text_at(
            k,
            x,
            c.y + disc * 0.5 + 40.0,
            band,
            vec![Seg::T(v.title.clone())],
            28.0,
            &css(pal.cream),
        ));
        out.push(text_at(
            k,
            x,
            c.y + disc * 0.5 + 72.0,
            band,
            vec![Seg::D(l.status.clone())],
            theme::SIZE_BODY,
            &css(pal.text),
        ));
    }

    // ── Your own picture, in the corner ────────────────────────────────────
    if let Some(a) = pip {
        let pw = (w * 0.28).clamp(90.0, 180.0);
        let ph = (pw / a.max(0.1)).min(h * 0.32);
        let pw = ph * a;
        let top = if v.controls { 76.0 } else { EDGE };
        let rect = Rect::from_min_size(pos2(w - EDGE - pw, top), Vec2::new(pw, ph));
        out.push(plate(k, rect.expand(1.0), 13.0, &black(47.0)));
        out.push(picture(k, &tile_name(SELF_TILE), a, rect, 12.0, true));
        let rim = bx(
            k,
            rect.expand(1.0),
            &format!(
                "box-sizing: border-box; border-radius: {:.1}px; border: 1px solid {}; pointer-events: none",
                13.0 * k,
                white(24.0)
            ),
        );
        out.push(view! { <div style={rim}></div> });
        if v.me.muted {
            let b = pos2(rect.min.x + 16.0, rect.max.y - 16.0);
            let r = Rect::from_center_size(b, Vec2::splat(24.0));
            let style = bx(
                k,
                r,
                &format!(
                    "border-radius: {:.1}px; background: {}; color: {}; display: flex; align-items: center; \
                     justify-content: center; font-family: 'Material Symbols Rounded'; font-size: 14px; \
                     line-height: 1; pointer-events: none",
                    12.0 * k,
                    css(pal.cream),
                    css(pal.ink)
                ),
            );
            out.push(view! { <div style={style}>{icons::MIC_OFF}</div> });
        }
        if let Some(r) = &v.me.reaction {
            out.push(text_at(
                k,
                rect.max.x - 18.0,
                rect.max.y - 20.0,
                60.0,
                vec![Seg::T(r.clone())],
                26.0,
                &css(pal.cream),
            ));
        }
    }

    if v.controls {
        // ── The top: fold, name, timer, people ─────────────────────────────
        if v.any_picture {
            out.push(shade(
                k,
                Rect::from_min_size(full.min, Vec2::new(w, 120.0)),
                true,
            ));
            out.push(shade(
                k,
                Rect::from_min_max(pos2(0.0, h - 180.0), full.max),
                false,
            ));
        }
        out.push(fold_button(&l.acts, k));
        out.push(people_button(l, v, w));
        // Between the two round buttons.
        let title_band = (w - 2.0 * (EDGE + 48.0)).max(40.0);
        if centre_title {
            out.push(encrypted_line(k, x, EDGE + 20.0, title_band, v.agent));
        } else {
            out.push(text_at(
                k,
                x,
                EDGE + 14.0,
                title_band,
                vec![Seg::T(v.title.clone())],
                theme::SIZE_HEADING,
                &css(pal.cream),
            ));
            out.push(text_at(
                k,
                x,
                EDGE + 38.0,
                title_band,
                vec![Seg::I(icons::LOCK), Seg::D(l.status.clone())],
                theme::SIZE_SUB,
                &css(pal.text),
            ));
        }
        dock(&mut out, l, v, w, h);
        meeting_chips(&mut out, l, v, w);
    }

    // ── Captions, above the keys, whether or not the keys show ─────────────
    if let Some((who, text)) = &v.caption {
        let cw = (w - 2.0 * EDGE).min(640.0);
        let bottom = if v.controls {
            h - BOTTOM_EDGE - CALL_BUTTON_LARGE - 40.0
        } else {
            h - BOTTOM_EDGE
        };
        let style = format!(
            "position: absolute; left: {:.1}px; bottom: {:.1}px; width: {:.1}px; box-sizing: border-box; \
             padding: 8px 14px 10px; border-radius: 14px; background: {}; display: flex; \
             flex-direction: column; gap: 2px; pointer-events: none",
            (x - cw * 0.5) * k,
            (h - bottom) * k,
            cw * k,
            black(80.0)
        );
        let who_style = format!(
            "font-size: {}px; color: {}",
            theme::SIZE_TINY,
            css(pal.accent_ring)
        );
        let text_style = format!(
            "font-size: {}px; line-height: 1.35; color: {}",
            theme::SIZE_BODY + 2.0,
            css(pal.cream)
        );
        let (who, text) = (who.clone(), text.clone());
        out.push(view! {
            <div style={style}>
                <span style={who_style}>{who}</span>
                <span style={text_style}>{text}</span>
            </div>
        });
    }
    View::Fragment(out)
}

/// The people button, top right: how many are in the call, and a gold count
/// of raised hands (and of people waiting, for a host).
fn people_button(l: &ScreenLive, v: &InCallView, w: f32) -> View {
    let k = l.k;
    let pal = &theme::DARK;
    let d = 40.0;
    let c = pos2(w - EDGE - d * 0.5, EDGE + d * 0.5);
    let r = Rect::from_center_size(c, Vec2::splat(d));
    let style = bx(
        k,
        r,
        &format!(
            "border-radius: {:.1}px; background: {}; color: {}; display: flex; align-items: center; \
             justify-content: center; cursor: pointer",
            d * 0.5 * k,
            white(12.0),
            css(pal.cream)
        ),
    );
    let badge: View = match v.badge {
        Some((count, hands)) => {
            let (fill, ink) = if hands {
                (css(pal.warn), css(pal.ink))
            } else {
                (css(pal.surface_raised), css(pal.cream))
            };
            // `c + (0.38 d, −0.36 d)` in the button's own frame.
            let b = pos2(d * 0.5 + d * 0.38, d * 0.5 - d * 0.36);
            let br = Rect::from_center_size(b, Vec2::splat(18.0));
            let style = bx(
                k,
                br,
                &format!(
                    "border-radius: {:.1}px; background: {fill}; color: {ink}; display: flex; \
                     align-items: center; justify-content: center; font-size: {}px; line-height: 1; \
                     pointer-events: none",
                    9.0 * k,
                    theme::SIZE_TINY
                ),
            );
            let n = count.to_string();
            view! { <span style={style}>{n}</span> }
        }
        None => View::Empty,
    };
    let open = on(
        &l.acts,
        vec![CallAct::Sheet(Some(Sheet::People)), CallAct::Pick(None)],
    );
    view! {
        <div title="People in the call" style={style} on:click={tap(open)}>
            <span style="font-family: 'Material Symbols Rounded'; font-size: 22px; line-height: 1">{icons::GROUP}</span>
            {badge}
        </div>
    }
}

/// The keys along the bottom: microphone, camera, hand, more, leave, each
/// with its word under it.
fn dock(out: &mut Vec<View>, l: &ScreenLive, v: &InCallView, w: f32, h: f32) {
    const LABEL_GAP: f32 = 16.0;
    let k = l.k;
    let pal = &theme::DARK;
    let x = w * 0.5;
    let y = h - BOTTOM_EDGE - CALL_BUTTON_LARGE * 0.5 - LABEL_GAP * 0.5;
    let dk = &v.dock;
    let n = if dk.camera.is_some() { 5.0 } else { 4.0 };
    // Narrow phones fit five keys by tightening the step, not the keys.
    let step = (CALL_BUTTON_LARGE + 18.0).min((w - 2.0 * EDGE - 16.0) / n);
    let d = CALL_BUTTON_LARGE.min(step - 8.0);
    let mut cx = x - (n - 1.0) * step * 0.5;
    let tray = Rect::from_center_size(
        pos2(x, y + LABEL_GAP * 0.5),
        Vec2::new((n - 1.0) * step + d + 24.0, d + 20.0 + LABEL_GAP),
    );
    out.push(plate(k, tray, tray.height() * 0.5, &black(51.0)));
    let label = |cx: f32, word: &str| {
        text_at(
            k,
            cx,
            y + d * 0.5 + LABEL_GAP * 0.5 + 2.0,
            step,
            vec![Seg::T(word.into())],
            theme::SIZE_TINY,
            &css(pal.text),
        )
    };
    let button = |cx: f32, glyph: &'static str, tip: &str, tone: CallTone, list: Vec<CallAct>| {
        call_button(&l.acts, k, pal, pos2(cx, y), d, glyph, tip, tone, list)
    };
    let (glyph, tip, tone, word) = if dk.held {
        (icons::LOCK, "A host muted you", CallTone::Off, "Held")
    } else if dk.muted {
        (icons::MIC_OFF, "Unmute", CallTone::Off, "Unmute")
    } else {
        (icons::MIC, "Mute", CallTone::On, "Mute")
    };
    out.push(button(
        cx,
        glyph,
        tip,
        tone,
        vec![CallAct::DockMute, CallAct::Touch],
    ));
    out.push(label(cx, word));
    cx += step;
    if let Some(on_now) = dk.camera {
        let (glyph, tip, tone) = if on_now {
            (icons::VIDEOCAM, "Turn the camera off", CallTone::On)
        } else {
            (icons::VIDEOCAM_OFF, "Turn the camera on", CallTone::Off)
        };
        out.push(button(
            cx,
            glyph,
            tip,
            tone,
            vec![CallAct::ToggleCamera, CallAct::Touch],
        ));
        out.push(label(cx, "Camera"));
        cx += step;
    }
    if dk.raised {
        out.push(ring(k, pos2(cx, y), d * 0.5 + 3.0, 3.0, &css(pal.warn)));
    }
    let tip = if dk.raised {
        "Lower your hand"
    } else {
        "Raise your hand"
    };
    out.push(button(
        cx,
        icons::HAND,
        tip,
        CallTone::On,
        vec![
            CallAct::Request(CallRequest::Control(CallAction::Hand(!dk.raised))),
            CallAct::Touch,
        ],
    ));
    out.push(label(cx, if dk.raised { "Lower" } else { "Hand" }));
    cx += step;
    out.push(button(
        cx,
        icons::MORE,
        "React, and more",
        CallTone::On,
        vec![CallAct::Sheet(Some(Sheet::More)), CallAct::Touch],
    ));
    out.push(label(cx, "More"));
    cx += step;
    out.push(button(
        cx,
        icons::CALL_END,
        "Leave",
        CallTone::HangUp,
        vec![CallAct::LeaveButton],
    ));
    out.push(label(cx, "Leave"));
}

/// What the meeting says about everybody, under the title: somebody
/// records, your screen goes out, you are in a breakout room.
fn meeting_chips(out: &mut Vec<View>, l: &ScreenLive, v: &InCallView, w: f32) {
    let k = l.k;
    let pal = &theme::DARK;
    let mut y = EDGE + 60.0;
    // A band the window's width, centred on `y`, its content centred in it.
    let band = |y: f32, child: View| -> View {
        let r = Rect::from_center_size(pos2(w * 0.5, y), Vec2::new(w, 32.0));
        let style = bx(
            k,
            r,
            "display: flex; align-items: center; justify-content: center; gap: 6px; pointer-events: none",
        );
        view! { <div style={style}>{child}</div> }
    };
    let pill = |pad_y: f32, bg: String, ink: String| -> String {
        format!(
            "display: flex; align-items: center; gap: 4px; padding: {pad_y}px 10px; border-radius: 999px; \
             background: {bg}; color: {ink}; white-space: nowrap"
        )
    };
    if let Some(text) = &v.chips.recording {
        let style = pill(4.0, alpha(theme::CALL_END_RED, 80.0), css(pal.cream));
        let text = text.clone();
        out.push(band(
            y,
            view! { <div style={style}>{line(vec![Seg::T(text)], theme::SIZE_SUB)}</div> },
        ));
        y += 30.0;
    }
    // Your own screen going out: said where it cannot be missed, with the
    // way to stop it in it.
    if let Some(what) = v.chips.sharing {
        let style = format!(
            "{}; pointer-events: auto; cursor: pointer",
            pill(6.0, alpha(theme::CALL_END_RED, 85.0), css(pal.cream))
        );
        let stop = on(&l.acts, vec![CallAct::Request(CallRequest::Share(false))]);
        let segs = vec![
            Seg::I(icons::SCREEN_SHARE),
            Seg::T(format!(" {what} · Stop")),
        ];
        out.push(band(
            y,
            view! { <div title="Stop sharing your screen" style={style} on:click={tap(stop)}>{line(segs, theme::SIZE_SUB)}</div> },
        ));
        y += 34.0;
    }
    if let Some((text, may_move)) = &v.chips.room {
        let style = pill(4.0, css(pal.accent), css(pal.ink));
        let segs = vec![Seg::I(icons::GROUP), Seg::T(format!(" {text}"))];
        let back: View = if *may_move {
            let style = format!(
                "{}; pointer-events: auto; cursor: pointer",
                pill(4.0, white(12.0), css(pal.cream))
            );
            let go = on(&l.acts, vec![CallAct::RoomBack]);
            view! {
                <div title="Back to the call" style={style} on:click={tap(go)}>{line(vec![Seg::T("Back to the call".into())], theme::SIZE_SUB)}</div>
            }
        } else {
            View::Empty
        };
        out.push(band(
            y,
            view! {
                <div style={style}>{line(segs, theme::SIZE_SUB)}</div>
                {back}
            },
        ));
    }
}

// ─── The sheet ───────────────────────────────────────────────────────────────

/// The sheet over the call screen: the shade (a tap closes it), and the
/// sheet pinned to the bottom edge, its contents scrolling. Follows only
/// which sheet is open, so the note field keeps its focus while the
/// agenda's clock runs.
fn sheet_frame(l: &ScreenLive) -> View {
    let Some(kind) = l.sheet_kind.get() else {
        return View::Empty;
    };
    let (w, h) = l.size.get_untracked();
    let k = l.k;
    let pal = theme::p();
    let shade = format!(
        "position: absolute; left: 0; top: 0; width: {:.1}px; height: {:.1}px; background: {}",
        w * k,
        h * k,
        black(55.0)
    );
    let close = on(&l.acts, vec![CallAct::Sheet(None)]);
    let sw = w.min(520.0);
    let frame = format!(
        "position: absolute; left: {:.1}px; bottom: 0; width: {:.1}px; max-height: {:.1}px; box-sizing: border-box; \
         display: flex; flex-direction: column; padding: 10px 16px 28px; background: {}; \
         border: 1px solid {}; border-radius: 22px 22px 0 0",
        (w - sw) * 0.5 * k,
        sw * k,
        h * 0.8 * k,
        css(pal.surface),
        css(pal.overlay_line)
    );
    let grab = format!(
        "width: 38px; height: 5px; border-radius: 3px; background: {}",
        css(pal.text_dim)
    );
    let body: View = if kind == Sheet::Notes {
        notes_sheet(l)
    } else {
        let l = l.clone();
        let contents = move || -> View {
            match l.sheet.get() {
                Some(SheetView::More(m)) => more_sheet(&l, &m),
                Some(SheetView::People(p)) => people_sheet(&l, &p),
                Some(SheetView::Leave { others }) => leave_sheet(&l, others),
                _ => View::Empty,
            }
        };
        view! { {contents} }
    };
    view! {
        <div style={shade} title="Dismiss" on:click={tap(close)}></div>
        <div style={frame} on:click={|_: &Event| {}}>
            <div style="flex: none; height: 14px; display: flex; align-items: center; justify-content: center">
                <div style={grab}></div>
            </div>
            <div style="flex: 1; min-height: 0; overflow-y: auto; display: flex; flex-direction: column; gap: 6px">
                {body}
            </div>
        </div>
    }
}

/// A switch's look at a row's end, as `sheet_row` paints it.
fn switch(on: bool) -> View {
    let pal = theme::p();
    let track = format!(
        "position: relative; flex: none; width: 34px; height: 20px; border-radius: 10px; background: {}",
        css(if on { pal.accent } else { pal.surface_raised })
    );
    let knob = format!(
        "position: absolute; top: 3px; left: {}px; width: 14px; height: 14px; border-radius: 7px; background: {}",
        if on { 17 } else { 3 },
        css(if on { pal.on_accent } else { pal.text_dim })
    );
    view! { <span style={track}><span style={knob}></span></span> }
}

/// A row of a sheet: icon, words, and a switch's state when it has one.
fn row(
    acts: &Acts,
    glyph: &'static str,
    words: impl Into<String>,
    on_now: Option<bool>,
    list: Vec<CallAct>,
) -> View {
    let words: String = words.into();
    let end: View = match on_now {
        Some(v) => switch(v),
        None => View::Empty,
    };
    let click = Some(on(acts, list));
    view! { <ListItem icon={icon(glyph)} end={end} onclick={click}>{words}</ListItem> }
}

/// A sheet's title.
fn title(text: String) -> View {
    let style = format!(
        "margin: 0 0 4px; font-size: {}px; font-weight: 600; color: {}",
        theme::SIZE_PANEL_TITLE,
        css(theme::p().cream)
    );
    view! { <h2 style={style}>{text}</h2> }
}

/// A small button: the accent one when `primary`, as `theme::chip`.
fn small(acts: &Acts, words: impl Into<String>, primary: bool, list: Vec<CallAct>) -> View {
    let words: String = words.into();
    let variant = if primary {
        Variant::Primary
    } else {
        Variant::Secondary
    };
    let click = Some(on(acts, list));
    view! { <Button size=Size::Sm variant={variant} onclick={click}>{words}</Button> }
}

/// A host's setting as a switch row: the call's settings with it flipped.
fn setting(
    acts: &Acts,
    glyph: &'static str,
    words: &str,
    s: CallSettings,
    on_now: bool,
    flip: fn(&mut CallSettings, bool),
) -> View {
    let mut next = s;
    flip(&mut next, !on_now);
    row(
        acts,
        glyph,
        words,
        Some(on_now),
        vec![CallAct::Request(CallRequest::Control(
            CallAction::Settings(next),
        ))],
    )
}

/// Reactions, and the call's other actions; a host's controls at the end.
fn more_sheet(l: &ScreenLive, m: &MoreView) -> View {
    let acts = &l.acts;
    let close = CallAct::Sheet(None);
    let key = format!(
        "flex: none; width: 48px; height: 48px; display: flex; align-items: center; justify-content: center; \
         border-radius: 12px; background: {}; font-size: 26px; line-height: 1; cursor: pointer",
        css(theme::p().surface_raised)
    );
    let reactions: Vec<View> = REACTIONS
        .iter()
        .map(|r| {
            let style = key.clone();
            let go = on(
                acts,
                vec![
                    CallAct::Request(CallRequest::React((*r).to_string())),
                    close.clone(),
                ],
            );
            let r = *r;
            view! { <div title={r} style={style} on:click={tap(go)}>{r}</div> }
        })
        .collect();
    let mut call: Vec<View> = vec![
        row(
            acts,
            icons::GROUP,
            "People",
            None,
            vec![CallAct::Sheet(Some(Sheet::People))],
        ),
        row(
            acts,
            icons::NOTE,
            "Notes and agenda",
            None,
            vec![CallAct::Sheet(Some(Sheet::Notes))],
        ),
        row(
            acts,
            icons::STAR,
            "Speaker view",
            Some(m.speaker_view),
            vec![CallAct::ToggleSpeakerView],
        ),
        // Off for music, or a room where the background is the point.
        row(
            acts,
            icons::MIC,
            "Noise suppression",
            Some(m.noise),
            vec![CallAct::ToggleNoise],
        ),
    ];
    // Your avatar in your camera's place, pulling your faces.
    if let Some(on_now) = m.avatar {
        call.push(row(
            acts,
            icons::APPEARANCE,
            "Show your avatar instead",
            Some(on_now),
            vec![CallAct::ToggleAvatar],
        ));
    }
    call.push(row(
        acts,
        icons::SCREEN_SHARE,
        "Share your screen",
        Some(m.sharing),
        vec![
            CallAct::Request(CallRequest::Share(!m.sharing)),
            close.clone(),
        ],
    ));
    let hosting: View = match &m.hosting {
        None => View::Empty,
        Some(host) => {
            let s = host.settings;
            let invite = vec![
                row(
                    acts,
                    icons::MIC_OFF,
                    "Mute everyone",
                    None,
                    vec![
                        CallAct::Request(CallRequest::Control(CallAction::MuteAll)),
                        close.clone(),
                    ],
                ),
                // A link for somebody without an account: they wait to be
                // let in, and this device hands them the call's key.
                row(
                    acts,
                    icons::GROUP,
                    "Invite a guest",
                    None,
                    vec![CallAct::Request(CallRequest::InviteGuest)],
                ),
            ];
            let link: View = match &host.invited {
                Some(link) => {
                    let shown = link.clone();
                    let copy = Some(on(acts, vec![CallAct::CopyLink(link.clone())]));
                    let style = format!(
                        "margin: 0; padding-inline: 4px; font-size: {}px; color: {}; word-break: break-all",
                        theme::SIZE_SUB,
                        css(theme::p().text_dim)
                    );
                    view! {
                        {note("Guest link, good for a day. Anybody with it can ask to join.")}
                        <p style={style}>{shown}</p>
                        <div style="display: flex">
                            <Button size=Size::Sm variant=Variant::Secondary icon={icon(icons::COPY)} onclick={copy}>"Copy the guest link"</Button>
                        </div>
                    }
                }
                None => View::Empty,
            };
            let settings = vec![
                setting(acts, icons::LOCK, "Lock the call", s, s.locked, |s, v| {
                    s.locked = v;
                }),
                setting(
                    acts,
                    icons::MIC,
                    "Others may unmute",
                    s,
                    s.may_unmute,
                    |s, v| {
                        s.may_unmute = v;
                    },
                ),
                setting(
                    acts,
                    icons::SCREEN_SHARE,
                    "Others may share a screen",
                    s,
                    s.may_share,
                    |s, v| {
                        s.may_share = v;
                    },
                ),
                setting(
                    acts,
                    icons::HOST,
                    "Members wait to be let in",
                    s,
                    s.members_wait,
                    |s, v| {
                        s.members_wait = v;
                    },
                ),
                setting(
                    acts,
                    icons::DOT,
                    "Anybody may record",
                    s,
                    s.may_record,
                    |s, v| {
                        s.may_record = v;
                    },
                ),
            ];
            let mut rooms: Vec<View> = Vec::new();
            if host.rooms_open {
                rooms.push(row(
                    acts,
                    icons::GROUP,
                    "Bring everybody back",
                    None,
                    vec![
                        CallAct::Request(CallRequest::Control(CallAction::CloseRooms)),
                        close.clone(),
                    ],
                ));
            } else {
                for n in [2u32, 3, 4] {
                    rooms.push(row(
                        acts,
                        icons::GROUP,
                        format!("Split into {n} rooms for 10 minutes"),
                        None,
                        vec![CallAct::SplitRooms(n), close.clone()],
                    ));
                }
            }
            let end = row(
                acts,
                icons::CALL_END,
                "End for everyone",
                None,
                vec![
                    CallAct::Request(CallRequest::Control(CallAction::EndForAll)),
                    close.clone(),
                ],
            );
            view! {
                {heading("Hosting")}
                <List>{invite}</List>
                {link}
                <List>{settings}</List>
                {heading("Breakout rooms")}
                <List>{rooms}</List>
                <List>{end}</List>
            }
        }
    };
    view! {
        {heading("React")}
        <div style="display: flex; flex-wrap: wrap; gap: 6px">{reactions}</div>
        {heading("In this call")}
        <List>{call}</List>
        {hosting}
    }
}

/// Everybody in the call: who waits (for a host), raised hands in the order
/// they went up, then everyone; a host taps a person for what can be done.
fn people_sheet(l: &ScreenLive, p: &PeopleView) -> View {
    let acts = &l.acts;
    let pal = theme::p();
    let words = |text: String, colour: String| -> View {
        let style = format!(
            "flex: 1; min-width: 0; font-size: {}px; color: {colour}; white-space: nowrap; overflow: hidden; text-overflow: ellipsis",
            theme::SIZE_BODY
        );
        view! { <span style={style}>{text}</span> }
    };
    let row_style =
        "display: flex; align-items: center; gap: 8px; min-height: 36px; padding: 0 4px";
    let waiting: View = if p.hosting && !p.waiting.is_empty() {
        let rows: Vec<View> = p
            .waiting
            .iter()
            .map(|(participant, who)| {
                let participant = *participant;
                let admit = small(
                    acts,
                    "Admit",
                    true,
                    vec![CallAct::Request(CallRequest::Control(CallAction::Admit {
                        participant,
                    }))],
                );
                let deny = small(
                    acts,
                    "Turn away",
                    false,
                    vec![CallAct::Request(CallRequest::Control(CallAction::Deny {
                        participant,
                    }))],
                );
                let name = words(who.clone(), css(pal.text));
                view! { <div style={row_style}>{name}{admit}{deny}</div> }
            })
            .collect();
        let all: View = if p.waiting.len() > 1 {
            let b = small(
                acts,
                "Admit everyone",
                false,
                vec![CallAct::Request(CallRequest::Control(CallAction::AdmitAll))],
            );
            view! { <div style="display: flex">{b}</div> }
        } else {
            View::Empty
        };
        view! {
            {heading(format!("Waiting to join · {}", p.waiting.len()))}
            {rows}
            {all}
        }
    } else {
        View::Empty
    };
    let hands: View = if p.hands.is_empty() {
        View::Empty
    } else {
        let rows: Vec<View> = p
            .hands
            .iter()
            .map(|hr| {
                let participant = hr.participant;
                let place_style = format!(
                    "flex: none; display: flex; align-items: center; gap: 4px; color: {}",
                    css(pal.warn)
                );
                let place = line(
                    vec![Seg::I(icons::HAND), Seg::T(hr.place.to_string())],
                    theme::SIZE_BODY,
                );
                let name = words(hr.name.clone(), css(pal.text));
                let buttons: View = if p.hosting {
                    let talk = small(
                        acts,
                        "Let talk",
                        true,
                        vec![CallAct::Request(CallRequest::Control(
                            CallAction::AllowTalk { participant },
                        ))],
                    );
                    let lower = small(
                        acts,
                        "Lower",
                        false,
                        vec![CallAct::Request(CallRequest::Control(
                            CallAction::LowerHand { participant },
                        ))],
                    );
                    view! { {talk}{lower} }
                } else if hr.mine {
                    small(
                        acts,
                        "Lower",
                        false,
                        vec![CallAct::Request(CallRequest::Control(CallAction::Hand(
                            false,
                        )))],
                    )
                } else {
                    View::Empty
                };
                view! {
                    <div style={row_style}>
                        <span style={place_style}>{place}</span>
                        {name}
                        {buttons}
                    </div>
                }
            })
            .collect();
        view! {
            {heading("Hands up")}
            {rows}
        }
    };
    let members: Vec<View> = p
        .members
        .iter()
        .map(|m| {
            let end: View = if m.glyphs.is_empty() {
                View::Empty
            } else {
                let style = format!("display: flex; gap: 6px; color: {}", css(pal.text_dim));
                let glyphs = line(m.glyphs.iter().map(|g| Seg::I(*g)).collect(), theme::SIZE_BODY);
                view! { <span style={style}>{glyphs}</span> }
            };
            let click = (p.hosting && !m.you).then(|| {
                let next = if m.picked { None } else { Some(m.participant) };
                on(acts, vec![CallAct::Pick(next)])
            });
            let tone = m.picked.then_some(Tone::Accent);
            let label = m.label.clone();
            let item = view! { <ListItem icon={icon(icons::DOT)} end={end} tone={tone} onclick={click}>{label}</ListItem> };
            let actions: View = if m.actions.is_empty() {
                View::Empty
            } else {
                let rows: Vec<View> = m
                    .actions
                    .iter()
                    .map(|(glyph, words, list)| row(acts, *glyph, words.clone(), None, list.clone()))
                    .collect();
                view! { <div style="padding-left: 16px"><List>{rows}</List></div> }
            };
            view! { {item}{actions} }
        })
        .collect();
    let tip: View = if p.hosting {
        note("Tap a person for what a host can do.")
    } else {
        View::Empty
    };
    view! {
        {title(format!("People · {}", p.count))}
        {waiting}
        {hands}
        {heading("In the call")}
        <List>{members}</List>
        {tip}
    }
}

/// A host leaving a call others are still in: leave it to them, or end it.
fn leave_sheet(l: &ScreenLive, others: usize) -> View {
    let acts = &l.acts;
    let close = CallAct::Sheet(None);
    let rows = vec![
        row(
            acts,
            icons::BACK,
            "Leave — the call goes on",
            None,
            vec![CallAct::Request(CallRequest::Leave), close.clone()],
        ),
        row(
            acts,
            icons::CALL_END,
            format!("End for everyone ({others} others)"),
            None,
            vec![
                CallAct::Request(CallRequest::Control(CallAction::EndForAll)),
                close.clone(),
            ],
        ),
        row(acts, icons::CLEAR, "Cancel", None, vec![close]),
    ];
    view! {
        {title("Leave the call?".into())}
        <List>{rows}</List>
    }
}

/// A tint as CSS, on the dark call screen.
fn tint(t: Tint) -> String {
    let pal = theme::p();
    css(match t {
        Tint::Accent => pal.accent,
        Tint::Warn => pal.warn,
        Tint::Dim => pal.text_dim,
        Tint::Text => pal.text,
    })
}

/// Notes, decisions and actions taken in the call, and the agenda; the
/// field to add one stays put while the rest follows the view.
fn notes_sheet(l: &ScreenLive) -> View {
    let upper = {
        let l = l.clone();
        move || -> View {
            match l.sheet.get() {
                Some(SheetView::Notes(n)) => notes_upper(&l, &n),
                _ => View::Empty,
            }
        }
    };
    let kinds = {
        let (kind, acts) = (l.note_kind.clone(), l.acts.clone());
        move || -> View {
            let now = kind.get();
            let chips: Vec<View> = [NoteKind::Note, NoteKind::Decision, NoteKind::Action]
                .into_iter()
                .map(|k| {
                    small(
                        &acts,
                        k.label().to_string(),
                        k == now,
                        vec![CallAct::NoteKind(k)],
                    )
                })
                .collect();
            view! { <div style="display: flex; gap: 6px">{chips}</div> }
        }
    };
    let field = {
        let (kind, draft) = (l.note_kind.clone(), l.draft.clone());
        move || -> View {
            let hint = match kind.get() {
                NoteKind::Action => "Who does what, by when",
                NoteKind::Decision => "What was decided",
                NoteKind::Note => "A note for everybody",
            };
            let model = draft.clone();
            view! { <FieldTextarea model={model} rows={2_u32} placeholder={hint} id="call-note" /> }
        }
    };
    let add = {
        let (draft, acts) = (l.draft.clone(), l.acts.clone());
        Rc::new(move |_: &Event| {
            let text = draft.get_untracked();
            if text.trim().is_empty() {
                return;
            }
            acts.borrow_mut().push(CallAct::AddNote(text));
            draft.set(String::new());
        }) as Handler
    };
    let lower = {
        let l = l.clone();
        move || -> View {
            let hosting = matches!(l.sheet.get(), Some(SheetView::Notes(n)) if n.hosting);
            let record: View = if hosting {
                row(
                    &l.acts,
                    icons::RECORD,
                    "Post the record now",
                    None,
                    vec![CallAct::PostRecord],
                )
            } else {
                View::Empty
            };
            view! {
                <List>{record}</List>
                {note("Posted in the chat, sealed. When the meeting ends a host's device gathers them into its record.")}
            }
        }
    };
    view! {
        {upper}
        {kinds}
        {field}
        {cell(view! { <Button variant=Variant::Primary onclick={Some(add)}>"Add"</Button> })}
        {lower}
    }
}

/// The agenda (the plan this call belongs to, and where it is) and the
/// notes taken while the call runs.
fn notes_upper(l: &ScreenLive, n: &NotesView) -> View {
    let acts = &l.acts;
    let pal = theme::p();
    let agenda: View = match &n.agenda {
        None => {
            note("No agenda. A meeting planned in this chat for about now brings its agenda here.")
        }
        Some(a) => {
            let head = format!(
                "font-size: {}px; font-weight: 600; color: {}",
                theme::SIZE_BODY,
                css(pal.cream)
            );
            let items: Vec<View> = a
                .items
                .iter()
                .map(|item| {
                    let colour = if item.current {
                        tint(Tint::Accent)
                    } else if item.done {
                        tint(Tint::Dim)
                    } else {
                        tint(Tint::Text)
                    };
                    let style = format!(
                        "flex: 1; min-width: 0; font-size: 14px; color: {colour}; {}{}",
                        if item.current { "font-weight: 600; " } else { "" },
                        if item.done { "text-decoration: line-through" } else { "" }
                    );
                    let right: View = match &item.right {
                        Some((text, t)) => {
                            let style = format!("flex: none; font-size: 12.5px; color: {}", tint(*t));
                            let text = text.clone();
                            view! { <span style={style}>{text}</span> }
                        }
                        None => View::Empty,
                    };
                    let label = item.label.clone();
                    view! {
                        <div style="display: flex; align-items: center; gap: 8px; min-height: 28px; padding: 0 4px">
                            <span style={style}>{label}</span>
                            {right}
                        </div>
                    }
                })
                .collect();
            let mut controls: Vec<View> = Vec::new();
            if let Some((label, act)) = &a.next {
                controls.push(small(acts, label.clone(), true, vec![act.clone()]));
            }
            if let Some(act) = &a.stop {
                controls.push(small(acts, "Stop", false, vec![act.clone()]));
            }
            let controls: View = if controls.is_empty() {
                View::Empty
            } else {
                view! { <div style="display: flex; gap: 6px; padding-top: 4px">{controls}</div> }
            };
            let title = a.title.clone();
            view! {
                <span style={head}>{title}</span>
                {items}
                {controls}
            }
        }
    };
    let notes: View = if n.notes.is_empty() {
        note("Nothing noted yet. What is said, decided and who does what goes here, for everybody.")
    } else {
        View::Fragment(
            n.notes
                .iter()
                .map(|(kind, t, text)| {
                    let kind_style = format!(
                        "flex: none; font-size: 11px; font-weight: 700; color: {}",
                        tint(*t)
                    );
                    let text_style = format!("font-size: 13.5px; color: {}", tint(Tint::Text));
                    let (kind, text) = (kind.clone(), text.clone());
                    view! {
                        <div style="display: flex; flex-wrap: wrap; align-items: baseline; gap: 6px; padding: 2px 4px">
                            <span style={kind_style}>{kind}</span>
                            <span style={text_style}>{text}</span>
                        </div>
                    }
                })
                .collect(),
        )
    };
    view! {
        {heading("Agenda")}
        {agenda}
        {heading("Notes")}
        {notes}
    }
}

// ─── The top: the folded call, ringing banners, the notice ───────────────────

/// The column at the top, centred, as wide as the bar.
fn bar_view(l: &BarLive) -> View {
    let content = {
        let l = l.clone();
        move || -> View {
            match l.top.get() {
                Some((t, w)) => top_column(&l, &t, w),
                None => View::Empty,
            }
        }
    };
    view! {
        <div style="position: absolute; left: 0; top: 0; right: 0; bottom: 0; pointer-events: none">
            {content}
        </div>
    }
}

fn top_column(l: &BarLive, t: &TopView, w: f32) -> View {
    let k = l.k;
    let pal = theme::p();
    let style = format!(
        "position: absolute; left: {:.1}px; top: {:.1}px; width: {:.1}px; display: flex; \
         flex-direction: column; gap: 6px; pointer-events: auto",
        ((w - t.width) * 0.5).max(0.0) * k,
        t.top * k,
        t.width * k
    );
    let folded: View = match &t.folded {
        Some(FoldedView::Bar { who, muted, .. }) => folded_bar(l, pal, t.width, who, *muted),
        Some(FoldedView::Tiles {
            people,
            controls,
            muted,
        }) => folded_tiles(l, pal, t.width, people, *controls, *muted),
        None => View::Empty,
    };
    let banners: Vec<View> = t
        .banners
        .iter()
        .map(|b| banner(l, pal, t.width, b))
        .collect();
    let notice: View = match &t.notice {
        Some(text) => {
            let style = format!(
                "box-sizing: border-box; width: 100%; padding: 10px 16px; border-radius: {}px; \
                 background: {}; color: {}; font-size: {}px; text-align: center; cursor: pointer",
                theme::RADIUS_LG,
                css(theme::LABEL_CHIP),
                css(theme::DARK.cream),
                theme::SIZE_BODY
            );
            let go = on(&l.acts, vec![CallAct::ClearNotice]);
            let text = text.clone();
            view! { <div title="Dismiss" style={style} on:click={tap(go)}>{text}</div> }
        }
        None => View::Empty,
    };
    view! {
        <div style={style}>
            {folded}
            {banners}
            {notice}
        </div>
    }
}

/// The call folded to a bar: green, with who and how long, the microphone
/// and hang-up. A tap on the bar unfolds it.
fn folded_bar(l: &BarLive, pal: &Palette, width: f32, who: &str, muted: bool) -> View {
    let k = l.k;
    let h = CALL_BUTTON_SMALL + 12.0;
    let style = format!(
        "position: relative; flex: none; width: {:.1}px; height: {:.1}px; border-radius: {:.1}px; \
         background: {}; cursor: pointer",
        width * k,
        h * k,
        h * 0.5 * k,
        css(theme::CALL_ACCEPT_GREEN)
    );
    // The words: who, then the timer just left of the buttons.
    let buttons_w = 2.0 * CALL_BUTTON_SMALL + 8.0 + 6.0;
    let words = format!(
        "position: absolute; left: {:.1}px; right: {:.1}px; top: 0; height: {:.1}px; display: flex; \
         align-items: center; gap: 8px; color: {}; pointer-events: none",
        16.0 * k,
        (buttons_w + 12.0) * k,
        h * k,
        css(theme::DARK.cream)
    );
    let who_style =
        "flex: 1; min-width: 0; display: flex; align-items: center; gap: 6px; overflow: hidden";
    let status_style = format!("flex: none; font-size: {}px", theme::SIZE_SUB);
    let y = h * 0.5;
    let hang = pos2(width - 6.0 - CALL_BUTTON_SMALL * 0.5, y);
    let mic = pos2(hang.x - CALL_BUTTON_SMALL - 8.0, y);
    let (glyph, tip, tone) = if muted {
        (icons::MIC_OFF, "Unmute", CallTone::Off)
    } else {
        (icons::MIC, "Mute", CallTone::On)
    };
    let mic = call_button(
        &l.acts,
        k,
        pal,
        mic,
        CALL_BUTTON_SMALL,
        glyph,
        tip,
        tone,
        vec![CallAct::ToggleMute],
    );
    let hang = call_button(
        &l.acts,
        k,
        pal,
        hang,
        CALL_BUTTON_SMALL,
        icons::CALL_END,
        "Hang up",
        CallTone::HangUp,
        vec![CallAct::Request(CallRequest::Leave)],
    );
    let unfold = on(&l.acts, vec![CallAct::Unfold]);
    let who = line(
        vec![Seg::I(icons::CALL), Seg::T(format!(" {who}"))],
        theme::SIZE_BODY,
    );
    // The timer follows its signal in place (`BarLive::status`).
    let status = {
        let s = l.status.clone();
        View::DynText(Rc::new(move || s.get()))
    };
    view! {
        <div title="Back to the call" style={style} on:click={tap(unfold)}>
            <div style={words}>
                <div style={who_style}>{who}</div>
                <span style={status_style}>{status}</span>
            </div>
            {mic}
            {hang}
        </div>
    }
}

/// The call folded over the map: a row of tiles — up to three others and
/// you — with the way back to the full screen in the corner, and the
/// microphone and hang-up under them unless the chat sheet's strip has them.
fn folded_tiles(
    l: &BarLive,
    pal: &Palette,
    width: f32,
    people: &[PersonView],
    controls: bool,
    muted: bool,
) -> View {
    let k = l.k;
    let n = people.len().max(1) as f32;
    let gap = 8.0;
    let tw = (width - gap * (n - 1.0)) / n;
    let th = (tw * 1.2).clamp(90.0, 150.0);
    let row_h = if controls {
        CALL_BUTTON_SMALL + 10.0
    } else {
        0.0
    };
    let style = format!(
        "position: relative; flex: none; width: {:.1}px; height: {:.1}px",
        width * k,
        (th + row_h) * k
    );
    let mut tiles: Vec<View> = Vec::new();
    for (i, person) in people.iter().enumerate() {
        let tile = Rect::from_min_size(pos2(i as f32 * (tw + gap), 0.0), Vec2::new(tw, th));
        // The card's shadow under each tile, as egui's `card_shadow`.
        let lift = bx(
            k,
            tile,
            &format!(
                "border-radius: {:.1}px; box-shadow: var(--m-shadow-lg); pointer-events: none",
                14.0 * k
            ),
        );
        tiles.push(view! { <div style={lift}></div> });
        grid_tile(&mut tiles, k, pal, tile, person);
    }
    // The tiles take the tap that unfolds: a box over them, listening.
    let over = bx(
        k,
        Rect::from_min_size(Vec2::ZERO, Vec2::new(width, th)),
        "cursor: pointer",
    );
    let unfold = on(&l.acts, vec![CallAct::Unfold]);
    let corner = pos2(
        width - 6.0 - CALL_BUTTON_SMALL * 0.5,
        6.0 + CALL_BUTTON_SMALL * 0.5,
    );
    let full = call_button(
        &l.acts,
        k,
        pal,
        corner,
        CALL_BUTTON_SMALL,
        icons::OPEN_EXTERNAL,
        "Full screen",
        CallTone::On,
        vec![CallAct::Unfold],
    );
    let keys: View = if controls {
        let y = th + 6.0 + (row_h - 6.0) * 0.5;
        let hang = pos2(width - CALL_BUTTON_SMALL * 0.5, y);
        let mic = pos2(hang.x - CALL_BUTTON_SMALL - 8.0, y);
        let (glyph, tip, tone) = if muted {
            (icons::MIC_OFF, "Unmute", CallTone::Off)
        } else {
            (icons::MIC, "Mute", CallTone::On)
        };
        let mic = call_button(
            &l.acts,
            k,
            pal,
            mic,
            CALL_BUTTON_SMALL,
            glyph,
            tip,
            tone,
            vec![CallAct::ToggleMute],
        );
        let hang = call_button(
            &l.acts,
            k,
            pal,
            hang,
            CALL_BUTTON_SMALL,
            icons::CALL_END,
            "Hang up",
            CallTone::HangUp,
            vec![CallAct::Request(CallRequest::Leave)],
        );
        view! { {mic}{hang} }
    } else {
        View::Empty
    };
    view! {
        <div style={style}>
            <div title="Back to the call" style={over} on:click={tap(unfold)}>
                {tiles}
            </div>
            {full}
            {keys}
        </div>
    }
}

/// A call ringing as a banner: over a call you are in, or folded. A tap on
/// its body unfolds it (when not in a call).
fn banner(l: &BarLive, pal: &Palette, width: f32, b: &BannerView) -> View {
    let k = l.k;
    let h = 64.0;
    let style = format!(
        "position: relative; flex: none; box-sizing: border-box; width: {:.1}px; height: {:.1}px; \
         border-radius: {}px; background: {}; border: 1px solid {}; cursor: pointer",
        width * k,
        h * k,
        theme::RADIUS_LG,
        css(pal.surface_raised),
        css(pal.overlay_line)
    );
    let disc_c = pos2(12.0 + 20.0, h * 0.5);
    let face = avatar(k, disc_c, &b.name, 40.0);
    let answer = pos2(width - 12.0 - CALL_BUTTON_SMALL * 0.5, h * 0.5);
    let decline = pos2(answer.x - CALL_BUTTON_SMALL - 10.0, h * 0.5);
    let words = format!(
        "position: absolute; left: {:.1}px; right: {:.1}px; top: 0; height: {:.1}px; display: flex; \
         flex-direction: column; justify-content: center; gap: 2px; pointer-events: none",
        (disc_c.x + 32.0) * k,
        (width - decline.x + CALL_BUTTON_SMALL * 0.5 + 8.0) * k,
        h * k
    );
    let name_style = format!(
        "font-size: {}px; color: {}; white-space: nowrap; overflow: hidden; text-overflow: ellipsis",
        theme::SIZE_VALUE,
        css(pal.cream)
    );
    let sub_style = format!(
        "font-size: {}px; color: {}; white-space: nowrap; overflow: hidden; text-overflow: ellipsis",
        theme::SIZE_SUB,
        css(pal.text_dim)
    );
    let decline = call_button(
        &l.acts,
        k,
        pal,
        decline,
        CALL_BUTTON_SMALL,
        icons::CALL_END,
        "Decline",
        CallTone::HangUp,
        vec![CallAct::Request(CallRequest::Dismiss(b.group.clone()))],
    );
    let answer = call_button(
        &l.acts,
        k,
        pal,
        answer,
        CALL_BUTTON_SMALL,
        icons::CALL,
        "Answer",
        CallTone::Accept,
        vec![CallAct::Request(CallRequest::Join(b.group.clone()))],
    );
    let open = on(&l.acts, vec![CallAct::BannerOpen]);
    let (name, sub) = (b.name.clone(), b.sub.clone());
    view! {
        <div title="Incoming call" style={style} on:click={tap(open)}>
            {face}
            <div style={words}>
                <span style={name_style}>{name}</span>
                <span style={sub_style}>{sub}</span>
            </div>
            {decline}
            {answer}
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_letterboxed_picture_keeps_its_shape() {
        let rect = Rect::from_min_size(Vec2::ZERO, Vec2::new(1600.0, 900.0));
        let r = contain(0.75, rect);
        assert!((r.height() - 900.0).abs() < 1e-3);
        assert!((r.width() / r.height() - 0.75).abs() < 1e-4);
        assert!((r.center().x - 800.0).abs() < 1e-3);
    }
}

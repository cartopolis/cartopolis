//! The camera drawn by the component library (`lib_host`), beside egui's
//! (`camera_screen::render_camera`): the viewfinder with the shutter, the
//! modes, Photo | Video, the selfie flip and the upload; then the result
//! with Post, Send, Save and Retake. Opened from the dock, a chat's + menu,
//! the post composer or the polaroid.
//!
//! On with `CARTO_UI_LIB=camera` (or `1`). egui's system steps aside
//! (`CameraScreen::lib`) but keeps every decision and the layout both
//! editions share (`camera_screen::finder_layout`, `result_layout`), and
//! publishes what shows in `CameraScreen::lib_view`. A tap here goes back in
//! `CameraScreen::lib_acts` and is done by `Session::apply`, the code egui's
//! buttons call. The app's only: the web build has no camera.
//!
//! **The pictures are live images.** The viewfinder is
//! `<img src="bevy:camera_viewfinder">` and the result
//! `<img src="bevy:camera_result">`: two Bevy images this system made and
//! registered in `mithril_bevy::LiveImages` before the screen mounts, whose
//! pixels it overwrites when egui's system hands new ones over
//! (`CameraScreen::lib_pixels`) — the viewfinder's frame when one arrives,
//! the result once, a clip's each time its player has a new picture. The
//! handles stay, so nothing is copied into the view. There is no
//! `object-fit`: the viewfinder fills its pane by a box larger than the pane
//! (cover, as egui's `cover_uv`), clipped by the pane; the result is sized
//! to fit its room in Rust.
//!
//! **Dark in both themes**, as egui's (`theme::dark_here`): it is mounted
//! dark and takes its colours from `theme::DARK`.
//!
//! **Out of the picture.** While the shutter's capture runs
//! (`ScreenshotState::hide_ui`, which hides egui for that frame) the root
//! is `display: none`: the library draws on a camera of its own over the
//! window, and would otherwise be in the polaroid. A clip's frames are taken
//! with the screen showing, as egui's are, which is why the recording screen
//! shows only the shutter and the running time.
//!
//! **Placement.** Every rect comes from egui's layout, in egui points from
//! the window's top left — the library's px on the Bevy backend, the only
//! one with a camera. In world only the controls' strip listens, so the map
//! inside the frame is still the map, to be framed by dragging; in Mixed and
//! Real, and on the result, the whole screen does.

use std::cell::RefCell;
use std::rc::Rc;

use bevy::ecs::system::NonSendMarker;
use bevy::prelude::{Res, ResMut};
// The library's, not Bevy's: an explicit import outranks the glob.
use mithril::prelude::*;
use mithril::prelude::{Button, Event};

use crate::resources::ScreenshotState;
use crate::systems::camera_screen::{
    mode_at, result_actions, CameraAct, CameraScreen, CameraView, ChipTone, FinderView, Mode,
    ResultView, ACTION_DISC, CHIP_GOOD, CHIP_WARN, POLAROID_RIM, POLAROID_RIM_W, RECORD_RED,
    SHADE_ALPHA, SWITCH_SIZE,
};
use crate::systems::lib_settings::LibScreens;
use crate::systems::theme::tokens::{alpha, hex};
use crate::systems::{icons, theme};

/// The camera's name in the library's host.
const KEY: &str = "camera";

/// The live image the viewfinder shows (`<img src="bevy:camera_viewfinder">`).
pub const VIEWFINDER: &str = "camera_viewfinder";

/// The live image the result shows (`<img src="bevy:camera_result">`).
pub const RESULT: &str = "camera_result";

/// The round buttons' diameter: the map's disc.
const DISC: f32 = theme::ROUND_BUTTON_SIZE;

/// The shutter's ring, across, and the disc inside it.
const SHUTTER: f32 = 72.0;
const SHUTTER_DISC: f32 = 54.0;

type Acts = Rc<RefCell<Vec<CameraAct>>>;

/// What is mounted: the screen, and what changes more often than its
/// layout — the pose chips, a clip's running time, the status line — in
/// signals of their own, so a heading that moves does not rebuild the
/// pictures.
#[derive(Clone)]
struct Live {
    page: Signal<CameraView>,
    chips: Signal<Vec<(String, ChipTone)>>,
    timer: Signal<Option<String>>,
    status: Signal<Option<String>>,
    hidden: Signal<bool>,
    acts: Acts,
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

/// The two images the screen shows, and the size each holds.
#[cfg(feature = "ui-probe")]
#[derive(Default)]
pub struct Pictures {
    viewfinder: Option<(bevy::prelude::Handle<bevy::prelude::Image>, [u32; 2])>,
    result: Option<(bevy::prelude::Handle<bevy::prelude::Image>, [u32; 2])>,
}

/// Mounts, bridges and unmounts the camera, and keeps its pictures.
/// `PostUpdate`, before `DriveUi`, as every library screen.
#[allow(clippy::too_many_arguments)]
pub fn drive_lib_camera(
    _main: NonSendMarker,
    lib: Res<LibScreens>,
    mut screen: ResMut<CameraScreen>,
    shot: Res<ScreenshotState>,
    #[cfg(feature = "ui-probe")] mut images: ResMut<bevy::prelude::Assets<bevy::prelude::Image>>,
    #[cfg(feature = "ui-probe")] mut live_images: ResMut<mithril_bevy::LiveImages>,
    #[cfg(feature = "ui-probe")] mut pictures: bevy::prelude::Local<Pictures>,
) {
    if !lib.camera {
        return;
    }
    if !screen.lib {
        screen.lib = true;
    }
    // The pictures first: an `<img>` reads its source when it is styled,
    // so both names are registered before the screen mounts.
    let pixels = std::mem::take(&mut screen.lib_pixels);
    #[cfg(feature = "ui-probe")]
    {
        let Pictures { viewfinder, result } = &mut *pictures;
        show(
            &mut images,
            &mut live_images,
            viewfinder,
            VIEWFINDER,
            pixels.viewfinder,
        );
        show(&mut images, &mut live_images, result, RESULT, pixels.result);
    }
    #[cfg(not(feature = "ui-probe"))]
    let _ = pixels;
    let hidden = shot.hide_ui;
    MOUNTED.with(|m| {
        let mut m = m.borrow_mut();
        // The taps of the frame, for egui's system to do next frame.
        if let Some(live) = m.as_ref() {
            let acts: Vec<CameraAct> = std::mem::take(&mut *live.acts.borrow_mut());
            screen.lib_acts.extend(acts);
        }
        let Some(view) = screen.lib_view.clone() else {
            if m.take().is_some() {
                crate::systems::lib_host::clear_keyed(KEY);
            }
            return;
        };
        let (page, chips, timer, status) = split(view);
        match m.as_mut() {
            None => {
                let live = Live {
                    page: signal(page),
                    chips: signal(chips),
                    timer: signal(timer),
                    status: signal(status),
                    hidden: signal(hidden),
                    acts: Rc::new(RefCell::new(Vec::new())),
                };
                // Dark in both themes, as a phone's camera is — by its own
                // colours (`theme::DARK`), not the host's switch: that is one
                // for every screen, and set dark here it stayed dark for all
                // of them after the camera closed.
                crate::systems::lib_host::mount_keyed(
                    KEY,
                    view_of(&live),
                    theme::p().ink == theme::DARK.ink,
                    theme::touch_layout(),
                );
                if let Some(path) = std::env::var_os("CARTO_UI_LIB_HTML") {
                    if let Err(e) = std::fs::write(&path, html(&live)) {
                        bevy::log::warn!(?path, error = %e, "lib camera: could not write the HTML");
                    }
                }
                *m = Some(live);
            }
            Some(live) => {
                put(&live.page, page);
                put(&live.chips, chips);
                put(&live.timer, timer);
                put(&live.status, status);
                put(&live.hidden, hidden);
            }
        }
    });
}

/// The image `name` shows, made (black, 2 × 2) and registered the first
/// time, and `pixels` written into it when there are new ones. Its handle
/// never changes: a new size is a new image under the same handle.
#[cfg(feature = "ui-probe")]
fn show(
    images: &mut bevy::prelude::Assets<bevy::prelude::Image>,
    live: &mut mithril_bevy::LiveImages,
    held: &mut Option<(bevy::prelude::Handle<bevy::prelude::Image>, [u32; 2])>,
    name: &str,
    pixels: Option<(Vec<u8>, [u32; 2])>,
) {
    let (handle, size) = held.get_or_insert_with(|| (images.add(blank([2, 2])), [2, 2]));
    if live.0.get(name) != Some(&*handle) {
        live.0.insert(name.to_owned(), handle.clone());
    }
    let Some((rgba, new)) = pixels else {
        return;
    };
    if rgba.len() != (new[0] * new[1] * 4) as usize || new[0] == 0 || new[1] == 0 {
        return;
    }
    if let Some(mut image) = images.get_mut(&*handle) {
        if *size != new {
            *image = blank(new);
            *size = new;
        }
        image.data = Some(rgba);
    }
}

/// A black RGBA image `size` large, for pixels to be written into.
#[cfg(feature = "ui-probe")]
fn blank([width, height]: [u32; 2]) -> bevy::prelude::Image {
    use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
    bevy::prelude::Image::new_fill(
        Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        &[0, 0, 0, 255],
        TextureFormat::Rgba8UnormSrgb,
        bevy::asset::RenderAssetUsages::default(),
    )
}

/// The view without what changes often, and that apart.
fn split(
    view: CameraView,
) -> (
    CameraView,
    Vec<(String, ChipTone)>,
    Option<String>,
    Option<String>,
) {
    match view {
        CameraView::Finder(mut f) => {
            let chips = std::mem::take(&mut f.chips);
            let timer = f.timer.take();
            let status = f.status.take();
            (CameraView::Finder(f), chips, timer, status)
        }
        result => (result, Vec::new(), None, None),
    }
}

/// The HTML from the library's string renderer, for the browser beside a
/// capture. The pictures show nothing there: `bevy:` is the app's.
fn html(l: &Live) -> String {
    use mithril::view::{mount, StringRenderer};
    let r = Rc::new(RefCell::new(StringRenderer::new()));
    let root = r.borrow().root();
    let mounted = mount(view_of(l), &r, &root, None);
    let out = r.borrow().html();
    drop(mounted);
    out
}

// ─── Pieces ──────────────────────────────────────────────────────────────────

/// `r` as an absolutely placed box.
fn at(r: egui::Rect) -> String {
    format!(
        "position: absolute; left: {:.1}px; top: {:.1}px; width: {:.1}px; height: {:.1}px",
        r.min.x,
        r.min.y,
        r.width(),
        r.height()
    )
}

/// A box `w` × `h` centred on `c`, its content centred in it and
/// listening nowhere: for a pill whose width is its words'.
fn centred(c: egui::Pos2, w: f32, h: f32) -> String {
    format!(
        "{}; display: flex; align-items: center; justify-content: center; pointer-events: none",
        at(egui::Rect::from_center_size(c, egui::vec2(w, h)))
    )
}

/// Black at egui's `from_black_alpha(a)`.
fn black(a: u8) -> String {
    alpha(egui::Color32::BLACK, f32::from(a) / 255.0 * 100.0)
}

/// White, the ink over the picture.
fn white() -> String {
    hex(egui::Color32::WHITE)
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

/// A tap that hands `a` back.
fn tap(acts: &Acts, a: CameraAct) -> impl Fn(&Event) + 'static {
    let acts = acts.clone();
    move |_: &Event| acts.borrow_mut().push(a.clone())
}

/// The same, as a component's handler.
fn handler(acts: &Acts, a: CameraAct) -> Handler {
    Rc::new(tap(acts, a)) as Handler
}

/// The four rectangles of `full` around `hole`.
fn around(full: egui::Rect, hole: egui::Rect) -> [egui::Rect; 4] {
    [
        egui::Rect::from_min_max(full.min, egui::pos2(full.max.x, hole.min.y)),
        egui::Rect::from_min_max(egui::pos2(full.min.x, hole.max.y), full.max),
        egui::Rect::from_min_max(
            egui::pos2(full.min.x, hole.min.y),
            egui::pos2(hole.min.x, hole.max.y),
        ),
        egui::Rect::from_min_max(
            egui::pos2(hole.max.x, hole.min.y),
            egui::pos2(full.max.x, hole.max.y),
        ),
    ]
}

/// A filled box, listening nowhere.
fn fill(r: egui::Rect, colour: &str) -> View {
    el("div")
        .attr("style", format!("{}; background: {colour}", at(r)))
        .into_view()
}

/// The live image `name` filling `area` without stretching (egui's
/// `cover_uv`): a box of the picture's shape covering the area, clipped by
/// it. `size` is the picture's.
fn cover(area: egui::Rect, name: &str, size: [u32; 2], listen: bool) -> View {
    let (w, h) = (size[0].max(1) as f32, size[1].max(1) as f32);
    let k = (area.width() / w).max(area.height() / h);
    let (iw, ih) = (w * k, h * k);
    let img = format!(
        "position: absolute; left: {:.1}px; top: {:.1}px; width: {:.1}px; height: {:.1}px",
        (area.width() - iw) / 2.0,
        (area.height() - ih) / 2.0,
        iw,
        ih
    );
    el("div")
        .attr(
            "style",
            format!(
                "{}; overflow: hidden; pointer-events: {}",
                at(area),
                if listen { "auto" } else { "none" }
            ),
        )
        .child(
            el("img")
                .attr("src", format!("bevy:{name}"))
                .attr("alt", "")
                .attr("style", img),
        )
        .into_view()
}

/// The map's round button (`theme::round_button`), in the dark palette.
fn plate(r: egui::Rect, icon: &'static str, hint: &str, act: CameraAct, acts: &Acts) -> View {
    let d = theme::DARK;
    let style = format!(
        "{}; box-sizing: border-box; display: flex; align-items: center; justify-content: center; \
         border-radius: var(--m-radius-full); border: 1.5px solid {}; background: {}; color: {}; \
         box-shadow: var(--m-shadow); cursor: pointer; pointer-events: auto",
        at(r),
        hex(d.map_rim),
        hex(d.map_fill),
        hex(d.map_ink)
    );
    el("div")
        .attr("style", style)
        .attr("title", hint.to_owned())
        .attr("aria-label", hint.to_owned())
        .on("click", tap(acts, act))
        .child(glyph(icon, 17.0))
        .into_view()
}

/// The camera's own round button (`camera_screen::round`): a dark disc over
/// the picture, the accent when on.
fn round(
    r: egui::Rect,
    icon: &'static str,
    on: bool,
    hint: &str,
    act: CameraAct,
    acts: &Acts,
) -> View {
    let d = theme::DARK;
    let (bg, ink) = if on {
        (hex(d.accent), hex(d.ink))
    } else {
        (black(150), white())
    };
    let style = format!(
        "{}; display: flex; align-items: center; justify-content: center; \
         border-radius: var(--m-radius-full); background: {bg}; color: {ink}; \
         cursor: pointer; pointer-events: auto",
        at(r)
    );
    el("div")
        .attr("style", style)
        .attr("title", hint.to_owned())
        .attr("aria-label", hint.to_owned())
        .on("click", tap(acts, act))
        .child(glyph(icon, 18.0))
        .into_view()
}

/// A word on a dark pill over the picture ("REAL", "CARTOPOLIS").
fn tag(at_: egui::Pos2, text: &'static str) -> View {
    let style = format!(
        "position: absolute; left: {:.1}px; top: {:.1}px; padding: 3px 7px; border-radius: 7px; \
         background: {}; color: {}; font-size: {}px; line-height: 1.2; white-space: nowrap; \
         pointer-events: none",
        at_.x,
        at_.y,
        black(180),
        white(),
        theme::SIZE_CHIP
    );
    el("span").attr("style", style).child(text).into_view()
}

// ─── The screen ──────────────────────────────────────────────────────────────

/// The root: the whole window, listening only where a piece does; gone
/// while the shutter's capture runs.
fn view_of(l: &Live) -> View {
    let root = {
        let hidden = l.hidden.clone();
        move || {
            if hidden.get() {
                "display: none".to_string()
            } else {
                "position: absolute; left: 0; top: 0; right: 0; bottom: 0; overflow: hidden; \
                 pointer-events: none"
                    .to_string()
            }
        }
    };
    let page = {
        let (page, acts) = (l.page.clone(), l.acts.clone());
        move || -> View {
            match page.get() {
                CameraView::Finder(f) => finder_view(&f, &acts),
                CameraView::Result(r) => result_view(&r, &acts),
            }
        }
    };
    let chips = {
        let (page, chips) = (l.page.clone(), l.chips.clone());
        move || -> View {
            match page.get() {
                CameraView::Finder(f) => chips_view(f.chips_at, &chips.get()),
                CameraView::Result(_) => View::Empty,
            }
        }
    };
    let timer = {
        let (page, timer) = (l.page.clone(), l.timer.clone());
        move || -> View {
            match (page.get(), timer.get()) {
                (CameraView::Finder(f), Some(text)) => timer_view(&f, text),
                _ => View::Empty,
            }
        }
    };
    let status = {
        let (page, status) = (l.page.clone(), l.status.clone());
        move || -> View {
            match (page.get(), status.get()) {
                (CameraView::Finder(f), Some(text)) => status_view(&f, text),
                _ => View::Empty,
            }
        }
    };
    view! {
        <div style={root}>
            {page}
            {chips}
            {timer}
            {status}
        </div>
    }
}

/// The viewfinder: what fills the screen in this mode, then the controls.
fn finder_view(f: &FinderView, acts: &Acts) -> View {
    let d = theme::DARK;
    let mut parts: Vec<View> = Vec::new();
    match f.mode {
        Mode::World => {
            // The polaroid's frame over the map: the world outside it
            // dimmed, and the picture is what is inside.
            for r in around(f.full, f.frame) {
                parts.push(fill(r, &black(SHADE_ALPHA)));
            }
            let rim = format!(
                "{}; box-sizing: border-box; border: {:.0}px solid {}; pointer-events: none",
                at(f.frame.expand(POLAROID_RIM_W)),
                POLAROID_RIM_W,
                hex(POLAROID_RIM)
            );
            parts.push(el("div").attr("style", rim).into_view());
            // Only the controls' strip takes touches: the map inside the
            // frame is still the map, to be framed by dragging.
            parts.push(
                el("div")
                    .attr("style", format!("{}; pointer-events: auto", at(f.strip)))
                    .on("click", |_: &Event| {})
                    .into_view(),
            );
        }
        Mode::Real => {
            // The backdrop swallows touches; the photograph fills it.
            parts.push(
                el("div")
                    .attr(
                        "style",
                        format!(
                            "{}; background: {}; pointer-events: auto",
                            at(f.full),
                            hex(d.ink)
                        ),
                    )
                    .on("click", |_: &Event| {})
                    .into_view(),
            );
            if let Some(size) = f.photo {
                parts.push(cover(f.full, VIEWFINDER, size, false));
            }
        }
        Mode::Mixed => {
            // Two panes of the photograph's shape: the photograph in the
            // first, and the world, drawn from the phone's pose, through a
            // hole in the second (`hole_view` aims the world camera into
            // it). The backdrop swallows touches, the hole included.
            parts.push(
                el("div")
                    .attr("style", format!("{}; pointer-events: auto", at(f.full)))
                    .on("click", |_: &Event| {})
                    .into_view(),
            );
            for r in around(f.full, f.hole) {
                parts.push(fill(r, &hex(d.ink)));
            }
            if let Some(size) = f.photo {
                parts.push(cover(f.pane, VIEWFINDER, size, false));
            }
            parts.push(tag(f.pane.min + egui::vec2(10.0, 10.0), "REAL"));
            // Inside the hole only when nothing is recording it.
            if !f.recording {
                parts.push(tag(f.hole.min + egui::vec2(10.0, 10.0), "CARTOPOLIS"));
            }
        }
    }

    // While a clip runs nothing is drawn where it is taken from: the close
    // button, the switch, the modes and the chips step aside.
    if !f.recording {
        parts.push(if f.mode == Mode::World {
            plate(
                f.close,
                icons::CLEAR,
                "Close the camera",
                CameraAct::Close,
                acts,
            )
        } else {
            round(
                f.close,
                icons::CLEAR,
                false,
                "Close",
                CameraAct::Close,
                acts,
            )
        });
        parts.push(photo_video(f, acts));
    }

    parts.push(shutter(f, acts));

    // The rest goes while a clip runs: its running time stands where the
    // upload is (`timer_view`).
    if !f.recording {
        let disc = egui::Vec2::splat(DISC);
        // Upload is a picture's: there is no clip to pick.
        if !f.video {
            parts.push(round(
                egui::Rect::from_center_size(f.shutter_c - f.step, disc),
                icons::UPLOAD,
                false,
                "Upload a picture from the phone",
                CameraAct::Upload,
                acts,
            ));
        }
        parts.push(round(
            egui::Rect::from_center_size(f.shutter_c + f.step, disc),
            icons::FLIP_CAMERA,
            f.front,
            "Selfie",
            CameraAct::Flip,
            acts,
        ));
        for (i, m) in Mode::ALL.into_iter().enumerate() {
            parts.push(mode_view(f, i, m, acts));
        }
    }
    View::Fragment(parts)
}

/// Photo | Video, top right: a pill, the chosen side a cream plate.
fn photo_video(f: &FinderView, acts: &Acts) -> View {
    let d = theme::DARK;
    let r = egui::Rect::from_min_size(f.switch_at.min, SWITCH_SIZE);
    let frame = format!(
        "{}; box-sizing: border-box; padding: 3px; display: flex; \
         border-radius: {:.0}px; background: {}; pointer-events: auto",
        at(r),
        SWITCH_SIZE.y / 2.0,
        black(150)
    );
    let halves: Vec<View> = [
        ("Photo", false, "Take pictures"),
        ("Video", true, "Record clips"),
    ]
    .into_iter()
    .map(|(label, is_video, hint)| {
        let on = is_video == f.video;
        let style = format!(
            "flex: 1 1 0; min-width: 0; display: flex; align-items: center; \
                 justify-content: center; border-radius: {:.0}px; background: {}; color: {}; \
                 font-size: {}px; white-space: nowrap; cursor: pointer",
            SWITCH_SIZE.y / 2.0 - 3.0,
            if on {
                hex(d.cream)
            } else {
                "transparent".to_string()
            },
            if on { hex(d.ink) } else { white() },
            theme::SIZE_BODY
        );
        let half = el("div")
            .attr("style", style)
            .attr("title", hint)
            .attr("aria-label", label);
        // Only the other side does anything.
        if on {
            half.on("click", |_: &Event| {}).child(label).into_view()
        } else {
            half.on("click", tap(acts, CameraAct::PhotoVideo(is_video)))
                .child(label)
                .into_view()
        }
    })
    .collect();
    el("div")
        .attr("style", frame)
        .on("click", |_: &Event| {})
        .children(halves)
        .into_view()
}

/// The shutter: a white ring, and inside it the stop square, the record
/// disc, or the picture disc (the brand's In world, white otherwise, half
/// each in Mixed); grey while it cannot take one.
fn shutter(f: &FinderView, acts: &Acts) -> View {
    let d = theme::DARK;
    let ring = format!(
        "{}; box-sizing: border-box; display: flex; align-items: center; justify-content: center; \
         border: 4px solid {}; border-radius: var(--m-radius-full); cursor: pointer; \
         pointer-events: auto",
        at(egui::Rect::from_center_size(
            f.shutter_c,
            egui::Vec2::splat(SHUTTER)
        )),
        white()
    );
    let disc = |paint: String| {
        format!(
            "flex: none; width: {s:.0}px; height: {s:.0}px; border-radius: var(--m-radius-full); {paint}",
            s = SHUTTER_DISC
        )
    };
    let inner = if f.stoppable {
        format!(
            "flex: none; width: 28px; height: 28px; border-radius: 6px; background: {}",
            hex(RECORD_RED)
        )
    } else if f.video {
        disc(format!(
            "background: {}",
            if f.ready {
                hex(RECORD_RED)
            } else {
                hex(d.text_dim)
            }
        ))
    } else if !f.ready {
        disc(format!("background: {}", hex(d.text_dim)))
    } else {
        match f.mode {
            Mode::World => disc(format!("background: {}", hex(theme::BRAND))),
            // Half photograph, half world: the Mixed mark.
            Mode::Mixed => disc(format!(
                "background-image: linear-gradient(to right, {w} 50%, {b} 50%)",
                w = white(),
                b = hex(theme::BRAND)
            )),
            Mode::Real => disc(format!("background: {}", white())),
        }
    };
    let (name, hint) = match (f.stoppable, f.video) {
        (true, _) => ("Stop", "Stop recording"),
        (false, true) => ("Record", "Record a clip"),
        (false, false) => ("Take the picture", "Take the picture"),
    };
    el("div")
        .attr("style", ring)
        .attr("title", hint)
        .attr("aria-label", name)
        .on("click", tap(acts, CameraAct::Shutter))
        .child(el("span").attr("style", inner))
        .into_view()
}

/// One of the three modes: a word, the brand's when chosen, dimmed when
/// Mixed is not on offer (a tap then says why).
fn mode_view(f: &FinderView, i: usize, m: Mode, acts: &Acts) -> View {
    let p = mode_at(f.shutter_c, f.sideways, i);
    let on = m == f.mode;
    let offered = m != Mode::Mixed || f.mixed_offered;
    let ink = if on {
        hex(theme::BRAND)
    } else if offered {
        white()
    } else {
        alpha(egui::Color32::WHITE, 40.0)
    };
    // Sideways the words end at `p` (egui's RIGHT_CENTER); upright they are
    // centred on it. Each box is the touch, as egui's text rect and 8 more.
    let (r, justify) = if f.sideways {
        (
            egui::Rect::from_min_max(
                egui::pos2(p.x - 112.0, p.y - 15.0),
                egui::pos2(p.x, p.y + 15.0),
            ),
            "flex-end",
        )
    } else {
        (
            egui::Rect::from_center_size(p, egui::vec2(88.0, 30.0)),
            "center",
        )
    };
    let style = format!(
        "{}; display: flex; align-items: center; justify-content: {justify}; color: {ink}; \
         font-size: {}px; font-weight: {}; white-space: nowrap; cursor: pointer; pointer-events: auto",
        at(r),
        theme::SIZE_BODY,
        if on { 600 } else { 400 }
    );
    el("div")
        .attr("style", style)
        .attr("aria-label", m.label())
        .on("click", tap(acts, CameraAct::Mode(m)))
        .child(m.label())
        .into_view()
}

/// The pose's quality, a row of chips from `at_`.
fn chips_view(at_: egui::Pos2, chips: &[(String, ChipTone)]) -> View {
    if chips.is_empty() {
        return View::Empty;
    }
    let row = format!(
        "position: absolute; left: {:.1}px; top: {:.1}px; display: flex; gap: 8px; \
         pointer-events: none",
        at_.x, at_.y
    );
    let pills: Vec<View> = chips
        .iter()
        .map(|(text, tone)| {
            let ink = match tone {
                ChipTone::Good => hex(CHIP_GOOD),
                ChipTone::Warn => hex(CHIP_WARN),
                ChipTone::Plain => hex(theme::DARK.cream),
            };
            let style = format!(
                "padding: 5px 10px; border-radius: 11px; background: {}; color: {ink}; \
                 font-size: {}px; line-height: 1.2; white-space: nowrap",
                black(170),
                theme::SIZE_SUB
            );
            el("span")
                .attr("style", style)
                .child(text.clone())
                .into_view()
        })
        .collect();
    el("div").attr("style", row).children(pills).into_view()
}

/// A clip's running time where the upload button is, in the record red.
fn timer_view(f: &FinderView, text: String) -> View {
    let pill = format!(
        "padding: 5px 8px; border-radius: 10px; background: {}; color: {}; font-size: {}px; \
         line-height: 1.2; white-space: nowrap",
        hex(RECORD_RED),
        white(),
        theme::SIZE_BODY
    );
    el("div")
        .attr("style", centred(f.shutter_c - f.step, 160.0, 40.0))
        .child(el("span").attr("style", pill).child(text))
        .into_view()
}

/// What it is doing or what went wrong, on a dark pill.
fn status_view(f: &FinderView, text: String) -> View {
    let pill = format!(
        "padding: 5px 10px; border-radius: 10px; background: {}; color: {}; font-size: 13px; \
         line-height: 1.2; white-space: nowrap",
        black(170),
        white()
    );
    el("div")
        .attr("style", centred(f.status_at, f.full.width(), 40.0))
        .child(el("span").attr("style", pill).child(text))
        .into_view()
}

// ─── The result ──────────────────────────────────────────────────────────────

/// The picture once it is made: as large as fits, Post, Send, Save and
/// Retake under it (sideways: beside it), the close button top left.
fn result_view(r: &ResultView, acts: &Acts) -> View {
    let d = theme::DARK;
    let backdrop = el("div")
        .attr(
            "style",
            format!(
                "{}; background: {}; pointer-events: auto",
                at(r.full),
                hex(d.ink)
            ),
        )
        .on("click", |_: &Event| {})
        .into_view();
    let picture = el("img")
        .attr("src", format!("bevy:{RESULT}"))
        .attr("alt", "The picture")
        .attr("style", format!("{}; pointer-events: none", at(r.shown)))
        .into_view();
    let close = plate(
        r.close,
        icons::CLEAR,
        "Close the camera",
        CameraAct::Close,
        acts,
    );
    // Held sideways the column has room to spare: centred, as egui's.
    let top = if r.sideways {
        ((r.side.height() - 200.0).max(0.0) / 2.0).round()
    } else {
        0.0
    };
    let column = format!(
        "{}; box-sizing: border-box; padding-top: {top:.0}px; display: flex; \
         flex-direction: column; align-items: center; pointer-events: auto",
        at(r.side)
    );
    let mut items: Vec<View> = Vec::new();
    if let Some(place) = &r.place {
        items.push(
            el("span")
                .attr(
                    "style",
                    format!(
                        "margin-bottom: 10px; font-size: {}px; color: {}; text-align: center",
                        theme::SIZE_TITLE,
                        hex(d.cream)
                    ),
                )
                .child(place.clone())
                .into_view(),
        );
    }
    if r.choosing_chat {
        items.push(chat_list(r, acts));
    } else {
        if let Some(line) = &r.clip_line {
            items.push(
                el("span")
                    .attr(
                        "style",
                        format!(
                            "margin-bottom: 8px; font-size: {}px; color: {}",
                            theme::SIZE_SUB,
                            hex(d.text_dim)
                        ),
                    )
                    .child(line.clone())
                    .into_view(),
            );
        }
        let cols: Vec<View> = result_actions(r.clip_line.is_some())
            .into_iter()
            .map(|(icon, label, hot, act)| {
                let enabled = !(act == CameraAct::Save && r.saving);
                action(icon, label, hot, enabled, act, acts)
            })
            .collect();
        items.push(
            el("div")
                .attr(
                    "style",
                    format!(
                        "flex: none; width: 100%; height: {:.0}px; display: flex",
                        ACTION_DISC + 26.0
                    ),
                )
                .children(cols)
                .into_view(),
        );
        if let Some((line, bad)) = &r.save_line {
            items.push(
                el("span")
                    .attr(
                        "style",
                        format!(
                            "margin-top: 10px; font-size: {}px; color: {}; text-align: center",
                            theme::SIZE_SUB,
                            hex(if *bad { d.error } else { d.text_dim })
                        ),
                    )
                    .child(line.clone())
                    .into_view(),
            );
        }
    }
    let side = el("div")
        .attr("style", column)
        .on("click", |_: &Event| {})
        .children(items)
        .into_view();
    View::Fragment(vec![backdrop, picture, close, side])
}

/// "Send to", the private chats, and Back.
fn chat_list(r: &ResultView, acts: &Acts) -> View {
    let d = theme::DARK;
    let heading = format!(
        "font-size: {}px; color: {}; margin-bottom: 6px",
        theme::SIZE_BODY,
        hex(d.text_dim)
    );
    let rows: View = if r.chats.is_empty() {
        let note = format!(
            "font-size: {}px; font-style: italic; color: {}",
            theme::SIZE_CHIP,
            hex(d.text_dim)
        );
        el("span")
            .attr("style", note)
            .child("No private chats yet.")
            .into_view()
    } else {
        View::Fragment(
            r.chats
                .iter()
                .map(|(key, name)| {
                    let name = name.clone();
                    let go = Some(handler(acts, CameraAct::SendTo(key.clone())));
                    view! { <Button onclick={go}>{name}</Button> }
                })
                .collect(),
        )
    };
    let back = Some(handler(acts, CameraAct::ChatsBack));
    view! {
        <div style="width: 100%; display: flex; flex-direction: column; align-items: stretch; gap: 6px">
            <span style={heading}>"Send to"</span>
            <div style="max-height: 120px; overflow-y: auto; display: flex; flex-direction: column; align-items: stretch; gap: 6px">
                {rows}
            </div>
            <Button onclick={back}>"Back"</Button>
        </div>
    }
}

/// One of the result's actions: a disc with its label under it, the whole
/// column taking the touch (`camera_screen::action`).
fn action(
    icon: &'static str,
    label: &'static str,
    hot: bool,
    enabled: bool,
    act: CameraAct,
    acts: &Acts,
) -> View {
    let d = theme::DARK;
    let (fill_, ring, ink) = if hot {
        (hex(theme::BRAND), hex(theme::BRAND), white())
    } else {
        (hex(d.surface_raised), hex(d.overlay_line), hex(d.text))
    };
    let ink = if enabled { ink } else { hex(d.text_dim) };
    let disc = format!(
        "flex: none; width: {s:.0}px; height: {s:.0}px; box-sizing: border-box; display: flex; \
         align-items: center; justify-content: center; border-radius: var(--m-radius-full); \
         border: 1.5px solid {ring}; background: {fill_}; color: {ink}",
        s = ACTION_DISC
    );
    let word = format!(
        "margin-top: 6px; font-size: {}px; color: {}; white-space: nowrap",
        theme::SIZE_BODY,
        hex(if enabled { d.text } else { d.text_dim })
    );
    let col = el("div")
        .attr(
            "style",
            format!(
                "flex: 1 1 0; min-width: 0; display: flex; flex-direction: column; \
                 align-items: center; cursor: {}",
                if enabled { "pointer" } else { "default" }
            ),
        )
        .attr("title", label)
        .attr("aria-label", label);
    let col = if enabled {
        col.on("click", tap(acts, act))
    } else {
        col.on("click", |_: &Event| {})
    };
    col.child(el("div").attr("style", disc).child(glyph(icon, 22.0)))
        .child(el("span").attr("style", word).child(label))
        .into_view()
}

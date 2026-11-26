//! The map's pins drawn by the component library (`lib_host`): saved places,
//! a chat's points, a community's posts, people's markers (`chat_people`)
//! with what they last said over them, and the "you are sharing your
//! location" chip.
//!
//! `place_pins::place_map_pins` keeps everything but the drawing: it
//! projects every marker, culls it to `place_pins::map_area` and the map's
//! buttons, and publishes the frame's markers in `PlacePins::lib_view`; a
//! tap here goes back as `PlacePins::lib_tapped` (the marker's key) and is
//! done there.
//!
//! **One mount, moved, never rebuilt per frame.** Everything anchored to the
//! world is drawn through [`Anchored`]: a keyed list whose items each hold
//! their look and their spot in signals. A marker that only moved restyles
//! its own node (and only after moving half a pixel); one that went off the
//! map is hidden, not unmounted; one seen for the first time builds its own
//! place in the list and nothing else. `lib_place_names`
//! and `lib_chat_overlay` draw through the same list.
//!
//! **Built from nodes.** The teardrop is a square with three round corners
//! turned 45°, the person's disc a `border-radius` circle on a ring, a
//! balloon's tail a small turned square. Each anchor is a 0 × 0 node at the
//! tip, its parts placed around it: a box wide enough to centre a label in
//! would take the taps meant for the pins under it, so a label is centred by
//! a measure of it (`place_pins::text_size`).
//!
//! What differs from egui's old pins: the library has no hover, so a pin's
//! name chip (egui showed it on hover) and the hover ring are not drawn — the
//! name is the node's `title`, which a browser shows; a pin seen for the
//! first time does not pop. The sharing chip is centred in a row across the
//! screen, which pins under that row cannot be tapped through.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use bevy::ecs::system::NonSendMarker;
use bevy::prelude::*;
// The library's, not Bevy's: an explicit import outranks the two globs.
use mithril::prelude::Event;
use mithril::prelude::*;

use crate::systems::lib_settings::LibScreens;
use crate::systems::place_pins::{PinLook, PlacePins};
use crate::systems::theme;
use crate::systems::ui_input::UiMetrics;

/// The pins' name in the library's host.
const KEY: &str = "pins";

/// A marker moves on screen only once it has moved this far, px: under it
/// the node keeps its style and nothing is restyled.
const MOVE_PX: f32 = 0.5;
/// An overlay's alpha is written again only once it has changed this much.
const FADE_STEP: f32 = 0.02;
/// Markers hidden (off the map) are kept, so coming back costs a restyle and
/// not a remount, until there are more than this many; then a new marker
/// takes a hidden one's place.
const MAX_HIDDEN: usize = 64;

/// The teardrop's square side, px: the head's diameter. Its point sits
/// `side / √2` under the head's centre (egui's is `2 × HEAD_R` under it).
const DROP: f32 = 16.0;
/// The hole in the head, as a share of its radius (egui's `HOLE_R`).
const HOLE: f32 = 0.4;
/// The hole's ink: the dark theme's cream, nearly opaque, as egui's — a pin's
/// own colours are content, the same over any theme.
const HOLE_ALPHA: f32 = 0.92;
/// The shadow at a marker's tip.
const TIP_SHADOW: &str = "rgba(0, 0, 0, 0.18)";
/// The shadow under a person's marker, darker: a disc stands higher.
#[cfg(feature = "comms")]
const PERSON_SHADOW: &str = "rgba(0, 0, 0, 0.35)";
/// The lift under a chat point's label and a balloon.
const LIFT: &str = "0 6px 16px rgba(0, 0, 0, 0.3)";

// ─── A keyed list of things anchored to the world ───────────────────────────

/// Where an anchored item is this frame, px in the library's root, and how
/// strongly it shows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Spot {
    pub x: f32,
    pub y: f32,
    pub alpha: f32,
}

impl Spot {
    pub(crate) fn at(x: f32, y: f32) -> Self {
        Spot { x, y, alpha: 1.0 }
    }

    /// Whether `next` is far enough from `self` to be drawn again.
    fn moved(self, next: Spot) -> bool {
        (self.x - next.x).abs() >= MOVE_PX
            || (self.y - next.y).abs() >= MOVE_PX
            || (self.alpha - next.alpha).abs() >= FADE_STEP
            || (self.alpha != next.alpha && (next.alpha == 0.0 || next.alpha == 1.0))
    }
}

/// One place in the list: the item it draws now (its key and look, `None`
/// while unused), and where it is (`None` hidden).
struct Slot<S> {
    look: Signal<Option<(String, S)>>,
    spot: Signal<Option<Spot>>,
}

impl<S> Clone for Slot<S> {
    fn clone(&self) -> Self {
        Slot {
            look: self.look.clone(),
            spot: self.spot.clone(),
        }
    }
}

/// Places are added this many at a time, each batch behind the last.
const CHUNK: usize = 16;

/// The places, and which item each holds.
struct Pool<S> {
    slots: Vec<Slot<S>>,
    /// The key each held place draws.
    by_key: HashMap<String, usize>,
    /// Places holding nothing.
    free: Vec<usize>,
    /// Per batch, whether it is built: the last one, false, is where the
    /// next batch goes.
    built: Vec<Signal<bool>>,
}

/// A keyed list of things anchored to the world, as absolutely placed
/// nodes in one mount: each item's look and spot are signals, so a frame in
/// which the camera moved restyles the items that moved and nothing else.
///
/// **An item seen for the first time takes a place and builds only that
/// place.** The places come in batches, each mounted behind the last, so a
/// new batch builds itself alone: rebuilding the whole list for one new
/// item blanked every name and pin on the map for a frame (a node is not
/// laid out in the frame that makes it), which a zoom did many times a
/// second. A hidden item keeps its place until a new one needs it.
pub(crate) struct Anchored<S: Clone + PartialEq + 'static> {
    pool: Rc<RefCell<Pool<S>>>,
}

impl<S: Clone + PartialEq + 'static> Clone for Anchored<S> {
    fn clone(&self) -> Self {
        Anchored {
            pool: self.pool.clone(),
        }
    }
}

impl<S: Clone + PartialEq + 'static> Anchored<S> {
    pub(crate) fn new() -> Self {
        Anchored {
            pool: Rc::new(RefCell::new(Pool {
                slots: Vec::new(),
                by_key: HashMap::new(),
                free: Vec::new(),
                built: vec![signal(false)],
            })),
        }
    }

    /// This frame's items; an item missing from them is hidden. Writes only
    /// what moved. Items stack in the order of their places.
    pub(crate) fn sync(&self, items: impl IntoIterator<Item = (String, Spot, S)>) {
        // A slot, its new look, and its new spot (`Some(None)`: hide it).
        #[allow(clippy::type_complexity)]
        let mut writes: Vec<(Slot<S>, Option<(String, S)>, Option<Option<Spot>>)> = Vec::new();
        let mut added: Vec<(String, Spot, S)> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        let mut grown: Vec<Signal<bool>> = Vec::new();
        {
            let mut pool = self.pool.borrow_mut();
            for (key, spot, look) in items {
                if !seen.insert(key.clone()) {
                    continue;
                }
                match pool.by_key.get(&key) {
                    Some(&i) => {
                        let slot = &pool.slots[i];
                        let changed = slot.look.get_untracked().is_none_or(|(_, was)| was != look);
                        let look = changed.then(|| (key.clone(), look));
                        let spot = match slot.spot.get_untracked() {
                            Some(was) if !was.moved(spot) => None,
                            _ => Some(Some(spot)),
                        };
                        if look.is_some() || spot.is_some() {
                            writes.push((slot.clone(), look, spot));
                        }
                    }
                    None => added.push((key, spot, look)),
                }
            }
            let mut hidden: Vec<(String, usize)> = Vec::new();
            for (key, &i) in pool.by_key.iter() {
                if seen.contains(key) {
                    continue;
                }
                hidden.push((key.clone(), i));
                if pool.slots[i].spot.get_untracked().is_some() {
                    writes.push((pool.slots[i].clone(), None, Some(None)));
                }
            }
            // Past the cap, a new item takes a hidden one's place.
            let mut evictable = hidden.len().saturating_sub(MAX_HIDDEN);
            for (key, spot, look) in added {
                let i = match pool.free.pop() {
                    Some(i) => i,
                    None if evictable > 0 => {
                        evictable -= 1;
                        let (old, i) = hidden.pop().expect("counted");
                        pool.by_key.remove(&old);
                        i
                    }
                    None => {
                        let start = pool.slots.len();
                        for _ in 0..CHUNK {
                            pool.slots.push(Slot {
                                look: signal(None),
                                spot: signal(None),
                            });
                        }
                        pool.free.extend((start + 1..start + CHUNK).rev());
                        grown.push(pool.built.last().expect("one unbuilt").clone());
                        pool.built.push(signal(false));
                        start
                    }
                };
                pool.by_key.insert(key.clone(), i);
                writes.push((pool.slots[i].clone(), Some((key, look)), Some(Some(spot))));
            }
        }
        // Written with the pool released: a write runs the views that read
        // it, and a new batch's view reads the pool.
        for built in grown {
            built.set(true);
        }
        for (slot, look, spot) in writes {
            if let Some(look) = look {
                slot.look.set(Some(look));
            }
            if let Some(spot) = spot {
                slot.spot.set(spot);
            }
        }
    }

    /// The list: a 0 × 0 node at the root's corner, under every other
    /// library screen (`z`, below zero: the Plans outlines at -4, the place
    /// names at -3, the Plans marks at -2, the pins and the people at -1),
    /// with a node per place — a 0 × 0 node at its item's spot, holding what
    /// `look` builds for it around that point. A 0 × 0 node takes no taps;
    /// only what is drawn round it does.
    pub(crate) fn view(&self, z: i32, look: impl Fn(&str, &S) -> View + 'static) -> View {
        let look: Rc<dyn Fn(&str, &S) -> View> = Rc::new(look);
        let frame =
            format!("position: absolute; left: 0; top: 0; width: 0; height: 0; z-index: {z}");
        let list = batch(self.pool.clone(), 0, look);
        view! {
            <div style={frame}>{list}</div>
        }
    }
}

/// Batch `k` of the places once it is built, and behind it the next.
fn batch<S: Clone + PartialEq + 'static>(
    pool: Rc<RefCell<Pool<S>>>,
    k: usize,
    look: Rc<dyn Fn(&str, &S) -> View>,
) -> View {
    let built = pool.borrow().built[k].clone();
    View::Dyn(Rc::new(move || -> View {
        if !built.get() {
            return View::Empty;
        }
        let slots: Vec<Slot<S>> = pool.borrow().slots[k * CHUNK..(k + 1) * CHUNK].to_vec();
        let mut views: Vec<View> = slots
            .into_iter()
            .map(|slot| item(slot, look.clone()))
            .collect();
        views.push(batch(pool.clone(), k + 1, look.clone()));
        View::Fragment(views)
    }))
}

fn item<S: Clone + PartialEq + 'static>(slot: Slot<S>, look: Rc<dyn Fn(&str, &S) -> View>) -> View {
    let spot = slot.spot.clone();
    let style = move || -> String {
        match spot.get() {
            Some(s) if s.alpha < 1.0 => format!(
                "position: absolute; left: {:.1}px; top: {:.1}px; width: 0; height: 0; opacity: {:.2}",
                s.x, s.y, s.alpha
            ),
            Some(s) => format!(
                "position: absolute; left: {:.1}px; top: {:.1}px; width: 0; height: 0",
                s.x, s.y
            ),
            None => "display: none".to_string(),
        }
    };
    let what = slot.look.clone();
    let inner = move || match what.get() {
        Some((key, s)) => look(&key, &s),
        None => View::Empty,
    };
    view! { <div style={style}>{inner}</div> }
}

/// The library's px for an egui point: natively the same (`lib_host`
/// sets `UiScale` to egui's), as DOM the page's, `ui_scale` times larger.
pub(crate) fn px_scale(ui_scale: f32) -> f32 {
    if cfg!(all(feature = "ui-dom", target_arch = "wasm32")) {
        ui_scale
    } else {
        1.0
    }
}

/// A colour as CSS, for a marker's own palette.
pub(crate) fn css(c: theme::Rgba8) -> String {
    let [r, g, b, a] = c.to_array();
    if a == 255 {
        format!("#{r:02x}{g:02x}{b:02x}")
    } else {
        format!("rgba({r}, {g}, {b}, {:.3})", f32::from(a) / 255.0)
    }
}

/// An absolutely placed box: left, top, width, height, then the rest.
pub(crate) fn boxed(left: f32, top: f32, w: f32, h: f32, rest: &str) -> String {
    format!(
        "position: absolute; left: {left:.2}px; top: {top:.2}px; width: {w:.2}px; height: {h:.2}px; {rest}"
    )
}

// ─── The pins ────────────────────────────────────────────────────────────────

/// What a marker looks like, for the list.
#[derive(Clone, PartialEq)]
struct Marker {
    name: String,
    look: PinLook,
}

/// A balloon's look: everything but where it is.
#[derive(Clone, PartialEq)]
struct Balloon {
    text: String,
    width: f32,
    tail: f32,
    mine: bool,
}

#[derive(Clone)]
struct Live {
    markers: Anchored<Marker>,
    balloons: Anchored<Balloon>,
    sharing: Signal<Vec<(String, String)>>,
    /// The key of the marker tapped since the last frame.
    tapped: Rc<RefCell<Option<String>>>,
    /// The chat whose Stop was tapped on the sharing chip.
    stop: Rc<RefCell<Option<String>>>,
    /// The theme it was mounted in: another remounts it.
    dark: bool,
}

thread_local! {
    static MOUNTED: RefCell<Option<Live>> = const { RefCell::new(None) };
}

/// Mounts the pins once, mirrors `place_map_pins`' frame of markers into them, and
/// hands their taps back. `PostUpdate`, before `DriveUi`, as every library
/// screen; the pins' state is inserted here, the first frame.
pub fn drive_lib_pins(
    _main: NonSendMarker,
    lib: Res<LibScreens>,
    pins: Option<ResMut<PlacePins>>,
    metrics: Res<UiMetrics>,
    mut commands: Commands,
) {
    if !lib.pins {
        return;
    }
    let Some(mut pins) = pins else {
        commands.insert_resource(PlacePins::default());
        return;
    };
    let k = px_scale(metrics.ui_scale());
    let dark = theme::p().ink == theme::DARK.ink;
    MOUNTED.with(|m| {
        let mut m = m.borrow_mut();
        if m.as_ref().is_some_and(|live| live.dark != dark) {
            *m = None;
            crate::systems::lib_host::clear_keyed(KEY);
        }
        let live = m.get_or_insert_with(|| {
            let live = Live {
                markers: Anchored::new(),
                balloons: Anchored::new(),
                sharing: signal(Vec::new()),
                tapped: Rc::new(RefCell::new(None)),
                stop: Rc::new(RefCell::new(None)),
                dark,
            };
            crate::systems::lib_host::mount_keyed(KEY, view_of(&live), dark, theme::touch_layout());
            live
        });
        // The taps of the last frame, for `place_map_pins` to do.
        if let Some(key) = live.tapped.borrow_mut().take() {
            pins.lib_tapped = Some(key);
        }
        if let Some(channel) = live.stop.borrow_mut().take() {
            pins.lib_stop = Some(channel);
        }
        let view = &pins.lib_view;
        live.markers.sync(view.pins.iter().map(|p| {
            (
                p.key.clone(),
                Spot::at(p.tip.x * k, p.tip.y * k),
                Marker {
                    name: p.name.clone(),
                    look: p.look.clone(),
                },
            )
        }));
        live.balloons.sync(view.balloons.iter().map(|b| {
            (
                b.key.clone(),
                Spot {
                    x: b.left * k,
                    y: b.bottom * k,
                    alpha: b.alpha,
                },
                Balloon {
                    text: b.text.clone(),
                    width: b.width,
                    tail: b.tail,
                    mine: b.mine,
                },
            )
        }));
        if live.sharing.get_untracked() != view.sharing {
            live.sharing.set(view.sharing.clone());
        }
    });
}

/// The pins, the balloons over them, and the sharing chip over all.
fn view_of(l: &Live) -> View {
    let markers = {
        let tapped = l.tapped.clone();
        l.markers.view(-1, move |key, m| marker(key, m, &tapped))
    };
    let balloons = l.balloons.view(-1, |_, b| balloon(b));
    let sharing = {
        let l = l.clone();
        move || sharing_chip(&l)
    };
    view! {
        {markers}
        {balloons}
        {sharing}
    }
}

/// One marker around its tip, which is its item's origin.
fn marker(key: &str, m: &Marker, tapped: &Rc<RefCell<Option<String>>>) -> View {
    let tap: Handler = {
        let (tapped, key) = (tapped.clone(), key.to_string());
        Rc::new(move |_: &Event| *tapped.borrow_mut() = Some(key.clone()))
    };
    match &m.look {
        PinLook::Place { color } => teardrop(&m.name, *color, tap),
        #[cfg(feature = "comms")]
        PinLook::Point { number, width } => point(&m.name, number, *width, tap),
        #[cfg(feature = "comms")]
        PinLook::Person { kind, chip } => person(&m.name, *kind, *chip, tap),
    }
}

/// A saved place's teardrop (`place_pins::pin`): a square with three round
/// corners turned so the sharp one points down at the tip, a hole in the
/// head, a soft shadow where it stands.
fn teardrop(name: &str, color: [f32; 3], tap: Handler) -> View {
    let to = |k: f32| {
        theme::Rgba8::from_rgb(
            (color[0] * k) as u8,
            (color[1] * k) as u8,
            (color[2] * k) as u8,
        )
    };
    // The rim is the hue darkened, as egui's: one object, not a sticker.
    let (hue, rim) = (css(to(255.0)), css(to(145.0)));
    let r = DROP / 2.0;
    let reach = DROP / std::f32::consts::SQRT_2;
    let h = r + reach;
    let cream = theme::DARK.cream;
    let hole = css(cream.with_alpha((HOLE_ALPHA * 255.0) as u8));
    let shadow_r = r * 0.3;
    let frame = boxed(-r, -h, DROP, h, "");
    let shadow = boxed(
        r - shadow_r,
        h - shadow_r,
        2.0 * shadow_r,
        2.0 * shadow_r,
        &format!("border-radius: {shadow_r:.1}px; background: {TIP_SHADOW}"),
    );
    let drop = boxed(
        0.0,
        0.0,
        DROP,
        DROP,
        &format!(
            "border-radius: {r}px {r}px {r}px 0; background: {hue}; border: 1px solid {rim}; transform: rotate(-45deg)"
        ),
    );
    let hr = r * HOLE;
    let hole = boxed(
        r - hr,
        r - hr,
        2.0 * hr,
        2.0 * hr,
        &format!("border-radius: {hr:.1}px; background: {hole}"),
    );
    let title = name.to_string();
    view! {
        <div style={frame} title={title} on:click={move |e: &Event| tap(e)}>
            <div style={shadow}></div>
            <div style={drop}></div>
            <div style={hole}></div>
        </div>
    }
}

/// A chat's point (`place_pins::point_pin`): a cream label with the number
/// in an ink disc and the name, on a stem down to a dot on the ground.
#[cfg(feature = "comms")]
fn point(name: &str, number: &str, width: f32, tap: Handler) -> View {
    const STEM: f32 = 16.0;
    const DOT: f32 = 8.0;
    const HEAD: f32 = 30.0;
    let (cream, ink) = (css(theme::DARK.cream), css(theme::DARK.ink));
    let h = HEAD + STEM + DOT;
    let frame = boxed(-width / 2.0, -h, width, h, "");
    let head = boxed(
        0.0,
        0.0,
        width,
        HEAD,
        &format!(
            "display: flex; align-items: center; gap: 7px; padding-left: 4px; border-radius: 14px; background: {cream}; box-shadow: {LIFT}"
        ),
    );
    let disc = format!(
        "flex: none; display: flex; align-items: center; justify-content: center; width: 22px; height: 22px; border-radius: 11px; background: {ink}; color: {cream}; font-size: 12px; line-height: 1"
    );
    let label = format!(
        "min-width: 0; max-width: 170px; color: {ink}; font-size: 12.5px; white-space: nowrap; text-overflow: ellipsis; overflow: hidden"
    );
    let stem = boxed(
        width / 2.0 - 1.0,
        HEAD,
        2.0,
        STEM,
        &format!("background: {cream}"),
    );
    let dot = boxed(
        width / 2.0 - DOT / 2.0,
        HEAD + STEM,
        DOT,
        DOT,
        &format!("border-radius: 4px; background: {cream}; border: 2px solid {ink}"),
    );
    let (number, name, title) = (number.to_string(), name.to_string(), name.to_string());
    view! {
        <div style={frame} title={title} on:click={move |e: &Event| tap(e)}>
            <div style={stem}></div>
            <div style={dot}></div>
            <div style={head}>
                <span style={disc}>{number}</span>
                <span style={label}>{name}</span>
            </div>
        </div>
    }
}

/// A person (`chat_people::marker`): their disc on a ring in the colour of
/// which position it is — the avatar's orange, the phone's blue — with a
/// glow, a short stem down to a shadow at the tip, and their name in a chip
/// under it.
#[cfg(feature = "comms")]
fn person(
    name: &str,
    kind: crate::systems::chat_people::MarkerKind,
    chip: f32,
    tap: Handler,
) -> View {
    use crate::systems::chat_people::{MarkerKind, DISC, RING, STEM};
    let colour = kind.colour();
    let (solid, glow) = (css(colour), css(colour.with_alpha(70)));
    let ring = DISC + 2.0 * RING;
    let h = ring + STEM;
    // The box is the ring over the stem; the rest is placed from the tip,
    // which is the box's bottom centre.
    let (cx, cy) = (ring / 2.0, ring / 2.0);
    let frame = boxed(-ring / 2.0, -h, ring, h, "");
    let shadow = boxed(
        cx - 6.0,
        h - 2.5,
        12.0,
        5.0,
        &format!("border-radius: 3px; background: {PERSON_SHADOW}"),
    );
    let stem = boxed(cx - 1.5, ring, 3.0, STEM, &format!("background: {solid}"));
    let g = ring + 10.0;
    let glow = boxed(
        cx - g / 2.0,
        cy - g / 2.0,
        g,
        g,
        &format!("border-radius: {:.1}px; background: {glow}", g / 2.0),
    );
    let ringed = boxed(
        0.0,
        0.0,
        ring,
        ring,
        &format!("border-radius: {:.1}px; background: {solid}", ring / 2.0),
    );
    let disc_at = boxed(RING, RING, DISC, DISC, "");
    let chip_style = boxed(
        cx - chip / 2.0,
        h + 4.0,
        chip,
        crate::systems::chat_people::NAME_TEXT + 6.0,
        &format!(
            "display: flex; align-items: center; justify-content: center; border-radius: 8px; background: var(--m-surface); color: var(--m-text-strong); font-size: {}px; white-space: nowrap; line-height: 1",
            crate::systems::chat_people::NAME_TEXT
        ),
    );
    let disc = crate::systems::lib_chats::disc(
        name,
        crate::systems::discs::ChatKind::Private,
        DISC as u32,
    );
    // The colour says which position this is; the title says it in words.
    let title = match kind {
        MarkerKind::Avatar => name.to_string(),
        MarkerKind::Phone => format!("{name} · where their phone is"),
    };
    let name = name.to_string();
    view! {
        <div style={frame} title={title} on:click={move |e: &Event| tap(e)}>
            <div style={shadow}></div>
            <div style={stem}></div>
            <div style={glow}></div>
            <div style={ringed}></div>
            <div style={disc_at}>{disc}</div>
            <div style={chip_style}>{name}</div>
        </div>
    }
}

/// A balloon's text size: `chat_people`'s, which the web build (no chat, so
/// no balloons) does not have.
#[cfg(feature = "comms")]
use crate::systems::chat_people::BALLOON_TEXT;
#[cfg(not(feature = "comms"))]
const BALLOON_TEXT: f32 = 13.5;

/// A speech balloon (`chat_people::balloon`), its bottom-left corner on the
/// item's origin: the words, and a tail under them pointing at the person.
/// Nothing on it listens, so a tap goes through it to the world.
fn balloon(b: &Balloon) -> View {
    let (fill, ink) = if b.mine {
        ("var(--m-accent)", "var(--m-on-accent)")
    } else {
        ("var(--m-surface-raised)", "var(--m-text-strong)")
    };
    let frame = format!(
        "position: absolute; left: 0; bottom: 0; width: {:.1}px; padding: 7px 11px; border-radius: 14px; background: {fill}; color: {ink}; font-size: {}px; box-shadow: {LIFT}",
        b.width, BALLOON_TEXT
    );
    let tail = format!(
        "position: absolute; left: {:.1}px; bottom: -4px; width: 9px; height: 9px; background: {fill}; transform: rotate(45deg)",
        b.tail - 4.5
    );
    let text = b.text.clone();
    view! {
        <div style={frame}>
            <div style={tail}></div>
            <span>{text}</span>
        </div>
    }
}

/// The "you are sharing your real location" chip: top centre, under the
/// search bar, a line per chat with a Stop for each. Always over the pins.
fn sharing_chip(l: &Live) -> View {
    let lines = l.sharing.get();
    if lines.is_empty() {
        return View::Empty;
    }
    let top = crate::systems::dock::reserved_top();
    // The chip's ground is one colour in both themes (`theme::REAL_LOCATION`,
    // what the blue means), so its ink is the dark theme's.
    let (fill, ink) = (css(theme::REAL_LOCATION), css(theme::DARK.ink));
    let rows: Vec<View> = lines
        .into_iter()
        .map(|(channel, line)| {
            let stop = {
                let stop = l.stop.clone();
                Rc::new(move |_: &Event| *stop.borrow_mut() = Some(channel.clone())) as Handler
            };
            let word = format!(
                "padding: 2px 10px; border-radius: var(--m-radius-full); color: {ink}; font-weight: 600; border: 1px solid {ink}"
            );
            let words = format!("color: {ink}; font-size: var(--m-font-size-sm)");
            view! {
                <div style="display: flex; align-items: center; gap: 10px">
                    <span style={words}>{line}</span>
                    <span style={word} on:click={move |e: &Event| stop(e)}>"Stop"</span>
                </div>
            }
        })
        .collect();
    let row = format!(
        "position: absolute; left: 0; right: 0; top: {top:.0}px; display: flex; justify-content: center"
    );
    let chip = format!(
        "display: flex; flex-direction: column; gap: 4px; padding: 5px 10px; border-radius: 12px; background: {fill}"
    );
    view! {
        <div style={row}>
            <div style={chip} on:click={|_: &Event| {}}>{rows}</div>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items(keys: &[&str]) -> Vec<(String, Spot, u32)> {
        keys.iter()
            .map(|k| (k.to_string(), Spot::at(1.0, 2.0), 7))
            .collect()
    }

    #[test]
    fn a_new_item_leaves_the_others_in_their_places() {
        let list = Anchored::<u32>::new();
        list.sync(items(&["a", "b"]));
        let (a, b) = {
            let pool = list.pool.borrow();
            (pool.by_key["a"], pool.by_key["b"])
        };
        list.sync(items(&["a", "b", "c"]));
        let pool = list.pool.borrow();
        assert_eq!((pool.by_key["a"], pool.by_key["b"]), (a, b));
        // One batch built, the next still waiting.
        assert_eq!(pool.built.len(), 2);
        assert!(pool.built[0].get_untracked());
        assert!(!pool.built[1].get_untracked());
    }

    #[test]
    fn a_full_batch_opens_the_next() {
        let list = Anchored::<u32>::new();
        let keys: Vec<String> = (0..CHUNK + 1).map(|i| i.to_string()).collect();
        let refs: Vec<&str> = keys.iter().map(String::as_str).collect();
        list.sync(items(&refs));
        let pool = list.pool.borrow();
        assert_eq!(pool.slots.len(), 2 * CHUNK);
        assert!(pool.built[1].get_untracked());
    }

    #[test]
    fn past_the_cap_a_new_item_takes_a_hidden_place() {
        let list = Anchored::<u32>::new();
        // Whole batches, so no place is free.
        let n = CHUNK * (MAX_HIDDEN / CHUNK + 1);
        let keys: Vec<String> = (0..n).map(|i| i.to_string()).collect();
        let refs: Vec<&str> = keys.iter().map(String::as_str).collect();
        list.sync(items(&refs));
        assert!(list.pool.borrow().free.is_empty());
        list.sync(items(&["new"]));
        let pool = list.pool.borrow();
        assert_eq!(pool.slots.len(), n);
        assert_eq!(pool.by_key.len(), n);
        assert!(pool.by_key.contains_key("new"));
    }
}

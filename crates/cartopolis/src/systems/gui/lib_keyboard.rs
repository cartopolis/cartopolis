//! The on-screen keyboard's keys, drawn by the component library
//! (`lib_host`): its state, placement and fingers are `onscreen_keyboard`'s,
//! which raises the keys while a library text field has the focus. This
//! draws them where they stand — the bottom edge, full width, or the top
//! where they flip to clear their field — with the keys under a finger
//! pressed and the character under each finger shown above its key; hands
//! what they typed to the focused field (`mithril_bevy::KeyEdits`, Enter as
//! the field's `keydown`); clears the field's focus when they are put away;
//! and tells `onscreen_keyboard` where the field is.
//!
//! The keys carry `data-keep-focus`, so a press on one leaves the field its
//! focus (`mithril_bevy`'s `events.rs`). They type from the fingers
//! (`OnScreenKeyboard::track`), not their clicks; a click types only with no
//! finger behind it (`OnScreenKeyboard::clicked`: the MCP server's `press`).

use std::cell::RefCell;
use std::rc::Rc;

use bevy::ecs::system::NonSendMarker;
use bevy::input_focus::InputFocus;
use bevy::prelude::*;
use bevy::text::EditableText;
use bevy::ui::{ComputedNode, UiGlobalTransform};
use mithril::prelude::{el, signal, IntoView, Signal, View};

use crate::systems::onscreen_keyboard::{
    cap_at, cap_name, Cap, KeyLayer, KeyPlace, OnScreenKeyboard, Preview, ShiftState, Stroke,
};
use crate::systems::{icons, theme};

/// How many fingers get a key preview at once: two thumbs.
const PREVIEW_SLOTS: usize = 2;

/// The keys as drawn, rebuilt when they move (a flip to the top, a resize).
#[derive(Clone, PartialEq)]
struct Geometry {
    slab: Rect,
    keys: Vec<KeyPlace>,
}

#[derive(Clone)]
struct Live {
    geometry: Geometry,
    face: Signal<(KeyLayer, ShiftState)>,
    /// The keys under a finger, by row and column.
    held: Signal<Vec<(usize, usize)>>,
    /// The key previews, relative to the slab.
    previews: Signal<Vec<Preview>>,
    /// The keys clicked since the last frame, by row and column.
    clicks: Rc<RefCell<Vec<(usize, usize)>>>,
}

thread_local! {
    static MOUNTED: RefCell<Option<Live>> = const { RefCell::new(None) };
}

/// Mounts, feeds and unmounts the keyboard. `PostUpdate`, before
/// `DriveUi`, as every library screen; after `drive_onscreen_keyboard`
/// (`Update`), which decides whether the keys are up and where.
pub fn drive_lib_keyboard(
    _main: NonSendMarker,
    mut kb: ResMut<OnScreenKeyboard>,
    mut edits: ResMut<mithril_bevy::KeyEdits>,
    focus: Option<ResMut<InputFocus>>,
    fields: Query<(&ComputedNode, &UiGlobalTransform), With<EditableText>>,
    time: Res<Time<Real>>,
) {
    // Last frame's clicks, against the layer they were made on.
    let clicks: Vec<(usize, usize)> = MOUNTED.with(|m| {
        m.borrow()
            .as_ref()
            .map(|live| std::mem::take(&mut *live.clicks.borrow_mut()))
            .unwrap_or_default()
    });
    let now = time.elapsed_secs_f64();
    for (row, col) in clicks {
        kb.clicked(row, col, now);
    }
    for stroke in kb.take_strokes() {
        edits.push(edit_of(stroke));
    }

    // The keys went away (hide, Back): the field's focus too, or the field
    // would hold the keyboard and bring the keys straight back.
    let blur = kb.take_blur();
    let mut focused = None;
    if let Some(mut focus) = focus {
        if blur && focus.get().is_some() {
            focus.clear();
        }
        focused = focus.get();
    }
    // Where the focused field stands, for the keys to get off it.
    let field = focused.and_then(|e| {
        let (node, transform) = fields.get(e).ok()?;
        let k = node.inverse_scale_factor();
        let size = node.size() * k;
        (size.x > 0.0 && size.y > 0.0)
            .then(|| (e, Rect::from_center_size(transform.translation * k, size)))
    });
    kb.set_field(field);

    let shown = kb.keys().map(|(slab, keys)| Geometry { slab, keys });
    let face = kb.face();
    let held = kb.held();
    let previews: Vec<Preview> = match shown.as_ref() {
        Some(g) => kb
            .previews()
            .into_iter()
            .take(PREVIEW_SLOTS)
            .map(|p| Preview {
                rect: Rect::from_corners(p.rect.min - g.slab.min, p.rect.max - g.slab.min),
                ch: p.ch,
            })
            .collect(),
        None => Vec::new(),
    };
    MOUNTED.with(|m| {
        let mut m = m.borrow_mut();
        // Moved: drawn again where it stands now.
        let moved = match (shown.as_ref(), m.as_ref()) {
            (Some(g), Some(live)) => live.geometry != *g,
            _ => false,
        };
        if moved {
            *m = None;
            crate::systems::lib_host::clear_keyed("keyboard");
        }
        match (shown, m.as_mut()) {
            (None, None) => {}
            (None, Some(_)) => {
                *m = None;
                crate::systems::lib_host::clear_keyed("keyboard");
            }
            (Some(geometry), None) => {
                let live = Live {
                    geometry,
                    face: signal(face),
                    held: signal(held),
                    previews: signal(previews),
                    clicks: Rc::new(RefCell::new(Vec::new())),
                };
                let p = theme::p();
                crate::systems::lib_host::mount_keyed(
                    "keyboard",
                    keyboard(&live),
                    p.ink == theme::DARK.ink,
                    theme::touch_layout(),
                );
                *m = Some(live);
            }
            (Some(_), Some(live)) => {
                if live.face.get_untracked() != face {
                    live.face.set(face);
                }
                if live.held.peek(|now| *now != held) {
                    live.held.set(held);
                }
                if live.previews.peek(|now| *now != previews) {
                    live.previews.set(previews);
                }
            }
        }
    });
}

/// A stroke as the library's field takes it.
fn edit_of(stroke: Stroke) -> mithril_bevy::KeyEdit {
    use mithril_bevy::KeyEdit;
    match stroke {
        Stroke::Text(text) => KeyEdit::Text(text),
        Stroke::Backspace => KeyEdit::Backspace,
        Stroke::Enter => KeyEdit::Enter,
        Stroke::Left { select } => KeyEdit::Left { select },
        Stroke::Right { select } => KeyEdit::Right { select },
        Stroke::SelectAll => KeyEdit::SelectAll,
    }
}

/// An icon glyph, in the icon face.
fn icon(glyph: &'static str) -> View {
    el("span")
        .attr(
            "style",
            "font-family: 'Material Symbols Rounded'; font-size: 22px; line-height: 1",
        )
        .child(glyph)
        .into_view()
}

/// What a key shows: its character in the shift state, or its glyph.
fn label(cap: Cap, shift: ShiftState, layer: KeyLayer) -> View {
    match cap {
        Cap::Char { base, alt } => {
            let ch = if shift.alt() { alt } else { base };
            ch.to_string().into_view()
        }
        // The space bar is a bar: a word is a translation problem and a
        // legibility problem at this size; the bar is neither.
        Cap::Space => el("span")
            .attr(
                "style",
                "display: block; width: 44%; height: 3px; border-radius: 2px; background: var(--m-text-dim)",
            )
            .into_view(),
        // A band across a line of text: what select-all does, at any size.
        Cap::SelectAll => el("span")
            .attr(
                "style",
                "display: flex; align-items: center; justify-content: center; width: 56%; height: 32%; border-radius: 3px; background: var(--m-accent-soft)",
            )
            .child(
                el("span").attr(
                    "style",
                    "display: block; width: 76%; height: 3px; border-radius: 2px; background: var(--m-text)",
                ),
            )
            .into_view(),
        Cap::Backspace => icon(icons::BACKSPACE),
        Cap::Enter => icon(icons::ENTER),
        Cap::Shift => icon(icons::SHIFT),
        Cap::Hide => icon(icons::HIDE_KEYBOARD),
        Cap::Left => icon(icons::CHEVRON_LEFT),
        Cap::Right => icon(icons::CHEVRON_RIGHT),
        // The other layer's name.
        Cap::Layer => match layer {
            KeyLayer::Letters => "123",
            KeyLayer::Symbols => "ABC",
        }
        .into_view(),
    }
}

/// A key's fill and ink, by role: characters raised, the modifiers back on
/// the panel's surface, Enter in the accent, shift lit while armed and solid
/// while locked; any key under a finger pressed.
fn colours(cap: Cap, shift: ShiftState, down: bool) -> (&'static str, &'static str) {
    if down {
        return ("var(--m-accent-soft)", "var(--m-text)");
    }
    match cap {
        Cap::Char { .. } | Cap::Space => ("var(--m-surface-raised)", "var(--m-text)"),
        Cap::Enter => ("var(--m-accent)", "var(--m-on-accent)"),
        Cap::Shift if shift == ShiftState::Lock => ("var(--m-accent)", "var(--m-on-accent)"),
        Cap::Shift if shift == ShiftState::Once => ("var(--m-accent-soft)", "var(--m-accent-text)"),
        _ => ("var(--m-surface-sunken)", "var(--m-text-dim)"),
    }
}

/// One key, at its rect within the slab, its face following the layer,
/// shift and the fingers on it.
fn key(live: &Live, k: KeyPlace) -> View {
    let origin = live.geometry.slab.min;
    let place = format!(
        "position: absolute; left: {:.1}px; top: {:.1}px; width: {:.1}px; height: {:.1}px; \
         display: flex; align-items: center; justify-content: center; padding: 0; margin: 0; \
         border: 1px solid var(--m-border); border-radius: var(--m-radius); \
         font-size: 18px; line-height: 1;",
        k.rect.min.x - origin.x,
        k.rect.min.y - origin.y,
        k.rect.width(),
        k.rect.height(),
    );
    let (row, col) = (k.row, k.col);
    let style = {
        let face = live.face.clone();
        let held = live.held.clone();
        move || -> String {
            let (layer, shift) = face.get();
            let down = held.get().contains(&(row, col));
            let (fill, ink) = cap_at(layer, row, col)
                .map_or(("var(--m-surface-raised)", "var(--m-text)"), |cap| {
                    colours(cap, shift, down)
                });
            format!("{place} background: {fill}; color: {ink}")
        }
    };
    let name = {
        let face = live.face.clone();
        move || -> String {
            let (layer, shift) = face.get();
            cap_at(layer, row, col).map_or_else(String::new, |cap| cap_name(cap, shift.alt()))
        }
    };
    let glyph = {
        let face = live.face.clone();
        move || -> View {
            let (layer, shift) = face.get();
            cap_at(layer, row, col).map_or(View::Empty, |cap| label(cap, shift, layer))
        }
    };
    let clicks = live.clicks.clone();
    el("button")
        .attr("type", "button")
        .attr("data-keep-focus", "1")
        .attr("aria-label", name)
        .attr("style", style)
        .on("click", move |_| clicks.borrow_mut().push((row, col)))
        .child(glyph)
        .into_view()
}

/// The `slot`th key preview: a raised bubble with the character in it,
/// hidden while fewer fingers are on character keys. Restyled in place, never
/// rebuilt, as a finger moves.
fn preview(live: &Live, slot: usize) -> View {
    let style = {
        let previews = live.previews.clone();
        move || -> String {
            previews.get().get(slot).map_or_else(
                || "display: none".to_owned(),
                |p| {
                    format!(
                        "position: absolute; left: {:.1}px; top: {:.1}px; width: {:.1}px; \
                         height: {:.1}px; display: flex; align-items: center; \
                         justify-content: center; background: var(--m-surface-raised); \
                         color: var(--m-text); border: 1px solid var(--m-border); \
                         border-radius: var(--m-radius); \
                         box-shadow: 0 4px 14px rgba(0, 0, 0, 0.47); \
                         font-size: {:.0}px; line-height: 1",
                        p.rect.min.x,
                        p.rect.min.y,
                        p.rect.width(),
                        p.rect.height(),
                        p.rect.height() * 0.55,
                    )
                },
            )
        }
    };
    let text = {
        let previews = live.previews.clone();
        move || -> String {
            previews
                .get()
                .get(slot)
                .map_or_else(String::new, |p| p.ch.to_string())
        }
    };
    el("div")
        .attr("aria-hidden", "true")
        .attr("style", style)
        .child(text)
        .into_view()
}

/// The slab: every key, absolute in a panel on the app's surface, and the
/// key previews over them.
fn keyboard(live: &Live) -> View {
    let slab = live.geometry.slab;
    let frame = format!(
        "position: absolute; left: {:.1}px; top: {:.1}px; width: {:.1}px; height: {:.1}px; \
         z-index: 100; background: var(--m-surface); border: 1px solid var(--m-border); \
         border-radius: var(--m-radius-lg)",
        slab.min.x,
        slab.min.y,
        slab.width(),
        slab.height(),
    );
    let mut children: Vec<View> = live.geometry.keys.iter().map(|k| key(live, *k)).collect();
    children.extend((0..PREVIEW_SLOTS).map(|slot| preview(live, slot)));
    el("div")
        .attr("data-keep-focus", "1")
        .attr("style", frame)
        // A tap between two keys is the keyboard's, not the world's
        // (`UiPointer` counts the elements that listen).
        .on("click", |_| {})
        .children(children)
        .into_view()
}

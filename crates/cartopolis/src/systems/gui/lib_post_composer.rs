//! The post composer over the map drawn by the component library
//! (`lib_host`): a post started from the map (Post here, the camera),
//! written with the place it is about still in sight.
//!
//! `community_posts::drive_floating_composer` decides whether the composer
//! shows (an open panel or the camera hide it, a draft gone ends it) and
//! publishes where it stands and who the post reaches in
//! `Posts::composer_view`. The draft's words are diffed into `Posts::draft`
//! each frame, both ways; every other tap goes through
//! `community_posts::composer_act`.
//!
//! **Placement.** Held sideways, beside the map at the right, 380 px or half
//! the screen wide and the screen's height; held upright, along the bottom,
//! the screen's width and at most 55 % of its height.
//!
//! The library has no image node, so the polaroid and the picture are rows
//! naming them, each with its remove; the public colour is the library's
//! info tone, the nearest token.

use std::cell::RefCell;
use std::rc::Rc;

use bevy::ecs::system::NonSendMarker;
use bevy::prelude::*;
use cartopolis_core::posts::{MAX_PICTURE_ALT, MAX_POST_TEXT};
use uuid::Uuid;
// The library's, not Bevy's: an explicit import outranks the two globs.
use mithril::prelude::*;
use mithril::prelude::{Button, Event};

use crate::systems::communities::Communities;
use crate::systems::community_posts::{
    composer_act, reach_words, ready_to_post, ComposerAction, Draft,
};
use crate::systems::lib_place_card::{cell, icon};
use crate::systems::lib_settings::LibScreens;
use crate::systems::ui_input::UiMetrics;
use crate::systems::{icons, theme};

/// The composer's name in the library's host.
const KEY: &str = "post_composer";

/// Its width held sideways, px, before half the screen narrows it.
const SIDE_WIDTH: f32 = 380.0;

/// Where the composer stands, px in the library's root.
#[derive(Clone, Copy, PartialEq, Default)]
struct Placement {
    left: f32,
    /// From the top held sideways, from the bottom held upright.
    top: Option<f32>,
    bottom: Option<f32>,
    width: f32,
    max_height: f32,
}

impl Placement {
    fn of(w: f32, h: f32, sideways: bool, ui_scale: f32) -> Self {
        let m = theme::OSD_MARGIN;
        let k = if cfg!(all(feature = "ui-dom", target_arch = "wasm32")) {
            ui_scale
        } else {
            1.0
        };
        if sideways {
            let width = SIDE_WIDTH.min(w * 0.5);
            Placement {
                left: (w - m - width).max(0.0) * k,
                top: Some(m * k),
                bottom: None,
                width: width * k,
                max_height: (h - 2.0 * m).max(160.0) * k,
            }
        } else {
            Placement {
                left: m * k,
                top: None,
                bottom: Some(m * k),
                width: (w - 2.0 * m).max(0.0) * k,
                max_height: (h * 0.55).max(160.0) * k,
            }
        }
    }

    fn style(&self) -> String {
        let edge = match (self.top, self.bottom) {
            (Some(t), _) => format!("top: {t:.0}px"),
            (None, Some(b)) => format!("bottom: {b:.0}px"),
            (None, None) => "bottom: 0px".to_string(),
        };
        format!(
            "position: absolute; left: {:.0}px; {edge}; width: {:.0}px; max-height: {:.0}px; \
             display: flex; flex-direction: column; background: var(--m-surface); \
             border: 1px solid var(--m-border); border-radius: var(--m-radius-lg); \
             box-shadow: var(--m-shadow-lg); overflow: hidden",
            self.left, self.width, self.max_height
        )
    }
}

/// The draft's words and switches, as typed: diffed against the draft each
/// frame both ways.
#[derive(Clone, PartialEq, Default)]
struct Fields {
    text: String,
    warning: Option<String>,
    alt: String,
    sensitive: bool,
}

impl Fields {
    fn of(d: &Draft) -> Self {
        Fields {
            text: d.text.clone(),
            warning: d.warning.clone(),
            alt: d.alt.clone(),
            sensitive: d.sensitive,
        }
    }
}

#[derive(Clone)]
struct Live {
    /// The community the draft was mounted for: another mounts it again.
    community: Uuid,
    text: Signal<String>,
    warning_on: Signal<bool>,
    warning: Signal<String>,
    alt: Signal<String>,
    sensitive: Signal<bool>,
    /// The fields as last agreed with the draft.
    last: Rc<RefCell<Fields>>,
    /// The place's name, or `None` while the view is being taken.
    place: Signal<Option<String>>,
    /// A polaroid, a picture on the post.
    snapshot: Signal<bool>,
    picture: Signal<bool>,
    /// A picture or a polaroid is on its way: Camera and Upload wait.
    busy: Signal<bool>,
    sending: Signal<bool>,
    /// Post waits: no text, no place, or something still on its way.
    not_ready: Signal<bool>,
    empty: Signal<bool>,
    /// What went wrong with a picture.
    note: Signal<Option<String>>,
    reach: Signal<String>,
    placement: Signal<Placement>,
    acts: Rc<RefCell<Vec<ComposerAction>>>,
}

impl Live {
    fn act(&self, a: ComposerAction) -> Option<Handler> {
        let acts = self.acts.clone();
        Some(Rc::new(move |_: &Event| acts.borrow_mut().push(a)) as Handler)
    }

    /// The fields as the view holds them.
    fn fields(&self) -> Fields {
        Fields {
            text: self.text.get_untracked(),
            warning: self
                .warning_on
                .get_untracked()
                .then(|| self.warning.get_untracked()),
            alt: self.alt.get_untracked(),
            sensitive: self.sensitive.get_untracked(),
        }
    }

    /// The fields → the view.
    fn show_fields(&self, f: &Fields) {
        put(&self.text, f.text.clone());
        put(&self.warning_on, f.warning.is_some());
        if let Some(w) = &f.warning {
            put(&self.warning, w.clone());
        }
        put(&self.alt, f.alt.clone());
        put(&self.sensitive, f.sensitive);
    }
}

/// Write a signal only when its value moves.
fn put<T: Clone + PartialEq + 'static>(s: &Signal<T>, v: T) {
    if s.get_untracked() != v {
        s.set(v);
    }
}

thread_local! {
    static MOUNTED: RefCell<Option<Live>> = const { RefCell::new(None) };
}

/// What the composer shows besides the fields, this frame.
struct Info {
    place: Option<String>,
    snapshot: bool,
    picture: bool,
    busy: bool,
    sending: bool,
    ready: bool,
    empty: bool,
    note: Option<String>,
}

fn info_of(cs: &Communities, d: &Draft) -> Info {
    Info {
        place: d.place.as_ref().map(|p| {
            p.name
                .clone()
                .unwrap_or_else(|| format!("{:.5}, {:.5}", p.latitude, p.longitude))
        }),
        snapshot: d.snapshot.is_some(),
        picture: d.picture.is_some(),
        busy: !cs.picture_for_post_idle() || cs.posts.snapshot_busy,
        sending: cs.posts.sending,
        ready: ready_to_post(cs),
        empty: d.text.trim().is_empty(),
        note: cs.picture_note.clone(),
    }
}

/// Mounts, bridges and unmounts the composer. `PostUpdate`, before
/// `DriveUi`, as every library screen.
pub fn drive_lib_post_composer(
    _main: NonSendMarker,
    lib: Res<LibScreens>,
    mut cs: ResMut<Communities>,
    metrics: Res<UiMetrics>,
) {
    if !lib.post_composer {
        return;
    }
    let size = metrics.viewport_pts();
    MOUNTED.with(|m| {
        let mut m = m.borrow_mut();
        if let Some(live) = m.as_ref() {
            // What was typed → the draft.
            let typed = live.fields();
            if typed != *live.last.borrow() {
                if let Some(d) = cs.posts.draft.as_mut() {
                    d.text = typed.text.clone();
                    d.warning = typed.warning.clone();
                    d.alt = typed.alt.clone();
                    d.sensitive = typed.sensitive;
                }
                *live.last.borrow_mut() = typed;
            }
            // The taps of the frame.
            let acts: Vec<ComposerAction> = std::mem::take(&mut *live.acts.borrow_mut());
            for a in acts {
                composer_act(&mut cs, a);
            }
        }
        // Shown by `drive_floating_composer`, and the draft still there.
        let view = cs.posts.composer_view.clone().filter(|v| {
            cs.posts
                .draft
                .as_ref()
                .is_some_and(|d| d.community == v.community)
        });
        let stale = m
            .as_ref()
            .is_some_and(|live| view.as_ref().map(|v| v.community) != Some(live.community));
        if stale {
            *m = None;
            crate::systems::lib_host::clear_keyed(KEY);
        }
        let (Some(view), Some(draft)) = (view, cs.posts.draft.as_ref()) else {
            return;
        };
        let placement = Placement::of(size.x, size.y, view.sideways, metrics.ui_scale());
        let fields = Fields::of(draft);
        let info = info_of(&cs, draft);
        let reach = reach_words(view.federating, view.followers);
        match m.as_mut() {
            None => {
                let live = Live {
                    community: view.community,
                    text: signal(fields.text.clone()),
                    warning_on: signal(fields.warning.is_some()),
                    warning: signal(fields.warning.clone().unwrap_or_default()),
                    alt: signal(fields.alt.clone()),
                    sensitive: signal(fields.sensitive),
                    last: Rc::new(RefCell::new(fields)),
                    place: signal(info.place),
                    snapshot: signal(info.snapshot),
                    picture: signal(info.picture),
                    busy: signal(info.busy),
                    sending: signal(info.sending),
                    not_ready: signal(!info.ready),
                    empty: signal(info.empty),
                    note: signal(info.note),
                    reach: signal(reach),
                    placement: signal(placement),
                    acts: Rc::new(RefCell::new(Vec::new())),
                };
                let p = theme::p();
                crate::systems::lib_host::mount_keyed(
                    KEY,
                    view_of(&live),
                    p.ink == theme::DARK.ink,
                    theme::touch_layout(),
                );
                if let Some(path) = std::env::var_os("CARTO_UI_LIB_HTML") {
                    if let Err(e) = std::fs::write(&path, html(&live)) {
                        warn!(?path, error = %e, "lib post composer: could not write the HTML");
                    }
                }
                *m = Some(live);
            }
            Some(live) => {
                // The draft moved elsewhere (posted, a field cleared) → the view.
                if fields != *live.last.borrow() {
                    live.show_fields(&fields);
                    *live.last.borrow_mut() = fields;
                }
                put(&live.place, info.place);
                put(&live.snapshot, info.snapshot);
                put(&live.picture, info.picture);
                put(&live.busy, info.busy);
                put(&live.sending, info.sending);
                put(&live.not_ready, !info.ready);
                put(&live.empty, info.empty);
                put(&live.note, info.note);
                put(&live.reach, reach);
                put(&live.placement, placement);
            }
        }
    });
}

/// The HTML from the library's string renderer, for the browser beside a
/// capture.
fn html(l: &Live) -> String {
    use mithril::view::{mount, StringRenderer};
    let r = Rc::new(RefCell::new(StringRenderer::new()));
    let root = r.borrow().root();
    let mounted = mount(view_of(l), &r, &root, None);
    let out = r.borrow().html();
    drop(mounted);
    out
}

/// A quiet sentence, as `theme::note` was.
fn note_line(text: String) -> View {
    view! {
        <p style="margin: 0; color: var(--m-text-dim); font-size: var(--m-font-size-sm)">{text}</p>
    }
}

/// An attachment as a row: what it is, and its remove.
fn attachment(glyph: &'static str, what: &'static str, remove: Option<Handler>) -> View {
    view! {
        <div style="display: flex; align-items: center; gap: 12px; padding: 8px 8px 8px 16px; border-radius: var(--m-radius); background: var(--m-surface-raised)">
            <span style="color: var(--m-accent)">{icon(glyph)}</span>
            <span style="flex: 1; min-width: 0; color: var(--m-text)">{what}</span>
            <Button variant=Variant::Ghost icon={icon(icons::DELETE)} icon_only=true onclick={remove} title="Remove it from the post" />
        </div>
    }
}

/// The composer: its title row with Post and ✕, then the words, where, what
/// goes with it, and who it reaches.
fn view_of(l: &Live) -> View {
    let frame = {
        let placement = l.placement.clone();
        move || placement.get().style()
    };
    let post = {
        let l = l.clone();
        move || -> View {
            let label = if l.sending.get() {
                "Posting…"
            } else {
                "Post"
            };
            let waiting = l.not_ready.get();
            view! {
                <Button variant=Variant::Primary icon={icon(icons::SEND)} disabled={waiting} onclick={l.act(ComposerAction::Post)} title="Post it: public, with this place">{label}</Button>
            }
        }
    };
    // Where: one line, and the one thing to do about it.
    let place = {
        let l = l.clone();
        move || -> View {
            let what: View = match l.place.get() {
                Some(name) => view! {
                    <div style="flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 2px">
                        <span style="overflow: hidden; white-space: nowrap; color: var(--m-text-strong)">{name}</span>
                        <span style="color: var(--m-text-dim); font-size: var(--m-font-size-sm)">"Pinned where you are looking"</span>
                    </div>
                },
                None => view! {
                    <div style="flex: 1; min-width: 0; display: flex; align-items: center; gap: 8px; color: var(--m-text-dim)">
                        <Spinner size=Size::Sm />"Taking the view…"
                    </div>
                },
            };
            view! {
                <div style="display: flex; align-items: center; gap: 12px; padding: 12px 12px 12px 16px; border-radius: var(--m-radius); background: var(--m-surface-raised)">
                    <span style="flex: none; color: var(--m-accent)">{icon(icons::PIN)}</span>
                    {what}
                    <Button icon={icon(icons::HERE)} onclick={l.act(ComposerAction::UseView)} title="Close the panel, look somewhere else, come back and press this">"Use view"</Button>
                </div>
            }
        }
    };
    // What goes with it, the polaroid first, as it goes out first.
    let attachments = {
        let l = l.clone();
        move || -> View {
            let snapshot = if l.snapshot.get() {
                attachment(
                    icons::CAMERA,
                    "Polaroid of the view",
                    l.act(ComposerAction::RemoveSnapshot),
                )
            } else {
                View::Empty
            };
            let picture = if l.picture.get() {
                attachment(
                    icons::UPLOAD,
                    "Picture",
                    l.act(ComposerAction::RemovePicture),
                )
            } else {
                View::Empty
            };
            view! { {snapshot}{picture} }
        }
    };
    // Each optional field its own reactive node, so the others (and the
    // keyboard on one) are left alone when it comes or goes.
    let warning = {
        let l = l.clone();
        move || -> View {
            if !l.warning_on.get() {
                return View::Empty;
            }
            let model = l.warning.clone();
            view! {
                <FieldText model={model} placeholder="What readers see before they open it" autocomplete="off" id="post-warning" />
            }
        }
    };
    let picture_extras = {
        let l = l.clone();
        move || -> View {
            if !l.picture.get() {
                return View::Empty;
            }
            let alt = l.alt.clone();
            let sensitive = l.sensitive.clone();
            view! {
                <FieldTextarea model={alt} rows={2_u32} maxlength={MAX_PICTURE_ALT as u32} placeholder="What the photo shows, in words" id="post-alt" />
                <FieldCheckbox model={sensitive} label="Sensitive: blur the photo until opened" id="post-sensitive" />
            }
        }
    };
    // Camera and Upload; with a picture on the post they replace it.
    let add = {
        let l = l.clone();
        move || -> View {
            let has = l.picture.get();
            let busy = l.busy.get();
            let (camera, upload) = if has {
                ("Retake", "Replace")
            } else {
                ("Camera", "Upload")
            };
            view! {
                <div style="display: flex; gap: 8px">
                    {cell(view! { <Button icon={icon(icons::CAMERA)} disabled={busy} onclick={l.act(ComposerAction::Camera)} title="Take a picture for this post">{camera}</Button> })}
                    {cell(view! { <Button icon={icon(icons::UPLOAD)} disabled={busy} onclick={l.act(ComposerAction::Upload)} title="A picture from this device">{upload}</Button> })}
                </div>
            }
        }
    };
    let note = {
        let note = l.note.clone();
        move || -> View {
            match note.get() {
                Some(n) => note_line(n),
                None => View::Empty,
            }
        }
    };
    let reach = {
        let reach = l.reach.clone();
        move || -> View {
            let words = reach.get();
            view! {
                <div style="display: flex; align-items: flex-start; gap: 8px; color: var(--m-info); font-size: var(--m-font-size-sm)">
                    {icon(icons::SHARE)}
                    <span style="flex: 1; min-width: 0">{words}</span>
                </div>
            }
        }
    };
    let hint = {
        let empty = l.empty.clone();
        move || -> View {
            if empty.get() {
                note_line("Write something, and Post above goes live.".to_string())
            } else {
                View::Empty
            }
        }
    };
    let text = l.text.clone();
    let warning_on = l.warning_on.clone();
    // The card's own node listens (a click that does nothing), so the
    // pointer over its padding is the library's too.
    view! {
        <div style={frame} on:click={|_: &Event| {}}>
            <div style="flex: none; display: flex; align-items: center; gap: 12px; min-height: 56px; padding: 8px 12px 8px 20px; border-bottom: 1px solid var(--m-border)">
                <h1 style="flex: 1; min-width: 0; margin: 0; font-size: 17px; font-weight: 600; color: var(--m-text-strong)">"New post"</h1>
                {post}
                <CloseButton onclick={l.act(ComposerAction::Discard)} label="Discard" />
            </div>
            <div style="flex: 1; min-height: 0; overflow-y: auto; padding: 20px; display: flex; flex-direction: column; gap: 16px">
                <FieldTextarea model={text} rows={4_u32} maxlength={MAX_POST_TEXT as u32} placeholder="What's happening here?" id="post-text" />
                {place}
                {attachments}
                <FieldCheckbox model={warning_on} label="Content warning" id="post-warning-on" />
                {warning}
                {picture_extras}
                {add}
                {note}
                {reach}
                {hint}
            </div>
        </div>
    }
}

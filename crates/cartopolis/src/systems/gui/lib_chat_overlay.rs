//! Chat over the map drawn by the component library (`lib_host`), beside
//! egui's (`chat_overlay`): the bubbles over the people who spoke, the chip
//! with the newest line, and the sheet that holds one chat at a time.
//!
//! On with `CARTO_UI_LIB=chat_overlay` (or `1`); the app's only (`comms`).
//! egui's three systems still decide everything — whether the overlay may
//! show at all (`chat_overlay::suppressed`), which line the chip carries
//! and how faded it is, where the sheet goes over the keys, the tabs, the
//! strip, the runs — and publish it in `ChatOverlay::lib_view`; they paint
//! nothing. A tap here goes back as `ChatOverlay::lib_chip` or
//! `ChatOverlay::lib_sheet` (`chat_overlay::SheetActs`, the same record
//! egui's drawing fills) and is done by the code egui's taps run. The field
//! is bridged to `ChatState::input` both ways (`lib_chats::Typed`).
//!
//! The bubbles go through `lib_pins::Anchored`, the one keyed list
//! everything anchored to the world goes through, under the other screens;
//! the chip and the sheet are one keyed mount beside them. The sheet is
//! built in parts — its frame, its head, its runs, its foot — each
//! following its own signal, so a new line does not rebuild the field
//! being typed into.
//!
//! What differs from egui's: the grab handle closes on a tap, not a drag;
//! a bubble's tail is a turned square; the field's hint follows Shout by
//! rebuilding the field; the field keeps the keyboard after a send only as
//! far as the library's focus does (egui asks for it again).

use std::cell::RefCell;
use std::rc::Rc;

use bevy::ecs::system::NonSendMarker;
use bevy::prelude::*;
// The library's, not Bevy's: an explicit import outranks the two globs.
use mithril::prelude::*;
use mithril::prelude::{Button, Event};

use crate::systems::chat::{ChatState, PersonAction};
use crate::systems::chat_overlay::{
    ChatOverlay, ChipAct, ChipView, RunView, SheetActs, SheetStrip, SheetView,
};
use crate::systems::discs::ChatKind;
use crate::systems::egui_manager::EguiRef;
use crate::systems::lib_chats::{disc, icon, Typed};
use crate::systems::lib_pins::{css, px_scale, Anchored, Spot};
use crate::systems::lib_settings::LibScreens;
use crate::systems::voice::VoiceMode;
use crate::systems::{icons, theme};

/// The bubbles' key in the library's host, and the chip's and sheet's.
const BUBBLES: &str = "chat_bubbles";
const KEY: &str = "chat_overlay";

/// The ring round someone talking (`chat_overlay::TALKING_RING`): what it
/// means, not chrome.
const TALKING_RING: egui::Color32 = egui::Color32::from_rgb(0x4F, 0xD1, 0x8F);

/// A bubble's look: everything but where it is and how strongly it shows.
#[derive(Clone, PartialEq)]
struct Bubble {
    speaker: String,
    tone: [u8; 3],
    bodies: Vec<String>,
    width: f32,
    tail: f32,
}

/// The sheet's head: the tabs, the keys beside them, who reads the chat,
/// and the strip.
#[derive(Clone, PartialEq)]
struct Head {
    tabs: Vec<(usize, String, ChatKind, bool, bool)>,
    subtitle: String,
    public: bool,
    voice_key: Option<bool>,
    call_key: Option<bool>,
    strip: Option<SheetStrip>,
}

/// The sheet's foot: the extras' row, Shout, whether there is anything to
/// send.
#[derive(Clone, PartialEq, Default)]
struct Foot {
    plus: bool,
    shout: bool,
    here: bool,
    ready: bool,
}

#[derive(Clone)]
struct Live {
    bubbles: Anchored<Bubble>,
    chip: Signal<Option<ChipView>>,
    chip_alpha: Signal<f32>,
    /// The sheet's place: left, top, width, height in px, and whether it is
    /// a phone's sheet (or a card).
    frame: Signal<Option<([f32; 4], bool)>>,
    head: Signal<Option<Head>>,
    runs: Signal<Vec<RunView>>,
    foot: Signal<Foot>,
    /// Shout, alone: the field follows it (its hint) and nothing else in
    /// the foot, or a first letter typed would rebuild the field.
    shout: Signal<bool>,
    /// Whose name's menu is open in the sheet.
    menu: Signal<Option<String>>,
    input: Typed,
    chip_act: Rc<RefCell<Option<ChipAct>>>,
    sheet_act: Rc<RefCell<SheetActs>>,
    /// The theme it was mounted in: another remounts it.
    dark: bool,
}

impl Live {
    /// A handler that fills the sheet's record for the next frame.
    fn act(&self, f: impl Fn(&mut SheetActs) + 'static) -> Handler {
        let acts = self.sheet_act.clone();
        Rc::new(move |_: &Event| f(&mut acts.borrow_mut()))
    }

    fn chip_tap(&self, a: ChipAct) -> Handler {
        let acts = self.chip_act.clone();
        Rc::new(move |_: &Event| *acts.borrow_mut() = Some(a.clone()))
    }
}

thread_local! {
    static MOUNTED: RefCell<Option<Live>> = const { RefCell::new(None) };
}

/// Write `value` into `s` only when it differs: a write re-runs every view
/// that reads it.
fn set<T: Clone + PartialEq + 'static>(s: &Signal<T>, value: T) {
    if s.get_untracked() != value {
        s.set(value);
    }
}

/// Mounts the overlay once, mirrors what egui's systems decided into it,
/// bridges the field, and hands the taps back. `PostUpdate`, before
/// `DriveUi`.
pub fn drive_lib_chat_overlay(
    _main: NonSendMarker,
    lib: Res<LibScreens>,
    mut overlay: ResMut<ChatOverlay>,
    mut chat: ResMut<ChatState>,
    egui: EguiRef,
) {
    if !lib.chat_overlay {
        return;
    }
    if !overlay.lib {
        overlay.lib = true;
    }
    let k = px_scale(egui.ui_scale());
    let dark = theme::p().ink == theme::DARK.ink;
    MOUNTED.with(|m| {
        let mut m = m.borrow_mut();
        if m.as_ref().is_some_and(|live| live.dark != dark) {
            *m = None;
            crate::systems::lib_host::clear_keyed(BUBBLES);
            crate::systems::lib_host::clear_keyed(KEY);
        }
        let live = m.get_or_insert_with(|| {
            let live = Live {
                bubbles: Anchored::new(),
                chip: signal(None),
                chip_alpha: signal(1.0),
                frame: signal(None),
                head: signal(None),
                runs: signal(Vec::new()),
                foot: signal(Foot::default()),
                shout: signal(false),
                menu: signal(None),
                input: Typed::new(&chat.input),
                chip_act: Rc::new(RefCell::new(None)),
                sheet_act: Rc::new(RefCell::new(SheetActs::default())),
                dark,
            };
            let touch = theme::touch_layout();
            crate::systems::lib_host::mount_keyed(
                BUBBLES,
                live.bubbles.view(-1, |_, b| bubble(b)),
                dark,
                touch,
            );
            crate::systems::lib_host::mount_keyed(KEY, view_of(&live), dark, touch);
            live
        });
        // The taps of the last frame, for egui's systems to do.
        if let Some(a) = live.chip_act.borrow_mut().take() {
            overlay.lib_chip = Some(a);
        }
        overlay.lib_sheet = std::mem::take(&mut *live.sheet_act.borrow_mut());
        // What was typed, into the world; a send's clearing, into the field.
        if let Some(typed) = live.input.typed(&chat.input) {
            chat.input = typed;
        }
        live.input.follow(&chat.input);

        let view = &overlay.lib_view;
        live.bubbles.sync(view.bubbles.iter().map(|b| {
            (
                b.key.clone(),
                Spot {
                    x: b.left * k,
                    y: b.bottom * k,
                    alpha: b.alpha,
                },
                Bubble {
                    speaker: b.speaker.clone(),
                    tone: b.tone,
                    bodies: b.bodies.clone(),
                    width: b.width,
                    tail: b.tail,
                },
            )
        }));
        match &view.chip {
            Some((chip, alpha)) => {
                let mut chip = chip.clone();
                chip.left *= k;
                chip.bottom *= k;
                chip.width *= k;
                set(&live.chip, Some(chip));
                // Faded in steps the eye cannot tell apart, not every frame.
                if (live.chip_alpha.get_untracked() - alpha).abs() >= 0.02
                    || (*alpha == 1.0 && live.chip_alpha.get_untracked() != 1.0)
                {
                    live.chip_alpha.set(*alpha);
                }
            }
            None => set(&live.chip, None),
        }
        match &view.sheet {
            Some(s) => {
                let [x, y, w, h] = s.rect;
                set(&live.frame, Some(([x * k, y * k, w * k, h * k], s.compact)));
                set(&live.head, Some(head_of(s)));
                set(&live.runs, s.runs.clone());
                set(
                    &live.foot,
                    Foot {
                        plus: s.plus,
                        shout: s.shout,
                        here: s.here,
                        ready: s.ready,
                    },
                );
                set(&live.shout, s.shout);
            }
            None => {
                set(&live.frame, None);
                set(&live.head, None);
                set(&live.menu, None);
            }
        }
    });
}

fn head_of(s: &SheetView) -> Head {
    Head {
        tabs: s.tabs.clone(),
        subtitle: s.subtitle.clone(),
        public: s.public,
        voice_key: s.voice_key,
        call_key: s.call_key,
        strip: s.strip.clone(),
    }
}

/// A colour of the content (a person's tone) as CSS.
fn tone(c: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])
}

// ─── The bubbles ────────────────────────────────────────────────────────────

/// A bubble (`chat_overlay::bubble`), its bottom-left corner on the item's
/// origin: the speaker's name in their colour over their lines, and a tail
/// pointing down at them. Nothing on it listens: a tap goes through it to
/// the world, where it means pick, walk or place.
fn bubble(b: &Bubble) -> View {
    let frame = format!(
        "position: absolute; left: 0; bottom: 0; width: {:.1}px; display: flex; flex-direction: column; gap: 1px; padding: 7px 11px; border-radius: 14px; background: var(--m-surface); border: 1px solid var(--m-border); box-shadow: var(--m-shadow)",
        b.width
    );
    let tail = format!(
        "position: absolute; left: {:.1}px; bottom: -5px; width: 10px; height: 10px; background: var(--m-surface); transform: rotate(45deg)",
        b.tail - 5.0
    );
    let name = format!(
        "color: {}; font-size: 12.5px; font-weight: 600",
        tone(b.tone)
    );
    let speaker = b.speaker.clone();
    let lines: Vec<View> = b
        .bodies
        .iter()
        .map(|body| {
            let body = body.clone();
            view! { <span style="color: var(--m-text-strong); font-size: 14.5px">{body}</span> }
        })
        .collect();
    view! {
        <div style={frame}>
            <div style={tail}></div>
            <span style={name}>{speaker}</span>
            {lines}
        </div>
    }
}

// ─── The chip and the sheet ──────────────────────────────────────────────────

fn view_of(l: &Live) -> View {
    let chip = {
        let l = l.clone();
        move || chip(&l)
    };
    let sheet = {
        let l = l.clone();
        move || -> View {
            // Built when the sheet opens and when it moves (the keys came
            // up); its parts follow their own signals inside.
            match l.frame.get() {
                Some((rect, compact)) => sheet(&l, rect, compact),
                None => View::Empty,
            }
        }
    };
    view! {
        {chip}
        {sheet}
    }
}

/// The chip (`chat_overlay::render_chat_chip`): the chat's disc, who said
/// the newest line and what, the unread count, and while talking nearby is
/// on its mute and leave. A tap on the line opens the sheet on its chat.
fn chip(l: &Live) -> View {
    let Some(c) = l.chip.get() else {
        return View::Empty;
    };
    let ground = if c.talking {
        "var(--m-success-soft)"
    } else {
        "var(--m-surface)"
    };
    let frame = {
        let alpha = l.chip_alpha.clone();
        let (left, bottom, width) = (c.left, c.bottom, c.width);
        move || {
            format!(
                "position: absolute; left: {left:.0}px; bottom: {bottom:.0}px; width: {width:.0}px; display: flex; align-items: center; gap: 6px; padding: 6px 8px; border-radius: 18px; background: {ground}; border: 1px solid var(--m-border); box-shadow: var(--m-shadow); opacity: {:.2}",
                alpha.get()
            )
        }
    };
    let who: View = match &c.sender {
        Some((name, t)) => {
            let style = format!(
                "flex: none; color: {}; font-size: 12.5px; white-space: nowrap",
                tone(*t)
            );
            let name = name.clone();
            view! { <span style={style}>{name}</span> }
        }
        None => View::Empty,
    };
    let body = c.body.clone();
    // Live only while it can be read: a chip on its way out leaves the map
    // its taps. It opens the sheet on the chat of the line in it.
    let open = c.live.then(|| l.chip_tap(ChipAct::Open(c.open.clone())));
    let unread: View = if c.unread > 0 {
        let n = if c.unread > 99 {
            "99+".to_string()
        } else {
            c.unread.to_string()
        };
        view! { <Badge tone=Tone::Accent>{n}</Badge> }
    } else {
        View::Empty
    };
    let voice: View = if c.talking {
        let (glyph, title) = if c.muted {
            (icons::MIC_OFF, "Unmute")
        } else {
            (icons::MIC, "Mute")
        };
        let mute = c.live.then(|| l.chip_tap(ChipAct::Mute));
        let leave = c.live.then(|| l.chip_tap(ChipAct::Leave));
        let variant = if c.muted {
            Variant::Secondary
        } else {
            Variant::Primary
        };
        view! {
            <Button variant={variant} size=Size::Sm icon={icon(glyph)} icon_only=true onclick={mute} title={title} />
            <Button size=Size::Sm icon={icon(icons::CLEAR)} icon_only=true onclick={leave} title="Stop talking nearby" />
        }
    } else {
        View::Empty
    };
    let (name, kind) = c.chat.clone();
    let opener = {
        let open = open.clone();
        move |e: &Event| {
            if let Some(open) = &open {
                open(e);
            }
        }
    };
    view! {
        <div style={frame} on:click={|_: &Event| {}}>
            <div title="Open the chat" on:click={opener} style="flex: 1; min-width: 0; display: flex; align-items: center; gap: 8px">
                {disc(&name, kind, 26)}
                {who}
                <span style="flex: 1; min-width: 0; color: var(--m-text); font-size: 14.5px; white-space: nowrap; text-overflow: ellipsis; overflow: hidden">{body}</span>
            </div>
            {unread}
            {voice}
        </div>
    }
}

/// The sheet (`chat_overlay::render_chat_sheet`) at `rect`, px: a phone's
/// across the bottom with its top corners round, a card's round all over.
fn sheet(l: &Live, rect: [f32; 4], compact: bool) -> View {
    let [x, y, w, h] = rect;
    let corners = if compact { "22px 22px 0 0" } else { "18px" };
    let frame = format!(
        "position: absolute; left: {x:.0}px; top: {y:.0}px; width: {w:.0}px; height: {h:.0}px; box-sizing: border-box; display: flex; flex-direction: column; gap: 6px; padding: 6px 10px 10px; border-radius: {corners}; background: var(--m-surface); border: 1px solid var(--m-border); box-shadow: var(--m-shadow-lg)"
    );
    // The grab handle: a tap closes (egui's is dragged down).
    let handle: View = if compact {
        let close = l.act(|a| a.close = true);
        view! {
            <div title="Close the chat" on:click={move |e: &Event| close(e)} style="flex: none; display: flex; justify-content: center; padding: 4px 0">
                <div style="width: 38px; height: 4px; border-radius: 2px; background: var(--m-border-strong)"></div>
            </div>
        }
    } else {
        View::Empty
    };
    let head = {
        let l = l.clone();
        move || match l.head.get() {
            Some(h) => head(&l, &h),
            None => View::Empty,
        }
    };
    // The lines open at the latest and follow the new ones (`@scroll-end`,
    // given again each time they move).
    let runs = {
        let l = l.clone();
        move || runs(&l)
    };
    let at_end = {
        let runs = l.runs.clone();
        move || {
            runs.get();
            true
        }
    };
    let scroller = mithril::view::el("div")
        .attr(
            "style",
            "flex: 1; min-height: 0; overflow-y: auto; display: flex; flex-direction: column",
        )
        .prop_bool("@scroll-end", at_end)
        .child(runs)
        .into_view();
    let extras = {
        let l = l.clone();
        move || extras(&l)
    };
    view! {
        <div style={frame} on:click={|_: &Event| {}}>
            {handle}
            {head}
            {scroller}
            {extras}
            {field_row(l)}
        </div>
    }
}

/// The tab row with the keys beside it and ✕, who reads the chat, and the
/// strip while voices or a call are on.
fn head(l: &Live, h: &Head) -> View {
    let tabs: Vec<View> = h
        .tabs
        .iter()
        .map(|(i, name, kind, unread, on)| {
            let i = *i;
            let pick = l.act(move |a| a.tab = Some(i));
            let style = if *on {
                "flex: none; display: flex; align-items: center; gap: 6px; height: 34px; padding: 0 8px; border-radius: 12px; background: var(--m-accent-soft); border: 1px solid var(--m-accent)"
            } else {
                "flex: none; display: flex; align-items: center; gap: 6px; height: 34px; padding: 0 8px; border-radius: 12px; background: var(--m-surface-raised); border: 1px solid transparent"
            };
            let ink = if *on {
                "color: var(--m-text-strong); font-size: 13px; white-space: nowrap"
            } else {
                "color: var(--m-text-dim); font-size: 13px; white-space: nowrap"
            };
            let dot: View = if *unread {
                view! { <span style="flex: none; width: 7px; height: 7px; border-radius: 4px; background: var(--m-accent)"></span> }
            } else {
                View::Empty
            };
            let (label, title) = (name.clone(), name.clone());
            view! {
                <div style={style} title={title} on:click={move |e: &Event| pick(e)}>
                    {disc(name, *kind, 20)}
                    <span style={ink}>{label}</span>
                    {dot}
                </div>
            }
        })
        .collect();
    let voice_key: View = match h.voice_key {
        Some(on) => {
            let (variant, title, mode) = if on {
                (Variant::Primary, "Stop talking nearby", VoiceMode::Off)
            } else {
                (
                    Variant::Secondary,
                    "Talk with the people near you (public)",
                    VoiceMode::Open,
                )
            };
            let tap = l.act(move |a| a.voice = Some(mode));
            view! { <Button variant={variant} icon={icon(icons::MIC)} icon_only=true onclick={Some(tap)} title={title} /> }
        }
        None => View::Empty,
    };
    let call_key: View = match h.call_key {
        Some(others) => {
            let (variant, title) = if others {
                (Variant::Primary, "Join the call")
            } else {
                (Variant::Secondary, "Call (end-to-end encrypted)")
            };
            let tap = l.act(|a| a.join_call = true);
            view! { <Button variant={variant} icon={icon(icons::CALL)} icon_only=true onclick={Some(tap)} title={title} /> }
        }
        None => View::Empty,
    };
    let close = l.act(|a| a.close = true);
    let who = if h.public {
        format!("font-size: 11.5px; color: {}", css(theme::PUBLIC))
    } else {
        "font-size: 11.5px; color: var(--m-text-dim)".to_string()
    };
    let subtitle = h.subtitle.clone();
    let strip: View = match &h.strip {
        Some(s) => strip(l, s),
        None => View::Empty,
    };
    view! {
        <div style="flex: none; display: flex; align-items: center; gap: 6px">
            <div style="flex: 1; min-width: 0; overflow-x: auto; display: flex; gap: 6px">{tabs}</div>
            {voice_key}
            {call_key}
            <Button variant=Variant::Ghost icon={icon(icons::CLEAR)} icon_only=true onclick={Some(close)} title="Close the chat" />
        </div>
        <span style={who}>{subtitle}</span>
        {strip}
    }
}

/// The voices nearby or the call, as a green card across the sheet.
fn strip(l: &Live, s: &SheetStrip) -> View {
    let label = |title: String, detail: String| -> View {
        view! {
            <div style="flex: 1; min-width: 0; display: flex; flex-direction: column">
                <span style="color: var(--m-text-strong); font-size: 14px; font-weight: 600">{title}</span>
                <span style="color: var(--m-text-dim); font-size: 11.5px">{detail}</span>
            </div>
        }
    };
    let body: View = match s {
        SheetStrip::Voice {
            people,
            talker,
            muted,
        } => {
            let faces: Vec<View> = people
                .iter()
                .map(|(name, speaking)| {
                    let ring = if *speaking {
                        format!(
                            "flex: none; padding: 2px; border-radius: 17px; border: 2px solid {}",
                            css(TALKING_RING)
                        )
                    } else {
                        "flex: none; padding: 2px; border-radius: 17px; border: 2px solid transparent"
                            .to_string()
                    };
                    view! { <div style={ring}>{disc(name, ChatKind::Private, 26)}</div> }
                })
                .collect();
            let muted = *muted;
            let (glyph, title, mode) = if muted {
                (icons::MIC_OFF, "Unmute", VoiceMode::Open)
            } else {
                (icons::MIC, "Mute", VoiceMode::PushToTalk)
            };
            let mute = l.act(move |a| a.voice = Some(mode));
            let leave = l.act(|a| a.voice = Some(VoiceMode::Off));
            view! {
                {faces}
                {label("Talking nearby".into(), talker.clone())}
                <Button size=Size::Sm icon={icon(glyph)} icon_only=true onclick={Some(mute)} title={title} />
                <Button variant=Variant::Danger size=Size::Sm icon={icon(icons::CLEAR)} icon_only=true onclick={Some(leave)} title="Stop talking nearby" />
            }
        }
        SheetStrip::Call {
            status,
            camera,
            muted,
        } => {
            let hang_up = l.act(|a| a.call = Some(crate::systems::calls::CallRequest::Leave));
            let unfold = l.act(|a| a.unfold_call = true);
            let camera: View = match camera {
                Some(on) => {
                    let (glyph, title) = if *on {
                        (icons::VIDEOCAM, "Camera off")
                    } else {
                        (icons::VIDEOCAM_OFF, "Camera on")
                    };
                    let tap = l.act(|a| a.toggle_camera = true);
                    view! { <Button size=Size::Sm icon={icon(glyph)} icon_only=true onclick={Some(tap)} title={title} /> }
                }
                None => View::Empty,
            };
            let (glyph, title) = if *muted {
                (icons::MIC_OFF, "Unmute")
            } else {
                (icons::MIC, "Mute")
            };
            let mic = l.act(|a| a.toggle_mic = true);
            view! {
                {label(status.clone(), format!("{} end-to-end encrypted", icons::LOCK))}
                <Button size=Size::Sm icon={icon(glyph)} icon_only=true onclick={Some(mic)} title={title} />
                {camera}
                <Button size=Size::Sm icon={icon(icons::OPEN_EXTERNAL)} icon_only=true onclick={Some(unfold)} title="Full screen" />
                <Button variant=Variant::Danger size=Size::Sm icon={icon(icons::CALL_END)} icon_only=true onclick={Some(hang_up)} title="Hang up" />
            }
        }
    };
    view! {
        <div style="flex: none; display: flex; align-items: center; gap: 6px; padding: 7px 10px; border-radius: 16px; background: var(--m-success-soft); border: 1px solid var(--m-success)">
            {body}
        </div>
    }
}

/// The open chat's runs, sitting on the field when they are few, as a
/// messenger's do; "Nothing said yet" when there are none.
fn runs(l: &Live) -> View {
    let runs = l.runs.get();
    if runs.is_empty() {
        return view! {
            <div style="margin: auto; color: var(--m-text-dim); font-size: 12.5px">"Nothing said yet"</div>
        };
    }
    let menu = l.menu.get();
    let items: Vec<View> = runs.iter().map(|r| run(l, r, menu.as_deref())).collect();
    view! {
        <div style="margin-top: auto; display: flex; flex-direction: column; gap: 6px; padding-bottom: 4px">{items}</div>
    }
}

/// One run (`chat_overlay::sheet_run`): someone else's under their disc
/// with their name once, in their colour, then their lines; yours on the
/// right, each in a quiet accent bubble. The name opens what you can do
/// about the person.
fn run(l: &Live, r: &RunView, menu: Option<&str>) -> View {
    if r.mine {
        let lines: Vec<View> = r
            .lines
            .iter()
            .enumerate()
            .map(|(i, line)| {
                let top_right = if i == 0 { 14 } else { 4 };
                let style = format!(
                    "max-width: 78%; padding: 5px 11px; border-radius: 14px {top_right}px 4px 14px; background: var(--m-accent-soft); color: var(--m-text-strong); font-size: 14.5px"
                );
                let line = line.clone();
                view! { <div style={style}>{line}</div> }
            })
            .collect();
        return view! {
            <div style="display: flex; flex-direction: column; align-items: flex-end; gap: 2px">{lines}</div>
        };
    }
    let open = menu.is_some_and(|m| m == r.sender);
    let toggle: Handler = {
        let (menu, sender) = (l.menu.clone(), r.sender.clone());
        Rc::new(move |_: &Event| {
            let next =
                (menu.get_untracked().as_deref() != Some(sender.as_str())).then(|| sender.clone());
            menu.set(next);
        })
    };
    let name = format!(
        "align-self: flex-start; color: {}; font-size: 12.5px; font-weight: 600",
        tone(r.tone)
    );
    let menu_view: View = if open {
        person_menu(l, &r.sender, r.last.clone())
    } else {
        View::Empty
    };
    let lines: Vec<View> = r
        .lines
        .iter()
        .map(|line| {
            let line = line.clone();
            view! { <span style="color: var(--m-text); font-size: 14.5px">{line}</span> }
        })
        .collect();
    let sender = r.sender.clone();
    view! {
        <div style="display: flex; align-items: flex-start; gap: 8px">
            {disc(&r.sender, ChatKind::Private, 26)}
            <div style="flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 1px">
                <span style={name} title="What you can do about this person" on:click={move |e: &Event| toggle(e)}>{sender}</span>
                {menu_view}
                {lines}
            </div>
        </div>
    }
}

/// What you can do about a speaker (`chat::person_menu`, as the sheet
/// offers it: no community, so no moderation).
fn person_menu(l: &Live, sender: &str, last: Option<String>) -> View {
    /// An entry, made a `PersonAction` when picked (which is not `Clone`).
    #[derive(Clone, Copy)]
    enum Pick {
        Message,
        Block,
        Report,
    }
    let entry = |pick: Pick, glyph: &'static str, label: &'static str, danger: bool| -> View {
        let tap = {
            let (sender, last, menu) = (sender.to_string(), last.clone(), l.menu.clone());
            let acts = l.sheet_act.clone();
            Rc::new(move |_: &Event| {
                let action = match pick {
                    Pick::Message => PersonAction::Message,
                    Pick::Block => PersonAction::Block,
                    Pick::Report => PersonAction::Report(last.clone()),
                };
                acts.borrow_mut().person = Some((sender.clone(), action));
                menu.set(None);
            }) as Handler
        };
        let tone = danger.then_some(Tone::Danger);
        view! { <ListItem icon={icon(glyph)} tone={tone} onclick={Some(tap)}>{label}</ListItem> }
    };
    view! {
        <div style="display: flex; flex-direction: column; max-width: 280px; padding: 6px; border: 1px solid var(--m-border); border-radius: var(--m-radius-lg); background: var(--m-surface-raised)">
            <List>
                {entry(Pick::Message, icons::CHAT, "Message privately", false)}
                {entry(Pick::Block, icons::STOP, "Block", true)}
                {entry(Pick::Report, icons::FLAG, "Report to the server…", false)}
            </List>
        </div>
    }
}

/// The row of extras under the field, when "+" opened it: Shout (in Here)
/// and the way to the full chat in Community.
fn extras(l: &Live) -> View {
    let f = l.foot.get();
    if !f.plus {
        return View::Empty;
    }
    let chip = |label: String, on: bool, tap: Handler| -> View {
        let style = if on {
            "flex: none; padding: 6px 10px; border-radius: 12px; background: var(--m-accent-soft); color: var(--m-text-strong); font-size: 12.5px"
        } else {
            "flex: none; padding: 6px 10px; border-radius: 12px; background: var(--m-surface-raised); color: var(--m-text); font-size: 12.5px"
        };
        view! { <div style={style} on:click={move |e: &Event| tap(e)}>{label}</div> }
    };
    let shout = if f.here {
        chip(
            format!("{} Shout", icons::FLAG),
            f.shout,
            l.act(|a| a.toggle_shout = true),
        )
    } else {
        View::Empty
    };
    let community = chip(
        format!("{} Pictures, points and plans", icons::ADD),
        false,
        l.act(|a| a.open_in_community = true),
    );
    view! {
        <div style="flex: none; display: flex; gap: 6px; flex-wrap: wrap">
            {shout}
            {community}
        </div>
    }
}

/// "+", the field and Send. The field is built once per Shout (its hint
/// says where the words go) and takes the keyboard as it is built.
fn field_row(l: &Live) -> View {
    let plus = {
        let l = l.clone();
        move || -> View {
            let f = l.foot.get();
            let variant = if f.plus || f.shout {
                Variant::Primary
            } else {
                Variant::Secondary
            };
            let tap = l.act(|a| a.toggle_plus = true);
            view! { <Button variant={variant} icon={icon(icons::ADD)} icon_only=true onclick={Some(tap)} title="More" /> }
        }
    };
    let send_now = l.act(|a| a.send = true);
    let keys: Handler = {
        let send = send_now.clone();
        Rc::new(move |e: &Event| {
            if e.key.as_deref() == Some("Enter") {
                send(e);
            }
        })
    };
    let field = {
        let l = l.clone();
        move || -> View {
            let hint = if l.shout.get() {
                "Shout to the street…"
            } else {
                "Message"
            };
            let model = l.input.model.clone();
            let keys = keys.clone();
            view! {
                <FieldText model={model} placeholder={hint} autocomplete="off" autofocus=true onkeydown={Some(keys)} id="chat-sheet-field" />
            }
        }
    };
    let send = {
        let l = l.clone();
        move || -> View {
            let off = !l.foot.get().ready;
            view! {
                <Button variant=Variant::Primary icon={icon(icons::SEND)} icon_only=true disabled={off} onclick={Some(send_now.clone())} title="Send" />
            }
        }
    };
    view! {
        <div style="flex: none; display: flex; align-items: center; gap: 8px">
            {plus}
            <div style="flex: 1; min-width: 0">{field}</div>
            {send}
        </div>
    }
}

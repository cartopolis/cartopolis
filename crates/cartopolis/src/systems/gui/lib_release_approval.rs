//! The release approval drawn by the component library (`lib_host`): the
//! release in front of the owner over everything, Claude's verdict at the
//! top, and **Approve with fingerprint** only when it is green. Also
//! Developer tools ▸ Midas ▸ Release key as a library view
//! ([`key_section_view`]), for the screen that draws that tab.
//!
//! `release_approval::drive_release_approval` fetches, checks, asks Claude
//! and follows the fingerprint sheet, and publishes the card as plain data
//! (`release_approval::lib_view`). A tap here goes back through
//! `release_approval::lib_act` and is done next frame by `release_approval::
//! act`.
//!
//! **Placement.** A veil over the whole window that listens (no tap reaches
//! the map or a panel), the card centred in it, at most `CARD_WIDTH` of
//! content or the screen less its margins. The list under the verdict
//! scrolls past 45 % of the window's height.
//!
//! Native only, as the module it draws (`not(wasm32)`): one px per ui
//! point, no DOM scale.

use std::cell::RefCell;
use std::rc::Rc;

use bevy::ecs::system::NonSendMarker;
use bevy::prelude::*;
// The library's, not Bevy's: an explicit import outranks the two globs.
use mithril::prelude::*;
use mithril::prelude::{Button, Event};

use crate::systems::lib_settings::LibScreens;
use crate::systems::release_approval::{
    self, ApprovalAct, ApprovalView, KeyAct, KeySectionView, VerdictView, CARD_WIDTH,
    KEY_MAKE_NOTE, KEY_TOOL_NOTE, NOT_GREEN,
};
use crate::systems::theme::tokens::alpha;
use crate::systems::ui_input::UiMetrics;
use crate::systems::{icons, theme};

/// The card's name in the library's host.
const KEY: &str = "release_approval";

/// Over every other library screen but the splash (1000).
const Z: i32 = 900;

/// egui's `overlay_frame` across: both paddings (14) and rims (1).
const FRAME_X: f32 = 2.0 * 14.0 + 2.0;

/// The window the card is centred in, px.
#[derive(Clone, Copy, PartialEq, Default)]
struct Placement {
    w: f32,
    h: f32,
}

impl Placement {
    /// The card's whole width: egui's content width and its frame, no
    /// wider than the screen less its margins.
    fn card_width(&self) -> f32 {
        (CARD_WIDTH + FRAME_X)
            .min(self.w - 2.0 * theme::OSD_MARGIN)
            .max(0.0)
    }
}

#[derive(Clone)]
struct Live {
    view: Signal<ApprovalView>,
    placement: Signal<Placement>,
    /// "What changes, and what this phone checked", open or shut: kept
    /// across the card's redraws, shut at first as egui's.
    details: Signal<bool>,
}

thread_local! {
    static MOUNTED: RefCell<Option<Live>> = const { RefCell::new(None) };
}

/// A tap that hands `a` to egui's system for next frame.
fn on(a: ApprovalAct) -> Option<Handler> {
    Some(Rc::new(move |_: &Event| release_approval::lib_act(a)) as Handler)
}

/// Mounts, bridges and unmounts the card. `PostUpdate`, before `DriveUi`,
/// as every library screen.
pub fn drive_lib_release_approval(
    _main: NonSendMarker,
    lib: Res<LibScreens>,
    metrics: Res<UiMetrics>,
) {
    if !lib.release_approval {
        return;
    }
    let size = metrics.viewport_pts();
    let placement = Placement {
        w: size.x,
        h: size.y,
    };
    let view = release_approval::lib_view();
    MOUNTED.with(|m| {
        let mut m = m.borrow_mut();
        let Some(view) = view else {
            if m.take().is_some() {
                crate::systems::lib_host::clear_keyed(KEY);
            }
            return;
        };
        match m.as_mut() {
            None => {
                let live = Live {
                    view: signal(view),
                    placement: signal(placement),
                    details: signal(false),
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
                        warn!(?path, error = %e, "lib release approval: could not write the HTML");
                    }
                }
                *m = Some(live);
            }
            Some(live) => {
                if live.view.peek(|now| *now != view) {
                    live.view.set(view);
                }
                if live.placement.get_untracked() != placement {
                    live.placement.set(placement);
                }
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

/// A line in egui's `line`: the small size, dimmed.
fn line(text: String) -> View {
    let style = format!(
        "font-size: {}px; color: var(--m-text-dim); overflow-wrap: anywhere",
        theme::SIZE_SUB
    );
    view! { <span style={style}>{text}</span> }
}

/// A heading in `theme::section`'s look: the small size, semibold, dimmed,
/// with room above it.
fn head(text: String) -> View {
    let style = format!(
        "padding-top: 12px; font-size: {}px; font-weight: 600; color: var(--m-text-dim)",
        theme::SIZE_SUB
    );
    view! { <span style={style}>{text}</span> }
}

/// A mark before a line: the icon face's check in the accent, or "!" in red.
fn mark(ok: bool) -> View {
    if ok {
        view! {
            <span style="flex: none; font-family: 'Material Symbols Rounded'; font-size: 16px; line-height: 1.2; color: var(--m-accent)">{icons::CHECK}</span>
        }
    } else {
        view! {
            <span style="flex: none; font-weight: 600; color: var(--m-danger)">"!"</span>
        }
    }
}

/// The veil and the card in it.
fn view_of(l: &Live) -> View {
    let veil = format!(
        "position: absolute; left: 0; top: 0; right: 0; bottom: 0; z-index: {Z}; \
         display: flex; align-items: center; justify-content: center; background: {}",
        alpha(theme::DARK.ink, 86.0)
    );
    let frame = {
        let placement = l.placement.clone();
        move || {
            format!(
                "box-sizing: border-box; width: {:.0}px; padding: 12px 14px; \
                 display: flex; flex-direction: column; gap: 6px; \
                 background: var(--m-surface); border: 1px solid var(--m-border); \
                 border-radius: var(--m-radius-lg); box-shadow: var(--m-shadow-lg)",
                placement.get().card_width()
            )
        }
    };
    let body = {
        let l = l.clone();
        move || -> View { card_body(&l.view.get(), &l.placement.get(), &l.details) }
    };
    // The veil listens (a tap that does nothing), as egui's blocker under the
    // card: nothing beneath it answers while a release waits.
    view! {
        <div style={veil} on:click={|_: &Event| {}}>
            <div style={frame} on:click={|_: &Event| {}}>{body}</div>
        </div>
    }
}

/// The title, who asked, the verdict, the list, and the two buttons.
fn card_body(v: &ApprovalView, p: &Placement, details: &Signal<bool>) -> View {
    let title_style = format!(
        "font-size: {}px; color: var(--m-text-strong); line-height: 1.25",
        theme::SIZE_HEADING
    );
    let title = v.title.clone();
    let asked = line(v.asked.clone());
    let verdict = verdict_view(&v.verdict);
    let findings: Vec<View> = v
        .findings
        .iter()
        .map(|(at, why)| {
            let at = at.clone();
            let why = line(why.clone());
            view! {
                <div style="display: flex; flex-direction: column; gap: 2px">
                    <div style="display: flex; align-items: baseline; gap: 6px">
                        {mark(false)}
                        <span style="min-width: 0; font-family: monospace; overflow-wrap: anywhere; color: var(--m-text)">{at}</span>
                    </div>
                    {why}
                </div>
            }
        })
        .collect();
    let findings = View::Fragment(findings);
    let scroll = format!(
        "max-height: {:.0}px; overflow-y: auto; display: flex; flex-direction: column; gap: 6px",
        p.h * 0.45
    );
    let inside = details_view(v);
    let approve = if v.signing {
        "Waiting for your fingerprint…"
    } else {
        "Approve with fingerprint"
    };
    let not_green: View = if v.not_green {
        line(NOT_GREEN.to_string())
    } else {
        View::Empty
    };
    view! {
        <span style={title_style}>{title}</span>
        {asked}
        <div style="height: 6px"></div>
        {verdict}
        <div style={scroll}>
            {findings}
            <Collapse model={details.clone()} title="What changes, and what this phone checked">
                <div style="display: flex; flex-direction: column; gap: 4px; padding-top: 4px">{inside}</div>
            </Collapse>
        </div>
        <div style="height: 8px"></div>
        <div style="display: flex; flex-direction: column">
            <Button variant=Variant::Primary disabled={!v.ready} onclick={on(ApprovalAct::Approve)}>{approve}</Button>
        </div>
        {not_green}
        <div style="display: flex">
            <Button onclick={on(ApprovalAct::Decline)}>"Not now"</Button>
        </div>
    }
}

/// Claude's verdict: a spinner while it reads, then its title in green's
/// accent or red, and what it said.
fn verdict_view(v: &VerdictView) -> View {
    match v {
        VerdictView::Pending(what) => {
            let style = format!(
                "font-size: {}px; color: var(--m-text-strong)",
                theme::SIZE_BODY
            );
            let what = line(what.clone());
            view! {
                <div style="display: flex; align-items: center; gap: 8px">
                    <Spinner size=Size::Sm />
                    <span style={style}>"Claude is checking this release"</span>
                </div>
                {what}
            }
        }
        VerdictView::Said { title, green, body } => {
            let style = format!(
                "font-size: {}px; color: {}",
                theme::SIZE_BODY,
                if *green {
                    "var(--m-accent)"
                } else {
                    "var(--m-danger)"
                }
            );
            let title = *title;
            let body = body.clone();
            view! {
                <span style={style}>{title}</span>
                <span style="color: var(--m-text); white-space: pre-wrap">{body}</span>
            }
        }
    }
}

/// Inside the fold: what the release lists, then what this phone checked.
fn details_view(v: &ApprovalView) -> View {
    let changes: View = match &v.release {
        Some((changes, code)) => {
            let heading = head(format!("What changes · {}", changes.len()));
            let none: View = if changes.is_empty() {
                line("The release lists no changes.".to_string())
            } else {
                View::Empty
            };
            let rows: Vec<View> = changes
                .iter()
                .map(|c| {
                    let c = format!("• {c}");
                    view! { <span style="color: var(--m-text)">{c}</span> }
                })
                .collect();
            let rows = View::Fragment(rows);
            let code = line(format!("Version code {code}"));
            view! {
                {heading}
                {none}
                {rows}
                {code}
            }
        }
        None => View::Empty,
    };
    let checking: View = match &v.checking {
        Some(what) => {
            let what = line(what.clone());
            view! {
                <div style="display: flex; align-items: center; gap: 8px">
                    <Spinner size=Size::Sm />
                    {what}
                </div>
            }
        }
        None => View::Empty,
    };
    let checks: Vec<View> = v
        .checks
        .iter()
        .map(|(ok, text)| {
            let text = text.clone();
            view! {
                <div style="display: flex; align-items: baseline; gap: 6px">
                    {mark(*ok)}
                    <span style="min-width: 0; color: var(--m-text); overflow-wrap: anywhere">{text}</span>
                </div>
            }
        })
        .collect();
    let checks = View::Fragment(checks);
    let heading = head("Checks".to_string());
    view! {
        {changes}
        {heading}
        {checking}
        {checks}
    }
}

// ─── Developer tools ▸ Midas ▸ Release key ───────────────────────────────────

/// Release key, as egui's `release_approval::key_section`: make the key, or
/// where it is kept, the key shortened and Copy; then the Claude API key's
/// note, a password field and Save key. `View::Empty` where releases are not
/// approved (not the owner's Android phone).
///
/// Self-contained: its buttons call `release_approval::key_act`, the code
/// egui's call, from the tap itself (no ECS needed), and redraw it from
/// `release_approval::key_view`. For the screen that draws the Midas tab to
/// put under its own sections (`lib_dev_panel`'s Midas tab).
pub fn key_section_view() -> View {
    let Some(first) = release_approval::key_view() else {
        return View::Empty;
    };
    let shown: Signal<KeySectionView> = signal(first);
    let draft: Signal<String> = signal(String::new());
    let body = {
        let (shown, draft) = (shown.clone(), draft.clone());
        move || -> View { key_body(&shown.get(), &shown, &draft) }
    };
    view! {
        <div style="display: flex; flex-direction: column; gap: 8px">
            {head("Release key".to_string())}
            {body}
        </div>
    }
}

/// Do `a`, then show the section as it is now.
fn key_tap(shown: &Signal<KeySectionView>, a: impl Fn() -> KeyAct + 'static) -> Option<Handler> {
    let shown = shown.clone();
    Some(Rc::new(move |_: &Event| {
        release_approval::key_act(a());
        if let Some(now) = release_approval::key_view() {
            shown.set(now);
        }
    }) as Handler)
}

fn key_body(v: &KeySectionView, shown: &Signal<KeySectionView>, draft: &Signal<String>) -> View {
    let key: View = match &v.public {
        None => {
            let note = line(KEY_MAKE_NOTE.to_string());
            view! {
                {note}
                <div style="display: flex">
                    <Button variant=Variant::Primary onclick={key_tap(shown, || KeyAct::Make)}>"Make release key"</Button>
                </div>
            }
        }
        Some(public) => {
            let kept = line(v.kept.clone());
            let short = v.short.clone();
            let public = public.clone();
            let copy = Some(Rc::new(move |_: &Event| {
                crate::utils::clipboard::set_text(&public);
            }) as Handler);
            let tool = line(KEY_TOOL_NOTE.to_string());
            view! {
                {kept}
                <span style="font-family: monospace; color: var(--m-text)">{short}</span>
                <div style="display: flex">
                    <Button size=Size::Sm onclick={copy}>"Copy public key"</Button>
                </div>
                {tool}
            }
        }
    };
    let api = line(v.api_note.to_string());
    let save = {
        let draft = draft.clone();
        key_tap(shown, move || {
            let typed = draft.get_untracked();
            draft.set(String::new());
            KeyAct::SaveApiKey(typed)
        })
    };
    let message: View = match &v.message {
        Some(m) => {
            let m = m.clone();
            view! { <Alert tone=Tone::Danger>{m}</Alert> }
        }
        None => View::Empty,
    };
    view! {
        {key}
        {api}
        <div style="display: flex; align-items: center; gap: 8px">
            <div style="flex: 1; min-width: 0">
                <FieldText model={draft.clone()} r#type=TextType::Password placeholder="sk-ant-…" autocomplete="off" id="release-api-key" />
            </div>
            <Button size=Size::Sm onclick={save}>"Save key"</Button>
        </div>
        {message}
    }
}

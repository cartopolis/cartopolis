//! The About panel drawn by the component library (`lib_host`): what this
//! build is, the safety contact, the controls, and the third-party notices —
//! **a licence obligation, not a courtesy** (`settings`, `# About`). The
//! notices are `LICENSE-THIRD-PARTY.md`, parsed by `settings::parse_notices`;
//! nothing here types a credit.
//!
//! Owns `RightPanel::About`: mounts the panel when it opens, unmounts it when
//! it closes, closes it from its ✕. A tap on a notice's link is handed back
//! and opened by `data_layers::open_url`, the door every link in the app goes
//! through.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use bevy::ecs::system::NonSendMarker;
use bevy::prelude::*;
// The library's, not Bevy's: an explicit import outranks the two globs.
use mithril::prelude::*;
use mithril::prelude::{Button, Event};

use crate::resources::RightPanel;
use crate::systems::settings::{self, Notice};
use crate::systems::ui_input::UiMetrics;
use crate::systems::{icons, theme};

/// The panel's name in the library's host.
const KEY: &str = "about";

/// The panel's width beside the map, px: egui's.
const WIDTH: f32 = 320.0;

/// What the view asks the system to do.
#[derive(Clone, Default)]
struct Acts {
    close: Rc<Cell<bool>>,
    open_url: Rc<RefCell<Option<String>>>,
}

thread_local! {
    static LIVE: RefCell<Option<Acts>> = const { RefCell::new(None) };
}

/// Mounts and unmounts the About panel as `RightPanel::About` comes and
/// goes, and does its taps. `PostUpdate`, before the library applies the
/// frame's operations, as every library screen.
pub fn drive_lib_about(
    _main: NonSendMarker,
    mut right_panel: ResMut<RightPanel>,
    metrics: Res<UiMetrics>,
) {
    let open = right_panel.is(RightPanel::About);
    LIVE.with(|live| {
        let mut live = live.borrow_mut();
        match (open, live.as_ref()) {
            (false, None) => {}
            (false, Some(_)) => {
                *live = None;
                crate::systems::lib_host::clear_keyed(KEY);
            }
            (true, None) => {
                // Parsed per opening: ~130 lines of static text, and the
                // view holds what it needs once built.
                let notices = settings::notices();
                let acts = Acts::default();
                crate::systems::lib_host::mount_keyed(
                    KEY,
                    panel(&acts, &notices, metrics.compact()),
                    theme::p().ink == theme::DARK.ink,
                    theme::touch_layout(),
                );
                *live = Some(acts);
            }
            (true, Some(acts)) => {
                if let Some(url) = acts.open_url.borrow_mut().take() {
                    crate::systems::data_layers::open_url(&url);
                }
                if acts.close.take() {
                    *right_panel = RightPanel::None;
                }
            }
        }
    });
}

/// A glyph of the app's icon font.
fn icon(glyph: &'static str) -> View {
    view! {
        <span style="font-family: 'Material Symbols Rounded'; font-size: 18px; line-height: 1">{glyph}</span>
    }
}

/// A handler from a closure.
fn on(f: impl Fn() + 'static) -> Option<Handler> {
    Some(Rc::new(move |_: &Event| f()) as Handler)
}

/// A line of text at `size` px in `colour` (a CSS value).
fn line(text: impl Into<String>, size: f32, colour: &str) -> View {
    let text: String = text.into();
    let style = format!("margin: 0; font-size: {size}px; color: {colour}; line-height: 1.4");
    view! { <p style={style}>{text}</p> }
}

/// The panel: a header with its ✕, then everything, scrolling.
fn panel(acts: &Acts, notices: &[Notice], compact: bool) -> View {
    let frame = if compact {
        "position: absolute; left: 0; right: 0; top: 0; bottom: 0; display: flex; flex-direction: column; background: var(--m-bg)".to_owned()
    } else {
        format!(
            "position: absolute; right: 0; top: 0; bottom: 0; width: {WIDTH}px; display: flex; flex-direction: column; background: var(--m-bg); border-left: 1px solid var(--m-border)"
        )
    };
    let close = {
        let close = acts.close.clone();
        on(move || close.set(true))
    };
    let version = format!("Version {}", env!("CARGO_PKG_VERSION"));
    let credits: Vec<View> = notices.iter().map(|n| notice(n, acts)).collect();
    // Safety sits above the credits rather than below them: it is the one
    // block somebody may be looking for in a hurry, and the licence list is
    // ~130 lines of scrolling.
    view! {
        <div style={frame} data-reserves-map="right">
            <div style="display: flex; align-items: center; gap: 12px; min-height: 56px; padding: 0 12px 0 20px; border-bottom: 1px solid var(--m-border)">
                <span style="color: var(--m-accent)">{icon(icons::ABOUT)}</span>
                <h1 style="flex: 1; margin: 0; font-size: 19px; font-weight: 600; color: var(--m-text-strong)">"About"</h1>
                <CloseButton onclick={close} />
            </div>
            <div style="flex: 1; min-height: 0; overflow-y: auto; padding: 20px 20px 28px; display: flex; flex-direction: column; gap: 20px">
                <div style="display: flex; flex-direction: column; gap: 4px">
                    <p style="margin: 0; font-size: 19px; font-weight: 700; color: var(--m-text-strong)">"Cartopolis"</p>
                    {line(version, theme::SIZE_BODY, "var(--m-text)")}
                    {line(settings::build_line(), theme::SIZE_CHIP, "var(--m-text-dim)")}
                    {line("Cartopolis's own source is MIT OR Apache-2.0. Everything below is used under its own licence.", theme::SIZE_CHIP, "var(--m-text-dim)")}
                </div>
                <Section title="Safety">
                    {line(settings::SAFETY_TEXT, theme::SIZE_SUB, "var(--m-text-dim)")}
                    <p style="margin: 0; font-size: var(--m-font-size-sm)"><span style="color: var(--m-text-dim)">"Contact: "</span><span style="color: var(--m-accent-text)">{settings::SAFETY_CONTACT}</span></p>
                </Section>
                <Section title="Controls">{controls()}</Section>
                <div style="display: flex; flex-direction: column">{credits}</div>
            </div>
        </div>
    }
}

/// Every binding, grouped by the mode it works in (`settings::CONTROLS`).
fn controls() -> Vec<View> {
    settings::CONTROLS
        .iter()
        .map(|(mode, rows)| {
            let rows: Vec<View> = rows
                .iter()
                .map(|(keys, what)| {
                    let (keys, what) = (*keys, *what);
                    let k = format!(
                        "flex: none; width: 86px; font-size: {}px; color: var(--m-accent-text)",
                        theme::SIZE_SUB
                    );
                    let w = format!(
                        "flex: 1; min-width: 0; font-size: {}px; color: var(--m-text)",
                        theme::SIZE_SUB
                    );
                    view! {
                        <div style="display: flex; gap: 8px; align-items: baseline">
                            <span style={k}>{keys}</span>
                            <span style={w}>{what}</span>
                        </div>
                    }
                })
                .collect();
            let mode = *mode;
            view! {
                <div style="display: flex; flex-direction: column; gap: 4px">
                    <p style="margin: 0; color: var(--m-text-dim); font-size: var(--m-font-size-sm); font-style: italic">{mode}</p>
                    {rows}
                </div>
            }
        })
        .collect()
}

/// One credit: the name, what it is for, the notice the licence requires,
/// the licence and its scope, and its link.
fn notice(n: &Notice, acts: &Acts) -> View {
    let name = n.name.clone();
    let role =
        (!n.role.is_empty()).then(|| line(n.role.clone(), theme::SIZE_CHIP, "var(--m-text)"));
    // The `notice` line is the one the licence actually requires, so it is
    // the one shown at body weight.
    let required =
        (!n.notice.is_empty()).then(|| line(n.notice.clone(), theme::SIZE_SUB, "var(--m-text)"));
    let licence: View = if n.license.is_empty() {
        View::Empty
    } else {
        let s = format!(
            "font-size: {}px; color: var(--m-accent-text)",
            theme::SIZE_CHIP
        );
        let t = n.license.clone();
        view! { <span style={s}>{t}</span> }
    };
    let scope: View = if n.scope.is_empty() {
        View::Empty
    } else {
        let s = format!(
            "font-size: {}px; color: var(--m-text-dim)",
            theme::SIZE_CHIP
        );
        let t = format!("· {}", n.scope);
        view! { <span style={s}>{t}</span> }
    };
    let link: View = if n.url.is_empty() {
        View::Empty
    } else {
        // Disabled where nothing can open it, rather than dead — the same
        // contract the POI card's `Website ↗` follows.
        let can_open = crate::systems::data_layers::open_url_supported();
        let click = {
            let (open, url) = (acts.open_url.clone(), n.url.clone());
            on(move || *open.borrow_mut() = Some(url.clone()))
        };
        let url = n.url.clone();
        view! {
            <div><Button variant=Variant::Ghost size=Size::Sm onclick={click} disabled={!can_open}>{url}</Button></div>
        }
    };
    view! {
        <div style="display: flex; flex-direction: column; gap: 2px; padding: 8px 0; border-bottom: 1px solid var(--m-border)">
            <p style="margin: 0; font-weight: 600; color: var(--m-text-strong)">{name}</p>
            {role.unwrap_or(View::Empty)}
            {required.unwrap_or(View::Empty)}
            <div style="display: flex; flex-wrap: wrap; gap: 6px">{licence}{scope}</div>
            {link}
        </div>
    }
}

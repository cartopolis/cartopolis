//! Your face's sheet drawn by the component library (`lib_host`): who you
//! are, your profile and your link (or Sign in…), your avatar, places and
//! models, Settings, Admin, About, Exit, the developer rows, and Sign out.
//!
//! `ui::drive_main_menu` keeps every decision — whether the sheet is open,
//! where it stands (on the World | Community switch on a phone, under your
//! face on a desktop), what it offers, whether Sign out is armed, a tap
//! outside it — and publishes it in `MainMenu::lib_view`. A tap here is a
//! `ui::SheetAct`, handed back in `MainMenu::lib_acts` and done by it next
//! frame.
//!
//! **Placement.** At `ui::SheetSpot`, in ui points times the library's px
//! per point. Your face is its initial on its colour: the library has no
//! image node.

use std::cell::RefCell;
use std::rc::Rc;

use bevy::ecs::system::NonSendMarker;
use bevy::prelude::*;
// The library's, not Bevy's: an explicit import outranks the two globs.
use mithril::prelude::*;
use mithril::prelude::{Button, Event};

use crate::resources::RightPanel;
use crate::systems::discs::{disc_look, ChatKind};
use crate::systems::lib_place_card::{cell, icon};
use crate::systems::lib_settings::LibScreens;
use crate::systems::theme::tokens::hex;
use crate::systems::ui::{MainMenu, MenuView, SheetAct, SheetSpot};
use crate::systems::ui_input::UiMetrics;
use crate::systems::{icons, theme};

/// The sheet's name in the library's host.
const KEY: &str = "main_menu";

/// Your face's diameter at the head of the sheet: egui's.
const FACE: f32 = 42.0;

type Acts = Rc<RefCell<Vec<SheetAct>>>;

#[derive(Clone)]
struct Live {
    dark: bool,
    /// The library's px per egui point: 1 on the Bevy backend, the UI scale
    /// as DOM (`lib_place_card::Placement::over_the_dock` says why).
    k: f32,
    view: Signal<MenuView>,
    acts: Acts,
}

impl Live {
    fn act(&self, a: SheetAct) -> Option<Handler> {
        let acts = self.acts.clone();
        Some(Rc::new(move |_: &Event| acts.borrow_mut().push(a)) as Handler)
    }

    fn tap(&self, a: SheetAct) -> impl Fn(&Event) + 'static {
        let acts = self.acts.clone();
        move |_: &Event| acts.borrow_mut().push(a)
    }
}

thread_local! {
    static MOUNTED: RefCell<Option<Live>> = const { RefCell::new(None) };
}

/// Mounts, bridges and unmounts the sheet. `PostUpdate`, before `DriveUi`,
/// as every library screen.
pub fn drive_lib_main_menu(
    _main: NonSendMarker,
    lib: Res<LibScreens>,
    mut menu: ResMut<MainMenu>,
    metrics: Res<UiMetrics>,
) {
    if !lib.main_menu {
        return;
    }
    let dark = theme::p().ink == theme::DARK.ink;
    let k = if cfg!(all(feature = "ui-dom", target_arch = "wasm32")) {
        metrics.ui_scale()
    } else {
        1.0
    };
    MOUNTED.with(|m| {
        let mut m = m.borrow_mut();
        // The taps of the frame, for `ui::drive_main_menu` next frame.
        if let Some(live) = m.as_ref() {
            let acts: Vec<SheetAct> = std::mem::take(&mut *live.acts.borrow_mut());
            if !acts.is_empty() {
                menu.lib_acts.extend(acts);
            }
        }
        // Closed, or a new theme or scale: down (to mount again).
        let stale = m
            .as_ref()
            .is_some_and(|l| menu.lib_view.is_none() || l.dark != dark || l.k != k);
        if stale {
            *m = None;
            crate::systems::lib_host::clear_keyed(KEY);
        }
        let Some(view) = menu.lib_view.clone() else {
            return;
        };
        match m.as_ref() {
            None => {
                let live = Live {
                    dark,
                    k,
                    view: signal(view),
                    acts: Rc::new(RefCell::new(Vec::new())),
                };
                crate::systems::lib_host::mount_keyed(
                    KEY,
                    view_of(&live),
                    dark,
                    theme::touch_layout(),
                );
                write_html(&live);
                *m = Some(live);
            }
            Some(live) => {
                if live.view.peek(|now| *now != view) {
                    live.view.set(view);
                    write_html(live);
                }
            }
        }
    });
}

/// `CARTO_UI_LIB_HTML=<path>`: the sheet as HTML, for the browser beside a
/// capture.
fn write_html(l: &Live) {
    if let Some(path) = std::env::var_os("CARTO_UI_LIB_HTML") {
        if let Err(e) = std::fs::write(&path, html(l)) {
            warn!(?path, error = %e, "lib main menu: could not write the HTML");
        }
    }
}

/// The sheet's HTML from the library's string renderer.
fn html(l: &Live) -> String {
    use mithril::view::{mount, StringRenderer};
    let r = Rc::new(RefCell::new(StringRenderer::new()));
    let root = r.borrow().root();
    let mounted = mount(view_of(l), &r, &root, None);
    let out = r.borrow().html();
    drop(mounted);
    out
}

/// The card's frame at its spot: the library's card surface.
fn frame_style(spot: SheetSpot, k: f32) -> String {
    let (at, width, max_height) = match spot {
        SheetSpot::Bottom {
            left,
            bottom,
            width,
            max_height,
        } => (
            format!("left: {:.0}px; bottom: {:.0}px", left * k, bottom * k),
            width,
            max_height,
        ),
        SheetSpot::Hang {
            right,
            top,
            width,
            max_height,
        } => (
            format!("right: {:.0}px; top: {:.0}px", right * k, top * k),
            width,
            max_height,
        ),
    };
    format!(
        "position: absolute; {at}; width: {:.0}px; max-height: {:.0}px; box-sizing: border-box; \
         display: flex; flex-direction: column; background: var(--m-surface); \
         border: 1px solid var(--m-border); border-radius: var(--m-radius-lg); \
         box-shadow: var(--m-shadow-lg); overflow: hidden",
        width * k,
        max_height * k
    )
}

/// The sheet: its frame at its spot, and what is in it, both following the
/// view `ui::drive_main_menu` publishes.
fn view_of(l: &Live) -> View {
    let frame = {
        let (v, k) = (l.view.clone(), l.k);
        move || frame_style(v.get().spot, k)
    };
    let content = {
        let l = l.clone();
        move || -> View { body(&l, &l.view.get()) }
    };
    // The card's own node listens (a click that does nothing), so the
    // pointer over its padding is the library's too, and a tap there is not
    // a tap outside it.
    view! {
        <div style={frame} on:click={|_: &Event| {}}>
            <div style="min-height: 0; overflow-y: auto; display: flex; flex-direction: column; gap: 12px; padding: 16px 16px 14px">
                {content}
            </div>
        </div>
    }
}

/// Your face, `FACE` across: your initial on your colour signed in, a grey
/// person signed out, and the orange dot while something waits
/// (`dock::paint_face`). The colour is the person's own (`discs`), content
/// rather than chrome.
fn face(me: Option<&str>, waiting: bool) -> View {
    let (bg, ink, text, size, glyph) = match me {
        Some(name) => {
            let look = disc_look(name, ChatKind::Private);
            (
                hex(look.tone.into()),
                "var(--m-on-accent)",
                look.text,
                FACE * look.scale,
                look.icon,
            )
        }
        None => (
            "var(--m-hover)".to_string(),
            "var(--m-text-dim)",
            icons::APPEARANCE.to_string(),
            FACE * 0.55,
            true,
        ),
    };
    let family = if glyph {
        "font-family: 'Material Symbols Rounded'; "
    } else {
        ""
    };
    let disc = format!(
        "position: absolute; left: 0; top: 0; width: {FACE}px; height: {FACE}px; display: flex; \
         align-items: center; justify-content: center; border-radius: var(--m-radius-full); \
         background: {bg}; color: {ink}; {family}font-size: {size:.0}px; font-weight: 600; line-height: 1"
    );
    let dot: View = if waiting {
        let r = FACE / 2.0;
        let d = 12.0;
        let style = format!(
            "position: absolute; left: {:.1}px; top: {:.1}px; width: {d}px; height: {d}px; \
             box-sizing: border-box; border-radius: var(--m-radius-full); \
             border: 1.5px solid var(--m-surface); background: {}",
            r + r * 0.72 - d / 2.0,
            r - r * 0.72 - d / 2.0,
            hex(theme::BRAND),
        );
        view! { <span style={style}></span> }
    } else {
        View::Empty
    };
    let frame = format!("position: relative; flex: none; width: {FACE}px; height: {FACE}px");
    view! {
        <div style={frame}>
            <span style={disc}>{text}</span>
            {dot}
        </div>
    }
}

/// A quiet line, as `theme::note`.
fn note(text: String) -> View {
    view! {
        <p style="margin: 0; padding-inline: 4px; color: var(--m-text-dim); font-size: var(--m-font-size-sm)">{text}</p>
    }
}

/// A row that opens a panel: its icon and its name, a chevron.
fn panel_row(l: &Live, panel: RightPanel) -> View {
    view! {
        <ListItem icon={icon(panel.icon())} chevron=true onclick={l.act(SheetAct::Panel(panel))}>{panel.label()}</ListItem>
    }
}

/// Everything in the sheet, in order.
fn body(l: &Live, v: &MenuView) -> View {
    let comms = cfg!(feature = "comms");

    // Who you are, and the close.
    let sub: View = if v.sub.is_empty() {
        View::Empty
    } else {
        let sub = v.sub.clone();
        view! {
            <span style="color: var(--m-text-dim); font-size: var(--m-font-size-sm); white-space: nowrap; overflow: hidden; text-overflow: ellipsis">{sub}</span>
        }
    };
    let title = v.title.clone();
    let head = view! {
        <div style="display: flex; align-items: center; gap: 12px">
            {face(v.face.as_deref(), v.waiting)}
            <div style="flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 2px">
                <h1 style="margin: 0; font-size: 19px; font-weight: 600; color: var(--m-text-strong); white-space: nowrap; overflow: hidden; text-overflow: ellipsis">{title}</h1>
                {sub}
            </div>
            <CloseButton onclick={l.act(SheetAct::Close)} />
        </div>
    };

    // Your profile and your link, or the way in. In the web build who you
    // are is the page's around the World tab.
    let account: View = if !comms {
        View::Empty
    } else if v.signed_in {
        view! {
            <div style="display: flex; gap: 8px">
                {cell(view! { <Button icon={icon(icons::APPEARANCE)} onclick={l.act(SheetAct::Profile)}>"Your profile"</Button> })}
                {cell(view! { <Button icon={icon(icons::SHARE)} onclick={l.act(SheetAct::ShareLink)}>"Share link"</Button> })}
            </div>
        }
    } else {
        view! {
            <div style="display: flex; flex-direction: column">
                <Button variant=Variant::Primary onclick={l.act(SheetAct::SignIn)}>"Sign in…"</Button>
            </div>
        }
    };
    // Why a remembered session did not come up, beside the way to try again.
    let error: View = match &v.error {
        Some(e) => note(e.clone()),
        None => View::Empty,
    };

    // Yours: the body others see, the places you keep, the models you own.
    let mut yours: Vec<View> = Vec::new();
    if v.signed_in {
        yours.push(panel_row(l, RightPanel::Appearance));
    }
    yours.push(panel_row(l, RightPanel::Places));
    if v.signed_in {
        yours.push(panel_row(l, RightPanel::Models));
    }

    // The app's: Settings, Admin for an account that may run something,
    // About, and Exit where an app can be ended.
    let mut app: Vec<View> = vec![panel_row(l, RightPanel::Settings)];
    if let Some(waiting) = v.admin {
        let sub: View = if waiting > 0 {
            format!("{waiting} waiting").into_view()
        } else {
            View::Empty
        };
        let end: View = if waiting > 0 {
            let n = if waiting > 99 {
                "99+".to_string()
            } else {
                waiting.to_string()
            };
            view! { <Badge tone=Tone::Accent>{n}</Badge> }
        } else {
            View::Empty
        };
        app.push(view! {
            <ListItem icon={icon(RightPanel::Admin.icon())} sub={sub} end={end} tone={Some(Tone::Warning)} chevron=true onclick={l.act(SheetAct::Panel(RightPanel::Admin))}>{RightPanel::Admin.label()}</ListItem>
        });
    }
    app.push(panel_row(l, RightPanel::About));
    #[cfg(not(target_arch = "wasm32"))]
    app.push(view! {
        <ListItem icon={icon(icons::CLEAR)} onclick={l.act(SheetAct::Exit)}>"Exit"</ListItem>
    });

    // Developer tools and Share diagnostics, with Settings ▸ General ▸
    // Developer on.
    let dev: View = if v.dev {
        let diag: View = match &v.diagnostics {
            Some(d) => d.clone().into_view(),
            None => View::Empty,
        };
        view! {
            <List>
                <ListItem icon={icon(icons::TUNE)} chevron=true onclick={l.act(SheetAct::DevTools)}>"Developer tools"</ListItem>
                <ListItem icon={icon(icons::COPY)} sub={diag} onclick={l.act(SheetAct::Diagnostics)}>"Share diagnostics…"</ListItem>
            </List>
        }
    } else {
        View::Empty
    };

    // Last, in red, and armed before it acts.
    let sign_out: View = if v.signed_in && comms {
        let label = if v.armed {
            "Tap again to sign out"
        } else {
            "Sign out…"
        };
        let bg = if v.armed {
            "var(--m-hover)"
        } else {
            "transparent"
        };
        let style = format!(
            "display: flex; align-items: center; justify-content: center; width: 100%; height: 40px; \
             box-sizing: border-box; margin: 4px 0 0; padding: 0; border: 0; border-radius: var(--m-radius); \
             background: {bg}; color: var(--m-danger); font: inherit; font-size: {:.0}px; font-weight: 600; cursor: pointer",
            theme::SIZE_VALUE
        );
        view! {
            <button title={label} style={style} on:click={l.tap(SheetAct::SignOut)}>{label}</button>
        }
    } else {
        View::Empty
    };

    view! {
        {head}
        {account}
        {error}
        <List>{yours}</List>
        <List>{app}</List>
        {dev}
        {sign_out}
    }
}

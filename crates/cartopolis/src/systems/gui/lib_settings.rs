//! Settings drawn by the component library through its Bevy renderer
//! (`mithril_bevy`), beside egui — phase 3b of
//! `docs/mockups/component-library`: the first real screen on the library,
//! with the app's own state behind it.
//!
//! On with `CARTO_UI_LIB=settings` (feature `ui-probe`): egui's
//! `render_settings_panel` steps aside (`SettingsState::lib`) and this
//! module owns `RightPanel::Settings` — mounting the panel when it opens,
//! unmounting it when it closes, closing it from its ✕. The list is the
//! mockup's (`home-and-profile/v1`, D); of the pages, General is ported
//! with its preferences bound both ways, the others say so and wait.
//!
//! **State.** The view reads signals; the app's state is Bevy resources. The
//! bridge is a frame-by-frame diff in [`drive_lib_settings`]: a signal
//! whose value moved since the last frame writes the preference, a
//! preference that moved writes the signal, and `AppPrefs` is written only
//! when something differs (its `is_changed()` gates re-run work).
//!
//! **Not yet:** the panel overlays the map rather than taking width from
//! it, so pins and the dock do not move aside as they do for egui's; the
//! library's tokens dress it (cartopolis' own come with phase 4).
//!
//! `CARTO_UI_LIB_PAGE=general` opens on that page (for a capture);
//! `CARTO_UI_LIB_HTML=<path>` writes the panel's HTML once, for the browser
//! beside it.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use bevy::ecs::system::NonSendMarker;
use bevy::prelude::*;
// The library's, not Bevy's: an explicit import outranks the two globs.
use mithril::prelude::*;
use mithril::prelude::{Button, Event};

use crate::components::FreeCamera;
use crate::resources::{
    AppPrefs, AuthIdentity, MotionPref, OnlineSession, RightPanel, UiTheme, Units,
};
use crate::systems::egui_manager::EguiMut;
use crate::systems::settings::{SettingsState, SettingsTab};
use crate::systems::{icons, theme};

/// Which screens the library draws instead of egui.
#[derive(Resource, Default, Clone, Copy, Debug)]
pub struct LibScreens {
    pub settings: bool,
}

impl LibScreens {
    /// `CARTO_UI_LIB=settings` (or `1`, every screen there is).
    pub fn from_env() -> Self {
        let v = std::env::var("CARTO_UI_LIB").unwrap_or_default();
        let all = v.trim() == "1";
        LibScreens {
            settings: all || v.split(',').any(|s| s.trim() == "settings"),
        }
    }
}

/// The mounted panel's signals and what the app last saw of its state.
struct Live {
    page: Signal<Option<SettingsTab>>,
    /// The username when signed in.
    who: Signal<Option<String>>,
    signed_in: Signal<bool>,
    ui_theme: Signal<UiTheme>,
    units: Signal<Units>,
    keep_screen_on: Signal<bool>,
    motion: Signal<MotionPref>,
    developer_menu: Signal<bool>,
    voice_guidance: Signal<bool>,
    /// The preferences as last synced, to tell a signal's move from a
    /// resource's.
    last: AppPrefs,
    close: Rc<Cell<bool>>,
    to_about: Rc<Cell<bool>>,
}

impl Live {
    fn new(prefs: &AppPrefs, who: Option<String>, signed_in: bool) -> Self {
        let page = std::env::var("CARTO_UI_LIB_PAGE").ok().and_then(|p| {
            SettingsTab::ALL.into_iter().find(|t| {
                t.label().eq_ignore_ascii_case(&p) || format!("{t:?}").eq_ignore_ascii_case(&p)
            })
        });
        Live {
            page: signal(page),
            who: signal(who),
            signed_in: signal(signed_in),
            ui_theme: signal(prefs.ui_theme),
            units: signal(prefs.units),
            keep_screen_on: signal(prefs.keep_screen_on),
            motion: signal(prefs.motion),
            developer_menu: signal(prefs.developer_menu),
            voice_guidance: signal(prefs.voice_guidance),
            last: *prefs,
            close: Rc::new(Cell::new(false)),
            to_about: Rc::new(Cell::new(false)),
        }
    }

    /// Signals that moved write the preferences; preferences that moved
    /// write the signals. Answers the preferences to keep.
    fn sync(&mut self, prefs: &AppPrefs) -> AppPrefs {
        let mut edited = *prefs;
        macro_rules! field {
            ($f:ident) => {
                let s = self.$f.get();
                if s != self.last.$f {
                    edited.$f = s;
                } else if prefs.$f != self.last.$f {
                    self.$f.set(prefs.$f);
                }
            };
        }
        field!(ui_theme);
        field!(units);
        field!(keep_screen_on);
        field!(motion);
        field!(developer_menu);
        field!(voice_guidance);
        self.last = edited;
        edited
    }
}

thread_local! {
    static LIVE: RefCell<Option<Live>> = const { RefCell::new(None) };
}

/// Mounts, syncs and unmounts the panel as `RightPanel::Settings` comes and
/// goes. `PostUpdate`, before the library applies the frame's operations.
#[allow(clippy::too_many_arguments)]
pub fn drive_lib_settings(
    _main: NonSendMarker,
    lib: Res<LibScreens>,
    mut right_panel: ResMut<RightPanel>,
    mut state: ResMut<SettingsState>,
    mut prefs: ResMut<AppPrefs>,
    identity: Res<AuthIdentity>,
    online: Option<Res<OnlineSession>>,
    mut egui: EguiMut,
    pointer: Res<mithril_bevy::UiPointer>,
    cameras: Query<Entity, With<FreeCamera>>,
    mut fonts: ResMut<Assets<Font>>,
    mut ui_scale: ResMut<UiScale>,
    mut commands: Commands,
    mut ready: Local<bool>,
) {
    if !lib.settings {
        return;
    }
    if !state.lib {
        state.lib = true;
    }
    if !*ready {
        // The camera the panel draws to and the faces it draws with, once.
        let Ok(camera) = cameras.single() else {
            return;
        };
        ui_scale.0 = egui.ui_scale();
        let body = fonts.add(Font::from_bytes(theme::BODY_TTF.to_vec()));
        commands.insert_resource(mithril_bevy::UiCamera(camera));
        commands.insert_resource(mithril_bevy::FontMap {
            regular: body.clone(),
            semibold: fonts.add(Font::from_bytes(theme::BRAND_TTF.to_vec())),
            mono: body,
            icons: Some(fonts.add(Font::from_bytes(icons::FONT.to_vec()))),
        });
        *ready = true;
    }
    let open = right_panel.is(RightPanel::Settings);
    // A world click under the panel is the panel's.
    egui.set_overlay_owns_pointer(open && pointer.owns_pointer());
    let signed_in = identity.username.is_some() && online.is_some();
    let who = signed_in.then(|| identity.username.clone()).flatten();
    LIVE.with(|live| {
        let mut live = live.borrow_mut();
        match (open, live.as_mut()) {
            (false, None) => {}
            (false, Some(_)) => {
                *live = None;
                mithril_bevy::with_host(|host| host.clear());
                state.page = None;
            }
            (true, None) => {
                let l = Live::new(&prefs, who, signed_in);
                let compact = egui.compact();
                let p = theme::p();
                mithril_bevy::with_host(|host| {
                    host.env.dark = p.ink == theme::DARK.ink;
                    host.env.coarse_pointer = theme::touch_layout();
                    host.mount(panel(&l, compact));
                });
                if let Some(path) = std::env::var_os("CARTO_UI_LIB_HTML") {
                    if let Err(e) = std::fs::write(&path, html(&l, compact)) {
                        warn!(?path, error = %e, "lib settings: could not write the HTML");
                    }
                }
                *live = Some(l);
            }
            (true, Some(l)) => {
                let edited = l.sync(&prefs);
                if edited != *prefs {
                    *prefs = edited;
                }
                if l.signed_in.get() != signed_in {
                    l.signed_in.set(signed_in);
                }
                if l.who.get() != who {
                    l.who.set(who);
                }
                state.page = l.page.get();
                if l.close.take() {
                    *right_panel = RightPanel::None;
                }
                if l.to_about.take() {
                    *right_panel = RightPanel::About;
                }
            }
        }
    });
}

/// The panel's HTML from the library's string renderer, for the browser
/// beside the capture.
fn html(l: &Live, compact: bool) -> String {
    use mithril::view::{mount, StringRenderer};
    let r = Rc::new(RefCell::new(StringRenderer::new()));
    let root = r.borrow().root();
    let mounted = mount(panel(l, compact), &r, &root, None);
    let out = r.borrow().html();
    drop(mounted);
    out
}

// ─── The view ────────────────────────────────────────────────────────────────

/// A glyph of the app's icon font.
fn icon(glyph: &'static str) -> View {
    view! {
        <span style="font-family: 'Material Symbols Rounded'; font-size: 18px; line-height: 1">{glyph}</span>
    }
}

/// A line of explanation under a control (`theme::note`).
fn note(text: &'static str) -> View {
    view! {
        <p style="margin: 0; color: var(--m-text-dim); font-size: var(--m-font-size-sm); font-style: italic; line-height: 1.4">{text}</p>
    }
}

/// The panel: a header with the way back or out, then the page, scrolling.
fn panel(l: &Live, compact: bool) -> View {
    let frame = if compact {
        "position: absolute; left: 0; right: 0; top: 0; bottom: 0; display: flex; flex-direction: column; background: var(--m-bg)"
    } else {
        "position: absolute; right: 0; top: 0; bottom: 0; width: 340px; display: flex; flex-direction: column; background: var(--m-bg); border-left: 1px solid var(--m-border)"
    };
    let header = {
        let page = l.page.clone();
        let close = l.close.clone();
        move || -> View {
            let back_to_list = {
                let page = page.clone();
                Some(Rc::new(move |_: &Event| page.set(None)) as Handler)
            };
            let close_panel = {
                let close = close.clone();
                Some(Rc::new(move |_: &Event| close.set(true)) as Handler)
            };
            match page.get() {
                None => view! {
                    <div style="display: flex; align-items: center; gap: 10px; min-height: 52px; padding: 0 12px 0 16px; border-bottom: 1px solid var(--m-border)">
                        <span style="color: var(--m-accent)">{icon(icons::SETTINGS)}</span>
                        <h1 style="flex: 1; margin: 0; font-size: 22px; font-weight: 600; color: var(--m-text-strong)">"Settings"</h1>
                        <CloseButton onclick={close_panel} />
                    </div>
                },
                Some(p) => view! {
                    <div style="display: flex; align-items: center; gap: 4px; min-height: 52px; padding: 0 12px 0 8px; border-bottom: 1px solid var(--m-border)">
                        <Button variant=Variant::Ghost size=Size::Sm onclick={back_to_list} aria_label="Back">{icon(icons::BACK)}</Button>
                        <h1 style="flex: 1; margin: 0; font-size: 20px; font-weight: 600; color: var(--m-text-strong)">{p.label()}</h1>
                    </div>
                },
            }
        }
    };
    let body = {
        let page = l.page.clone();
        let who = l.who.clone();
        let signed_in = l.signed_in.clone();
        let to_about = l.to_about.clone();
        let (ui_theme, units, keep_screen_on, motion, developer_menu, voice_guidance) = (
            l.ui_theme.clone(),
            l.units.clone(),
            l.keep_screen_on.clone(),
            l.motion.clone(),
            l.developer_menu.clone(),
            l.voice_guidance.clone(),
        );
        move || -> View {
            match page.get() {
                None => home(&page, &who, &signed_in, &to_about),
                Some(SettingsTab::General) => general(
                    &ui_theme,
                    &units,
                    &keep_screen_on,
                    &motion,
                    &developer_menu,
                    &voice_guidance,
                ),
                Some(_) => note(
                    "This page is not in the library's edition yet; without CARTO_UI_LIB it is egui's.",
                ),
            }
        }
    };
    view! {
        <div style={frame}>
            {header}
            <div style="flex: 1; min-height: 0; overflow-y: auto; padding: 16px; display: flex; flex-direction: column; gap: 16px">
                {body}
            </div>
        </div>
    }
}

/// The list of pages (`home-and-profile/v1`, D): the account's, then the
/// app's, About last.
fn home(
    page: &Signal<Option<SettingsTab>>,
    who: &Signal<Option<String>>,
    signed_in: &Signal<bool>,
    to_about: &Rc<Cell<bool>>,
) -> View {
    let signed_in = signed_in.get();
    let who_line = who.get().map_or_else(
        || "Not signed in".to_owned(),
        |u| format!("Signed in as {u}"),
    );
    let row = |tab: SettingsTab| -> View {
        let page = page.clone();
        let sub: View = match tab {
            SettingsTab::Account => who_line.clone().into_view(),
            _ => tab.hint().map_or(View::Empty, IntoView::into_view),
        };
        let open = Some(Rc::new(move |_: &Event| page.set(Some(tab))) as Handler);
        view! {
            <ListItem icon={icon(tab.icon())} sub={sub} chevron=true onclick={open}>{tab.label()}</ListItem>
        }
    };
    let rows = |account: bool| -> Vec<View> {
        SettingsTab::ALL
            .into_iter()
            .filter(|t| t.about_the_account() == account)
            // Tokens are made and revoked over a session.
            .filter(|t| *t != SettingsTab::Tokens || signed_in)
            .map(row)
            .collect()
    };
    let about = {
        let to_about = to_about.clone();
        Some(Rc::new(move |_: &Event| to_about.set(true)) as Handler)
    };
    let (account_rows, app_rows) = (rows(true), rows(false));
    view! {
        <Section title="Account"><List>{account_rows}</List></Section>
        <Section title="App">
            <List>
                {app_rows}
                <ListItem icon={icon(icons::ABOUT)} chevron=true onclick={about}>"About"</ListItem>
            </List>
        </Section>
    }
}

/// The General page's preferences (`settings::general_tab`), the receiver,
/// storage and editor-access sections still to come.
fn general(
    ui_theme: &Signal<UiTheme>,
    units: &Signal<Units>,
    keep_screen_on: &Signal<bool>,
    motion: &Signal<MotionPref>,
    developer_menu: &Signal<bool>,
    voice_guidance: &Signal<bool>,
) -> View {
    let themes: Vec<Opt<UiTheme>> = UiTheme::ALL
        .iter()
        .map(|t| Opt::new(*t, t.label()))
        .collect();
    let unit_opts: Vec<Opt<Units>> = Units::ALL.iter().map(|u| Opt::new(*u, u.label())).collect();
    let motions: Vec<Opt<MotionPref>> = MotionPref::ALL
        .iter()
        .map(|m| Opt::new(*m, m.label()))
        .collect();
    view! {
        <Section title="Appearance">
            <FieldSegmented model={ui_theme.clone()} options={themes} id="g-theme" />
            {note("Auto is light while the sun is up where the map is. The sky's own light is under World.")}
        </Section>
        <Section title="Units">
            <FieldSegmented model={units.clone()} options={unit_opts} id="g-units" />
            {note("Distances are shown in kilometres until the route and driver readouts read this.")}
        </Section>
        <Section title="Screen">
            <FieldCheckbox model={keep_screen_on.clone()} label="Keep the screen on while Cartopolis is in front" id="g-screen" />
        </Section>
        <Section title="Motion">
            <FieldSegmented model={motion.clone()} options={motions} id="g-motion" />
            {note("Reduced keeps buttons answering under the finger, and makes cards and pages appear in place.")}
        </Section>
        <Section title="Developer">
            <FieldCheckbox model={developer_menu.clone()} label="Show Developer tools and Share diagnostics in the menu" id="g-dev" />
        </Section>
        <Section title="Navigation">
            <FieldCheckbox model={voice_guidance.clone()} label="Chime at turns while navigating" id="g-chime" />
        </Section>
        <Section title="Receiver, storage, editor access">
            {note("Still egui's: these sections come with the next pages.")}
        </Section>
    }
}

//! Settings drawn by the component library through its Bevy renderer
//! (`mithril_bevy`), beside egui — phase 3b of
//! `docs/mockups/component-library`: the first real screen on the library,
//! with the app's own state behind it.
//!
//! On with `CARTO_UI_LIB=settings` (feature `ui-probe`): egui's
//! `render_settings_panel` steps aside (`SettingsState::lib`), keeping an
//! empty panel of the same width so the dock and the map move over, and
//! this module owns `RightPanel::Settings` — mounting the panel when it
//! opens, unmounting it when it closes, closing it from its ✕. The list is
//! the mockup's (`home-and-profile/v1`, D); General, Graphics and World are
//! ported with their state bound both ways, the others say so and wait.
//!
//! **State.** The view reads signals; the app's state is Bevy resources. The
//! bridge is a frame-by-frame diff in [`drive_lib_settings`]: a signal
//! whose value moved since the last frame writes the resource, a resource
//! that moved writes the signal, and a resource is assigned only when
//! something differs (`AppPrefs` and `DevPanelState` are read through
//! `is_changed()` gates that re-run work).
//!
//! `CARTO_UI_LIB_PAGE=general|graphics|world` opens on that page (for a
//! capture); `CARTO_UI_LIB_HTML=<path>` writes the panel's HTML once, for
//! the browser beside it.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use bevy::ecs::system::{NonSendMarker, SystemParam};
use bevy::prelude::*;
// The library's, not Bevy's: an explicit import outranks the two globs.
use mithril::prelude::*;
use mithril::prelude::{Button, Event};

use crate::components::FreeCamera;
use crate::resources::{
    AppPrefs, AuthIdentity, DevPanelState, MotionPref, OnlineSession, QualityPreset, RightPanel,
    SkyPreference, SunMode, UiTheme, Units,
};
use crate::systems::egui_manager::EguiMut;
use crate::systems::render_scale::RenderScale;
use crate::systems::settings::{clock_hhmm, SettingsState, SettingsTab};
use crate::systems::sky_dome::SkyMode;
use crate::systems::weather::WeatherState;
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

/// The app's state the pages read and write, in one parameter.
#[derive(SystemParam)]
pub struct AppState<'w> {
    prefs: ResMut<'w, AppPrefs>,
    dev: ResMut<'w, DevPanelState>,
    weather: ResMut<'w, WeatherState>,
    identity: Res<'w, AuthIdentity>,
    online: Option<Res<'w, OnlineSession>>,
    sky: Option<Res<'w, SkyMode>>,
}

/// An `Option<bool>` preference as the library shows it: the resolved
/// checkbox, and whether it is on Automatic. Toggling the box leaves
/// Automatic; the "Automatic" button returns to it.
#[derive(Clone)]
struct Tri {
    value: Signal<bool>,
    auto: Signal<bool>,
}

impl Tri {
    fn new(v: Option<bool>, default: bool) -> Self {
        Tri {
            value: signal(v.unwrap_or(default)),
            auto: signal(v.is_none()),
        }
    }

    fn get(&self) -> Option<bool> {
        if self.auto.get() {
            None
        } else {
            Some(self.value.get())
        }
    }

    fn set(&self, v: Option<bool>, default: bool) {
        self.auto.set(v.is_none());
        self.value.set(v.unwrap_or(default));
    }

    /// Under Automatic the box shows the device's default, which can move.
    fn follow(&self, default: bool) {
        if self.auto.get() && self.value.get() != default {
            self.value.set(default);
        }
    }
}

/// `Option<f32>` the same way, for the render scale's slider.
#[derive(Clone)]
struct Scale {
    value: Signal<f64>,
    auto: Signal<bool>,
}

impl Scale {
    fn new(v: Option<f32>, default: f32) -> Self {
        Scale {
            value: signal(f64::from(v.unwrap_or(default))),
            auto: signal(v.is_none()),
        }
    }

    fn get(&self) -> Option<f32> {
        if self.auto.get() {
            None
        } else {
            Some(RenderScale::new(self.value.get() as f32).0)
        }
    }

    fn set(&self, v: Option<f32>, default: f32) {
        self.auto.set(v.is_none());
        self.value.set(f64::from(v.unwrap_or(default)));
    }
}

/// The mounted panel's signals and what the app last saw of its state.
struct Live {
    page: Signal<Option<SettingsTab>>,
    /// The username when signed in.
    who: Signal<Option<String>>,
    signed_in: Signal<bool>,
    // General
    ui_theme: Signal<UiTheme>,
    units: Signal<Units>,
    keep_screen_on: Signal<bool>,
    motion: Signal<MotionPref>,
    developer_menu: Signal<bool>,
    voice_guidance: Signal<bool>,
    // Graphics
    quality: Signal<QualityPreset>,
    render_scale: Scale,
    frame_rate: Signal<Option<u32>>,
    bloom: Tri,
    anti_aliasing: Tri,
    water_effects: Signal<bool>,
    sky: Signal<SkyPreference>,
    sky_now: Signal<Option<SkyMode>>,
    shadows: Tri,
    sky_lighting: Tri,
    light_shafts: Tri,
    // World
    sun_mode: Signal<SunMode>,
    time_of_day: Signal<f64>,
    moon_age_days: Signal<f64>,
    auto_cycle: Signal<bool>,
    cycle_seconds: Signal<Option<f64>>,
    weather_live: Signal<bool>,
    clouds_enabled: Signal<bool>,
    cloud_shadows: Signal<bool>,
    cloud_cover: Signal<f64>,
    precipitation: Signal<f64>,
    fog_density: Signal<f64>,
    /// The state as last synced, to tell a signal's move from a resource's.
    last_prefs: AppPrefs,
    last_dev: DevPanelState,
    last_weather_live: bool,
    close: Rc<Cell<bool>>,
    to_about: Rc<Cell<bool>>,
}

/// The device's defaults behind every Automatic, as the egui page computes
/// them (`settings::graphics_tab`).
struct Defaults {
    bloom: bool,
    anti_aliasing: bool,
    shadows: bool,
    sky_lighting: bool,
    light_shafts: bool,
    render_scale: f32,
}

impl Defaults {
    fn now(prefs: &AppPrefs) -> Self {
        let budget = crate::quality::budget();
        let caps = crate::quality::capabilities();
        let shafts_possible = caps.is_some_and(|caps| !caps.software_renderer);
        let shafts_affordable = crate::quality::affordable_at(crate::quality::main_pass_pixels());
        Defaults {
            bloom: budget.bloom,
            anti_aliasing: budget.msaa != bevy::render::view::Msaa::Off,
            shadows: budget.shadows,
            sky_lighting: budget.sky_ibl,
            light_shafts: shafts_possible && prefs.shadows_on() && shafts_affordable,
            render_scale: budget.render_scale,
        }
    }
}

impl Live {
    fn new(
        prefs: &AppPrefs,
        dev: &DevPanelState,
        weather_live: bool,
        sky_now: Option<SkyMode>,
        who: Option<String>,
        signed_in: bool,
    ) -> Self {
        let page = std::env::var("CARTO_UI_LIB_PAGE").ok().and_then(|p| {
            SettingsTab::ALL.into_iter().find(|t| {
                t.label().eq_ignore_ascii_case(&p) || format!("{t:?}").eq_ignore_ascii_case(&p)
            })
        });
        let d = Defaults::now(prefs);
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
            quality: signal(prefs.quality),
            render_scale: Scale::new(prefs.render_scale, d.render_scale),
            frame_rate: signal(prefs.frame_rate),
            bloom: Tri::new(prefs.bloom, d.bloom),
            anti_aliasing: Tri::new(prefs.anti_aliasing, d.anti_aliasing),
            water_effects: signal(prefs.water_effects),
            sky: signal(prefs.sky),
            sky_now: signal(sky_now),
            shadows: Tri::new(prefs.shadows, d.shadows),
            sky_lighting: Tri::new(prefs.sky_lighting, d.sky_lighting),
            light_shafts: Tri::new(prefs.light_shafts, d.light_shafts),
            sun_mode: signal(dev.sun_mode),
            time_of_day: signal(f64::from(dev.time_of_day)),
            moon_age_days: signal(f64::from(dev.moon_age_days)),
            auto_cycle: signal(dev.auto_cycle),
            cycle_seconds: signal(Some(f64::from(dev.cycle_seconds))),
            weather_live: signal(weather_live),
            clouds_enabled: signal(dev.clouds_enabled),
            cloud_shadows: signal(dev.cloud_shadows),
            cloud_cover: signal(f64::from(dev.cloud_cover)),
            precipitation: signal(f64::from(dev.precipitation)),
            fog_density: signal(f64::from(dev.fog_density)),
            last_prefs: *prefs,
            last_dev: dev.clone(),
            last_weather_live: weather_live,
            close: Rc::new(Cell::new(false)),
            to_about: Rc::new(Cell::new(false)),
        }
    }

    /// Signals that moved write the state; state that moved writes the
    /// signals. Answers what to keep.
    fn sync(
        &mut self,
        prefs: &AppPrefs,
        dev: &DevPanelState,
        weather_live: bool,
    ) -> (AppPrefs, DevPanelState, bool) {
        let d = Defaults::now(prefs);
        let mut edited = *prefs;
        // A plain signal against a plain field.
        macro_rules! plain {
            ($sig:expr, $edited:expr, $now:expr, $last:expr, $conv:expr) => {{
                let s = $sig.get();
                if s != $last {
                    $edited = $conv(s);
                } else if $now != $last {
                    $sig.set($now);
                }
            }};
        }
        macro_rules! pref {
            ($f:ident) => {
                plain!(self.$f, edited.$f, prefs.$f, self.last_prefs.$f, |v| v)
            };
        }
        macro_rules! tri {
            ($f:ident, $default:expr) => {{
                self.$f.follow($default);
                let s = self.$f.get();
                if s != self.last_prefs.$f {
                    edited.$f = s;
                } else if prefs.$f != self.last_prefs.$f {
                    self.$f.set(prefs.$f, $default);
                }
            }};
        }
        pref!(ui_theme);
        pref!(units);
        pref!(keep_screen_on);
        pref!(motion);
        pref!(developer_menu);
        pref!(voice_guidance);
        pref!(quality);
        pref!(frame_rate);
        pref!(water_effects);
        pref!(sky);
        tri!(bloom, d.bloom);
        tri!(anti_aliasing, d.anti_aliasing);
        tri!(shadows, d.shadows);
        tri!(sky_lighting, d.sky_lighting);
        tri!(light_shafts, d.light_shafts);
        {
            let s = self.render_scale.get();
            if s != self.last_prefs.render_scale {
                edited.render_scale = s;
            } else if prefs.render_scale != self.last_prefs.render_scale {
                self.render_scale.set(prefs.render_scale, d.render_scale);
            }
        }
        self.last_prefs = edited;

        let mut world = dev.clone();
        macro_rules! devf {
            ($f:ident) => {
                plain!(self.$f, world.$f, dev.$f, self.last_dev.$f, |v| v)
            };
            ($f:ident, f32) => {{
                let s = self.$f.get() as f32;
                if s != self.last_dev.$f {
                    world.$f = s;
                } else if dev.$f != self.last_dev.$f {
                    self.$f.set(f64::from(dev.$f));
                }
            }};
        }
        devf!(sun_mode);
        devf!(auto_cycle);
        devf!(clouds_enabled);
        devf!(cloud_shadows);
        devf!(time_of_day, f32);
        devf!(moon_age_days, f32);
        devf!(cloud_cover, f32);
        devf!(precipitation, f32);
        devf!(fog_density, f32);
        {
            let s = self
                .cycle_seconds
                .get()
                .map_or(dev.cycle_seconds, |v| v as f32);
            if s != self.last_dev.cycle_seconds {
                world.cycle_seconds = s.clamp(1.0, 120.0);
            } else if dev.cycle_seconds != self.last_dev.cycle_seconds {
                self.cycle_seconds.set(Some(f64::from(dev.cycle_seconds)));
            }
        }
        self.last_dev = world.clone();

        let mut live = weather_live;
        {
            let s = self.weather_live.get();
            if s != self.last_weather_live {
                live = s;
            } else if weather_live != self.last_weather_live {
                self.weather_live.set(weather_live);
            }
        }
        self.last_weather_live = live;
        (edited, world, live)
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
    mut app: AppState,
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
    let signed_in = app.identity.username.is_some() && app.online.is_some();
    let who = signed_in.then(|| app.identity.username.clone()).flatten();
    let sky_now = app.sky.as_deref().copied();
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
                let l = Live::new(
                    &app.prefs,
                    &app.dev,
                    app.weather.live,
                    sky_now,
                    who,
                    signed_in,
                );
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
                let (prefs, dev, weather_live) = l.sync(&app.prefs, &app.dev, app.weather.live);
                if prefs != *app.prefs {
                    *app.prefs = prefs;
                }
                if dev != *app.dev {
                    *app.dev = dev;
                }
                if weather_live != app.weather.live {
                    app.weather.live = weather_live;
                }
                if l.signed_in.get() != signed_in {
                    l.signed_in.set(signed_in);
                }
                if l.who.get() != who {
                    l.who.set(who);
                }
                if l.sky_now.get() != sky_now {
                    l.sky_now.set(sky_now);
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
fn note(text: impl Into<String>) -> View {
    let text: String = text.into();
    view! {
        <p style="margin: 0; color: var(--m-text-dim); font-size: var(--m-font-size-sm); font-style: italic; line-height: 1.4">{text}</p>
    }
}

/// A handler from a closure.
fn on(f: impl Fn() + 'static) -> Option<Handler> {
    Some(Rc::new(move |_: &Event| f()) as Handler)
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
                on(move || page.set(None))
            };
            let close_panel = {
                let close = close.clone();
                on(move || close.set(true))
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
        let l = Pages::from(l);
        move || -> View {
            match page.get() {
                None => l.home(&page),
                Some(SettingsTab::General) => l.general(),
                Some(SettingsTab::Graphics) => l.graphics(),
                Some(SettingsTab::World) => l.world(),
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

/// The signals the pages draw from, cloned out of [`Live`] so a page can be
/// re-rendered on a page turn.
#[derive(Clone)]
struct Pages {
    who: Signal<Option<String>>,
    signed_in: Signal<bool>,
    to_about: Rc<Cell<bool>>,
    ui_theme: Signal<UiTheme>,
    units: Signal<Units>,
    keep_screen_on: Signal<bool>,
    motion: Signal<MotionPref>,
    developer_menu: Signal<bool>,
    voice_guidance: Signal<bool>,
    quality: Signal<QualityPreset>,
    render_scale: Scale,
    frame_rate: Signal<Option<u32>>,
    bloom: Tri,
    anti_aliasing: Tri,
    water_effects: Signal<bool>,
    sky: Signal<SkyPreference>,
    sky_now: Signal<Option<SkyMode>>,
    shadows: Tri,
    sky_lighting: Tri,
    light_shafts: Tri,
    sun_mode: Signal<SunMode>,
    time_of_day: Signal<f64>,
    moon_age_days: Signal<f64>,
    auto_cycle: Signal<bool>,
    cycle_seconds: Signal<Option<f64>>,
    weather_live: Signal<bool>,
    clouds_enabled: Signal<bool>,
    cloud_shadows: Signal<bool>,
    cloud_cover: Signal<f64>,
    precipitation: Signal<f64>,
    fog_density: Signal<f64>,
}

impl From<&Live> for Pages {
    fn from(l: &Live) -> Self {
        Pages {
            who: l.who.clone(),
            signed_in: l.signed_in.clone(),
            to_about: l.to_about.clone(),
            ui_theme: l.ui_theme.clone(),
            units: l.units.clone(),
            keep_screen_on: l.keep_screen_on.clone(),
            motion: l.motion.clone(),
            developer_menu: l.developer_menu.clone(),
            voice_guidance: l.voice_guidance.clone(),
            quality: l.quality.clone(),
            render_scale: l.render_scale.clone(),
            frame_rate: l.frame_rate.clone(),
            bloom: l.bloom.clone(),
            anti_aliasing: l.anti_aliasing.clone(),
            water_effects: l.water_effects.clone(),
            sky: l.sky.clone(),
            sky_now: l.sky_now.clone(),
            shadows: l.shadows.clone(),
            sky_lighting: l.sky_lighting.clone(),
            light_shafts: l.light_shafts.clone(),
            sun_mode: l.sun_mode.clone(),
            time_of_day: l.time_of_day.clone(),
            moon_age_days: l.moon_age_days.clone(),
            auto_cycle: l.auto_cycle.clone(),
            cycle_seconds: l.cycle_seconds.clone(),
            weather_live: l.weather_live.clone(),
            clouds_enabled: l.clouds_enabled.clone(),
            cloud_shadows: l.cloud_shadows.clone(),
            cloud_cover: l.cloud_cover.clone(),
            precipitation: l.precipitation.clone(),
            fog_density: l.fog_density.clone(),
        }
    }
}

/// `[(value, label)]` as the segmented control's options.
fn opts<T: Clone + PartialEq + 'static>(items: &[(T, &str)]) -> Vec<Opt<T>> {
    items.iter().map(|(v, l)| Opt::new(v.clone(), *l)).collect()
}

/// A checkbox over an `Option<bool>` with its "Automatic" beside it, and the
/// line saying what Automatic means on this device (`settings::tri_state`).
fn tri_row(tri: &Tri, label: &'static str, default: bool, id: &'static str) -> View {
    let (value, auto) = (tri.value.clone(), tri.auto.clone());
    let leave_auto: Rc<dyn Fn(&bool)> = {
        let auto = auto.clone();
        Rc::new(move |_| auto.set(false))
    };
    let back_to_auto = {
        let (value, auto) = (value.clone(), auto.clone());
        on(move || {
            auto.set(true);
            value.set(default);
        })
    };
    let auto_button = {
        let auto = auto.clone();
        move || -> View {
            if auto.get() {
                View::Empty
            } else {
                view! { <Button variant=Variant::Ghost size=Size::Sm onclick={back_to_auto.clone()}>"Automatic"</Button> }
            }
        }
    };
    let auto_note = {
        let auto = auto.clone();
        move || -> View {
            if auto.get() {
                note(if default {
                    "Automatic — on for this device."
                } else {
                    "Automatic — off for this device."
                })
            } else {
                View::Empty
            }
        }
    };
    view! {
        <div style="display: flex; align-items: center; gap: 8px">
            <div style="flex: 1"><FieldCheckbox model={value} label={label} id={id} onchange={Some(leave_auto)} /></div>
            {auto_button}
        </div>
        {auto_note}
    }
}

impl Pages {
    /// The list of pages (`home-and-profile/v1`, D): the account's, then
    /// the app's, About last.
    fn home(&self, page: &Signal<Option<SettingsTab>>) -> View {
        let signed_in = self.signed_in.get();
        let who_line = self.who.get().map_or_else(
            || "Not signed in".to_owned(),
            |u| format!("Signed in as {u}"),
        );
        let row = |tab: SettingsTab| -> View {
            let page = page.clone();
            let sub: View = match tab {
                SettingsTab::Account => who_line.clone().into_view(),
                _ => tab.hint().map_or(View::Empty, IntoView::into_view),
            };
            let open = on(move || page.set(Some(tab)));
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
            let to_about = self.to_about.clone();
            on(move || to_about.set(true))
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

    /// The General page's preferences (`settings::general_tab`); the
    /// receiver, storage and editor-access sections still to come.
    fn general(&self) -> View {
        let themes: Vec<Opt<UiTheme>> = UiTheme::ALL
            .iter()
            .map(|t| Opt::new(*t, t.label()))
            .collect();
        let unit_opts: Vec<Opt<Units>> =
            Units::ALL.iter().map(|u| Opt::new(*u, u.label())).collect();
        let motions: Vec<Opt<MotionPref>> = MotionPref::ALL
            .iter()
            .map(|m| Opt::new(*m, m.label()))
            .collect();
        view! {
            <Section title="Appearance">
                <FieldSegmented model={self.ui_theme.clone()} options={themes} id="g-theme" />
                {note("Auto is light while the sun is up where the map is. The sky's own light is under World.")}
            </Section>
            <Section title="Units">
                <FieldSegmented model={self.units.clone()} options={unit_opts} id="g-units" />
                {note("Distances are shown in kilometres until the route and driver readouts read this.")}
            </Section>
            <Section title="Screen">
                <FieldCheckbox model={self.keep_screen_on.clone()} label="Keep the screen on while Cartopolis is in front" id="g-screen" />
            </Section>
            <Section title="Motion">
                <FieldSegmented model={self.motion.clone()} options={motions} id="g-motion" />
                {note("Reduced keeps buttons answering under the finger, and makes cards and pages appear in place.")}
            </Section>
            <Section title="Developer">
                <FieldCheckbox model={self.developer_menu.clone()} label="Show Developer tools and Share diagnostics in the menu" id="g-dev" />
            </Section>
            <Section title="Navigation">
                <FieldCheckbox model={self.voice_guidance.clone()} label="Chime at turns while navigating" id="g-chime" />
            </Section>
            <Section title="Receiver, storage, editor access">
                {note("Still egui's: these sections come with the next pages.")}
            </Section>
        }
    }

    /// Graphics (`settings::graphics_tab`): what this machine spends on the
    /// picture, in the order a struggling device should reach for.
    fn graphics(&self) -> View {
        let d = Defaults::now(&AppPrefs::default());
        let presets: Vec<Opt<QualityPreset>> = QualityPreset::ALL
            .iter()
            .map(|p| Opt::new(*p, p.label()))
            .collect();
        let skies: Vec<Opt<SkyPreference>> = SkyPreference::ALL
            .iter()
            .map(|s| Opt::new(*s, s.label()))
            .collect();
        let rates = opts(&[
            (None, "Automatic"),
            (Some(30), "30"),
            (Some(60), "60"),
            (Some(120), "120"),
            (Some(0), "No cap"),
        ]);
        // Render scale: a slider over `RenderScale::RANGE` with its
        // "Automatic" beside it.
        let scale = self.render_scale.clone();
        let leave_auto: Rc<dyn Fn(&f64)> = {
            let auto = scale.auto.clone();
            Rc::new(move |_| auto.set(false))
        };
        let scale_auto_button = {
            let (value, auto) = (scale.value.clone(), scale.auto.clone());
            move || -> View {
                if auto.get() {
                    note("Automatic for this device.")
                } else {
                    let back = {
                        let (value, auto) = (value.clone(), auto.clone());
                        on(move || {
                            auto.set(true);
                            value.set(f64::from(d.render_scale));
                        })
                    };
                    view! { <Button variant=Variant::Ghost size=Size::Sm onclick={back}>"Automatic"</Button> }
                }
            }
        };
        let percent: Rc<dyn Fn(f64) -> String> = Rc::new(|v| format!("{:.0}%", v * 100.0));
        let frame_note = {
            let rate = self.frame_rate.clone();
            move || -> View {
                note(match rate.get() {
                    None => {
                        let fps = crate::quality::budget().target_fps();
                        if fps > 0.0 {
                            format!("Automatic — {fps:.0} fps for this device. Lower saves battery and heat.")
                        } else {
                            "Automatic — no cap for this device.".to_owned()
                        }
                    }
                    Some(0) => "As fast as the device can draw while you use it, 60 fps when nothing moves. Uses the most battery.".to_owned(),
                    Some(_) => "Lower saves battery and heat.".to_owned(),
                })
            }
        };
        let sky_note = {
            let (sky, sky_now) = (self.sky.clone(), self.sky_now.clone());
            move || -> View {
                let filterable = crate::quality::capabilities().map(|c| c.float32_filterable);
                note(match sky_now.get() {
                    Some(SkyMode::Atmosphere) => "Drawing the full atmosphere.".to_owned(),
                    Some(SkyMode::Dome) => {
                        let launched_without_hdr = crate::quality::hdr_at_launch() == Some(false);
                        let why = match (sky.get(), filterable) {
                            (SkyPreference::Simple, _) => "as asked",
                            (_, Some(false)) => "this GPU cannot sample the atmosphere's lookup textures",
                            _ if launched_without_hdr && SkyMode::atmosphere_possible() => {
                                "the full sky turns on at the next start, since this run was launched with the simple sky and no bloom"
                            }
                            _ => "forced by a startup switch",
                        };
                        format!("Drawing the simple sky — {why}.")
                    }
                    Some(SkyMode::None) => {
                        "No sky at all — CARTO_NO_ATMOSPHERE is set for this run.".to_owned()
                    }
                    None => "The renderer has not chosen a sky yet.".to_owned(),
                })
            }
        };
        let shadows_forced_off = crate::env_flag("CARTO_NO_SHADOWS");
        let ibl_forced_off = crate::env_flag("CARTO_NO_ENVMAP");
        let ibl_possible = self.sky_now.get() != Some(SkyMode::Dome) && !ibl_forced_off;
        view! {
            <Section title="Detail">
                <FieldSegmented model={self.quality.clone()} options={presets} id="gr-quality" />
                {note("Applies immediately. Automatic picks by device. The settings below that are on Automatic follow it.")}
            </Section>
            <Section title="Render scale">
                <FieldSlider model={scale.value.clone()} min={f64::from(*RenderScale::RANGE.start())} max={f64::from(*RenderScale::RANGE.end())} step=0.05 format={Some(percent)} id="gr-scale" onchange={Some(leave_auto)} />
                {scale_auto_button}
                {note("The 3D world is drawn smaller and scaled up; the UI and text stay sharp.")}
            </Section>
            <Section title="Frame rate">
                <FieldSegmented model={self.frame_rate.clone()} options={rates} size=Some(Size::Sm) id="gr-fps" />
                {frame_note}
            </Section>
            <Section title="Effects">
                {tri_row(&self.bloom, "Bloom", d.bloom, "gr-bloom")}
                {note("Glare around the sun, lit windows and the planet's edge from orbit. Thirteen full-screen passes when on.")}
                {tri_row(&self.anti_aliasing, "Anti-aliasing", d.anti_aliasing, "gr-aa")}
                {note("Smooth edges. Applies immediately as an edge filter; multisampling, which is sharper and costs more memory traffic, follows at the next start.")}
                <FieldCheckbox model={self.water_effects.clone()} label="Splashes and spray" id="gr-water" />
                {note("Water that reacts: a splash where someone goes in, rings as they swim, spray on fountain jets. Their sound follows Sound effects.")}
            </Section>
            <Section title="Sky">
                <FieldSegmented model={self.sky.clone()} options={skies} id="gr-sky" />
                {note("Applies immediately. Simple is cheaper and works on every device.")}
                {sky_note}
            </Section>
            <Section title="Lighting">
                {tri_row(&self.shadows, "Shadows", d.shadows, "gr-shadows")}
                {note(if shadows_forced_off { "Forced off for this run by CARTO_NO_SHADOWS." } else { "The sun's shadow maps. Applies immediately — the light is respawned." })}
                {tri_row(&self.sky_lighting, "Sky reflections", d.sky_lighting, "gr-ibl")}
                {note(if ibl_forced_off { "Forced off for this run by CARTO_NO_ENVMAP." } else if ibl_possible { "Lights shadowed faces with the sky and gives water something to reflect. Costs a cubemap re-baked every frame. Applies immediately." } else { "Needs the full atmosphere — the simple sky has nothing to reflect." })}
                {tri_row(&self.light_shafts, "Light shafts", d.light_shafts, "gr-shafts")}
                {note("Sunbeams through the haze, marched along the sun's shadow maps. Needs shadows and a hardware adapter.")}
            </Section>
            {note("Frame rate, draw counts, GPU uploads and the startup switches for bisecting a render problem are in Developer tools — in the menu, or Ctrl+Shift+D.")}
        }
    }

    /// World (`settings::world_tab`): the clock and the weather over the map.
    fn world(&self) -> View {
        let suns = opts(&[
            (SunMode::Auto, "Auto"),
            (SunMode::Day, "Light"),
            (SunMode::Night, "Dark"),
            (SunMode::Manual, "Set a time"),
        ]);
        let presets: Vec<View> = [
            ("Sunrise", 6.0),
            ("Noon", 12.0),
            ("Sunset", 18.0),
            ("Midnight", 0.0),
        ]
        .into_iter()
        .map(|(label, hour)| {
            let (time, mode) = (self.time_of_day.clone(), self.sun_mode.clone());
            let set = on(move || {
                time.set(hour);
                mode.set(SunMode::Manual);
            });
            view! { <Button variant=Variant::Secondary size=Size::Sm onclick={set}>{label}</Button> }
        })
        .collect();
        let clock: Rc<dyn Fn(f64) -> String> = Rc::new(|h| clock_hhmm(h as f32));
        let moon: Rc<dyn Fn(f64) -> String> = Rc::new(|days| {
            let lit = crate::utils::sun::moon_illuminated_fraction(days as f32);
            format!("{days:.1} d · {:.0}% lit", lit * 100.0)
        });
        let percent: Rc<dyn Fn(f64) -> String> = Rc::new(|v| format!("{:.0}%", v * 100.0));
        let manual_controls = {
            let (mode, time, moon_age, auto_cycle, cycle_seconds) = (
                self.sun_mode.clone(),
                self.time_of_day.clone(),
                self.moon_age_days.clone(),
                self.auto_cycle.clone(),
                self.cycle_seconds.clone(),
            );
            let (clock, moon) = (clock.clone(), moon.clone());
            move || -> View {
                // Disabled, not hidden, outside Manual: the tab always says
                // what the scene is doing.
                let off = mode.get() != SunMode::Manual;
                view! {
                    <FieldSlider model={time.clone()} min=0.0 max=24.0 step=0.05 format={Some(clock.clone())} label="Time" id="w-time" disabled={off} />
                    <FieldSlider model={moon_age.clone()} min=0.0 max={f64::from(crate::utils::sun::LUNAR_CYCLE_DAYS)} step=0.1 format={Some(moon.clone())} label="Moon" id="w-moon" disabled={off} />
                    <div style="display: flex; align-items: center; gap: 12px">
                        <div style="flex: 1"><FieldCheckbox model={auto_cycle.clone()} label="Run the day" id="w-cycle" disabled={off} /></div>
                        <div style="width: 150px"><FieldNumber model={cycle_seconds.clone()} min=Some(1.0) max=Some(120.0) step=1.0 unit="s" id="w-cycle-s" disabled={off} /></div>
                    </div>
                }
            }
        };
        let sun_note = {
            let mode = self.sun_mode.clone();
            move || -> View {
                note(match mode.get() {
                    SunMode::Auto => "Sun and moon follow the real time where the map is anchored.",
                    SunMode::Day => "Noon in the street. From above, the side you look at is day, and the real sunset line is dashed.",
                    SunMode::Night => "Midnight in the street. From above, the Earth at night all round.",
                    SunMode::Manual => "The clock is yours. Run the day loops it in the seconds beside it.",
                })
            }
        };
        let clouds_forced_off = crate::env_flag("CARTO_NO_CLOUDS");
        let weather_modes = opts(&[(true, "Automatic"), (false, "Manual")]);
        let cloud_shadows_row = {
            let (clouds, shadows) = (self.clouds_enabled.clone(), self.cloud_shadows.clone());
            move || -> View {
                let off = clouds_forced_off || !clouds.get();
                view! { <FieldCheckbox model={shadows.clone()} label="Cloud shadows" id="w-cloud-shadows" disabled={off} /> }
            }
        };
        let manual_weather = {
            let (live, cover, rain, fog) = (
                self.weather_live.clone(),
                self.cloud_cover.clone(),
                self.precipitation.clone(),
                self.fog_density.clone(),
            );
            let percent = percent.clone();
            move || -> View {
                let off = live.get();
                view! {
                    <FieldSlider model={cover.clone()} min=0.0 max=1.0 step=0.01 format={Some(percent.clone())} label="Cloud" id="w-cloud" disabled={off} />
                    <FieldSlider model={rain.clone()} min=0.0 max=1.0 step=0.01 format={Some(percent.clone())} label="Rain" id="w-rain" disabled={off} />
                    <FieldSlider model={fog.clone()} min=0.0 max=1.0 step=0.01 format={Some(percent.clone())} label="Fog" id="w-fog" disabled={off} />
                }
            }
        };
        view! {
            <Section title="Light">
                <FieldSegmented model={self.sun_mode.clone()} options={suns} id="w-sun" />
                <div style="display: flex; flex-wrap: wrap; gap: 6px">{presets}</div>
                {manual_controls}
                {sun_note}
            </Section>
            <Section title="Weather">
                <FieldSegmented model={self.weather_live.clone()} options={weather_modes} id="w-weather" />
                <FieldCheckbox model={self.clouds_enabled.clone()} label="Clouds" id="w-clouds" disabled={clouds_forced_off} />
                {note(if clouds_forced_off { "Forced off for this run by CARTO_NO_CLOUDS." } else { "Draws the cloud deck over the city and the cloud layer over the globe. Turning it off is the cheapest thing on this tab — from orbit the cloud shell covers the whole screen." })}
                {cloud_shadows_row}
                {note("The clouds' shadows drift over the streets and the roofs. Costs a little on every pixel of ground and building; none under a clear sky or a closed deck, where there is nothing to cast them.")}
                {manual_weather}
            </Section>
        }
    }
}

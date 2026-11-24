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
//! the mockup's (`home-and-profile/v1`, D); every page is here, the camera
//! test and General's receiver, storage and editor-access sections apart.
//!
//! **State.** The view reads signals; the app's state is Bevy resources. The
//! bridge is a frame-by-frame diff in [`drive_lib_settings`]: a signal
//! whose value moved since the last frame writes the resource, a resource
//! that moved writes the signal, and a resource is assigned only when
//! something differs (`AppPrefs` and `DevPanelState` are read through
//! `is_changed()` gates that re-run work). An action — a tone, the
//! microphone, a deletion, a revoke — is a `Cell` the view sets and the
//! system takes, where the resources and the connection are.
//!
//! `CARTO_UI_LIB_PAGE=general|graphics|world|…` opens on that page (for a
//! capture); `CARTO_UI_LIB_HTML=<path>` writes the panel's HTML once, for
//! the browser beside it.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use bevy::ecs::system::{NonSendMarker, SystemParam};
use bevy::prelude::*;
#[cfg(feature = "comms")]
use cartopolis_core::{msg, ContactSettings, NetMessage, RequestPolicy};
// The library's, not Bevy's: an explicit import outranks the two globs.
use mithril::prelude::*;
use mithril::prelude::{Button, Event};

use crate::platform::audio::AudioInput;
use crate::resources::{
    AppPrefs, AuthIdentity, DevPanelState, MotionPref, OnlineSession, QualityPreset, RightPanel,
    SkyPreference, SunMode, UiTheme, Units,
};
#[cfg(feature = "comms")]
use crate::systems::access_tokens::{self, AccessTokens, Expiry, TokenInfo};
use crate::systems::egui_manager::EguiMut;
use crate::systems::render_scale::RenderScale;
use crate::systems::settings::{clock_hhmm, observe_peak, SettingsState, SettingsTab};
use crate::systems::sky_dome::SkyMode;
use crate::systems::weather::WeatherState;
use crate::systems::{icons, theme};

/// Which screens the library draws instead of egui, and what was asked of
/// them at startup.
#[derive(Resource, Default, Clone, Debug)]
pub struct LibScreens {
    pub settings: bool,
    pub layers: bool,
    pub search: bool,
    /// Open Settings at once (a capture, a page's `?panel=settings`).
    pub open_settings: bool,
    /// Open Layers at once (`?panel=layers`).
    pub open_layers: bool,
    /// Open Search at once (`?panel=search`).
    pub open_search: bool,
    /// The Settings page to open on, by label or name.
    pub page: Option<String>,
}

impl LibScreens {
    /// Natively `CARTO_UI_LIB=settings` (or `1`, every screen there is) and
    /// `CARTO_UI_LIB_PAGE`; on the web the page's query, `?ui=lib`,
    /// `&panel=settings`, `&page=graphics`.
    pub fn from_env() -> Self {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let v = std::env::var("CARTO_UI_LIB").unwrap_or_default();
            let all = v.trim() == "1";
            let named = |s: &str| v.split(',').any(|x| x.trim() == s);
            LibScreens {
                settings: all || named("settings"),
                layers: all || named("layers"),
                search: all || named("search"),
                open_settings: false,
                open_layers: false,
                open_search: false,
                page: std::env::var("CARTO_UI_LIB_PAGE").ok(),
            }
        }
        #[cfg(target_arch = "wasm32")]
        {
            let search = web_sys::window()
                .and_then(|w| w.location().search().ok())
                .unwrap_or_default();
            let params = web_sys::UrlSearchParams::new_with_str(&search).ok();
            let get = |k: &str| params.as_ref().and_then(|p| p.get(k));
            let ui = get("ui").unwrap_or_default();
            let named = |s: &str| ui == "lib" || ui.split(',').any(|x| x == s);
            let panel = get("panel");
            LibScreens {
                settings: named("settings"),
                layers: named("layers"),
                search: named("search"),
                open_settings: panel.as_deref() == Some("settings"),
                open_layers: panel.as_deref() == Some("layers"),
                open_search: panel.as_deref() == Some("search"),
                page: get("page"),
            }
        }
    }
}

/// What the Bevy backend tells the panel: whether the pointer is over it.
/// (Its camera, faces and scale are `lib_host::prepare_bevy`'s.)
#[cfg(feature = "ui-probe")]
#[derive(SystemParam)]
pub struct BevyBackend<'w> {
    pointer: Res<'w, mithril_bevy::UiPointer>,
}

/// The app's state the pages read and write, in one parameter.
#[derive(SystemParam)]
pub struct AppState<'w> {
    prefs: ResMut<'w, AppPrefs>,
    dev: ResMut<'w, DevPanelState>,
    weather: ResMut<'w, WeatherState>,
    identity: ResMut<'w, AuthIdentity>,
    online: Option<Res<'w, OnlineSession>>,
    sky: Option<Res<'w, SkyMode>>,
    time: Res<'w, Time>,
    #[cfg(feature = "audio")]
    soundscape: Option<Res<'w, crate::systems::player::soundscape::Soundscape>>,
    #[cfg(feature = "comms")]
    chat: ResMut<'w, crate::systems::chat::ChatState>,
    #[cfg(feature = "comms")]
    contacts: ResMut<'w, crate::systems::contacts::Contacts>,
    #[cfg(feature = "comms")]
    tokens: ResMut<'w, AccessTokens>,
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

/// Something the view asks the system to do, where the resources are.
#[derive(Clone, Default)]
struct Actions {
    close: Rc<Cell<bool>>,
    to_about: Rc<Cell<bool>>,
    play_tone: Rc<Cell<bool>>,
    toggle_mic: Rc<Cell<bool>>,
    delete_account: Rc<Cell<bool>>,
    unblock: Rc<RefCell<Vec<String>>>,
    revoke: Rc<RefCell<Option<String>>>,
    make_token: Rc<Cell<bool>>,
    token_done: Rc<Cell<bool>>,
    copy: Rc<RefCell<Option<String>>>,
}

/// The signals every page draws from, and what the app last saw of its
/// state (held by [`Live`]; the pages get a clone of the signals).
#[derive(Clone)]
struct Pages {
    who: Signal<Option<String>>,
    signed_in: Signal<bool>,
    act: Actions,
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
    // Audio
    effects_volume: Signal<f64>,
    voice_volume: Signal<f64>,
    mic_gain: Signal<f64>,
    mic_running: Signal<bool>,
    /// The meter and its peak while a test runs (or the demo's).
    mic_level: Signal<Option<(f32, f32)>>,
    /// What the microphone says of itself: an error, "waiting", its name.
    mic_status: Signal<Option<String>>,
    // Account
    deleted: Signal<bool>,
    delete_error: Signal<Option<String>>,
    delete_password: Signal<String>,
    delete_armed: Signal<bool>,
    // Privacy and tokens (`comms`)
    #[cfg(feature = "comms")]
    policy: Signal<RequestPolicy>,
    #[cfg(feature = "comms")]
    separate_new_accounts: Signal<bool>,
    #[cfg(feature = "comms")]
    notify_requests: Signal<bool>,
    #[cfg(feature = "comms")]
    show_online: Signal<bool>,
    #[cfg(feature = "comms")]
    blocked: Signal<Vec<String>>,
    #[cfg(feature = "comms")]
    token_list: Signal<Option<Result<Vec<TokenInfo>, String>>>,
    #[cfg(feature = "comms")]
    token_label: Signal<String>,
    #[cfg(feature = "comms")]
    token_expiry: Signal<Expiry>,
    #[cfg(feature = "comms")]
    token_elevated: Signal<bool>,
    #[cfg(feature = "comms")]
    token_password: Signal<String>,
    #[cfg(feature = "comms")]
    token_shown: Signal<Option<(String, String)>>,
    #[cfg(feature = "comms")]
    token_busy: Signal<bool>,
    #[cfg(feature = "comms")]
    token_error: Signal<Option<String>>,
    #[cfg(feature = "comms")]
    may_elevate: Signal<bool>,
    #[cfg(feature = "comms")]
    revoke_armed: Signal<Option<String>>,
    /// The page shows its demo content (a capture), signed out.
    #[cfg(feature = "comms")]
    tokens_demo: Signal<bool>,
}

/// The mounted panel: its signals and the state as last synced, to tell a
/// signal's move from a resource's.
struct Live {
    page: Signal<Option<SettingsTab>>,
    p: Pages,
    last_prefs: AppPrefs,
    last_dev: DevPanelState,
    last_weather_live: bool,
    last_delete_password: String,
    #[cfg(feature = "comms")]
    last_contacts: ContactSettings,
    #[cfg(feature = "comms")]
    last_token_form: (String, Expiry, bool, String),
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

/// Whether the signed-in account may give a token its admin rights.
#[cfg(feature = "comms")]
fn may_elevate(identity: &AuthIdentity) -> bool {
    identity
        .permissions
        .iter()
        .filter_map(|s| cartopolis_core::permissions::Permission::from_slug(s))
        .any(|p| p.elevated())
}

impl Live {
    fn new(
        app: &AppState,
        state: &SettingsState,
        page_hint: Option<&str>,
        who: Option<String>,
        signed_in: bool,
    ) -> Self {
        // The page asked for: by the capture's environment or the page's
        // query (`LibScreens::page`), else by whoever opened the panel on one
        // (`--ui graphics`, `shot_harness`).
        let page = page_hint
            .and_then(|p| {
                SettingsTab::ALL.into_iter().find(|t| {
                    t.label().eq_ignore_ascii_case(p) || format!("{t:?}").eq_ignore_ascii_case(p)
                })
            })
            .or(state.page);
        let prefs = &*app.prefs;
        let dev = &*app.dev;
        let d = Defaults::now(prefs);
        let p = Pages {
            who: signal(who),
            signed_in: signal(signed_in),
            act: Actions::default(),
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
            sky_now: signal(app.sky.as_deref().copied()),
            shadows: Tri::new(prefs.shadows, d.shadows),
            sky_lighting: Tri::new(prefs.sky_lighting, d.sky_lighting),
            light_shafts: Tri::new(prefs.light_shafts, d.light_shafts),
            sun_mode: signal(dev.sun_mode),
            time_of_day: signal(f64::from(dev.time_of_day)),
            moon_age_days: signal(f64::from(dev.moon_age_days)),
            auto_cycle: signal(dev.auto_cycle),
            cycle_seconds: signal(Some(f64::from(dev.cycle_seconds))),
            weather_live: signal(app.weather.live),
            clouds_enabled: signal(dev.clouds_enabled),
            cloud_shadows: signal(dev.cloud_shadows),
            cloud_cover: signal(f64::from(dev.cloud_cover)),
            precipitation: signal(f64::from(dev.precipitation)),
            fog_density: signal(f64::from(dev.fog_density)),
            effects_volume: signal(f64::from(prefs.effects_volume)),
            voice_volume: signal(f64::from(prefs.voice_volume)),
            mic_gain: signal(f64::from(prefs.mic_gain)),
            mic_running: signal(state.mic.is_some()),
            mic_level: signal(state.demo_level),
            mic_status: signal(None),
            deleted: signal(app.identity.deleted),
            delete_error: signal(app.identity.delete_error.clone()),
            delete_password: signal(state.delete_password.clone()),
            delete_armed: signal(false),
            #[cfg(feature = "comms")]
            policy: signal(app.contacts.settings.policy),
            #[cfg(feature = "comms")]
            separate_new_accounts: signal(app.contacts.settings.separate_new_accounts),
            #[cfg(feature = "comms")]
            notify_requests: signal(app.contacts.settings.notify_requests),
            #[cfg(feature = "comms")]
            show_online: signal(app.contacts.settings.show_online),
            #[cfg(feature = "comms")]
            blocked: signal(app.chat.blocked.clone()),
            #[cfg(feature = "comms")]
            token_list: signal(app.tokens.list.clone()),
            #[cfg(feature = "comms")]
            token_label: signal(app.tokens.label.clone()),
            #[cfg(feature = "comms")]
            token_expiry: signal(app.tokens.expiry),
            #[cfg(feature = "comms")]
            token_elevated: signal(app.tokens.elevated),
            #[cfg(feature = "comms")]
            token_password: signal(app.tokens.password.clone()),
            #[cfg(feature = "comms")]
            token_shown: signal(app.tokens.shown.clone()),
            #[cfg(feature = "comms")]
            token_busy: signal(app.tokens.busy),
            #[cfg(feature = "comms")]
            token_error: signal(app.tokens.error.clone()),
            #[cfg(feature = "comms")]
            may_elevate: signal(may_elevate(&app.identity)),
            #[cfg(feature = "comms")]
            revoke_armed: signal(None),
            #[cfg(feature = "comms")]
            tokens_demo: signal(app.tokens.demo),
        };
        Live {
            page: signal(page),
            p,
            last_prefs: *prefs,
            last_dev: dev.clone(),
            last_weather_live: app.weather.live,
            last_delete_password: state.delete_password.clone(),
            #[cfg(feature = "comms")]
            last_contacts: app.contacts.settings.clone(),
            #[cfg(feature = "comms")]
            last_token_form: (
                app.tokens.label.clone(),
                app.tokens.expiry,
                app.tokens.elevated,
                app.tokens.password.clone(),
            ),
        }
    }

    /// Signals that moved write the state; state that moved writes the
    /// signals. Answers the preferences, the world state and the weather
    /// switch to keep.
    fn sync(
        &mut self,
        prefs: &AppPrefs,
        dev: &DevPanelState,
        weather_live: bool,
    ) -> (AppPrefs, DevPanelState, bool) {
        let p = &self.p;
        let d = Defaults::now(prefs);
        let mut edited = *prefs;
        // A plain signal against a plain field.
        macro_rules! plain {
            ($sig:expr, $edited:expr, $now:expr, $last:expr) => {{
                let s = $sig.get();
                if s != $last {
                    $edited = s;
                } else if $now != $last {
                    $sig.set($now);
                }
            }};
        }
        // An `f64` signal against an `f32` field.
        macro_rules! plain32 {
            ($sig:expr, $edited:expr, $now:expr, $last:expr) => {{
                let s = $sig.get() as f32;
                if s != $last {
                    $edited = s;
                } else if $now != $last {
                    $sig.set(f64::from($now));
                }
            }};
        }
        macro_rules! pref {
            ($f:ident) => {
                plain!(p.$f, edited.$f, prefs.$f, self.last_prefs.$f)
            };
            ($f:ident, f32) => {
                plain32!(p.$f, edited.$f, prefs.$f, self.last_prefs.$f)
            };
        }
        macro_rules! tri {
            ($f:ident, $default:expr) => {{
                p.$f.follow($default);
                let s = p.$f.get();
                if s != self.last_prefs.$f {
                    edited.$f = s;
                } else if prefs.$f != self.last_prefs.$f {
                    p.$f.set(prefs.$f, $default);
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
        pref!(effects_volume, f32);
        pref!(voice_volume, f32);
        pref!(mic_gain, f32);
        tri!(bloom, d.bloom);
        tri!(anti_aliasing, d.anti_aliasing);
        tri!(shadows, d.shadows);
        tri!(sky_lighting, d.sky_lighting);
        tri!(light_shafts, d.light_shafts);
        {
            let s = p.render_scale.get();
            if s != self.last_prefs.render_scale {
                edited.render_scale = s;
            } else if prefs.render_scale != self.last_prefs.render_scale {
                p.render_scale.set(prefs.render_scale, d.render_scale);
            }
        }
        self.last_prefs = edited;

        let mut world = dev.clone();
        macro_rules! devf {
            ($f:ident) => {
                plain!(p.$f, world.$f, dev.$f, self.last_dev.$f)
            };
            ($f:ident, f32) => {
                plain32!(p.$f, world.$f, dev.$f, self.last_dev.$f)
            };
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
            let s = p
                .cycle_seconds
                .get()
                .map_or(dev.cycle_seconds, |v| v as f32);
            if s != self.last_dev.cycle_seconds {
                world.cycle_seconds = s.clamp(1.0, 120.0);
            } else if dev.cycle_seconds != self.last_dev.cycle_seconds {
                p.cycle_seconds.set(Some(f64::from(dev.cycle_seconds)));
            }
        }
        self.last_dev = world.clone();

        let mut live = weather_live;
        plain!(p.weather_live, live, weather_live, self.last_weather_live);
        self.last_weather_live = live;
        (edited, world, live)
    }

    /// The contact settings, the same way; answers them when they changed.
    #[cfg(feature = "comms")]
    fn sync_contacts(&mut self, now: &ContactSettings) -> Option<ContactSettings> {
        let p = &self.p;
        let mut edited = now.clone();
        macro_rules! field {
            ($f:ident) => {{
                let s = p.$f.get();
                if s != self.last_contacts.$f {
                    edited.$f = s;
                } else if now.$f != self.last_contacts.$f {
                    p.$f.set(now.$f);
                }
            }};
        }
        field!(policy);
        field!(separate_new_accounts);
        field!(notify_requests);
        field!(show_online);
        self.last_contacts = edited.clone();
        (edited != *now).then_some(edited)
    }

    /// The token form's fields, the same way, written straight into the
    /// store (plain strings, no gate behind them).
    #[cfg(feature = "comms")]
    fn sync_token_form(&mut self, tokens: &mut AccessTokens) {
        let p = &self.p;
        let last = &mut self.last_token_form;
        macro_rules! field {
            ($sig:ident, $f:ident, $i:tt) => {{
                let s = p.$sig.get();
                if s != last.$i {
                    tokens.$f = s.clone();
                    last.$i = s;
                } else if tokens.$f != last.$i {
                    p.$sig.set(tokens.$f.clone());
                    last.$i = tokens.$f.clone();
                }
            }};
        }
        field!(token_label, label, 0);
        field!(token_expiry, expiry, 1);
        field!(token_elevated, elevated, 2);
        field!(token_password, password, 3);
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
    // Written to only by the Bevy backend (the pointer's ownership).
    #[cfg_attr(not(feature = "ui-probe"), allow(unused_mut))] mut egui: EguiMut,
    #[cfg(feature = "ui-probe")] bevy: BevyBackend,
    mut commands: Commands,
    mut opened: Local<bool>,
) {
    if !lib.settings {
        return;
    }
    if !state.lib {
        state.lib = true;
    }
    // Asked to open on Settings (`?panel=settings` on the web): once.
    if lib.open_settings && !*opened {
        *opened = true;
        *right_panel = RightPanel::Settings;
    }
    let open = right_panel.is(RightPanel::Settings);
    // A world click under the panel is the panel's. (As DOM the panel sits
    // over the canvas and the click never reaches it.)
    #[cfg(feature = "ui-probe")]
    egui.set_overlay_owns_pointer(open && bevy.pointer.owns_pointer());
    let signed_in = app.identity.username.is_some() && app.online.is_some();
    let who = signed_in.then(|| app.identity.username.clone()).flatten();
    LIVE.with(|live| {
        let mut live = live.borrow_mut();
        match (open, live.as_mut()) {
            (false, None) => {}
            (false, Some(_)) => {
                *live = None;
                crate::systems::lib_host::clear();
                // What egui's panel does on closing: the microphone off, the
                // typed password gone, the tokens forgotten.
                state.page = None;
                if state.mic.is_some() {
                    state.stop_mic();
                }
                state.delete_password.clear();
                #[cfg(feature = "comms")]
                app.tokens.forget();
            }
            (true, None) => {
                let l = Live::new(&app, &state, lib.page.as_deref(), who, signed_in);
                let compact = egui.compact();
                let p = theme::p();
                crate::systems::lib_host::mount(
                    panel(&l, compact),
                    p.ink == theme::DARK.ink,
                    theme::touch_layout(),
                );
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
                let p = &l.p;
                macro_rules! mirror {
                    ($sig:expr, $now:expr) => {{
                        let now = $now;
                        if $sig.get() != now {
                            $sig.set(now);
                        }
                    }};
                }
                mirror!(p.signed_in, signed_in);
                mirror!(p.who, who.clone());
                mirror!(p.sky_now, app.sky.as_deref().copied());
                mirror!(p.deleted, app.identity.deleted);
                mirror!(p.delete_error, app.identity.delete_error.clone());
                state.page = l.page.get();

                // Audio: the microphone test and its meter.
                let on_audio = state.page == Some(SettingsTab::Audio);
                if p.act.toggle_mic.take() {
                    if state.mic.is_some() {
                        state.stop_mic();
                    } else {
                        state.start_mic();
                    }
                }
                if !on_audio && state.mic.is_some() {
                    state.stop_mic();
                }
                mirror!(p.mic_running, state.mic.is_some());
                let (now, dt) = (app.time.elapsed_secs_f64(), app.time.delta_secs());
                let gain = app.prefs.capture_gain();
                let (level, status) = match observe_peak(&mut state, gain, now, dt) {
                    Some(level) => {
                        let mic = state.mic.as_ref().expect("observe_peak proved it is Some");
                        if let Some(err) = mic.error() {
                            (None, Some(err))
                        } else if !mic.is_receiving() {
                            (None, Some("Waiting for the microphone…".to_owned()))
                        } else {
                            (Some(level), mic.device_name())
                        }
                    }
                    None => (state.demo_level, None),
                };
                mirror!(p.mic_level, level);
                mirror!(p.mic_status, status);
                if p.act.play_tone.take() {
                    #[cfg(feature = "audio")]
                    if let Some(soundscape) = app.soundscape.as_deref() {
                        commands.spawn((
                            bevy::audio::AudioPlayer(soundscape.test_tone.clone()),
                            bevy::audio::PlaybackSettings {
                                mode: bevy::audio::PlaybackMode::Despawn,
                                ..default()
                            },
                        ));
                    }
                }

                // Account: the password typed, and the deletion asked for.
                {
                    let s = p.delete_password.get();
                    if s != l.last_delete_password {
                        state.delete_password = s.clone();
                        l.last_delete_password = s;
                    } else if state.delete_password != l.last_delete_password {
                        p.delete_password.set(state.delete_password.clone());
                        l.last_delete_password = state.delete_password.clone();
                    }
                }
                if p.act.delete_account.take() && !state.delete_password.is_empty() {
                    if let Some(online) = app.online.as_deref() {
                        online.send(
                            "delete account",
                            cartopolis_core::NetMessage::DeleteAccount(
                                cartopolis_core::msg::DeleteAccount {
                                    request_id: crate::resources::DELETE_ACCOUNT_REQUEST,
                                    password: std::mem::take(&mut state.delete_password),
                                },
                            ),
                        );
                        p.delete_password.set(String::new());
                        l.last_delete_password.clear();
                    }
                }

                #[cfg(feature = "comms")]
                {
                    // Privacy: the request policy and the block list.
                    let settings_now = app.contacts.settings.clone();
                    if let Some(edited) = l.sync_contacts(&settings_now) {
                        app.contacts.settings = edited;
                        app.contacts.push_settings(false);
                    }
                    mirror!(l.p.blocked, app.chat.blocked.clone());
                    let unblock: Vec<String> = std::mem::take(&mut *l.p.act.unblock.borrow_mut());
                    if let Some(online) = app.online.as_deref() {
                        for username in unblock {
                            online.send(
                                "set blocked",
                                NetMessage::SetBlocked(msg::SetBlocked {
                                    request_id: 0,
                                    username,
                                    blocked: false,
                                }),
                            );
                        }
                    }
                    // Tokens: the list asked for on the page, the form, the
                    // actions, the secret shown once.
                    if state.page == Some(SettingsTab::Tokens) {
                        app.tokens.drain();
                        if !app.tokens.asked {
                            app.tokens.asked = true;
                            app.tokens.refresh();
                        }
                    }
                    l.sync_token_form(&mut app.tokens);
                    if l.p.act.make_token.take() {
                        app.tokens.make();
                    }
                    if let Some(id) = l.p.act.revoke.borrow_mut().take() {
                        let token = app
                            .tokens
                            .list
                            .as_ref()
                            .and_then(|r| r.as_ref().ok())
                            .and_then(|list| list.iter().find(|t| t.id == id).cloned());
                        if let Some(t) = token {
                            app.tokens.revoke(&t);
                        }
                    }
                    if l.p.act.token_done.take() {
                        app.tokens.shown = None;
                    }
                    if let Some(text) = l.p.act.copy.borrow_mut().take() {
                        egui.ctx().copy_text(text);
                    }
                    mirror!(l.p.token_list, app.tokens.list.clone());
                    mirror!(l.p.token_shown, app.tokens.shown.clone());
                    mirror!(l.p.token_busy, app.tokens.busy);
                    mirror!(l.p.token_error, app.tokens.error.clone());
                    mirror!(l.p.may_elevate, may_elevate(&app.identity));
                    mirror!(l.p.tokens_demo, app.tokens.demo);
                }

                if l.p.act.close.take() {
                    *right_panel = RightPanel::None;
                }
                if l.p.act.to_about.take() {
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

/// A line in the error colour.
fn error_line(text: impl Into<String>) -> View {
    let text: String = text.into();
    view! {
        <p style="margin: 0; color: var(--m-danger); font-size: var(--m-font-size-sm); line-height: 1.4">{text}</p>
    }
}

/// A short strong line: a subsection's title.
fn strong(text: impl Into<String>) -> View {
    let text: String = text.into();
    view! {
        <p style="margin: 0; color: var(--m-text-strong); font-weight: 600">{text}</p>
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
        let close = l.p.act.close.clone();
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
                    <div style="display: flex; align-items: center; gap: 12px; min-height: 56px; padding: 0 12px 0 20px; border-bottom: 1px solid var(--m-border)">
                        <span style="color: var(--m-accent)">{icon(icons::SETTINGS)}</span>
                        <h1 style="flex: 1; margin: 0; font-size: 22px; font-weight: 600; color: var(--m-text-strong)">"Settings"</h1>
                        <CloseButton onclick={close_panel} />
                    </div>
                },
                Some(p) => view! {
                    <div style="display: flex; align-items: center; gap: 4px; min-height: 56px; padding: 0 12px 0 10px; border-bottom: 1px solid var(--m-border)">
                        <Button variant=Variant::Ghost size=Size::Sm onclick={back_to_list} aria_label="Back">{icon(icons::BACK)}</Button>
                        <h1 style="flex: 1; margin: 0; font-size: 20px; font-weight: 600; color: var(--m-text-strong)">{p.label()}</h1>
                    </div>
                },
            }
        }
    };
    let body = {
        let page = l.page.clone();
        let p = l.p.clone();
        move || -> View {
            match page.get() {
                None => p.home(&page),
                Some(SettingsTab::General) => p.general(),
                Some(SettingsTab::Graphics) => p.graphics(),
                Some(SettingsTab::World) => p.world(),
                Some(SettingsTab::Audio) => p.audio(),
                Some(SettingsTab::Account) => p.account(),
                #[cfg(feature = "comms")]
                Some(SettingsTab::Privacy) => p.privacy(),
                #[cfg(feature = "comms")]
                Some(SettingsTab::Tokens) => p.tokens(),
                // Not in the web build's list (`SettingsTab::ALL`).
                #[cfg(not(feature = "comms"))]
                Some(SettingsTab::Privacy | SettingsTab::Tokens) => View::Empty,
            }
        }
    };
    view! {
        <div style={frame}>
            {header}
            <div style="flex: 1; min-height: 0; overflow-y: auto; padding: 20px 20px 28px; display: flex; flex-direction: column; gap: 20px">
                {body}
            </div>
        </div>
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

/// A button that asks twice (`theme::confirm_button`): the first press arms
/// it into the danger tone, the second does it.
fn confirm_button(
    armed: Signal<bool>,
    label: &'static str,
    confirm_label: &'static str,
    enabled: Rc<dyn Fn() -> bool>,
    fire: impl Fn() + 'static,
) -> View {
    let fire = Rc::new(fire);
    move || -> View {
        let is_armed = armed.get();
        let disabled = !enabled();
        let click = {
            let (armed, fire) = (armed.clone(), fire.clone());
            on(move || {
                if armed.get() {
                    armed.set(false);
                    fire();
                } else {
                    armed.set(true);
                }
            })
        };
        if is_armed {
            view! { <Button variant=Variant::Danger size=Size::Sm onclick={click} disabled={disabled}>{confirm_label}</Button> }
        } else {
            view! { <Button variant=Variant::Secondary size=Size::Sm onclick={click} disabled={disabled}>{label}</Button> }
        }
    }
    .into_view()
}

/// Seconds since the epoch, for the tokens' dates.
#[cfg(feature = "comms")]
fn now_s() -> u64 {
    web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
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
            let to_about = self.act.to_about.clone();
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

    /// Sound and voice (`settings::audio_tab`): the chime, the levels, the
    /// microphone test with its meter. The camera test stays egui's: it
    /// shows a live picture, which the library has no image node for yet.
    fn audio(&self) -> View {
        let has_audio = cfg!(feature = "audio");
        let play = {
            let play = self.act.play_tone.clone();
            on(move || play.set(true))
        };
        let percent: Rc<dyn Fn(f64) -> String> = Rc::new(|v| format!("{:.0}%", v * 100.0));
        let gain_pct: Rc<dyn Fn(f64) -> String> = Rc::new(|v| format!("{:.0}%", v * 100.0));
        let mic_block = if AudioInput::available() {
            let toggle = {
                let t = self.act.toggle_mic.clone();
                on(move || t.set(true))
            };
            let button = {
                let running = self.mic_running.clone();
                let toggle = toggle.clone();
                move || -> View {
                    if running.get() {
                        view! { <Button variant=Variant::Secondary icon={icon(icons::STOP)} onclick={toggle.clone()}>"Stop microphone"</Button> }
                    } else {
                        view! { <Button variant=Variant::Secondary icon={icon(icons::MIC)} onclick={toggle.clone()}>"Test microphone"</Button> }
                    }
                }
            };
            let meter = {
                let (level, status, running) = (
                    self.mic_level.clone(),
                    self.mic_status.clone(),
                    self.mic_running.clone(),
                );
                move || -> View {
                    let bar = level.get().map_or(View::Empty, |(meter, peak)| {
                        let shown = f64::from(meter.clamp(0.0, 1.0) * 100.0);
                        let peak_note = format!("peak {:.0}%", peak * 100.0);
                        view! {
                            <Progress value={Some(shown)} label="Level" />
                            {note(peak_note)}
                        }
                    });
                    let line = match (running.get(), status.get()) {
                        (true, Some(s)) if level.get().is_none() => error_line(s),
                        (true, Some(name)) => note(format!(
                            "{name} — speak normally; the bar should reach the middle."
                        )),
                        (true, None) => note("Speak normally — the bar should reach the middle."),
                        (false, _) => View::Empty,
                    };
                    view! { {bar} {line} }
                }
            };
            view! {
                <FieldSlider model={self.mic_gain.clone()} min=0.0 max={f64::from(crate::resources::MAX_MIC_GAIN)} step=0.05 format={Some(gain_pct)} label="Input gain" id="a-gain" />
                {note("Lift a quiet headset. Aim for the middle of the bar while speaking.")}
                <div>{button}</div>
                {meter}
            }
        } else {
            error_line(if has_audio {
                "No microphone backend on this platform."
            } else {
                "This build has no audio support (built without the `audio` feature)."
            })
        };
        view! {
            <Section title="Output">
                {note("Play a short chime to confirm you can hear the application.")}
                <div><Button variant=Variant::Secondary icon={icon(icons::PLAY)} onclick={play} disabled={!has_audio}>"Sound test"</Button></div>
                {if has_audio { View::Empty } else { error_line("This build has no audio support.") }}
            </Section>
            <Section title="Levels">
                <FieldSlider model={self.effects_volume.clone()} min=0.0 max=1.0 step=0.01 format={Some(percent.clone())} label="Sound effects" id="a-effects" />
                {note("Footsteps, wind, landings, splashes and fountains, and the driving turn cues.")}
                <FieldSlider model={self.voice_volume.clone()} min=0.0 max=1.0 step=0.01 format={Some(percent)} label="Voice" id="a-voice" />
                {note("How loudly people nearby are heard, before distance is applied.")}
            </Section>
            <Section title="Microphone">
                {mic_block}
            </Section>
            <Section title="Camera">
                {note("The camera test is still egui's: it shows a live picture.")}
            </Section>
        }
    }

    /// Account (`settings::account_tab`): who you are here, and the one
    /// thing that cannot be undone, asked for twice with the password.
    fn account(&self) -> View {
        let (deleted, who, signed_in, error, password, armed) = (
            self.deleted.clone(),
            self.who.clone(),
            self.signed_in.clone(),
            self.delete_error.clone(),
            self.delete_password.clone(),
            self.delete_armed.clone(),
        );
        let fire = {
            let act = self.act.delete_account.clone();
            move || act.set(true)
        };
        let ready: Rc<dyn Fn() -> bool> = {
            let password = password.clone();
            Rc::new(move || !password.get().is_empty())
        };
        move || -> View {
            if deleted.get() {
                return view! {
                    {strong("Your account has been deleted.")}
                    {note("Nothing of it remains on the server except shared chat history, which no longer carries your name.")}
                };
            }
            let who_line: View = match who.get() {
                Some(name) => view! {
                    <p style="margin: 0"><span style="color: var(--m-text-dim); font-size: var(--m-font-size-sm)">"Signed in as "</span><span style="color: var(--m-text-strong); font-weight: 600">{name}</span></p>
                },
                None => note("You are not signed in. Offline, there is no account and nothing leaves this device."),
            };
            let can_delete = signed_in.get();
            let delete_block: View = if can_delete {
                let err: View = error
                    .get()
                    .map_or(View::Empty, error_line);
                view! {
                    {err}
                    <FieldText model={password.clone()} r#type=TextType::Password label="Type your password to confirm" id="acc-pw" autocomplete="current-password" />
                    <div>{confirm_button(armed.clone(), "Delete account…", "Delete permanently", ready.clone(), fire.clone())}</div>
                }
            } else {
                note("Sign in to the world holding the account to delete it.")
            };
            view! {
                <Section title="Signed in">{who_line}</Section>
                <Section title="Delete this account">
                    {note("This cannot be undone. The account, its password and its block list are erased; anything you said in Global or in a room stays, with your name removed from it.")}
                    {delete_block}
                </Section>
            }
        }
        .into_view()
    }

    /// Privacy and safety (`settings::privacy_tab`, `contacts::privacy_body`):
    /// who may send you a request, who you blocked, and how to report.
    #[cfg(feature = "comms")]
    fn privacy(&self) -> View {
        let signed_in = self.signed_in.clone();
        let policies = opts(&[
            (RequestPolicy::Anyone, "Anyone — including by username"),
            (
                RequestPolicy::Near,
                "People you've been near — in the world in the last hour, or who share a contact with you",
            ),
            (
                RequestPolicy::LinkOnly,
                "Only with my link — nobody finds you by name or in the world",
            ),
        ]);
        let (policy, separate, notify, show_online, blocked, unblock) = (
            self.policy.clone(),
            self.separate_new_accounts.clone(),
            self.notify_requests.clone(),
            self.show_online.clone(),
            self.blocked.clone(),
            self.act.unblock.clone(),
        );
        let blocked_list = move || -> View {
            let names = blocked.get();
            if names.is_empty() {
                return note("You have not blocked anyone.");
            }
            let rows: Vec<View> = names
                .into_iter()
                .map(|name| {
                    let click = {
                        let (unblock, name) = (unblock.clone(), name.clone());
                        on(move || unblock.borrow_mut().push(name.clone()))
                    };
                    let end = view! { <Button variant=Variant::Ghost size=Size::Sm onclick={click}>"Unblock"</Button> };
                    view! { <ListItem icon={icon(icons::APPEARANCE)} end={end}>{name}</ListItem> }
                })
                .collect();
            view! { <List>{rows}</List> }
        };
        let safety = view! {
            <Section title="Safety">
                {note("Tap or right-click somebody's name in chat to block or report them. Blocking is immediate and lasts until you undo it; reports go to the people who run this world.")}
                <p style="margin: 0; font-size: var(--m-font-size-sm)"><span style="color: var(--m-text-dim)">"Contact: "</span><span style="color: var(--m-accent)">{crate::systems::settings::SAFETY_CONTACT}</span></p>
            </Section>
        };
        move || -> View {
            if !signed_in.get() {
                return view! {
                    {note("Signed out, nobody can message you and there is nobody to block.")}
                    {safety.clone()}
                };
            }
            view! {
                <Section title="Who can send you a request">
                    {note("Your contacts can always message you. Anyone with your link can always send a request.")}
                    <FieldRadio model={policy.clone()} options={policies.clone()} id="pr-policy" />
                </Section>
                <Section title="Requests">
                    <FieldCheckbox model={separate.clone()} label="Requests from accounts younger than a week wait in their own section" id="pr-separate" />
                    <FieldCheckbox model={notify.clone()} label="Say so when a request arrives (off: they wait quietly in the tray)" id="pr-notify" />
                </Section>
                <Section title="Contacts can see">
                    <FieldCheckbox model={show_online.clone()} label="That I'm online" id="pr-online" />
                    {note("Where your avatar is stays per chat and until a time you set, as before. Being a contact doesn't show it.")}
                </Section>
                <Section title="Blocked">{blocked_list.clone()}</Section>
                {safety.clone()}
            }
        }
        .into_view()
    }

    /// Access tokens (`access_tokens::page`): the list with Revoke, the
    /// form with the password asked again, the secret shown once.
    #[cfg(feature = "comms")]
    fn tokens(&self) -> View {
        let demo = self.tokens_demo.clone();
        let (signed_in, list, shown, busy, error, may_elevate, armed) = (
            self.signed_in.clone(),
            self.token_list.clone(),
            self.token_shown.clone(),
            self.token_busy.clone(),
            self.token_error.clone(),
            self.may_elevate.clone(),
            self.revoke_armed.clone(),
        );
        let (label, expiry, elevated, password) = (
            self.token_label.clone(),
            self.token_expiry.clone(),
            self.token_elevated.clone(),
            self.token_password.clone(),
        );
        let act = self.act.clone();
        let expiries = opts(&[
            (Expiry::Day, "1 day"),
            (Expiry::Month, "30 days"),
            (Expiry::Never, "Never"),
        ]);
        let shown_card = {
            let (shown, act) = (shown.clone(), act.clone());
            move || -> View {
                let Some((label, secret)) = shown.get() else {
                    return View::Empty;
                };
                let copy = {
                    let (act, secret) = (act.clone(), secret.clone());
                    on(move || *act.copy.borrow_mut() = Some(secret.clone()))
                };
                let done = {
                    let act = act.clone();
                    on(move || act.token_done.set(true))
                };
                let title = format!("{label} is ready. Copy it now: it is shown only this once.");
                view! {
                    <Alert tone=Tone::Success title={title}>
                        <code style="display: block; word-break: break-all; font-family: var(--m-font-mono); font-size: var(--m-font-size-sm)">{secret}</code>
                        <div style="display: flex; gap: 8px; margin-top: 8px">
                            <Button size=Size::Sm icon={icon(icons::COPY)} onclick={copy}>"Copy"</Button>
                            <Button size=Size::Sm onclick={done}>"Done"</Button>
                        </div>
                    </Alert>
                }
            }
        };
        let list_view = {
            let (list, armed, act) = (list.clone(), armed.clone(), act.clone());
            move || -> View {
                let now = now_s();
                match list.get() {
                    Some(Ok(tokens)) if !tokens.is_empty() => {
                        let rows: Vec<View> = tokens
                            .iter()
                            .map(|t| {
                                let expires = match t.expires_at {
                                    Some(at) if at <= now => "expired".to_owned(),
                                    Some(at) => format!("expires {}", access_tokens::day(at)),
                                    None => "never expires".to_owned(),
                                };
                                let used = t
                                    .last_used
                                    .map_or_else(|| "Never used".to_owned(), |at| access_tokens::ago(at, now));
                                let sub = format!("Made {} · {expires} · {used}", access_tokens::day(t.created_at));
                                let is_armed = armed.get().as_deref() == Some(t.id.as_str());
                                let click = {
                                    let (armed, act, id) = (armed.clone(), act.clone(), t.id.clone());
                                    on(move || {
                                        if armed.get().as_deref() == Some(id.as_str()) {
                                            armed.set(None);
                                            *act.revoke.borrow_mut() = Some(id.clone());
                                        } else {
                                            armed.set(Some(id.clone()));
                                        }
                                    })
                                };
                                let end: View = if is_armed {
                                    view! { <Button variant=Variant::Danger size=Size::Sm onclick={click}>"Revoke: it stops working now"</Button> }
                                } else {
                                    view! { <Button variant=Variant::Ghost size=Size::Sm onclick={click}>"Revoke…"</Button> }
                                };
                                let badge: View = if t.elevated {
                                    view! { <Badge tone=Tone::Warning>"admin"</Badge> }
                                } else {
                                    View::Empty
                                };
                                let title = t.label.clone();
                                view! {
                                    <ListItem icon={icon(icons::KEY)} sub={sub} end={end}>{title}" "{badge}</ListItem>
                                }
                            })
                            .collect();
                        view! { <List>{rows}</List> }
                    }
                    Some(Ok(_)) => note("You have no access tokens."),
                    Some(Err(e)) => note(format!("Could not load your tokens: {e}")),
                    None => {
                        view! { <div style="display: flex; align-items: center; gap: 8px"><Spinner size=Size::Sm />{note("Loading…")}</div> }
                    }
                }
            }
        };
        let make = {
            let act = act.clone();
            on(move || act.make_token.set(true))
        };
        let form_foot = {
            let (label, password, busy) = (label.clone(), password.clone(), busy.clone());
            let make = make.clone();
            move || -> View {
                let ready =
                    !label.get().trim().is_empty() && !password.get().is_empty() && !busy.get();
                let why = if busy.get() {
                    "Making it…"
                } else {
                    "Give it a name and your password"
                };
                view! {
                    <div><Button variant=Variant::Primary onclick={make.clone()} disabled={!ready}>"Make token"</Button></div>
                    {if ready { View::Empty } else { note(why) }}
                }
            }
        };
        let elevate_row = {
            let (may, elevated) = (may_elevate.clone(), elevated.clone());
            move || -> View {
                if may.get() {
                    view! { <FieldCheckbox model={elevated.clone()} label="Admin rights too: it may do what your admin roles allow" id="tk-admin" /> }
                } else {
                    View::Empty
                }
            }
        };
        let error_view = {
            let error = error.clone();
            move || -> View { error.get().map_or(View::Empty, error_line) }
        };
        move || -> View {
            if !signed_in.get() && !demo.get() {
                return note("Sign in to make or revoke access tokens.");
            }
            view! {
                {note("What a bot or a script signs in with instead of your password. Revoking one stops it at once, also a bot already in the world.")}
                {shown_card.clone()}
                {error_view.clone()}
                {list_view.clone()}
                <Section title="New token">
                    <FieldText model={label.clone()} label="Name" placeholder="Name, e.g. \"deck bot\"" id="tk-name" />
                    <FieldSegmented model={expiry.clone()} options={expiries.clone()} label="Expires" id="tk-expiry" />
                    {elevate_row.clone()}
                    <FieldText model={password.clone()} r#type=TextType::Password label="Your password, to confirm" id="tk-pw" autocomplete="current-password" />
                    {form_foot.clone()}
                </Section>
            }
        }
        .into_view()
    }
}

//! The sign-in card drawn by the component library (`lib_host`): which
//! server, as whom, a modal over the running world.
//!
//! `login_screen::drive_login_screen` keeps what is not the card's — closing
//! it on a live session (`settle`), saying it owns the screen
//! (`LoginScreen::modal`), and collecting a sign-in the browser finished
//! (`external_signin`). Everything a press here asks for goes through the
//! `LoginScreen` methods (`submit`, `dismiss`, `choose_external`, `missing`).
//!
//! **Placement.** A dimmed full-window backdrop with the card centred, at
//! most [`CARD_WIDTH`] wide with 20 px kept clear on every side. The
//! backdrop is four click-to-dismiss spacers *around* the card, not a parent
//! of it: as DOM a click on the card would bubble into a backdrop it sat in.
//!
//! **Not done yet.** No fade-and-rise on opening; the mark is left out and
//! the wordmark is text; the card does not move into the band an on-screen
//! keyboard leaves, and the app's own on-screen keyboard does not open for
//! these fields; the providers' buttons carry the name's first letter.

use std::cell::RefCell;
use std::rc::Rc;

use bevy::ecs::system::NonSendMarker;
use bevy::prelude::*;
// The library's, not Bevy's: an explicit import outranks the two globs.
use mithril::prelude::*;
use mithril::prelude::{Button, Event};

use crate::resources::AuthIdentity;
use crate::systems::external_signin::Offered;
use crate::systems::lib_settings::LibScreens;
use crate::systems::login_screen::{status_of, LoginScreen, StatusDot};
use crate::systems::session::{SessionState, SessionStatus};
use crate::systems::splash::BootSplash;
use crate::systems::theme;
use crate::systems::ui_input::UiMetrics;
use crate::systems::user_store::UserStore;

/// The card's name in the library's host.
const KEY: &str = "login";

/// Width of the card, px, before the screen narrows it.
const CARD_WIDTH: f32 = 380.0;

/// Kept clear around the card, px, per side.
const EDGE: f32 = 20.0;

/// The card's size on this viewport.
#[derive(Clone, Copy, PartialEq, Default)]
struct Frame {
    width: f32,
    max_height: f32,
}

impl Frame {
    /// For a viewport of `w` × `h` UI points; as DOM the page's px are
    /// `ui_scale` times larger (`lib_place_card::Placement`).
    fn of(w: f32, h: f32, ui_scale: f32) -> Self {
        let k = if cfg!(all(feature = "ui-dom", target_arch = "wasm32")) {
            ui_scale
        } else {
            1.0
        };
        Frame {
            width: CARD_WIDTH.min(w - 2.0 * EDGE).max(160.0) * k,
            max_height: (h - 2.0 * EDGE).max(160.0) * k,
        }
    }
}

/// The form's fields, as last mirrored: a field whose signal moved from
/// this was typed in; one whose `LoginScreen` value moved was set by the
/// card's logic (the password taken by a submit, a name the browser sent).
#[derive(Clone, PartialEq, Default)]
struct Form {
    server: String,
    username: String,
    password: String,
    remember: bool,
}

impl Form {
    fn of(s: &LoginScreen) -> Self {
        Form {
            server: s.server.clone(),
            username: s.username.clone(),
            password: s.password.clone(),
            remember: s.remember,
        }
    }
}

/// What the card is about: the form, or a sign-in out in the browser.
#[derive(Clone, PartialEq)]
enum Mode {
    Form,
    /// The pairing code is on its way.
    Opening,
    /// The browser is out: the provider's name and the code to compare.
    Waiting {
        provider: String,
        code: String,
    },
}

impl Mode {
    fn of(s: &LoginScreen) -> Self {
        match &s.external.pending {
            Some(p) => Mode::Waiting {
                provider: p.provider.name.clone(),
                code: p.code.clone(),
            },
            None if s.external.busy() => Mode::Opening,
            None => Mode::Form,
        }
    }
}

/// Everything else the card shows, compared from frame to frame.
#[derive(Clone, PartialEq, Default)]
struct State {
    /// The status line: a handshake under way, or the world you are in.
    status: Option<(StatusDot, String)>,
    busy: bool,
    busy_label: &'static str,
    ready: bool,
    missing: Option<&'static str>,
    error: Option<String>,
    offered: Vec<Offered>,
    external_error: Option<String>,
    register: bool,
    /// The server, named at the foot of the card.
    server: Option<String>,
}

impl State {
    fn of(s: &LoginScreen, session: &SessionState, identity: &AuthIdentity) -> Self {
        let busy = session.status.is_busy();
        State {
            status: (!matches!(session.status, SessionStatus::Offline))
                .then(|| status_of(session, identity)),
            busy,
            busy_label: session.status.label(),
            ready: s.answerable(),
            missing: s.missing(),
            error: session.error.clone(),
            offered: s.external.offered.clone(),
            external_error: s.external.error.clone(),
            register: !busy && s.register_url().is_some(),
            server: s.server_shown(),
        }
    }
}

/// What a press asked for; done by the system.
#[derive(Clone, Copy)]
enum Act {
    Submit,
    Close,
    Provider(usize),
    Reopen,
    Cancel,
    Register,
    ChangeServer,
}

#[derive(Clone)]
struct Live {
    server: Signal<String>,
    username: Signal<String>,
    password: Signal<String>,
    remember: Signal<bool>,
    /// The fields as mirrored at the end of the last frame.
    last: Rc<RefCell<Form>>,
    mode: Signal<Mode>,
    editing: Signal<bool>,
    state: Signal<State>,
    frame: Signal<Frame>,
    acts: Rc<RefCell<Vec<Act>>>,
}

impl Live {
    fn on(&self, a: Act) -> Handler {
        let acts = self.acts.clone();
        Rc::new(move |_: &Event| acts.borrow_mut().push(a)) as Handler
    }

    fn act(&self, a: Act) -> Option<Handler> {
        Some(self.on(a))
    }

    fn typed(&self) -> Form {
        Form {
            server: self.server.get_untracked(),
            username: self.username.get_untracked(),
            password: self.password.get_untracked(),
            remember: self.remember.get_untracked(),
        }
    }
}

thread_local! {
    static MOUNTED: RefCell<Option<Live>> = const { RefCell::new(None) };
}

/// Mounts, bridges and unmounts the sign-in card. `PostUpdate`, before
/// `DriveUi`, as every library screen — so after the session driver has
/// taken any request, which is what lets `awaiting` go here.
#[allow(clippy::too_many_arguments)]
pub fn drive_lib_login(
    _main: NonSendMarker,
    lib: Res<LibScreens>,
    mut screen: ResMut<LoginScreen>,
    mut session: ResMut<SessionState>,
    mut store: ResMut<UserStore>,
    identity: Res<AuthIdentity>,
    splash: Res<BootSplash>,
    metrics: Res<UiMetrics>,
) {
    if !lib.login {
        return;
    }
    // A request that came back offline is no longer awaited. Before this
    // frame's presses: one made now has not reached the driver yet.
    if screen.is_busy() && !session.status.is_busy() {
        screen.settle_awaiting(&session);
    }
    // Wait behind the boot splash (`LoginScreen::modal` says why).
    let open = screen.open && !splash.covering();
    let size = metrics.viewport_pts();
    let frame = Frame::of(size.x, size.y, metrics.ui_scale());
    MOUNTED.with(|m| {
        let mut m = m.borrow_mut();
        if let Some(live) = m.as_ref() {
            // What was typed since the last frame → the screen.
            let typed = live.typed();
            let last = live.last.borrow().clone();
            if typed != last {
                if typed.server != last.server {
                    screen.server = typed.server;
                }
                if typed.username != last.username {
                    screen.username = typed.username;
                }
                if typed.password != last.password {
                    screen.password = typed.password;
                }
                if typed.remember != last.remember {
                    screen.remember = typed.remember;
                }
            }
            // The presses of the frame.
            let acts: Vec<Act> = std::mem::take(&mut *live.acts.borrow_mut());
            for a in acts {
                act(a, &mut screen, &mut session, &mut store);
            }
        }
        if !open {
            if m.take().is_some() {
                crate::systems::lib_host::clear_keyed(KEY);
            }
            return;
        }
        // A card with no server yet asks for one in a field that stays a
        // field while it is typed into.
        if screen.server_shown().is_none() && !screen.edit_server {
            screen.edit_server = true;
        }
        let form = Form::of(&screen);
        let mode = Mode::of(&screen);
        let state = State::of(&screen, &session, &identity);
        match m.as_mut() {
            None => {
                let live = Live {
                    server: signal(form.server.clone()),
                    username: signal(form.username.clone()),
                    password: signal(form.password.clone()),
                    remember: signal(form.remember),
                    last: Rc::new(RefCell::new(form)),
                    mode: signal(mode),
                    editing: signal(screen.edit_server),
                    state: signal(state),
                    frame: signal(frame),
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
                        warn!(?path, error = %e, "lib login: could not write the HTML");
                    }
                }
                *m = Some(live);
            }
            Some(live) => {
                // The screen moved under the fields → the fields.
                if live.server.get_untracked() != form.server {
                    live.server.set(form.server.clone());
                }
                if live.username.get_untracked() != form.username {
                    live.username.set(form.username.clone());
                }
                if live.password.get_untracked() != form.password {
                    live.password.set(form.password.clone());
                }
                if live.remember.get_untracked() != form.remember {
                    live.remember.set(form.remember);
                }
                *live.last.borrow_mut() = form;
                if live.mode.get_untracked() != mode {
                    live.mode.set(mode);
                }
                if live.editing.get_untracked() != screen.edit_server {
                    live.editing.set(screen.edit_server);
                }
                if live.state.get_untracked() != state {
                    live.state.set(state);
                }
                if live.frame.get_untracked() != frame {
                    live.frame.set(frame);
                }
            }
        }
    });
}

/// One press, done through the card's own methods.
fn act(a: Act, screen: &mut LoginScreen, session: &mut SessionState, store: &mut UserStore) {
    match a {
        Act::Submit => {
            screen.submit(store, session);
        }
        Act::Close => screen.dismiss(session),
        Act::Provider(i) => {
            if session.status.is_busy() {
                return;
            }
            if let Some(p) = screen.external.offered.get(i).cloned() {
                screen.choose_external(p);
            }
        }
        Act::Reopen => screen.external.reopen(),
        Act::Cancel => screen.external.cancel(),
        Act::Register => {
            if let Some(url) = screen.register_url() {
                crate::systems::data_layers::open_url(&url);
            }
        }
        Act::ChangeServer => screen.edit_server = true,
    }
}

/// The card's HTML from the library's string renderer, for the browser
/// beside a capture.
fn html(l: &Live) -> String {
    use mithril::view::{mount, StringRenderer};
    let r = Rc::new(RefCell::new(StringRenderer::new()));
    let root = r.borrow().root();
    let mounted = mount(view_of(l), &r, &root, None);
    let out = r.borrow().html();
    drop(mounted);
    out
}

/// A dim, small line of text.
fn note(text: String) -> View {
    view! {
        <p style="margin: 0; color: var(--m-text-dim); font-size: var(--m-font-size-sm)">{text}</p>
    }
}

/// A spinner beside a dim line.
fn busy_line(text: String) -> View {
    view! {
        <div style="display: flex; align-items: center; gap: 8px; color: var(--m-text-dim); font-size: var(--m-font-size-sm)">
            <Spinner size=Size::Sm />{text}
        </div>
    }
}

/// A part of the backdrop: a tap on it closes the card.
fn spacer(style: &'static str, on: Handler) -> View {
    view! {
        <div style={style} on:click={move |e: &Event| on(e)}></div>
    }
}

/// The form: account name, password, staying signed in, Sign in, and what
/// goes under it.
fn form(l: &Live) -> View {
    let enter: Handler = {
        let acts = l.acts.clone();
        Rc::new(move |e: &Event| {
            if e.key.as_deref() == Some("Enter") {
                acts.borrow_mut().push(Act::Submit);
            }
        })
    };
    let sign_in = {
        let l = l.clone();
        move || -> View {
            let s = l.state.get();
            let label = if s.busy { s.busy_label } else { "Sign in" };
            view! {
                <div style="display: flex; flex-direction: column">
                    <Button variant=Variant::Primary size=Size::Lg loading={s.busy} disabled={!s.ready} onclick={l.act(Act::Submit)}>{label}</Button>
                </div>
            }
        }
    };
    // Under the button: the handshake, the field still unanswered, or why
    // the last attempt failed.
    let under = {
        let l = l.clone();
        move || -> View {
            let s = l.state.get();
            if s.busy {
                busy_line(s.busy_label.to_string())
            } else if let Some(missing) = s.missing {
                note(missing.to_string())
            } else if let Some(error) = s.error {
                view! {
                    <p style="margin: 0; color: var(--m-danger); font-size: var(--m-font-size-sm); font-weight: 600">{error}</p>
                }
            } else {
                View::Empty
            }
        }
    };
    // The providers this server offers, behind "or with".
    let others = {
        let l = l.clone();
        move || -> View {
            let s = l.state.get();
            if s.offered.is_empty() {
                return View::Empty;
            }
            let buttons: Vec<View> = s
                .offered
                .iter()
                .enumerate()
                .map(|(i, p)| {
                    // The name's first letter stands in for the provider's
                    // mark: the app's fonts carry none of them.
                    let mark: String = p.name.chars().take(1).collect();
                    let name = p.name.clone();
                    let click = if s.busy { None } else { l.act(Act::Provider(i)) };
                    let mark: View = view! { <span style="font-weight: 700">{mark}</span> };
                    view! {
                        <Button variant=Variant::Secondary disabled={s.busy} onclick={click} icon={mark}>{name}</Button>
                    }
                })
                .collect();
            let error: View = match s.external_error {
                Some(e) => view! {
                    <p style="margin: 0; color: var(--m-danger); font-size: var(--m-font-size-sm)">{e}</p>
                },
                None => View::Empty,
            };
            view! {
                <div style="display: flex; align-items: center; gap: 12px; color: var(--m-text-faint); font-size: var(--m-font-size-sm)">
                    <div style="flex: 1; height: 1px; background: var(--m-border)"></div>
                    "or with"
                    <div style="flex: 1; height: 1px; background: var(--m-border)"></div>
                </div>
                <div style="display: flex; flex-direction: column; gap: 8px">{buttons}</div>
                {error}
            }
        }
    };
    // A way in for somebody with no account: the server's own page.
    let register = {
        let l = l.clone();
        move || -> View {
            if !l.state.get().register {
                return View::Empty;
            }
            view! {
                <div style="display: flex; align-items: center; gap: 8px; color: var(--m-text-dim); font-size: var(--m-font-size-sm)">
                    "New here?"
                    <Button variant=Variant::Ghost size=Size::Sm onclick={l.act(Act::Register)}>"Create an account"</Button>
                </div>
            }
        }
    };
    // The server, last: a fact with a way to change it, or the field.
    let server = {
        let l = l.clone();
        move || -> View {
            if l.editing.get() {
                let enter: Handler = {
                    let acts = l.acts.clone();
                    Rc::new(move |e: &Event| {
                        if e.key.as_deref() == Some("Enter") {
                            acts.borrow_mut().push(Act::Submit);
                        }
                    })
                };
                return view! {
                    <FieldText model={l.server.clone()} label="Server" placeholder="e.g. cartopolis.org" autocomplete="url" onkeydown={Some(enter)} id="signin-server" />
                };
            }
            let named = {
                let l = l.clone();
                move || -> View {
                    let name = l.state.get().server.unwrap_or_default();
                    view! {
                        <span style="flex: 1; min-width: 0; overflow: hidden; white-space: nowrap; text-overflow: ellipsis">{format!("Server: {name}")}</span>
                    }
                }
            };
            view! {
                <div style="display: flex; align-items: center; gap: 8px; color: var(--m-text-dim); font-size: var(--m-font-size-sm)">
                    {named}
                    <Button variant=Variant::Ghost size=Size::Sm onclick={l.act(Act::ChangeServer)}>"Change"</Button>
                </div>
            }
        }
    };
    let enter_password = enter.clone();
    view! {
        <FieldText model={l.username.clone()} label="Account name" placeholder="your account name" autocomplete="username" onkeydown={Some(enter)} id="signin-user" />
        <FieldText model={l.password.clone()} r#type=TextType::Password label="Password" autocomplete="current-password" onkeydown={Some(enter_password)} id="signin-password" />
        <FieldSwitch model={l.remember.clone()} label="Stay signed in on this device" id="signin-remember" />
        {sign_in}
        {under}
        {others}
        {register}
        {server}
    }
}

/// The card while the browser is signing in: the code to compare, a way
/// back to the browser, and a way out.
fn waiting(l: &Live, provider: Option<(String, String)>) -> View {
    let Some((provider, code)) = provider else {
        // The pairing code is still on its way.
        return busy_line("Opening your browser…".to_string());
    };
    view! {
        <div style="display: flex; flex-direction: column; gap: 8px">
            <p style="margin: 0; font-weight: 600; color: var(--m-text-strong)">"Finish in your browser"</p>
            {note(format!("We opened {provider}'s sign-in in your browser. When it asks for the app's code, type this one, then come back here:"))}
        </div>
        <p style="margin: 0; text-align: center; font-family: var(--m-font-mono); font-size: 26px; letter-spacing: 0.08em; color: var(--m-text-strong)">{code}</p>
        {busy_line(format!("Waiting for {provider}…"))}
        <div style="display: flex; flex-direction: column; gap: 8px">
            <Button variant=Variant::Secondary onclick={l.act(Act::Reopen)}>"Open the browser again"</Button>
            <Button variant=Variant::Ghost onclick={l.act(Act::Cancel)}>"Cancel"</Button>
        </div>
    }
}

/// The backdrop and the card: the wordmark and ✕, "Sign in", the status
/// line, and the form or the browser's round trip.
fn view_of(l: &Live) -> View {
    let card = {
        let frame = l.frame.clone();
        move || {
            let f = frame.get();
            format!(
                "flex: none; width: {:.0}px; max-height: {:.0}px; overflow-y: auto; \
                 display: flex; flex-direction: column; gap: 16px; padding: 20px; \
                 background: var(--m-surface); border: 1px solid var(--m-border); \
                 border-radius: var(--m-radius-lg); box-shadow: var(--m-shadow-lg)",
                f.width, f.max_height
            )
        }
    };
    // No way out while a connection is in flight: closing the card would
    // hide the only place the outcome is reported.
    let close = {
        let l = l.clone();
        move || -> View {
            if l.state.get().busy {
                view! { <div style="opacity: 0.4"><CloseButton /></div> }
            } else {
                view! { <CloseButton onclick={l.act(Act::Close)} /> }
            }
        }
    };
    let status = {
        let l = l.clone();
        move || -> View {
            let Some((dot, text)) = l.state.get().status else {
                return View::Empty;
            };
            let colour = match dot {
                StatusDot::Online => "var(--m-success)",
                StatusDot::Offline => "var(--m-text-dim)",
                StatusDot::Busy => "var(--m-warning)",
            };
            let dot = format!(
                "flex: none; width: 8px; height: 8px; border-radius: var(--m-radius-full); background: {colour}"
            );
            view! {
                <div style="display: flex; align-items: center; gap: 8px; font-size: var(--m-font-size-sm); color: var(--m-text)">
                    <div style={dot}></div>{text}
                </div>
            }
        }
    };
    // While the browser is out, the card is about that and nothing else:
    // the password form under it would be a second, contradictory question.
    let body = {
        let l = l.clone();
        move || -> View {
            match l.mode.get() {
                Mode::Form => form(&l),
                Mode::Opening => waiting(&l, None),
                Mode::Waiting { provider, code } => waiting(&l, Some((provider, code))),
            }
        }
    };
    let head = theme::WORDMARK_HEAD;
    let tail = theme::WORDMARK_TAIL;
    view! {
        <div style="position: absolute; left: 0; right: 0; top: 0; bottom: 0; display: flex; flex-direction: column; background: var(--m-backdrop)">
            {spacer("flex: 1; min-height: 20px", l.on(Act::Close))}
            <div style="flex: none; display: flex">
                {spacer("flex: 1; min-width: 20px", l.on(Act::Close))}
                <div style={card} on:click={|_: &Event| {}}>
                    <div style="display: flex; align-items: center; gap: 12px">
                        <span style="flex: 1; min-width: 0; font-size: 26px; font-weight: 700; color: var(--m-text-strong)">{head}<span style="color: var(--m-accent)">{tail}</span></span>
                        {close}
                    </div>
                    <div style="display: flex; flex-direction: column; gap: 8px">
                        <h1 style="margin: 0; font-size: 19px; font-weight: 600; color: var(--m-text-strong)">"Sign in"</h1>
                        {status}
                    </div>
                    {body}
                </div>
                {spacer("flex: 1; min-width: 20px", l.on(Act::Close))}
            </div>
            {spacer("flex: 1; min-height: 20px", l.on(Act::Close))}
        </div>
    }
}

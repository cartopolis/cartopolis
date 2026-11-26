//! The Admin pages drawn by the component library (`lib_host`): the home
//! with the areas this account may open, the report queue and one report,
//! the accounts and one account, the roles and the building claims.
//!
//! `&panel=admin` on the web opens it. `admin::drive_admin` drains the HTTP
//! replies, polls the open reports for the You page's badge and closes the
//! panel for an account that may not run anything. Every change — mark
//! handled, suspend, roles, delete, release — goes through `AdminState`'s
//! methods over the operator API (`/api/admin/*`); the server checks the
//! permission on every call.
//!
//! **State.** The lists are mirrored into signals each frame, and only when
//! they differ; the two text fields (the account search, what was done about
//! a report) and the Open | Handled switch are diffed back into
//! `AdminState`; a tap is an [`Act`] the system does where the resources are.
//! Each part of a page that reads a list is its own reactive closure, so a
//! reply landing never re-mounts a field being typed in.

use std::cell::RefCell;
use std::rc::Rc;

use bevy::ecs::system::NonSendMarker;
use bevy::prelude::*;
// The library's, not Bevy's: an explicit import outranks the two globs.
use mithril::prelude::*;
use mithril::prelude::{Button, Event};

use crate::resources::{AuthIdentity, RightPanel};
use crate::systems::admin::{
    self, Account, AccountsDoc, AdminState, Claim, Load, Offers, Page, Report, Role,
};
use crate::systems::chats::ChatsView;
use crate::systems::lib_settings::LibScreens;
use crate::systems::ui_input::UiMetrics;
use crate::systems::{icons, theme};

/// What a tap asked for; done by the system, against the state as it is then.
#[derive(Clone)]
enum Act {
    /// Open a page, fetching what it shows (`AdminState::open`).
    Open(Page),
    /// Go to a page as it is (a report's person → their account).
    Go(Page),
    /// The header's arrow: up a page, or out to the You page from the home.
    Back,
    /// Content and Data are on the web page.
    Web,
    MarkHandled(i64),
    /// An account id, and whether it is to be stopped.
    Suspend(String, bool),
    /// An account id, a role id, and whether it is to be held.
    SetRole(String, String, bool),
    Delete(String),
    /// A claimed building's `pand_id`.
    Release(String),
    /// From an account to the open reports.
    ReportsOf,
    DismissNotice,
    Close,
}

/// The home page's two live numbers.
#[derive(Clone, PartialEq, Default)]
struct HomeInfo {
    open_reports: usize,
    accounts_summary: String,
}

#[derive(Clone)]
struct Live {
    page: Signal<Page>,
    offers: Signal<Offers>,
    home: Signal<HomeInfo>,
    notice: Signal<Option<(String, bool)>>,
    reports: Signal<Load<Vec<Report>>>,
    accounts: Signal<Load<AccountsDoc>>,
    roles: Signal<Load<Vec<Role>>>,
    claims: Signal<Load<Vec<Claim>>>,
    /// Whether the open report is handled (`None`: no report page). Its own
    /// signal, so the field under it is mounted once per report.
    resolved: Signal<Option<bool>>,
    search: Signal<String>,
    resolution: Signal<String>,
    /// Open | Handled on the Reports page.
    handled: Signal<bool>,
    /// The destructive button pressed once, by key (`suspend:<id>`,
    /// `delete:<id>`, `release:<pand>`): the second press does it.
    armed: Signal<Option<String>>,
    acts: Rc<RefCell<Vec<Act>>>,
    /// Laid out for a phone (the HTML written beside a capture needs it).
    compact: bool,
}

impl Live {
    fn act(&self, a: Act) -> Option<Handler> {
        let acts = self.acts.clone();
        Some(Rc::new(move |_: &Event| acts.borrow_mut().push(a.clone())) as Handler)
    }
}

thread_local! {
    static MOUNTED: RefCell<Option<Live>> = const { RefCell::new(None) };
}

/// Whether the report the page shows is handled; `None` off a report page.
fn resolved_of(admin: &AdminState) -> Option<bool> {
    match admin.page {
        Page::Report(id) => admin.report(id).map(|r| r.resolved),
        _ => None,
    }
}

fn home_of(admin: &AdminState) -> HomeInfo {
    HomeInfo {
        open_reports: admin.open_reports,
        accounts_summary: admin.accounts_summary(),
    }
}

/// Mounts, bridges and unmounts the Admin panel. `PostUpdate`, before
/// `DriveUi`, as every library screen.
#[allow(clippy::too_many_arguments)]
pub fn drive_lib_admin(
    _main: NonSendMarker,
    lib: Res<LibScreens>,
    mut right_panel: ResMut<RightPanel>,
    mut admin: ResMut<AdminState>,
    identity: Res<AuthIdentity>,
    mut view: ResMut<ChatsView>,
    metrics: Res<UiMetrics>,
    mut opened: Local<bool>,
) {
    if !lib.admin {
        return;
    }
    // Asked to open on Admin (`?panel=admin` on the web): once.
    // `admin::drive_admin` closes it again for an account that may not run anything.
    if lib.open_admin && !*opened {
        *opened = true;
        *right_panel = RightPanel::Admin;
    }
    let open = right_panel.is(RightPanel::Admin);
    MOUNTED.with(|m| {
        let mut m = m.borrow_mut();
        match (open, m.as_mut()) {
            (false, None) => {}
            (false, Some(_)) => {
                *m = None;
                crate::systems::lib_host::clear_keyed("admin");
            }
            (true, None) => {
                admin.settle();
                let live = Live {
                    page: signal(admin.page.clone()),
                    offers: signal(Offers::of(&identity, admin.demo)),
                    home: signal(home_of(&admin)),
                    notice: signal(admin.notice.clone()),
                    reports: signal(admin.reports.clone()),
                    accounts: signal(admin.accounts.clone()),
                    roles: signal(admin.roles.clone()),
                    claims: signal(admin.claims.clone()),
                    resolved: signal(resolved_of(&admin)),
                    search: signal(admin.search.clone()),
                    resolution: signal(admin.resolution.clone()),
                    handled: signal(admin.show_handled),
                    armed: signal(None),
                    acts: Rc::new(RefCell::new(Vec::new())),
                    compact: metrics.compact(),
                };
                let p = theme::p();
                crate::systems::lib_host::mount_keyed(
                    "admin",
                    panel(&live),
                    p.ink == theme::DARK.ink,
                    theme::touch_layout(),
                );
                if let Some(path) = std::env::var_os("CARTO_UI_LIB_HTML") {
                    if let Err(e) = std::fs::write(&path, html(&live)) {
                        warn!(?path, error = %e, "lib admin: could not write the HTML");
                    }
                }
                *m = Some(live);
            }
            (true, Some(live)) => {
                // The fields → the state, as egui's text edits write it.
                let typed = live.search.get_untracked();
                if typed != admin.search {
                    admin.search = typed;
                }
                let typed = live.resolution.get_untracked();
                if typed != admin.resolution {
                    admin.resolution = typed;
                }
                let handled = live.handled.get_untracked();
                if handled != admin.show_handled {
                    admin.show_handled = handled;
                }
                // The taps of the frame.
                let acts: Vec<Act> = std::mem::take(&mut *live.acts.borrow_mut());
                for a in acts {
                    act(a, &mut admin, &mut right_panel, &mut view);
                }
                admin.settle();
                // The state → the signals, each only when it moved.
                macro_rules! mirror {
                    ($sig:expr, $now:expr) => {{
                        let now = $now;
                        if $sig.get_untracked() != now {
                            $sig.set(now);
                        }
                    }};
                }
                mirror!(live.search, admin.search.clone());
                mirror!(live.resolution, admin.resolution.clone());
                mirror!(live.handled, admin.show_handled);
                if live.page.get_untracked() != admin.page {
                    live.armed.set(None);
                    live.page.set(admin.page.clone());
                }
                mirror!(live.offers, Offers::of(&identity, admin.demo));
                mirror!(live.home, home_of(&admin));
                mirror!(live.notice, admin.notice.clone());
                mirror!(live.reports, admin.reports.clone());
                mirror!(live.accounts, admin.accounts.clone());
                mirror!(live.roles, admin.roles.clone());
                mirror!(live.claims, admin.claims.clone());
                mirror!(live.resolved, resolved_of(&admin));
            }
        }
    });
}

/// One tap, done.
fn act(a: Act, admin: &mut AdminState, right_panel: &mut RightPanel, view: &mut ChatsView) {
    match a {
        Act::Open(page) => admin.open(page),
        Act::Go(page) => admin.page = page,
        Act::Back => {
            if admin.page == Page::Home {
                admin::to_you(right_panel, view);
            } else {
                admin.go_back();
            }
        }
        Act::Web => admin::open_web_admin(),
        Act::MarkHandled(id) => admin.mark_handled(id),
        Act::Suspend(id, off) => {
            if let Some(a) = admin.account(&id).cloned() {
                admin.suspend(&a, off);
            }
        }
        Act::SetRole(id, role, on) => {
            if let Some(a) = admin.account(&id).cloned() {
                admin.set_role(&a, &role, on);
            }
        }
        Act::Delete(id) => {
            if let Some(a) = admin.account(&id).cloned() {
                admin.delete_account(&a);
            }
        }
        Act::Release(pand) => {
            let claim = admin
                .claims
                .ready()
                .and_then(|l| l.iter().find(|c| c.pand_id == pand))
                .cloned();
            if let Some(c) = claim {
                admin.release_claim(&c);
            }
        }
        Act::ReportsOf => admin.reports_of(),
        Act::DismissNotice => admin.notice = None,
        Act::Close => *right_panel = RightPanel::None,
    }
}

/// The panel's HTML from the library's string renderer, for the browser
/// beside the capture.
fn html(l: &Live) -> String {
    use mithril::view::{mount, StringRenderer};
    let r = Rc::new(RefCell::new(StringRenderer::new()));
    let root = r.borrow().root();
    let mounted = mount(panel(l), &r, &root, None);
    let out = r.borrow().html();
    drop(mounted);
    out
}

// ─── The view ────────────────────────────────────────────────────────────────

/// A glyph of the app's icon font.
fn icon(glyph: &'static str) -> View {
    view! {
        <span style="font-family: 'Material Symbols Rounded'; font-size: 20px; line-height: 1">{glyph}</span>
    }
}

/// A line of explanation (`theme::note`).
fn note(text: impl Into<String>) -> View {
    let text: String = text.into();
    view! {
        <p style="margin: 0; color: var(--m-text-dim); font-size: var(--m-font-size-sm); font-style: italic; line-height: 1.4">{text}</p>
    }
}

/// A section's title in admin's gold (`theme::section_tinted`).
fn gold(text: &'static str) -> View {
    view! { <span style="color: var(--m-warning)">{text}</span> }
}

/// A row's end word ("web page", "suspended").
fn word(text: impl Into<String>) -> View {
    let text: String = text.into();
    view! {
        <span style="white-space: nowrap; color: var(--m-text-dim); font-size: var(--m-font-size-sm)">{text}</span>
    }
}

/// A page still loading, or one that could not be: `None` once it is here.
fn not_ready<T>(load: &Load<T>) -> Option<View> {
    match load {
        Load::Ready(_) => None,
        Load::Idle | Load::Loading => Some(view! {
            <div style="display: flex; align-items: center; gap: 8px; color: var(--m-text-dim); font-size: var(--m-font-size-sm)">
                <Spinner size=Size::Sm />"Loading…"
            </div>
        }),
        Load::Failed(e) => Some(note(format!("Could not load this: {e}"))),
    }
}

/// A button that asks twice (`theme::confirm_button`): the first press arms
/// it into the danger tone, the second does it. One is armed at a time.
fn confirm(l: &Live, key: String, label: String, confirm_label: &'static str, a: Act) -> View {
    let is_armed = l.armed.get().as_deref() == Some(key.as_str());
    let click: Handler = {
        let (armed, acts) = (l.armed.clone(), l.acts.clone());
        Rc::new(move |_: &Event| {
            if armed.get_untracked().as_deref() == Some(key.as_str()) {
                armed.set(None);
                acts.borrow_mut().push(a.clone());
            } else {
                armed.set(Some(key.clone()));
            }
        })
    };
    if is_armed {
        view! { <Button variant=Variant::Danger size=Size::Sm onclick={Some(click)}>{confirm_label}</Button> }
    } else {
        view! { <Button variant=Variant::Secondary size=Size::Sm onclick={Some(click)}>{label}</Button> }
    }
}

/// The panel: a header with the way back and out, the last change's
/// outcome, then the page, scrolling.
fn panel(l: &Live) -> View {
    let frame = if l.compact {
        "position: absolute; left: 0; right: 0; top: 0; bottom: 0; display: flex; flex-direction: column; background: var(--m-bg)".to_string()
    } else {
        format!(
            "position: absolute; right: 0; top: 0; bottom: 0; width: {}px; display: flex; flex-direction: column; background: var(--m-bg); border-left: 1px solid var(--m-border)",
            admin::PANEL_WIDTH
        )
    };
    let header = {
        let l = l.clone();
        move || -> View { header(&l) }
    };
    let notice = {
        let l = l.clone();
        move || -> View {
            let Some((text, failed)) = l.notice.get() else {
                return View::Empty;
            };
            let acts = l.acts.clone();
            let dismiss: Rc<dyn Fn()> = Rc::new(move || acts.borrow_mut().push(Act::DismissNotice));
            if failed {
                view! { <Alert tone=Tone::Danger ondismiss={Some(dismiss)}>{text}</Alert> }
            } else {
                view! { <Alert tone=Tone::Success ondismiss={Some(dismiss)}>{text}</Alert> }
            }
        }
    };
    let body = {
        let l = l.clone();
        move || -> View {
            match l.page.get() {
                Page::Home => home(&l),
                Page::Reports => reports(&l),
                Page::Report(id) => report(&l, id),
                Page::Accounts => accounts(&l),
                Page::Account(id) => account(&l, id),
                Page::Roles => roles(&l),
                Page::Claims => claims(&l),
            }
        }
    };
    view! {
        <div style={frame}>
            {header}
            <div style="flex: 1; min-height: 0; overflow-y: auto; padding: 20px 20px 28px; display: flex; flex-direction: column; gap: 20px">
                {notice}
                {body}
            </div>
        </div>
    }
}

/// Every page's header carries the back arrow (to the You page from the
/// home, up a page elsewhere) and the panel's ✕, as egui's `back_title`.
fn header(l: &Live) -> View {
    let page = l.page.get();
    let back = l.act(Act::Back);
    let close = l.act(Act::Close);
    const ROW: &str = "display: flex; align-items: center; gap: 8px; min-height: 56px; padding: 0 12px 0 12px; border-bottom: 1px solid var(--m-border)";
    if page == Page::Home {
        let label = RightPanel::Admin.label();
        return view! {
            <div style={ROW}>
                <Button variant=Variant::Ghost icon={icon(icons::BACK)} icon_only=true onclick={back} title="Back to your page" aria_label="Back" />
                <span style="color: var(--m-warning)">{icon(RightPanel::Admin.icon())}</span>
                <h1 style="flex: 1; min-width: 0; margin: 0; font-size: 19px; font-weight: 600; color: var(--m-text-strong)">{label}</h1>
                <CloseButton onclick={close} />
            </div>
        };
    }
    let title = match &page {
        Page::Home => String::new(),
        Page::Reports => "Reports".into(),
        Page::Report(id) => l
            .reports
            .get()
            .ready()
            .and_then(|list| {
                list.iter()
                    .find(|r| r.id == *id)
                    .map(|r| format!("Report about {}", r.subject_username))
            })
            .unwrap_or_else(|| "Report".into()),
        Page::Accounts => "Accounts".into(),
        Page::Account(id) => l
            .accounts
            .get()
            .ready()
            .and_then(|d| d.accounts.iter().find(|a| a.id == *id))
            .map_or_else(|| "Account".into(), |a| a.username.clone()),
        Page::Roles => "Roles".into(),
        Page::Claims => "Building claims".into(),
    };
    view! {
        <div style={ROW}>
            <Button variant=Variant::Ghost icon={icon(icons::BACK)} icon_only=true onclick={back} title="Back" aria_label="Back" />
            <h1 style="flex: 1; min-width: 0; margin: 0; font-size: 17px; font-weight: 600; color: var(--m-text-strong); white-space: nowrap; text-overflow: ellipsis; overflow: hidden">{title}</h1>
            <CloseButton onclick={close} />
        </div>
    }
}

/// The admin home: the pages this account may open, in the web page's three
/// areas (`docs/mockups/home-and-profile/v2`, H).
fn home(l: &Live) -> View {
    let l = l.clone();
    (move || -> View {
        let o = l.offers.get();
        let info = l.home.get();
        let warn = Some(Tone::Warning);
        let people: View = if o.people() {
            let reports: View = if o.triage {
                let end: View = if info.open_reports > 0 {
                    let n = if info.open_reports > 99 {
                        "99+".to_string()
                    } else {
                        info.open_reports.to_string()
                    };
                    view! { <Badge tone=Tone::Accent>{n}</Badge> }
                } else {
                    View::Empty
                };
                view! {
                    <ListItem icon={icon(icons::FLAG)} sub="About a person, from anyone" end={end} tone={warn} chevron=true onclick={l.act(Act::Open(Page::Reports))}>"Reports"</ListItem>
                }
            } else {
                View::Empty
            };
            let accounts: View = if o.accounts {
                let sub = info.accounts_summary.clone();
                view! {
                    <ListItem icon={icon(icons::GROUP)} sub={sub} tone={warn} chevron=true onclick={l.act(Act::Open(Page::Accounts))}>"Accounts"</ListItem>
                }
            } else {
                View::Empty
            };
            let roles: View = if o.roles {
                view! {
                    <ListItem icon={icon(icons::KEY)} sub="What each role may do" tone={warn} chevron=true onclick={l.act(Act::Open(Page::Roles))}>"Roles"</ListItem>
                }
            } else {
                View::Empty
            };
            let claims: View = if o.triage {
                view! {
                    <ListItem icon={icon(icons::BUILDINGS)} sub="Who holds which building" tone={warn} chevron=true onclick={l.act(Act::Open(Page::Claims))}>"Building claims"</ListItem>
                }
            } else {
                View::Empty
            };
            view! {
                <Section title={gold("People")}>
                    <List>{reports}{accounts}{roles}{claims}</List>
                </Section>
            }
        } else {
            View::Empty
        };
        let content: View = if o.overrides || o.library {
            let overrides: View = if o.overrides {
                view! {
                    <ListItem icon={icon(icons::BUILD)} end={word("web page")} tone={warn} onclick={l.act(Act::Web)}>"Building overrides"</ListItem>
                }
            } else {
                View::Empty
            };
            let models: View = if o.library {
                view! {
                    <ListItem icon={icon(icons::CABINET)} sub="The shared library, street objects" end={word("web page")} tone={warn} onclick={l.act(Act::Web)}>"Models"</ListItem>
                }
            } else {
                View::Empty
            };
            view! {
                <Section title={gold("Content")}>
                    <List>{overrides}{models}</List>
                </Section>
            }
        } else {
            View::Empty
        };
        let data: View = if o.pipeline {
            view! {
                <Section title={gold("Data")}>
                    <List>
                        <ListItem icon={icon(icons::CAT_MAP)} sub="Map datasets per city" end={word("web page")} tone={warn} onclick={l.act(Act::Web)}>"Pipeline"</ListItem>
                    </List>
                </Section>
            }
        } else {
            View::Empty
        };
        view! { {people}{content}{data} }
    })
    .into_view()
}

/// The report queue: Open | Handled, the rows, and where community reports go.
fn reports(l: &Live) -> View {
    let switch = {
        let l = l.clone();
        move || -> View {
            let open = l.home.get().open_reports;
            let options = vec![
                Opt::new(false, format!("Open · {open}")),
                Opt::new(true, "Handled"),
            ];
            view! { <FieldSegmented model={l.handled.clone()} options={options} id="admin-handled" /> }
        }
    };
    let list = {
        let l = l.clone();
        move || -> View {
            let load = l.reports.get();
            if let Some(v) = not_ready(&load) {
                return v;
            }
            let handled = l.handled.get();
            let rows: Vec<View> = load
                .ready()
                .map(|list| {
                    list.iter()
                        .filter(|r| r.resolved == handled)
                        .map(|r| report_row(&l, r))
                        .collect()
                })
                .unwrap_or_default();
            if rows.is_empty() {
                note(if handled {
                    "Nothing handled yet."
                } else {
                    "No open reports."
                })
            } else {
                view! { <List>{rows}</List> }
            }
        }
    };
    view! {
        {switch}
        {list}
        {note("Reports from inside a community go to its moderators, not here.")}
    }
}

/// One report in the queue: who, why, what was said, by whom.
fn report_row(l: &Live, r: &Report) -> View {
    let said = admin::report_said(r);
    let quote: View = if said.is_empty() {
        View::Empty
    } else {
        let q = format!("\u{201c}{said}\u{201d}");
        view! {
            <span style="display: block; min-width: 0; font-style: italic; white-space: nowrap; text-overflow: ellipsis; overflow: hidden">{q}</span>
        }
    };
    let reason = admin::reason_word(&r.reason).to_string();
    let by = admin::reported_by(r);
    let sub = view! {
        <span style="display: flex; flex-direction: column; align-items: flex-start; gap: 4px; min-width: 0; padding-top: 2px">
            <Badge tone=Tone::Accent>{reason}</Badge>
            {quote}
            <span style="color: var(--m-text-faint)">{by}</span>
        </span>
    };
    let when = word(admin::short_time(&r.created_at));
    let subject = r.subject_username.clone();
    view! {
        <ListItem sub={sub} end={when} onclick={l.act(Act::Open(Page::Report(r.id)))}>{subject}</ListItem>
    }
}

/// One report: what was said, the person it is about, what was done.
fn report(l: &Live, id: i64) -> View {
    let find = move |reports: &Load<Vec<Report>>| -> Option<Report> {
        reports
            .ready()
            .and_then(|list| list.iter().find(|r| r.id == id).cloned())
    };
    // The line as it was reported, or the reporter's own words.
    let card = {
        let l = l.clone();
        move || -> View {
            let Some(r) = find(&l.reports.get()) else {
                return View::Empty;
            };
            let message: View = match r.message.as_deref().filter(|m| !m.is_empty()) {
                Some(m) => {
                    let m = format!("\u{201c}{m}\u{201d}");
                    view! { <p style="margin: 0; color: var(--m-text); line-height: 1.4">{m}</p> }
                }
                None => View::Empty,
            };
            let note_line: View = if r.note.is_empty() {
                View::Empty
            } else {
                let n = format!("Note: {}", r.note);
                view! { <p style="margin: 0; color: var(--m-text); font-size: var(--m-font-size-sm); line-height: 1.4">{n}</p> }
            };
            let meta = format!(
                "{} · reported by {} · {}",
                admin::reason_word(&r.reason),
                r.reporter_username
                    .as_deref()
                    .unwrap_or("a deleted account"),
                admin::short_time(&r.created_at)
            );
            view! {
                <div style="display: flex; flex-direction: column; gap: 8px; padding: 14px 16px; border-radius: 14px; background: var(--m-surface-raised); border: 1px solid var(--m-border)">
                    {message}
                    {note_line}
                    <p style="margin: 0; color: var(--m-text-dim); font-size: var(--m-font-size-sm)">{meta}</p>
                </div>
            }
        }
    };
    // The person it is about: their account, when this account may manage it.
    let person = {
        let l = l.clone();
        move || -> View {
            let Some(r) = find(&l.reports.get()) else {
                return View::Empty;
            };
            let accounts = l.accounts.get();
            let account = accounts.ready().and_then(|d| {
                d.accounts
                    .iter()
                    .find(|a| a.username.eq_ignore_ascii_case(&r.subject_username))
                    .cloned()
            });
            let about = l.reports.get().ready().map_or(0, |list| {
                list.iter()
                    .filter(|x| x.subject_username.eq_ignore_ascii_case(&r.subject_username))
                    .count()
            });
            let sub = format!(
                "{about} {}{}",
                if about == 1 { "report" } else { "reports" },
                if account.as_ref().is_some_and(|a| a.disabled) {
                    " · suspended"
                } else {
                    ""
                }
            );
            let manage = l.offers.get().accounts;
            let open = match (&account, manage) {
                (Some(a), true) => l.act(Act::Go(Page::Account(a.id.clone()))),
                _ => None,
            };
            let chevron = open.is_some();
            let suspend: View = match account.filter(|a| manage && !a.disabled && !a.admin) {
                Some(a) => confirm(
                    &l,
                    format!("suspend:{}", a.id),
                    format!("Suspend {}", a.username),
                    "Suspend: they can't sign in",
                    Act::Suspend(a.id.clone(), true),
                ),
                None => View::Empty,
            };
            let name = r.subject_username.clone();
            view! {
                <List>
                    <ListItem icon={icon(icons::APPEARANCE)} sub={sub} tone={Some(Tone::Warning)} chevron={chevron} onclick={open}>{name}</ListItem>
                </List>
                <div>{suspend}</div>
            }
        }
    };
    // What was done: the record of a handled report, or the field and the
    // button; mounted again only when the report is handled.
    let done = {
        let l = l.clone();
        move || -> View {
            match l.resolved.get() {
                None => View::Empty,
                Some(true) => {
                    let said = find(&l.reports.get_untracked())
                        .and_then(|r| r.resolution)
                        .filter(|s| !s.is_empty())
                        .unwrap_or_else(|| "Marked handled, with nothing written.".into());
                    view! { <Section title="What was done">{note(said)}</Section> }
                }
                Some(false) => view! {
                    <Section title="What was done">
                        <FieldText model={l.resolution.clone()} placeholder="e.g. \"warned in chat\", \"suspended\"" autocomplete="off" id="admin-resolution" />
                        <div><Button variant=Variant::Primary onclick={l.act(Act::MarkHandled(id))}>"Mark handled"</Button></div>
                    </Section>
                },
            }
        }
    };
    view! {
        {card}
        {person}
        {done}
    }
}

/// The accounts: the search, and the first hundred that match.
fn accounts(l: &Live) -> View {
    let list = {
        let l = l.clone();
        move || -> View {
            let load = l.accounts.get();
            if let Some(v) = not_ready(&load) {
                return v;
            }
            let needle = l.search.get().trim().to_lowercase();
            let matching: Vec<Account> = load
                .ready()
                .map(|d| {
                    d.accounts
                        .iter()
                        .filter(|a| admin::account_matches(a, &needle))
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();
            if matching.is_empty() {
                return note("No account matches.");
            }
            // A few hundred rows at most (the server's own note); the first
            // hundred keep a phone's layout short until a search narrows it.
            let rows: Vec<View> = matching
                .iter()
                .take(100)
                .map(|a| {
                    let status = admin::status_word(a);
                    let end: View = if status.is_empty() {
                        View::Empty
                    } else {
                        word(status)
                    };
                    let (glyph, tone) = if a.disabled {
                        (icons::CLEAR, Tone::Danger)
                    } else {
                        (icons::APPEARANCE, Tone::Warning)
                    };
                    let sub = admin::account_sub(a);
                    let name = a.username.clone();
                    view! {
                        <ListItem icon={icon(glyph)} sub={sub} end={end} tone={Some(tone)} chevron=true onclick={l.act(Act::Open(Page::Account(a.id.clone())))}>{name}</ListItem>
                    }
                })
                .collect();
            let more: View = if matching.len() > 100 {
                note(format!(
                    "{} more: search to narrow it.",
                    matching.len() - 100
                ))
            } else {
                View::Empty
            };
            view! {
                <List>{rows}</List>
                {more}
            }
        }
    };
    view! {
        <FieldText model={l.search.clone()} r#type=TextType::Search placeholder="Search by name or email" autocomplete="off" prefix={icon(icons::SEARCH)} id="admin-search" />
        {list}
    }
}

/// One account: what is known of it, whether it can sign in, its reports,
/// its roles, and deleting it.
fn account(l: &Live, id: String) -> View {
    let l = l.clone();
    (move || -> View {
        let load = l.accounts.get();
        let Some(a) = load
            .ready()
            .and_then(|d| d.accounts.iter().find(|a| a.id == id))
            .cloned()
        else {
            return not_ready(&load).unwrap_or(View::Empty);
        };
        let held: Vec<String> = load
            .ready()
            .and_then(|d| d.roles.get(&a.id).cloned())
            .unwrap_or_default();
        let o = l.offers.get();
        let admin_note: View = if a.admin {
            note(
                "An admin. Only another admin can change this account, and the last admin \
                 cannot be suspended or deleted.",
            )
        } else {
            View::Empty
        };
        let can = signal(!a.disabled);
        let on_can: Rc<dyn Fn(&bool)> = {
            let (acts, id) = (l.acts.clone(), a.id.clone());
            Rc::new(move |v: &bool| acts.borrow_mut().push(Act::Suspend(id.clone(), !*v)))
        };
        let reports = l.reports.get().ready().map_or(0, |list| {
            list.iter()
                .filter(|r| r.subject_username.eq_ignore_ascii_case(&a.username))
                .count()
        });
        let reports_title = format!("Reports about {}", a.username);
        // Roles: on for each one held. `player` is everybody's.
        let roles: View = match l.roles.get().ready() {
            Some(roles) => {
                let switches: Vec<View> = roles
                    .iter()
                    .filter(|r| r.id != "player")
                    .map(|role| {
                        let on = signal(held.contains(&role.id));
                        let change: Rc<dyn Fn(&bool)> = {
                            let (acts, id, role) =
                                (l.acts.clone(), a.id.clone(), role.id.clone());
                            Rc::new(move |v: &bool| {
                                acts.borrow_mut()
                                    .push(Act::SetRole(id.clone(), role.clone(), *v))
                            })
                        };
                        let field_id = format!("admin-role-{}", role.id);
                        view! {
                            <FieldSwitch model={on} label={role.name.clone()} disabled={!o.assign_roles} onchange={Some(change)} id={field_id} />
                        }
                    })
                    .collect();
                view! { <Section title={gold("Roles")}>{switches}</Section> }
            }
            None => View::Empty,
        };
        let delete: View = if o.accounts && !a.admin {
            view! {
                <div>{confirm(&l, format!("delete:{}", a.id), "Delete account…".into(), "Delete permanently", Act::Delete(a.id.clone()))}</div>
            }
        } else {
            View::Empty
        };
        view! {
            {note(admin::account_facts(&a))}
            {admin_note}
            <FieldSwitch model={can} label={admin::CAN_SIGN_IN} disabled={!o.accounts} onchange={Some(on_can)} id="admin-can-sign-in" />
            <List>
                <ListItem icon={icon(icons::FLAG)} end={word(reports.to_string())} tone={Some(Tone::Warning)} chevron=true onclick={l.act(Act::ReportsOf)}>{reports_title}</ListItem>
            </List>
            {roles}
            {delete}
        }
    })
    .into_view()
}

/// The roles and what each may do; making or changing one is the web page's.
fn roles(l: &Live) -> View {
    let list = {
        let l = l.clone();
        move || -> View {
            let load = l.roles.get();
            if let Some(v) = not_ready(&load) {
                return v;
            }
            let rows: Vec<View> = load
                .ready()
                .map(|roles| {
                    roles
                        .iter()
                        .map(|role| {
                            let built_in: View = if role.built_in {
                                view! { <Badge>"built in"</Badge> }
                            } else {
                                View::Empty
                            };
                            let may = admin::role_may(role);
                            let may: View = if may.is_empty() {
                                View::Empty
                            } else {
                                let may = may.join(" · ");
                                view! { <span style="display: block; color: var(--m-text); padding-top: 2px">{may}</span> }
                            };
                            let people = admin::role_people(role);
                            let sub = view! {
                                <span style="display: flex; flex-direction: column; min-width: 0">
                                    <span>{people}</span>
                                    {may}
                                </span>
                            };
                            let name = role.name.clone();
                            view! {
                                <ListItem icon={icon(icons::KEY)} sub={sub} end={built_in}>{name}</ListItem>
                            }
                        })
                        .collect()
                })
                .unwrap_or_default();
            view! { <List>{rows}</List> }
        }
    };
    view! {
        {list}
        {note("Give a person a role on their account. Making or changing a role is on the web page.")}
        <div><Button icon={icon(icons::OPEN_EXTERNAL)} onclick={l.act(Act::Web)}>"Open the web page"</Button></div>
    }
}

/// Who holds which building, each with Release (asked twice).
fn claims(l: &Live) -> View {
    let list = {
        let l = l.clone();
        move || -> View {
            let load = l.claims.get();
            if let Some(v) = not_ready(&load) {
                return v;
            }
            let claims = load.ready().cloned().unwrap_or_default();
            if claims.is_empty() {
                return note("Nobody has claimed a building yet.");
            }
            let rows: Vec<View> = claims
                .iter()
                .map(|c| {
                    let release = confirm(
                        &l,
                        format!("release:{}", c.pand_id),
                        "Release…".into(),
                        "Release: anyone may claim it",
                        Act::Release(c.pand_id.clone()),
                    );
                    let place = admin::claim_place(c);
                    let holder = admin::claim_holder(c);
                    view! {
                        <div style="display: flex; flex-direction: column; gap: 10px; padding-bottom: 12px">
                            <ListItem icon={icon(icons::BUILDINGS)} sub={holder}>{place}</ListItem>
                            <div style="padding-left: 16px">{release}</div>
                        </div>
                    }
                })
                .collect();
            view! { <List>{rows}</List> }
        }
    };
    view! {
        {note("Claiming is first come, first served: releasing one is how a dispute is settled.")}
        {list}
    }
}

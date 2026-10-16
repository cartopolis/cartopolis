//! Admin: the third, gold side of the Community | World | Admin switch, for an
//! account whose role lets it run something (`docs/mockups/home-and-profile/v2`).
//!
//! The web page has had the same switch since 2026-10-11, and this is its
//! People area in the app: the report queue, the accounts, the roles and the
//! building claims — what an admin needs away from a desk. Content (building
//! overrides, the model library) and Data (the pipeline) open the web page
//! instead: they are uploads and logs, and the web page already does them.
//!
//! **Nothing here is new on the server.** Every page is the operator API the
//! web page uses (`/api/admin/*`), with the session this app signed in with,
//! and the server checks the permission on every call; what this module reads
//! from [`crate::resources::AuthIdentity::permissions`] decides only what it
//! *offers*. A role that may triage reports sees Reports and claims, one that
//! may manage accounts sees Accounts, and so on.
//!
//! HTTP is off the main thread the way the rest of the client does it: a task
//! on the IO pool posts its answer into a shared inbox, and the panel drains it
//! the next frame it runs.

use crate::systems::egui_manager::EguiMut;
use bevy::prelude::*;
use bevy::tasks::IoTaskPool;
use cartopolis_core::permissions::Permission;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use crate::resources::{AuthIdentity, OnlineSession, RightPanel};
use crate::systems::chats::{top_bar, ChatsView, Seg};
use crate::systems::{icons, theme};

/// Admin's colour, everywhere it appears: the web page's gold.
const GOLD: egui::Color32 = theme::WARN;

/// How often the open-report count behind the switch's badge is refreshed.
const REPORT_POLL_SECS: f32 = 120.0;

/// One report about a person (`db::ReportRow` on the server).
#[derive(Clone, Debug, Default, Deserialize)]
pub struct Report {
    pub id: i64,
    #[serde(default)]
    pub reporter_username: Option<String>,
    pub subject_username: String,
    pub reason: String,
    #[serde(default)]
    pub note: String,
    #[serde(default)]
    pub message: Option<String>,
    pub created_at: String,
    #[serde(default)]
    pub resolved: bool,
    #[serde(default)]
    pub resolution: Option<String>,
}

/// One account (`auth::AccountRow` on the server).
#[derive(Clone, Debug, Default, Deserialize)]
pub struct Account {
    pub id: String,
    pub username: String,
    #[serde(default)]
    pub disabled: bool,
    #[serde(default)]
    pub admin: bool,
    pub created_at: String,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub email_pending: bool,
}

#[derive(Clone, Debug, Default, Deserialize)]
struct AccountsDoc {
    accounts: Vec<Account>,
    /// Account id → the roles it holds explicitly.
    #[serde(default)]
    roles: BTreeMap<String, Vec<String>>,
}

/// A role (`roles::RoleRow` on the server).
#[derive(Clone, Debug, Default, Deserialize)]
pub struct Role {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub built_in: bool,
    #[serde(default)]
    pub permissions: Vec<String>,
    #[serde(default)]
    pub members: u64,
}

#[derive(Clone, Debug, Default, Deserialize)]
struct RolesDoc {
    roles: Vec<Role>,
}

/// An owned building (`buildings::Ownership` on the server).
#[derive(Clone, Debug, Default, Deserialize)]
pub struct Claim {
    pub pand_id: String,
    pub address: String,
    #[serde(default)]
    pub postcode: Option<String>,
    pub claimed_at: String,
    #[serde(default)]
    pub username: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
struct ClaimsDoc {
    buildings: Vec<Claim>,
}

/// Something fetched: not yet asked, on its way, here, or refused.
#[derive(Clone, Debug, Default)]
pub enum Load<T> {
    #[default]
    Idle,
    Loading,
    Ready(T),
    Failed(String),
}

impl<T> Load<T> {
    fn ready(&self) -> Option<&T> {
        match self {
            Load::Ready(t) => Some(t),
            _ => None,
        }
    }
}

/// Which admin page is showing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Page {
    #[default]
    Home,
    Reports,
    Report(i64),
    Accounts,
    Account(String),
    Roles,
    Claims,
}

/// What a task posted back.
enum Reply {
    Reports(Result<Vec<Report>, String>),
    Accounts(Result<AccountsDoc, String>),
    Roles(Result<Vec<Role>, String>),
    Claims(Result<Vec<Claim>, String>),
    /// A change went through: say so, and fetch these again.
    Done(String, Refetch),
    Failed(String),
}

/// What a change makes stale.
#[derive(Clone, Copy)]
enum Refetch {
    Reports,
    Accounts,
    Claims,
}

#[derive(Resource, Default)]
pub struct AdminState {
    pub page: Page,
    pub reports: Load<Vec<Report>>,
    /// Handled reports too, not only the open ones.
    pub show_handled: bool,
    accounts: Load<AccountsDoc>,
    pub roles: Load<Vec<Role>>,
    pub claims: Load<Vec<Claim>>,
    /// The Accounts page's search.
    pub search: String,
    /// What was done about the open report, as typed.
    pub resolution: String,
    /// The last change's outcome: (text, failed).
    pub notice: Option<(String, bool)>,
    /// How many reports are open, for the switch's badge.
    pub open_reports: usize,
    /// When the open-report count was last asked for.
    polled: Option<web_time::Instant>,
    inbox: Arc<Mutex<Vec<Reply>>>,
    /// A scripted capture's stand-in data (`--ui admin`): nothing is fetched.
    pub demo: bool,
}

/// The permissions that make an account an admin of something, and so give
/// it the switch's Admin side.
const ADMIN_PERMISSIONS: [Permission; 6] = [
    Permission::ReportsTriage,
    Permission::AccountsManage,
    Permission::RolesManage,
    Permission::BuildingOverride,
    Permission::LibraryManage,
    Permission::PipelineRun,
];

/// Whether this account may run anything the Admin side offers.
#[must_use]
pub fn may_admin(identity: &AuthIdentity) -> bool {
    ADMIN_PERMISSIONS.iter().any(|p| identity.may(*p))
}

impl AdminState {
    fn spawn(&self, work: impl bevy::tasks::ConditionalSendFuture<Output = Reply> + 'static) {
        let inbox = self.inbox.clone();
        IoTaskPool::get()
            .spawn(async move {
                let reply = work.await;
                if let Ok(mut v) = inbox.lock() {
                    v.push(reply);
                }
            })
            .detach();
    }

    fn fetch_reports(&mut self) {
        if self.demo {
            return;
        }
        if !matches!(self.reports, Load::Ready(_)) {
            self.reports = Load::Loading;
        }
        self.polled = Some(web_time::Instant::now());
        self.spawn(async {
            Reply::Reports(get_json::<Vec<Report>>("/api/admin/reports?all=true").await)
        });
    }

    fn fetch_accounts(&mut self) {
        if self.demo {
            return;
        }
        if !matches!(self.accounts, Load::Ready(_)) {
            self.accounts = Load::Loading;
        }
        self.spawn(async { Reply::Accounts(get_json::<AccountsDoc>("/api/admin/accounts").await) });
    }

    fn fetch_roles(&mut self) {
        if self.demo {
            return;
        }
        if !matches!(self.roles, Load::Ready(_)) {
            self.roles = Load::Loading;
        }
        self.spawn(async {
            Reply::Roles(
                get_json::<RolesDoc>("/api/admin/roles")
                    .await
                    .map(|d| d.roles),
            )
        });
    }

    fn fetch_claims(&mut self) {
        if self.demo {
            return;
        }
        if !matches!(self.claims, Load::Ready(_)) {
            self.claims = Load::Loading;
        }
        self.spawn(async {
            Reply::Claims(
                get_json::<ClaimsDoc>("/api/admin/claims")
                    .await
                    .map(|d| d.buildings),
            )
        });
    }

    /// Make a change, then say `done` and fetch `then` again.
    fn change(
        &self,
        work: impl bevy::tasks::ConditionalSendFuture<Output = Result<(), String>> + 'static,
        done: String,
        then: Refetch,
    ) {
        if self.demo {
            if let Ok(mut v) = self.inbox.lock() {
                v.push(Reply::Done(done, then));
            }
            return;
        }
        self.spawn(async move {
            match work.await {
                Ok(()) => Reply::Done(done, then),
                Err(e) => Reply::Failed(e),
            }
        });
    }

    fn drain(&mut self) {
        let replies = self
            .inbox
            .lock()
            .map(|mut v| std::mem::take(&mut *v))
            .unwrap_or_default();
        for reply in replies {
            match reply {
                Reply::Reports(r) => {
                    self.reports = match r {
                        Ok(list) => {
                            self.open_reports = list.iter().filter(|r| !r.resolved).count();
                            Load::Ready(list)
                        }
                        Err(e) => Load::Failed(e),
                    }
                }
                Reply::Accounts(r) => self.accounts = r.map_or_else(Load::Failed, Load::Ready),
                Reply::Roles(r) => self.roles = r.map_or_else(Load::Failed, Load::Ready),
                Reply::Claims(r) => self.claims = r.map_or_else(Load::Failed, Load::Ready),
                Reply::Done(said, then) => {
                    self.notice = Some((said, false));
                    match then {
                        Refetch::Reports => self.fetch_reports(),
                        Refetch::Accounts => self.fetch_accounts(),
                        Refetch::Claims => self.fetch_claims(),
                    }
                }
                Reply::Failed(e) => self.notice = Some((format!("Not done: {e}"), true)),
            }
        }
    }

    fn account(&self, id: &str) -> Option<&Account> {
        self.accounts.ready()?.accounts.iter().find(|a| a.id == id)
    }

    fn account_named(&self, name: &str) -> Option<&Account> {
        self.accounts
            .ready()?
            .accounts
            .iter()
            .find(|a| a.username.eq_ignore_ascii_case(name))
    }
}

async fn get_json<T: serde::de::DeserializeOwned>(path: &str) -> Result<T, String> {
    let body = crate::systems::model_assets::api_get(path).await?;
    serde_json::from_str(&body).map_err(|e| format!("unexpected answer: {e}"))
}

/// Open the web page's Admin, where Content and Data are.
fn open_web_admin() {
    if let Some(base) = crate::systems::model_assets::api_base() {
        crate::systems::data_layers::open_url(&format!("{base}/admin"));
    }
}

/// The Admin panel. Ordered in the panel chain like every other panel.
#[allow(clippy::too_many_arguments)]
pub fn render_admin_panel(
    mut egui: EguiMut,
    mut right_panel: ResMut<RightPanel>,
    mut admin: ResMut<AdminState>,
    identity: Res<AuthIdentity>,
    mut view: ResMut<ChatsView>,
    contacts: Res<crate::systems::contacts::Contacts>,
    online: Option<Res<OnlineSession>>,
) {
    admin.drain();
    let allowed = admin.demo || (online.is_some() && may_admin(&identity));
    view.admin = allowed.then_some(admin.open_reports);
    if !allowed {
        if right_panel.is(RightPanel::Admin) {
            *right_panel = RightPanel::None;
        }
        return;
    }
    // Keep the badge behind the switch current, whether or not the panel is up.
    if identity.may(Permission::ReportsTriage)
        && admin
            .polled
            .is_none_or(|t| t.elapsed().as_secs_f32() > REPORT_POLL_SECS)
    {
        admin.fetch_reports();
    }
    if !right_panel.is(RightPanel::Admin) {
        return;
    }
    let me = identity.username.clone().unwrap_or_default();
    let unread = 0;
    let waiting = !contacts.incoming.is_empty();
    let compact = egui.compact();
    let viewport_w = egui.viewport_pts().x;
    let mut open = true;
    let mut go: Option<Seg> = None;
    let mut you = false;
    crate::systems::egui_manager::responsive_right_panel("admin_panel", compact, viewport_w, 360.0)
        .frame(
            egui::Frame::NONE
                .fill(theme::PANEL)
                .inner_margin(egui::Margin::symmetric(12, 10)),
        )
        .show_collapsible(egui.ui_mut(), &mut open, |ui| {
            if admin.page == Page::Home {
                let bar = top_bar(ui, Seg::Admin, unread, view.admin, &me, waiting);
                go = bar.seg;
                you = bar.you;
            }
            if let Some((text, failed)) = admin.notice.clone() {
                let dismiss = if failed {
                    theme::inline_error(ui, &text)
                } else {
                    let mut ok = false;
                    ui.horizontal_wrapped(|ui| {
                        theme::note(ui, &text);
                        ok = theme::action_button(ui, "OK").clicked();
                    });
                    ok
                };
                if dismiss {
                    admin.notice = None;
                }
            }
            let page = admin.page.clone();
            match page {
                Page::Home => home(ui, &mut admin, &identity),
                Page::Reports => reports_page(ui, &mut admin),
                Page::Report(id) => report_page(ui, &mut admin, &identity, id),
                Page::Accounts => accounts_page(ui, &mut admin),
                Page::Account(id) => account_page(ui, &mut admin, &identity, &id),
                Page::Roles => roles_page(ui, &mut admin),
                Page::Claims => claims_page(ui, &mut admin),
            }
        });
    if !open {
        *right_panel = RightPanel::None;
    }
    match go {
        Some(Seg::Community) => {
            *right_panel = RightPanel::Chat;
            view.open = None;
            view.you = false;
        }
        Some(Seg::World) => *right_panel = RightPanel::None,
        _ => {}
    }
    if you {
        *right_panel = RightPanel::Chat;
        view.open = None;
        view.you = true;
    }
}

/// The admin home: the pages this account may open, in the web page's three
/// areas (`docs/mockups/home-and-profile/v2`, H).
fn home(ui: &mut egui::Ui, admin: &mut AdminState, identity: &AuthIdentity) {
    let triage = identity.may(Permission::ReportsTriage) || admin.demo;
    let accounts = identity.may(Permission::AccountsManage) || admin.demo;
    let roles = identity.may(Permission::RolesManage) || accounts;
    let overrides = identity.may(Permission::BuildingOverride) || admin.demo;
    let library = identity.may(Permission::LibraryManage) || admin.demo;
    let pipeline = identity.may(Permission::PipelineRun) || admin.demo;
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            if triage || accounts || roles {
                theme::section_tinted(ui, "People", GOLD);
                theme::list_group(ui, |ui| {
                    if triage
                        && theme::list_row(
                            ui,
                            icons::FLAG,
                            "Reports",
                            Some("About a person, from anyone"),
                            theme::RowEnd::Badge(admin.open_reports),
                            GOLD,
                        )
                        .clicked()
                    {
                        admin.page = Page::Reports;
                        admin.fetch_reports();
                    }
                    if accounts {
                        let sub = match admin.accounts.ready() {
                            Some(doc) => {
                                let pending =
                                    doc.accounts.iter().filter(|a| a.email_pending).count();
                                if pending > 0 {
                                    format!("{} · {pending} waiting on email", doc.accounts.len())
                                } else {
                                    format!("{} accounts", doc.accounts.len())
                                }
                            }
                            None => "Find, suspend, delete".into(),
                        };
                        if theme::list_row(
                            ui,
                            icons::GROUP,
                            "Accounts",
                            Some(&sub),
                            theme::RowEnd::Chevron,
                            GOLD,
                        )
                        .clicked()
                        {
                            admin.page = Page::Accounts;
                            admin.fetch_accounts();
                        }
                    }
                    if roles
                        && theme::list_row(
                            ui,
                            icons::KEY,
                            "Roles",
                            Some("What each role may do"),
                            theme::RowEnd::Chevron,
                            GOLD,
                        )
                        .clicked()
                    {
                        admin.page = Page::Roles;
                        admin.fetch_roles();
                    }
                    if triage
                        && theme::list_row(
                            ui,
                            icons::BUILDINGS,
                            "Building claims",
                            Some("Who holds which building"),
                            theme::RowEnd::Chevron,
                            GOLD,
                        )
                        .clicked()
                    {
                        admin.page = Page::Claims;
                        admin.fetch_claims();
                    }
                });
            }
            if overrides || library {
                theme::section_tinted(ui, "Content", GOLD);
                theme::list_group(ui, |ui| {
                    if overrides
                        && theme::list_row(
                            ui,
                            icons::BUILD,
                            "Building overrides",
                            None,
                            theme::RowEnd::Word("web page"),
                            GOLD,
                        )
                        .clicked()
                    {
                        open_web_admin();
                    }
                    if library
                        && theme::list_row(
                            ui,
                            icons::CABINET,
                            "Models",
                            Some("The shared library, street objects"),
                            theme::RowEnd::Word("web page"),
                            GOLD,
                        )
                        .clicked()
                    {
                        open_web_admin();
                    }
                });
            }
            if pipeline {
                theme::section_tinted(ui, "Data", GOLD);
                theme::list_group(ui, |ui| {
                    if theme::list_row(
                        ui,
                        icons::CAT_MAP,
                        "Pipeline",
                        Some("Map datasets per city"),
                        theme::RowEnd::Word("web page"),
                        GOLD,
                    )
                    .clicked()
                    {
                        open_web_admin();
                    }
                });
            }
        });
}

/// A page that is still loading, or could not be loaded.
fn load_state<T>(ui: &mut egui::Ui, load: &Load<T>) -> bool {
    match load {
        Load::Ready(_) => true,
        Load::Idle | Load::Loading => {
            ui.horizontal(|ui| {
                ui.spinner();
                theme::note(ui, "Loading…");
            });
            false
        }
        Load::Failed(e) => {
            theme::note(ui, &format!("Could not load this: {e}"));
            false
        }
    }
}

/// A report's reason, as the server names it, in words.
fn reason_word(reason: &str) -> &str {
    match reason {
        "spam" => "Spam",
        "harassment" => "Harassment",
        "hate_speech" => "Hate speech",
        "sexual_content" => "Sexual content",
        "threat" => "Threat or self-harm",
        _ => "Something else",
    }
}

/// `2026-10-16 11:40:02` (or an RFC 3339 stamp) as `16 Oct 11:40`.
fn short_time(stamp: &str) -> String {
    let date = stamp.get(..10).unwrap_or(stamp);
    let time = stamp.get(11..16).unwrap_or("");
    let month = date
        .get(5..7)
        .and_then(|m| m.parse::<usize>().ok())
        .and_then(|m| {
            [
                "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
            ]
            .get(m.wrapping_sub(1))
            .copied()
        });
    match (date.get(8..10), month) {
        (Some(day), Some(month)) => format!("{} {month} {time}", day.trim_start_matches('0'))
            .trim()
            .to_string(),
        _ => stamp.to_string(),
    }
}

/// A small chip with a tinted background ("Spam", "suspended").
fn chip(ui: &mut egui::Ui, text: &str, ink: egui::Color32) {
    egui::Frame::NONE
        .fill(ink.gamma_multiply(0.16))
        .corner_radius(egui::CornerRadius::same(8))
        .inner_margin(egui::Margin::symmetric(7, 1))
        .show(ui, |ui| {
            ui.label(egui::RichText::new(text).size(11.0).color(ink));
        });
}

fn reports_page(ui: &mut egui::Ui, admin: &mut AdminState) {
    if theme::back_title(ui, "Reports") {
        admin.page = Page::Home;
        return;
    }
    let mut handled = admin.show_handled;
    let open = admin.open_reports;
    let open_label = format!("Open · {open}");
    theme::segmented(
        ui,
        &mut handled,
        &[(false, open_label.as_str()), (true, "Handled")],
    );
    admin.show_handled = handled;
    ui.add_space(8.0);
    if !load_state(ui, &admin.reports) {
        return;
    }
    let list: Vec<Report> = admin
        .reports
        .ready()
        .map(|l| {
            l.iter()
                .filter(|r| r.resolved == admin.show_handled)
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            if list.is_empty() {
                theme::note(
                    ui,
                    if admin.show_handled {
                        "Nothing handled yet."
                    } else {
                        "No open reports."
                    },
                );
            }
            theme::list_group(ui, |ui| {
                for r in &list {
                    if report_row(ui, r).clicked() {
                        admin.resolution.clear();
                        admin.page = Page::Report(r.id);
                        admin.fetch_accounts();
                    }
                }
            });
            ui.add_space(4.0);
            theme::note(
                ui,
                "Reports from inside a community go to its moderators, not here.",
            );
        });
}

/// One report in the queue: who, why, what was said, by whom.
fn report_row(ui: &mut egui::Ui, r: &Report) -> egui::Response {
    let inner = egui::Frame::NONE
        .inner_margin(egui::Margin::symmetric(14, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(&r.subject_username)
                        .size(14.5)
                        .color(theme::CREAM),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        egui::RichText::new(short_time(&r.created_at))
                            .size(12.0)
                            .color(theme::TEXT_DIM),
                    );
                });
            });
            chip(ui, reason_word(&r.reason), theme::BRAND);
            let said = r
                .message
                .as_deref()
                .filter(|m| !m.is_empty())
                .unwrap_or(&r.note);
            if !said.is_empty() {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(format!("\u{201c}{said}\u{201d}"))
                            .italics()
                            .size(13.0)
                            .color(theme::TEXT_DIM),
                    )
                    .truncate(),
                );
            }
            let by = match &r.reporter_username {
                Some(who) => format!("Reported by {who}"),
                None => "Reported by a deleted account".into(),
            };
            ui.label(egui::RichText::new(by).size(12.0).color(theme::LINE));
        });
    let rect = inner.response.rect;
    let resp = ui.interact(rect, ui.id().with(("report", r.id)), egui::Sense::click());
    if resp.hovered() {
        ui.painter().rect_stroke(
            rect,
            egui::CornerRadius::same(8),
            egui::Stroke::new(1.0, theme::LINE),
            egui::StrokeKind::Inside,
        );
    }
    resp
}

fn report_page(ui: &mut egui::Ui, admin: &mut AdminState, identity: &AuthIdentity, id: i64) {
    let Some(r) = admin
        .reports
        .ready()
        .and_then(|l| l.iter().find(|r| r.id == id))
        .cloned()
    else {
        admin.page = Page::Reports;
        return;
    };
    if theme::back_title(ui, &format!("Report about {}", r.subject_username)) {
        admin.page = Page::Reports;
        return;
    }
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            // The line as it was reported, or the reporter's own words.
            egui::Frame::NONE
                .fill(theme::SURFACE_RAISED)
                .corner_radius(egui::CornerRadius::same(14))
                .inner_margin(egui::Margin::symmetric(14, 12))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    if let Some(m) = r.message.as_deref().filter(|m| !m.is_empty()) {
                        ui.label(
                            egui::RichText::new(format!("\u{201c}{m}\u{201d}"))
                                .size(14.0)
                                .color(theme::TEXT),
                        );
                    }
                    if !r.note.is_empty() {
                        ui.label(
                            egui::RichText::new(format!("Note: {}", r.note))
                                .size(13.0)
                                .color(theme::TEXT),
                        );
                    }
                    ui.label(
                        egui::RichText::new(format!(
                            "{} · reported by {} · {}",
                            reason_word(&r.reason),
                            r.reporter_username
                                .as_deref()
                                .unwrap_or("a deleted account"),
                            short_time(&r.created_at)
                        ))
                        .size(12.0)
                        .color(theme::TEXT_DIM),
                    );
                });
            ui.add_space(8.0);
            let account = admin.account_named(&r.subject_username).cloned();
            let about = admin.reports.ready().map_or(0, |l| {
                l.iter()
                    .filter(|x| x.subject_username.eq_ignore_ascii_case(&r.subject_username))
                    .count()
            });
            let manage = identity.may(Permission::AccountsManage) || admin.demo;
            theme::list_group(ui, |ui| {
                let sub = format!(
                    "{about} {}{}",
                    if about == 1 { "report" } else { "reports" },
                    if account.as_ref().is_some_and(|a| a.disabled) {
                        " · suspended"
                    } else {
                        ""
                    }
                );
                let end = if manage && account.is_some() {
                    theme::RowEnd::Chevron
                } else {
                    theme::RowEnd::Nothing
                };
                if theme::list_row(
                    ui,
                    icons::APPEARANCE,
                    &r.subject_username,
                    Some(&sub),
                    end,
                    GOLD,
                )
                .clicked()
                {
                    if let (true, Some(a)) = (manage, &account) {
                        admin.page = Page::Account(a.id.clone());
                    }
                }
            });
            if r.resolved {
                theme::section(ui, "What was done");
                theme::note(
                    ui,
                    r.resolution
                        .as_deref()
                        .filter(|s| !s.is_empty())
                        .unwrap_or("Marked handled, with nothing written."),
                );
                return;
            }
            theme::section(ui, "What was done");
            crate::systems::text_touch::field(
                ui,
                egui::TextEdit::singleline(&mut admin.resolution)
                    .id(egui::Id::new("admin_resolution"))
                    .desired_width(ui.available_width())
                    .hint_text("e.g. \"warned in chat\", \"suspended\""),
            );
            ui.add_space(8.0);
            let handled = ui
                .add_sized(
                    [ui.available_width(), 42.0],
                    egui::Button::new(
                        egui::RichText::new("Mark handled")
                            .size(14.5)
                            .color(theme::INK),
                    )
                    .fill(GOLD)
                    .corner_radius(egui::CornerRadius::same(14)),
                )
                .clicked();
            if handled {
                let body = serde_json::json!({ "resolution": admin.resolution.trim() }).to_string();
                let path = format!("/api/admin/reports/{id}");
                if admin.demo {
                    if let Load::Ready(list) = &mut admin.reports {
                        if let Some(x) = list.iter_mut().find(|x| x.id == id) {
                            x.resolved = true;
                            x.resolution = Some(admin.resolution.trim().to_string());
                        }
                    }
                }
                admin.change(
                    async move {
                        crate::systems::model_assets::api_patch(&path, body)
                            .await
                            .map(|_| ())
                    },
                    format!("Report about {} marked handled.", r.subject_username),
                    Refetch::Reports,
                );
                admin.resolution.clear();
                admin.page = Page::Reports;
            }
            if let (true, Some(a)) = (manage, account.filter(|a| !a.disabled && !a.admin)) {
                ui.add_space(6.0);
                if theme::confirm_button(
                    ui,
                    egui::Id::new(("admin_suspend", &a.id)),
                    &format!("Suspend {}", a.username),
                    "Suspend: they can't sign in",
                ) {
                    suspend(admin, &a, true);
                }
            }
        });
}

/// Stop (`off`) or let back in an account.
fn suspend(admin: &mut AdminState, account: &Account, off: bool) {
    let path = format!("/api/admin/accounts/{}", account.id);
    let body = serde_json::json!({ "disabled": off }).to_string();
    let said = if off {
        format!(
            "Suspended {}: they can't sign in until an admin lifts it.",
            account.username
        )
    } else {
        format!("{} can sign in again.", account.username)
    };
    if admin.demo {
        if let Load::Ready(doc) = &mut admin.accounts {
            if let Some(a) = doc.accounts.iter_mut().find(|a| a.id == account.id) {
                a.disabled = off;
            }
        }
    }
    admin.change(
        async move {
            crate::systems::model_assets::api_patch(&path, body)
                .await
                .map(|_| ())
        },
        said,
        Refetch::Accounts,
    );
}

fn accounts_page(ui: &mut egui::Ui, admin: &mut AdminState) {
    if theme::back_title(ui, "Accounts") {
        admin.page = Page::Home;
        return;
    }
    crate::systems::text_touch::field(
        ui,
        egui::TextEdit::singleline(&mut admin.search)
            .id(egui::Id::new("admin_account_search"))
            .desired_width(ui.available_width())
            .hint_text(format!("{}  Search by name or email", icons::SEARCH)),
    );
    ui.add_space(8.0);
    if !load_state(ui, &admin.accounts) {
        return;
    }
    let needle = admin.search.trim().to_lowercase();
    let list: Vec<Account> = admin
        .accounts
        .ready()
        .map(|d| {
            d.accounts
                .iter()
                .filter(|a| {
                    needle.is_empty()
                        || a.username.to_lowercase().contains(&needle)
                        || a.email
                            .as_deref()
                            .is_some_and(|e| e.to_lowercase().contains(&needle))
                })
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            if list.is_empty() {
                theme::note(ui, "No account matches.");
            }
            // A few hundred rows at most (the server's own note); drawing the
            // first hundred keeps a phone's frame short until a search narrows it.
            theme::list_group(ui, |ui| {
                for a in list.iter().take(100) {
                    let status = if a.disabled {
                        "suspended"
                    } else if a.email_pending {
                        "waiting on email"
                    } else if a.admin {
                        "admin"
                    } else {
                        ""
                    };
                    let sub = match &a.email {
                        Some(e) => format!("Since {} · {e}", short_date(&a.created_at)),
                        None => format!("Since {}", short_date(&a.created_at)),
                    };
                    let end = if status.is_empty() {
                        theme::RowEnd::Chevron
                    } else {
                        theme::RowEnd::Word(status)
                    };
                    let tone = if a.disabled { theme::ERROR } else { GOLD };
                    let icon = if a.disabled {
                        icons::CLEAR
                    } else {
                        icons::APPEARANCE
                    };
                    if theme::list_row(ui, icon, &a.username, Some(&sub), end, tone).clicked() {
                        admin.page = Page::Account(a.id.clone());
                        admin.fetch_roles();
                    }
                }
            });
            if list.len() > 100 {
                theme::note(
                    ui,
                    &format!("{} more: search to narrow it.", list.len() - 100),
                );
            }
        });
}

/// `2026-10-16…` as `16 Oct 2026`.
fn short_date(stamp: &str) -> String {
    let t = short_time(stamp);
    let mut parts = t.split(' ');
    match (parts.next(), parts.next(), stamp.get(..4)) {
        (Some(d), Some(m), Some(y)) if !m.is_empty() => format!("{d} {m} {y}"),
        _ => stamp.to_string(),
    }
}

fn account_page(ui: &mut egui::Ui, admin: &mut AdminState, identity: &AuthIdentity, id: &str) {
    let Some(a) = admin.account(id).cloned() else {
        if theme::back_title(ui, "Account") {
            admin.page = Page::Accounts;
        }
        load_state(ui, &admin.accounts);
        return;
    };
    if theme::back_title(ui, &a.username) {
        admin.page = Page::Accounts;
        return;
    }
    let held: Vec<String> = admin
        .accounts
        .ready()
        .and_then(|d| d.roles.get(&a.id).cloned())
        .unwrap_or_default();
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            let mut facts = format!("Since {}", short_date(&a.created_at));
            if let Some(e) = &a.email {
                facts.push_str(&format!(" · {e}"));
            }
            if a.email_pending {
                facts.push_str(" · waiting on email");
            }
            theme::note(ui, &facts);
            ui.add_space(6.0);
            if a.admin {
                theme::note(
                    ui,
                    "An admin. Only another admin can change this account, and the last admin \
                     cannot be suspended or deleted.",
                );
            }
            theme::list_group(ui, |ui| {
                let mut can = !a.disabled;
                ui.horizontal(|ui| {
                    ui.add_space(14.0);
                    ui.add_enabled_ui(
                        identity.may(Permission::AccountsManage) || admin.demo,
                        |ui| {
                            if theme::checkbox(
                                ui,
                                &mut can,
                                "Can sign in (off: stopped at the next sign-in)",
                            )
                            .changed()
                            {
                                suspend(admin, &a, !can);
                            }
                        },
                    );
                });
                ui.add_space(6.0);
                let reports = admin.reports.ready().map_or(0, |l| {
                    l.iter()
                        .filter(|r| r.subject_username.eq_ignore_ascii_case(&a.username))
                        .count()
                });
                if theme::list_row(
                    ui,
                    icons::FLAG,
                    &format!("Reports about {}", a.username),
                    None,
                    theme::RowEnd::Count(reports),
                    GOLD,
                )
                .clicked()
                {
                    admin.page = Page::Reports;
                    admin.show_handled = false;
                }
            });
            // Roles: ticked for each one held. `player` is everybody's.
            let may_roles = identity.may(Permission::RolesManage) || admin.demo;
            if let Some(roles) = admin.roles.ready().cloned() {
                theme::section_tinted(ui, "Roles", GOLD);
                let mut want = held.clone();
                let mut changed = false;
                theme::list_group(ui, |ui| {
                    ui.add_space(6.0);
                    for role in roles.iter().filter(|r| r.id != "player") {
                        let mut on = want.contains(&role.id);
                        ui.horizontal(|ui| {
                            ui.add_space(14.0);
                            ui.add_enabled_ui(may_roles, |ui| {
                                if theme::checkbox(ui, &mut on, &role.name).changed() {
                                    changed = true;
                                }
                            });
                        });
                        if on && !want.contains(&role.id) {
                            want.push(role.id.clone());
                        } else if !on {
                            want.retain(|r| r != &role.id);
                        }
                    }
                    ui.add_space(6.0);
                });
                if changed {
                    set_roles(admin, &a, want);
                }
            }
            ui.add_space(10.0);
            if (identity.may(Permission::AccountsManage) || admin.demo) && !a.admin {
                if theme::confirm_button(
                    ui,
                    egui::Id::new(("admin_delete", &a.id)),
                    "Delete account…",
                    "Delete permanently",
                ) {
                    let path = format!("/api/admin/accounts/{}", a.id);
                    let name = a.username.clone();
                    admin.change(
                        async move { crate::systems::model_assets::api_delete(&path).await },
                        format!("Deleted {name}'s account."),
                        Refetch::Accounts,
                    );
                    admin.page = Page::Accounts;
                }
            }
        });
}

/// Give an account exactly these roles.
fn set_roles(admin: &mut AdminState, account: &Account, roles: Vec<String>) {
    if let Load::Ready(doc) = &mut admin.accounts {
        doc.roles.insert(account.id.clone(), roles.clone());
    }
    let path = format!("/api/admin/accounts/{}/roles", account.id);
    let body = serde_json::json!({ "roles": roles }).to_string();
    admin.change(
        async move {
            let request = crate::systems::model_assets::api_put(&path, body);
            request.await.map(|_| ())
        },
        format!("{}'s roles changed.", account.username),
        Refetch::Accounts,
    );
}

fn roles_page(ui: &mut egui::Ui, admin: &mut AdminState) {
    if theme::back_title(ui, "Roles") {
        admin.page = Page::Home;
        return;
    }
    if !load_state(ui, &admin.roles) {
        return;
    }
    let roles = admin.roles.ready().cloned().unwrap_or_default();
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for role in &roles {
                theme::list_group(ui, |ui| {
                    egui::Frame::NONE
                        .inner_margin(egui::Margin::symmetric(14, 10))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.horizontal(|ui| {
                                ui.label(
                                    egui::RichText::new(&role.name)
                                        .size(14.5)
                                        .color(theme::CREAM),
                                );
                                if role.built_in {
                                    chip(ui, "built in", theme::TEXT_DIM);
                                }
                            });
                            let people = if role.id == "player" {
                                "Everybody".to_string()
                            } else {
                                format!(
                                    "{} {}",
                                    role.members,
                                    if role.members == 1 {
                                        "person"
                                    } else {
                                        "people"
                                    }
                                )
                            };
                            ui.label(
                                egui::RichText::new(people)
                                    .size(12.0)
                                    .color(theme::TEXT_DIM),
                            );
                            let may: Vec<String> = role
                                .permissions
                                .iter()
                                .map(|p| {
                                    Permission::from_slug(p)
                                        .map_or_else(|| p.clone(), |p| p.label().to_string())
                                })
                                .collect();
                            if !may.is_empty() {
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(may.join(" · "))
                                            .size(12.0)
                                            .color(theme::TEXT),
                                    )
                                    .wrap(),
                                );
                            }
                        });
                });
            }
            theme::note(
                ui,
                "Give a person a role on their account. Making or changing a role is on the \
                 web page.",
            );
            if theme::action_button(ui, format!("{}  Open the web page", icons::OPEN_EXTERNAL))
                .clicked()
            {
                open_web_admin();
            }
        });
}

fn claims_page(ui: &mut egui::Ui, admin: &mut AdminState) {
    if theme::back_title(ui, "Building claims") {
        admin.page = Page::Home;
        return;
    }
    if !load_state(ui, &admin.claims) {
        return;
    }
    let claims = admin.claims.ready().cloned().unwrap_or_default();
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            if claims.is_empty() {
                theme::note(ui, "Nobody has claimed a building yet.");
            }
            theme::note(
                ui,
                "Claiming is first come, first served: releasing one is how a dispute is settled.",
            );
            ui.add_space(4.0);
            for c in &claims {
                theme::list_group(ui, |ui| {
                    egui::Frame::NONE
                        .inner_margin(egui::Margin::symmetric(14, 10))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            let place = match &c.postcode {
                                Some(pc) => format!("{} · {pc}", c.address),
                                None => c.address.clone(),
                            };
                            ui.label(egui::RichText::new(place).size(14.5).color(theme::CREAM));
                            ui.label(
                                egui::RichText::new(format!(
                                    "{} · since {}",
                                    c.username.as_deref().unwrap_or("a deleted account"),
                                    short_date(&c.claimed_at)
                                ))
                                .size(12.0)
                                .color(theme::TEXT_DIM),
                            );
                            if theme::confirm_button(
                                ui,
                                egui::Id::new(("admin_release", &c.pand_id)),
                                "Release…",
                                "Release: anyone may claim it",
                            ) {
                                let path = format!("/api/admin/claims/{}", c.pand_id);
                                if admin.demo {
                                    if let Load::Ready(list) = &mut admin.claims {
                                        list.retain(|x| x.pand_id != c.pand_id);
                                    }
                                }
                                admin.change(
                                    async move {
                                        crate::systems::model_assets::api_delete(&path).await
                                    },
                                    format!("Released {}.", c.address),
                                    Refetch::Claims,
                                );
                            }
                        });
                });
            }
        });
}

/// Stand-in data for a scripted capture (`--ui admin`, `--ui adminreports`,
/// `--ui adminreport`, `--ui adminaccount`): a capture has no server to ask.
pub fn seed_demo(admin: &mut AdminState, screen: &str) {
    admin.demo = true;
    admin.reports = Load::Ready(vec![
        Report {
            id: 2,
            reporter_username: Some("tom".into()),
            subject_username: "pieter".into(),
            reason: "spam".into(),
            message: Some("Cheap phones, DM me, crypto only".into()),
            created_at: "2026-10-16 11:40:00".into(),
            ..default()
        },
        Report {
            id: 1,
            reporter_username: Some("sanne".into()),
            subject_username: "xx_unknown".into(),
            reason: "harassment".into(),
            note: "keeps following my avatar around the Vismarkt".into(),
            created_at: "2026-10-15 20:12:00".into(),
            ..default()
        },
    ]);
    admin.open_reports = 2;
    let account = |id: &str, name: &str, since: &str, email: Option<&str>| Account {
        id: id.into(),
        username: name.into(),
        created_at: since.into(),
        email: email.map(str::to_string),
        ..default()
    };
    let mut accounts = vec![
        account("a1", "jeroen", "2026-03-02 10:00:00", None),
        account(
            "a2",
            "pieter",
            "2026-10-16 09:12:00",
            Some("pieter@example.com"),
        ),
        account("a3", "pietje", "2026-09-03 18:30:00", None),
        account(
            "a4",
            "piet.k",
            "2026-10-15 07:45:00",
            Some("piet@example.org"),
        ),
        account("a5", "sanne", "2026-04-11 12:00:00", None),
    ];
    accounts[0].admin = true;
    accounts[3].email_pending = true;
    let mut roles = BTreeMap::new();
    roles.insert("a5".to_string(), vec!["moderator".to_string()]);
    admin.accounts = Load::Ready(AccountsDoc { accounts, roles });
    admin.roles = Load::Ready(vec![
        Role {
            id: "player".into(),
            name: "Player".into(),
            built_in: true,
            permissions: vec!["prim_create".into(), "prim_edit_own".into()],
            members: 0,
        },
        Role {
            id: "moderator".into(),
            name: "Moderator".into(),
            built_in: true,
            permissions: vec!["reports_triage".into()],
            members: 1,
        },
        Role {
            id: "builder".into(),
            name: "Builder".into(),
            built_in: false,
            permissions: vec!["prim_edit_any".into(), "building_design_any".into()],
            members: 0,
        },
    ]);
    admin.claims = Load::Ready(vec![Claim {
        pand_id: "0014100010903891".into(),
        address: "Oude Ebbingestraat 12".into(),
        postcode: Some("9712 HE".into()),
        claimed_at: "2026-10-02 14:00:00".into(),
        username: Some("sanne".into()),
    }]);
    admin.page = match screen {
        "adminreports" => Page::Reports,
        "adminreport" => Page::Report(2),
        "adminaccount" => Page::Account("a2".into()),
        _ => Page::Home,
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stamp_reads_as_a_day_and_a_month() {
        assert_eq!(short_time("2026-10-16 11:40:02"), "16 Oct 11:40");
        assert_eq!(short_time("2026-03-02T09:05:00Z"), "2 Mar 09:05");
        assert_eq!(short_date("2026-03-02 09:05:00"), "2 Mar 2026");
        // Something it cannot read is shown as it came.
        assert_eq!(short_time("yesterday"), "yesterday");
    }

    #[test]
    fn the_server_documents_parse() {
        let accounts: AccountsDoc = serde_json::from_str(
            r#"{"accounts":[{"id":"x","username":"anna","disabled":false,"admin":false,
                "created_at":"2026-10-01 10:00:00","email":null,"email_pending":false}],
                "roles":{"x":["moderator"]}}"#,
        )
        .unwrap();
        assert_eq!(accounts.accounts[0].username, "anna");
        assert_eq!(accounts.roles["x"], vec!["moderator".to_string()]);
        let reports: Vec<Report> = serde_json::from_str(
            r#"[{"id":3,"reporter_username":null,"subject_username":"p","reason":"spam",
                "note":"","message":"hi","created_at":"2026-10-16 11:40:00","resolved":false,
                "resolved_at":null,"resolution":null}]"#,
        )
        .unwrap();
        assert_eq!(reports[0].id, 3);
        let claims: ClaimsDoc = serde_json::from_str(
            r#"{"buildings":[{"pand_id":"1","nummeraanduiding_id":"2","address":"A 1",
                "postcode":null,"latitude":53.2,"longitude":6.5,"claimed_at":"2026-10-01",
                "username":"anna"}]}"#,
        )
        .unwrap();
        assert_eq!(claims.buildings[0].username.as_deref(), Some("anna"));
    }

    #[test]
    fn only_an_account_that_may_run_something_sees_admin() {
        let mut id = AuthIdentity::default();
        assert!(!may_admin(&id));
        id.permissions = vec![Permission::PrimCreate.slug().to_string()];
        assert!(!may_admin(&id));
        id.permissions
            .push(Permission::ReportsTriage.slug().to_string());
        assert!(may_admin(&id));
    }
}

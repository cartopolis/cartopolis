//! Admin: a row on the You page (behind your disc), for an account whose role
//! lets it run something (`docs/mockups/home-and-profile/v2`). It was a third,
//! gold side of the Community | World switch until 2026-10-17, which put a few
//! people's tool on everybody's main control.
//!
//! The web page has an Admin area of its own, and this is its
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
use crate::systems::chats::ChatsView;
use crate::systems::{icons, theme};

/// The account page's sign-in switch, in both editions.
pub const CAN_SIGN_IN: &str = "Can sign in (off: stopped at the next sign-in)";

/// Admin's colour, everywhere it appears: the web page's gold.
fn gold() -> egui::Color32 {
    theme::p32().warn
}

/// How often the open-report count behind the You page's badge is refreshed.
const REPORT_POLL_SECS: f32 = 120.0;

/// The panel's width on a wide screen, in both editions.
pub const PANEL_WIDTH: f32 = 360.0;

/// One report about a person (`db::ReportRow` on the server).
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
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
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
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

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct AccountsDoc {
    pub accounts: Vec<Account>,
    /// Account id → the roles it holds explicitly.
    #[serde(default)]
    pub roles: BTreeMap<String, Vec<String>>,
}

/// A role (`roles::RoleRow` on the server).
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
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
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
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
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Load<T> {
    #[default]
    Idle,
    Loading,
    Ready(T),
    Failed(String),
}

impl<T> Load<T> {
    pub fn ready(&self) -> Option<&T> {
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
    pub accounts: Load<AccountsDoc>,
    pub roles: Load<Vec<Role>>,
    pub claims: Load<Vec<Claim>>,
    /// The Accounts page's search.
    pub search: String,
    /// What was done about the open report, as typed.
    pub resolution: String,
    /// The last change's outcome: (text, failed).
    pub notice: Option<(String, bool)>,
    /// How many reports are open, for the You page's badge.
    pub open_reports: usize,
    /// When the open-report count was last asked for.
    polled: Option<web_time::Instant>,
    inbox: Arc<Mutex<Vec<Reply>>>,
    /// A scripted capture's stand-in data (`--ui admin`): nothing is fetched.
    pub demo: bool,
    /// The component library draws the panel (`systems::lib_admin`): egui
    /// keeps only an empty strip of its width, and still drains the replies
    /// and keeps the You page's badge.
    pub lib: bool,
}

/// The permissions that make an account an admin of something, and so give
/// it the You page's Admin row.
const ADMIN_PERMISSIONS: [Permission; 6] = [
    Permission::ReportsTriage,
    Permission::AccountsManage,
    Permission::RolesManage,
    Permission::BuildingOverride,
    Permission::LibraryManage,
    Permission::PipelineRun,
];

/// Whether this account may run anything the Admin pages offer.
#[must_use]
pub fn may_admin(identity: &AuthIdentity) -> bool {
    ADMIN_PERMISSIONS.iter().any(|p| identity.may(*p))
}

/// What the pages offer this account: read from its permissions (and all of
/// it in a capture's demo). The server checks every call regardless.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Offers {
    /// Reports and building claims.
    pub triage: bool,
    /// Accounts: find, suspend, delete.
    pub accounts: bool,
    /// The Roles page (also for whoever manages accounts).
    pub roles: bool,
    /// Giving an account roles.
    pub assign_roles: bool,
    pub overrides: bool,
    pub library: bool,
    pub pipeline: bool,
}

impl Offers {
    #[must_use]
    pub fn of(identity: &AuthIdentity, demo: bool) -> Self {
        let accounts = identity.may(Permission::AccountsManage) || demo;
        Offers {
            triage: identity.may(Permission::ReportsTriage) || demo,
            accounts,
            roles: identity.may(Permission::RolesManage) || accounts,
            assign_roles: identity.may(Permission::RolesManage) || demo,
            overrides: identity.may(Permission::BuildingOverride) || demo,
            library: identity.may(Permission::LibraryManage) || demo,
            pipeline: identity.may(Permission::PipelineRun) || demo,
        }
    }

    /// The People area has something in it.
    #[must_use]
    pub fn people(self) -> bool {
        self.triage || self.accounts || self.roles
    }
}

/// Back from the admin home to the You page it was opened from.
pub fn to_you(right_panel: &mut RightPanel, view: &mut ChatsView) {
    *right_panel = RightPanel::Chat;
    view.open = None;
    view.you = true;
}

/// Whether an account matches the Accounts page's search (lowercased).
#[must_use]
pub fn account_matches(a: &Account, needle: &str) -> bool {
    needle.is_empty()
        || a.username.to_lowercase().contains(needle)
        || a.email
            .as_deref()
            .is_some_and(|e| e.to_lowercase().contains(needle))
}

/// An account's status word in the list: suspended, waiting, admin, or none.
#[must_use]
pub fn status_word(a: &Account) -> &'static str {
    if a.disabled {
        "suspended"
    } else if a.email_pending {
        "waiting on email"
    } else if a.admin {
        "admin"
    } else {
        ""
    }
}

/// An account's line under its name in the list.
#[must_use]
pub fn account_sub(a: &Account) -> String {
    match &a.email {
        Some(e) => format!("Since {} · {e}", short_date(&a.created_at)),
        None => format!("Since {}", short_date(&a.created_at)),
    }
}

/// What the account page says of it, on one line.
#[must_use]
pub fn account_facts(a: &Account) -> String {
    let mut facts = format!("Since {}", short_date(&a.created_at));
    if let Some(e) = &a.email {
        facts.push_str(&format!(" · {e}"));
    }
    if a.email_pending {
        facts.push_str(" · waiting on email");
    }
    facts
}

/// The words a report row quotes: the line as reported, else the note.
#[must_use]
pub fn report_said(r: &Report) -> &str {
    r.message
        .as_deref()
        .filter(|m| !m.is_empty())
        .unwrap_or(&r.note)
}

/// Who filed a report, as a row says it.
#[must_use]
pub fn reported_by(r: &Report) -> String {
    match &r.reporter_username {
        Some(who) => format!("Reported by {who}"),
        None => "Reported by a deleted account".into(),
    }
}

/// How many people hold a role, in words.
#[must_use]
pub fn role_people(role: &Role) -> String {
    if role.id == "player" {
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
    }
}

/// What a role may do, in words.
#[must_use]
pub fn role_may(role: &Role) -> Vec<String> {
    role.permissions
        .iter()
        .map(|p| Permission::from_slug(p).map_or_else(|| p.clone(), |p| p.label().to_string()))
        .collect()
}

/// A claimed building's address with its postcode.
#[must_use]
pub fn claim_place(c: &Claim) -> String {
    match &c.postcode {
        Some(pc) => format!("{} · {pc}", c.address),
        None => c.address.clone(),
    }
}

/// Who holds a claimed building, and since when.
#[must_use]
pub fn claim_holder(c: &Claim) -> String {
    format!(
        "{} · since {}",
        c.username.as_deref().unwrap_or("a deleted account"),
        short_date(&c.claimed_at)
    )
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

    pub fn account(&self, id: &str) -> Option<&Account> {
        self.accounts.ready()?.accounts.iter().find(|a| a.id == id)
    }

    pub fn account_named(&self, name: &str) -> Option<&Account> {
        self.accounts
            .ready()?
            .accounts
            .iter()
            .find(|a| a.username.eq_ignore_ascii_case(name))
    }

    /// A report in the queue, by id.
    pub fn report(&self, id: i64) -> Option<&Report> {
        self.reports.ready()?.iter().find(|r| r.id == id)
    }

    /// How many reports (open or handled) are about this person.
    #[must_use]
    pub fn reports_about(&self, username: &str) -> usize {
        self.reports.ready().map_or(0, |l| {
            l.iter()
                .filter(|r| r.subject_username.eq_ignore_ascii_case(username))
                .count()
        })
    }

    /// The roles an account holds explicitly.
    #[must_use]
    pub fn held_roles(&self, account_id: &str) -> Vec<String> {
        self.accounts
            .ready()
            .and_then(|d| d.roles.get(account_id).cloned())
            .unwrap_or_default()
    }

    /// The Accounts row's line on the home page.
    #[must_use]
    pub fn accounts_summary(&self) -> String {
        match self.accounts.ready() {
            Some(doc) => {
                let pending = doc.accounts.iter().filter(|a| a.email_pending).count();
                if pending > 0 {
                    format!("{} · {pending} waiting on email", doc.accounts.len())
                } else {
                    format!("{} accounts", doc.accounts.len())
                }
            }
            None => "Find, suspend, delete".into(),
        }
    }

    /// Open a page, fetching what it shows.
    pub fn open(&mut self, page: Page) {
        match &page {
            Page::Home => {}
            Page::Reports => self.fetch_reports(),
            Page::Report(_) => {
                self.resolution.clear();
                self.fetch_accounts();
            }
            Page::Accounts => self.fetch_accounts(),
            Page::Account(_) => self.fetch_roles(),
            Page::Roles => self.fetch_roles(),
            Page::Claims => self.fetch_claims(),
        }
        self.page = page;
    }

    /// The page a page's back arrow leads to; `None` on the home page, whose
    /// arrow leads out to the You page.
    #[must_use]
    pub fn parent(&self) -> Option<Page> {
        match self.page {
            Page::Home => None,
            Page::Reports | Page::Accounts | Page::Roles | Page::Claims => Some(Page::Home),
            Page::Report(_) => Some(Page::Reports),
            Page::Account(_) => Some(Page::Accounts),
        }
    }

    /// Follow a page's back arrow (the home page's leads out: [`to_you`]).
    pub fn go_back(&mut self) {
        if let Some(page) = self.parent() {
            self.page = page;
        }
    }

    /// A report page whose report is gone (handled away, or never loaded)
    /// falls back to the queue.
    pub fn settle(&mut self) {
        if let Page::Report(id) = self.page {
            if self.report(id).is_none() {
                self.page = Page::Reports;
            }
        }
    }

    /// The Reports page about one person, open ones first.
    pub fn reports_of(&mut self) {
        self.page = Page::Reports;
        self.show_handled = false;
    }

    /// Mark a report handled with what was typed in `resolution`, and go back
    /// to the queue.
    pub fn mark_handled(&mut self, id: i64) {
        let Some(subject) = self.report(id).map(|r| r.subject_username.clone()) else {
            return;
        };
        let body = serde_json::json!({ "resolution": self.resolution.trim() }).to_string();
        let path = format!("/api/admin/reports/{id}");
        if self.demo {
            let resolution = self.resolution.trim().to_string();
            if let Load::Ready(list) = &mut self.reports {
                if let Some(x) = list.iter_mut().find(|x| x.id == id) {
                    x.resolved = true;
                    x.resolution = Some(resolution);
                }
            }
        }
        self.change(
            async move {
                crate::systems::model_assets::api_patch(&path, body)
                    .await
                    .map(|_| ())
            },
            format!("Report about {subject} marked handled."),
            Refetch::Reports,
        );
        self.resolution.clear();
        self.page = Page::Reports;
    }

    /// Stop (`off`) or let back in an account.
    pub fn suspend(&mut self, account: &Account, off: bool) {
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
        if self.demo {
            if let Load::Ready(doc) = &mut self.accounts {
                if let Some(a) = doc.accounts.iter_mut().find(|a| a.id == account.id) {
                    a.disabled = off;
                }
            }
        }
        self.change(
            async move {
                crate::systems::model_assets::api_patch(&path, body)
                    .await
                    .map(|_| ())
            },
            said,
            Refetch::Accounts,
        );
    }

    /// Give an account exactly these roles.
    pub fn set_roles(&mut self, account: &Account, roles: Vec<String>) {
        if let Load::Ready(doc) = &mut self.accounts {
            doc.roles.insert(account.id.clone(), roles.clone());
        }
        let path = format!("/api/admin/accounts/{}/roles", account.id);
        let body = serde_json::json!({ "roles": roles }).to_string();
        self.change(
            async move {
                let request = crate::systems::model_assets::api_put(&path, body);
                request.await.map(|_| ())
            },
            format!("{}'s roles changed.", account.username),
            Refetch::Accounts,
        );
    }

    /// Give an account one role more (`on`) or less.
    pub fn set_role(&mut self, account: &Account, role: &str, on: bool) {
        let mut want = self.held_roles(&account.id);
        if on && !want.iter().any(|r| r == role) {
            want.push(role.to_string());
        } else if !on {
            want.retain(|r| r != role);
        }
        self.set_roles(account, want);
    }

    /// Delete an account for good, and go back to the list.
    pub fn delete_account(&mut self, account: &Account) {
        let path = format!("/api/admin/accounts/{}", account.id);
        let name = account.username.clone();
        self.change(
            async move { crate::systems::model_assets::api_delete(&path).await },
            format!("Deleted {name}'s account."),
            Refetch::Accounts,
        );
        self.page = Page::Accounts;
    }

    /// Release a building claim: anyone may claim it again.
    pub fn release_claim(&mut self, claim: &Claim) {
        let path = format!("/api/admin/claims/{}", claim.pand_id);
        if self.demo {
            if let Load::Ready(list) = &mut self.claims {
                list.retain(|x| x.pand_id != claim.pand_id);
            }
        }
        self.change(
            async move { crate::systems::model_assets::api_delete(&path).await },
            format!("Released {}.", claim.address),
            Refetch::Claims,
        );
    }
}

async fn get_json<T: serde::de::DeserializeOwned>(path: &str) -> Result<T, String> {
    let body = crate::systems::model_assets::api_get(path).await?;
    serde_json::from_str(&body).map_err(|e| format!("unexpected answer: {e}"))
}

/// Open the web page's Admin, where Content and Data are.
pub fn open_web_admin() {
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
    // Keep the You page's badge current, whether or not the panel is up.
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
    admin.settle();
    let compact = egui.compact();
    let viewport_w = egui.viewport_pts().x;
    let mut open = true;
    // The library draws the panel (`systems::lib_admin`): keep its strip, so
    // the dock and the map move aside and a click there is not the world's.
    if admin.lib {
        let mut strip = crate::systems::egui_manager::responsive_right_panel(
            "admin_panel",
            compact,
            viewport_w,
            PANEL_WIDTH,
        )
        .frame(egui::Frame::NONE)
        .show_separator_line(false);
        if !compact {
            strip = strip.resizable(false).exact_size(PANEL_WIDTH);
        }
        strip.show_collapsible(egui.ui_mut(), &mut open, |_ui| {});
        return;
    }
    let mut you = false;
    crate::systems::egui_manager::responsive_right_panel(
        "admin_panel",
        compact,
        viewport_w,
        PANEL_WIDTH,
    )
    .frame(
        egui::Frame::NONE
            .fill(theme::p32().panel)
            .inner_margin(egui::Margin::symmetric(12, 10)),
    )
    .show_collapsible(egui.ui_mut(), &mut open, |ui| {
        // Home leads back to the You page it was opened from; every
        // other page draws its own way back to Home.
        if admin.page == Page::Home {
            you = theme::back_title(ui, RightPanel::Admin.label());
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
        let depth = match page {
            Page::Home => 0,
            Page::Reports | Page::Accounts | Page::Roles | Page::Claims => 1,
            Page::Report(_) | Page::Account(_) => 2,
        };
        crate::systems::motion::page_turn(
            ui,
            egui::Id::new("admin_page"),
            page,
            depth,
            |ui, page| match page {
                Page::Home => home(ui, &mut admin, &identity),
                Page::Reports => reports_page(ui, &mut admin),
                Page::Report(id) => report_page(ui, &mut admin, &identity, id),
                Page::Accounts => accounts_page(ui, &mut admin),
                Page::Account(id) => account_page(ui, &mut admin, &identity, &id),
                Page::Roles => roles_page(ui, &mut admin),
                Page::Claims => claims_page(ui, &mut admin),
            },
        );
    });
    // Every page's title carries the panel's close (`theme::back_title`).
    if !open || theme::take_panel_close(egui.ctx()) {
        *right_panel = RightPanel::None;
        you = false;
    }
    if you {
        to_you(&mut right_panel, &mut view);
    }
}

/// The admin home: the pages this account may open, in the web page's three
/// areas (`docs/mockups/home-and-profile/v2`, H).
fn home(ui: &mut egui::Ui, admin: &mut AdminState, identity: &AuthIdentity) {
    let offers = Offers::of(identity, admin.demo);
    let Offers {
        triage,
        accounts,
        roles,
        overrides,
        library,
        pipeline,
        ..
    } = offers;
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            if offers.people() {
                theme::section_tinted(ui, "People", gold());
                theme::list_group(ui, |ui| {
                    if triage
                        && theme::list_row(
                            ui,
                            icons::FLAG,
                            "Reports",
                            Some("About a person, from anyone"),
                            theme::RowEnd::Badge(admin.open_reports),
                            gold(),
                        )
                        .clicked()
                    {
                        admin.open(Page::Reports);
                    }
                    if accounts {
                        let sub = admin.accounts_summary();
                        if theme::list_row(
                            ui,
                            icons::GROUP,
                            "Accounts",
                            Some(&sub),
                            theme::RowEnd::Chevron,
                            gold(),
                        )
                        .clicked()
                        {
                            admin.open(Page::Accounts);
                        }
                    }
                    if roles
                        && theme::list_row(
                            ui,
                            icons::KEY,
                            "Roles",
                            Some("What each role may do"),
                            theme::RowEnd::Chevron,
                            gold(),
                        )
                        .clicked()
                    {
                        admin.open(Page::Roles);
                    }
                    if triage
                        && theme::list_row(
                            ui,
                            icons::BUILDINGS,
                            "Building claims",
                            Some("Who holds which building"),
                            theme::RowEnd::Chevron,
                            gold(),
                        )
                        .clicked()
                    {
                        admin.open(Page::Claims);
                    }
                });
            }
            if overrides || library {
                theme::section_tinted(ui, "Content", gold());
                theme::list_group(ui, |ui| {
                    if overrides
                        && theme::list_row(
                            ui,
                            icons::BUILD,
                            "Building overrides",
                            None,
                            theme::RowEnd::Word("web page"),
                            gold(),
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
                            gold(),
                        )
                        .clicked()
                    {
                        open_web_admin();
                    }
                });
            }
            if pipeline {
                theme::section_tinted(ui, "Data", gold());
                theme::list_group(ui, |ui| {
                    if theme::list_row(
                        ui,
                        icons::CAT_MAP,
                        "Pipeline",
                        Some("Map datasets per city"),
                        theme::RowEnd::Word("web page"),
                        gold(),
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
pub fn reason_word(reason: &str) -> &str {
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
pub fn short_time(stamp: &str) -> String {
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
        admin.go_back();
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
                        admin.open(Page::Report(r.id));
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
                        .color(theme::p32().cream),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        egui::RichText::new(short_time(&r.created_at))
                            .size(12.0)
                            .color(theme::p32().text_dim),
                    );
                });
            });
            chip(ui, reason_word(&r.reason), theme::BRAND.to_egui());
            let said = report_said(r);
            if !said.is_empty() {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(format!("\u{201c}{said}\u{201d}"))
                            .italics()
                            .size(13.0)
                            .color(theme::p32().text_dim),
                    )
                    .truncate(),
                );
            }
            let by = reported_by(r);
            ui.label(egui::RichText::new(by).size(12.0).color(theme::p32().line));
        });
    let rect = inner.response.rect;
    let resp = ui.interact(rect, ui.id().with(("report", r.id)), egui::Sense::click());
    let resp = theme::named(ui, resp, &r.subject_username);
    if resp.hovered() {
        ui.painter().rect_stroke(
            rect,
            egui::CornerRadius::same(8),
            egui::Stroke::new(1.0, theme::p32().line),
            egui::StrokeKind::Inside,
        );
    }
    resp
}

fn report_page(ui: &mut egui::Ui, admin: &mut AdminState, identity: &AuthIdentity, id: i64) {
    let Some(r) = admin.report(id).cloned() else {
        admin.page = Page::Reports;
        return;
    };
    if theme::back_title(ui, &format!("Report about {}", r.subject_username)) {
        admin.go_back();
        return;
    }
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            // The line as it was reported, or the reporter's own words.
            egui::Frame::NONE
                .fill(theme::p32().surface_raised)
                .corner_radius(egui::CornerRadius::same(14))
                .inner_margin(egui::Margin::symmetric(14, 12))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    if let Some(m) = r.message.as_deref().filter(|m| !m.is_empty()) {
                        ui.label(
                            egui::RichText::new(format!("\u{201c}{m}\u{201d}"))
                                .size(14.0)
                                .color(theme::p32().text),
                        );
                    }
                    if !r.note.is_empty() {
                        ui.label(
                            egui::RichText::new(format!("Note: {}", r.note))
                                .size(13.0)
                                .color(theme::p32().text),
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
                        .color(theme::p32().text_dim),
                    );
                });
            ui.add_space(8.0);
            let account = admin.account_named(&r.subject_username).cloned();
            let about = admin.reports_about(&r.subject_username);
            let manage = Offers::of(identity, admin.demo).accounts;
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
                    gold(),
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
            crate::systems::theme::text_field(
                ui,
                egui::TextEdit::singleline(&mut admin.resolution)
                    .id(egui::Id::new("admin_resolution"))
                    .desired_width(ui.available_width())
                    .hint_text("e.g. \"warned in chat\", \"suspended\""),
            );
            ui.add_space(8.0);
            let handled =
                theme::primary_button_sized(ui, "Mark handled", ui.available_width()).clicked();
            if handled {
                admin.mark_handled(id);
            }
            if let (true, Some(a)) = (manage, account.filter(|a| !a.disabled && !a.admin)) {
                ui.add_space(6.0);
                if theme::confirm_button(
                    ui,
                    egui::Id::new(("admin_suspend", &a.id)),
                    &format!("Suspend {}", a.username),
                    "Suspend: they can't sign in",
                ) {
                    admin.suspend(&a, true);
                }
            }
        });
}

fn accounts_page(ui: &mut egui::Ui, admin: &mut AdminState) {
    if theme::back_title(ui, "Accounts") {
        admin.go_back();
        return;
    }
    crate::systems::theme::text_field(
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
                .filter(|a| account_matches(a, &needle))
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
                    let status = status_word(a);
                    let sub = account_sub(a);
                    let end = if status.is_empty() {
                        theme::RowEnd::Chevron
                    } else {
                        theme::RowEnd::Word(status)
                    };
                    let tone = if a.disabled {
                        theme::p32().error
                    } else {
                        gold()
                    };
                    let icon = if a.disabled {
                        icons::CLEAR
                    } else {
                        icons::APPEARANCE
                    };
                    if theme::list_row(ui, icon, &a.username, Some(&sub), end, tone).clicked() {
                        admin.open(Page::Account(a.id.clone()));
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
pub fn short_date(stamp: &str) -> String {
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
            admin.go_back();
        }
        load_state(ui, &admin.accounts);
        return;
    };
    if theme::back_title(ui, &a.username) {
        admin.go_back();
        return;
    }
    let held = admin.held_roles(&a.id);
    let offers = Offers::of(identity, admin.demo);
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            theme::note(ui, &account_facts(&a));
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
                    ui.add_enabled_ui(offers.accounts, |ui| {
                        if theme::checkbox(ui, &mut can, CAN_SIGN_IN).changed() {
                            admin.suspend(&a, !can);
                        }
                    });
                });
                ui.add_space(6.0);
                let reports = admin.reports_about(&a.username);
                if theme::list_row(
                    ui,
                    icons::FLAG,
                    &format!("Reports about {}", a.username),
                    None,
                    theme::RowEnd::Count(reports),
                    gold(),
                )
                .clicked()
                {
                    admin.reports_of();
                }
            });
            // Roles: ticked for each one held. `player` is everybody's.
            if let Some(roles) = admin.roles.ready().cloned() {
                theme::section_tinted(ui, "Roles", gold());
                let mut toggled: Option<(String, bool)> = None;
                theme::list_group(ui, |ui| {
                    ui.add_space(6.0);
                    for role in roles.iter().filter(|r| r.id != "player") {
                        let mut on = held.contains(&role.id);
                        ui.horizontal(|ui| {
                            ui.add_space(14.0);
                            ui.add_enabled_ui(offers.assign_roles, |ui| {
                                if theme::checkbox(ui, &mut on, &role.name).changed() {
                                    toggled = Some((role.id.clone(), on));
                                }
                            });
                        });
                    }
                    ui.add_space(6.0);
                });
                if let Some((role, on)) = toggled {
                    admin.set_role(&a, &role, on);
                }
            }
            ui.add_space(10.0);
            if offers.accounts
                && !a.admin
                && theme::confirm_button(
                    ui,
                    egui::Id::new(("admin_delete", &a.id)),
                    "Delete account…",
                    "Delete permanently",
                )
            {
                admin.delete_account(&a);
            }
        });
}

fn roles_page(ui: &mut egui::Ui, admin: &mut AdminState) {
    if theme::back_title(ui, "Roles") {
        admin.go_back();
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
                                        .color(theme::p32().cream),
                                );
                                if role.built_in {
                                    chip(ui, "built in", theme::p32().text_dim);
                                }
                            });
                            let people = role_people(role);
                            ui.label(
                                egui::RichText::new(people)
                                    .size(12.0)
                                    .color(theme::p32().text_dim),
                            );
                            let may = role_may(role);
                            if !may.is_empty() {
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(may.join(" · "))
                                            .size(12.0)
                                            .color(theme::p32().text),
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
        admin.go_back();
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
                            ui.label(
                                egui::RichText::new(claim_place(c))
                                    .size(14.5)
                                    .color(theme::p32().cream),
                            );
                            ui.label(
                                egui::RichText::new(claim_holder(c))
                                    .size(12.0)
                                    .color(theme::p32().text_dim),
                            );
                            if theme::confirm_button(
                                ui,
                                egui::Id::new(("admin_release", &c.pand_id)),
                                "Release…",
                                "Release: anyone may claim it",
                            ) {
                                admin.release_claim(c);
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

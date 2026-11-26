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
//! on the IO pool posts its answer into a shared inbox, and [`drive_admin`]
//! drains it the next frame. The library draws the pages (`systems::lib_admin`).

use bevy::prelude::*;
use bevy::tasks::IoTaskPool;
use cartopolis_core::permissions::Permission;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use crate::resources::{AuthIdentity, OnlineSession, RightPanel};
use crate::systems::chats::ChatsView;

/// The account page's sign-in switch.
pub const CAN_SIGN_IN: &str = "Can sign in (off: stopped at the next sign-in)";

/// How often the open-report count behind the You page's badge is refreshed.
const REPORT_POLL_SECS: f32 = 120.0;

/// The panel's width on a wide screen.
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

    /// A report in the queue, by id.
    pub fn report(&self, id: i64) -> Option<&Report> {
        self.reports.ready()?.iter().find(|r| r.id == id)
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

/// The Admin panel's state, every frame: drains the HTTP replies, keeps the
/// You page's badge, closes the panel for an account that may not run
/// anything, and settles a report page whose report is gone. The library
/// draws the pages (`systems::lib_admin`).
pub fn drive_admin(
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
    if right_panel.is(RightPanel::Admin) {
        admin.settle();
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

/// `2026-10-16…` as `16 Oct 2026`.
pub fn short_date(stamp: &str) -> String {
    let t = short_time(stamp);
    let mut parts = t.split(' ');
    match (parts.next(), parts.next(), stamp.get(..4)) {
        (Some(d), Some(m), Some(y)) if !m.is_empty() => format!("{d} {m} {y}"),
        _ => stamp.to_string(),
    }
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

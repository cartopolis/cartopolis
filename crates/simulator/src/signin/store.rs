//! What signing in with other accounts keeps in the database (V41): the
//! operator's provider settings, who each account is elsewhere, the fediverse
//! servers this one has registered with, and communities' linked groups.

use anyhow::Context as _;
use rusqlite::{params, Connection, OptionalExtension as _};
use uuid::Uuid;

use super::{Kind, NewAccounts, PRESETS};

/// One provider as Admin › Sign-in and every round trip see it: a preset
/// nobody has touched yet, or a row.
#[derive(Debug, Clone, PartialEq)]
pub struct Provider {
    pub id: String,
    pub kind: Kind,
    pub name: String,
    /// Where an OpenID provider publishes its configuration.
    pub issuer: Option<String>,
    pub client_id: String,
    pub secret_sealed: Option<Vec<u8>>,
    pub enabled: bool,
    pub position: i64,
    pub new_accounts: NewAccounts,
    pub tested_at: Option<i64>,
    /// Added by the operator ("Another OpenID server") rather than built in.
    pub custom: bool,
}

impl Provider {
    /// Whether the operator has given it what it needs to work at all.
    #[must_use]
    pub fn has_keys(&self) -> bool {
        self.kind == Kind::Mastodon || (!self.client_id.is_empty() && self.secret_sealed.is_some())
    }

    /// Whether sign-in buttons may offer it: switched on, with its keys, and
    /// — except the fediverse, which has neither — tested.
    #[must_use]
    pub fn live(&self) -> bool {
        self.enabled && self.has_keys() && (self.kind == Kind::Mastodon || self.tested_at.is_some())
    }
}

fn preset(index: usize) -> Provider {
    let p = &PRESETS[index];
    Provider {
        id: p.id.into(),
        kind: p.kind,
        name: p.name.into(),
        issuer: p.issuer.map(str::to_string),
        client_id: String::new(),
        secret_sealed: None,
        enabled: false,
        position: 1000 + index as i64,
        new_accounts: NewAccounts::Anyone,
        tested_at: None,
        custom: false,
    }
}

fn row_to_provider(r: &rusqlite::Row<'_>) -> rusqlite::Result<Provider> {
    let id: String = r.get(0)?;
    let kind: String = r.get(1)?;
    Ok(Provider {
        custom: !PRESETS.iter().any(|p| p.id == id),
        id,
        kind: Kind::parse(&kind).unwrap_or(Kind::Oidc),
        name: r.get(2)?,
        issuer: r.get(3)?,
        client_id: r.get(4)?,
        secret_sealed: r.get(5)?,
        enabled: r.get::<_, i64>(6)? != 0,
        position: r.get(7)?,
        new_accounts: NewAccounts::from_i64(r.get(8)?),
        tested_at: r.get(9)?,
    })
}

const PROVIDER_COLUMNS: &str =
    "id, kind, name, issuer, client_id, secret_sealed, enabled, position, new_accounts, tested_at";

/// Every provider, in the order the buttons appear: the rows by their
/// position, then the presets nobody has set up yet.
pub fn all_providers(conn: &Connection) -> anyhow::Result<Vec<Provider>> {
    let mut stmt = conn.prepare(&format!("SELECT {PROVIDER_COLUMNS} FROM signin_providers"))?;
    let mut list: Vec<Provider> = stmt
        .query_map([], row_to_provider)?
        .collect::<Result<_, _>>()?;
    for (i, p) in PRESETS.iter().enumerate() {
        if !list.iter().any(|r| r.id == p.id) {
            list.push(preset(i));
        }
    }
    list.sort_by(|a, b| a.position.cmp(&b.position).then(a.id.cmp(&b.id)));
    Ok(list)
}

/// The providers sign-in buttons offer, in order.
pub fn enabled_providers(conn: &Connection) -> anyhow::Result<Vec<Provider>> {
    Ok(all_providers(conn)?.into_iter().filter(Provider::live).collect())
}

/// One provider by id: its row, or the preset of that name.
pub fn provider(conn: &Connection, id: &str) -> anyhow::Result<Option<Provider>> {
    let row = conn
        .query_row(
            &format!("SELECT {PROVIDER_COLUMNS} FROM signin_providers WHERE id = ?1"),
            [id],
            row_to_provider,
        )
        .optional()?;
    Ok(row.or_else(|| PRESETS.iter().position(|p| p.id == id).map(preset)))
}

/// Write a provider's settings, making its row if it has none.
pub fn save_provider(conn: &Connection, p: &Provider) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO signin_providers
             (id, kind, name, issuer, client_id, secret_sealed, enabled, position, new_accounts, tested_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT (id) DO UPDATE SET
             kind = ?2, name = ?3, issuer = ?4, client_id = ?5, secret_sealed = ?6,
             enabled = ?7, position = ?8, new_accounts = ?9, tested_at = ?10",
        params![
            p.id,
            p.kind.as_str(),
            p.name,
            p.issuer,
            p.client_id,
            p.secret_sealed,
            i64::from(p.enabled),
            p.position,
            p.new_accounts.as_i64(),
            p.tested_at,
        ],
    )
    .context("save sign-in provider")?;
    Ok(())
}

/// Put the providers in this order; any not named keep their place after them.
pub fn set_order(conn: &Connection, ids: &[String]) -> anyhow::Result<()> {
    let tx = conn.unchecked_transaction()?;
    for p in all_providers(&tx)? {
        let position = ids
            .iter()
            .position(|id| *id == p.id)
            .map_or(p.position + ids.len() as i64, |i| i as i64);
        save_provider(&tx, &Provider { position, ..p })?;
    }
    tx.commit()?;
    Ok(())
}

/// Forget a provider's settings: a custom one goes, a preset returns to "not
/// set up". The accounts linked through it keep their rows, so switching it on
/// again finds them; they cannot sign in with it meanwhile.
pub fn delete_provider(conn: &Connection, id: &str) -> anyhow::Result<()> {
    conn.execute("DELETE FROM signin_providers WHERE id = ?1", [id])?;
    Ok(())
}

/// How many accounts are linked through `provider`, and how many communities'
/// groups name it.
pub fn usage(conn: &Connection, provider: &str) -> anyhow::Result<(i64, i64)> {
    let accounts = conn.query_row(
        "SELECT COUNT(*) FROM account_identities WHERE provider = ?1",
        [provider],
        |r| r.get(0),
    )?;
    let links = conn.query_row(
        "SELECT COUNT(*) FROM community_links WHERE provider = ?1",
        [provider],
        |r| r.get(0),
    )?;
    Ok((accounts, links))
}

// ── Identities ─────────────────────────────────────────────────────────────

/// One way an account signs in, besides its password.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Identity {
    pub id: String,
    pub provider: String,
    pub handle: String,
    pub linked_at: i64,
}

/// The account `subject` at `provider` is linked to: its id and name, if it
/// is still there and not switched off.
pub fn find_identity(
    conn: &Connection,
    provider: &str,
    subject: &str,
) -> anyhow::Result<Option<(Uuid, String)>> {
    let row: Option<(String, String)> = conn
        .query_row(
            "SELECT a.id, a.username FROM account_identities i
               JOIN accounts a ON a.id = i.account
              WHERE i.provider = ?1 AND i.subject = ?2 AND a.disabled = 0",
            params![provider, subject],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    Ok(row.and_then(|(id, name)| Uuid::parse_str(&id).ok().map(|u| (u, name))))
}

/// Link `subject` at `provider` to `account`. A subject already linked to
/// somebody is refused: one Discord account signs in to one account here.
pub fn add_identity(
    conn: &Connection,
    provider: &str,
    subject: &str,
    account: Uuid,
    handle: &str,
    now_s: i64,
) -> anyhow::Result<bool> {
    let made = conn.execute(
        "INSERT OR IGNORE INTO account_identities (id, provider, subject, account, handle, linked_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            Uuid::new_v4().to_string(),
            provider,
            subject,
            account.to_string(),
            handle,
            now_s
        ],
    )?;
    if made == 0 {
        // Already there: fine when it is this account's (linking twice), a
        // refusal when it is somebody else's. The handle may have changed.
        let owner: String = conn.query_row(
            "SELECT account FROM account_identities WHERE provider = ?1 AND subject = ?2",
            params![provider, subject],
            |r| r.get(0),
        )?;
        if owner != account.to_string() {
            return Ok(false);
        }
        conn.execute(
            "UPDATE account_identities SET handle = ?3 WHERE provider = ?1 AND subject = ?2",
            params![provider, subject, handle],
        )?;
    }
    Ok(true)
}

/// Every way `account` signs in through another account.
pub fn identities_of(conn: &Connection, account: Uuid) -> anyhow::Result<Vec<Identity>> {
    let mut stmt = conn.prepare(
        "SELECT id, provider, handle, linked_at FROM account_identities
          WHERE account = ?1 ORDER BY linked_at",
    )?;
    let list = stmt
        .query_map([account.to_string()], |r| {
            Ok(Identity {
                id: r.get(0)?,
                provider: r.get(1)?,
                handle: r.get(2)?,
                linked_at: r.get(3)?,
            })
        })?
        .collect::<Result<_, _>>()?;
    Ok(list)
}

/// What removing one of an account's sign-ins came to.
#[derive(Debug, PartialEq, Eq)]
pub enum Unlink {
    Removed,
    /// Not this account's, or not there.
    Unknown,
    /// It is the only way in: no password and nothing else linked.
    LastWayIn,
}

pub fn remove_identity(conn: &Connection, account: Uuid, id: &str) -> anyhow::Result<Unlink> {
    let tx = conn.unchecked_transaction()?;
    let mine: bool = tx
        .query_row(
            "SELECT 1 FROM account_identities WHERE id = ?1 AND account = ?2",
            params![id, account.to_string()],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if !mine {
        return Ok(Unlink::Unknown);
    }
    let others: i64 = tx.query_row(
        "SELECT COUNT(*) FROM account_identities WHERE account = ?1 AND id != ?2",
        params![account.to_string(), id],
        |r| r.get(0),
    )?;
    if others == 0 && !has_password(&tx, account)? {
        return Ok(Unlink::LastWayIn);
    }
    tx.execute("DELETE FROM account_identities WHERE id = ?1", [id])?;
    tx.commit()?;
    Ok(Unlink::Removed)
}

/// Whether the account has a password of its own.
pub fn has_password(conn: &Connection, account: Uuid) -> anyhow::Result<bool> {
    let phc: Option<String> = conn
        .query_row(
            "SELECT pw_hash FROM accounts WHERE id = ?1",
            [account.to_string()],
            |r| r.get(0),
        )
        .optional()?;
    Ok(phc.is_some_and(|p| crate::auth::has_password(&p)))
}

/// A deleted account's sign-ins go with it (`db::erase_account_traces`).
pub fn forget_account(conn: &Connection, account: &str) -> anyhow::Result<()> {
    conn.execute("DELETE FROM account_identities WHERE account = ?1", [account])?;
    Ok(())
}

/// The account a verified address belongs to, if any: its name.
pub fn email_owner(conn: &Connection, email: &str) -> anyhow::Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT username FROM accounts WHERE lower(email) = lower(?1)",
            [email],
            |r| r.get(0),
        )
        .optional()?)
}

/// What making an account through another one came to.
#[derive(Debug, PartialEq, Eq)]
pub enum Made {
    Created(Uuid),
    NameTaken,
    /// The address belongs to another account (somebody typed one in by
    /// hand that is already registered here).
    EmailTaken,
    /// The other account is linked here already (the same round trip spent
    /// twice, in two tabs).
    AlreadyLinked,
}

/// Make an account with no password, already linked to `subject` at
/// `provider`, with the address the provider vouched for, stored confirmed.
///
/// **Never an address typed in by hand.** An address without a confirmation
/// is a pending registration to `registration::sweep`, which deletes the
/// account once the link expires — so an account made through Reddit with an
/// unconfirmed address would vanish a day later. Reddit and the fediverse
/// give no address, and these accounts have none.
pub fn create_account(
    conn: &mut Connection,
    username: &str,
    email: Option<&str>,
    provider: &str,
    subject: &str,
    handle: &str,
    now_s: i64,
) -> anyhow::Result<Made> {
    let tx = conn.transaction()?;
    if crate::registration::name_taken(&tx, username)? {
        return Ok(Made::NameTaken);
    }
    if let Some(email) = email {
        if email_owner(&tx, email)?.is_some() {
            return Ok(Made::EmailTaken);
        }
    }
    let linked: bool = tx
        .query_row(
            "SELECT 1 FROM account_identities WHERE provider = ?1 AND subject = ?2",
            params![provider, subject],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if linked {
        return Ok(Made::AlreadyLinked);
    }
    let principal = Uuid::new_v4();
    tx.execute(
        "INSERT INTO accounts (id, username, pw_hash, email, email_verified_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            principal.to_string(),
            username,
            crate::auth::NO_PASSWORD,
            email.map(str::to_lowercase),
            email.is_some().then_some(now_s),
        ],
    )
    .context("insert account")?;
    add_identity(&tx, provider, subject, principal, handle, now_s)?;
    tx.commit()?;
    Ok(Made::Created(principal))
}

// ── Communities' linked groups ─────────────────────────────────────────────

/// A community linked to a Discord server or a subreddit.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CommunityLink {
    pub provider: String,
    pub group_id: String,
    pub group_name: String,
    pub linked_by: String,
    pub linked_at: i64,
}

pub fn links_of(conn: &Connection, community: Uuid) -> anyhow::Result<Vec<CommunityLink>> {
    let mut stmt = conn.prepare(
        "SELECT l.provider, l.group_id, l.group_name, COALESCE(a.username, '[deleted]'), l.linked_at
           FROM community_links l LEFT JOIN accounts a ON a.id = l.linked_by
          WHERE l.community = ?1 ORDER BY l.linked_at",
    )?;
    let list = stmt
        .query_map([community.to_string()], |r| {
            Ok(CommunityLink {
                provider: r.get(0)?,
                group_id: r.get(1)?,
                group_name: r.get(2)?,
                linked_by: r.get(3)?,
                linked_at: r.get(4)?,
            })
        })?
        .collect::<Result<_, _>>()?;
    Ok(list)
}

/// Link a group to a community. `false` when the group is linked to a
/// different community already: one Discord server brings its people to one
/// place.
pub fn add_link(
    conn: &Connection,
    community: Uuid,
    provider: &str,
    group_id: &str,
    group_name: &str,
    by: Uuid,
    now_s: i64,
) -> anyhow::Result<bool> {
    let existing: Option<String> = conn
        .query_row(
            "SELECT community FROM community_links WHERE provider = ?1 AND group_id = ?2",
            params![provider, group_id],
            |r| r.get(0),
        )
        .optional()?;
    if existing.as_deref().is_some_and(|c| c != community.to_string()) {
        return Ok(false);
    }
    conn.execute(
        "INSERT OR REPLACE INTO community_links
             (community, provider, group_id, group_name, linked_by, linked_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            community.to_string(),
            provider,
            group_id,
            group_name,
            by.to_string(),
            now_s
        ],
    )?;
    Ok(true)
}

pub fn remove_link(
    conn: &Connection,
    community: Uuid,
    provider: &str,
    group_id: &str,
) -> anyhow::Result<bool> {
    Ok(conn.execute(
        "DELETE FROM community_links WHERE community = ?1 AND provider = ?2 AND group_id = ?3",
        params![community.to_string(), provider, group_id],
    )? > 0)
}

/// Whether any community is linked to a group at `provider` — whether a
/// sign-in there should ask which groups the person is in at all.
pub fn any_links(conn: &Connection, provider: &str) -> anyhow::Result<bool> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM community_links WHERE provider = ?1 LIMIT 1",
            [provider],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

/// A community somebody may join because they are in one of its groups.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Offer {
    pub community: String,
    pub name: String,
    pub group_name: String,
    pub provider: String,
    pub members: i64,
}

/// The communities linked to any of `groups` at `provider`, leaving out those
/// `account` is already in.
pub fn offers(
    conn: &Connection,
    provider: &str,
    groups: &[String],
    account: Option<Uuid>,
) -> anyhow::Result<Vec<Offer>> {
    let mut out: Vec<Offer> = Vec::new();
    let mut stmt = conn.prepare(
        "SELECT l.community, c.name, l.group_name,
                (SELECT COUNT(*) FROM community_members m WHERE m.community = l.community)
           FROM community_links l JOIN communities c ON c.id = l.community
          WHERE l.provider = ?1 AND l.group_id = ?2",
    )?;
    for group in groups {
        let rows: Vec<(String, String, String, i64)> = stmt
            .query_map(params![provider, group], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })?
            .collect::<Result<_, _>>()?;
        for (community, name, group_name, members) in rows {
            if out.iter().any(|o| o.community == community) {
                continue;
            }
            if let Some(account) = account {
                let id = Uuid::parse_str(&community).unwrap_or_else(|_| Uuid::nil());
                if crate::communities::role(conn, id, &account.to_string())
                    .map_err(|e| anyhow::anyhow!("{e}"))?
                    .is_some()
                {
                    continue;
                }
            }
            out.push(Offer {
                community,
                name,
                group_name,
                provider: provider.to_string(),
                members,
            });
        }
    }
    Ok(out)
}

// ── The fediverse's per-server registrations ──────────────────────────────

pub fn fediverse_app(conn: &Connection, host: &str) -> anyhow::Result<Option<(String, Vec<u8>)>> {
    Ok(conn
        .query_row(
            "SELECT client_id, secret_sealed FROM fediverse_signin_apps WHERE host = ?1",
            [host],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?)
}

pub fn save_fediverse_app(
    conn: &Connection,
    host: &str,
    client_id: &str,
    secret_sealed: &[u8],
    now_s: i64,
) -> anyhow::Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO fediverse_signin_apps (host, client_id, secret_sealed, created_at)
         VALUES (?1, ?2, ?3, ?4)",
        params![host, client_id, secret_sealed, now_s],
    )?;
    Ok(())
}

/// Forget a server's registration, when it no longer recognises it (the
/// server's database was reset): the next sign-in from there registers anew.
pub fn forget_fediverse_app(conn: &Connection, host: &str) -> anyhow::Result<()> {
    conn.execute("DELETE FROM fediverse_signin_apps WHERE host = ?1", [host])?;
    Ok(())
}

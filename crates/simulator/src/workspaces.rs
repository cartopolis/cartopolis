//! Workspaces and their channels (protocol 38, `docs/notes/web-chat.md`).
//!
//! A workspace is a named set of people and channels; a channel is an
//! encrypted chat (`e2ee`) that the server also lists, by name and topic, to
//! people who are not in it yet. Those two strings, the member list and who is
//! in which channel are what the server sees; what is said is sealed.
//!
//! The rules, all enforced here:
//!
//! 1. **Only a member sees a workspace**, and only an owner or an admin adds
//!    people to it.
//! 2. **A channel is a chat its creator is already in**, registered on this
//!    server, and listed in a workspace its creator belongs to. Its name is
//!    unique within the workspace.
//! 3. **Anyone in the workspace may ask into a public channel**; nobody may ask
//!    into a private one. The server cannot add anybody to an encrypted chat —
//!    only a member's device can — so asking records a request
//!    (`channel_joins`) that a member device settles by committing them in.
//!
//! Plain functions over a `&Connection`, as `e2ee` is, so the tests below run
//! the rules against a real database with no network.

pub use cartopolis_core::channel_name;
use cartopolis_core::{ChannelDto, WorkspaceDto, WorkspaceMemberDto, WorkspaceRole};
use rusqlite::{params, Connection, OptionalExtension};
use uuid::Uuid;

/// Longest workspace or channel name, in characters.
pub const MAX_NAME: usize = 80;
/// Longest channel topic, in characters.
pub const MAX_TOPIC: usize = 250;
/// Workspaces one account may create. A bound so one account cannot fill the
/// table; a real team needs one or two.
pub const MAX_CREATED: i64 = 20;

/// Why a request was refused, in words the client can show.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Refusal {
    #[error("a name needs at least one letter or digit")]
    EmptyName,
    #[error("that name is too long")]
    TooLong,
    #[error("you have started as many workspaces as one account may")]
    TooMany,
    #[error("no such workspace")]
    NoSuchWorkspace,
    #[error("only an owner or an admin may do that")]
    NotAnAdmin,
    #[error("no such person")]
    NoSuchPerson,
    #[error("a channel by that name already exists")]
    NameTaken,
    #[error("that chat is not yours to list as a channel")]
    NotYourChat,
    #[error("no such channel")]
    NoSuchChannel,
    #[error("a private channel can only be joined by being added")]
    Private,
    #[error("the server could not store that: {0}")]
    Storage(String),
}

impl From<rusqlite::Error> for Refusal {
    fn from(e: rusqlite::Error) -> Self {
        Refusal::Storage(e.to_string())
    }
}

fn role_of(n: i64) -> WorkspaceRole {
    match n {
        0 => WorkspaceRole::Owner,
        1 => WorkspaceRole::Admin,
        _ => WorkspaceRole::Member,
    }
}

fn role_number(r: WorkspaceRole) -> i64 {
    match r {
        WorkspaceRole::Owner => 0,
        WorkspaceRole::Admin => 1,
        WorkspaceRole::Member => 2,
    }
}

/// A workspace's name as stored: trimmed, inner whitespace collapsed.
fn clean_name(name: &str) -> Result<String, Refusal> {
    let name = name.split_whitespace().collect::<Vec<_>>().join(" ");
    if !name.chars().any(char::is_alphanumeric) {
        return Err(Refusal::EmptyName);
    }
    if name.chars().count() > MAX_NAME {
        return Err(Refusal::TooLong);
    }
    Ok(name)
}

fn role(
    conn: &Connection,
    workspace: &str,
    account: &str,
) -> Result<Option<WorkspaceRole>, Refusal> {
    Ok(conn
        .query_row(
            "SELECT role FROM workspace_members WHERE workspace = ?1 AND account = ?2",
            params![workspace, account],
            |r| r.get::<_, i64>(0),
        )
        .optional()?
        .map(role_of))
}

/// Create a workspace with `account` as its owner.
pub fn create(conn: &Connection, account: &str, name: &str, now_ms: i64) -> Result<Uuid, Refusal> {
    let name = clean_name(name)?;
    let made: i64 = conn.query_row(
        "SELECT COUNT(*) FROM workspaces WHERE created_by = ?1",
        params![account],
        |r| r.get(0),
    )?;
    if made >= MAX_CREATED {
        return Err(Refusal::TooMany);
    }
    let id = Uuid::new_v4();
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "INSERT INTO workspaces (id, name, created_by, created_ms) VALUES (?1, ?2, ?3, ?4)",
        params![id.to_string(), name, account, now_ms],
    )?;
    tx.execute(
        "INSERT INTO workspace_members (workspace, account, role, joined_ms) VALUES (?1, ?2, ?3, ?4)",
        params![id.to_string(), account, role_number(WorkspaceRole::Owner), now_ms],
    )?;
    tx.commit()?;
    Ok(id)
}

/// Add the account called `username` to a workspace. Only an owner or an admin
/// may. Adding somebody already in it changes nothing.
pub fn add_member(
    conn: &Connection,
    by: &str,
    workspace: Uuid,
    username: &str,
    now_ms: i64,
) -> Result<String, Refusal> {
    let ws = workspace.to_string();
    match role(conn, &ws, by)? {
        Some(WorkspaceRole::Owner | WorkspaceRole::Admin) => {}
        Some(WorkspaceRole::Member) => return Err(Refusal::NotAnAdmin),
        None => return Err(Refusal::NoSuchWorkspace),
    }
    let account = crate::db::enabled_account_id(conn, username)
        .map_err(|e| Refusal::Storage(e.to_string()))?
        .ok_or(Refusal::NoSuchPerson)?;
    conn.execute(
        "INSERT OR IGNORE INTO workspace_members (workspace, account, role, joined_ms)
         VALUES (?1, ?2, ?3, ?4)",
        params![ws, account, role_number(WorkspaceRole::Member), now_ms],
    )?;
    Ok(account)
}

/// List a chat as a channel of a workspace (rule 2).
#[allow(clippy::too_many_arguments)]
pub fn create_channel(
    conn: &Connection,
    account: &str,
    workspace: Uuid,
    group: &[u8],
    name: &str,
    topic: &str,
    private: bool,
    now_ms: i64,
) -> Result<(), Refusal> {
    let ws = workspace.to_string();
    if role(conn, &ws, account)?.is_none() {
        return Err(Refusal::NoSuchWorkspace);
    }
    let name = channel_name(name);
    if name.is_empty() {
        return Err(Refusal::EmptyName);
    }
    if name.chars().count() > MAX_NAME {
        return Err(Refusal::TooLong);
    }
    let topic = topic.trim();
    if topic.chars().count() > MAX_TOPIC {
        return Err(Refusal::TooLong);
    }
    // The chat must be one its creator registered and is in, and not a direct
    // chat: a direct chat is between two people and stays that.
    let mine: bool = conn
        .query_row(
            "SELECT 1 FROM e2ee_groups g JOIN e2ee_members m ON m.group_id = g.id
             WHERE g.id = ?1 AND g.created_by = ?2 AND m.account = ?2 AND g.direct_a IS NULL",
            params![group, account],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if !mine {
        return Err(Refusal::NotYourChat);
    }
    match conn.execute(
        "INSERT INTO channels (group_id, workspace, name, topic, private, created_by, created_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![group, ws, name, topic, private, account, now_ms],
    ) {
        Ok(_) => Ok(()),
        Err(rusqlite::Error::SqliteFailure(e, _))
            if e.code == rusqlite::ErrorCode::ConstraintViolation =>
        {
            Err(Refusal::NameTaken)
        }
        Err(e) => Err(e.into()),
    }
}

/// Ask into a public channel (rule 3). Answers the accounts already in it, one
/// of whose devices the caller hands the request to; empty when the asker is
/// already in it.
pub fn request_join(
    conn: &Connection,
    account: &str,
    workspace: Uuid,
    group: &[u8],
    now_ms: i64,
) -> Result<Vec<String>, Refusal> {
    let ws = workspace.to_string();
    if role(conn, &ws, account)?.is_none() {
        return Err(Refusal::NoSuchWorkspace);
    }
    let private: Option<bool> = conn
        .query_row(
            "SELECT private FROM channels WHERE group_id = ?1 AND workspace = ?2",
            params![group, ws],
            |r| r.get(0),
        )
        .optional()?;
    match private {
        None => return Err(Refusal::NoSuchChannel),
        Some(true) => return Err(Refusal::Private),
        Some(false) => {}
    }
    let members = crate::e2ee::members(conn, group).map_err(|e| Refusal::Storage(e.to_string()))?;
    if members.iter().any(|m| m == account) {
        return Ok(Vec::new());
    }
    conn.execute(
        "INSERT OR IGNORE INTO channel_joins (group_id, account, requested_ms) VALUES (?1, ?2, ?3)",
        params![group, account, now_ms],
    )?;
    Ok(members)
}

/// Join requests a device of `account` could settle: for every channel it is
/// in, who is waiting to be added. Handed to a member's device when it
/// connects, so a request made while nobody was online is not lost.
pub fn pending_for(
    conn: &Connection,
    account: &str,
) -> Result<Vec<(Uuid, Vec<u8>, String)>, Refusal> {
    let mut stmt = conn.prepare(
        "SELECT c.workspace, j.group_id, a.username
         FROM channel_joins j
         JOIN channels c ON c.group_id = j.group_id
         JOIN e2ee_members m ON m.group_id = j.group_id AND m.account = ?1
         JOIN accounts a ON a.id = j.account
         ORDER BY j.requested_ms",
    )?;
    let rows = stmt.query_map(params![account], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, Vec<u8>>(1)?,
            r.get::<_, String>(2)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (ws, group, username) = row?;
        if let Ok(ws) = Uuid::parse_str(&ws) {
            out.push((ws, group, username));
        }
    }
    Ok(out)
}

/// Settle the join requests of `added` (account ids) to `group`: they are in.
/// Called by `e2ee::append` when a commit's invitation names them.
pub fn settle_joins(
    conn: &Connection,
    group: &[u8],
    added: &[String],
) -> Result<(), rusqlite::Error> {
    for account in added {
        conn.execute(
            "DELETE FROM channel_joins WHERE group_id = ?1 AND account = ?2",
            params![group, account],
        )?;
    }
    Ok(())
}

/// The accounts in a workspace, for telling them it changed.
pub fn member_accounts(conn: &Connection, workspace: Uuid) -> Result<Vec<String>, Refusal> {
    let mut stmt = conn.prepare("SELECT account FROM workspace_members WHERE workspace = ?1")?;
    let rows = stmt.query_map(params![workspace.to_string()], |r| r.get::<_, String>(0))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// The workspace a channel belongs to, if `group` is a channel.
pub fn workspace_of(conn: &Connection, group: &[u8]) -> Result<Option<Uuid>, Refusal> {
    let ws: Option<String> = conn
        .query_row(
            "SELECT workspace FROM channels WHERE group_id = ?1",
            params![group],
            |r| r.get(0),
        )
        .optional()?;
    Ok(ws.and_then(|w| Uuid::parse_str(&w).ok()))
}

/// Every workspace `account` belongs to, as it sees them: every public
/// channel and the private ones it is in. People are full addresses on
/// `server`.
pub fn list(conn: &Connection, account: &str, server: &str) -> Result<Vec<WorkspaceDto>, Refusal> {
    let mut stmt = conn.prepare(
        "SELECT w.id, w.name, m.role FROM workspaces w
         JOIN workspace_members m ON m.workspace = w.id AND m.account = ?1
         ORDER BY w.created_ms",
    )?;
    let rows = stmt.query_map(params![account], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, i64>(2)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (id, name, my_role) = row?;
        let Ok(uuid) = Uuid::parse_str(&id) else {
            continue;
        };
        let mut members_stmt = conn.prepare(
            "SELECT a.username, m.role FROM workspace_members m
             JOIN accounts a ON a.id = m.account
             WHERE m.workspace = ?1 ORDER BY m.role, a.username",
        )?;
        let members = members_stmt
            .query_map(params![id], |r| {
                Ok(WorkspaceMemberDto {
                    address: cartopolis_core::address::person(&r.get::<_, String>(0)?, server),
                    role: role_of(r.get(1)?),
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let mut ch_stmt = conn.prepare(
            "SELECT c.group_id, c.name, c.topic, c.private,
                    EXISTS (SELECT 1 FROM e2ee_members m WHERE m.group_id = c.group_id AND m.account = ?2),
                    EXISTS (SELECT 1 FROM channel_joins j WHERE j.group_id = c.group_id AND j.account = ?2),
                    (SELECT COUNT(*) FROM e2ee_members m WHERE m.group_id = c.group_id)
             FROM channels c WHERE c.workspace = ?1 ORDER BY c.name",
        )?;
        let channels = ch_stmt
            .query_map(params![id, account], |r| {
                Ok(ChannelDto {
                    group: r.get(0)?,
                    name: r.get(1)?,
                    topic: r.get(2)?,
                    private: r.get(3)?,
                    joined: r.get(4)?,
                    pending: r.get(5)?,
                    members: r.get::<_, i64>(6)?.max(0) as u32,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .filter(|c| !c.private || c.joined)
            .collect();
        out.push(WorkspaceDto {
            id: uuid,
            name,
            home: server.to_string(),
            role: role_of(my_role),
            members,
            channels,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn world() -> (tempfile::TempDir, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("w.db");
        let conn = crate::db::open_and_migrate(path.to_str().unwrap()).unwrap();
        for name in ["ann", "bob", "cas"] {
            crate::ensure_account(&conn, name, "pw").unwrap();
        }
        (dir, conn)
    }

    fn id(conn: &Connection, name: &str) -> String {
        crate::db::enabled_account_id(conn, name).unwrap().unwrap()
    }

    /// A chat registered by `account`, as `e2ee::create_group` would.
    fn chat(conn: &Connection, account: &str, n: u8) -> Vec<u8> {
        let group = cartopolis_core::address::group_id("localhost", [n; 16]);
        crate::e2ee::create_group(conn, account, &group, None).unwrap();
        group
    }

    #[test]
    fn channel_names_read_as_they_are_written_after_a_hash() {
        assert_eq!(channel_name("#Ebbinge Quarter"), "ebbinge-quarter");
        assert_eq!(channel_name("  design  review "), "design-review");
        assert_eq!(channel_name("!!!"), "");
    }

    #[test]
    fn only_members_see_a_workspace_and_only_admins_add_people() {
        let (_d, c) = world();
        let (ann, bob) = (id(&c, "ann"), id(&c, "bob"));
        let ws = create(&c, &ann, "  Atelier   Noord ", 0).unwrap();
        assert!(list(&c, &bob, "localhost").unwrap().is_empty());

        assert_eq!(
            add_member(&c, &bob, ws, "cas", 0),
            Err(Refusal::NoSuchWorkspace)
        );
        add_member(&c, &ann, ws, "Bob", 0).unwrap();
        assert_eq!(add_member(&c, &bob, ws, "cas", 0), Err(Refusal::NotAnAdmin));

        let seen = list(&c, &bob, "localhost").unwrap();
        assert_eq!(seen[0].name, "Atelier Noord");
        assert_eq!(seen[0].role, WorkspaceRole::Member);
        let people: Vec<_> = seen[0].members.iter().map(|m| m.address.as_str()).collect();
        assert_eq!(people, ["ann@localhost", "bob@localhost"]);
    }

    #[test]
    fn a_channel_is_a_chat_its_creator_is_in_with_a_unique_name() {
        let (_d, c) = world();
        let (ann, bob) = (id(&c, "ann"), id(&c, "bob"));
        let ws = create(&c, &ann, "Team", 0).unwrap();
        add_member(&c, &ann, ws, "bob", 0).unwrap();
        let g = chat(&c, &ann, 1);
        assert_eq!(
            create_channel(&c, &bob, ws, &g, "general", "", false, 0),
            Err(Refusal::NotYourChat)
        );
        create_channel(&c, &ann, ws, &g, "#General", "Everything", false, 0).unwrap();
        let g2 = chat(&c, &ann, 2);
        assert_eq!(
            create_channel(&c, &ann, ws, &g2, "general", "", false, 0),
            Err(Refusal::NameTaken)
        );
        let seen = list(&c, &bob, "localhost").unwrap();
        let general = &seen[0].channels[0];
        assert_eq!(general.name, "general");
        assert!(!general.joined && !general.pending && general.members == 1);
    }

    #[test]
    fn asking_into_a_public_channel_waits_for_a_member_and_a_private_one_is_closed() {
        let (_d, c) = world();
        let (ann, bob, cas) = (id(&c, "ann"), id(&c, "bob"), id(&c, "cas"));
        let ws = create(&c, &ann, "Team", 0).unwrap();
        add_member(&c, &ann, ws, "bob", 0).unwrap();
        let open = chat(&c, &ann, 1);
        let closed = chat(&c, &ann, 2);
        create_channel(&c, &ann, ws, &open, "general", "", false, 0).unwrap();
        create_channel(&c, &ann, ws, &closed, "partners", "", true, 0).unwrap();

        assert_eq!(
            request_join(&c, &cas, ws, &open, 0),
            Err(Refusal::NoSuchWorkspace)
        );
        assert_eq!(
            request_join(&c, &bob, ws, &closed, 0),
            Err(Refusal::Private)
        );
        assert_eq!(
            request_join(&c, &bob, ws, &open, 5).unwrap(),
            vec![ann.clone()]
        );

        // Bob sees it waiting and not the private one; Ann's device is asked.
        let seen = list(&c, &bob, "localhost").unwrap();
        assert_eq!(seen[0].channels.len(), 1);
        assert!(seen[0].channels[0].pending);
        assert_eq!(
            pending_for(&c, &ann).unwrap(),
            vec![(ws, open.clone(), "bob".into())]
        );
        assert!(pending_for(&c, &bob).unwrap().is_empty());

        settle_joins(&c, &open, &[bob.clone()]).unwrap();
        assert!(pending_for(&c, &ann).unwrap().is_empty());
    }
}

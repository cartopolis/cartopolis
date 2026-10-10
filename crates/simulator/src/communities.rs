//! Communities and their topics (protocol 39, `docs/notes/communities.md`).
//!
//! A community is a named set of people with a few topics; a topic is an
//! encrypted chat (`e2ee`) that every member is in. What the server sees is
//! what it needs to list one, route to it and enforce its moderation: the
//! name, the description, the topic names, who is in it with which role, who
//! is asking to join and what was reported. What is said stays sealed.
//!
//! The rules, all enforced here:
//!
//! 1. **Being in a community is being in all its topics** (bar one a moderator
//!    took somebody out of). The server cannot add anybody to an encrypted
//!    chat, so admitting somebody records a *join* per topic (`topic_joins`)
//!    and taking somebody out records a *removal* (`topic_removals`); a device
//!    already in the topic does the work, handed it by `net::communities`.
//! 2. **Three roles.** The owner (exactly one) appoints moderators, hands the
//!    community over and deletes it. Moderators let people in, add and remove
//!    them, delete messages and edit the community and its topics. Members
//!    talk, and start topics when the community allows it.
//! 3. **Three ways in.** Open: join at once. Ask: a moderator confirms; a
//!    refusal stands for a week. Invite: only people a moderator adds.
//! 4. **Moderation is the community's.** A server operator who triages reports
//!    may delete a community outright, and does nothing else inside it: they
//!    cannot read it.
//!
//! Plain functions over a `&Connection`, as `e2ee` is, so the tests below run
//! the rules against a real database with no network.

use cartopolis_core::{
    address, topic_name, CommunityDto, CommunityListing, CommunityMemberDto, CommunityPlace,
    CommunityReportDto, CommunityRole, CommunitySettings, DirectoryOrder, JoinRequestDto, JoinRule,
    ListingState, ReportReason, TopicDto,
};
use rusqlite::{params, Connection, OptionalExtension};
use uuid::Uuid;

/// Longest community or topic name, in characters.
pub const MAX_NAME: usize = 80;
/// Longest description, in characters.
pub const MAX_ABOUT: usize = 300;
/// Longest line to the moderators when asking to join.
pub const MAX_REQUEST_MESSAGE: usize = 300;
/// Communities one account may start. A bound so one account cannot fill the
/// table; a real person runs a handful.
pub const MAX_CREATED: i64 = 20;
/// Topics one community may have.
pub const MAX_TOPICS: i64 = 50;
/// How long a refusal stands before the person may ask again.
pub const REFUSAL_MS: i64 = 7 * 24 * 60 * 60 * 1000;
/// Entries a directory search answers with.
pub const DIRECTORY_LIMIT: i64 = 50;
/// How long a device handed topic work is given before another is.
pub const WORK_AGAIN_MS: i64 = 2 * 60 * 1000;

/// Why a request was refused, in words the client can show.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Refusal {
    #[error("a name needs at least one letter or digit")]
    EmptyName,
    #[error("that is too long")]
    TooLong,
    #[error("you have started as many communities as one account may")]
    TooMany,
    #[error("no such community")]
    NoSuchCommunity,
    #[error("only a moderator may do that")]
    NotAModerator,
    #[error("only the owner may do that")]
    NotTheOwner,
    #[error("no such person")]
    NoSuchPerson,
    #[error("they are not in this community")]
    NotAMember,
    #[error("a topic by that name already exists")]
    NameTaken,
    #[error("that chat is not yours to make a topic")]
    NotYourChat,
    #[error("no such topic")]
    NoSuchTopic,
    #[error("only moderators start topics here")]
    TopicsClosed,
    #[error("this community has as many topics as it may")]
    TooManyTopics,
    #[error("only people a moderator adds can join this community")]
    InviteOnly,
    #[error("you can't ask to join this community again yet")]
    Refused,
    #[error("you are already in it")]
    AlreadyIn,
    #[error("the owner can't leave; hand the community over first")]
    OwnerStays,
    #[error("the owner can't be removed")]
    OwnerCannotBeRemoved,
    #[error("no such message")]
    NoSuchMessage,
    #[error("no such report")]
    NoSuchReport,
    #[error("a community's picture is a hash in the picture store")]
    BadPicture,
    #[error("that is not a place on Earth")]
    BadPlace,
    #[error("that invite has expired or does not exist")]
    NoSuchInvite,
    #[error("the server could not store that: {0}")]
    Storage(String),
}

impl From<rusqlite::Error> for Refusal {
    fn from(e: rusqlite::Error) -> Self {
        Refusal::Storage(e.to_string())
    }
}

fn role_of(n: i64) -> CommunityRole {
    match n {
        0 => CommunityRole::Owner,
        1 => CommunityRole::Moderator,
        _ => CommunityRole::Member,
    }
}

fn role_number(r: CommunityRole) -> i64 {
    match r {
        CommunityRole::Owner => 0,
        CommunityRole::Moderator => 1,
        CommunityRole::Member => 2,
    }
}

fn rule_of(n: i64) -> JoinRule {
    match n {
        0 => JoinRule::Open,
        1 => JoinRule::Ask,
        _ => JoinRule::Invite,
    }
}

fn rule_number(r: JoinRule) -> i64 {
    match r {
        JoinRule::Open => 0,
        JoinRule::Ask => 1,
        JoinRule::Invite => 2,
    }
}

/// A name as stored: trimmed, inner whitespace collapsed.
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

fn clean_settings(s: &CommunitySettings) -> Result<CommunitySettings, Refusal> {
    let about = s.about.trim().to_string();
    if about.chars().count() > MAX_ABOUT {
        return Err(Refusal::TooLong);
    }
    // A picture is a hash in the picture store, nothing else: never a URL a
    // client would be sent off to fetch.
    if let Some(p) = &s.picture {
        if p.len() != 64 || !p.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(Refusal::BadPicture);
        }
    }
    if let Some(p) = &s.place {
        let ok = (-90.0..=90.0).contains(&p.latitude)
            && (-180.0..=180.0).contains(&p.longitude)
            && p.name.chars().count() <= MAX_NAME;
        if !ok {
            return Err(Refusal::BadPlace);
        }
    }
    Ok(CommunitySettings {
        name: clean_name(&s.name)?,
        about,
        ..s.clone()
    })
}

fn clean_topic(name: &str) -> Result<String, Refusal> {
    let name = topic_name(name);
    if name.is_empty() {
        return Err(Refusal::EmptyName);
    }
    if name.chars().count() > MAX_NAME {
        return Err(Refusal::TooLong);
    }
    Ok(name)
}

fn exists(conn: &Connection, id: &str) -> Result<bool, Refusal> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM communities WHERE id = ?1",
            params![id],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

/// `account`'s role in a community, if it is in it.
pub fn role(conn: &Connection, id: Uuid, account: &str) -> Result<Option<CommunityRole>, Refusal> {
    Ok(conn
        .query_row(
            "SELECT role FROM community_members WHERE community = ?1 AND account = ?2",
            params![id.to_string(), account],
            |r| r.get::<_, i64>(0),
        )
        .optional()?
        .map(role_of))
}

fn must_moderate(conn: &Connection, id: Uuid, account: &str) -> Result<CommunityRole, Refusal> {
    match role(conn, id, account)? {
        Some(r) if r.moderates() => Ok(r),
        Some(_) => Err(Refusal::NotAModerator),
        None if exists(conn, &id.to_string())? => Err(Refusal::NotAModerator),
        None => Err(Refusal::NoSuchCommunity),
    }
}

fn account_named(conn: &Connection, username: &str) -> Result<String, Refusal> {
    crate::db::enabled_account_id(conn, username)
        .map_err(|e| Refusal::Storage(e.to_string()))?
        .ok_or(Refusal::NoSuchPerson)
}

fn username_of(conn: &Connection, account: &str) -> Result<String, Refusal> {
    Ok(conn
        .query_row(
            "SELECT username FROM accounts WHERE id = ?1",
            params![account],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or_default())
}

/// The topics of a community, in order.
fn topic_groups(conn: &Connection, id: Uuid, with_archived: bool) -> Result<Vec<Vec<u8>>, Refusal> {
    let mut stmt = conn.prepare(
        "SELECT group_id FROM topics WHERE community = ?1 AND (?2 OR archived = 0)
         ORDER BY position, name",
    )?;
    let rows = stmt.query_map(params![id.to_string(), with_archived], |r| r.get(0))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Start a community with `account` as its owner.
pub fn create(
    conn: &Connection,
    account: &str,
    settings: &CommunitySettings,
    now_ms: i64,
) -> Result<Uuid, Refusal> {
    let s = clean_settings(settings)?;
    let made: i64 = conn.query_row(
        "SELECT COUNT(*) FROM communities WHERE created_by = ?1",
        params![account],
        |r| r.get(0),
    )?;
    if made >= MAX_CREATED {
        return Err(Refusal::TooMany);
    }
    let id = Uuid::new_v4();
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "INSERT INTO communities (id, name, about, join_rule, listed, members_start_topics, created_by, created_ms,
                                  picture, place_lat, place_lng, place_name)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            id.to_string(),
            s.name,
            s.about,
            rule_number(s.join),
            s.listed,
            s.members_start_topics,
            account,
            now_ms,
            s.picture,
            s.place.as_ref().map(|p| p.latitude),
            s.place.as_ref().map(|p| p.longitude),
            s.place.as_ref().map(|p| p.name.clone()),
        ],
    )?;
    tx.execute(
        "INSERT INTO community_members (community, account, role, joined_ms) VALUES (?1, ?2, 0, ?3)",
        params![id.to_string(), account, now_ms],
    )?;
    tx.commit()?;
    Ok(id)
}

/// Change a community's settings (a moderator).
pub fn edit(
    conn: &Connection,
    by: &str,
    id: Uuid,
    settings: &CommunitySettings,
) -> Result<(), Refusal> {
    must_moderate(conn, id, by)?;
    let s = clean_settings(settings)?;
    conn.execute(
        "UPDATE communities SET name = ?2, about = ?3, join_rule = ?4, listed = ?5,
         members_start_topics = ?6, picture = ?7, place_lat = ?8, place_lng = ?9, place_name = ?10
         WHERE id = ?1",
        params![
            id.to_string(),
            s.name,
            s.about,
            rule_number(s.join),
            s.listed,
            s.members_start_topics,
            s.picture,
            s.place.as_ref().map(|p| p.latitude),
            s.place.as_ref().map(|p| p.longitude),
            s.place.as_ref().map(|p| p.name.clone()),
        ],
    )?;
    Ok(())
}

/// Put `account` in a community and queue it for every topic (rule 1).
fn admit(conn: &Connection, id: Uuid, account: &str, now_ms: i64) -> Result<(), Refusal> {
    conn.execute(
        "INSERT OR IGNORE INTO community_members (community, account, role, joined_ms)
         VALUES (?1, ?2, 2, ?3)",
        params![id.to_string(), account, now_ms],
    )?;
    conn.execute(
        "DELETE FROM community_requests WHERE community = ?1 AND account = ?2",
        params![id.to_string(), account],
    )?;
    for group in topic_groups(conn, id, false)? {
        queue_join(conn, &group, account)?;
    }
    Ok(())
}

/// Queue `account` to be added to `group`, unless it is in it already or was
/// taken out of this one topic.
fn queue_join(conn: &Connection, group: &[u8], account: &str) -> Result<(), Refusal> {
    conn.execute(
        "INSERT OR IGNORE INTO topic_joins (group_id, account)
         SELECT ?1, ?2
         WHERE NOT EXISTS (SELECT 1 FROM e2ee_members WHERE group_id = ?1 AND account = ?2)
           AND NOT EXISTS (SELECT 1 FROM topic_exclusions WHERE group_id = ?1 AND account = ?2)",
        params![group, account],
    )?;
    conn.execute(
        "DELETE FROM topic_removals WHERE group_id = ?1 AND account = ?2",
        params![group, account],
    )?;
    Ok(())
}

/// Take `account` out of `group`: delivery stops now, and a device in the
/// topic is to commit its removal.
fn take_out(conn: &Connection, group: &[u8], account: &str) -> Result<(), Refusal> {
    let was_in = conn.execute(
        "DELETE FROM e2ee_members WHERE group_id = ?1 AND account = ?2",
        params![group, account],
    )? > 0;
    conn.execute(
        "DELETE FROM e2ee_group_devices WHERE group_id = ?1
           AND device IN (SELECT device FROM e2ee_devices WHERE account = ?2)",
        params![group, account],
    )?;
    conn.execute(
        "DELETE FROM topic_joins WHERE group_id = ?1 AND account = ?2",
        params![group, account],
    )?;
    if was_in {
        let who = username_of(conn, account)?;
        conn.execute(
            "INSERT OR REPLACE INTO topic_removals (group_id, account, username, asked_ms)
             VALUES (?1, ?2, ?3, 0)",
            params![group, account, who],
        )?;
    }
    Ok(())
}

/// What joining did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Joined {
    Member,
    Asked,
}

/// Join an open community, or ask to join one that asks first (rule 3).
pub fn join(
    conn: &Connection,
    account: &str,
    id: Uuid,
    message: &str,
    now_ms: i64,
) -> Result<Joined, Refusal> {
    let rule: Option<i64> = conn
        .query_row(
            "SELECT join_rule FROM communities WHERE id = ?1",
            params![id.to_string()],
            |r| r.get(0),
        )
        .optional()?;
    let Some(rule) = rule.map(rule_of) else {
        return Err(Refusal::NoSuchCommunity);
    };
    if role(conn, id, account)?.is_some() {
        return Err(Refusal::AlreadyIn);
    }
    let blocked: bool = conn
        .query_row(
            "SELECT 1 FROM community_blocks WHERE community = ?1 AND account = ?2
               AND (until_ms IS NULL OR until_ms > ?3)",
            params![id.to_string(), account, now_ms],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if blocked {
        return Err(Refusal::Refused);
    }
    match rule {
        JoinRule::Invite => Err(Refusal::InviteOnly),
        JoinRule::Open => {
            let tx = conn.unchecked_transaction()?;
            admit(&tx, id, account, now_ms)?;
            tx.commit()?;
            Ok(Joined::Member)
        }
        JoinRule::Ask => {
            let message: String = message.trim().chars().take(MAX_REQUEST_MESSAGE).collect();
            conn.execute(
                "INSERT OR REPLACE INTO community_requests (community, account, message, requested_ms)
                 VALUES (?1, ?2, ?3, ?4)",
                params![id.to_string(), account, message, now_ms],
            )?;
            Ok(Joined::Asked)
        }
    }
}

/// A moderator lets somebody in, or turns them down for a week.
pub fn decide(
    conn: &Connection,
    by: &str,
    id: Uuid,
    username: &str,
    accept: bool,
    now_ms: i64,
) -> Result<String, Refusal> {
    must_moderate(conn, id, by)?;
    let account = account_named(conn, username)?;
    let asked = conn.execute(
        "DELETE FROM community_requests WHERE community = ?1 AND account = ?2",
        params![id.to_string(), account],
    )? > 0;
    if !asked {
        return Err(Refusal::NoSuchPerson);
    }
    if accept {
        let tx = conn.unchecked_transaction()?;
        admit(&tx, id, &account, now_ms)?;
        tx.commit()?;
    } else {
        conn.execute(
            "INSERT OR REPLACE INTO community_blocks (community, account, until_ms) VALUES (?1, ?2, ?3)",
            params![id.to_string(), account, now_ms + REFUSAL_MS],
        )?;
    }
    Ok(account)
}

/// A moderator adds somebody, whatever the community's way in. A block made
/// by removing them is lifted: a moderator adding them is the decision.
pub fn add_member(
    conn: &Connection,
    by: &str,
    id: Uuid,
    username: &str,
    now_ms: i64,
) -> Result<String, Refusal> {
    must_moderate(conn, id, by)?;
    let account = account_named(conn, username)?;
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "DELETE FROM community_blocks WHERE community = ?1 AND account = ?2",
        params![id.to_string(), account],
    )?;
    admit(&tx, id, &account, now_ms)?;
    tx.commit()?;
    Ok(account)
}

/// The owner makes somebody a moderator or a member, or hands the community
/// over (rule 2).
pub fn set_role(
    conn: &Connection,
    by: &str,
    id: Uuid,
    username: &str,
    new_role: CommunityRole,
) -> Result<(), Refusal> {
    match role(conn, id, by)? {
        Some(CommunityRole::Owner) => {}
        Some(_) => return Err(Refusal::NotTheOwner),
        None => return Err(Refusal::NoSuchCommunity),
    }
    let account = account_named(conn, username)?;
    if account == by {
        return Err(Refusal::NotTheOwner);
    }
    if role(conn, id, &account)?.is_none() {
        return Err(Refusal::NotAMember);
    }
    let tx = conn.unchecked_transaction()?;
    if new_role == CommunityRole::Owner {
        tx.execute(
            "UPDATE community_members SET role = 1 WHERE community = ?1 AND account = ?2",
            params![id.to_string(), by],
        )?;
    }
    tx.execute(
        "UPDATE community_members SET role = ?3 WHERE community = ?1 AND account = ?2",
        params![id.to_string(), account, role_number(new_role)],
    )?;
    tx.commit()?;
    Ok(())
}

/// What taking somebody out of a community or a topic changed.
#[derive(Debug, Clone, PartialEq)]
pub struct Removal {
    pub account: String,
    /// The topics they were taken out of.
    pub groups: Vec<Vec<u8>>,
}

/// A moderator takes somebody out of one topic or the whole community, and
/// with `block` stops them asking again. The owner cannot be removed, and
/// only the owner removes a moderator.
#[allow(clippy::too_many_arguments)]
pub fn remove(
    conn: &Connection,
    by: &str,
    id: Uuid,
    username: &str,
    topic: Option<&[u8]>,
    block: bool,
) -> Result<Removal, Refusal> {
    let mine = must_moderate(conn, id, by)?;
    let account = account_named(conn, username)?;
    match role(conn, id, &account)? {
        None => return Err(Refusal::NotAMember),
        Some(CommunityRole::Owner) => return Err(Refusal::OwnerCannotBeRemoved),
        Some(CommunityRole::Moderator) if mine != CommunityRole::Owner => {
            return Err(Refusal::NotTheOwner)
        }
        Some(_) => {}
    }
    let tx = conn.unchecked_transaction()?;
    let groups = match topic {
        Some(g) => {
            if community_of_topic(&tx, g)? != Some(id) {
                return Err(Refusal::NoSuchTopic);
            }
            tx.execute(
                "INSERT OR IGNORE INTO topic_exclusions (group_id, account) VALUES (?1, ?2)",
                params![g, account],
            )?;
            vec![g.to_vec()]
        }
        None => {
            tx.execute(
                "DELETE FROM community_members WHERE community = ?1 AND account = ?2",
                params![id.to_string(), account],
            )?;
            if block {
                tx.execute(
                    "INSERT OR REPLACE INTO community_blocks (community, account, until_ms)
                     VALUES (?1, ?2, NULL)",
                    params![id.to_string(), account],
                )?;
            }
            let groups = topic_groups(&tx, id, true)?;
            for g in &groups {
                tx.execute(
                    "DELETE FROM topic_exclusions WHERE group_id = ?1 AND account = ?2",
                    params![g, account],
                )?;
            }
            groups
        }
    };
    for g in &groups {
        take_out(&tx, g, &account)?;
    }
    tx.commit()?;
    Ok(Removal { account, groups })
}

/// Leave a community, or withdraw a request to join one.
pub fn leave(conn: &Connection, account: &str, id: Uuid) -> Result<Removal, Refusal> {
    let tx = conn.unchecked_transaction()?;
    match role(&tx, id, account)? {
        Some(CommunityRole::Owner) => return Err(Refusal::OwnerStays),
        Some(_) => {}
        None => {
            let asked = tx.execute(
                "DELETE FROM community_requests WHERE community = ?1 AND account = ?2",
                params![id.to_string(), account],
            )? > 0;
            tx.commit()?;
            return if asked {
                Ok(Removal {
                    account: account.to_string(),
                    groups: Vec::new(),
                })
            } else {
                Err(Refusal::NotAMember)
            };
        }
    }
    tx.execute(
        "DELETE FROM community_members WHERE community = ?1 AND account = ?2",
        params![id.to_string(), account],
    )?;
    let groups = topic_groups(&tx, id, true)?;
    for g in &groups {
        tx.execute(
            "DELETE FROM topic_exclusions WHERE group_id = ?1 AND account = ?2",
            params![g, account],
        )?;
        take_out(&tx, g, account)?;
    }
    tx.commit()?;
    Ok(Removal {
        account: account.to_string(),
        groups,
    })
}

/// What deleting a community took down: whom to tell, and the topics.
#[derive(Debug, Clone, PartialEq)]
pub struct Deleted {
    pub members: Vec<String>,
    pub groups: Vec<Vec<u8>>,
}

/// The owner deletes a community, or a server operator (`may_triage`) does.
/// Every topic stops being delivered to anybody; the sealed logs go with it,
/// since nobody is left to read them.
pub fn delete(conn: &Connection, by: &str, id: Uuid, may_triage: bool) -> Result<Deleted, Refusal> {
    match role(conn, id, by)? {
        Some(CommunityRole::Owner) => {}
        _ if may_triage && exists(conn, &id.to_string())? => {}
        Some(_) => return Err(Refusal::NotTheOwner),
        None => return Err(Refusal::NoSuchCommunity),
    }
    let members = member_accounts(conn, id)?;
    let groups = topic_groups(conn, id, true)?;
    let tx = conn.unchecked_transaction()?;
    for g in &groups {
        for table in [
            "e2ee_members",
            "e2ee_log",
            "e2ee_invites",
            "e2ee_group_devices",
            "e2ee_device_asks",
            "topic_joins",
            "topic_removals",
            "topic_exclusions",
        ] {
            tx.execute(
                &format!("DELETE FROM {table} WHERE group_id = ?1"),
                params![g],
            )?;
        }
        tx.execute("DELETE FROM e2ee_groups WHERE id = ?1", params![g])?;
    }
    for table in [
        "topics",
        "community_members",
        "community_requests",
        "community_blocks",
        "community_reports",
    ] {
        tx.execute(
            &format!("DELETE FROM {table} WHERE community = ?1"),
            params![id.to_string()],
        )?;
    }
    tx.execute(
        "DELETE FROM communities WHERE id = ?1",
        params![id.to_string()],
    )?;
    tx.commit()?;
    Ok(Deleted { members, groups })
}

/// List a chat its creator has just registered as a topic, and queue every
/// other member for it.
pub fn create_topic(
    conn: &Connection,
    account: &str,
    id: Uuid,
    group: &[u8],
    name: &str,
    now_ms: i64,
) -> Result<(), Refusal> {
    let mine = role(conn, id, account)?.ok_or(Refusal::NoSuchCommunity)?;
    let open: bool = conn.query_row(
        "SELECT members_start_topics FROM communities WHERE id = ?1",
        params![id.to_string()],
        |r| r.get(0),
    )?;
    if !mine.moderates() && !open {
        return Err(Refusal::TopicsClosed);
    }
    let name = clean_topic(name)?;
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM topics WHERE community = ?1",
        params![id.to_string()],
        |r| r.get(0),
    )?;
    if count >= MAX_TOPICS {
        return Err(Refusal::TooManyTopics);
    }
    // A chat its creator registered and is in, not a direct chat, and not
    // already a topic somewhere.
    let mine: bool = conn
        .query_row(
            "SELECT 1 FROM e2ee_groups g JOIN e2ee_members m ON m.group_id = g.id
             WHERE g.id = ?1 AND g.created_by = ?2 AND m.account = ?2 AND g.direct_a IS NULL
               AND NOT EXISTS (SELECT 1 FROM topics t WHERE t.group_id = g.id)",
            params![group, account],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if !mine {
        return Err(Refusal::NotYourChat);
    }
    let tx = conn.unchecked_transaction()?;
    match tx.execute(
        "INSERT INTO topics (group_id, community, name, position, created_by, created_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![group, id.to_string(), name, count, account, now_ms],
    ) {
        Ok(_) => {}
        Err(rusqlite::Error::SqliteFailure(e, _))
            if e.code == rusqlite::ErrorCode::ConstraintViolation =>
        {
            return Err(Refusal::NameTaken)
        }
        Err(e) => return Err(e.into()),
    }
    for member in member_accounts(&tx, id)? {
        if member != account {
            queue_join(&tx, group, &member)?;
        }
    }
    tx.commit()?;
    Ok(())
}

/// A moderator renames, moves or archives a topic.
pub fn edit_topic(
    conn: &Connection,
    by: &str,
    id: Uuid,
    group: &[u8],
    name: &str,
    position: u32,
    archived: bool,
) -> Result<(), Refusal> {
    must_moderate(conn, id, by)?;
    if community_of_topic(conn, group)? != Some(id) {
        return Err(Refusal::NoSuchTopic);
    }
    let name = clean_topic(name)?;
    match conn.execute(
        "UPDATE topics SET name = ?2, position = ?3, archived = ?4 WHERE group_id = ?1",
        params![group, name, position, archived],
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

/// The community a chat is a topic of.
pub fn community_of_topic(conn: &Connection, group: &[u8]) -> Result<Option<Uuid>, Refusal> {
    let id: Option<String> = conn
        .query_row(
            "SELECT community FROM topics WHERE group_id = ?1",
            params![group],
            |r| r.get(0),
        )
        .optional()?;
    Ok(id.and_then(|i| Uuid::parse_str(&i).ok()))
}

/// Whether `group` is an archived topic, which takes no new messages.
pub fn is_archived(conn: &Connection, group: &[u8]) -> Result<bool, rusqlite::Error> {
    Ok(conn
        .query_row(
            "SELECT archived FROM topics WHERE group_id = ?1",
            params![group],
            |r| r.get::<_, bool>(0),
        )
        .optional()?
        .unwrap_or(false))
}

/// A moderator deletes entry `seq` of a topic. Commits cannot be deleted: a
/// device that misses one can read nothing in that chat again.
pub fn redact(
    conn: &Connection,
    by: &str,
    by_name: &str,
    group: &[u8],
    seq: u64,
) -> Result<Uuid, Refusal> {
    let id = community_of_topic(conn, group)?.ok_or(Refusal::NoSuchTopic)?;
    must_moderate(conn, id, by)?;
    let changed = conn.execute(
        "UPDATE e2ee_log SET redacted_by = ?3, message = x'00'
         WHERE group_id = ?1 AND seq = ?2 AND is_commit = 0",
        params![group, seq as i64, by_name],
    )?;
    if changed == 0 {
        return Err(Refusal::NoSuchMessage);
    }
    Ok(id)
}

/// A member reports something said in one of the community's topics.
#[allow(clippy::too_many_arguments)]
pub fn report(
    conn: &Connection,
    reporter: &str,
    id: Uuid,
    reported_username: &str,
    group: &[u8],
    seq: u64,
    reason: ReportReason,
    note: &str,
    line: &str,
    now_ms: i64,
) -> Result<(), Refusal> {
    if role(conn, id, reporter)?.is_none() {
        return Err(Refusal::NoSuchCommunity);
    }
    if community_of_topic(conn, group)? != Some(id) {
        return Err(Refusal::NoSuchTopic);
    }
    let reported = account_named(conn, reported_username)?;
    let note: String = note
        .chars()
        .take(cartopolis_core::MAX_REPORT_NOTE_LEN)
        .collect();
    let line: String = line
        .chars()
        .take(cartopolis_core::MAX_REPORT_MESSAGE_LEN)
        .collect();
    conn.execute(
        "INSERT INTO community_reports (community, reporter, reported, group_id, seq, reason, note, line, created_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            id.to_string(),
            reporter,
            reported,
            group,
            seq as i64,
            reason.as_str(),
            note,
            line,
            now_ms
        ],
    )?;
    Ok(())
}

/// A moderator closes a report.
pub fn resolve_report(conn: &Connection, by: &str, id: Uuid, report: i64) -> Result<(), Refusal> {
    must_moderate(conn, id, by)?;
    let gone = conn.execute(
        "DELETE FROM community_reports WHERE id = ?1 AND community = ?2",
        params![report, id.to_string()],
    )?;
    if gone == 0 {
        return Err(Refusal::NoSuchReport);
    }
    Ok(())
}

/// The accounts in a community.
pub fn member_accounts(conn: &Connection, id: Uuid) -> Result<Vec<String>, Refusal> {
    let mut stmt = conn.prepare("SELECT account FROM community_members WHERE community = ?1")?;
    let rows = stmt.query_map(params![id.to_string()], |r| r.get::<_, String>(0))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Whom a change to a community concerns: its members and whoever is asking
/// to join it.
pub fn concerned_accounts(conn: &Connection, id: Uuid) -> Result<Vec<String>, Refusal> {
    let mut out = member_accounts(conn, id)?;
    let mut stmt = conn.prepare("SELECT account FROM community_requests WHERE community = ?1")?;
    for a in stmt.query_map(params![id.to_string()], |r| r.get::<_, String>(0))? {
        out.push(a?);
    }
    Ok(out)
}

/// Every community `account` is in or waiting for, as it sees them. People
/// are full addresses on `server`.
pub fn list(conn: &Connection, account: &str, server: &str) -> Result<Vec<CommunityDto>, Refusal> {
    let mut stmt = conn.prepare(
        "SELECT c.id, c.name, c.about, c.join_rule, c.listed, c.members_start_topics, m.role,
                c.picture, c.place_lat, c.place_lng, c.place_name
         FROM communities c JOIN community_members m ON m.community = c.id AND m.account = ?1
         UNION ALL
         SELECT c.id, c.name, c.about, c.join_rule, c.listed, c.members_start_topics, NULL,
                c.picture, c.place_lat, c.place_lng, c.place_name
         FROM communities c JOIN community_requests r ON r.community = c.id AND r.account = ?1
         ORDER BY 2",
    )?;
    let rows = stmt
        .query_map(params![account], |r| {
            Ok((
                r.get::<_, String>(0)?,
                CommunitySettings {
                    name: r.get(1)?,
                    about: r.get(2)?,
                    join: rule_of(r.get(3)?),
                    listed: r.get(4)?,
                    members_start_topics: r.get(5)?,
                    picture: r.get(7)?,
                    place: place_of(r.get(8)?, r.get(9)?, r.get(10)?),
                },
                r.get::<_, Option<i64>>(6)?.map(role_of),
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut out = Vec::new();
    for (id, settings, my_role) in rows {
        let Ok(uuid) = Uuid::parse_str(&id) else {
            continue;
        };
        let mut dto = CommunityDto {
            id: uuid,
            home: server.to_string(),
            settings,
            role: my_role,
            members: Vec::new(),
            topics: Vec::new(),
            requests: Vec::new(),
            reports: Vec::new(),
        };
        if let Some(role) = my_role {
            dto.members = members(conn, &id, server)?;
            let mut t = conn.prepare(
                "SELECT t.group_id, t.name, t.position, t.archived,
                        EXISTS (SELECT 1 FROM e2ee_members m WHERE m.group_id = t.group_id AND m.account = ?2)
                 FROM topics t WHERE t.community = ?1 ORDER BY t.position, t.name",
            )?;
            dto.topics = t
                .query_map(params![id, account], |r| {
                    Ok(TopicDto {
                        group: r.get(0)?,
                        name: r.get(1)?,
                        position: r.get::<_, i64>(2)?.max(0) as u32,
                        archived: r.get(3)?,
                        joined: r.get(4)?,
                    })
                })?
                .collect::<Result<_, _>>()?;
            if role.moderates() {
                let mut q = conn.prepare(
                    "SELECT a.username, r.message, r.requested_ms FROM community_requests r
                     JOIN accounts a ON a.id = r.account WHERE r.community = ?1
                     ORDER BY r.requested_ms",
                )?;
                dto.requests = q
                    .query_map(params![id], |r| {
                        Ok(JoinRequestDto {
                            address: address::person(&r.get::<_, String>(0)?, server),
                            message: r.get(1)?,
                            requested_ms: r.get::<_, i64>(2)?.max(0) as u64,
                        })
                    })?
                    .collect::<Result<_, _>>()?;
                let mut q = conn.prepare(
                    "SELECT r.id, COALESCE(a.username, ''), COALESCE(b.username, ''), r.group_id,
                            r.seq, r.reason, r.note, r.line, r.created_ms
                     FROM community_reports r
                     LEFT JOIN accounts a ON a.id = r.reporter
                     LEFT JOIN accounts b ON b.id = r.reported
                     WHERE r.community = ?1 ORDER BY r.created_ms",
                )?;
                dto.reports = q
                    .query_map(params![id], |r| {
                        Ok(CommunityReportDto {
                            id: r.get(0)?,
                            reporter: address::person(&r.get::<_, String>(1)?, server),
                            reported: address::person(&r.get::<_, String>(2)?, server),
                            group: r.get(3)?,
                            seq: r.get::<_, i64>(4)?.max(0) as u64,
                            reason: ReportReason::from_wire(&r.get::<_, String>(5)?),
                            note: r.get(6)?,
                            line: r.get(7)?,
                            created_ms: r.get::<_, i64>(8)?.max(0) as u64,
                        })
                    })?
                    .collect::<Result<_, _>>()?;
            }
        }
        out.push(dto);
    }
    Ok(out)
}

fn members(conn: &Connection, id: &str, server: &str) -> Result<Vec<CommunityMemberDto>, Refusal> {
    let mut stmt = conn.prepare(
        "SELECT a.username, m.role FROM community_members m
         JOIN accounts a ON a.id = m.account
         WHERE m.community = ?1 ORDER BY m.role, a.username COLLATE NOCASE",
    )?;
    let rows = stmt.query_map(params![id], |r| {
        Ok(CommunityMemberDto {
            address: address::person(&r.get::<_, String>(0)?, server),
            role: role_of(r.get(1)?),
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

fn place_of(lat: Option<f64>, lng: Option<f64>, name: Option<String>) -> Option<CommunityPlace> {
    Some(CommunityPlace {
        latitude: lat?,
        longitude: lng?,
        name: name.unwrap_or_default(),
    })
}

/// Great-circle distance in metres.
fn distance_m(a: (f64, f64), b: (f64, f64)) -> f64 {
    let (la1, lo1) = (a.0.to_radians(), a.1.to_radians());
    let (la2, lo2) = (b.0.to_radians(), b.1.to_radians());
    let h = ((la2 - la1) / 2.0).sin().powi(2)
        + la1.cos() * la2.cos() * ((lo2 - lo1) / 2.0).sin().powi(2);
    2.0 * 6_371_000.0 * h.sqrt().asin()
}

/// Communities a directory search looks through before "Near you" sorts them:
/// far more than it shows, so the nearest are among them.
const NEAR_CANDIDATES: i64 = 1000;

/// Search the listed communities (an empty query lists them all, in `order`;
/// `near` is where the asker is, for [`DirectoryOrder::Near`]).
#[allow(clippy::too_many_arguments)]
pub fn directory(
    conn: &Connection,
    account: &str,
    server: &str,
    query: &str,
    order: DirectoryOrder,
    near: Option<(f64, f64)>,
    now_ms: i64,
) -> Result<Vec<CommunityListing>, Refusal> {
    let q = query.trim().to_lowercase();
    let (order_by, limit) = match order {
        DirectoryOrder::Popular => ("n DESC, c.created_ms DESC", DIRECTORY_LIMIT),
        DirectoryOrder::New => ("c.created_ms DESC", DIRECTORY_LIMIT),
        DirectoryOrder::Near => ("n DESC", NEAR_CANDIDATES),
    };
    let ids: Vec<String> = {
        let mut stmt = conn.prepare(&format!(
            "SELECT c.id, (SELECT COUNT(*) FROM community_members m WHERE m.community = c.id) AS n
             FROM communities c
             WHERE c.listed = 1
               AND (?1 = '' OR instr(lower(c.name), ?1) > 0 OR instr(lower(c.about), ?1) > 0)
             ORDER BY {order_by} LIMIT ?2"
        ))?;
        let rows = stmt.query_map(params![q, limit], |r| r.get::<_, String>(0))?;
        rows.collect::<Result<_, _>>()?
    };
    let mut out = Vec::new();
    for id in ids {
        if let Some(l) = listing(conn, account, server, &id, near, now_ms)? {
            out.push(l);
        }
    }
    if order == DirectoryOrder::Near {
        out.sort_by(|a, b| {
            a.distance_m
                .unwrap_or(f64::INFINITY)
                .total_cmp(&b.distance_m.unwrap_or(f64::INFINITY))
        });
        out.truncate(DIRECTORY_LIMIT as usize);
    }
    Ok(out)
}

/// One community as the directory shows it to `account`, listed or not.
fn listing(
    conn: &Connection,
    account: &str,
    server: &str,
    id: &str,
    near: Option<(f64, f64)>,
    now_ms: i64,
) -> Result<Option<CommunityListing>, Refusal> {
    let Ok(uuid) = Uuid::parse_str(id) else {
        return Ok(None);
    };
    let row = conn
        .query_row(
            "SELECT c.name, c.about, c.join_rule, c.created_ms, c.picture, c.place_lat, c.place_lng,
                    c.place_name, (SELECT COUNT(*) FROM community_members m WHERE m.community = c.id)
             FROM communities c WHERE c.id = ?1",
            params![id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    rule_of(r.get(2)?),
                    r.get::<_, i64>(3)?,
                    r.get::<_, Option<String>>(4)?,
                    place_of(r.get(5)?, r.get(6)?, r.get(7)?),
                    r.get::<_, i64>(8)?,
                ))
            },
        )
        .optional()?;
    let Some((name, about, join, created_ms, picture, place, n)) = row else {
        return Ok(None);
    };
    let mut t = conn.prepare(
        "SELECT name FROM topics WHERE community = ?1 AND archived = 0 ORDER BY position, name",
    )?;
    let topics = t
        .query_map(params![id], |r| r.get::<_, String>(0))?
        .collect::<Result<_, _>>()?;
    let moderators = members(conn, id, server)?
        .into_iter()
        .filter(|m| m.role.moderates())
        .collect();
    let mine = if role(conn, uuid, account)?.is_some() {
        ListingState::Member
    } else if conn
        .query_row(
            "SELECT 1 FROM community_requests WHERE community = ?1 AND account = ?2",
            params![id, account],
            |_| Ok(()),
        )
        .optional()?
        .is_some()
    {
        ListingState::Asked
    } else if conn
        .query_row(
            "SELECT 1 FROM community_blocks WHERE community = ?1 AND account = ?2
               AND (until_ms IS NULL OR until_ms > ?3)",
            params![id, account, now_ms],
            |_| Ok(()),
        )
        .optional()?
        .is_some()
    {
        ListingState::Refused
    } else {
        ListingState::Outside
    };
    let distance_m = near
        .zip(place.as_ref())
        .map(|(at, p)| distance_m(at, (p.latitude, p.longitude)));
    Ok(Some(CommunityListing {
        id: uuid,
        name,
        about,
        join,
        members: n.max(0) as u32,
        topics,
        moderators,
        mine,
        created_ms: created_ms.max(0) as u64,
        picture,
        place,
        distance_m,
    }))
}

/// A moderator makes an invite link to a community. Answers its code.
pub fn create_invite(
    conn: &Connection,
    by: &str,
    id: Uuid,
    now_ms: i64,
) -> Result<String, Refusal> {
    must_moderate(conn, id, by)?;
    let code: String = Uuid::new_v4()
        .simple()
        .to_string()
        .chars()
        .take(12)
        .collect();
    let days = cartopolis_core::COMMUNITY_INVITE_DAYS as i64;
    conn.execute(
        "INSERT INTO community_invites (code, community, created_by, created_ms, expires_ms)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            code,
            id.to_string(),
            by,
            now_ms,
            now_ms + days * 24 * 60 * 60 * 1000
        ],
    )?;
    Ok(code)
}

fn invite_community(conn: &Connection, code: &str, now_ms: i64) -> Result<Uuid, Refusal> {
    let id: Option<String> = conn
        .query_row(
            "SELECT community FROM community_invites WHERE code = ?1 AND expires_ms > ?2",
            params![code.trim(), now_ms],
            |r| r.get(0),
        )
        .optional()?;
    id.and_then(|i| Uuid::parse_str(&i).ok())
        .ok_or(Refusal::NoSuchInvite)
}

/// What an invite leads to, before joining with it.
pub fn lookup_invite(
    conn: &Connection,
    account: &str,
    server: &str,
    code: &str,
    now_ms: i64,
) -> Result<CommunityListing, Refusal> {
    let id = invite_community(conn, code, now_ms)?;
    listing(conn, account, server, &id.to_string(), None, now_ms)?.ok_or(Refusal::NoSuchInvite)
}

/// Join with an invite: whatever the community's way in, unless a moderator
/// removed this account with a block.
pub fn join_invite(
    conn: &Connection,
    account: &str,
    code: &str,
    now_ms: i64,
) -> Result<Uuid, Refusal> {
    let id = invite_community(conn, code, now_ms)?;
    if role(conn, id, account)?.is_some() {
        return Ok(id);
    }
    let blocked: bool = conn
        .query_row(
            "SELECT 1 FROM community_blocks WHERE community = ?1 AND account = ?2 AND until_ms IS NULL",
            params![id.to_string(), account],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if blocked {
        return Err(Refusal::Refused);
    }
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "DELETE FROM community_blocks WHERE community = ?1 AND account = ?2",
        params![id.to_string(), account],
    )?;
    admit(&tx, id, account, now_ms)?;
    tx.execute(
        "UPDATE community_invites SET uses = uses + 1 WHERE code = ?1",
        params![code.trim()],
    )?;
    tx.commit()?;
    Ok(id)
}

/// Settle the joins of `added` (account ids) to `group`: they are in. Called
/// by `e2ee::append` when a commit's invitation names them.
pub fn settle_joins(
    conn: &Connection,
    group: &[u8],
    added: &[String],
) -> Result<(), rusqlite::Error> {
    for account in added {
        conn.execute(
            "DELETE FROM topic_joins WHERE group_id = ?1 AND account = ?2",
            params![group, account],
        )?;
    }
    Ok(())
}

/// Settle the removals a device says it committed, by username.
pub fn settle_removals(
    conn: &Connection,
    group: &[u8],
    usernames: &[String],
) -> Result<(), rusqlite::Error> {
    for a in usernames {
        conn.execute(
            "DELETE FROM topic_removals WHERE group_id = ?1 AND lower(username) = lower(?2)",
            params![group, a],
        )?;
    }
    Ok(())
}

/// Work waiting in one topic for a device in it.
#[derive(Debug, Clone, PartialEq)]
pub struct TopicWork {
    pub group: Vec<u8>,
    /// Devices in the topic whose account still is, any of which can do it.
    pub holders: Vec<Uuid>,
    /// Accounts to add: (account id, username).
    pub joins: Vec<(String, String)>,
    /// Usernames to take out.
    pub removals: Vec<String>,
}

/// Topic work not handed to a device in the last [`WORK_AGAIN_MS`]: in the
/// topics `device` holds, in `group`, or for whatever `account` is waiting to
/// be added to.
pub fn pending_work(
    conn: &Connection,
    scope: WorkScope<'_>,
    now_ms: i64,
) -> Result<Vec<TopicWork>, Refusal> {
    let groups: Vec<Vec<u8>> = match scope {
        WorkScope::Group(g) => vec![g.to_vec()],
        WorkScope::HeldBy(device) => {
            let mut stmt = conn.prepare(
                "SELECT h.group_id FROM e2ee_group_devices h
                 WHERE h.device = ?1 AND EXISTS (SELECT 1 FROM topics t WHERE t.group_id = h.group_id)",
            )?;
            let rows = stmt.query_map(params![device.to_string()], |r| r.get(0))?;
            rows.collect::<Result<_, _>>()?
        }
        WorkScope::Joining(account) => {
            let mut stmt = conn.prepare("SELECT group_id FROM topic_joins WHERE account = ?1")?;
            let rows = stmt.query_map(params![account], |r| r.get(0))?;
            rows.collect::<Result<_, _>>()?
        }
    };
    let since = now_ms - WORK_AGAIN_MS;
    let mut out = Vec::new();
    for group in groups {
        let mut j = conn.prepare(
            "SELECT j.account, a.username FROM topic_joins j
             JOIN accounts a ON a.id = j.account AND a.disabled = 0
             WHERE j.group_id = ?1 AND (j.asked_ms = 0 OR j.asked_ms <= ?2)
               AND EXISTS (SELECT 1 FROM e2ee_devices d WHERE d.account = j.account)",
        )?;
        let joins: Vec<(String, String)> = j
            .query_map(params![group, since], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_, _>>()?;
        let mut r = conn.prepare(
            "SELECT username FROM topic_removals WHERE group_id = ?1 AND (asked_ms = 0 OR asked_ms <= ?2)",
        )?;
        let removals: Vec<String> = r
            .query_map(params![group, since], |row| row.get(0))?
            .collect::<Result<_, _>>()?;
        if joins.is_empty() && removals.is_empty() {
            continue;
        }
        let mut h = conn.prepare(
            "SELECT h.device FROM e2ee_group_devices h
             JOIN e2ee_devices d ON d.device = h.device
             JOIN e2ee_members m ON m.group_id = h.group_id AND m.account = d.account
             WHERE h.group_id = ?1 ORDER BY h.device",
        )?;
        let holders: Vec<Uuid> = h
            .query_map(params![group], |row| row.get::<_, String>(0))?
            .filter_map(Result::ok)
            .filter_map(|d| Uuid::parse_str(&d).ok())
            .collect();
        if holders.is_empty() {
            continue;
        }
        out.push(TopicWork {
            group,
            holders,
            joins,
            removals,
        });
    }
    Ok(out)
}

/// Where to look for topic work.
#[derive(Debug, Clone, Copy)]
pub enum WorkScope<'a> {
    Group(&'a [u8]),
    HeldBy(Uuid),
    Joining(&'a str),
}

/// Claim one key package per recently seen device of each account joining
/// `group`, and mark the work handed out. Answers (device, username, package).
pub fn claim_for_work(
    conn: &Connection,
    work: &TopicWork,
    now_ms: i64,
) -> Result<Vec<(Uuid, String, Vec<u8>)>, Refusal> {
    let tx = conn.unchecked_transaction()?;
    let mut out = Vec::new();
    for (account, username) in &work.joins {
        let mut d = tx.prepare(
            "SELECT device FROM e2ee_devices WHERE account = ?1
               AND last_seen >= datetime('now', ?2) ORDER BY device",
        )?;
        let stale = format!("-{} days", crate::e2ee::STALE_DEVICE_DAYS);
        let devices: Vec<String> = d
            .query_map(params![account, stale], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        drop(d);
        for dev in devices {
            let Ok(id) = Uuid::parse_str(&dev) else {
                continue;
            };
            let row: Option<(i64, Vec<u8>)> = tx
                .query_row(
                    "SELECT id, bytes FROM e2ee_key_packages WHERE device = ?1 ORDER BY id LIMIT 1",
                    params![dev],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            if let Some((row_id, bytes)) = row {
                tx.execute(
                    "DELETE FROM e2ee_key_packages WHERE id = ?1",
                    params![row_id],
                )?;
                out.push((id, username.clone(), bytes));
            }
        }
        tx.execute(
            "UPDATE topic_joins SET asked_ms = ?3 WHERE group_id = ?1 AND account = ?2",
            params![work.group, account, now_ms],
        )?;
    }
    tx.execute(
        "UPDATE topic_removals SET asked_ms = ?2 WHERE group_id = ?1",
        params![work.group, now_ms],
    )?;
    tx.commit()?;
    Ok(out)
}

/// An account is deleted: it leaves every community. A community it owned
/// passes to its longest-serving moderator, else its longest-serving member;
/// one nobody else is in is deleted.
pub fn erase_account(conn: &Connection, account: &str) -> Result<(), Refusal> {
    let mut stmt =
        conn.prepare("SELECT community, role FROM community_members WHERE account = ?1")?;
    let rows: Vec<(String, i64)> = stmt
        .query_map(params![account], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?;
    drop(stmt);
    for (id, r) in rows {
        let Ok(uuid) = Uuid::parse_str(&id) else {
            continue;
        };
        if role_of(r) == CommunityRole::Owner {
            let heir: Option<String> = conn
                .query_row(
                    "SELECT account FROM community_members WHERE community = ?1 AND account <> ?2
                     ORDER BY role, joined_ms LIMIT 1",
                    params![id, account],
                    |r| r.get(0),
                )
                .optional()?;
            match heir {
                Some(h) => {
                    conn.execute(
                        "UPDATE community_members SET role = 0 WHERE community = ?1 AND account = ?2",
                        params![id, h],
                    )?;
                    conn.execute(
                        "UPDATE community_members SET role = 2 WHERE community = ?1 AND account = ?2",
                        params![id, account],
                    )?;
                }
                None => {
                    delete(conn, account, uuid, false)?;
                    continue;
                }
            }
        }
        leave(conn, account, uuid)?;
    }
    for table in ["community_requests", "community_blocks"] {
        conn.execute(
            &format!("DELETE FROM {table} WHERE account = ?1"),
            params![account],
        )?;
    }
    for table in ["topic_joins", "topic_removals", "topic_exclusions"] {
        conn.execute(
            &format!("DELETE FROM {table} WHERE account = ?1"),
            params![account],
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn world() -> (tempfile::TempDir, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.db");
        let conn = crate::db::open_and_migrate(path.to_str().unwrap()).unwrap();
        for name in ["ann", "bob", "cas", "dee"] {
            crate::ensure_account(&conn, name, "pw").unwrap();
        }
        (dir, conn)
    }

    fn id(conn: &Connection, name: &str) -> String {
        crate::db::enabled_account_id(conn, name).unwrap().unwrap()
    }

    fn settings(join: JoinRule) -> CommunitySettings {
        CommunitySettings {
            name: "  Buurt   Oosterpoort ".into(),
            about: "lost cats".into(),
            join,
            listed: true,
            members_start_topics: false,
            picture: None,
            place: None,
        }
    }

    /// A chat registered by `account`, as `e2ee::create_group` would.
    fn chat(conn: &Connection, account: &str, n: u8) -> Vec<u8> {
        let group = cartopolis_core::address::group_id("localhost", [n; 16]);
        crate::e2ee::create_group(conn, account, &group, None).unwrap();
        group
    }

    fn joins(conn: &Connection, group: &[u8]) -> Vec<String> {
        let mut s = conn
            .prepare("SELECT account FROM topic_joins WHERE group_id = ?1 ORDER BY account")
            .unwrap();
        s.query_map(params![group], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    #[test]
    fn topic_names_read_as_they_are_written_after_a_hash() {
        assert_eq!(topic_name("#Lost and Found"), "lost-and-found");
        assert_eq!(topic_name("!!!"), "");
    }

    #[test]
    fn being_in_a_community_queues_you_for_every_topic() {
        let (_d, c) = world();
        let (ann, bob, cas) = (id(&c, "ann"), id(&c, "bob"), id(&c, "cas"));
        let cid = create(&c, &ann, &settings(JoinRule::Open), 0).unwrap();
        add_member(&c, &ann, cid, "bob", 1).unwrap();
        let g = chat(&c, &ann, 1);
        create_topic(&c, &ann, cid, &g, "#General", 2).unwrap();
        // Bob was in the community when the topic started.
        assert_eq!(joins(&c, &g), vec![bob.clone()]);
        // Cas joins the open community afterwards and is queued too.
        assert_eq!(join(&c, &cas, cid, "", 3).unwrap(), Joined::Member);
        let mut want = vec![bob.clone(), cas.clone()];
        want.sort();
        assert_eq!(joins(&c, &g), want);
        // A commit adding Bob settles his.
        settle_joins(&c, &g, &[bob.clone()]).unwrap();
        assert_eq!(joins(&c, &g), vec![cas]);
        let seen = list(&c, &bob, "localhost").unwrap();
        assert_eq!(seen[0].settings.name, "Buurt Oosterpoort");
        assert_eq!(seen[0].topics[0].name, "general");
    }

    #[test]
    fn asking_first_waits_for_a_moderator_and_a_refusal_stands_a_week() {
        let (_d, c) = world();
        let (ann, bob) = (id(&c, "ann"), id(&c, "bob"));
        let cid = create(&c, &ann, &settings(JoinRule::Ask), 0).unwrap();
        assert_eq!(
            join(&c, &bob, cid, "  I live here ", 1).unwrap(),
            Joined::Asked
        );
        let mine = list(&c, &bob, "localhost").unwrap();
        assert_eq!(mine[0].role, None, "waiting, not in");
        let theirs = list(&c, &ann, "localhost").unwrap();
        assert_eq!(theirs[0].requests[0].message, "I live here");
        assert_eq!(theirs[0].requests[0].address, "bob@localhost");
        // Only a moderator decides.
        assert_eq!(
            decide(&c, &bob, cid, "bob", true, 2),
            Err(Refusal::NotAModerator)
        );
        decide(&c, &ann, cid, "bob", false, 2).unwrap();
        assert_eq!(join(&c, &bob, cid, "", 3), Err(Refusal::Refused));
        assert_eq!(
            join(&c, &bob, cid, "", 3 + REFUSAL_MS).unwrap(),
            Joined::Asked
        );
        decide(&c, &ann, cid, "bob", true, 4 + REFUSAL_MS).unwrap();
        assert_eq!(role(&c, cid, &bob).unwrap(), Some(CommunityRole::Member));
    }

    #[test]
    fn invite_only_cannot_be_joined_and_unlisted_is_not_found() {
        let (_d, c) = world();
        let (ann, bob) = (id(&c, "ann"), id(&c, "bob"));
        let mut s = settings(JoinRule::Invite);
        s.listed = false;
        let cid = create(&c, &ann, &s, 0).unwrap();
        assert_eq!(join(&c, &bob, cid, "", 1), Err(Refusal::InviteOnly));
        assert!(
            directory(&c, &bob, "localhost", "", DirectoryOrder::Popular, None, 1)
                .unwrap()
                .is_empty()
        );
        let listed = create(&c, &ann, &settings(JoinRule::Open), 0).unwrap();
        let found = directory(&c, &bob, "localhost", "CATS", DirectoryOrder::New, None, 1).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, listed);
        assert_eq!(found[0].mine, ListingState::Outside);
        assert_eq!(found[0].moderators[0].role, CommunityRole::Owner);
    }

    #[test]
    fn only_the_owner_appoints_moderators_and_moderators_cannot_remove_each_other() {
        let (_d, c) = world();
        let ann = id(&c, "ann");
        let bob = id(&c, "bob");
        let cid = create(&c, &ann, &settings(JoinRule::Open), 0).unwrap();
        for n in ["bob", "cas", "dee"] {
            add_member(&c, &ann, cid, n, 1).unwrap();
        }
        set_role(&c, &ann, cid, "bob", CommunityRole::Moderator).unwrap();
        assert_eq!(
            set_role(&c, &bob, cid, "cas", CommunityRole::Moderator),
            Err(Refusal::NotTheOwner)
        );
        set_role(&c, &ann, cid, "cas", CommunityRole::Moderator).unwrap();
        assert_eq!(
            remove(&c, &bob, cid, "cas", None, false),
            Err(Refusal::NotTheOwner)
        );
        assert_eq!(
            remove(&c, &bob, cid, "ann", None, false),
            Err(Refusal::OwnerCannotBeRemoved)
        );
        // A moderator removes a member, with a block.
        remove(&c, &bob, cid, "dee", None, true).unwrap();
        let dee = id(&c, "dee");
        assert_eq!(join(&c, &dee, cid, "", 2), Err(Refusal::Refused));
        // Handing over: Bob owns it, Ann moderates.
        set_role(&c, &ann, cid, "bob", CommunityRole::Owner).unwrap();
        assert_eq!(role(&c, cid, &bob).unwrap(), Some(CommunityRole::Owner));
        assert_eq!(role(&c, cid, &ann).unwrap(), Some(CommunityRole::Moderator));
        assert_eq!(leave(&c, &bob, cid), Err(Refusal::OwnerStays));
    }

    #[test]
    fn removing_from_a_topic_stops_delivery_and_keeps_them_out_of_it() {
        let (_d, c) = world();
        let (ann, bob) = (id(&c, "ann"), id(&c, "bob"));
        let cid = create(&c, &ann, &settings(JoinRule::Open), 0).unwrap();
        let g = chat(&c, &ann, 1);
        create_topic(&c, &ann, cid, &g, "sale", 1).unwrap();
        add_member(&c, &ann, cid, "bob", 2).unwrap();
        // Bob is added (as a commit naming him would).
        c.execute(
            "INSERT INTO e2ee_members (group_id, account) VALUES (?1, ?2)",
            params![g, bob],
        )
        .unwrap();
        settle_joins(&c, &g, &[bob.clone()]).unwrap();
        let r = remove(&c, &ann, cid, "bob", Some(&g), false).unwrap();
        assert_eq!(r.groups, vec![g.clone()]);
        assert!(!crate::e2ee::members(&c, &g).unwrap().contains(&bob));
        // Still in the community, and not queued back into that topic.
        assert_eq!(role(&c, cid, &bob).unwrap(), Some(CommunityRole::Member));
        let g2 = chat(&c, &ann, 2);
        create_topic(&c, &ann, cid, &g2, "general", 3).unwrap();
        assert_eq!(joins(&c, &g2), vec![bob.clone()]);
        assert!(joins(&c, &g).is_empty());
        // A device in the topic is to commit the removal; its word settles it.
        let n: i64 = c
            .query_row("SELECT COUNT(*) FROM topic_removals", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
        settle_removals(&c, &g, &["Bob".into()]).unwrap();
        let n: i64 = c
            .query_row("SELECT COUNT(*) FROM topic_removals", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn a_moderator_deletes_a_message_but_never_a_commit() {
        let (_d, c) = world();
        let (ann, bob) = (id(&c, "ann"), id(&c, "bob"));
        let cid = create(&c, &ann, &settings(JoinRule::Open), 0).unwrap();
        let g = chat(&c, &ann, 1);
        create_topic(&c, &ann, cid, &g, "sale", 1).unwrap();
        let dev = Uuid::new_v4();
        crate::e2ee::register_device(&c, &ann, dev, &[]).unwrap();
        for (commit, n) in [(true, 0), (false, 1)] {
            crate::e2ee::append(&c, &ann, "ann", dev, &g, n, commit, b"x", None, 5).unwrap();
        }
        add_member(&c, &ann, cid, "bob", 2).unwrap();
        assert_eq!(redact(&c, &bob, "bob", &g, 2), Err(Refusal::NotAModerator));
        assert_eq!(
            redact(&c, &ann, "ann", &g, 1),
            Err(Refusal::NoSuchMessage),
            "a commit"
        );
        assert_eq!(redact(&c, &ann, "ann", &g, 2).unwrap(), cid);
    }

    #[test]
    fn deleting_an_owner_hands_the_community_on_or_deletes_it() {
        let (_d, c) = world();
        let ann = id(&c, "ann");
        let cid = create(&c, &ann, &settings(JoinRule::Open), 0).unwrap();
        add_member(&c, &ann, cid, "bob", 1).unwrap();
        let alone = create(&c, &ann, &settings(JoinRule::Open), 0).unwrap();
        erase_account(&c, &ann).unwrap();
        let bob = id(&c, "bob");
        assert_eq!(role(&c, cid, &bob).unwrap(), Some(CommunityRole::Owner));
        assert!(!exists(&c, &alone.to_string()).unwrap());
    }

    #[test]
    fn work_waits_for_a_device_in_the_topic_and_is_not_handed_twice_at_once() {
        let (_d, c) = world();
        let (ann, bob) = (id(&c, "ann"), id(&c, "bob"));
        let cid = create(&c, &ann, &settings(JoinRule::Open), 0).unwrap();
        let g = chat(&c, &ann, 1);
        let ann_dev = Uuid::new_v4();
        crate::e2ee::register_device(&c, &ann, ann_dev, &[]).unwrap();
        crate::e2ee::hold(&c, &g, ann_dev).unwrap();
        create_topic(&c, &ann, cid, &g, "general", 1).unwrap();
        add_member(&c, &ann, cid, "bob", 2).unwrap();
        // Bob has no device yet: nothing to do.
        assert!(pending_work(&c, WorkScope::Group(&g), 10)
            .unwrap()
            .is_empty());
        let bob_dev = Uuid::new_v4();
        crate::e2ee::register_device(&c, &bob, bob_dev, &[b"k".to_vec()]).unwrap();
        let work = pending_work(&c, WorkScope::Joining(&bob), 10).unwrap();
        assert_eq!(work.len(), 1);
        assert_eq!(work[0].holders, vec![ann_dev]);
        let claimed = claim_for_work(&c, &work[0], 10).unwrap();
        assert_eq!(claimed, vec![(bob_dev, "bob".to_string(), b"k".to_vec())]);
        assert!(pending_work(&c, WorkScope::HeldBy(ann_dev), 11)
            .unwrap()
            .is_empty());
        assert_eq!(
            pending_work(&c, WorkScope::HeldBy(ann_dev), 11 + WORK_AGAIN_MS)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn near_you_sorts_by_distance_and_a_community_with_no_place_sorts_last() {
        let (_d, c) = world();
        let (ann, bob) = (id(&c, "ann"), id(&c, "bob"));
        let place = |lat: f64, lng: f64| CommunitySettings {
            place: Some(CommunityPlace {
                latitude: lat,
                longitude: lng,
                name: "here".into(),
            }),
            ..settings(JoinRule::Open)
        };
        let far = create(&c, &ann, &place(52.37, 4.90), 0).unwrap(); // Amsterdam
        let none = create(&c, &ann, &settings(JoinRule::Open), 0).unwrap();
        let near = create(&c, &ann, &place(53.22, 6.57), 0).unwrap(); // Groningen
        let found = directory(
            &c,
            &bob,
            "localhost",
            "",
            DirectoryOrder::Near,
            Some((53.21, 6.56)),
            1,
        )
        .unwrap();
        let order: Vec<Uuid> = found.iter().map(|l| l.id).collect();
        assert_eq!(order, vec![near, far, none]);
        assert!(found[0].distance_m.unwrap() < 2_000.0);
        assert!(found[2].distance_m.is_none());
        let bad = CommunitySettings {
            picture: Some("../etc/passwd".into()),
            ..settings(JoinRule::Open)
        };
        assert_eq!(create(&c, &ann, &bad, 0), Err(Refusal::BadPicture));
    }

    #[test]
    fn an_invite_lets_anyone_in_until_it_expires_but_not_somebody_blocked() {
        let (_d, c) = world();
        let (ann, bob, cas) = (id(&c, "ann"), id(&c, "bob"), id(&c, "cas"));
        let mut s = settings(JoinRule::Invite);
        s.listed = false;
        let cid = create(&c, &ann, &s, 0).unwrap();
        assert_eq!(create_invite(&c, &bob, cid, 0), Err(Refusal::NotAModerator));
        let code = create_invite(&c, &ann, cid, 0).unwrap();
        let seen = lookup_invite(&c, &bob, "localhost", &code, 1).unwrap();
        assert_eq!(
            seen.id, cid,
            "an unlisted community shows through its invite"
        );
        assert_eq!(join_invite(&c, &bob, &code, 2).unwrap(), cid);
        assert_eq!(role(&c, cid, &bob).unwrap(), Some(CommunityRole::Member));
        // Cas is removed with a block; the invite does not undo that.
        join_invite(&c, &cas, &code, 3).unwrap();
        remove(&c, &ann, cid, "cas", None, true).unwrap();
        assert_eq!(join_invite(&c, &cas, &code, 4), Err(Refusal::Refused));
        let later = 5 + cartopolis_core::COMMUNITY_INVITE_DAYS as i64 * 24 * 60 * 60 * 1000;
        assert_eq!(
            lookup_invite(&c, &bob, "localhost", &code, later),
            Err(Refusal::NoSuchInvite)
        );
    }
}

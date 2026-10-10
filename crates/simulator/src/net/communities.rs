//! Communities and topics (protocol 39). `crate::communities` holds the rules;
//! these handlers translate, run them off the async runtime, tell the people a
//! change concerns, and hand the work only a device in a topic can do (adding
//! and removing people) to one that is.

use super::{now_millis, server_error, ConnCtx, FrameTransport};
use crate::communities::{Refusal, WorkScope};
use crate::db;
use bytes::Bytes;
use cartopolis_core::permissions::Permission;
use cartopolis_core::{
    address, encode_app_frame, CommunityRole, CommunitySettings, DirectoryOrder, E2eeKeyPackage,
    E2eeNewDevice, NetMessage, ReportReason,
};
use uuid::Uuid;

/// The answer a refusal is sent as.
fn refused(request_id: u32, r: &Refusal) -> NetMessage {
    let code = match r {
        Refusal::NoSuchCommunity
        | Refusal::NoSuchPerson
        | Refusal::NoSuchTopic
        | Refusal::NoSuchMessage
        | Refusal::NoSuchReport => 404,
        Refusal::NotAModerator | Refusal::NotTheOwner | Refusal::TopicsClosed => 403,
        Refusal::Storage(_) => 500,
        _ => 409,
    };
    server_error(request_id, code, r.to_string())
}

/// A person named on the wire, as the local username storage keys by.
fn local<T>(ctx: &ConnCtx<T>, who: &str) -> Result<String, String> {
    super::e2ee::local_name(ctx, who)
}

/// Every community this connection's account is in or waiting for, as an
/// answer.
async fn answer<T: FrameTransport>(ctx: &mut ConnCtx<T>, request_id: u32) -> anyhow::Result<()> {
    let account = ctx.principal_key.clone();
    let server = ctx.config.server_name();
    let list = db::call(&ctx.conn, move |c| {
        crate::communities::list(c, &account, &server)
    })
    .await
    .ok()
    .and_then(Result::ok)
    .unwrap_or_default();
    ctx.send(&NetMessage::Communities {
        request_id,
        communities: list,
    })
    .await
}

/// Tell every other connected account a community concerns (and `also`, who
/// were just taken out of it) that it changed, each with its own view
/// (`request_id` 0). The asker gets its answer directly.
///
/// Not an `async fn`: it takes what it needs out of the connection and returns
/// a future that holds no borrow of it, which keeps the connection future free
/// of a `Sync` bound a WebSocket transport cannot meet.
fn tell<T: FrameTransport>(
    ctx: &ConnCtx<T>,
    id: Uuid,
    also: Vec<String>,
) -> impl std::future::Future<Output = ()> + Send + 'static {
    let (conn, chat) = (ctx.conn.clone(), ctx.chat.clone());
    let (server, me) = (ctx.config.server_name(), ctx.principal_key.clone());
    async move {
        let lists = db::call(&conn, move |c| {
            let mut accounts = crate::communities::concerned_accounts(c, id).unwrap_or_default();
            accounts.extend(also);
            accounts.sort();
            accounts.dedup();
            accounts
                .into_iter()
                .filter(|a| *a != me)
                .filter_map(|a| {
                    crate::communities::list(c, &a, &server)
                        .ok()
                        .map(|l| (a, l))
                })
                .collect::<Vec<_>>()
        })
        .await
        .unwrap_or_default();
        for (account, communities) in lists {
            if !chat.account_online(&account) {
                continue;
            }
            if let Ok(frame) = encode_app_frame(&NetMessage::Communities {
                request_id: 0,
                communities,
            }) {
                chat.deliver_to_account(&account, &Bytes::from(frame));
            }
        }
    }
}

/// Where to look for topic work, owned so it can cross into the database.
pub(super) enum Scope {
    Group(Vec<u8>),
    HeldBy(Uuid),
    Joining(String),
}

/// Hand the topic work in `scope` — people to add, people to take out — to a
/// device in each topic: this one if it holds it, otherwise any connected one.
/// A topic with no device online waits until one syncs.
pub(super) async fn hand_work<T: FrameTransport>(
    ctx: &mut ConnCtx<T>,
    scope: Scope,
) -> anyhow::Result<()> {
    let me = ctx.e2ee_device;
    let now = now_millis() as i64;
    let works = match db::call(&ctx.conn, move |c| {
        let scope = match &scope {
            Scope::Group(g) => WorkScope::Group(g),
            Scope::HeldBy(d) => WorkScope::HeldBy(*d),
            Scope::Joining(a) => WorkScope::Joining(a),
        };
        crate::communities::pending_work(c, scope, now)
    })
    .await
    {
        Ok(Ok(w)) => w,
        _ => return Ok(()),
    };
    let server = ctx.config.server_name();
    for work in works {
        let adder = match me {
            Some(d) if work.holders.contains(&d) => Some(d),
            _ => ctx.chat.connected_device_among(&work.holders),
        };
        let Some(adder) = adder else {
            continue;
        };
        let w = work.clone();
        let claimed = match db::call(&ctx.conn, move |c| {
            crate::communities::claim_for_work(c, &w, now)
        })
        .await
        {
            Ok(Ok(claimed)) => claimed,
            _ => continue,
        };
        let mut msgs = Vec::new();
        if !claimed.is_empty() {
            msgs.push(NetMessage::TopicAddPeople {
                group: work.group.clone(),
                people: claimed
                    .into_iter()
                    .map(|(device, username, bytes)| E2eeNewDevice {
                        address: address::person(&username, &server),
                        package: E2eeKeyPackage { device, bytes },
                    })
                    .collect(),
            });
        }
        if !work.removals.is_empty() {
            msgs.push(NetMessage::TopicRemovePeople {
                group: work.group.clone(),
                addresses: work
                    .removals
                    .iter()
                    .map(|u| address::person(u, &server))
                    .collect(),
            });
        }
        for msg in msgs {
            if Some(adder) == me {
                ctx.send(&msg).await?;
            } else {
                ctx.chat
                    .deliver_to_device(adder, &Bytes::from(encode_app_frame(&msg)?));
            }
        }
    }
    Ok(())
}

/// Tell the devices of somebody taken out of topics that they are gone.
fn tell_left<T: FrameTransport>(ctx: &ConnCtx<T>, account: &str, id: Uuid, groups: &[Vec<u8>]) {
    let by = if account == ctx.principal_key {
        String::new()
    } else {
        address::person(&ctx.display_name, &ctx.config.server_name())
    };
    for g in groups {
        if let Ok(frame) = encode_app_frame(&NetMessage::TopicLeft {
            group: g.clone(),
            community: id,
            by: by.clone(),
        }) {
            ctx.chat.deliver_to_account(account, &Bytes::from(frame));
        }
    }
}

/// Run a rule off the async runtime: its value, or the refusal sent as the answer.
macro_rules! act {
    ($ctx:ident, $request_id:ident, $id:expr, $call:expr, |$out:ident| $then:expr) => {{
        match db::call(&$ctx.conn, $call).await {
            Ok(Ok($out)) => $then,
            Ok(Err(refusal)) => return $ctx.send(&refused($request_id, &refusal)).await,
            Err(_) => return Ok(()),
        }
    }};
}

pub(super) async fn on_list<T: FrameTransport>(
    ctx: &mut ConnCtx<T>,
    request_id: u32,
) -> anyhow::Result<()> {
    answer(ctx, request_id).await
}

pub(super) async fn on_create<T: FrameTransport>(
    ctx: &mut ConnCtx<T>,
    request_id: u32,
    settings: CommunitySettings,
) -> anyhow::Result<()> {
    let account = ctx.principal_key.clone();
    let now = now_millis() as i64;
    let id = act!(
        ctx,
        request_id,
        (),
        move |c| crate::communities::create(c, &account, &settings, now),
        |id| id
    );
    ctx.send(&NetMessage::CommunityCreated { request_id, id })
        .await?;
    answer(ctx, request_id).await
}

pub(super) async fn on_edit<T: FrameTransport>(
    ctx: &mut ConnCtx<T>,
    request_id: u32,
    id: Uuid,
    settings: CommunitySettings,
) -> anyhow::Result<()> {
    let by = ctx.principal_key.clone();
    act!(
        ctx,
        request_id,
        id,
        move |c| crate::communities::edit(c, &by, id, &settings),
        |_ok| ()
    );
    answer(ctx, request_id).await?;
    tell(ctx, id, Vec::new()).await;
    Ok(())
}

pub(super) async fn on_directory<T: FrameTransport>(
    ctx: &mut ConnCtx<T>,
    request_id: u32,
    query: String,
    order: DirectoryOrder,
) -> anyhow::Result<()> {
    let account = ctx.principal_key.clone();
    let server = ctx.config.server_name();
    let now = now_millis() as i64;
    let listings = act!(
        ctx,
        request_id,
        (),
        move |c| crate::communities::directory(c, &account, &server, &query, order, now),
        |l| l
    );
    ctx.send(&NetMessage::CommunityListings {
        request_id,
        listings,
    })
    .await
}

pub(super) async fn on_join<T: FrameTransport>(
    ctx: &mut ConnCtx<T>,
    request_id: u32,
    id: Uuid,
    message: String,
) -> anyhow::Result<()> {
    let account = ctx.principal_key.clone();
    let now = now_millis() as i64;
    let joined = act!(
        ctx,
        request_id,
        id,
        move |c| crate::communities::join(c, &account, id, &message, now),
        |j| j
    );
    answer(ctx, request_id).await?;
    tell(ctx, id, Vec::new()).await;
    if joined == crate::communities::Joined::Member {
        let me = ctx.principal_key.clone();
        hand_work(ctx, Scope::Joining(me)).await?;
    }
    Ok(())
}

pub(super) async fn on_leave<T: FrameTransport>(
    ctx: &mut ConnCtx<T>,
    request_id: u32,
    id: Uuid,
) -> anyhow::Result<()> {
    let account = ctx.principal_key.clone();
    let removal = act!(
        ctx,
        request_id,
        id,
        move |c| crate::communities::leave(c, &account, id),
        |r| r
    );
    answer(ctx, request_id).await?;
    tell(ctx, id, Vec::new()).await;
    let me = ctx.principal_key.clone();
    tell_left(ctx, &me, id, &removal.groups);
    for g in removal.groups {
        hand_work(ctx, Scope::Group(g)).await?;
    }
    Ok(())
}

pub(super) async fn on_decide<T: FrameTransport>(
    ctx: &mut ConnCtx<T>,
    request_id: u32,
    id: Uuid,
    who: String,
    accept: bool,
) -> anyhow::Result<()> {
    let name = match local(ctx, &who) {
        Ok(n) => n,
        Err(r) => return ctx.send(&server_error(request_id, 501, r)).await,
    };
    let by = ctx.principal_key.clone();
    let now = now_millis() as i64;
    let account = act!(
        ctx,
        request_id,
        id,
        move |c| crate::communities::decide(c, &by, id, &name, accept, now),
        |a| a
    );
    answer(ctx, request_id).await?;
    tell(ctx, id, vec![account.clone()]).await;
    if accept {
        hand_work(ctx, Scope::Joining(account)).await?;
    }
    Ok(())
}

pub(super) async fn on_add_member<T: FrameTransport>(
    ctx: &mut ConnCtx<T>,
    request_id: u32,
    id: Uuid,
    who: String,
) -> anyhow::Result<()> {
    let name = match local(ctx, &who) {
        Ok(n) => n,
        Err(r) => return ctx.send(&server_error(request_id, 501, r)).await,
    };
    let by = ctx.principal_key.clone();
    let now = now_millis() as i64;
    let account = act!(
        ctx,
        request_id,
        id,
        move |c| crate::communities::add_member(c, &by, id, &name, now),
        |a| a
    );
    answer(ctx, request_id).await?;
    tell(ctx, id, Vec::new()).await;
    hand_work(ctx, Scope::Joining(account)).await
}

pub(super) async fn on_set_role<T: FrameTransport>(
    ctx: &mut ConnCtx<T>,
    request_id: u32,
    id: Uuid,
    who: String,
    role: CommunityRole,
) -> anyhow::Result<()> {
    let name = match local(ctx, &who) {
        Ok(n) => n,
        Err(r) => return ctx.send(&server_error(request_id, 501, r)).await,
    };
    let by = ctx.principal_key.clone();
    act!(
        ctx,
        request_id,
        id,
        move |c| crate::communities::set_role(c, &by, id, &name, role),
        |_ok| ()
    );
    answer(ctx, request_id).await?;
    tell(ctx, id, Vec::new()).await;
    Ok(())
}

pub(super) async fn on_remove<T: FrameTransport>(
    ctx: &mut ConnCtx<T>,
    request_id: u32,
    id: Uuid,
    who: String,
    topic: Option<Vec<u8>>,
    block: bool,
) -> anyhow::Result<()> {
    let name = match local(ctx, &who) {
        Ok(n) => n,
        Err(r) => return ctx.send(&server_error(request_id, 501, r)).await,
    };
    let by = ctx.principal_key.clone();
    let removal = act!(
        ctx,
        request_id,
        id,
        move |c| crate::communities::remove(c, &by, id, &name, topic.as_deref(), block),
        |r| r
    );
    answer(ctx, request_id).await?;
    tell(ctx, id, vec![removal.account.clone()]).await;
    tell_left(ctx, &removal.account, id, &removal.groups);
    for g in removal.groups {
        hand_work(ctx, Scope::Group(g)).await?;
    }
    Ok(())
}

pub(super) async fn on_delete<T: FrameTransport>(
    ctx: &mut ConnCtx<T>,
    request_id: u32,
    id: Uuid,
) -> anyhow::Result<()> {
    let by = ctx.principal_key.clone();
    let may_triage = ctx.actor.perms.has(Permission::ReportsTriage);
    let deleted = act!(
        ctx,
        request_id,
        id,
        move |c| crate::communities::delete(c, &by, id, may_triage),
        |d| d
    );
    answer(ctx, request_id).await?;
    tell(ctx, id, deleted.members.clone()).await;
    for account in &deleted.members {
        tell_left(ctx, account, id, &deleted.groups);
    }
    Ok(())
}

pub(super) async fn on_topic_create<T: FrameTransport>(
    ctx: &mut ConnCtx<T>,
    request_id: u32,
    id: Uuid,
    group: Vec<u8>,
    name: String,
) -> anyhow::Result<()> {
    let account = ctx.principal_key.clone();
    let now = now_millis() as i64;
    let g = group.clone();
    act!(
        ctx,
        request_id,
        id,
        move |c| crate::communities::create_topic(c, &account, id, &g, &name, now),
        |_ok| ()
    );
    answer(ctx, request_id).await?;
    tell(ctx, id, Vec::new()).await;
    hand_work(ctx, Scope::Group(group)).await
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn on_topic_edit<T: FrameTransport>(
    ctx: &mut ConnCtx<T>,
    request_id: u32,
    id: Uuid,
    group: Vec<u8>,
    name: String,
    position: u32,
    archived: bool,
) -> anyhow::Result<()> {
    let by = ctx.principal_key.clone();
    act!(
        ctx,
        request_id,
        id,
        move |c| crate::communities::edit_topic(c, &by, id, &group, &name, position, archived),
        |_ok| ()
    );
    answer(ctx, request_id).await?;
    tell(ctx, id, Vec::new()).await;
    Ok(())
}

/// A device says its commit taking `addresses` out of `group` was accepted.
/// Only a device holding the topic is listened to.
pub(super) async fn on_topic_removed<T: FrameTransport>(
    ctx: &mut ConnCtx<T>,
    group: Vec<u8>,
    addresses: Vec<String>,
) -> anyhow::Result<()> {
    let Some(device) = ctx.e2ee_device else {
        return Ok(());
    };
    let names: Vec<String> = addresses
        .iter()
        .filter_map(|a| local(ctx, a).ok())
        .collect();
    let _ = db::call(&ctx.conn, move |c| {
        let holds: bool = c
            .query_row(
                "SELECT 1 FROM e2ee_group_devices WHERE group_id = ?1 AND device = ?2",
                rusqlite::params![group, device.to_string()],
                |_| Ok(()),
            )
            .is_ok();
        if holds {
            let _ = crate::communities::settle_removals(c, &group, &names);
        }
    })
    .await;
    Ok(())
}

pub(super) async fn on_moderate_delete<T: FrameTransport>(
    ctx: &mut ConnCtx<T>,
    request_id: u32,
    group: Vec<u8>,
    seq: u64,
) -> anyhow::Result<()> {
    let by = ctx.principal_key.clone();
    let by_name = ctx.display_name.clone();
    let g = group.clone();
    act!(
        ctx,
        request_id,
        (),
        move |c| crate::communities::redact(c, &by, &by_name, &g, seq),
        |_id| ()
    );
    let g = group.clone();
    let members = db::call(&ctx.conn, move |c| crate::e2ee::members(c, &g))
        .await
        .ok()
        .and_then(Result::ok)
        .unwrap_or_default();
    let frame = Bytes::from(encode_app_frame(&NetMessage::E2eeRedacted {
        group,
        seq,
        by: address::person(&ctx.display_name, &ctx.config.server_name()),
    })?);
    // As a commit: a block must not hide a deletion.
    ctx.chat
        .deliver_private(&members, &ctx.display_name, true, &frame);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn on_report<T: FrameTransport>(
    ctx: &mut ConnCtx<T>,
    request_id: u32,
    id: Uuid,
    who: String,
    group: Vec<u8>,
    seq: u64,
    reason: ReportReason,
    note: String,
    line: String,
) -> anyhow::Result<()> {
    let name = match local(ctx, &who) {
        Ok(n) => n,
        Err(r) => return ctx.send(&server_error(request_id, 501, r)).await,
    };
    let reporter = ctx.principal_key.clone();
    let now = now_millis() as i64;
    act!(
        ctx,
        request_id,
        id,
        move |c| crate::communities::report(
            c, &reporter, id, &name, &group, seq, reason, &note, &line, now
        ),
        |_ok| ()
    );
    ctx.send(&NetMessage::ReportAck {
        request_id,
        accepted: true,
    })
    .await?;
    tell(ctx, id, Vec::new()).await;
    Ok(())
}

pub(super) async fn on_report_resolve<T: FrameTransport>(
    ctx: &mut ConnCtx<T>,
    request_id: u32,
    id: Uuid,
    report: i64,
) -> anyhow::Result<()> {
    let by = ctx.principal_key.clone();
    act!(
        ctx,
        request_id,
        id,
        move |c| crate::communities::resolve_report(c, &by, id, report),
        |_ok| ()
    );
    answer(ctx, request_id).await?;
    tell(ctx, id, Vec::new()).await;
    Ok(())
}

/// After a commit lands in `group`: if it is a topic, who is in it changed for
/// everybody in its community, and there may be more work queued in it.
pub(super) async fn after_commit<T: FrameTransport>(
    ctx: &mut ConnCtx<T>,
    group: &[u8],
) -> anyhow::Result<()> {
    let g = group.to_vec();
    let id = db::call(&ctx.conn, move |c| {
        crate::communities::community_of_topic(c, &g)
    })
    .await
    .ok()
    .and_then(Result::ok)
    .flatten();
    if let Some(id) = id {
        // The committer's own view changed too (somebody's `joined`).
        answer(ctx, 0).await?;
        tell(ctx, id, Vec::new()).await;
        hand_work(ctx, Scope::Group(group.to_vec())).await?;
    }
    Ok(())
}

/// Route one community message (`ClientRequest::Community`) to its handler.
pub(super) async fn dispatch<T: FrameTransport>(
    ctx: &mut ConnCtx<T>,
    msg: NetMessage,
) -> anyhow::Result<()> {
    match msg {
        NetMessage::CommunityList { request_id } => on_list(ctx, request_id).await,
        NetMessage::CommunityCreate {
            request_id,
            settings,
        } => on_create(ctx, request_id, settings).await,
        NetMessage::CommunityEdit {
            request_id,
            id,
            settings,
        } => on_edit(ctx, request_id, id, settings).await,
        NetMessage::CommunityDirectory {
            request_id,
            query,
            order,
        } => on_directory(ctx, request_id, query, order).await,
        NetMessage::CommunityJoin {
            request_id,
            id,
            message,
        } => on_join(ctx, request_id, id, message).await,
        NetMessage::CommunityLeave { request_id, id } => on_leave(ctx, request_id, id).await,
        NetMessage::CommunityDecide {
            request_id,
            id,
            address,
            accept,
        } => on_decide(ctx, request_id, id, address, accept).await,
        NetMessage::CommunityAddMember {
            request_id,
            id,
            address,
        } => on_add_member(ctx, request_id, id, address).await,
        NetMessage::CommunitySetRole {
            request_id,
            id,
            address,
            role,
        } => on_set_role(ctx, request_id, id, address, role).await,
        NetMessage::CommunityRemove {
            request_id,
            id,
            address,
            topic,
            block,
        } => on_remove(ctx, request_id, id, address, topic, block).await,
        NetMessage::CommunityDelete { request_id, id } => on_delete(ctx, request_id, id).await,
        NetMessage::TopicCreate {
            request_id,
            community,
            group,
            name,
        } => on_topic_create(ctx, request_id, community, group, name).await,
        NetMessage::TopicEdit {
            request_id,
            community,
            group,
            name,
            position,
            archived,
        } => on_topic_edit(ctx, request_id, community, group, name, position, archived).await,
        NetMessage::TopicRemoved { group, addresses } => {
            on_topic_removed(ctx, group, addresses).await
        }
        NetMessage::ModerateDelete {
            request_id,
            group,
            seq,
        } => on_moderate_delete(ctx, request_id, group, seq).await,
        NetMessage::CommunityReport {
            request_id,
            id,
            address,
            group,
            seq,
            reason,
            note,
            line,
        } => on_report(ctx, request_id, id, address, group, seq, reason, note, line).await,
        NetMessage::CommunityReportResolve {
            request_id,
            id,
            report,
        } => on_report_resolve(ctx, request_id, id, report).await,
        // `ClientRequest::try_from` lets nothing else through.
        _ => Ok(()),
    }
}

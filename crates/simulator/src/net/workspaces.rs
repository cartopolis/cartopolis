//! Workspaces and channels (protocol 38). `crate::workspaces` holds the rules;
//! these handlers translate, run them off the async runtime, and tell the
//! people a change concerns.

use super::{now_millis, server_error, ConnCtx, FrameTransport};
use crate::db;
use bytes::Bytes;
use cartopolis_core::{address, encode_app_frame, NetMessage};
use uuid::Uuid;

/// Every workspace this connection's account belongs to, as an answer.
async fn answer<T: FrameTransport>(ctx: &mut ConnCtx<T>, request_id: u32) -> anyhow::Result<()> {
    let account = ctx.principal_key.clone();
    let server = ctx.config.server_name();
    let list = db::call(&ctx.conn, move |c| {
        crate::workspaces::list(c, &account, &server)
    })
    .await
    .ok()
    .and_then(Result::ok)
    .unwrap_or_default();
    ctx.send(&NetMessage::Workspaces {
        request_id,
        workspaces: list,
    })
    .await
}

/// Tell every other connected member of a workspace that it changed, each with
/// their own view of it (`request_id` 0). The asker gets its answer directly.
///
/// Not an `async fn`: it takes what it needs out of the connection and returns
/// a future that holds no borrow of it. An `async fn` taking `&ConnCtx` keeps
/// that reference in its state across the database hop, which makes the whole
/// connection future require `Sync` — and a WebSocket transport is not.
fn tell_members<T: FrameTransport>(
    ctx: &ConnCtx<T>,
    workspace: Uuid,
) -> impl std::future::Future<Output = ()> + Send + 'static {
    notify(
        ctx.conn.clone(),
        ctx.chat.clone(),
        ctx.config.server_name(),
        ctx.principal_key.clone(),
        workspace,
    )
}

async fn notify(
    conn: db::SharedConn,
    chat: std::sync::Arc<crate::chat::ChatHub>,
    server: String,
    me: String,
    workspace: Uuid,
) {
    let lists = db::call(&conn, move |c| {
        let accounts = crate::workspaces::member_accounts(c, workspace).unwrap_or_default();
        accounts
            .into_iter()
            .filter(|a| *a != me)
            .filter_map(|a| crate::workspaces::list(c, &a, &server).ok().map(|l| (a, l)))
            .collect::<Vec<_>>()
    })
    .await
    .unwrap_or_default();
    for (account, workspaces) in lists {
        if !chat.account_online(&account) {
            continue;
        }
        if let Ok(frame) = encode_app_frame(&NetMessage::Workspaces {
            request_id: 0,
            workspaces,
        }) {
            chat.deliver_to_account(&account, &Bytes::from(frame));
        }
    }
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
    name: String,
) -> anyhow::Result<()> {
    let account = ctx.principal_key.clone();
    let now = now_millis() as i64;
    match db::call(&ctx.conn, move |c| {
        crate::workspaces::create(c, &account, &name, now)
    })
    .await
    {
        Ok(Ok(_)) => answer(ctx, request_id).await,
        Ok(Err(refusal)) => {
            ctx.send(&server_error(request_id, 400, refusal.to_string()))
                .await
        }
        Err(_) => Ok(()),
    }
}

pub(super) async fn on_add_member<T: FrameTransport>(
    ctx: &mut ConnCtx<T>,
    request_id: u32,
    workspace: Uuid,
    who: String,
) -> anyhow::Result<()> {
    // A full address on this server, or a bare name meaning one; anybody else
    // is refused, as everywhere else chats would cross servers.
    let home = ctx.config.server_name();
    let name = match address::split(who.trim()) {
        Some((name, server)) if server.eq_ignore_ascii_case(&home) => name.to_string(),
        Some(_) => {
            return ctx
                .send(&server_error(
                    request_id,
                    501,
                    format!("{who} is on another server, and workspaces cannot reach it yet"),
                ))
                .await
        }
        None => who.trim().to_string(),
    };
    let by = ctx.principal_key.clone();
    let now = now_millis() as i64;
    match db::call(&ctx.conn, move |c| {
        crate::workspaces::add_member(c, &by, workspace, &name, now)
    })
    .await
    {
        Ok(Ok(_)) => {
            answer(ctx, request_id).await?;
            tell_members(ctx, workspace).await;
            Ok(())
        }
        Ok(Err(refusal)) => {
            ctx.send(&server_error(request_id, 403, refusal.to_string()))
                .await
        }
        Err(_) => Ok(()),
    }
}

pub(super) async fn on_channel_create<T: FrameTransport>(
    ctx: &mut ConnCtx<T>,
    request_id: u32,
    workspace: Uuid,
    group: Vec<u8>,
    name: String,
    topic: String,
    private: bool,
) -> anyhow::Result<()> {
    let account = ctx.principal_key.clone();
    let now = now_millis() as i64;
    match db::call(&ctx.conn, move |c| {
        crate::workspaces::create_channel(
            c, &account, workspace, &group, &name, &topic, private, now,
        )
    })
    .await
    {
        Ok(Ok(())) => {
            answer(ctx, request_id).await?;
            if !private {
                tell_members(ctx, workspace).await;
            }
            Ok(())
        }
        Ok(Err(refusal)) => {
            ctx.send(&server_error(request_id, 400, refusal.to_string()))
                .await
        }
        Err(_) => Ok(()),
    }
}

pub(super) async fn on_channel_join<T: FrameTransport>(
    ctx: &mut ConnCtx<T>,
    request_id: u32,
    workspace: Uuid,
    group: Vec<u8>,
) -> anyhow::Result<()> {
    let account = ctx.principal_key.clone();
    let id = group.clone();
    let now = now_millis() as i64;
    let members = match db::call(&ctx.conn, move |c| {
        crate::workspaces::request_join(c, &account, workspace, &id, now)
    })
    .await
    {
        Ok(Ok(members)) => members,
        Ok(Err(refusal)) => {
            return ctx
                .send(&server_error(request_id, 403, refusal.to_string()))
                .await
        }
        Err(_) => return Ok(()),
    };
    answer(ctx, request_id).await?;
    if members.is_empty() {
        return Ok(());
    }
    // One member device does the adding. If none is online the request waits
    // in `channel_joins` and the next member device to connect is handed it.
    let frame = Bytes::from(encode_app_frame(&NetMessage::ChannelJoinRequest {
        workspace,
        group,
        address: address::person(&ctx.display_name, &ctx.config.server_name()),
    })?);
    ctx.chat.deliver_to_one_device(&members, &frame);
    Ok(())
}

/// Hand a member device that has just registered every join request it could
/// settle (`workspaces::pending_for`).
pub(super) async fn hand_pending_joins<T: FrameTransport>(
    ctx: &mut ConnCtx<T>,
) -> anyhow::Result<()> {
    let account = ctx.principal_key.clone();
    let pending = db::call(&ctx.conn, move |c| {
        crate::workspaces::pending_for(c, &account)
    })
    .await
    .ok()
    .and_then(Result::ok)
    .unwrap_or_default();
    let server = ctx.config.server_name();
    for (workspace, group, username) in pending {
        ctx.send(&NetMessage::ChannelJoinRequest {
            workspace,
            group,
            address: address::person(&username, &server),
        })
        .await?;
    }
    Ok(())
}

/// After a commit lands in `group`: if it is a channel, its member counts and
/// "waiting" flags changed for everybody in the workspace.
pub(super) fn after_commit<T: FrameTransport>(
    ctx: &ConnCtx<T>,
    group: &[u8],
) -> impl std::future::Future<Output = ()> + Send + 'static {
    let id = group.to_vec();
    let (conn, chat) = (ctx.conn.clone(), ctx.chat.clone());
    let (server, me) = (ctx.config.server_name(), ctx.principal_key.clone());
    async move {
        let ws = db::call(&conn, move |c| crate::workspaces::workspace_of(c, &id))
            .await
            .ok()
            .and_then(Result::ok)
            .flatten();
        if let Some(ws) = ws {
            notify(conn, chat, server, me, ws).await;
        }
    }
}

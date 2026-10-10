//! Private chats (protocol 25). `crate::e2ee` holds the rules; these handlers
//! only translate, run them off the async runtime, and deliver.

use super::{
    blocked_lowercase, now_millis, server_error, take_token, ConnCtx, FrameTransport, CLAIM_BURST,
    MIN_CLAIM_INTERVAL,
};
use crate::db;
use bytes::Bytes;
use cartopolis_core::{
    address, encode_app_frame, E2eeCursor, E2eeKeyPackage, E2eeNewDevice, E2eeWelcome, NetMessage,
};
use uuid::Uuid;

/// The local account a full address on the wire names (protocol 37), or why it
/// cannot be served. Storage here is keyed by local account; addresses are the
/// wire's, and this is the one place they are translated on the way in.
///
/// Somebody on another server is refused in words: every identity is already
/// a full address so that chats can span servers later, but nothing forwards
/// to another server yet. A bare name is read as a local one.
pub(super) fn local_name<T>(ctx: &ConnCtx<T>, who: &str) -> Result<String, String> {
    match address::split(who.trim()) {
        Some((name, server)) if server.eq_ignore_ascii_case(&ctx.config.server_name()) => {
            Ok(name.to_string())
        }
        Some(_) => Err(format!(
            "{} is on another server, and chats between servers are not built yet",
            who.trim()
        )),
        None => Ok(who.trim().to_string()),
    }
}

/// A local account as the full address the wire carries.
fn full<T>(ctx: &ConnCtx<T>, name: &str) -> String {
    address::person(name, &ctx.config.server_name())
}

pub(super) async fn on_register_device<T: FrameTransport>(
    ctx: &mut ConnCtx<T>,
    request_id: u32,
    device: Uuid,
    key_packages: Vec<Vec<u8>>,
) -> anyhow::Result<()> {
    let account = ctx.principal_key.clone();
    let result = match db::call(&ctx.conn, move |c| {
        crate::e2ee::register_device(c, &account, device, &key_packages)
    })
    .await
    {
        Ok(result) => result,
        Err(_) => return Ok(()),
    };
    match result {
        Ok(left) => {
            ctx.e2ee_device = Some(device);
            ctx.chat.set_device(ctx.avatar_id, device);
            // A call already running in one of this account's chats rings here
            // now, not at the next join or departure: a phone opened from a
            // call's push connects after the call started.
            if let Some(calls) = &ctx.calls {
                let account = ctx.principal_key.clone();
                let blocked = db::call(&ctx.conn, {
                    let account = account.clone();
                    move |c| blocked_lowercase(c, &account)
                })
                .await
                .unwrap_or_default();
                calls.catch_up(account, device, blocked);
            }
            ctx.send(&NetMessage::E2eeDeviceStatus {
                request_id,
                key_packages: left,
            })
            .await
        }
        Err(refusal) => {
            ctx.send(&server_error(request_id, 403, refusal.to_string()))
                .await
        }
    }
}

pub(super) async fn on_claim_key_packages<T: FrameTransport>(
    ctx: &mut ConnCtx<T>,
    request_id: u32,
    subject: String,
) -> anyhow::Result<()> {
    if ctx.e2ee_device.is_none() {
        return Ok(());
    }
    if !take_token(&mut ctx.gates.claim, MIN_CLAIM_INTERVAL, CLAIM_BURST) {
        // Said, not dropped: an empty answer would read as "they have no app that
        // can chat privately".
        return ctx
            .send(&server_error(
                request_id,
                429,
                "too many new private chats at once — try again in a moment",
            ))
            .await;
    }
    let subject = match local_name(ctx, &subject) {
        Ok(name) => name,
        Err(refusal) => return ctx.send(&server_error(request_id, 501, refusal)).await,
    };
    // Your own name means "my other devices".
    let except = if subject.eq_ignore_ascii_case(&ctx.display_name) {
        ctx.e2ee_device
    } else {
        None
    };
    // A private chat with somebody who is not a contact starts as a request
    // (`contacts`, rule 4): without one between you, their devices cannot be
    // added.
    let key = ctx.principal_key.clone();
    let whom = subject.clone();
    let refused = match db::call(&ctx.conn, move |c| {
        match crate::contacts::account_by_name(c, &whom) {
            Ok(Some(target)) => match crate::contacts::may_invite(c, &key, &target.id) {
                Ok(true) => None,
                Ok(false) => {
                    Some(crate::contacts::Refusal::NotAContact(target.username).to_string())
                }
                Err(e) => Some(e.to_string()),
            },
            // Nobody by that name: the empty answer below already says so.
            _ => None,
        }
    })
    .await
    {
        Ok(refused) => refused,
        Err(_) => return Ok(()),
    };
    if let Some(message) = refused {
        return ctx.send(&server_error(request_id, 403, message)).await;
    }
    let whose = subject.clone();
    let packages = match db::call(&ctx.conn, move |c| {
        crate::e2ee::claim_key_packages(c, &whose, except).unwrap_or_default()
    })
    .await
    {
        Ok(packages) => packages,
        Err(_) => return Ok(()),
    };
    ctx.send(&NetMessage::E2eeKeyPackages {
        request_id,
        address: full(ctx, &subject),
        packages: packages
            .into_iter()
            .map(|(device, bytes)| cartopolis_core::E2eeKeyPackage { device, bytes })
            .collect(),
    })
    .await
}

pub(super) async fn on_create_group<T: FrameTransport>(
    ctx: &mut ConnCtx<T>,
    request_id: u32,
    group: Vec<u8>,
    direct_with: Option<String>,
) -> anyhow::Result<()> {
    if ctx.e2ee_device.is_none() {
        return Ok(());
    }
    // This server orders the log of every chat registered on it, so a chat's
    // id must say it lives here (`address::group_id`). A client that could
    // register any id could later make two servers each believe they own one.
    let home = ctx.config.server_name();
    if !address::group_home(&group).is_some_and(|h| h.eq_ignore_ascii_case(&home)) {
        return ctx
            .send(&server_error(
                request_id,
                400,
                format!("a chat started here must have an id that begins with {home}/"),
            ))
            .await;
    }
    let direct_with = match direct_with.map(|w| local_name(ctx, &w)).transpose() {
        Ok(w) => w,
        Err(refusal) => return ctx.send(&server_error(request_id, 501, refusal)).await,
    };
    let account = ctx.principal_key.clone();
    let id = group.clone();
    let device = ctx.e2ee_device;
    let result = match db::call(&ctx.conn, move |c| {
        let created = crate::e2ee::create_group(c, &account, &id, direct_with.as_deref());
        if let (Ok(None), Some(device)) = (&created, device) {
            crate::e2ee::hold(c, &id, device)?;
        }
        created
    })
    .await
    {
        Ok(result) => result,
        Err(_) => return Ok(()),
    };
    let reply = match result {
        Ok(existing) => NetMessage::E2eeGroupCreated {
            request_id,
            group,
            existing,
        },
        Err(refusal) => server_error(request_id, 409, refusal.to_string()),
    };
    ctx.send(&reply).await
}

/// An `E2eeSend`, as the server reads it.
pub(super) struct Outgoing {
    pub request_id: u32,
    pub group: Vec<u8>,
    pub epoch: u64,
    pub commit: bool,
    pub message: Vec<u8>,
    pub welcome: Option<E2eeWelcome>,
}

pub(super) async fn on_send<T: FrameTransport>(
    ctx: &mut ConnCtx<T>,
    send: Outgoing,
) -> anyhow::Result<()> {
    let Some(device) = ctx.e2ee_device else {
        return Ok(());
    };
    let Outgoing {
        request_id,
        group,
        epoch,
        commit,
        message,
        welcome,
    } = send;
    // An invitation's members arrive as full addresses and are stored as the
    // local accounts they name.
    let welcome = match welcome
        .map(|mut w| {
            w.members = w
                .members
                .iter()
                .map(|m| local_name(ctx, m))
                .collect::<Result<_, _>>()?;
            Ok::<_, String>(w)
        })
        .transpose()
    {
        Ok(w) => w,
        Err(refusal) => {
            return ctx
                .send(&NetMessage::E2eeSent {
                    request_id,
                    group,
                    seq: None,
                    refused: Some(refusal),
                })
                .await
        }
    };
    // One message into a chat that is still a request (`contacts`, rule 4).
    // Commits are never held: a device that misses one can read nothing again.
    if !commit {
        let key = ctx.principal_key.clone();
        let id = group.clone();
        let held = match db::call(&ctx.conn, move |c| crate::contacts::may_send(c, &key, &id)).await
        {
            Ok(held) => held,
            Err(_) => return Ok(()),
        };
        if let Err(refusal) = held {
            return ctx
                .send(&NetMessage::E2eeSent {
                    request_id,
                    group,
                    seq: None,
                    refused: Some(refusal.to_string()),
                })
                .await;
        }
    }
    let account = ctx.principal_key.clone();
    let sender_name = ctx.display_name.clone();
    let id = group.clone();
    let result = match db::call(&ctx.conn, move |c| {
        crate::e2ee::append(
            c,
            &account,
            &sender_name,
            device,
            &id,
            epoch,
            commit,
            &message,
            welcome.as_ref().map(|w| crate::e2ee::WelcomeFor {
                devices: &w.devices,
                members: &w.members,
                bytes: &w.bytes,
            }),
            now_millis(),
        )
    })
    .await
    {
        Ok(result) => result,
        Err(_) => return Ok(()),
    };
    match result {
        Ok(appended) => {
            ctx.send(&NetMessage::E2eeSent {
                request_id,
                group: group.clone(),
                seq: Some(appended.entry.seq),
                refused: None,
            })
            .await?;
            // A chat with somebody who declined this sender's request: stored and
            // acknowledged as usual, delivered to nobody but the sender
            // (`contacts::silenced_by`), and they are taken straight back out of
            // it if this entry added them.
            let key = ctx.principal_key.clone();
            let id = group.clone();
            let silenced = match db::call(&ctx.conn, move |c| {
                let other = crate::contacts::silenced_by(c, &key, &id).ok().flatten();
                if let Some(o) = &other {
                    let _ = crate::contacts::leave_direct_chat(c, o, &key);
                }
                other
            })
            .await
            {
                Ok(silenced) => silenced,
                Err(_) => return Ok(()),
            };
            let (members, invites) = match &silenced {
                Some(o) => (
                    appended
                        .members
                        .into_iter()
                        .filter(|m| m != o)
                        .collect::<Vec<_>>(),
                    Vec::new(),
                ),
                None => (appended.members, appended.invites),
            };
            let e = appended.entry;
            let frame = Bytes::from(encode_app_frame(&NetMessage::E2eeDeliver {
                group: e.group,
                seq: e.seq,
                sender: full(ctx, &e.sender_name),
                device: e.device,
                sent_at_ms: e.sent_at_ms,
                commit: e.is_commit,
                message: e.message,
            })?);
            ctx.chat
                .deliver_private(&members, &ctx.display_name, commit, &frame);
            for inv in invites {
                let frame = Bytes::from(encode_app_frame(&NetMessage::E2eeInvite {
                    group: inv.group,
                    from: full(ctx, &inv.from),
                    after: inv.after,
                    welcome: inv.welcome,
                })?);
                ctx.chat.deliver_to_device(inv.device, &frame);
            }
            if commit {
                // A channel's members changed: its workspace's lists did too.
                super::communities::after_commit(ctx, &group).await?;
                // Whoever this commit added may have more devices than it named,
                // and so may the writer.
                ask_for_missing_devices(ctx, Some(group.clone())).await?;
            }
            Ok(())
        }
        Err(refusal) => {
            ctx.send(&NetMessage::E2eeSent {
                request_id,
                group,
                seq: None,
                refused: Some(refusal.to_string()),
            })
            .await
        }
    }
}

pub(super) async fn on_sync<T: FrameTransport>(
    ctx: &mut ConnCtx<T>,
    request_id: u32,
    cursors: Vec<E2eeCursor>,
) -> anyhow::Result<()> {
    let Some(device) = ctx.e2ee_device else {
        return Ok(());
    };
    let cursors: Vec<(Vec<u8>, u64)> = cursors.into_iter().map(|c| (c.group, c.after)).collect();
    let account = ctx.principal_key.clone();
    let result = match db::call(&ctx.conn, move |c| {
        // Read fresh rather than from the list at connect: a block made since
        // must hold on the replay too.
        let blocked = blocked_lowercase(c, &account);
        let gone = crate::e2ee::topics_left(c, &account, &cursors).unwrap_or_default();
        crate::e2ee::sync(c, &account, device, &cursors, &blocked).map(|s| (s, gone))
    })
    .await
    {
        Ok(result) => result,
        Err(_) => return Ok(()),
    };
    let ((entries, invites), gone) = result.unwrap_or_default();
    for e in entries {
        if let Some(by) = &e.redacted_by {
            ctx.send(&NetMessage::E2eeRedacted {
                group: e.group,
                seq: e.seq,
                by: full(ctx, by),
            })
            .await?;
            continue;
        }
        ctx.send(&NetMessage::E2eeDeliver {
            group: e.group,
            seq: e.seq,
            sender: full(ctx, &e.sender_name),
            device: e.device,
            sent_at_ms: e.sent_at_ms,
            commit: e.is_commit,
            message: e.message,
        })
        .await?;
    }
    for inv in invites {
        ctx.send(&NetMessage::E2eeInvite {
            group: inv.group,
            from: full(ctx, &inv.from),
            after: inv.after,
            welcome: inv.welcome,
        })
        .await?;
    }
    // Topics this device still holds that its account was taken out of while
    // it was away: say so, so it forgets them rather than waiting for ever.
    for (group, community) in gone {
        ctx.send(&NetMessage::TopicLeft {
            group,
            community,
            by: String::new(),
        })
        .await?;
    }
    ctx.send(&NetMessage::E2eeSynced { request_id }).await?;
    // A device that has just caught up is the moment to settle who is missing
    // from its chats: itself, from chats started on another device, or
    // somebody else's device, from chats it can add them to. And the
    // community work only a device in a topic can do.
    ask_for_missing_devices(ctx, None).await?;
    super::communities::hand_work(ctx, super::communities::Scope::HeldBy(device)).await?;
    let me = ctx.principal_key.clone();
    super::communities::hand_work(ctx, super::communities::Scope::Joining(me)).await
}

/// Find member devices missing from this account's chats (or from `only`) and
/// hand each chat's to one device that holds it: this one when it does,
/// otherwise another that is connected. A chat with no holder online waits
/// until one syncs. `docs/notes/encrypted-chat.md`, "Every device of every
/// member".
async fn ask_for_missing_devices<T: FrameTransport>(
    ctx: &mut ConnCtx<T>,
    only: Option<Vec<u8>>,
) -> anyhow::Result<()> {
    let Some(me) = ctx.e2ee_device else {
        return Ok(());
    };
    let account = ctx.principal_key.clone();
    let now = now_millis();
    let plans = match db::call(&ctx.conn, move |c| {
        crate::e2ee::missing_devices(c, &account, only.as_deref(), now)
    })
    .await
    {
        Ok(Ok(plans)) => plans,
        _ => return Ok(()),
    };
    for plan in plans {
        let adder = if plan.holders.contains(&me) {
            Some(me)
        } else {
            ctx.chat.connected_device_among(&plan.holders)
        };
        let Some(adder) = adder else {
            continue;
        };
        let names: std::collections::HashMap<Uuid, String> = plan.missing.iter().cloned().collect();
        let devices: Vec<Uuid> = plan.missing.iter().map(|(d, _)| *d).collect();
        let group = plan.group.clone();
        let claimed = match db::call(&ctx.conn, move |c| {
            crate::e2ee::claim_for_add(c, &group, &devices, now)
        })
        .await
        {
            Ok(Ok(claimed)) if !claimed.is_empty() => claimed,
            _ => continue,
        };
        let msg = NetMessage::E2eeAddDevices {
            group: plan.group,
            devices: claimed
                .into_iter()
                .map(|(device, bytes)| E2eeNewDevice {
                    address: full(ctx, names.get(&device).map_or("", String::as_str)),
                    package: E2eeKeyPackage { device, bytes },
                })
                .collect(),
        };
        if adder == me {
            ctx.send(&msg).await?;
        } else {
            ctx.chat
                .deliver_to_device(adder, &Bytes::from(encode_app_frame(&msg)?));
        }
    }
    Ok(())
}

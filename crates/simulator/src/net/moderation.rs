//! The player's half of user safety (protocol 16): blocking, reporting, and
//! deleting your own account. Every refusal is decided here or in `ChatHub`'s
//! fan-out, never in the panel that draws it — `docs/notes/user-safety.md`.

use super::{server_error, ConnCtx, FrameTransport, MAX_USERNAME_LEN};
use crate::db;
use cartopolis_core::{NetMessage, ReportReason};

pub(super) async fn on_set_blocked<T: FrameTransport>(
    ctx: &mut ConnCtx<T>,
    subject: String,
    on: bool,
) -> anyhow::Result<()> {
    let subject = subject.trim().to_string();
    // Blocking yourself would take your own words out of every channel you speak
    // in, and reads as the chat being broken. Refused, silently: nobody means it.
    if subject.is_empty()
        || subject.eq_ignore_ascii_case(&ctx.display_name)
        || subject.chars().count() > MAX_USERNAME_LEN
    {
        return Ok(());
    }
    let principal_id = ctx.principal_key.clone();
    let blocked = subject.clone();
    let written = db::call(&ctx.conn, move |c| -> anyhow::Result<Vec<String>> {
        crate::db::set_block(c, &principal_id, &blocked, on)?;
        Ok(crate::db::blocked_usernames(c, &principal_id).unwrap_or_default())
    })
    .await;
    let list = match written {
        Ok(Ok(list)) => list,
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "block write failed");
            return Ok(());
        }
        Err(_) => return Ok(()),
    };
    // The live connection and the row move together; a block that only lands at
    // the next login is not a block anyone would call working.
    ctx.chat.set_blocked(ctx.avatar_id, &subject, on);
    tracing::info!(display_name = %ctx.display_name, %subject, blocked = on, "block updated");
    ctx.send(&NetMessage::BlockedList { usernames: list }).await
}

/// A `ReportUser`, as the server reads it.
pub(super) struct Report {
    pub request_id: u32,
    pub subject: String,
    pub reason: ReportReason,
    pub note: String,
    pub message: Option<String>,
    pub asset: Option<String>,
}

pub(super) async fn on_report<T: FrameTransport>(
    ctx: &mut ConnCtx<T>,
    report: Report,
) -> anyhow::Result<()> {
    let request_id = report.request_id;
    let subject = report.subject.trim().to_string();
    let accepted = !subject.is_empty()
        && !subject.eq_ignore_ascii_case(&ctx.display_name)
        && subject.chars().count() <= MAX_USERNAME_LEN;
    if accepted {
        let note: String = report
            .note
            .trim()
            .chars()
            .take(cartopolis_core::MAX_REPORT_NOTE_LEN)
            .collect();
        let message: Option<String> = report.message.map(|m| {
            m.chars()
                .take(cartopolis_core::MAX_REPORT_MESSAGE_LEN)
                .collect()
        });
        // A hash that is not one is dropped rather than stored: the report still
        // stands against the person, and a garbage value in this column would
        // send an operator looking for a model that never existed.
        let asset = report.asset.filter(|a| crate::assets::is_hash(a));
        let reason = report.reason;
        let principal_id = ctx.principal_key.clone();
        let reporter = ctx.display_name.clone();
        let reported = subject.clone();
        let stored = db::call(&ctx.conn, move |c| {
            crate::db::insert_report(
                c,
                &principal_id,
                &reporter,
                &reported,
                reason.as_str(),
                &note,
                message.as_deref(),
                asset.as_deref(),
            )
        })
        .await;
        match stored {
            // At `warn`, and with the subject in it, because on a small deployment
            // the log *is* the moderation queue until somebody opens the admin API.
            Ok(Ok(id)) => tracing::warn!(
                report_id = id,
                reporter = %ctx.display_name,
                %subject,
                reason = reason.as_str(),
                "user reported"
            ),
            Ok(Err(e)) => tracing::error!(error = %e, "report write failed"),
            Err(_) => {}
        }
    }
    ctx.send(&NetMessage::ReportAck {
        request_id,
        accepted,
    })
    .await
}

pub(super) async fn on_delete_account<T: FrameTransport>(
    ctx: &mut ConnCtx<T>,
    request_id: u32,
    password: String,
) -> anyhow::Result<()> {
    // The password is re-checked even though this connection is authenticated: a
    // session token can be a remembered one on a device somebody else is holding,
    // and this is the one request that cannot be undone. Hashing happens off the
    // SQLite mutex, for the reason `authenticate` documents.
    let name = ctx.display_name.clone();
    let phc = match db::call(&ctx.conn, move |c| crate::auth::lookup_account(c, &name)).await {
        Ok(found) => found.ok().flatten(),
        Err(_) => return Ok(()),
    };
    let ok = match phc {
        Some((_, phc)) => crate::auth::verify_password_bounded(password, phc).await,
        None => false,
    };
    if !ok {
        return ctx
            .send(&server_error(request_id, 401, "that password is not right"))
            .await;
    }
    let principal = ctx.principal;
    let name = ctx.display_name.clone();
    let outcome = match db::call(&ctx.conn, move |c| {
        crate::auth::delete_own_account(c, principal, &name)
    })
    .await
    {
        Ok(outcome) => outcome,
        Err(_) => return Ok(()),
    };
    match outcome {
        Ok(crate::auth::SelfDelete::Deleted) => {
            let revoked = ctx.sessions.revoke_principal(principal);
            tracing::warn!(display_name = %ctx.display_name, revoked, "account deleted by its owner");
            ctx.send(&NetMessage::AccountDeleted).await?;
            // The account is gone, so this connection is authenticated as nobody.
            // Ending it here is what makes that true of the socket too.
            Err(anyhow::anyhow!("account deleted"))
        }
        Ok(crate::auth::SelfDelete::LastAdmin) => {
            ctx.send(&server_error(
                request_id,
                409,
                "this is the last admin account; grant admin to someone else first",
            ))
            .await
        }
        Ok(crate::auth::SelfDelete::Unknown) | Err(_) => {
            ctx.send(&server_error(
                request_id,
                500,
                "could not delete the account",
            ))
            .await
        }
    }
}

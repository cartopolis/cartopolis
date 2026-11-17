//! The spots an account follows for new plans (`FollowPlans`, protocol 50):
//! kept for `crate::plans_watch`, which wakes the account's closed phones for
//! a notice published near one (`docs/notes/plans.md`).

use super::{ConnCtx, FrameTransport};

pub(super) async fn on_follow<T: FrameTransport>(
    ctx: &mut ConnCtx<T>,
    places: Vec<[f64; 2]>,
) -> anyhow::Result<()> {
    let account = ctx.principal_key.clone();
    let n = places.len();
    let saved = crate::db::call(&ctx.conn, move |c| {
        crate::db::set_plan_follows(c, &account, &places)
    })
    .await;
    match saved {
        Ok(Ok(())) => tracing::info!(places = n, "followed places for plans"),
        Ok(Err(e)) | Err(e) => tracing::warn!(error = %e, "followed places not kept"),
    }
    Ok(())
}

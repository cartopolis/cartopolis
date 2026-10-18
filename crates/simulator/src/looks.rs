//! Avatar looks kept with the account (protocol 42, migration V38).
//!
//! Stored as the postcard bytes of [`AvatarLook`], so a look written by an
//! older client still reads (every field added since has a serde default) and
//! the server never has to understand a body to keep one.

use cartopolis_core::AvatarLook;
use rusqlite::{Connection, OptionalExtension};

/// The account's saved look, if it ever saved one that still reads.
pub fn load(c: &Connection, principal: &str) -> anyhow::Result<Option<AvatarLook>> {
    let bytes: Option<Vec<u8>> = c
        .prepare_cached("SELECT look FROM accounts WHERE id = ?1")?
        .query_row([principal], |r| r.get(0))
        .optional()?
        .flatten();
    Ok(bytes.and_then(|b| postcard::from_bytes(&b).ok()))
}

pub fn save(c: &Connection, principal: &str, look: &AvatarLook) -> anyhow::Result<()> {
    let bytes = postcard::to_allocvec(look)?;
    c.execute(
        "UPDATE accounts SET look = ?1 WHERE id = ?2",
        rusqlite::params![bytes, principal],
    )?;
    Ok(())
}

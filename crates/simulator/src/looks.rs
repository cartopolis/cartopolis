//! Avatar looks kept with the account (protocol 42, migration V38).
//!
//! Stored as the postcard bytes of [`AvatarLook`], so the server never has to
//! understand a body to keep one. postcard gives a field added at the end no
//! default when older bytes stop before it, so a look saved under an older
//! protocol is read through [`AvatarLook::from_stored`], which knows each
//! older layout.

use cartopolis_core::AvatarLook;
use rusqlite::{Connection, OptionalExtension};

/// The account's saved look, if it ever saved one that still reads.
pub fn load(c: &Connection, principal: &str) -> anyhow::Result<Option<AvatarLook>> {
    let bytes: Option<Vec<u8>> = c
        .prepare_cached("SELECT look FROM accounts WHERE id = ?1")?
        .query_row([principal], |r| r.get(0))
        .optional()?
        .flatten();
    Ok(bytes.and_then(|b| AvatarLook::from_stored(&b)))
}

pub fn save(c: &Connection, principal: &str, look: &AvatarLook) -> anyhow::Result<()> {
    let bytes = postcard::to_allocvec(look)?;
    c.execute(
        "UPDATE accounts SET look = ?1 WHERE id = ?2",
        rusqlite::params![bytes, principal],
    )?;
    Ok(())
}

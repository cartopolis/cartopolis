use anyhow::Context;
use cartopolis_core::world::{lat_lng_to_tile, REGION_ZOOM_LEVEL};
use cartopolis_core::{GeoAnchor, PrimDto, PrimGeometry, PrimSurface, RegionDto};
use glam::Vec3;
use rusqlite::{Connection, OptionalExtension};
use std::collections::HashMap;

/// Column list (in order) for selecting a full [`PrimDto`]; consumed by [`row_to_prim`].
const PRIM_COLUMNS: &str = "id, region_id, name, shape, \
     position_x, position_y, position_z, \
     rotation_x, rotation_y, rotation_z, \
     scale_x, scale_y, scale_z, \
     color_r, color_g, color_b, texture_id, \
     path_cut_begin, path_cut_end, hollow, \
     twist_begin, twist_end, taper_x, taper_y, \
     top_shear_x, top_shear_y, slice_begin, slice_end, \
     alpha, glow, full_bright, repeat_u, repeat_v, flip_u, flip_v, \
     texture_rotation, offset_u, offset_v, \
     latitude, longitude";

/// Maps a row selected with [`PRIM_COLUMNS`] (in order) into a [`PrimDto`].
fn row_to_prim(row: &rusqlite::Row) -> rusqlite::Result<PrimDto> {
    Ok(PrimDto {
        id: row.get(0)?,
        region_id: row.get(1)?,
        name: row.get(2)?,
        shape: row.get(3)?,
        position: Vec3::new(row.get(4)?, row.get(5)?, row.get(6)?),
        rotation: Vec3::new(row.get(7)?, row.get(8)?, row.get(9)?),
        scale: Vec3::new(row.get(10)?, row.get(11)?, row.get(12)?),
        color: [row.get(13)?, row.get(14)?, row.get(15)?],
        texture_id: row.get(16)?,
        path_cut_begin: row.get(17)?,
        path_cut_end: row.get(18)?,
        hollow: row.get(19)?,
        twist_begin: row.get(20)?,
        twist_end: row.get(21)?,
        taper_x: row.get(22)?,
        taper_y: row.get(23)?,
        top_shear_x: row.get(24)?,
        top_shear_y: row.get(25)?,
        slice_begin: row.get(26)?,
        slice_end: row.get(27)?,
        surface: PrimSurface {
            alpha: row.get(28)?,
            glow: row.get(29)?,
            full_bright: row.get(30)?,
            repeat_u: row.get(31)?,
            repeat_v: row.get(32)?,
            flip_u: row.get(33)?,
            flip_v: row.get(34)?,
            rotation: row.get(35)?,
            offset_u: row.get(36)?,
            offset_v: row.get(37)?,
        },
        // ADR-029: every prim is geo-anchored.
        geo: GeoAnchor {
            latitude: row.get(38)?,
            longitude: row.get(39)?,
        },
    })
}

mod embedded {
    use refinery::embed_migrations;
    embed_migrations!("migrations");
}

pub fn open_and_migrate(path: &str) -> anyhow::Result<Connection> {
    if let Some(parent) = std::path::Path::new(path).parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create_dir_all {parent:?}"))?;
    }
    let mut conn = Connection::open(path).with_context(|| format!("open sqlite {path}"))?;
    embedded::migrations::runner()
        .run(&mut conn)
        .context("refinery migrate")?;
    seed_default_region(&conn)?;
    Ok(conn)
}

fn seed_default_region(conn: &Connection) -> anyhow::Result<()> {
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM regions", [], |row| row.get(0))?;
    if count > 0 {
        return Ok(());
    }
    let groningen_lat = 53.2194_f64;
    let groningen_lng = 6.5665_f64;
    let (tile_x, tile_y) = lat_lng_to_tile(groningen_lat, groningen_lng, REGION_ZOOM_LEVEL);
    conn.execute(
        "INSERT INTO regions (name, latitude, longitude, tile_x, tile_y, tile_z, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, datetime('now'), datetime('now'))",
        rusqlite::params![
            "Groningen",
            groningen_lat,
            groningen_lng,
            tile_x,
            tile_y,
            REGION_ZOOM_LEVEL as i64,
        ],
    )?;
    tracing::info!("seeded default region Groningen");
    Ok(())
}

pub fn load_world(conn: &Connection) -> anyhow::Result<(Vec<RegionDto>, Vec<PrimDto>)> {
    let mut stmt = conn.prepare(
        "SELECT id, name, latitude, longitude, tile_x, tile_y, tile_z FROM regions ORDER BY id",
    )?;
    let regions = stmt
        .query_map([], |row| {
            Ok(RegionDto {
                id: row.get(0)?,
                name: row.get(1)?,
                latitude: row.get(2)?,
                longitude: row.get(3)?,
                tile_x: row.get(4)?,
                tile_y: row.get(5)?,
                tile_z: row.get(6)?,
                sim_x: 0.0,
                sim_y: 0.0,
                sim_z: 0.0,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    let sql = format!("SELECT {PRIM_COLUMNS} FROM prims ORDER BY id");
    let mut stmt = conn.prepare(&sql)?;
    let prims = stmt
        .query_map([], row_to_prim)?
        .collect::<Result<Vec<_>, _>>()?;

    Ok((regions, prims))
}

/// Create a new prim in the database.
/// Returns the full PrimDto with server-assigned id.
/// (ADR-017 Phase 2)
pub fn insert_prim(
    conn: &Connection,
    region_id: i64,
    position: Vec3,
    shape: &str,
    geo: GeoAnchor,
) -> anyhow::Result<PrimDto> {
    // Validate that the region exists
    let region_exists: bool = conn.query_row(
        "SELECT COUNT(*) > 0 FROM regions WHERE id = ?1",
        rusqlite::params![region_id],
        |row| row.get(0),
    )?;

    if !region_exists {
        return Err(anyhow::anyhow!("region_id {} not found", region_id));
    }

    // Insert with sensible defaults
    conn.execute(
        "INSERT INTO prims (region_id, name, shape, position_x, position_y, position_z,
                            rotation_x, rotation_y, rotation_z, scale_x, scale_y, scale_z,
                            color_r, color_g, color_b, texture_id, latitude, longitude,
                            created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, datetime('now'), datetime('now'))",
        rusqlite::params![
            region_id,
            "Prim",                   // default name
            shape,
            position.x,
            position.y,
            position.z,
            0.0_f32,                  // rotation defaults
            0.0_f32,
            0.0_f32,
            1.0_f32,                  // scale defaults (1.0 = unit size)
            1.0_f32,
            1.0_f32,
            0.5_f32,                  // color defaults (neutral gray)
            0.5_f32,
            0.5_f32,
            None::<String>,           // no texture by default
            geo.latitude,             // ADR-029: geo-anchored placement
            geo.longitude,
        ],
    )?;

    // Retrieve the newly created prim
    let id = conn.last_insert_rowid();
    let prim = select_prim_by_id(conn, id)?
        .ok_or_else(|| anyhow::anyhow!("failed to retrieve newly inserted prim {}", id))?;

    tracing::debug!(prim_id = id, region_id, shape, "prim created");
    Ok(prim)
}

/// Update an existing prim in the database.
/// Returns the full updated PrimDto, or None if the prim does not exist.
/// (ADR-017 Phase 2)
pub fn update_prim(
    conn: &Connection,
    prim_id: i64,
    position: Vec3,
    rotation: Vec3,
    scale: Vec3,
    color: [f32; 3],
    texture_id: Option<String>,
    name: &str,
    surface: PrimSurface,
    geometry: PrimGeometry,
    geo: GeoAnchor,
) -> anyhow::Result<Option<PrimDto>> {
    // Check if prim exists first
    let exists: bool = conn.query_row(
        "SELECT COUNT(*) > 0 FROM prims WHERE id = ?1",
        rusqlite::params![prim_id],
        |row| row.get(0),
    )?;

    if !exists {
        return Ok(None);
    }

    // Update all fields
    conn.execute(
        "UPDATE prims SET
            position_x = ?2, position_y = ?3, position_z = ?4,
            rotation_x = ?5, rotation_y = ?6, rotation_z = ?7,
            scale_x = ?8, scale_y = ?9, scale_z = ?10,
            color_r = ?11, color_g = ?12, color_b = ?13,
            texture_id = ?14,
            name = ?15,
            alpha = ?16, glow = ?17, full_bright = ?18,
            repeat_u = ?19, repeat_v = ?20, flip_u = ?21, flip_v = ?22,
            texture_rotation = ?23, offset_u = ?24, offset_v = ?25,
            path_cut_begin = ?26, path_cut_end = ?27, hollow = ?28,
            twist_begin = ?29, twist_end = ?30, taper_x = ?31, taper_y = ?32,
            top_shear_x = ?33, top_shear_y = ?34, slice_begin = ?35, slice_end = ?36,
            latitude = ?37, longitude = ?38,
            updated_at = datetime('now')
         WHERE id = ?1",
        rusqlite::params![
            prim_id,
            position.x,
            position.y,
            position.z,
            rotation.x,
            rotation.y,
            rotation.z,
            scale.x,
            scale.y,
            scale.z,
            color[0],
            color[1],
            color[2],
            texture_id,
            name,
            surface.alpha,
            surface.glow,
            surface.full_bright,
            surface.repeat_u,
            surface.repeat_v,
            surface.flip_u,
            surface.flip_v,
            surface.rotation,
            surface.offset_u,
            surface.offset_v,
            geometry.path_cut_begin,
            geometry.path_cut_end,
            geometry.hollow,
            geometry.twist_begin,
            geometry.twist_end,
            geometry.taper_x,
            geometry.taper_y,
            geometry.top_shear_x,
            geometry.top_shear_y,
            geometry.slice_begin,
            geometry.slice_end,
            geo.latitude,
            geo.longitude,
        ],
    )?;

    // Retrieve the updated prim
    let prim = select_prim_by_id(conn, prim_id)?
        .ok_or_else(|| anyhow::anyhow!("failed to retrieve updated prim {}", prim_id))?;

    tracing::debug!(prim_id, "prim updated");
    Ok(Some(prim))
}

/// Delete a prim from the database.
/// Returns true if a prim was deleted, false if the prim did not exist.
/// (ADR-017 Phase 2)
pub fn delete_prim(conn: &Connection, prim_id: i64) -> anyhow::Result<bool> {
    // The script row goes first, and explicitly: `prim_scripts` declares
    // `ON DELETE CASCADE`, but SQLite only honours a foreign key when
    // `PRAGMA foreign_keys = ON`, which is off by default and not set on this
    // connection. Relying on the declaration would leave an orphaned script row
    // keeping a deleted prim's source alive — and, worse, counting against the
    // region's script budget forever.
    conn.execute(
        "DELETE FROM prim_scripts WHERE prim_id = ?1",
        rusqlite::params![prim_id],
    )?;
    let rows_affected = conn.execute(
        "DELETE FROM prims WHERE id = ?1",
        rusqlite::params![prim_id],
    )?;

    let deleted = rows_affected > 0;
    if deleted {
        tracing::debug!(prim_id, "prim deleted");
    }
    Ok(deleted)
}

/// Per-region resource budget (schema V7). Loaded once at startup with the world.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegionCaps {
    pub max_prims: u32,
    pub max_scripts: u32,
}

/// Every region's capacity budget, keyed by region id.
pub fn load_region_caps(conn: &Connection) -> anyhow::Result<HashMap<i64, RegionCaps>> {
    let mut stmt = conn.prepare("SELECT id, max_prims, max_scripts FROM regions")?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            RegionCaps {
                max_prims: row.get::<_, i64>(1)?.max(0) as u32,
                max_scripts: row.get::<_, i64>(2)?.max(0) as u32,
            },
        ))
    })?;
    Ok(rows.collect::<Result<HashMap<_, _>, _>>()?)
}

/// Every stored script, as `(prim_id, source)`. Read once at startup so the host
/// can compile the world's scripts before the first tick.
pub fn load_prim_scripts(conn: &Connection) -> anyhow::Result<Vec<(i64, String)>> {
    let mut stmt = conn.prepare("SELECT prim_id, source FROM prim_scripts ORDER BY prim_id")?;
    let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// A single prim's script source, or `None` if it has none.
pub fn get_prim_script(conn: &Connection, prim_id: i64) -> anyhow::Result<Option<String>> {
    let mut stmt = conn.prepare("SELECT source FROM prim_scripts WHERE prim_id = ?1")?;
    let mut rows = stmt.query(rusqlite::params![prim_id])?;
    match rows.next()? {
        Some(row) => Ok(Some(row.get(0)?)),
        None => Ok(None),
    }
}

/// Store a prim's script source, replacing any existing one.
pub fn set_prim_script(conn: &Connection, prim_id: i64, source: &str) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO prim_scripts (prim_id, source, updated_at) VALUES (?1, ?2, datetime('now'))
         ON CONFLICT(prim_id) DO UPDATE SET source = excluded.source, updated_at = excluded.updated_at",
        rusqlite::params![prim_id, source],
    )?;
    Ok(())
}

/// Remove a prim's script. Returns true if there was one.
pub fn delete_prim_script(conn: &Connection, prim_id: i64) -> anyhow::Result<bool> {
    let n = conn.execute(
        "DELETE FROM prim_scripts WHERE prim_id = ?1",
        rusqlite::params![prim_id],
    )?;
    Ok(n > 0)
}

/// One stored chat message (ADR-028 scrollback).
pub struct ChatRow {
    pub sender_id: i64,
    pub sender_name: String,
    pub sent_at_ms: i64,
    pub body: String,
}

/// Persist a chat message to a durable channel's scrollback (ADR-028). Local
/// (proximity) messages are ephemeral and never passed here.
pub fn insert_chat(
    conn: &Connection,
    channel: &str,
    sender_id: i64,
    sender_name: &str,
    sent_at_ms: i64,
    body: &str,
) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO chat_messages (channel, sender_id, sender_name, sent_at_ms, body) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![channel, sender_id, sender_name, sent_at_ms, body],
    )?;
    Ok(())
}

/// Most recent `limit` messages for a channel, returned oldest → newest for replay.
pub fn recent_chat(conn: &Connection, channel: &str, limit: i64) -> anyhow::Result<Vec<ChatRow>> {
    let mut stmt = conn.prepare(
        "SELECT sender_id, sender_name, sent_at_ms, body FROM chat_messages \
         WHERE channel = ?1 ORDER BY sent_at_ms DESC, id DESC LIMIT ?2",
    )?;
    let mut out: Vec<ChatRow> = stmt
        .query_map(rusqlite::params![channel, limit], |r| {
            Ok(ChatRow {
                sender_id: r.get(0)?,
                sender_name: r.get(1)?,
                sent_at_ms: r.get(2)?,
                body: r.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    out.reverse();
    Ok(out)
}

// ---------------------------------------------------------------------------
// Blocks and reports (V9). See that migration for why a block is keyed by
// username and a report keeps its own copy of the message.
// ---------------------------------------------------------------------------

/// Everyone `blocker` refuses to hear, lowercased.
pub fn blocked_usernames(conn: &Connection, blocker: &str) -> anyhow::Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT blocked_username FROM blocks WHERE blocker_id = ?1 ORDER BY blocked_username",
    )?;
    let out = stmt
        .query_map(rusqlite::params![blocker], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(out)
}

/// Add or remove one block. Idempotent in both directions — a client that asks
/// twice gets the same list back rather than an error it cannot act on.
pub fn set_block(
    conn: &Connection,
    blocker: &str,
    blocked_username: &str,
    blocked: bool,
) -> anyhow::Result<()> {
    if blocked {
        conn.execute(
            "INSERT OR IGNORE INTO blocks (blocker_id, blocked_username) VALUES (?1, ?2)",
            rusqlite::params![blocker, blocked_username],
        )?;
    } else {
        conn.execute(
            "DELETE FROM blocks WHERE blocker_id = ?1 AND blocked_username = ?2",
            rusqlite::params![blocker, blocked_username],
        )?;
    }
    Ok(())
}

/// One filed report, as the admin API serves it.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ReportRow {
    pub id: i64,
    pub reporter_username: Option<String>,
    pub subject_username: String,
    pub reason: String,
    pub note: String,
    pub message: Option<String>,
    pub created_at: String,
    pub resolved: bool,
    pub resolved_at: Option<String>,
    pub resolution: Option<String>,
}

/// File a report. Returns its id, which is what an operator refers to it by.
pub fn insert_report(
    conn: &Connection,
    reporter_id: &str,
    reporter_username: &str,
    subject_username: &str,
    reason: &str,
    note: &str,
    message: Option<&str>,
) -> anyhow::Result<i64> {
    conn.execute(
        "INSERT INTO reports (reporter_id, reporter_username, subject_username, reason, note, message) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![
            reporter_id,
            reporter_username,
            subject_username,
            reason,
            note,
            message
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Reports for the operator queue, newest first. `open_only` is the default view:
/// the queue is the work, and a resolved report is a record.
pub fn list_reports(
    conn: &Connection,
    open_only: bool,
    limit: i64,
) -> anyhow::Result<Vec<ReportRow>> {
    let sql = format!(
        "SELECT id, reporter_username, subject_username, reason, note, message, created_at, \
                resolved, resolved_at, resolution \
         FROM reports {} ORDER BY created_at DESC, id DESC LIMIT ?1",
        if open_only { "WHERE resolved = 0" } else { "" }
    );
    let mut stmt = conn.prepare(&sql)?;
    let out = stmt
        .query_map(rusqlite::params![limit], |r| {
            Ok(ReportRow {
                id: r.get(0)?,
                reporter_username: r.get(1)?,
                subject_username: r.get(2)?,
                reason: r.get(3)?,
                note: r.get(4)?,
                message: r.get(5)?,
                created_at: r.get(6)?,
                resolved: r.get::<_, i64>(7)? != 0,
                resolved_at: r.get(8)?,
                resolution: r.get(9)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(out)
}

/// How many reports are waiting. Cheap enough to put on the health endpoint,
/// which is where an operator will actually notice it.
pub fn open_report_count(conn: &Connection) -> anyhow::Result<i64> {
    Ok(
        conn.query_row("SELECT COUNT(*) FROM reports WHERE resolved = 0", [], |r| {
            r.get(0)
        })?,
    )
}

/// Close a report with a note on what was done. `false` = no such report.
pub fn resolve_report(conn: &Connection, id: i64, resolution: &str) -> anyhow::Result<bool> {
    let n = conn.execute(
        "UPDATE reports SET resolved = 1, resolved_at = datetime('now'), resolution = ?2 \
         WHERE id = ?1",
        rusqlite::params![id, resolution],
    )?;
    Ok(n > 0)
}

/// Erase the personal traces of a deleted account (guideline 5.1.1(v), and the
/// plain reading of "delete my account").
///
/// Three different answers, on purpose:
///
/// - **The blocks it made** go. They are that person's preferences and nobody
///   else's.
/// - **The blocks *against* it stay**, which is the reason this function exists
///   rather than a cascade. Deleting an account is exactly what somebody would do
///   to shed a block, and the username can be registered again.
/// - **Chat scrollback is anonymised, not deleted.** Removing one side of a
///   conversation leaves the other half unintelligible for everyone who took part
///   in it, and rooms are shared history; the name is the personal datum, so the
///   name is what goes.
///
/// Reports keep their body and lose their reporter, for the same reason: a safety
/// record about a third party is not the deleted account's data to take with it.
pub fn erase_account_traces(
    conn: &Connection,
    principal: &str,
    username: &str,
) -> anyhow::Result<()> {
    conn.execute(
        "DELETE FROM blocks WHERE blocker_id = ?1",
        rusqlite::params![principal],
    )?;
    conn.execute(
        "UPDATE chat_messages SET sender_name = ?2 WHERE sender_name = ?1",
        rusqlite::params![username, DELETED_ACCOUNT_NAME],
    )?;
    conn.execute(
        "UPDATE reports SET reporter_id = NULL, reporter_username = ?2 WHERE reporter_id = ?1",
        rusqlite::params![principal, DELETED_ACCOUNT_NAME],
    )?;
    Ok(())
}

/// What a deleted account's name is replaced with in shared history.
pub const DELETED_ACCOUNT_NAME: &str = "[deleted]";

/// Helper: retrieve a prim by id (internal use)  (ADR-017 Phase 2)
fn select_prim_by_id(conn: &Connection, prim_id: i64) -> anyhow::Result<Option<PrimDto>> {
    let sql = format!("SELECT {PRIM_COLUMNS} FROM prims WHERE id = ?1");
    let mut stmt = conn.prepare(&sql)?;
    let prim = stmt
        .query_row(rusqlite::params![prim_id], row_to_prim)
        .optional()?;
    Ok(prim)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_db() -> anyhow::Result<(Connection, tempfile::TempDir)> {
        let tempdir = tempfile::TempDir::new()?;
        let db_path = tempdir.path().join("test.db");
        let conn = open_and_migrate(db_path.to_str().unwrap())?;
        Ok((conn, tempdir))
    }

    #[test]
    fn test_insert_prim() -> anyhow::Result<()> {
        let (conn, _temp) = test_db()?;

        // Get the default region (Groningen from seed)
        let region_id: i64 =
            conn.query_row("SELECT id FROM regions LIMIT 1", [], |row| row.get(0))?;

        // Create a prim
        let prim = insert_prim(
            &conn,
            region_id,
            Vec3::new(10.0, 5.0, 20.0),
            "box",
            GeoAnchor {
                latitude: 0.0,
                longitude: 0.0,
            },
        )?;

        // Verify the prim was created
        assert!(prim.id > 0);
        assert_eq!(prim.region_id, region_id);
        assert_eq!(prim.name, "Prim");
        assert_eq!(prim.shape, "box");
        assert_eq!(prim.position, Vec3::new(10.0, 5.0, 20.0));
        assert_eq!(prim.rotation, Vec3::ZERO);
        assert_eq!(prim.scale, Vec3::ONE);
        assert_eq!(prim.color, [0.5, 0.5, 0.5]);
        assert_eq!(prim.texture_id, None);

        // Verify it's in the database
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM prims WHERE id = ?1",
            rusqlite::params![prim.id],
            |row| row.get(0),
        )?;
        assert_eq!(count, 1);

        Ok(())
    }

    #[test]
    fn test_insert_prim_invalid_region() -> anyhow::Result<()> {
        let (conn, _temp) = test_db()?;

        // Try to create a prim with non-existent region
        let result = insert_prim(
            &conn,
            9999,
            Vec3::ZERO,
            "box",
            GeoAnchor {
                latitude: 0.0,
                longitude: 0.0,
            },
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));

        Ok(())
    }

    #[test]
    fn test_update_prim() -> anyhow::Result<()> {
        let (conn, _temp) = test_db()?;

        let region_id: i64 =
            conn.query_row("SELECT id FROM regions LIMIT 1", [], |row| row.get(0))?;

        // Create a prim
        let prim = insert_prim(
            &conn,
            region_id,
            Vec3::ZERO,
            "box",
            GeoAnchor {
                latitude: 0.0,
                longitude: 0.0,
            },
        )?;

        // Update it
        let updated = update_prim(
            &conn,
            prim.id,
            Vec3::new(15.0, 10.0, 25.0),
            Vec3::new(45.0, 90.0, 0.0),
            Vec3::new(2.0, 2.0, 2.0),
            [1.0, 0.0, 0.0],
            Some("brick".to_string()),
            "Updated Prim",
            PrimSurface {
                alpha: 0.5,
                glow: 0.25,
                full_bright: true,
                repeat_u: 3.0,
                repeat_v: 4.0,
                flip_u: true,
                flip_v: false,
                rotation: 90.0,
                offset_u: 0.1,
                offset_v: 0.2,
            },
            PrimGeometry {
                path_cut_begin: 0.1,
                path_cut_end: 0.9,
                hollow: 0.5,
                twist_begin: 10.0,
                twist_end: -10.0,
                taper_x: 0.3,
                taper_y: -0.3,
                top_shear_x: 0.2,
                top_shear_y: -0.2,
                slice_begin: 0.05,
                slice_end: 0.95,
            },
            GeoAnchor {
                latitude: 0.0,
                longitude: 0.0,
            },
        )?
        .ok_or_else(|| anyhow::anyhow!("prim not found after update"))?;

        // Verify the update
        assert_eq!(updated.id, prim.id);
        assert_eq!(updated.position, Vec3::new(15.0, 10.0, 25.0));
        assert_eq!(updated.rotation, Vec3::new(45.0, 90.0, 0.0));
        assert_eq!(updated.scale, Vec3::new(2.0, 2.0, 2.0));
        assert_eq!(updated.color, [1.0, 0.0, 0.0]);
        assert_eq!(updated.texture_id, Some("brick".to_string()));
        assert_eq!(updated.name, "Updated Prim");

        // Surface + geometry round-trip through the DB and back into the returned DTO.
        assert_eq!(updated.surface.alpha, 0.5);
        assert_eq!(updated.surface.glow, 0.25);
        assert!(updated.surface.full_bright);
        assert_eq!(updated.surface.repeat_u, 3.0);
        assert_eq!(updated.surface.repeat_v, 4.0);
        assert!(updated.surface.flip_u);
        assert!(!updated.surface.flip_v);
        assert_eq!(updated.surface.rotation, 90.0);
        assert_eq!(updated.hollow, 0.5);
        assert_eq!(updated.path_cut_begin, 0.1);
        assert_eq!(updated.path_cut_end, 0.9);
        assert_eq!(updated.twist_begin, 10.0);
        assert_eq!(updated.taper_x, 0.3);
        assert_eq!(updated.slice_end, 0.95);

        // And persists across a fresh read.
        let reloaded = select_prim_by_id(&conn, prim.id)?
            .ok_or_else(|| anyhow::anyhow!("prim not found on reload"))?;
        assert_eq!(reloaded.surface, updated.surface);
        assert_eq!(reloaded.hollow, 0.5);
        assert_eq!(reloaded.twist_end, -10.0);

        Ok(())
    }

    #[test]
    fn test_update_prim_nonexistent() -> anyhow::Result<()> {
        let (conn, _temp) = test_db()?;

        // Try to update a non-existent prim
        let result = update_prim(
            &conn,
            9999,
            Vec3::ZERO,
            Vec3::ZERO,
            Vec3::ONE,
            [0.5, 0.5, 0.5],
            None,
            "test",
            PrimSurface::default(),
            PrimGeometry::default(),
            GeoAnchor {
                latitude: 0.0,
                longitude: 0.0,
            },
        )?;

        assert!(result.is_none());

        Ok(())
    }

    #[test]
    fn test_delete_prim() -> anyhow::Result<()> {
        let (conn, _temp) = test_db()?;

        let region_id: i64 =
            conn.query_row("SELECT id FROM regions LIMIT 1", [], |row| row.get(0))?;

        // Create a prim
        let prim = insert_prim(
            &conn,
            region_id,
            Vec3::ZERO,
            "box",
            GeoAnchor {
                latitude: 0.0,
                longitude: 0.0,
            },
        )?;

        // Delete it
        let deleted = delete_prim(&conn, prim.id)?;
        assert!(deleted);

        // Verify it's gone
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM prims WHERE id = ?1",
            rusqlite::params![prim.id],
            |row| row.get(0),
        )?;
        assert_eq!(count, 0);

        Ok(())
    }

    #[test]
    fn test_delete_prim_nonexistent() -> anyhow::Result<()> {
        let (conn, _temp) = test_db()?;

        // Try to delete a non-existent prim
        let deleted = delete_prim(&conn, 9999)?;
        assert!(!deleted);

        Ok(())
    }

    #[test]
    fn test_texture_roundtrip() -> anyhow::Result<()> {
        let (conn, _temp) = test_db()?;

        let region_id: i64 =
            conn.query_row("SELECT id FROM regions LIMIT 1", [], |row| row.get(0))?;

        // Create a prim with a texture
        let prim = insert_prim(
            &conn,
            region_id,
            Vec3::ZERO,
            "sphere",
            GeoAnchor {
                latitude: 0.0,
                longitude: 0.0,
            },
        )?;

        // Update it with a texture
        let updated = update_prim(
            &conn,
            prim.id,
            Vec3::ZERO,
            Vec3::ZERO,
            Vec3::ONE,
            [0.5, 0.5, 0.5],
            Some("grass".to_string()),
            "Prim",
            PrimSurface::default(),
            PrimGeometry::default(),
            GeoAnchor {
                latitude: 0.0,
                longitude: 0.0,
            },
        )?
        .ok_or_else(|| anyhow::anyhow!("prim not found"))?;

        // Verify texture persists
        assert_eq!(updated.texture_id, Some("grass".to_string()));

        // Reload from DB to verify persistence
        let reloaded =
            select_prim_by_id(&conn, prim.id)?.ok_or_else(|| anyhow::anyhow!("prim not found"))?;

        assert_eq!(reloaded.texture_id, Some("grass".to_string()));

        Ok(())
    }

    #[test]
    fn test_multiple_prims_isolation() -> anyhow::Result<()> {
        let (conn, _temp) = test_db()?;

        let region_id: i64 =
            conn.query_row("SELECT id FROM regions LIMIT 1", [], |row| row.get(0))?;

        // Create two prims
        let prim1 = insert_prim(
            &conn,
            region_id,
            Vec3::new(0.0, 0.0, 0.0),
            "box",
            GeoAnchor {
                latitude: 0.0,
                longitude: 0.0,
            },
        )?;
        let prim2 = insert_prim(
            &conn,
            region_id,
            Vec3::new(10.0, 0.0, 0.0),
            "sphere",
            GeoAnchor {
                latitude: 0.0,
                longitude: 0.0,
            },
        )?;

        // Update one
        update_prim(
            &conn,
            prim1.id,
            Vec3::new(5.0, 0.0, 0.0),
            Vec3::ZERO,
            Vec3::ONE,
            [1.0, 0.0, 0.0],
            None,
            "Red Box",
            PrimSurface::default(),
            PrimGeometry::default(),
            GeoAnchor {
                latitude: 0.0,
                longitude: 0.0,
            },
        )?;

        // Delete the other
        delete_prim(&conn, prim2.id)?;

        // Verify prim1 was updated and prim2 was deleted
        let updated1 = select_prim_by_id(&conn, prim1.id)?
            .ok_or_else(|| anyhow::anyhow!("prim1 not found"))?;
        assert_eq!(updated1.name, "Red Box");
        assert_eq!(updated1.color, [1.0, 0.0, 0.0]);

        let prim2_exists = select_prim_by_id(&conn, prim2.id)?;
        assert!(prim2_exists.is_none());

        Ok(())
    }
}

use anyhow::Context;
use cartopolis_core::{GeoAnchor, ParticleEffect, PrimDto, PrimGeometry, PrimSurface};
use glam::Vec3;
use rusqlite::{Connection, OptionalExtension};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// The one SQLite connection the server shares between every task.
pub type SharedConn = Arc<Mutex<Connection>>;

/// Run `f` against the database on a blocking thread, and wait for it.
///
/// SQLite is synchronous: every statement is a syscall and possibly an fsync. Run
/// on an async task, it parks the runtime worker it happens to be on — one of the
/// few that also carry the 20 Hz tick — for as long as the disk takes, and while
/// the tick loop took this same mutex *inside* the world write lock, a slow
/// commit stalled every connected client at once. So the lock is taken on
/// `spawn_blocking`'s pool, never on a runtime worker, and never while the world
/// lock is held (see [`crate::prims`]).
///
/// The closure is `'static` because the blocking thread outlives nothing it is
/// given: whatever it needs is moved in. A poisoned mutex is an error rather than
/// a panic — the guard's owner died mid-statement, and SQLite's own transaction
/// rollback is what keeps the file consistent, not the mutex.
pub async fn call<F, R>(conn: &SharedConn, f: F) -> anyhow::Result<R>
where
    F: FnOnce(&mut Connection) -> R + Send + 'static,
    R: Send + 'static,
{
    let conn = conn.clone();
    tokio::task::spawn_blocking(move || {
        let mut guard = conn
            .lock()
            .map_err(|_| anyhow::anyhow!("the database lock is poisoned"))?;
        Ok(f(&mut *guard))
    })
    .await
    .map_err(|e| anyhow::anyhow!("database task failed: {e}"))?
}

/// Run a short synchronous database closure from async code without parking a
/// runtime worker.
///
/// [`call`] is the right tool when the work can be handed over whole, and it is
/// what every write on the world's path uses. The HTTP handlers' reads are the
/// other shape: dozens of one-line closures that borrow the request they are
/// answering, which `spawn_blocking` cannot take (it needs `'static`). For those,
/// `block_in_place` tells a multi-threaded runtime that this worker is about to
/// block, and it moves the worker's other tasks elsewhere first — the same
/// property, without an owned copy of every argument.
///
/// It is not available on a current-thread runtime (it panics there), which is
/// what `#[tokio::test]` builds; there the closure simply runs inline, exactly as
/// it did before this existed.
pub fn blocking<R>(f: impl FnOnce() -> R) -> R {
    match tokio::runtime::Handle::try_current() {
        Ok(h)
            if matches!(
                h.runtime_flavor(),
                tokio::runtime::RuntimeFlavor::MultiThread
            ) =>
        {
            tokio::task::block_in_place(f)
        }
        _ => f(),
    }
}

/// Column list (in order) for selecting a full [`PrimDto`]; consumed by [`row_to_prim`].
const PRIM_COLUMNS: &str = "id, name, shape, \
     position_x, position_y, position_z, \
     rotation_x, rotation_y, rotation_z, \
     scale_x, scale_y, scale_z, \
     color_r, color_g, color_b, texture_id, \
     path_cut_begin, path_cut_end, hollow, \
     twist_begin, twist_end, taper_x, taper_y, \
     top_shear_x, top_shear_y, slice_begin, slice_end, \
     alpha, glow, full_bright, repeat_u, repeat_v, flip_u, flip_v, \
     texture_rotation, offset_u, offset_v, \
     latitude, longitude, particles, model, video, physical, \
     EXISTS (SELECT 1 FROM prim_scripts s WHERE s.prim_id = prims.id)";

/// How many columns [`PRIM_COLUMNS`] selects.
///
/// A named number because two queries append a column *after* it and read that
/// column by index. Adding `model` moved the script source from 42 to 43 and
/// broke every export with a `500` — the row index is a fact about this list, so
/// it is derived from the list rather than written out beside it. Counting
/// commas works because no column in `PRIM_COLUMNS` contains one; the test below
/// is what keeps that true.
const PRIM_COLUMN_COUNT: usize = {
    let bytes = PRIM_COLUMNS.as_bytes();
    let mut i = 0;
    let mut commas = 0;
    while i < bytes.len() {
        if bytes[i] == b',' {
            commas += 1;
        }
        i += 1;
    }
    commas + 1
};

/// Maps a row selected with [`PRIM_COLUMNS`] (in order) into a [`PrimDto`].
fn row_to_prim(row: &rusqlite::Row) -> rusqlite::Result<PrimDto> {
    Ok(PrimDto {
        id: row.get(0)?,
        name: row.get(1)?,
        shape: row.get(2)?,
        position: Vec3::new(row.get(3)?, row.get(4)?, row.get(5)?),
        rotation: Vec3::new(row.get(6)?, row.get(7)?, row.get(8)?),
        scale: Vec3::new(row.get(9)?, row.get(10)?, row.get(11)?),
        color: [row.get(12)?, row.get(13)?, row.get(14)?],
        texture_id: row.get(15)?,
        path_cut_begin: row.get(16)?,
        path_cut_end: row.get(17)?,
        hollow: row.get(18)?,
        twist_begin: row.get(19)?,
        twist_end: row.get(20)?,
        taper_x: row.get(21)?,
        taper_y: row.get(22)?,
        top_shear_x: row.get(23)?,
        top_shear_y: row.get(24)?,
        slice_begin: row.get(25)?,
        slice_end: row.get(26)?,
        surface: PrimSurface {
            alpha: row.get(27)?,
            glow: row.get(28)?,
            full_bright: row.get(29)?,
            repeat_u: row.get(30)?,
            repeat_v: row.get(31)?,
            flip_u: row.get(32)?,
            flip_v: row.get(33)?,
            rotation: row.get(34)?,
            offset_u: row.get(35)?,
            offset_v: row.get(36)?,
        },
        // ADR-029: every prim is geo-anchored.
        geo: GeoAnchor {
            latitude: row.get(37)?,
            longitude: row.get(38)?,
        },
        // Protocol v20. Sanitised on the way *out* of the database as well as on
        // the way in: a row can predate a tightened limit, or have been edited
        // with sqlite3, and the client hands these numbers straight to a shader.
        particles: parse_particles(row.get::<_, Option<String>>(39)?.as_deref()),
        // Protocol v22. Not validated on the way out: `net.rs` refuses a hash the
        // store does not hold at the point it is *set*, and a row whose file was
        // later removed draws the placeholder — which is the honest picture, and
        // cheaper than a store lookup per prim per snapshot.
        model: row.get(40)?,
        // Protocol v21, and read from the database rather than tracked: a
        // subquery on the row is always right, where a flag maintained beside
        // `set_prim_script` is one more thing to forget. `SimWorld::set_scripted`
        // still updates its in-memory copy so the next snapshot is current
        // without a re-read — the database is what it falls back to being right
        // from.
        scripted: row.get(43)?,
        // Protocol v28. A column that will not parse is no video, and a
        // warning — the `particles` rule, for the same reason.
        video: row.get::<_, Option<String>>(41)?.and_then(|json| {
            match serde_json::from_str(&json) {
                Ok(v) => Some(v),
                Err(e) => {
                    tracing::warn!("prim video column is not a valid video ({e}); ignoring it");
                    None
                }
            }
        }),
        // Protocol 32. A loop lives in memory only (`SimWorld::replace_prim`
        // carries it across an edit); a restart runs `on_start` again.
        sound_loop: None,
        // Protocol 34 (`V26__prim_physical.sql`).
        physical: row.get(42)?,
        // Protocol 36. Memory only, like `sound_loop`: the script sets them
        // again in `on_start`.
        hand_range: 0.0,
        tone: None,
    })
}

/// Decode the `particles` column (see `V13__prim_particles.sql`).
///
/// A column that will not parse becomes `None` and a warning, never an error:
/// the alternative is that one hand-edited row stops the whole world loading,
/// and "that prim's fire is out" is a failure an operator can see and fix while
/// "the simulator will not start" is not.
fn parse_particles(json: Option<&str>) -> Option<ParticleEffect> {
    let raw = json?;
    match serde_json::from_str::<ParticleEffect>(raw) {
        Ok(e) => Some(e.sanitized()),
        Err(e) => {
            tracing::warn!("prim particles column is not a valid effect ({e}); ignoring it");
            None
        }
    }
}

/// Encode an emitter for the `particles` column. Sanitised first, so nothing out
/// of range is ever written down — a stored value is read by clients that were
/// not running when it was set.
fn particles_json(effect: Option<&ParticleEffect>) -> Option<String> {
    let e = effect?.sanitized();
    match serde_json::to_string(&e) {
        Ok(s) => Some(s),
        Err(err) => {
            tracing::error!("could not serialise particles: {err}");
            None
        }
    }
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
    tune(&conn)?;
    migrate(&mut conn)?;
    Ok(conn)
}

/// Put the connection into the mode this server actually needs, which is not the
/// one SQLite opens in.
///
/// Every write here runs on a Tokio worker thread behind the single shared
/// `Mutex<Connection>`, so the time a commit spends in the kernel is time no
/// other connection's work can start — including the 20 Hz tick. The defaults
/// are the slowest possible choice for that shape:
///
/// * **`journal_mode = WAL`.** The default rollback journal creates, writes,
///   fsyncs and deletes a separate `-journal` file around *every* transaction,
///   and holds a whole-database write lock while it does. WAL appends to one
///   file and lets readers run through a commit instead of waiting behind it.
/// * **`synchronous = NORMAL`.** Under WAL this is the documented safe setting:
///   a power cut can lose the last few transactions but cannot corrupt the
///   database. `FULL` fsyncs on each commit, which is what makes a prim flush a
///   disk-latency event rather than a memcpy. The thing being persisted is prim
///   positions a script is about to move again, so losing the last instant of it
///   costs nothing the tick loop's own flush cadence does not already cost.
/// * **`busy_timeout`.** Nothing in this process contends — the mutex serialises
///   it — but a second writer does exist in practice: `simulator --create-admin`
///   against the live file, and whatever an operator has open. Without a timeout
///   those meet an instant `SQLITE_BUSY` instead of waiting the moment out.
///
/// Best-effort on purpose. A database that refuses WAL (a network filesystem is
/// the usual reason) is still a database this server can run on, so a refusal is
/// logged and the boot continues — the alternative is a server that will not
/// start over a performance setting.
fn tune(conn: &Connection) -> anyhow::Result<()> {
    // `query_row`, not `execute`: `PRAGMA journal_mode` *returns* the mode it
    // ended up in, and rusqlite's `execute` treats a statement that yields a row
    // as an error ("Execute returned results"). The returned value is also the
    // only honest way to log what we actually got.
    let mode: String = conn
        .query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))
        .unwrap_or_else(|e| {
            tracing::warn!("could not set journal_mode=WAL: {e}");
            "unknown".into()
        });
    if !mode.eq_ignore_ascii_case("wal") {
        tracing::warn!(mode = %mode, "sqlite is not in WAL mode; writes will be slower");
    }
    if let Err(e) = conn.pragma_update(None, "synchronous", "NORMAL") {
        tracing::warn!("could not set synchronous=NORMAL: {e}");
    }
    if let Err(e) = conn.busy_timeout(std::time::Duration::from_secs(5)) {
        tracing::warn!("could not set busy_timeout: {e}");
    }
    Ok(())
}

/// Bring an already-open connection up to the current schema.
///
/// Public so a test can build the *real* schema in memory instead of a
/// hand-written approximation of it — an approximation is how a suite goes on
/// passing against columns production no longer has, which is exactly what the
/// account tables did until registration added two more.
pub fn migrate(conn: &mut Connection) -> anyhow::Result<()> {
    embedded::migrations::runner()
        .run(conn)
        .context("refinery migrate")?;
    Ok(())
}

/// Migrate only up to (and including) `version` — for a test that has to seed
/// rows in an old schema and watch a later migration carry them across.
#[cfg(test)]
pub fn migrate_to(conn: &mut Connection, version: u32) -> anyhow::Result<()> {
    embedded::migrations::runner()
        .set_target(refinery::Target::Version(version))
        .run(conn)
        .context("refinery migrate")?;
    Ok(())
}

pub fn load_world(conn: &Connection) -> anyhow::Result<Vec<PrimDto>> {
    let sql = format!("SELECT {PRIM_COLUMNS} FROM prims ORDER BY id");
    let mut stmt = conn.prepare(&sql)?;
    let prims = stmt
        .query_map([], row_to_prim)?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(prims)
}

/// Create a new prim in the database.
/// Returns the full PrimDto with server-assigned id.
/// (ADR-017 Phase 2)
pub fn insert_prim(
    conn: &Connection,
    position: Vec3,
    shape: &str,
    geo: GeoAnchor,
    particles: Option<&ParticleEffect>,
    model: Option<&str>,
) -> anyhow::Result<PrimDto> {
    // Insert with sensible defaults
    conn.execute(
        "INSERT INTO prims (name, shape, position_x, position_y, position_z,
                            rotation_x, rotation_y, rotation_z, scale_x, scale_y, scale_z,
                            color_r, color_g, color_b, texture_id, latitude, longitude,
                            particles, model, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, datetime('now'), datetime('now'))",
        rusqlite::params![
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
            particles_json(particles),
            model,
        ],
    )?;

    // Retrieve the newly created prim
    let id = conn.last_insert_rowid();
    let prim = select_prim_by_id(conn, id)?
        .ok_or_else(|| anyhow::anyhow!("failed to retrieve newly inserted prim {}", id))?;

    tracing::debug!(prim_id = id, shape, "prim created");
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
    particles: Option<&ParticleEffect>,
    model: Option<&str>,
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
            particles = ?39,
            model = ?40,
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
            particles_json(particles),
            model,
        ],
    )?;

    // Retrieve the updated prim
    let prim = select_prim_by_id(conn, prim_id)?
        .ok_or_else(|| anyhow::anyhow!("failed to retrieve updated prim {}", prim_id))?;

    tracing::debug!(prim_id, "prim updated");
    Ok(Some(prim))
}

/// Start or stop the video on a prim (`V22__prim_video.sql`). Its own write,
/// because nothing else may touch the column: `update_prim` rewrites every
/// other field on every edit, and a video that stopped whenever somebody moved
/// the screen would be a bug in every tool that edits prims.
///
/// Returns the prim as it now is, or `None` if there is no such prim.
pub fn set_prim_video(
    conn: &Connection,
    prim_id: i64,
    video: Option<&cartopolis_core::PrimVideo>,
) -> anyhow::Result<Option<PrimDto>> {
    let json = video.map(serde_json::to_string).transpose()?;
    let changed = conn.execute(
        "UPDATE prims SET video = ?2 WHERE id = ?1",
        rusqlite::params![prim_id, json],
    )?;
    if changed == 0 {
        return Ok(None);
    }
    select_prim_by_id(conn, prim_id)
}

/// Switch a prim's physics (`V26__prim_physical.sql`). Its own write for the
/// reason [`set_prim_video`] is: `update_prim` is every edit, and an edit must
/// not be able to switch a ball off by leaving the flag out. `false` when there
/// is no such prim.
pub fn set_prim_physical(conn: &Connection, prim_id: i64, physical: bool) -> anyhow::Result<bool> {
    let changed = conn.execute(
        "UPDATE prims SET physical = ?2 WHERE id = ?1",
        rusqlite::params![prim_id, physical],
    )?;
    Ok(changed > 0)
}

/// One clip in the video store, as measured when it was uploaded.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct VideoRow {
    pub hash: String,
    pub byte_size: u64,
    pub width: u32,
    pub height: u32,
    pub duration_ms: u64,
    pub has_audio: bool,
}

/// Record a clip. Uploading the same bytes twice is one row.
pub fn insert_video(conn: &Connection, v: &VideoRow, uploaded_by: &str) -> anyhow::Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO videos \
         (hash, byte_size, width, height, duration_ms, has_audio, uploaded_by, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, strftime('%s','now'))",
        rusqlite::params![
            v.hash,
            v.byte_size as i64,
            v.width,
            v.height,
            v.duration_ms as i64,
            v.has_audio,
            uploaded_by
        ],
    )?;
    Ok(())
}

/// A clip in the store, if there is one with this hash.
pub fn video(conn: &Connection, hash: &str) -> anyhow::Result<Option<VideoRow>> {
    Ok(conn
        .query_row(
            "SELECT hash, byte_size, width, height, duration_ms, has_audio FROM videos \
             WHERE hash = ?1",
            rusqlite::params![hash],
            |r| {
                Ok(VideoRow {
                    hash: r.get(0)?,
                    byte_size: r.get::<_, i64>(1)? as u64,
                    width: r.get(2)?,
                    height: r.get(3)?,
                    duration_ms: r.get::<_, i64>(4)? as u64,
                    has_audio: r.get(5)?,
                })
            },
        )
        .optional()?)
}

/// Record which content pack installed a prim (`V14__prim_origin.sql`).
///
/// A separate write rather than a column on the insert, because it is not part of
/// what an author describes: `insert_prim` is reached from the wire, from the
/// admin API and from a pack, and only the last of those has an origin to state.
pub fn set_prim_origin(conn: &Connection, prim_id: i64, origin: &str) -> anyhow::Result<()> {
    conn.execute(
        "UPDATE prims SET origin = ?2 WHERE id = ?1",
        rusqlite::params![prim_id, origin],
    )?;
    Ok(())
}

/// The current time, as SQLite's `datetime('now')` renders it.
///
/// Asked of SQLite rather than formatted here so an exported timestamp reads the
/// same as every `created_at` in this schema, and so the crate needs no date
/// formatting dependency for one string.
#[must_use]
pub fn now_iso8601() -> String {
    time_now().unwrap_or_default()
}

fn time_now() -> Option<String> {
    let conn = Connection::open_in_memory().ok()?;
    conn.query_row("SELECT datetime('now')", [], |r| r.get(0))
        .ok()
}

/// Record who built a prim (`V15__prim_owner.sql`).
///
/// A separate write for the same reason [`set_prim_origin`] is: `insert_prim` is
/// reached from the wire, from the admin API and from a content pack, and only
/// some of those have an owner to name.
pub fn set_prim_owner(conn: &Connection, prim_id: i64, owner: &str) -> anyhow::Result<()> {
    conn.execute(
        "UPDATE prims SET owner = ?2 WHERE id = ?1",
        rusqlite::params![prim_id, owner],
    )?;
    Ok(())
}

/// Who owns a prim, if anybody does.
pub fn prim_owner(conn: &Connection, prim_id: i64) -> anyhow::Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT owner FROM prims WHERE id = ?1",
            rusqlite::params![prim_id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten())
}

/// Every prim an account owns, oldest first, with its script source.
///
/// The script comes back in the same query rather than one round trip per prim:
/// an export of a hundred objects would otherwise be a hundred statements, and
/// the source is exactly the part an export exists to carry.
pub fn owned_prims(
    conn: &Connection,
    owner: &str,
) -> anyhow::Result<Vec<(PrimDto, Option<String>)>> {
    let sql = format!(
        "SELECT {PRIM_COLUMNS}, (SELECT source FROM prim_scripts s WHERE s.prim_id = prims.id)          FROM prims WHERE owner = ?1 ORDER BY id"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(rusqlite::params![owner], |row| {
            // The script is one column past the last of `PRIM_COLUMNS`; the
            // `scripted` flag that `row_to_prim` reads is the one before it.
            let source: Option<String> = row.get(PRIM_COLUMN_COUNT)?;
            Ok((row_to_prim(row)?, source))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Every prim in the world with its script, for an admin's whole-world export.
pub fn all_prims_with_scripts(conn: &Connection) -> anyhow::Result<Vec<(PrimDto, Option<String>)>> {
    let sql = format!(
        "SELECT {PRIM_COLUMNS}, (SELECT source FROM prim_scripts s WHERE s.prim_id = prims.id) \
         FROM prims ORDER BY id"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map([], |row| {
            let source: Option<String> = row.get(PRIM_COLUMN_COUNT)?;
            Ok((row_to_prim(row)?, source))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Every prim that has an owner, for the world's in-memory permission map.
///
/// Loaded once at startup and maintained in [`crate::state::SimWorld`] thereafter,
/// on the same rule the scripted set follows: `UpdatePrim` arrives at the gizmo's
/// drag rate, and a permission check that read the database twenty times a second
/// per dragging client would put SQLite on the movement path.
pub fn load_prim_owners(conn: &Connection) -> anyhow::Result<HashMap<i64, String>> {
    let mut stmt = conn.prepare("SELECT id, owner FROM prims WHERE owner IS NOT NULL")?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?
        .collect::<Result<HashMap<_, _>, _>>()?;
    Ok(rows)
}

/// An account's uuid by username, if the account exists and is enabled.
///
/// Enabled matters here because this resolves the *target* of an ownership
/// transfer: handing a build to a disabled account puts it somewhere nobody can
/// reach, which is a way to lose work rather than to move it.
pub fn enabled_account_id(conn: &Connection, username: &str) -> anyhow::Result<Option<String>> {
    // Case-insensitive, because a full address is lower-cased
    // (`cartopolis_core::address`, protocol 37) while an account made by
    // `--create-admin` keeps the capitals it was typed with. Two accounts that
    // differ only in case can exist (the column is case-sensitive), so an exact
    // match wins.
    Ok(conn
        .query_row(
            "SELECT id FROM accounts WHERE username = ?1 COLLATE NOCASE AND disabled = 0
             ORDER BY username = ?1 DESC LIMIT 1",
            rusqlite::params![username],
            |r| r.get(0),
        )
        .optional()?)
}

/// Every prim a given content pack installed, oldest first.
pub fn prim_ids_from(conn: &Connection, origin: &str) -> anyhow::Result<Vec<i64>> {
    let mut stmt = conn.prepare("SELECT id FROM prims WHERE origin = ?1 ORDER BY id")?;
    let ids = stmt
        .query_map(rusqlite::params![origin], |row| row.get(0))?
        .collect::<Result<Vec<i64>, _>>()?;
    Ok(ids)
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
    // world's script budget forever.
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
    subject_asset: Option<&str>,
) -> anyhow::Result<i64> {
    conn.execute(
        "INSERT INTO reports (reporter_id, reporter_username, subject_username, reason, note, message, subject_asset) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![
            reporter_id,
            reporter_username,
            subject_username,
            reason,
            note,
            message,
            subject_asset
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
    // A house held by an account that no longer exists is claimable by nobody
    // and visible to no one — the worst of both, so deletion frees it.
    crate::buildings::erase_for(conn, principal)?;
    // Public posts are the account's own words and go with it. Other servers
    // keep the copies they were sent; telling them is `fediverse`'s job when
    // it is told, and an account deleted from the shell is not.
    crate::fediverse::posts::forget_account(conn, principal).map_err(|e| anyhow::anyhow!(e))?;
    // Communities first: leaving one needs the topic memberships the next
    // step deletes. A community the account owned passes on or goes.
    crate::communities::erase_account(conn, principal)?;
    // Devices, key packages and memberships go; the sealed logs stay, being
    // the other members' history (`e2ee::erase_account` says why that is safe).
    crate::e2ee::erase_account(conn, principal)?;
    crate::contacts::erase_account(conn, principal)?;
    // A role held by nobody is harmless; one held by a recycled principal is
    // not, so the grants go with the account.
    crate::roles::forget_account(conn, principal)?;
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

    /// The two export queries append a column after `PRIM_COLUMNS` and read it by
    /// index. That index is derived from the list, and this is what says the
    /// derivation is right — adding `model` to the list is exactly what broke it
    /// when the index was a literal, and every export answered `500`.
    #[test]
    fn the_column_count_matches_the_column_list() {
        let counted = PRIM_COLUMNS.split(',').count();
        assert_eq!(
            PRIM_COLUMN_COUNT, counted,
            "the const and the list have to agree"
        );
        // And no column may contain a comma of its own, or counting them is not
        // counting columns. The `EXISTS (SELECT …)` subquery is the one to watch.
        assert!(
            !PRIM_COLUMNS.contains("(SELECT 1 FROM prim_scripts s WHERE s.prim_id = prims.id),"),
            "a comma inside a column expression would break the count"
        );
        for column in PRIM_COLUMNS.split(',') {
            assert_eq!(
                column.matches('(').count(),
                column.matches(')').count(),
                "unbalanced brackets in `{column}` mean a comma split a column in half"
            );
        }
    }

    #[test]
    fn test_insert_prim() -> anyhow::Result<()> {
        let (conn, _temp) = test_db()?;

        // Create a prim
        let prim = insert_prim(
            &conn,
            Vec3::new(10.0, 5.0, 20.0),
            "box",
            GeoAnchor {
                latitude: 0.0,
                longitude: 0.0,
            },
            None,
            None,
        )?;

        // Verify the prim was created
        assert!(prim.id > 0);
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
    fn test_update_prim() -> anyhow::Result<()> {
        let (conn, _temp) = test_db()?;

        // Create a prim
        let prim = insert_prim(
            &conn,
            Vec3::ZERO,
            "box",
            GeoAnchor {
                latitude: 0.0,
                longitude: 0.0,
            },
            None,
            None,
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
            None,
            None,
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
            None,
            None,
        )?;

        assert!(result.is_none());

        Ok(())
    }

    #[test]
    fn test_delete_prim() -> anyhow::Result<()> {
        let (conn, _temp) = test_db()?;

        // Create a prim
        let prim = insert_prim(
            &conn,
            Vec3::ZERO,
            "box",
            GeoAnchor {
                latitude: 0.0,
                longitude: 0.0,
            },
            None,
            None,
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

        // Create a prim with a texture
        let prim = insert_prim(
            &conn,
            Vec3::ZERO,
            "sphere",
            GeoAnchor {
                latitude: 0.0,
                longitude: 0.0,
            },
            None,
            None,
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
            None,
            None,
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

        // Create two prims
        let prim1 = insert_prim(
            &conn,
            Vec3::new(0.0, 0.0, 0.0),
            "box",
            GeoAnchor {
                latitude: 0.0,
                longitude: 0.0,
            },
            None,
            None,
        )?;
        let prim2 = insert_prim(
            &conn,
            Vec3::new(10.0, 0.0, 0.0),
            "sphere",
            GeoAnchor {
                latitude: 0.0,
                longitude: 0.0,
            },
            None,
            None,
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
            None,
            None,
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

    /// The write mode is a property of the file, and it is set exactly once — at
    /// `open_and_migrate`. Nothing downstream checks it, so a `tune` that stopped
    /// being called would cost every commit a rollback journal and a full fsync
    /// with nothing anywhere to say so.
    #[test]
    fn the_database_opens_in_wal_mode() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let path = dir.path().join("wal.db");
        let conn = open_and_migrate(path.to_str().unwrap()).expect("open");

        let mode: String = conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .expect("journal_mode");
        assert_eq!(mode.to_lowercase(), "wal");

        // 1 is NORMAL. Under WAL that is the documented durable-enough setting:
        // it cannot corrupt the file, it can only lose the last commits, and what
        // is being committed here is prim positions a script is about to move
        // again.
        let sync: i64 = conn
            .query_row("PRAGMA synchronous", [], |r| r.get(0))
            .expect("synchronous");
        assert_eq!(sync, 1, "synchronous = NORMAL");
    }
}

// ---------------------------------------------------------------------------
// Push endpoints (V23): where to wake a device whose app is closed.
// ---------------------------------------------------------------------------

/// Record `endpoint` as `account`'s, moving it from anyone who had it.
pub fn set_push_endpoint(
    conn: &Connection,
    account: &str,
    endpoint: &str,
    now_ms: i64,
) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO push_endpoints (endpoint, account, updated_ms) VALUES (?1, ?2, ?3)
         ON CONFLICT(endpoint) DO UPDATE SET account = excluded.account, updated_ms = excluded.updated_ms",
        rusqlite::params![endpoint, account, now_ms],
    )?;
    Ok(())
}

/// Forget `endpoint`, if it is `account`'s — a device can only take back its own.
pub fn remove_push_endpoint(
    conn: &Connection,
    account: &str,
    endpoint: &str,
) -> anyhow::Result<()> {
    conn.execute(
        "DELETE FROM push_endpoints WHERE endpoint = ?1 AND account = ?2",
        rusqlite::params![endpoint, account],
    )?;
    Ok(())
}

/// Forget `endpoint` whoever it belonged to: its distributor said it is gone.
pub fn drop_push_endpoint(conn: &Connection, endpoint: &str) -> anyhow::Result<()> {
    conn.execute(
        "DELETE FROM push_endpoints WHERE endpoint = ?1",
        rusqlite::params![endpoint],
    )?;
    Ok(())
}

/// Every endpoint registered by `account`.
pub fn push_endpoints(conn: &Connection, account: &str) -> anyhow::Result<Vec<String>> {
    let mut stmt =
        conn.prepare("SELECT endpoint FROM push_endpoints WHERE account = ?1 ORDER BY endpoint")?;
    let out = stmt
        .query_map(rusqlite::params![account], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(out)
}

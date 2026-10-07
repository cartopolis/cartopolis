//! Making, changing, scripting and deleting prims — once, for both surfaces.
//!
//! # Why one service
//!
//! A prim can be built from two places: the wire (`CreatePrim`, `UpdatePrim`,
//! `DeletePrim`, `SetPrimScript` — a player in front of the thing) and the
//! authoring API (`/api/admin/prims`, content packs, `/api/content/import` — an
//! operator or a document). Each used to carry its own copy of the rules: who may
//! edit, what the budget allows, whether a model hash is real, who ends up owning
//! the result, what gets written, what the script host is told and what every
//! client is sent. Two copies of a permission check are two answers waiting to
//! disagree, and the one that drifts is always the one nobody is testing.
//!
//! So both call paths come here with an [`Actor`] — who is asking, and whether
//! they are an admin — and keep only what is genuinely theirs: how a refusal is
//! *reported*. The wire answers a 403 with the prim's true state so a client can
//! undo its optimistic edit (`net::refuse_and_correct`); HTTP answers with a
//! status code. The decision behind both is [`PrimError`], made here.
//!
//! # The database is never touched under the world lock
//!
//! [`SimWorld`] is behind one `RwLock` that the tick loop takes twenty times a
//! second. Every SQLite write used to happen *inside* that lock, so a commit that
//! waited on the disk stalled every connected client, and the tick with them.
//! Each operation here is therefore three steps: decide under the lock, write
//! with it released (on a blocking thread, `db::call`), then take it again to make
//! the in-memory copy agree.
//!
//! Releasing the lock in the middle opens a window, and one mutex closes it:
//! `writer`. Every operation that writes an existing prim's row — an edit, a
//! delete, a script store and the periodic flush of script writes — holds it from
//! the moment it reads the prim to the moment memory agrees with the row. That
//! keeps the order rows are written in the same as the order the world changed
//! in; without it, a flush that snapshotted a prim before an edit could land its
//! write after the edit's and put the old values back on disk. The lock order is
//! always `writer`, then the world, then SQLite, and nothing takes `writer` while
//! holding the world lock.
//!
//! Creation does not need it: a new prim's id is the database's, so nothing else
//! can name it until it is in the world, and the budget is held by a reservation
//! ([`SimWorld::reserve_prim`]) rather than by the lock.

use crate::db::{self, SharedConn};
use crate::scripting::ScriptHandle;
use crate::state::{DirtyBatch, SimWorld};
use bytes::Bytes;
use cartopolis_core::{
    encode_app_frame, GeoAnchor, NetMessage, ParticleEffect, Permission, PermissionSet, PrimDto,
    PrimGeometry, PrimSurface,
};
use glam::Vec3;
use rusqlite::Connection;
use std::sync::Arc;
use tokio::sync::{broadcast, RwLock};

/// Who is asking, as far as a prim is concerned.
///
/// The wire builds one per connection at the handshake — which is where its
/// permissions are read, once, for the reason `net::handle_connection` gives —
/// and the authoring API builds one per request.
#[derive(Debug, Clone)]
pub struct Actor {
    /// The account, as the string `prims.owner` stores. `None` is the server
    /// itself: an operator authoring through the admin API, or a content pack.
    pub principal: Option<String>,
    /// What this actor may do (`roles::effective`; `docs/notes/permissions.md`).
    pub perms: PermissionSet,
}

impl Actor {
    /// A signed-in account holding these permissions.
    #[must_use]
    pub fn with_permissions(principal: impl Into<String>, perms: PermissionSet) -> Self {
        Self {
            principal: Some(principal.into()),
            perms,
        }
    }

    /// A signed-in account that is an admin (every permission) or holds the
    /// player role's defaults — the shape the tests and the old flag knew.
    #[must_use]
    pub fn account(principal: impl Into<String>, is_admin: bool) -> Self {
        let perms = if is_admin {
            PermissionSet::all()
        } else {
            cartopolis_core::permissions::ALL
                .iter()
                .copied()
                .filter(|p| p.player_default())
                .collect()
        };
        Self::with_permissions(principal, perms)
    }

    /// The server building on its own behalf. What it makes is owned by nobody,
    /// which is what "belongs to the server" means (`SimWorld::may_edit`).
    #[must_use]
    pub fn server() -> Self {
        Self {
            principal: None,
            perms: PermissionSet::all(),
        }
    }

    #[must_use]
    pub fn can(&self, p: Permission) -> bool {
        self.perms.has(p)
    }

    fn principal_str(&self) -> &str {
        self.principal.as_deref().unwrap_or_default()
    }

    /// May this actor change or delete `prim_id`? Its own with
    /// [`Permission::PrimEditOwn`], anybody's — and the server's — with
    /// [`Permission::PrimEditAny`]. The ownership half is [`SimWorld::owns`].
    #[must_use]
    pub fn may_edit(&self, world: &SimWorld, prim_id: i64) -> bool {
        self.can(Permission::PrimEditAny)
            || (self.can(Permission::PrimEditOwn) && world.owns(prim_id, self.principal_str()))
    }

    /// May this actor write `prim_id`'s script? The same rule over the script
    /// permissions. A script can move, rename and delete its prim, so writing
    /// one is never less than editing it, and the role decides who may do which.
    #[must_use]
    pub fn may_script(&self, world: &SimWorld, prim_id: i64) -> bool {
        self.can(Permission::ScriptEditAny)
            || (self.can(Permission::ScriptEditOwn) && world.owns(prim_id, self.principal_str()))
    }

    /// May this actor read `prim_id`'s script source?
    #[must_use]
    pub fn may_read_script(&self, world: &SimWorld, prim_id: i64) -> bool {
        self.can(Permission::ScriptReadAny)
            || self.can(Permission::ScriptEditAny)
            || world.owns(prim_id, self.principal_str())
    }
}

/// Whether a prim's model hash has to be one this server holds.
///
/// A player's build is checked, because a hash this server has never held is a
/// typo, a guess or a model from somebody else's server, and the prim it makes is
/// one no client can ever draw — indistinguishable from one still downloading.
/// An operator's authoring is not: seeding a world before uploading its models
/// to it is a legitimate order to do things in (see `authoring::PrimSpec::model`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelCheck {
    Required,
    Skip,
}

/// Why an operation on a prim did not happen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrimError {
    /// There is no such prim.
    NotFound(i64),
    /// There is, and it is not this actor's to change.
    NotYours(i64),
    /// The request itself was refused: the budget, a model, a shape, a limit.
    /// The message is written to be shown to the person who asked.
    Refused(String),
    /// The database failed. Not the caller's fault, and not something they can
    /// fix by asking differently.
    Storage(String),
    /// The actor's roles do not allow this (`docs/notes/permissions.md`). The
    /// message is written to be shown to the person who asked.
    Forbidden(String),
}

impl std::fmt::Display for PrimError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound(id) => write!(f, "prim {id} not found"),
            Self::NotYours(id) => f.write_str(&not_yours(*id)),
            Self::Refused(message) | Self::Storage(message) | Self::Forbidden(message) => {
                f.write_str(message)
            }
        }
    }
}

/// The refusal for editing something that is not yours.
///
/// One sentence, defined once, because it is reached from every kind of edit on
/// both surfaces, and a rule that is explained differently depending on which
/// message tripped it reads as several different rules.
#[must_use]
pub fn not_yours(prim_id: i64) -> String {
    format!(
        "prim {prim_id} belongs to somebody else. You can change what you built; \
         an operator can change anything."
    )
}

/// Every field an edit writes — everything about a prim except its id and its
/// shape, which only the insert sets.
///
/// Owned rather than borrowed because it crosses onto the blocking thread that
/// writes it.
#[derive(Debug, Clone)]
pub struct PrimFields {
    pub position: Vec3,
    pub rotation: Vec3,
    pub scale: Vec3,
    pub color: [f32; 3],
    pub texture_id: Option<String>,
    pub name: String,
    pub surface: PrimSurface,
    pub geometry: PrimGeometry,
    pub geo: GeoAnchor,
    pub particles: Option<ParticleEffect>,
    pub model: Option<String>,
}

impl PrimFields {
    /// What a prim currently is, as the fields an edit would write — the base an
    /// authoring patch merges onto, and what a flush writes back.
    #[must_use]
    pub fn of(p: &PrimDto) -> Self {
        Self {
            position: p.position,
            rotation: p.rotation,
            scale: p.scale,
            color: p.color,
            texture_id: p.texture_id.clone(),
            name: p.name.clone(),
            surface: p.surface,
            geometry: geometry_of(p),
            geo: p.geo,
            particles: p.particles,
            model: p.model.clone(),
        }
    }

    /// Write these fields onto a stored prim. `None` when there is no such row.
    fn write(&self, c: &Connection, prim_id: i64) -> anyhow::Result<Option<PrimDto>> {
        db::update_prim(
            c,
            prim_id,
            self.position,
            self.rotation,
            self.scale,
            self.color,
            self.texture_id.clone(),
            &self.name,
            self.surface,
            self.geometry,
            self.geo,
            self.particles.as_ref(),
            self.model.as_deref(),
        )
    }
}

/// Re-assemble the flattened geometry fields of a [`PrimDto`] into the grouped
/// [`PrimGeometry`] that the write path takes.
#[must_use]
pub fn geometry_of(p: &PrimDto) -> PrimGeometry {
    PrimGeometry {
        path_cut_begin: p.path_cut_begin,
        path_cut_end: p.path_cut_end,
        hollow: p.hollow,
        twist_begin: p.twist_begin,
        twist_end: p.twist_end,
        taper_x: p.taper_x,
        taper_y: p.taper_y,
        top_shear_x: p.top_shear_x,
        top_shear_y: p.top_shear_y,
        slice_begin: p.slice_begin,
        slice_end: p.slice_end,
    }
}

/// What the insert of a new prim takes: the three things `insert_prim` writes
/// that an edit cannot.
#[derive(Debug, Clone)]
pub struct PrimSeed {
    pub position: Vec3,
    pub shape: String,
    pub geo: GeoAnchor,
}

/// The one way prims are made, changed, scripted and deleted. Cheap to clone.
#[derive(Clone)]
pub struct PrimService {
    world: Arc<RwLock<SimWorld>>,
    conn: SharedConn,
    tx_snap: broadcast::Sender<Bytes>,
    scripts: ScriptHandle,
    /// Serialises every write to an existing prim's row against every other —
    /// see the module note for why it has to span the whole operation.
    writer: Arc<tokio::sync::Mutex<()>>,
}

impl PrimService {
    #[must_use]
    pub fn new(
        world: Arc<RwLock<SimWorld>>,
        conn: SharedConn,
        tx_snap: broadcast::Sender<Bytes>,
        scripts: ScriptHandle,
    ) -> Self {
        Self {
            world,
            conn,
            tx_snap,
            scripts,
            writer: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    /// Make a prim: claim room in the budget, store it, then add it to the world
    /// and tell every client.
    ///
    /// `finish` turns the freshly inserted defaults into the prim's full initial
    /// state — the whole of it for the wire, which sends every field, and a merge
    /// for an author, who names only some. It runs inside the same transaction as
    /// the insert, so a create either happened completely or not at all: a
    /// refused field used to leave a grey box named "Prim" behind for the caller
    /// to delete.
    ///
    /// The actor's account, if it has one, becomes the owner (schema V15). That
    /// write is best-effort: a prim that exists with no owner is a prim nobody can
    /// export, which is better than refusing a build over a bookkeeping write.
    pub async fn create<F>(
        &self,
        actor: &Actor,
        seed: PrimSeed,
        finish: F,
        models: ModelCheck,
    ) -> Result<PrimDto, PrimError>
    where
        F: FnOnce(&PrimDto) -> Result<PrimFields, String> + Send + 'static,
    {
        if !actor.can(Permission::PrimCreate) {
            return Err(PrimError::Forbidden(
                "this account may not build prims on this server".into(),
            ));
        }
        self.world
            .write()
            .await
            .reserve_prim()
            .map_err(PrimError::Refused)?;
        let owner = actor.principal.clone();
        let stored = db::call(&self.conn, move |c| {
            store_new(c, seed, finish, models, owner.as_deref())
        })
        .await;
        let stored = match stored {
            Ok(result) => result,
            Err(e) => Err(PrimError::Storage(format!("insert_prim failed: {e:#}"))),
        };
        let prim = match stored {
            Ok(prim) => prim,
            Err(e) => {
                self.world.write().await.release_prim_reservation();
                return Err(e);
            }
        };
        self.world
            .write()
            .await
            .commit_prim(prim.clone(), actor.principal.as_deref());
        self.broadcast(&NetMessage::PrimUpsert { prim: prim.clone() });
        self.broadcast_capacity().await;
        Ok(prim)
    }

    /// Change a prim: `edit` computes its new fields from what it is now.
    ///
    /// The prim must exist and be the actor's to change, in that order — a prim
    /// that is not there is "not found" to everybody, rather than "not yours" to
    /// everybody but an admin.
    pub async fn update<F>(
        &self,
        actor: &Actor,
        prim_id: i64,
        edit: F,
        models: ModelCheck,
    ) -> Result<PrimDto, PrimError>
    where
        F: FnOnce(&PrimDto) -> Result<PrimFields, String>,
    {
        let _writer = self.writer.lock().await;
        let base = {
            let w = self.world.read().await;
            editable(&w, actor, prim_id)?
        };
        let fields = edit(&base).map_err(PrimError::Refused)?;
        let written = db::call(&self.conn, move |c| -> Result<PrimDto, PrimError> {
            if models == ModelCheck::Required {
                check_model(c, fields.model.as_deref())?;
            }
            fields
                .write(c, prim_id)
                .map_err(|e| PrimError::Storage(format!("update_prim failed: {e:#}")))?
                .ok_or(PrimError::NotFound(prim_id))
        })
        .await;
        let prim = match written {
            Ok(result) => result?,
            Err(e) => return Err(PrimError::Storage(format!("update_prim failed: {e:#}"))),
        };
        // Back into the world, and what is broadcast is what the world now holds
        // — including the `scripted` flag the world keeps beside the script store.
        let stored = {
            let mut w = self.world.write().await;
            if w.replace_prim(prim) {
                w.prim(prim_id).cloned()
            } else {
                None
            }
        };
        let prim = stored.ok_or(PrimError::NotFound(prim_id))?;
        self.broadcast(&NetMessage::PrimUpsert { prim: prim.clone() });
        Ok(prim)
    }

    /// Start a clip on a prim (`Some`: a hash in the video store, and whether
    /// it loops) or stop the one playing (`None`) — protocol 28. The same
    /// ownership rule as any edit; the start time is this server's clock, so
    /// every client measures its position from one instant.
    pub async fn set_video(
        &self,
        actor: &Actor,
        prim_id: i64,
        video: Option<cartopolis_core::SetVideo>,
        now_ms: u64,
    ) -> Result<PrimDto, PrimError> {
        let _writer = self.writer.lock().await;
        {
            let w = self.world.read().await;
            editable(&w, actor, prim_id)?;
            if !actor.can(Permission::PrimVideo) {
                return Err(PrimError::Forbidden(
                    "this account may not play videos on prims".into(),
                ));
            }
        }
        let written = db::call(&self.conn, move |c| -> Result<PrimDto, PrimError> {
            let video = match video {
                Some(v) => {
                    let known = db::video(c, &v.asset)
                        .map_err(|e| PrimError::Storage(format!("video lookup failed: {e:#}")))?;
                    if known.is_none() {
                        return Err(PrimError::Refused(
                            "this server does not have that video".to_string(),
                        ));
                    }
                    Some(cartopolis_core::PrimVideo {
                        asset: v.asset,
                        started_at_ms: now_ms,
                        looped: v.looped,
                    })
                }
                None => None,
            };
            db::set_prim_video(c, prim_id, video.as_ref())
                .map_err(|e| PrimError::Storage(format!("set_prim_video failed: {e:#}")))?
                .ok_or(PrimError::NotFound(prim_id))
        })
        .await;
        let prim = match written {
            Ok(result) => result?,
            Err(e) => return Err(PrimError::Storage(format!("set_prim_video failed: {e:#}"))),
        };
        let stored = {
            let mut w = self.world.write().await;
            if w.replace_prim(prim) {
                w.prim(prim_id).cloned()
            } else {
                None
            }
        };
        let prim = stored.ok_or(PrimError::NotFound(prim_id))?;
        self.broadcast(&NetMessage::PrimUpsert { prim: prim.clone() });
        Ok(prim)
    }

    /// Switch a prim's physics on or off (protocol 34). The same ownership rule
    /// as any edit, and the world's [`crate::physics::MAX_PHYSICAL_PRIMS`]
    /// ceiling, checked before the row is written.
    pub async fn set_physical(
        &self,
        actor: &Actor,
        prim_id: i64,
        physical: bool,
    ) -> Result<PrimDto, PrimError> {
        let _writer = self.writer.lock().await;
        {
            let w = self.world.read().await;
            let prim = editable(&w, actor, prim_id)?;
            if !actor.can(Permission::PrimPhysical) {
                return Err(PrimError::Forbidden(
                    "this account may not make prims physical".into(),
                ));
            }
            if physical
                && !prim.physical
                && w.physical_count() >= crate::physics::MAX_PHYSICAL_PRIMS
            {
                return Err(PrimError::Refused(format!(
                    "this world already has {} physical prims, the most it may hold",
                    crate::physics::MAX_PHYSICAL_PRIMS
                )));
            }
        }
        let written = db::call(&self.conn, move |c| {
            db::set_prim_physical(c, prim_id, physical)
        })
        .await;
        match written {
            Ok(Ok(true)) => {}
            Ok(Ok(false)) => return Err(PrimError::NotFound(prim_id)),
            Ok(Err(e)) | Err(e) => {
                return Err(PrimError::Storage(format!(
                    "set_prim_physical failed: {e:#}"
                )))
            }
        }
        let prim = {
            let mut w = self.world.write().await;
            w.set_physical(prim_id, physical)
                .map_err(PrimError::Refused)?
                .ok_or(PrimError::NotFound(prim_id))?
        };
        self.broadcast(&NetMessage::PrimUpsert { prim: prim.clone() });
        Ok(prim)
    }

    /// Delete a prim: its row, its script, its place in the world, and every
    /// client's copy.
    ///
    /// The row goes first and the world follows, so a failed delete leaves the
    /// prim standing everywhere rather than gone from the screen and back after a
    /// restart.
    pub async fn delete(&self, actor: &Actor, prim_id: i64) -> Result<(), PrimError> {
        let _writer = self.writer.lock().await;
        {
            let w = self.world.read().await;
            editable(&w, actor, prim_id)?;
        }
        let deleted = match db::call(&self.conn, move |c| db::delete_prim(c, prim_id)).await {
            Ok(Ok(deleted)) => deleted,
            Ok(Err(e)) | Err(e) => {
                return Err(PrimError::Storage(format!("delete_prim failed: {e:#}")))
            }
        };
        if !deleted {
            return Err(PrimError::NotFound(prim_id));
        }
        self.world.write().await.forget_prim(prim_id);
        // The live VM is the host's, and has to be told — the row is gone, but
        // the host keeps its own map keyed by prim id and would go on running a
        // script for a prim nobody can see.
        self.scripts.unload(prim_id);
        self.broadcast(&NetMessage::PrimRemoved { id: prim_id });
        self.broadcast_capacity().await;
        Ok(())
    }

    /// Store a prim's script, or with an empty source remove it. Returns whether
    /// the prim now carries one.
    ///
    /// Trimmed first: an editor that saves an untouched buffer of whitespace
    /// means "no script", not "a script that does nothing", and the two differ in
    /// whether they draw against the world's budget.
    ///
    /// Scripting somebody else's object is the most complete way to take it over
    /// — a script can move it, rename it, set it alight and delete it — so this is
    /// the same gate as an edit, not a lesser one.
    ///
    /// The host is told *inside* the database call, while the connection is held:
    /// two stores to the same prim then reach the host in the order their rows
    /// were written, so the script that runs is always the one that is stored.
    /// Compile errors come back asynchronously as `ScriptOutput`; the store itself
    /// has succeeded by then.
    pub async fn set_script(
        &self,
        actor: &Actor,
        prim_id: i64,
        source: &str,
    ) -> Result<bool, PrimError> {
        let source = source.trim().to_string();
        if source.len() > crate::scripting::MAX_SOURCE_BYTES {
            return Err(PrimError::Refused(format!(
                "script is {} bytes; the limit is {}",
                source.len(),
                crate::scripting::MAX_SOURCE_BYTES
            )));
        }
        let running = !source.is_empty();
        let _writer = self.writer.lock().await;
        // The flag is set *before* the row is written, as it was when all of this
        // happened under one lock: it claims the budget, and it puts the prim in
        // the next tick's script views, so a script whose `on_start` runs the
        // moment the host compiles it finds its own prim there. A write that fails
        // puts the flag back.
        let was_scripted = {
            let mut w = self.world.write().await;
            scriptable(&w, actor, prim_id)?;
            if running {
                w.can_add_script(prim_id).map_err(PrimError::Refused)?;
            }
            let was = w.is_scripted(prim_id);
            w.set_scripted(prim_id, running);
            was
        };
        let scripts = self.scripts.clone();
        let stored = db::call(&self.conn, move |c| -> anyhow::Result<()> {
            if source.is_empty() {
                db::delete_prim_script(c, prim_id)?;
                scripts.unload(prim_id);
            } else {
                db::set_prim_script(c, prim_id, &source)?;
                scripts.load(prim_id, source);
            }
            Ok(())
        })
        .await;
        if let Some(e) = match stored {
            Ok(Ok(())) => None,
            Ok(Err(e)) | Err(e) => Some(e),
        } {
            let mut w = self.world.write().await;
            if w.has_prim(prim_id) {
                w.set_scripted(prim_id, was_scripted);
            }
            return Err(PrimError::Storage(format!("could not store script: {e:#}")));
        }
        if !self.world.read().await.has_prim(prim_id) {
            // A script deleted its own prim (`die`) while this was being stored.
            // Its row goes at the next flush, and the script row with it; the VM
            // this call just started has to go now.
            self.scripts.unload(prim_id);
            return Err(PrimError::NotFound(prim_id));
        }
        self.broadcast_capacity().await;
        Ok(running)
    }

    /// A prim's stored script source, if it has one.
    pub async fn script_source(&self, prim_id: i64) -> anyhow::Result<Option<String>> {
        db::call(&self.conn, move |c| db::get_prim_script(c, prim_id)).await?
    }

    /// Write back everything scripts have changed or deleted since the last
    /// flush, with the world lock released for the write.
    ///
    /// Best-effort: a failed write leaves the batch dirty for the next flush, and
    /// the in-memory value — which is what clients see — is unaffected.
    ///
    /// **One transaction for the whole batch, not one per prim.** Every statement
    /// outside an explicit transaction is its own implicit one, so a flush of
    /// forty moved prims was forty commits. Wrapping them makes it one commit
    /// regardless of how many scripts moved something. It also sets the failure
    /// granularity in the right direction: a rolled-back batch leaves *every* id
    /// in it dirty, so the retry writes the same set the database rejected rather
    /// than a half-applied one.
    pub async fn flush(&self) {
        let _writer = self.writer.lock().await;
        let batch = self.world.write().await.take_dirty();
        if batch.is_empty() {
            return;
        }
        // Shared rather than moved, so a batch whose blocking task never ran —
        // a poisoned lock, a runtime shutting down — can still be handed back.
        let batch = Arc::new(batch);
        let written = {
            let batch = batch.clone();
            db::call(&self.conn, move |c| write_batch(c, &batch)).await
        };
        let failure = match written {
            Ok(Ok(())) => None,
            Ok(Err(e)) | Err(e) => Some(e),
        };
        if let Some(e) = failure {
            tracing::warn!(
                rows = batch.rows.len(),
                retired = batch.retired.len(),
                "persisting script writes failed; kept for the next flush: {e:#}"
            );
            self.world.write().await.restore_dirty(&batch);
        }
    }

    /// [`Self::flush`] as a task of its own, for the tick loop, which must not
    /// wait on the disk.
    #[must_use]
    pub fn spawn_flush(&self) -> tokio::task::JoinHandle<()> {
        let this = self.clone();
        tokio::spawn(async move { this.flush().await })
    }

    pub(crate) fn broadcast(&self, msg: &NetMessage) {
        match encode_app_frame(msg) {
            Ok(frame) => {
                let _ = self.tx_snap.send(Bytes::from(frame));
            }
            Err(e) => tracing::error!("prim broadcast encode: {e}"),
        }
    }

    async fn broadcast_capacity(&self) {
        crate::net::broadcast_capacity(&self.world, &self.tx_snap).await;
    }
}

/// The prim, if it exists and the actor may change it — existence first.
fn editable(w: &SimWorld, actor: &Actor, prim_id: i64) -> Result<PrimDto, PrimError> {
    let prim = w.prim(prim_id).ok_or(PrimError::NotFound(prim_id))?;
    if !actor.may_edit(w, prim_id) {
        return Err(refusal(w, actor, prim_id, "change prims"));
    }
    Ok(prim.clone())
}

/// The prim, if it exists and the actor may write its script.
fn scriptable(w: &SimWorld, actor: &Actor, prim_id: i64) -> Result<PrimDto, PrimError> {
    let prim = w.prim(prim_id).ok_or(PrimError::NotFound(prim_id))?;
    if !actor.may_script(w, prim_id) {
        return Err(refusal(w, actor, prim_id, "write scripts"));
    }
    Ok(prim.clone())
}

/// Why an actor may not touch a prim: it is somebody else's, or it is theirs
/// and their roles do not allow `what` at all — two different answers, because
/// "it belongs to somebody else" told to its own builder is a lie.
fn refusal(w: &SimWorld, actor: &Actor, prim_id: i64, what: &str) -> PrimError {
    if w.owns(prim_id, actor.principal.as_deref().unwrap_or_default()) {
        PrimError::Forbidden(format!("this account may not {what} on this server"))
    } else {
        PrimError::NotYours(prim_id)
    }
}

/// Insert a prim and give it its initial state, in one transaction.
fn store_new<F>(
    c: &mut Connection,
    seed: PrimSeed,
    finish: F,
    models: ModelCheck,
    owner: Option<&str>,
) -> Result<PrimDto, PrimError>
where
    F: FnOnce(&PrimDto) -> Result<PrimFields, String>,
{
    let storage = |e: anyhow::Error| PrimError::Storage(format!("insert_prim failed: {e:#}"));
    let tx = c
        .transaction()
        .map_err(|e| storage(anyhow::Error::from(e)))?;
    // The insert writes defaults for everything but these three; `finish` and
    // the update below write the rest, which is the one function that does.
    let base =
        db::insert_prim(&tx, seed.position, &seed.shape, seed.geo, None, None).map_err(storage)?;
    let fields = finish(&base).map_err(PrimError::Refused)?;
    if models == ModelCheck::Required {
        check_model(&tx, fields.model.as_deref())?;
    }
    let prim = fields
        .write(&tx, base.id)
        .map_err(|e| PrimError::Storage(format!("update_prim failed: {e:#}")))?
        .ok_or_else(|| {
            PrimError::Storage(format!("prim {} vanished while it was created", base.id))
        })?;
    tx.commit().map_err(|e| storage(anyhow::Error::from(e)))?;
    if let Some(owner) = owner {
        if let Err(e) = db::set_prim_owner(c, prim.id, owner) {
            tracing::warn!(prim_id = prim.id, "recording the owner failed: {e:#}");
        }
    }
    Ok(prim)
}

/// Whether a prim may be given this model, and why not when it may not.
///
/// Checked when the model is *set* rather than when it is drawn, and refused
/// rather than stored (see [`ModelCheck`]). `None` is always allowed: that is a
/// prim wearing its procedural shape, which is every prim that existed before
/// protocol 22.
fn check_model(c: &Connection, model: Option<&str>) -> Result<(), PrimError> {
    let Some(hash) = model else {
        return Ok(());
    };
    match crate::library::model_is_available(c, hash) {
        Ok(true) => Ok(()),
        Ok(false) => Err(PrimError::Refused(
            "this server does not have that model".to_string(),
        )),
        // A database error is not a refusal of the model, and saying "no such
        // model" here would send a builder looking for a fault in their upload.
        Err(e) => {
            tracing::warn!("could not check model {hash}: {e:#}");
            Err(PrimError::Refused(
                "could not check that model just now".to_string(),
            ))
        }
    }
}

/// One flush, in one transaction.
fn write_batch(c: &mut Connection, batch: &DirtyBatch) -> anyhow::Result<()> {
    let tx = c.transaction()?;
    for p in &batch.rows {
        // `None` is a row already gone — a delete that landed first — and there
        // is nothing to write back to it.
        let _ = PrimFields::of(p).write(&tx, p.id)?;
        // Not one of the edit's fields (an edit must not switch physics), but a
        // script's `set_physical` lands here and nowhere else.
        db::set_prim_physical(&tx, p.id, p.physical)?;
    }
    for &id in &batch.retired {
        db::delete_prim(&tx, id)?;
    }
    tx.commit()?;
    Ok(())
}

//! Fetching a prim's model from the simulator, and drawing it.
//!
//! # Why this is an asset *source* and not a download
//!
//! The obvious shape — fetch the bytes, hand them to Bevy — is not available.
//! `AssetServer` loads from a path, and the glTF loader wants a `LoadContext` to
//! pull the model's own dependencies through; there is no public "here are some
//! bytes, make me a scene". The usual workaround, writing the file somewhere and
//! loading that path, fails on the two platforms this most has to work on: on
//! Android the default asset reader reads out of the APK and nothing else, and in
//! a browser there is no filesystem at all.
//!
//! So the fetch is registered *as* a place assets come from. `sim://<hash>.glb`
//! is a path like any other, the glTF loader is unmodified, and one reader serves
//! desktop, Android and the web. What it reads is the cache
//! `platform::storage` already provides, falling back to the server.
//!
//! # The cache never expires, and that is a property of the name
//!
//! A file named by a hash of its own content cannot change. So a hit is always
//! valid, there is no revalidation, and nothing has to decide how stale is too
//! stale. The same fact is why the server serves these `immutable`.
//!
//! # Where the server is has to be told to us
//!
//! The wire arrives over a TCP port or a WebSocket path and the HTTP surface is a
//! separate bind behind a reverse proxy, so the client cannot work out one from
//! the other — `ServerHelloAck::api_base_url` is the server saying. Until a
//! session sets [`set_endpoint`], every read here fails as not-found, which is the
//! honest answer: offline there is no simulator and therefore no model store.

use bevy::asset::io::{AssetReader, AssetReaderError, PathStream, Reader, VecReader};
use bevy::prelude::*;
use std::path::Path;
use std::sync::RwLock;

use crate::components::{Prim, PrimMesh};
use crate::platform::{http, storage};

/// The asset source name. `sim://<hash>.glb` is a model on the connected server.
pub const SOURCE: &str = "sim";

/// Where a model's bytes are kept between runs, under the evictable namespace.
///
/// `Cache` and not `Data`: a model can always be fetched again, and a browser
/// clearing it costs a download rather than somebody's work.
fn cache_key(hash: &str) -> String {
    format!("models/{hash}.glb")
}

/// The asset path for a model hash, as `AssetServer::load` wants it.
#[must_use]
pub fn asset_path(hash: &str) -> String {
    format!("{SOURCE}://{hash}.glb")
}

/// Where this session's models come from.
struct Endpoint {
    /// No trailing slash.
    base: String,
    token: String,
}

/// The connected server, or `None` when there is not one.
///
/// A `static` rather than a Bevy resource because the reader is built by the
/// asset plugin, before the app exists, and is then owned by the asset system —
/// it has no way to reach the world. The lock is taken for a clone of two short
/// strings, once per model fetched.
static ENDPOINT: RwLock<Option<Endpoint>> = RwLock::new(None);

/// Point the model store at a server. Called when a session comes up.
pub fn set_endpoint(base: &str, token: &str) {
    let base = base.trim_end_matches('/').to_string();
    if let Ok(mut slot) = ENDPOINT.write() {
        *slot = Some(Endpoint {
            base,
            token: token.to_string(),
        });
    }
}

/// Forget the server. Called from session teardown, alongside dropping the
/// socket — a stale token pointed at a server nobody is signed in to is a fetch
/// that will 401 forever.
pub fn clear_endpoint() {
    if let Ok(mut slot) = ENDPOINT.write() {
        *slot = None;
    }
}

/// Whether models can be fetched at all right now.
#[must_use]
pub fn have_endpoint() -> bool {
    ENDPOINT.read().is_ok_and(|slot| slot.is_some())
}

/// Work out the API base from the address a client was given, when the server did
/// not say.
///
/// Deliberately conservative, and it answers `None` rather than guessing in the
/// case it cannot know. A development simulator on this machine binds its HTTP
/// listener wherever `--http-listen` said, which is not derivable from the game
/// port — and a wrong base is worse than no base, because it fails as a network
/// error rather than as "this server did not tell me where its API is".
#[must_use]
pub fn derive_api_base(addr: &str) -> Option<String> {
    let addr = addr.trim().trim_end_matches('/');
    // An empty address means *this page's own origin* — the web build's default,
    // and what `network::ws_url` reads it as. The base is then the empty string,
    // so every request below is a relative URL the browser resolves against the
    // page. Falling through to the bare-hostname arm would build `https://` and
    // fetch nothing, with a network error for an explanation.
    if addr.is_empty() {
        return Some(String::new());
    }
    if let Some(rest) = addr.strip_prefix("wss://") {
        let host = rest.split('/').next().unwrap_or(rest);
        return Some(format!("https://{host}"));
    }
    if let Some(rest) = addr.strip_prefix("ws://") {
        let host = rest.split('/').next().unwrap_or(rest);
        return Some(format!("http://{host}"));
    }
    if addr.starts_with("http://") || addr.starts_with("https://") {
        return Some(addr.to_string());
    }
    if addr.starts_with("tcp://") || crate::systems::network::is_loopback_host(host_of(addr)) {
        // A dev simulator. Its HTTP port is a separate `--http-listen` and there
        // is nothing here that knows it.
        return None;
    }
    // A bare hostname, which `resolve_target` already reads as "the secure one".
    Some(format!("https://{}", host_of(addr)))
}

/// The host part of an address that may carry a port.
fn host_of(addr: &str) -> &str {
    match addr.rsplit_once(':') {
        Some((h, _)) => h.trim_start_matches('[').trim_end_matches(']'),
        None => addr,
    }
}

// ---------------------------------------------------------------------------
// Talking to the model API
// ---------------------------------------------------------------------------

/// Build a request to the connected server's HTTP surface, already carrying the
/// session token.
///
/// Every caller goes through here, so there is one place that knows the base and
/// the credential — the same reason the asset reader reads them from the same
/// `static`. `None` means there is no session, which is a refusal rather than a
/// failure: offline there is no server to ask.
fn api_request(make: fn(String) -> http::Request, path: &str) -> Option<http::Request> {
    let slot = ENDPOINT.read().ok()?;
    let endpoint = slot.as_ref()?;
    Some(
        make(format!("{}{path}", endpoint.base))
            .header("Authorization", format!("Bearer {}", endpoint.token)),
    )
}

/// `GET` a JSON document from the model API.
pub async fn api_get(path: &str) -> Result<String, String> {
    let request = api_request(http::Request::get, path).ok_or("not signed in")?;
    let bytes = http::fetch(request).await?;
    String::from_utf8(bytes).map_err(|e| e.to_string())
}

/// `POST` a JSON document (or nothing) to the model API.
pub async fn api_post(path: &str, body: Option<String>) -> Result<String, String> {
    let mut request = api_request(http::Request::post, path).ok_or("not signed in")?;
    if let Some(body) = body {
        request = request
            .header("Content-Type", "application/json")
            .body(body.into_bytes());
    }
    let bytes = http::fetch(request).await?;
    String::from_utf8(bytes).map_err(|e| e.to_string())
}

/// `DELETE` a path on the model API.
pub async fn api_delete(path: &str) -> Result<(), String> {
    let request = api_request(http::Request::delete, path).ok_or("not signed in")?;
    http::fetch(request).await.map(|_| ())
}

/// Upload a clip to the video store (`/api/videos`). Answers the server's
/// measurements as JSON, the hash among them.
#[cfg(not(target_arch = "wasm32"))]
pub async fn api_upload_video(bytes: Vec<u8>) -> Result<String, String> {
    let request = api_request(http::Request::post, "/api/videos")
        .ok_or("not signed in")?
        .header("Content-Type", "video/webm")
        // Up to 64 MB, possibly over a phone's uplink.
        .timeout(std::time::Duration::from_secs(300))
        .body(bytes);
    let bytes = http::fetch(request).await?;
    String::from_utf8(bytes).map_err(|e| e.to_string())
}

/// A clip's bytes: from the cache if this device has fetched it before (the
/// name is its hash, so a cached copy is never stale), else from the server.
#[cfg(not(target_arch = "wasm32"))]
pub async fn fetch_video(hash: &str) -> Result<Vec<u8>, String> {
    use crate::platform::storage::{self, Namespace};
    let key = format!("videos/{hash}.webm");
    if let Some(bytes) = storage::get(Namespace::Cache, &key).await {
        return Ok(bytes);
    }
    let request = api_request(http::Request::get, &format!("/api/videos/{hash}.webm"))
        .ok_or("not signed in")?
        .timeout(std::time::Duration::from_secs(300));
    let bytes = http::fetch(request).await?;
    storage::put(Namespace::Cache, &key, &bytes).await;
    Ok(bytes)
}

/// Upload a model. The file is the body; everything about it is the query string.
///
/// A raw body rather than a multipart form for the reason the route's own note
/// gives: there is exactly one file and nothing else that has to travel beside
/// it, so the size limit stays in one place instead of spread over a parser's
/// parts.
pub async fn api_upload(query: &str, bytes: Vec<u8>) -> Result<String, String> {
    let request = api_request(http::Request::post, &format!("/api/assets?{query}"))
        .ok_or("not signed in")?
        .header("Content-Type", "model/gltf-binary")
        // A model is megabytes over a link a phone may be holding up; the default
        // is tuned for tiles.
        .timeout(std::time::Duration::from_secs(120))
        .body(bytes);
    let bytes = http::fetch(request).await?;
    String::from_utf8(bytes).map_err(|e| e.to_string())
}

/// Reads `sim://` paths: the local cache first, then the server.
pub struct SimAssetReader;

impl SimAssetReader {
    async fn fetch(path: &Path) -> Result<Vec<u8>, AssetReaderError> {
        let name = path.to_string_lossy().to_string();
        let missing = || AssetReaderError::NotFound(path.to_path_buf());

        if let Some(bytes) = storage::get(storage::Namespace::Cache, &cache_key_for(&name)).await {
            return Ok(bytes);
        }

        let (base, token) = {
            let Ok(slot) = ENDPOINT.read() else {
                return Err(missing());
            };
            let Some(endpoint) = slot.as_ref() else {
                // Offline, or signed out. Not a warning: every scripted capture
                // and every offline launch takes this path.
                return Err(missing());
            };
            (endpoint.base.clone(), endpoint.token.clone())
        };

        let url = format!("{base}/api/assets/{name}");
        let request = http::Request::get(url).header("Authorization", format!("Bearer {token}"));
        match http::fetch(request).await {
            Ok(bytes) => {
                // Cached before it is parsed, so a model that turns out to be
                // unloadable is still only fetched once. It is what the server
                // sent either way, and the server is what decided it was a model.
                storage::put(storage::Namespace::Cache, &cache_key_for(&name), &bytes).await;
                Ok(bytes)
            }
            Err(e) => {
                warn!("could not fetch model {name}: {e}");
                Err(missing())
            }
        }
    }
}

/// The cache key for an asset path (`<hash>.glb`).
fn cache_key_for(name: &str) -> String {
    cache_key(name.trim_end_matches(".glb"))
}

impl AssetReader for SimAssetReader {
    async fn read<'a>(&'a self, path: &'a Path) -> Result<impl Reader + 'a, AssetReaderError> {
        Ok(VecReader::new(Self::fetch(path).await?))
    }

    /// There are no `.meta` files here and there cannot be: the store holds
    /// exactly the bytes somebody uploaded. Answering not-found is how a reader
    /// says "use the loader's defaults", which is what every one of these wants.
    async fn read_meta<'a>(&'a self, path: &'a Path) -> Result<impl Reader + 'a, AssetReaderError> {
        Err::<VecReader, _>(AssetReaderError::NotFound(path.to_path_buf()))
    }

    async fn read_directory<'a>(
        &'a self,
        path: &'a Path,
    ) -> Result<Box<PathStream>, AssetReaderError> {
        Err(AssetReaderError::NotFound(path.to_path_buf()))
    }

    async fn is_directory<'a>(&'a self, _path: &'a Path) -> Result<bool, AssetReaderError> {
        Ok(false)
    }
}

// ---------------------------------------------------------------------------
// Drawing one
// ---------------------------------------------------------------------------

/// Marks a prim entity whose model scene has been asked for, and which hash it
/// was — so a prim that is given a different model swaps rather than gaining a
/// second body.
#[derive(Component, Debug, Clone)]
pub struct ModelScene {
    pub hash: String,
    /// The child holding the glTF scene. A child and not the prim itself, because
    /// a `SceneRoot` despawns and rebuilds its own descendants, and the prim
    /// entity carries the components — `Prim`, `PrimGeo`, the particle emitter —
    /// that must survive a model swap.
    pub holder: Entity,
}

/// Give every model-wearing prim its scene, take it away when the model is
/// cleared, and swap it when the model changes.
///
/// `Changed<Prim>` covers a prim that has just been spawned as well as one that
/// has been edited — the component is *inserted* in the first case, which counts
/// as a change — so this is one query rather than two that would both fire on the
/// same frame for a new prim.
///
/// A model is an ordinary editable field, so all three directions have to work:
/// taking one off has to bring the procedural shape back, and putting one on has
/// to take the procedural mesh away.
pub fn attach_prim_models(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    changed: Query<(Entity, &Prim, Option<&ModelScene>), Changed<Prim>>,
) {
    for (entity, prim, existing) in changed.iter() {
        let wanted = prim.model.as_deref();
        let current = existing.map(|m| m.hash.as_str());
        if wanted == current {
            continue;
        }
        if let Some(old) = existing {
            // `try_despawn`: the holder hangs off an entity another system may have
            // retired between this frame's query and the command flush
            // (`docs/notes/entity-commands-outlive-entities.md`).
            commands.entity(old.holder).try_despawn();
            commands.entity(entity).try_remove::<ModelScene>();
        }
        match wanted {
            Some(hash) => {
                // The procedural mesh goes, and `PrimMesh` stays: everything that
                // treats a prim as pickable and editable queries on that marker,
                // and a model prim is all of those things.
                commands
                    .entity(entity)
                    .try_remove::<Mesh3d>()
                    .try_remove::<MeshMaterial3d<StandardMaterial>>();
                spawn_model(&mut commands, &asset_server, entity, hash);
            }
            None => {
                // Give the prim back to `rendering::spawn_prims`, which builds the
                // mesh *and* the material together. Reaching for `NeedsMeshRebuild`
                // instead would set a mesh on a prim that has no material, because
                // it never had one.
                commands.entity(entity).try_remove::<PrimMesh>();
            }
        }
    }
}

fn spawn_model(commands: &mut Commands, asset_server: &AssetServer, entity: Entity, hash: &str) {
    let scene =
        asset_server.load(bevy::gltf::GltfAssetLabel::Scene(0).from_asset(asset_path(hash)));
    // `WorldAssetRoot`, not Bevy's `SceneRoot`: this tree loads glTF scenes
    // through `bevy_world_serialization`, which is what the avatars already do
    // (`avatar::spawn_holder`) and what the `reflect_auto_register` feature is
    // enabled for.
    let holder = commands
        .spawn((
            bevy::world_serialization::WorldAssetRoot(scene),
            Transform::IDENTITY,
            Visibility::default(),
            ModelHolder,
        ))
        .id();
    commands.entity(entity).add_child(holder);
    commands.entity(entity).try_insert(ModelScene {
        hash: hash.to_string(),
        holder,
    });
}

/// The child entity a model's scene hangs from.
#[derive(Component, Debug, Clone, Copy)]
pub struct ModelHolder;

#[cfg(test)]
mod tests {
    use super::*;

    /// The name is the whole of the address, and it has to survive the round trip
    /// through an asset path and back into a cache key.
    #[test]
    fn a_hash_round_trips_through_the_asset_path() {
        let hash = "a".repeat(64);
        let path = asset_path(&hash);
        assert_eq!(path, format!("sim://{hash}.glb"));
        // What the reader is handed is the part after the scheme.
        assert_eq!(cache_key_for(&format!("{hash}.glb")), cache_key(&hash));
    }

    /// A bare hostname is the secure one, matching `network::resolve_target`'s
    /// rule that the easy thing to type is the encrypted one.
    #[test]
    fn a_bare_host_derives_an_https_base() {
        assert_eq!(
            derive_api_base("cartopolis.org").as_deref(),
            Some("https://cartopolis.org")
        );
        assert_eq!(
            derive_api_base("wss://cartopolis.org/ws").as_deref(),
            Some("https://cartopolis.org")
        );
    }

    /// A development simulator's HTTP port is whatever `--http-listen` said, and
    /// nothing here knows it. Answering `None` makes the client say so; guessing
    /// would make it fail as a connection error against a port nobody is on.
    #[test]
    fn a_loopback_address_derives_nothing() {
        assert_eq!(derive_api_base("127.0.0.1:4747"), None);
        assert_eq!(derive_api_base("localhost:4747"), None);
    }

    /// The web build's default server field is empty, meaning this page's own
    /// origin. The base has to be empty with it, so the requests are relative —
    /// the alternative builds the string `https://` and fetches nothing.
    #[test]
    fn an_empty_address_is_this_page() {
        assert_eq!(derive_api_base("").as_deref(), Some(""));
        assert_eq!(derive_api_base("   ").as_deref(), Some(""));
    }
}

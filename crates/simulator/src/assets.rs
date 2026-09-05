//! The model asset store: files named by their own bytes, and the rules about
//! what may become one.
//!
//! # Why the file is named after its content
//!
//! Two people upload the same bench and the server keeps one copy. That is the
//! obvious half. The half that matters more is that the name can never point at
//! different bytes than it did yesterday, so `/api/assets/{hash}.glb` is
//! immutable and everything downstream — the client's disk cache, the browser,
//! any proxy in between — can hold it forever without a revalidation. A model
//! that could be edited in place would need an expiry, and a stale model is a
//! building that changed shape while somebody was standing in it.
//!
//! The consequence to keep in mind is elsewhere, in `V16__model_assets.sql`: with
//! one file behind two uploaders, ownership and naming cannot live on the file.
//!
//! # Why the checks happen here and not on the client
//!
//! An upload is checked once, by the server, for everyone who will ever load it.
//! Doing it on the client instead means every viewer pays the check, a client
//! that skips it cannot be detected, and the first anyone hears about a model
//! nothing can draw is a phone dying in the street.
//!
//! What that costs is that the checks must be *decidable from the file*, which is
//! why they are counts and dimensions rather than judgements. Whether a model
//! looks good is a curator's question, in cartopy.
//!
//! # The refusals are on parsed facts, never on an estimate
//!
//! [`docs/notes/mvt-capacity-guard.md`] is the standing warning here: a guard
//! that predicted a cost from the shape of a file before parsing it refused real
//! content and was reverted the same day. So this module parses the glTF once and
//! counts what is actually in it. The only pre-parse limit is a byte ceiling,
//! which is not an estimate of anything — it is the size of the read.

use blake2::{Blake2s256, Digest};
use std::fs;
use std::path::{Path, PathBuf};

/// The largest upload accepted, in bytes.
///
/// This is the *read* bound and the first thing applied, before any parsing: a
/// body larger than this is refused without being held whole in memory. Sixteen
/// megabytes is far above any sensible prop — the twenty avatar characters this
/// project ships average about 3 MB each as glTF, and those are rigged, animated
/// characters — and far below anything that threatens the process.
pub const MAX_ASSET_BYTES: usize = 16 * 1024 * 1024;

/// The largest triangle count accepted, summed over every mesh in the file.
///
/// Counted per distinct mesh rather than per node that instances one: what the
/// budget is protecting is GPU memory, and an instanced mesh is uploaded once.
/// Node count is bounded separately, which is the draw-call side of the same
/// question.
pub const MAX_TRIANGLES: u64 = 250_000;

/// The largest node count accepted.
///
/// A model is a prop, not a scene. Thousands of nodes is a level someone has
/// exported whole, and every one of them is a transform to propagate each frame
/// on a phone.
pub const MAX_NODES: usize = 4_000;

/// The largest texture edge accepted, in pixels.
///
/// Read from each image's own header rather than inferred from its compressed
/// size, because the compressed size of a 16384-square image can be tiny — that
/// is what a decompression bomb is. Two thousand and forty-eight is generous for
/// something usually seen from across a street.
pub const MAX_TEXTURE_EDGE: u32 = 2048;

/// What an accepted upload turned out to be. Stored on the row, because these are
/// the numbers the refusal was decided on and re-deriving them means re-reading
/// every file to list the library.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AssetFacts {
    pub hash: String,
    pub byte_size: u64,
    pub triangles: u64,
    pub nodes: u64,
    pub textures: u64,
    /// The longest edge of the largest image in the file, in pixels; 0 when the
    /// model carries no textures at all.
    pub largest_texture: u32,
}

/// The name a file is stored under: 32 bytes of BLAKE2s over its content, hex.
///
/// BLAKE2s because `auth::token_hash` already uses it, so this costs the
/// dependency graph nothing. The choice is not cryptographically load-bearing in
/// the way a signature would be — nothing here trusts a hash somebody supplied,
/// every route recomputes it from the bytes it received — but a collision would
/// silently serve one person's model in place of another's, so a real hash it is.
#[must_use]
pub fn content_hash(bytes: &[u8]) -> String {
    let mut hasher = Blake2s256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// Whether a string could be a name this module produced.
///
/// Every route that takes a hash from a URL runs this first. It is the path
/// traversal guard as much as a format check: a hash is 64 lowercase hex digits
/// and therefore contains no separator, no dot and no `..`, so a value that
/// passes can be joined to a directory without further thought.
#[must_use]
pub fn is_hash(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Where a hash lives on disk.
///
/// Flat, not sharded into `ab/cdef…` subdirectories. That fan-out exists to keep
/// a directory small enough for a filesystem to list quickly, and the number of
/// models in one world is thousands at the very most — ext4 has indexed
/// directories and does not care. It can be added later without touching a single
/// stored name, since the name is not the path.
fn asset_path(dir: &str, hash: &str) -> PathBuf {
    Path::new(dir).join(format!("{hash}.glb"))
}

/// Read the model back, or `None` if this server has never held it.
///
/// The hash is checked rather than sanitised. Filtering the odd characters out of
/// an id and carrying on — which is what `textures::load_texture` does — is right
/// for a name an operator typed into a directory and wrong for one that arrives
/// in a URL, where a value that needed cleaning is a value somebody chose.
#[must_use]
pub fn load(dir: &str, hash: &str) -> Option<Vec<u8>> {
    if !is_hash(hash) {
        return None;
    }
    fs::read(asset_path(dir, hash)).ok()
}

/// Write the model, creating the store directory if this is the first one.
///
/// Writing to a temporary file and renaming, rather than writing in place: a
/// download route running concurrently with an upload of the same hash must see
/// either nothing or the whole file, never a prefix. `rename` within one
/// directory is atomic on every filesystem this runs on.
///
/// A file that is already there is left alone. It cannot differ — the name is a
/// hash of the content — so rewriting it would be pure risk.
pub fn store(dir: &str, hash: &str, bytes: &[u8]) -> std::io::Result<()> {
    let final_path = asset_path(dir, hash);
    if final_path.exists() {
        return Ok(());
    }
    fs::create_dir_all(dir)?;
    let temp_path = Path::new(dir).join(format!(".{hash}.part"));
    fs::write(&temp_path, bytes)?;
    fs::rename(&temp_path, &final_path)
}

/// Parse an uploaded file and answer what it is, or why it will not do.
///
/// The error strings are what a person sees in the upload dialog, so they say
/// what was measured and what the ceiling is. "Rejected" with no number is a
/// message that sends someone to a maintainer.
pub fn inspect(bytes: &[u8]) -> Result<AssetFacts, String> {
    if bytes.len() > MAX_ASSET_BYTES {
        return Err(format!(
            "that file is {:.1} MB and the limit is {} MB",
            bytes.len() as f64 / (1024.0 * 1024.0),
            MAX_ASSET_BYTES / (1024 * 1024)
        ));
    }
    if bytes.is_empty() {
        return Err("that file is empty".to_string());
    }
    // Binary glTF only, and the magic is the test. A `.gltf` JSON file names its
    // buffers and images as separate files, so accepting one would mean either
    // fetching whatever it names — an upload that makes the server issue
    // requests of the uploader's choosing — or storing a model that cannot be
    // drawn. `.glb` carries everything in one file, which is the property the
    // whole store is built on.
    if !bytes.starts_with(b"glTF") {
        return Err("that is not a .glb file — export as binary glTF".to_string());
    }

    let gltf =
        gltf::Gltf::from_slice(bytes).map_err(|e| format!("that .glb will not parse: {e}"))?;

    // Every buffer must be the GLB's own binary chunk. A `Uri` here is either an
    // external file (which did not come with the upload) or a data URI (which is
    // base64 in the JSON, a third larger than it needs to be, and a way to smuggle
    // past a byte limit measured on the file). Both are refusals.
    for buffer in gltf.buffers() {
        if let gltf::buffer::Source::Uri(uri) = buffer.source() {
            let shown: String = uri.chars().take(40).collect();
            return Err(format!(
                "that model refers to a file outside itself ({shown}) — export it with the buffers embedded"
            ));
        }
    }

    let mut triangles: u64 = 0;
    for mesh in gltf.meshes() {
        for primitive in mesh.primitives() {
            // A count of *vertices*, turned into triangles by the topology. A
            // strip or a fan of n vertices is n-2 triangles, not n/3, and reading
            // every mode as a list would over-count a strip threefold and let a
            // fan through a limit it should have hit.
            let vertices = match primitive.indices() {
                Some(accessor) => accessor.count() as u64,
                None => primitive
                    .get(&gltf::Semantic::Positions)
                    .map_or(0, |a| a.count() as u64),
            };
            triangles += match primitive.mode() {
                gltf::mesh::Mode::Triangles => vertices / 3,
                gltf::mesh::Mode::TriangleStrip | gltf::mesh::Mode::TriangleFan => {
                    vertices.saturating_sub(2)
                }
                // Points and lines draw no triangles. They are not refused: a
                // model may legitimately carry a line for an edge highlight.
                _ => 0,
            };
        }
    }
    if triangles > MAX_TRIANGLES {
        return Err(format!(
            "that model has {triangles} triangles and the limit is {MAX_TRIANGLES}"
        ));
    }

    let nodes = gltf.nodes().count();
    if nodes > MAX_NODES {
        return Err(format!(
            "that model has {nodes} nodes and the limit is {MAX_NODES}"
        ));
    }

    let blob = gltf.blob.as_deref().unwrap_or_default();
    let mut textures: u64 = 0;
    let mut largest_texture: u32 = 0;
    for image in gltf.images() {
        let gltf::image::Source::View { view, .. } = image.source() else {
            return Err(
                "that model refers to a texture file outside itself — export it with the textures embedded"
                    .to_string(),
            );
        };
        textures += 1;
        let start = view.offset();
        let end = start.saturating_add(view.length());
        let Some(encoded) = blob.get(start..end) else {
            return Err("that model's texture data is truncated".to_string());
        };
        // Header only. `into_dimensions` reads far enough to answer and stops,
        // which is the point: decoding a 16384-square image to find out it is
        // 16384 square is the attack, not the check.
        let edge = image_edge(encoded)?;
        largest_texture = largest_texture.max(edge);
    }
    if largest_texture > MAX_TEXTURE_EDGE {
        return Err(format!(
            "that model has a {largest_texture}-pixel texture and the limit is {MAX_TEXTURE_EDGE}"
        ));
    }

    Ok(AssetFacts {
        hash: content_hash(bytes),
        byte_size: bytes.len() as u64,
        triangles,
        nodes: nodes as u64,
        textures,
        largest_texture,
    })
}

/// The longer edge of an encoded image, read from its header.
fn image_edge(encoded: &[u8]) -> Result<u32, String> {
    let reader = image::ImageReader::new(std::io::Cursor::new(encoded))
        .with_guessed_format()
        .map_err(|e| format!("that model has a texture that will not read: {e}"))?;
    let (w, h) = reader
        .into_dimensions()
        .map_err(|e| format!("that model has a texture that will not read: {e}"))?;
    Ok(w.max(h))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal binary glTF: one triangle, no textures, no external references.
    ///
    /// Built here rather than checked in, for the reason `tools/ifc_demo_building.py`
    /// exists — a fixture that describes exactly what the test is about beats a
    /// binary nobody can read the diff of, and this one is small enough to write.
    fn glb(json: &str, bin: &[u8]) -> Vec<u8> {
        let mut json_chunk = json.as_bytes().to_vec();
        while json_chunk.len() % 4 != 0 {
            json_chunk.push(b' ');
        }
        let mut bin_chunk = bin.to_vec();
        while bin_chunk.len() % 4 != 0 {
            bin_chunk.push(0);
        }
        let total = 12
            + 8
            + json_chunk.len()
            + if bin_chunk.is_empty() {
                0
            } else {
                8 + bin_chunk.len()
            };
        let mut out = Vec::with_capacity(total);
        out.extend_from_slice(b"glTF");
        out.extend_from_slice(&2u32.to_le_bytes());
        out.extend_from_slice(&(total as u32).to_le_bytes());
        out.extend_from_slice(&(json_chunk.len() as u32).to_le_bytes());
        out.extend_from_slice(b"JSON");
        out.extend_from_slice(&json_chunk);
        if !bin_chunk.is_empty() {
            out.extend_from_slice(&(bin_chunk.len() as u32).to_le_bytes());
            out.extend_from_slice(b"BIN\0");
            out.extend_from_slice(&bin_chunk);
        }
        out
    }

    /// Three positions and nothing else — the smallest thing that is a model.
    fn one_triangle() -> Vec<u8> {
        let positions: [f32; 9] = [0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0];
        let mut bin = Vec::new();
        for v in positions {
            bin.extend_from_slice(&v.to_le_bytes());
        }
        let json = r#"{
            "asset": {"version": "2.0"},
            "scene": 0,
            "scenes": [{"nodes": [0]}],
            "nodes": [{"mesh": 0}],
            "meshes": [{"primitives": [{"attributes": {"POSITION": 0}}]}],
            "accessors": [{"bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3",
                           "min": [0,0,0], "max": [1,1,0]}],
            "bufferViews": [{"buffer": 0, "byteOffset": 0, "byteLength": 36}],
            "buffers": [{"byteLength": 36}]
        }"#;
        glb(json, &bin)
    }

    #[test]
    fn accepts_a_minimal_binary_gltf() {
        let facts = inspect(&one_triangle()).expect("a one-triangle glb is a model");
        assert_eq!(facts.triangles, 1);
        assert_eq!(facts.nodes, 1);
        assert_eq!(facts.textures, 0);
        assert_eq!(facts.largest_texture, 0);
        assert_eq!(facts.hash.len(), 64);
    }

    /// The name is the content, so two uploads of the same bytes are one file —
    /// and a byte's difference is a different file.
    #[test]
    fn the_hash_is_the_content() {
        let a = one_triangle();
        assert_eq!(content_hash(&a), content_hash(&a.clone()));
        let mut b = a.clone();
        *b.last_mut().unwrap() ^= 0x01;
        assert_ne!(content_hash(&a), content_hash(&b));
    }

    /// A JSON `.gltf` names its buffers as separate files. Accepting one would
    /// mean either fetching what it names or storing something undrawable.
    #[test]
    fn refuses_json_gltf() {
        let err = inspect(br#"{"asset":{"version":"2.0"}}"#).unwrap_err();
        assert!(err.contains(".glb"), "{err}");
    }

    /// The same refusal from the other direction: a GLB whose buffer points
    /// outside itself.
    #[test]
    fn refuses_an_external_buffer() {
        let json = r#"{
            "asset": {"version": "2.0"},
            "buffers": [{"uri": "geometry.bin", "byteLength": 36}]
        }"#;
        let err = inspect(&glb(json, &[])).unwrap_err();
        assert!(err.contains("outside itself"), "{err}");
    }

    #[test]
    fn refuses_an_external_texture() {
        let json = r#"{
            "asset": {"version": "2.0"},
            "images": [{"uri": "colour.png"}]
        }"#;
        let err = inspect(&glb(json, &[])).unwrap_err();
        assert!(err.contains("outside itself"), "{err}");
    }

    #[test]
    fn refuses_an_oversized_file() {
        let err = inspect(&vec![b'x'; MAX_ASSET_BYTES + 1]).unwrap_err();
        assert!(err.contains("limit is"), "{err}");
    }

    /// A hash from a URL is joined to a directory, so anything that is not one
    /// has to be refused before it gets there.
    #[test]
    fn a_hash_cannot_be_a_path() {
        assert!(is_hash(&"a".repeat(64)));
        assert!(
            !is_hash(&"A".repeat(64)),
            "uppercase is not the output format"
        );
        assert!(!is_hash("../../etc/passwd"));
        assert!(!is_hash(&format!("{}/x", "a".repeat(62))));
        assert!(!is_hash(""));
        assert!(load("/tmp", "../../etc/passwd").is_none());
    }

    /// A strip of n vertices is n-2 triangles. Reading every mode as a list
    /// over-counts a strip threefold, which is a limit that refuses honest models.
    #[test]
    fn counts_a_triangle_strip_as_a_strip() {
        let positions: [f32; 15] = [
            0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 1.0, 1.0, 0.0, 2.0, 0.0, 0.0,
        ];
        let mut bin = Vec::new();
        for v in positions {
            bin.extend_from_slice(&v.to_le_bytes());
        }
        let json = r#"{
            "asset": {"version": "2.0"},
            "nodes": [{"mesh": 0}],
            "meshes": [{"primitives": [{"attributes": {"POSITION": 0}, "mode": 5}]}],
            "accessors": [{"bufferView": 0, "componentType": 5126, "count": 5, "type": "VEC3",
                           "min": [0,0,0], "max": [2,1,0]}],
            "bufferViews": [{"buffer": 0, "byteOffset": 0, "byteLength": 60}],
            "buffers": [{"byteLength": 60}]
        }"#;
        let facts = inspect(&glb(json, &bin)).expect("a strip is a model");
        assert_eq!(
            facts.triangles, 3,
            "five vertices as a strip are three triangles"
        );
    }

    /// Storing is atomic and idempotent: the second write of a name that is
    /// already there is a no-op, because it cannot differ.
    #[test]
    fn stores_and_reads_back() {
        let dir = std::env::temp_dir().join(format!("carto-assets-{}", std::process::id()));
        let dir = dir.to_str().unwrap();
        let bytes = one_triangle();
        let hash = content_hash(&bytes);
        store(dir, &hash, &bytes).unwrap();
        store(dir, &hash, &bytes).unwrap();
        assert_eq!(load(dir, &hash).as_deref(), Some(&bytes[..]));
        assert!(load(dir, &content_hash(b"nothing")).is_none());
        let _ = fs::remove_dir_all(dir);
    }
}

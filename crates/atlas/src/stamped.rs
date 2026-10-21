//! Publishing a tree that changes while clients run: a cell file per cell and a
//! `coverage.json` naming each covered cell **with a stamp**.
//!
//! Every other tree atlas serves is rewritten by an extraction run, and a client
//! that sees an old cell is merely a day behind. Two are edited by people and
//! looked at a minute later — building designs (`crate::designs`) and building
//! overrides (`crate::overrides`) — so their clients poll the manifest and put
//! a cell's stamp in its cache key: a cached cell is good for ever under its
//! stamp, and an edit is a new stamp, a new key and one fetch.
//!
//! **A stamp is written when the cell is, never derived from what is in it.**
//! Derived, it could go *down*: remove the newest of two entries and the cell is
//! back at the older one's value, under which a client may already have cached a
//! file that still has the removed entry in it. A stamp only ever moves forward.
//! A cell with nothing left in it leaves the manifest, which is how a client
//! learns it is gone.

use std::collections::BTreeMap;
use std::path::Path;

use cartopolis_geo::formats::coverage;

/// Write a file by rename, never in place, so a client fetching while somebody
/// saves gets the old file or the new one and never half of each.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension(format!("tmp{}", std::process::id() ^ (bytes.len() as u32)));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

/// Files smaller than this are served as they are: Caddy's `encode` does not
/// bother below 512 bytes either, and a sidecar would cost more than it saves.
const BROTLI_MIN_BYTES: usize = 1024;

/// Where a served file's Brotli copy goes: `x.json` → `x.json.br`, which Caddy's
/// `file_server { precompressed br … }` sends to a client that accepts `br`.
#[must_use]
pub fn brotli_sidecar(path: &Path) -> std::path::PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".br");
    name.into()
}

/// The `.br` body for a served file, or `None` when it is too small or does not
/// shrink.
///
/// Quality 10, not 11: on a 1.9 MB BGT cell (2026-10-21) 11 took 4.7 s for
/// 347 KB, 10 took 1.9 s for 364 KB and 9 gave 491 KB, barely under gzip's
/// 536 KB. Done once per write and read by every client, so it is worth the
/// time; Caddy cannot do it on the fly at a level that wins anything.
#[must_use]
pub fn brotli(body: &[u8]) -> Option<Vec<u8>> {
    if body.len() < BROTLI_MIN_BYTES {
        return None;
    }
    let params = brotli::enc::BrotliEncoderParams {
        quality: 10,
        lgwin: 22,
        size_hint: body.len(),
        ..Default::default()
    };
    let mut out = Vec::with_capacity(body.len() / 4);
    brotli::BrotliCompress(&mut &body[..], &mut out, &params).ok()?;
    (out.len() < body.len()).then_some(out)
}

/// [`write_atomic`] for a file Caddy serves: its Brotli copy first, then the
/// file, so a client that finds the new file also finds its new copy.
///
/// A file too small for a copy removes any old one, which would otherwise be
/// served in its place for ever — `precompressed` checks only that the sidecar
/// exists, never that it is as new as the file.
pub fn write_served(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let sidecar = brotli_sidecar(path);
    match brotli(bytes) {
        Some(compressed) => write_atomic(&sidecar, &compressed)?,
        None => remove_if_present(&sidecar)?,
    }
    write_atomic(path, bytes)
}

/// Remove a served file and its Brotli copy, the copy first.
pub fn remove_served(path: &Path) -> std::io::Result<()> {
    remove_if_present(&brotli_sidecar(path))?;
    remove_if_present(path)
}

fn remove_if_present(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Milliseconds since the epoch.
#[must_use]
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// The cells a manifest lists, with their stamps. `[x, y]` entries (from a tree
/// written before stamps) read as stamp 0.
#[must_use]
pub fn read_cells(manifest: &Path) -> BTreeMap<(i64, i64), u64> {
    std::fs::read(manifest)
        .ok()
        .and_then(|b| coverage::parse_slice(&b).ok())
        .map(|m| m.cells)
        .unwrap_or_default()
        .into_iter()
        .map(|c| (c.xy(), c.stamp.unwrap_or(0)))
        .collect()
}

/// Write one cell (or remove it, for `None`) and restamp it in the manifest.
///
/// `newest` is a floor for the new stamp — a design's own revision, so a cell
/// never reads older than what is in it. `extra` is every other field the
/// manifest carries (`zoom` and `cells` are this function's); it replaces what
/// was there, so pass all of it.
pub fn publish_cell(
    root: &Path,
    zoom: u32,
    cell: (i64, i64),
    body: Option<&[u8]>,
    newest: u64,
    extra: serde_json::Map<String, serde_json::Value>,
) -> std::io::Result<()> {
    let path = root
        .join(zoom.to_string())
        .join(cell.0.to_string())
        .join(format!("{}.json", cell.1));
    match body {
        Some(b) => write_served(&path, b)?,
        None => remove_served(&path)?,
    }
    let manifest_path = root.join("coverage.json");
    let mut cells = read_cells(&manifest_path);
    if body.is_some() {
        let was = cells.get(&cell).copied().unwrap_or(0);
        cells.insert(cell, now_ms().max(was + 1).max(newest));
    } else {
        cells.remove(&cell);
    }
    write_manifest(&manifest_path, zoom, &cells, extra)
}

/// Rewrite the manifest's other fields without touching any cell.
pub fn write_manifest(
    manifest_path: &Path,
    zoom: u32,
    cells: &BTreeMap<(i64, i64), u64>,
    extra: serde_json::Map<String, serde_json::Value>,
) -> std::io::Result<()> {
    let mut extra = extra;
    // This function's own keys win over anything the caller passed under them.
    extra.remove("zoom");
    extra.remove("cells");
    let manifest = coverage::Manifest {
        zoom: Some(zoom),
        cells: cells
            .iter()
            .map(|(&(x, y), &r)| coverage::CellRef::stamped(x, y, r))
            .collect(),
        extra,
        ..coverage::Manifest::default()
    };
    write_served(manifest_path, &coverage::to_vec(&manifest)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stamp_only_moves_forward_and_an_empty_cell_leaves() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        publish_cell(root, 14, (1, 2), Some(b"{}"), 0, Default::default()).unwrap();
        let first = read_cells(&root.join("coverage.json"))[&(1, 2)];
        publish_cell(root, 14, (1, 2), Some(b"{}"), 0, Default::default()).unwrap();
        let second = read_cells(&root.join("coverage.json"))[&(1, 2)];
        assert!(second > first);
        // A floor from the content wins over the clock.
        publish_cell(
            root,
            14,
            (1, 2),
            Some(b"{}"),
            u64::MAX / 2,
            Default::default(),
        )
        .unwrap();
        assert_eq!(
            read_cells(&root.join("coverage.json"))[&(1, 2)],
            u64::MAX / 2
        );
        publish_cell(root, 14, (1, 2), None, 0, Default::default()).unwrap();
        assert!(read_cells(&root.join("coverage.json")).is_empty());
        assert!(!root.join("14/1/2.json").exists());
    }

    #[test]
    fn an_old_manifest_reads_as_stamp_zero() {
        let dir = tempfile::tempdir().unwrap();
        let m = dir.path().join("coverage.json");
        std::fs::write(&m, br#"{"zoom":14,"cells":[[3,4],[5,6,9]]}"#).unwrap();
        let cells = read_cells(&m);
        assert_eq!(cells[&(3, 4)], 0);
        assert_eq!(cells[&(5, 6)], 9);
    }

    #[test]
    fn a_served_file_carries_a_brotli_copy_until_it_is_too_small_for_one() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("14/1/2.json");
        let big = br#"{"buildings":[1,2,3]}"#.repeat(200);
        write_served(&path, &big).unwrap();
        let sidecar = std::fs::read(brotli_sidecar(&path)).unwrap();
        assert!(sidecar.len() < big.len());
        let mut back = Vec::new();
        brotli::BrotliDecompress(&mut &sidecar[..], &mut back).unwrap();
        assert_eq!(back, big);
        // Rewritten too small for a copy: the old copy must go, or Caddy would
        // keep serving it in place of the new file.
        write_served(&path, b"{}").unwrap();
        assert!(!brotli_sidecar(&path).exists());
        write_served(&path, &big).unwrap();
        remove_served(&path).unwrap();
        assert!(!path.exists() && !brotli_sidecar(&path).exists());
    }
}

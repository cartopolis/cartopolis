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
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
        .and_then(|v| v.get("cells").cloned())
        .and_then(|c| serde_json::from_value::<Vec<Vec<i64>>>(c).ok())
        .unwrap_or_default()
        .into_iter()
        .filter(|c| c.len() >= 2)
        .map(|c| ((c[0], c[1]), c.get(2).copied().unwrap_or(0).max(0) as u64))
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
        Some(b) => write_atomic(&path, b)?,
        None => match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        },
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
    let listed: Vec<(i64, i64, u64)> = cells.iter().map(|(&(x, y), &r)| (x, y, r)).collect();
    let mut manifest = extra;
    manifest.insert("zoom".into(), zoom.into());
    manifest.insert("cells".into(), serde_json::to_value(listed)?);
    write_atomic(manifest_path, &serde_json::to_vec(&manifest)?)
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
}

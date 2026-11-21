//! Write the packed sibling (`cartopolis_geo::formats::packed`) of every JSON
//! cell file already in a served tree.
//!
//! A pipeline run writes `x.pj` beside each `x.json` it writes
//! (`pipeline::Ctx::write_json`), but a tree extracted before 2026-11-21 has
//! none, and re-running every extraction to get them would take hours for a
//! few minutes' worth of packing. This walks the **cell trees** —
//! `<out>/<dataset>/14/{x}/{y}.json` and nothing else — and writes what the
//! pipeline would have, with the same rule: a sibling only where it is smaller,
//! and none for a manifest.
//!
//! Only the cell trees, and a cell that cannot be read or written is counted
//! and skipped rather than ending the walk: the first run over the live host
//! (2026-11-21) parsed the country-wide `areas/*.json` on its way — 3.1 GB of
//! `Value` for files no client fetches by cell — and then died on the first
//! `dikes` cell somebody had written as `0600`, after 21 minutes and one
//! sibling. A sibling as new as its JSON is left alone, so a second run over
//! the same tree does only what the first did not.

use std::path::Path;

use cartopolis_geo::formats::{packed, CACHE_ZOOM};

/// What a walk did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Packed {
    pub cells: usize,
    pub packed: usize,
    /// Cells left as they were because their sibling is as new as they are.
    pub kept: usize,
    pub json_bytes: u64,
    pub packed_bytes: u64,
    /// Cells that could not be read or whose sibling could not be written,
    /// with the first of them and its error.
    pub failed: usize,
    pub first_failure: Option<String>,
}

/// Pack every JSON cell under `root`, in place.
///
/// # Errors
///
/// When `root` itself cannot be read. A cell that cannot is counted in
/// [`Packed::failed`].
pub fn pack_tree(root: &Path) -> std::io::Result<Packed> {
    let mut done = Packed::default();
    let zoom = CACHE_ZOOM.to_string();
    for dataset in std::fs::read_dir(root)? {
        let cells = dataset?.path().join(&zoom);
        if cells.is_dir() {
            walk(&cells, &mut done);
        }
    }
    Ok(done)
}

fn walk(dir: &Path, done: &mut Packed) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        note(done, dir, "cannot list");
        return;
    };
    for entry in entries {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        if path.is_dir() {
            walk(&path, done);
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        // Cells only: a manifest is read by name and has no coordinate pairs
        // worth the trouble.
        if !name.ends_with(".json") || name == "coverage.json" {
            continue;
        }
        let Some(relative) = path.to_str() else {
            continue;
        };
        let Some(sibling) = packed::packed_path(relative) else {
            continue;
        };
        if let Err(e) = pack_one(&path, Path::new(&sibling), done) {
            note(done, &path, &e.to_string());
        }
    }
}

fn pack_one(json: &Path, sibling: &Path, done: &mut Packed) -> std::io::Result<()> {
    let modified = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    if let (Some(j), Some(s)) = (modified(json), modified(sibling)) {
        if s >= j {
            done.kept += 1;
            return Ok(());
        }
    }
    let body = std::fs::read(json)?;
    done.cells += 1;
    done.json_bytes += body.len() as u64;
    let packed = std::str::from_utf8(&body)
        .ok()
        .and_then(packed::pack)
        .filter(|p| p.len() < body.len());
    match packed {
        Some(bytes) => {
            done.packed += 1;
            done.packed_bytes += bytes.len() as u64;
            crate::stamped::write_served(sibling, &bytes)
        }
        None => crate::stamped::remove_served(sibling),
    }
}

fn note(done: &mut Packed, path: &Path, what: &str) {
    done.failed += 1;
    if done.first_failure.is_none() {
        done.first_failure = Some(format!("{}: {what}", path.display()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tree_gets_its_siblings_and_a_manifest_does_not() {
        let dir = tempfile::tempdir().unwrap();
        let cell = dir.path().join("bag/14/1/2.json");
        std::fs::create_dir_all(cell.parent().unwrap()).unwrap();
        std::fs::write(
            &cell,
            r#"{"b":[{"fp":[[53.2347567,6.5569274],[53.2348552,6.5568762]]}]}"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("bag/14/coverage.json"),
            r#"{"cells":[[1,2]]}"#,
        )
        .unwrap();
        // Not a cell tree: never read, however large.
        std::fs::create_dir_all(dir.path().join("areas")).unwrap();
        std::fs::write(dir.path().join("areas/big.json"), "not even json").unwrap();
        let done = pack_tree(dir.path()).unwrap();
        assert_eq!((done.cells, done.packed, done.failed), (1, 1, 0));
        let packed = std::fs::read(dir.path().join("bag/14/1/2.pj")).unwrap();
        assert!(packed::looks_packed(&packed));
        assert!(!dir.path().join("bag/14/coverage.pj").exists());
        assert!(!dir.path().join("areas/big.pj").exists());
        // A second run finds the sibling as new as the cell and leaves it.
        let again = pack_tree(dir.path()).unwrap();
        assert_eq!((again.cells, again.kept), (0, 1));
    }

    #[cfg(unix)]
    #[test]
    fn a_cell_that_cannot_be_read_is_counted_not_fatal() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("dikes/14/1/2.json");
        let b = dir.path().join("dikes/14/1/3.json");
        std::fs::create_dir_all(a.parent().unwrap()).unwrap();
        std::fs::write(&a, r#"{"p":[[53.2347567,6.5569274],[53.2348552,6.5568762],[53.2348392,6.5567905],[53.2347407,6.5568416],[53.2347438,6.5568582],[53.2347695,6.5568447]]}"#).unwrap();
        std::fs::write(&b, r#"{"p":[[53.2347567,6.5569274],[53.2348552,6.5568762],[53.2348392,6.5567905],[53.2347407,6.5568416],[53.2347438,6.5568582],[53.2347695,6.5568447]]}"#).unwrap();
        std::fs::set_permissions(&a, std::fs::Permissions::from_mode(0o000)).unwrap();
        let done = pack_tree(dir.path()).unwrap();
        // Root can read anything, so the unreadable cell counts only off root.
        if done.failed == 1 {
            assert!(done.first_failure.as_deref().unwrap().contains("2.json"));
            assert_eq!(done.packed, 1);
        } else {
            assert_eq!(done.packed, 2);
        }
        std::fs::set_permissions(&a, std::fs::Permissions::from_mode(0o644)).unwrap();
    }
}

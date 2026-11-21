//! Write the packed sibling (`cartopolis_geo::formats::packed`) of every JSON
//! cell file already in a served tree.
//!
//! A pipeline run writes `x.pj` beside each `x.json` it writes
//! (`pipeline::Ctx::write_json`), but a tree extracted before 2026-11-21 has
//! none, and re-running every extraction to get them would take hours for a
//! few minutes' worth of packing. This walks `<out>/<dataset>/14/**/*.json`
//! and writes what the pipeline would have, with the same rule — a sibling
//! only where it is smaller, and none for a manifest.

use std::path::Path;

use cartopolis_geo::formats::packed;

/// What a walk did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Packed {
    pub cells: usize,
    pub packed: usize,
    pub json_bytes: u64,
    pub packed_bytes: u64,
}

/// Pack every JSON cell under `root`, in place.
///
/// # Errors
///
/// When the tree cannot be read, or a sibling cannot be written.
pub fn pack_tree(root: &Path) -> std::io::Result<Packed> {
    let mut done = Packed::default();
    walk(root, &mut done)?;
    Ok(done)
}

fn walk(dir: &Path, done: &mut Packed) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            walk(&path, done)?;
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
        let body = std::fs::read(&path)?;
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
                crate::stamped::write_served(Path::new(&sibling), &bytes)?;
            }
            None => crate::stamped::remove_served(Path::new(&sibling))?,
        }
    }
    Ok(())
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
        let done = pack_tree(dir.path()).unwrap();
        assert_eq!(done.cells, 1);
        assert_eq!(done.packed, 1);
        let packed = std::fs::read(dir.path().join("bag/14/1/2.pj")).unwrap();
        assert!(packed::looks_packed(&packed));
        assert!(!dir.path().join("bag/14/coverage.pj").exists());
    }
}

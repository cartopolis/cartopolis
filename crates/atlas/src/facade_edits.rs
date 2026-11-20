//! Facades measured again because somebody said the city model got one wrong:
//! from Mapillary's street photographs, or from a photograph they took
//! themselves (the building card's two facade buttons).
//!
//! **A tree of its own, `facade_edits/`, beside the `facades` dataset's**, not
//! written into it: a run of that dataset rewrites every cell from the city
//! model, and would quietly undo every correction anybody made. The client
//! lays this tree over that one, a building at a time.
//!
//! Stamped like the building designs (`crate::stamped`), because somebody is
//! standing in front of the building looking at it: the client polls the
//! manifest, and a correction is a new stamp and one fetch.
//!
//! Every correction is also a line in `facade_edits.jsonl` in atlas's data
//! directory — who, when, from what, and what the building had before — so an
//! operator can see what changed and [`FacadeEdits::remove`] what was wrong.
//! The photograph itself is never kept.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use cartopolis_geo::formats::{facades as fmt, CACHE_ZOOM};

pub struct FacadeEdits {
    log_path: PathBuf,
    tree: PathBuf,
    /// One correction at a time: each rewrites a shared cell and the shared
    /// manifest, and two interleaved would lose one of them.
    lock: Mutex<()>,
    /// One measurement at a time: each runs the facade network, a second or
    /// two of every core, and a queue of them would starve the tile server.
    pub busy: Mutex<()>,
}

impl FacadeEdits {
    #[must_use]
    pub fn new(data_dir: &Path, site_dir: &Path) -> Self {
        Self {
            log_path: data_dir.join("facade_edits.jsonl"),
            tree: site_dir.join("facade_edits"),
            lock: Mutex::new(()),
            busy: Mutex::new(()),
        }
    }

    fn cell_path(&self, cell: (i64, i64)) -> PathBuf {
        self.tree
            .join(CACHE_ZOOM.to_string())
            .join(cell.0.to_string())
            .join(format!("{}.json", cell.1))
    }

    fn read_cell(&self, cell: (i64, i64)) -> fmt::Cell {
        std::fs::read_to_string(self.cell_path(cell))
            .ok()
            .and_then(|t| fmt::parse(&t).ok())
            .unwrap_or_default()
    }

    fn publish(&self, cell: (i64, i64), doc: &fmt::Cell) -> std::io::Result<()> {
        let body = if doc.facades.is_empty() {
            None
        } else {
            Some(cartopolis_geo::formats::to_vec(doc).map_err(std::io::Error::other)?)
        };
        let mut extra = serde_json::Map::new();
        extra.insert(
            "attribution".into(),
            "Facades measured from street photographs: Mapillary contributors (CC BY-SA 4.0) \
             and players"
                .into(),
        );
        crate::stamped::publish_cell(&self.tree, CACHE_ZOOM, cell, body.as_deref(), 0, extra)
    }

    fn record(&self, line: &serde_json::Value) -> std::io::Result<()> {
        if let Some(dir) = self.log_path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log_path)?;
        writeln!(f, "{line}")
    }

    /// The correction a building has, if any.
    #[must_use]
    pub fn get(&self, cell: (i64, i64), pand: &str) -> Option<fmt::Facade> {
        self.read_cell(cell).facades.get(pand).cloned()
    }

    /// Put `facade` in place of whatever `pand`, filed under z14 `cell`, had.
    pub fn save(
        &self,
        cell: (i64, i64),
        pand: &str,
        facade: &fmt::Facade,
        by: &str,
    ) -> std::io::Result<()> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut doc = self.read_cell(cell);
        let before = doc.facades.insert(pand.to_string(), facade.clone());
        self.publish(cell, &doc)?;
        self.record(&serde_json::json!({
            "at": crate::stamped::now_ms(),
            "by": by,
            "pand": pand,
            "cell": [cell.0, cell.1],
            "facade": facade,
            "before": before,
        }))
    }

    /// Take `pand`'s correction out: it goes back to the city model's
    /// measurement. `false` when it had none.
    pub fn remove(&self, cell: (i64, i64), pand: &str, by: &str) -> std::io::Result<bool> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut doc = self.read_cell(cell);
        let Some(before) = doc.facades.remove(pand) else {
            return Ok(false);
        };
        self.publish(cell, &doc)?;
        self.record(&serde_json::json!({
            "at": crate::stamped::now_ms(),
            "by": by,
            "pand": pand,
            "cell": [cell.0, cell.1],
            "facade": null,
            "before": before,
        }))?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_correction_is_published_stamped_and_logged_and_comes_out_again() {
        let dir = tempfile::tempdir().unwrap();
        let edits = FacadeEdits::new(&dir.path().join("data"), &dir.path().join("site"));
        let red = fmt::Facade {
            c: [150, 60, 50],
            m: fmt::Material::Brick,
            n: 4000,
            s: Some("photo".into()),
        };
        edits
            .save((8490, 5320), "0014100022192356", &red, "ann")
            .unwrap();
        assert_eq!(edits.get((8490, 5320), "0014100022192356"), Some(red));
        let manifest =
            crate::stamped::read_cells(&dir.path().join("site/facade_edits/coverage.json"));
        assert!(manifest[&(8490, 5320)] > 0);
        assert!(edits
            .remove((8490, 5320), "0014100022192356", "op")
            .unwrap());
        assert!(!edits
            .remove((8490, 5320), "0014100022192356", "op")
            .unwrap());
        // The last correction gone, the cell leaves the manifest.
        let manifest =
            crate::stamped::read_cells(&dir.path().join("site/facade_edits/coverage.json"));
        assert!(manifest.is_empty());
        let log = std::fs::read_to_string(dir.path().join("data/facade_edits.jsonl")).unwrap();
        assert_eq!(log.lines().count(), 2);
    }
}

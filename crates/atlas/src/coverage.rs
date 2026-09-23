//! What the served tree holds, read from the manifests rather than remembered.
//! Ported from the earlier TypeScript server (`coverage.ts`).
//!
//! The output directory is the authority, not this process: the tiles outlive
//! every restart and an operator may have rsynced them, so "what is covered" has a
//! filesystem answer and reading it costs six small files. The manifest's mtime is
//! what dates it — a `generated_at` key would be a change to a format the client's
//! parsers share, for a timestamp the filesystem already keeps.

use std::path::Path;
use std::time::UNIX_EPOCH;

use serde::de::{MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::pipeline::{Count, DATASETS};

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoverageEntry {
    /// Named totals the manifest carries — `building_count`, `tree_count`, …
    pub counts: Vec<Count>,
    pub dataset: String,
    /// How many z14 cells the client will source from this host.
    pub cells: usize,
    pub label: String,
    /// `None` when nothing has ever been extracted for this dataset.
    pub name: Option<String>,
    pub bbox: Option<Vec<f64>>,
    /// When the manifest was last written, in ms — the closest thing to "last
    /// refreshed".
    pub updated_at: Option<f64>,
}

/// `building_count` → "buildings". The manifests name totals in the singular,
/// which reads wrong beside a number, and an `s` is not the fix ("addresss"), so
/// the plurals are named and an unknown stem passes through as it is.
fn plural(stem: &str) -> &str {
    match stem {
        "address" => "addresses",
        "berth" => "berths",
        "building" => "buildings",
        "line" => "boundaries",
        "measured_line" => "measured boundaries",
        "monument" => "monuments",
        "piece" => "defence sections",
        "ring" => "water outlines",
        "prop" => "street objects",
        "surface" => "paved surfaces",
        "tree" => "trees",
        other => other,
    }
}

/// A JSON object's entries in the order the file has them. The console prints the
/// totals in the manifest's own order, and `serde_json`'s map sorts its keys
/// unless the workspace-wide `preserve_order` feature is on — which it is not,
/// and turning it on would change the map type under every crate that uses it.
struct Ordered(Vec<(String, Value)>);

impl<'de> Deserialize<'de> for Ordered {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Ordered;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a JSON object")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Ordered, A::Error> {
                let mut out = Vec::new();
                while let Some(entry) = map.next_entry::<String, Value>()? {
                    out.push(entry);
                }
                Ok(Ordered(out))
            }
        }
        d.deserialize_map(V)
    }
}

fn read(file: &Path, entry: &mut CoverageEntry) -> Option<()> {
    let Ordered(manifest) = serde_json::from_slice(&std::fs::read(file).ok()?).ok()?;
    for (key, value) in &manifest {
        match key.as_str() {
            "name" => entry.name = value.as_str().map(str::to_string),
            "bbox_rd" => {
                entry.bbox = value
                    .as_array()
                    .map(|a| a.iter().filter_map(Value::as_f64).collect())
            }
            "cells" => entry.cells = value.as_array().map_or(0, Vec::len),
            _ => {
                if let (Some(stem), Some(n)) = (key.strip_suffix("_count"), value.as_u64()) {
                    entry.counts.push(Count {
                        label: plural(stem).to_string(),
                        n,
                    });
                }
            }
        }
    }
    let modified = std::fs::metadata(file).ok()?.modified().ok()?;
    entry.updated_at = modified
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs_f64() * 1000.0);
    Some(())
}

/// Every dataset's coverage, in the registry's order. Blocking: call it from a
/// blocking thread.
#[must_use]
pub fn coverage(out_root: &Path) -> Vec<CoverageEntry> {
    DATASETS
        .iter()
        .map(|dataset| {
            let mut entry = CoverageEntry {
                counts: Vec::new(),
                dataset: dataset.id.to_string(),
                cells: 0,
                label: dataset.label.to_string(),
                name: None,
                bbox: None,
                updated_at: None,
            };
            // No manifest is the ordinary state of a dataset nobody has run yet,
            // and it is what the console draws as "not extracted".
            let _ = read(&out_root.join(dataset.coverage), &mut entry);
            entry
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unextracted_tree_reports_every_dataset_as_never_extracted() {
        let dir = tempfile::tempdir().unwrap();
        let all = coverage(&dir.path().join("nothing-here"));
        assert_eq!(all.len(), DATASETS.len());
        assert!(all.iter().all(|e| e.name.is_none() && e.cells == 0));
    }

    #[test]
    fn reads_the_name_the_cells_and_the_totals_in_the_files_order() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("bgt")).unwrap();
        std::fs::write(
            dir.path().join("bgt/coverage.json"),
            r#"{"attribution":"x","bbox_rd":[229000,578000,238000,587000],
               "cells":[[8490,5319],[8490,5320]],"name":"groningen","source":"y",
               "tree_count":2340,"prop_count":1781,"zoom":14}"#,
        )
        .unwrap();
        let entry = coverage(dir.path())
            .into_iter()
            .find(|e| e.dataset == "bgt")
            .unwrap();
        assert_eq!(entry.name.as_deref(), Some("groningen"));
        assert_eq!(entry.cells, 2);
        assert_eq!(
            entry.counts,
            vec![
                Count::new("trees", 2340),
                Count::new("street objects", 1781)
            ]
        );
        assert!(entry.updated_at.unwrap() > 0.0);
    }
}

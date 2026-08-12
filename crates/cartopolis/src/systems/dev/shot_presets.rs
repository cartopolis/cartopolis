//! Named camera positions for the scripted screenshot harness — the checked-in
//! answer to "which six views are the docs made of?".
//!
//! The dev panel can capture a chrome-free frame from wherever the camera
//! happens to be, but that makes the README images a manual ritual: fly there,
//! eyeball it, click, repeat six times, and hope the next person picks the same
//! viewpoints. A manifest ([`docs/shots.toml`]) turns those viewpoints into data
//! — each entry is a name, a lat/lng/altitude, a look direction, and the time of
//! day and weather it should be shot in — so the whole set regenerates from one
//! command and lands on identical framing every time:
//!
//! ```bash
//! cargo shots                     # every preset → docs/img/<name>.png
//! cargo shots --only globe,route  # just these two
//! ```
//!
//! The dev panel's "📋 Copy as preset" button prints the current camera as a
//! `[[shot]]` block, which is how a new position gets *into* the manifest: fly
//! there by hand once, paste, and it is reproducible from then on.
//!
//! Everything here is parsed before the Bevy app is built (the manifest can set
//! the render size, which a headless run needs in order to create its window
//! stand-in), so a bad manifest fails with a message instead of a black run.

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Manifest path used when `--shots` is passed without a value.
const DEFAULT_MANIFEST: &str = "docs/shots.toml";

/// Run-wide settings plus the ordered list of viewpoints. Unknown keys are a
/// hard error: a typo'd `hide-ui` that silently did nothing would show up only
/// as a chrome-covered README image days later.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// Offscreen render size, "WxH" (CLI `--size` still wins).
    #[serde(default)]
    pub size: Option<String>,
    /// Where `name`-derived outputs go, relative to the manifest's own
    /// directory. Defaults to the manifest's directory.
    #[serde(default)]
    pub out_dir: Option<PathBuf>,
    /// Seconds of quiet streaming before a capture (harness default otherwise).
    #[serde(default)]
    pub settle: Option<f32>,
    /// Hard cap on the settle wait, per shot.
    #[serde(default)]
    pub wait: Option<f32>,
    /// Default camera mode for shots that don't name one.
    #[serde(default)]
    pub mode: Option<String>,
    /// `{z}/{x}/{y}` tile template for the run. A scripted run never sees the
    /// server handshake that normally supplies one, and the built-in fallback is
    /// the *raster* OSM basemap — on which the route planner has nothing to
    /// solve against (ADR-037). Pinning the source here is what makes the image
    /// set reproducible rather than dependent on the operator's environment.
    /// `CARTO_TILE_URL` still wins, so a local mirror needs no file edit.
    #[serde(default)]
    pub tile_url: Option<String>,
    /// Default for `hide_ui` — README images want no chrome, so a manifest
    /// normally sets this once at the top instead of per shot.
    #[serde(default)]
    pub hide_ui: Option<bool>,
    #[serde(default, rename = "shot")]
    pub shots: Vec<Preset>,
}

/// One remembered camera position.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Preset {
    /// Identifies the shot on the command line (`--only`) and names its file.
    pub name: String,
    /// What the view is meant to show. Documentation only — it ends up in the
    /// log line for the shot, so a run reads as a list of intents.
    #[serde(default)]
    pub description: Option<String>,
    /// `[lat, lng, altitude_m]`.
    pub at: [f64; 3],
    /// `[pitch, yaw]` in degrees (−90 = straight down). Defaults to straight down.
    #[serde(default)]
    pub look: Option<[f32; 2]>,
    /// "free", "earth" or "avatar"; falls back to the manifest's `mode`.
    #[serde(default)]
    pub mode: Option<String>,
    /// Output path, relative to the manifest directory. Defaults to
    /// `<out_dir>/<name>.png`.
    #[serde(default)]
    pub out: Option<PathBuf>,
    /// Hour of day (0–24) to pin the sun to for this shot.
    #[serde(default)]
    pub time: Option<f32>,
    #[serde(default)]
    pub fog: Option<f32>,
    #[serde(default)]
    pub clouds: Option<f32>,
    #[serde(default)]
    pub rain: Option<f32>,
    /// UI panels to open for this shot (`dev`, `layers`, `route`, … or `all`) —
    /// same vocabulary as `--ui`. A shot that documents a panel opts in here;
    /// the others stay chrome-free.
    #[serde(default)]
    pub ui: Option<Vec<String>>,
    /// Per-shot override of the manifest's `hide_ui`.
    #[serde(default)]
    pub hide_ui: Option<bool>,
}

impl Manifest {
    /// Read and validate a manifest. `path` is resolved as given; the caller
    /// gets [`default_manifest_path`] when `--shots` carried no value.
    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read shot manifest {}: {e}", path.display()))?;
        let manifest: Manifest = toml::from_str(&text)
            .map_err(|e| format!("cannot parse shot manifest {}: {e}", path.display()))?;
        manifest.validate()?;
        Ok(manifest)
    }

    fn validate(&self) -> Result<(), String> {
        if self.shots.is_empty() {
            return Err("shot manifest has no [[shot]] entries".into());
        }
        let mut seen: Vec<&str> = Vec::new();
        for s in &self.shots {
            if s.name.trim().is_empty() {
                return Err("every [[shot]] needs a non-empty name".into());
            }
            if seen.contains(&s.name.as_str()) {
                // Duplicates would silently overwrite each other's PNG.
                return Err(format!("duplicate shot name {:?} in manifest", s.name));
            }
            seen.push(&s.name);
        }
        Ok(())
    }

    /// The shots named by `only` (all of them when it's empty), in manifest
    /// order. An unknown name is an error listing what *is* available — a typo
    /// would otherwise quietly regenerate nothing.
    pub fn select(&self, only: &[String]) -> Result<Vec<&Preset>, String> {
        if only.is_empty() {
            return Ok(self.shots.iter().collect());
        }
        for want in only {
            if !self.shots.iter().any(|s| &s.name == want) {
                let names: Vec<&str> = self.shots.iter().map(|s| s.name.as_str()).collect();
                return Err(format!(
                    "unknown shot {:?}; manifest has: {}",
                    want,
                    names.join(", ")
                ));
            }
        }
        Ok(self
            .shots
            .iter()
            .filter(|s| only.contains(&s.name))
            .collect())
    }

    /// Absolute-ish output path for a preset: an explicit `out`, else
    /// `<out_dir>/<name>.png`, both resolved against the manifest's directory so
    /// `cargo shots` writes to the same files from any working directory.
    pub fn out_path(&self, manifest_path: &Path, preset: &Preset) -> PathBuf {
        let base = manifest_path.parent().unwrap_or(Path::new("."));
        match &preset.out {
            Some(out) => base.join(out),
            None => {
                let dir = self.out_dir.clone().unwrap_or_else(|| PathBuf::from("."));
                base.join(dir).join(format!("{}.png", preset.name))
            }
        }
    }
}

/// Where `--shots` looks when given no path: the manifest relative to the
/// working directory, falling back to the one in the source tree. The fallback
/// is what makes `cargo shots` work from a subdirectory of the repo.
pub fn default_manifest_path() -> PathBuf {
    let cwd = PathBuf::from(DEFAULT_MANIFEST);
    if cwd.exists() {
        return cwd;
    }
    let in_tree = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(DEFAULT_MANIFEST);
    if in_tree.exists() {
        return in_tree;
    }
    cwd
}

/// Render the current camera as a `[[shot]]` block, ready to paste into the
/// manifest. This is the bridge from "I flew somewhere that looks good" to a
/// remembered position (the dev panel's copy button).
pub fn preset_toml(
    name: &str,
    lat: f64,
    lng: f64,
    alt: f64,
    pitch_deg: f32,
    yaw_deg: f32,
    time_of_day: f32,
    fog: f32,
    clouds: f32,
) -> String {
    format!(
        "[[shot]]\n\
         name = \"{name}\"\n\
         description = \"\"\n\
         at = [{lat:.5}, {lng:.5}, {alt:.0}]\n\
         look = [{pitch_deg:.1}, {yaw_deg:.1}]\n\
         time = {time_of_day:.1}\n\
         fog = {fog:.2}\n\
         clouds = {clouds:.2}\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
size = "1600x900"
out_dir = "img"
hide_ui = true

[[shot]]
name = "city"
at = [53.2194, 6.5665, 450]
look = [-20, 35]

[[shot]]
name = "globe"
at = [53.2194, 6.5665, 12000000]
mode = "earth"
out = "img/custom.png"
"#;

    #[test]
    fn parses_and_defaults_paths() {
        let m: Manifest = toml::from_str(SAMPLE).unwrap();
        m.validate().unwrap();
        assert_eq!(m.shots.len(), 2);
        let path = PathBuf::from("docs/shots.toml");
        // Name-derived output, resolved against the manifest's directory.
        assert_eq!(
            m.out_path(&path, &m.shots[0]),
            PathBuf::from("docs/img/city.png")
        );
        // An explicit `out` is used verbatim (still manifest-relative).
        assert_eq!(
            m.out_path(&path, &m.shots[1]),
            PathBuf::from("docs/img/custom.png")
        );
    }

    #[test]
    fn select_filters_and_rejects_unknown_names() {
        let m: Manifest = toml::from_str(SAMPLE).unwrap();
        assert_eq!(m.select(&[]).unwrap().len(), 2);
        let one = m.select(&["globe".to_string()]).unwrap();
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].name, "globe");
        let err = m.select(&["nope".to_string()]).unwrap_err();
        assert!(err.contains("nope") && err.contains("city"), "got {err:?}");
    }

    #[test]
    fn duplicate_names_are_rejected() {
        let dup = "[[shot]]\nname = \"a\"\nat = [0,0,10]\n[[shot]]\nname = \"a\"\nat = [0,0,20]\n";
        let m: Manifest = toml::from_str(dup).unwrap();
        assert!(m.validate().unwrap_err().contains("duplicate"));
    }

    #[test]
    fn unknown_keys_are_rejected() {
        // A typo'd key must fail loudly rather than being ignored.
        let bad = "[[shot]]\nname = \"a\"\nat = [0,0,10]\nhide-ui = true\n";
        assert!(toml::from_str::<Manifest>(bad).is_err());
    }

    #[test]
    fn copied_preset_round_trips_into_the_manifest() {
        // The dev panel's copy button must emit something the loader accepts.
        let block = preset_toml("here", 53.2194, 6.5665, 420.0, -18.0, 35.0, 9.5, 0.1, 0.2);
        let m: Manifest = toml::from_str(&block).unwrap();
        m.validate().unwrap();
        assert_eq!(m.shots[0].at[2], 420.0);
        assert_eq!(m.shots[0].look.unwrap()[0], -18.0);
    }
}

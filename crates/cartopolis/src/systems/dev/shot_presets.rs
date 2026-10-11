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
//! cargo shots                     # the documentation set → docs/img/<name>.png
//! cargo shots --only globe,route  # just these two
//! cargo shots --only tunnels      # a subject → docs/img/tunnels/<name>.png
//! cargo shots --only all          # everything
//! ```
//!
//! The dev panel's "📋 Copy as preset" button prints the current camera as a
//! `[[shot]]` block, which is how a new position gets *into* the manifest: fly
//! there by hand once, paste, and it is reproducible from then on.
//!
//! # Two kinds of remembered viewpoint
//!
//! The manifest began as the README's six images and is now also where a
//! *subject* is remembered — the mouth of a tunnel, a bridge deck, a door you
//! can walk through. Both are the same thing (a viewpoint somebody found by hand
//! and nobody should have to find again) and they differ in one field.
//!
//! A shot with no [`Preset::group`] is a documentation image. `cargo shots`
//! regenerates exactly those, into `<out_dir>/<name>.png`, and the README links
//! every file. A shot *with* a group is a subject: `--only <group>` captures it,
//! it lands in `<out_dir>/<group>/<name>.png`, and a bare run leaves it alone.
//!
//! Both halves of that split are load-bearing. `docs/img` is **published**, so a
//! tunnel-mouth viewpoint dropped in beside the README images makes "which of
//! these does the documentation use?" a question somebody has to go and answer.
//! And regenerating six published images must not become a twenty-shot run: the
//! subjects exist to be *asked for* while working on tunnels or doors, one group
//! at a time. [`ALL`] is the escape hatch for genuinely everything.
//!
//! One manifest rather than a file per subject, because the run-wide settings at
//! the top — the tile source, the size, the settle and wait — are what make two
//! runs comparable, and splitting the file splits those.
//!
//! Everything here is parsed before the Bevy app is built (the manifest can set
//! the render size, which a headless run needs in order to create its window
//! stand-in), so a bad manifest fails with a message instead of a black run.

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Manifest path used when `--shots` is passed without a value.
const DEFAULT_MANIFEST: &str = "docs/shots.toml";

/// `--only` selector meaning every shot in the manifest, grouped or not.
///
/// Reserved: [`Manifest::load`] refuses a shot or group called this, because a
/// group that silently shadowed "run everything" would be the worst kind of quiet
/// wrong answer — a full run that captures one subject.
pub const ALL: &str = "all";

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
    /// Hour of day, fog, cloud and rain for every shot that names none of its
    /// own.
    ///
    /// **A group is a set of views of one subject, and the light has to be the
    /// same across them or two captures are not comparable.** `docs/grade-shots
    /// .toml` wrote these four at the top on exactly that reasoning — a seven
    /// metre trench reads only when the sun puts one wall in shadow, so every
    /// tunnel view wants the same mid-afternoon — and `deny_unknown_fields`
    /// rejected the file. It had therefore **never parsed**, so the manifest that
    /// exists to check grade separation could not be run at all, which is a large
    /// part of why this feature was verified by hand-aimed one-off captures.
    #[serde(default)]
    pub time: Option<f32>,
    #[serde(default)]
    pub fog: Option<f32>,
    #[serde(default)]
    pub clouds: Option<f32>,
    #[serde(default)]
    pub rain: Option<f32>,
    /// Write each capture's aerial photograph beside it — see
    /// `ShotHarness::aerial_reference`.
    #[serde(default)]
    pub aerial_reference: Option<bool>,
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
    /// What subject this view is *for* — `tunnels`, `bridges`, `crossroads`.
    ///
    /// `--only` matches a group as readily as a name, so a manifest can hold every
    /// place worth checking and a run can ask for one subject: the alternative is
    /// a file per subject, which splits the shared settle/size/time settings that
    /// make two runs comparable in the first place.
    #[serde(default)]
    pub group: Option<String>,
    /// `[lat, lng, altitude_m]`.
    pub at: [f64; 3],
    /// `[pitch, yaw]` in degrees (−90 = straight down). Defaults to straight down.
    #[serde(default)]
    pub look: Option<[f32; 2]>,
    /// `[lat, lng]` to aim at, instead of an angle — overrides `look`.
    ///
    /// The instrument that removes the aiming error this manifest's own header
    /// is about. A pitch aims at whatever is `altitude / tan(pitch)` metres
    /// ahead of a camera whose altitude is measured from the *anchor's* ground,
    /// while the subject stands on the measured ground somewhere else; at the
    /// shallow angle a sunken road needs, three metres of difference moves the
    /// aim thirty. `shot_harness::aim_scripted_camera` has computed this from
    /// where the camera really ends up since the day tunnels landed, and until
    /// 2026-09-22 there was **no way to ask for it** — not a flag, not a
    /// manifest key — so every camera in this file was aimed by the method that
    /// missed eleven times.
    #[serde(default)]
    pub look_at: Option<[f64; 2]>,
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
            // `all` is how a run asks for everything, so a shot or group wearing
            // that name would turn "capture the lot" into "capture this one" — a
            // wrong answer that looks like a successful run.
            if s.name == ALL || s.group.as_deref() == Some(ALL) {
                return Err(format!(
                    "{ALL:?} is reserved for selecting every shot; rename the shot or group"
                ));
            }
            if let Some(g) = &s.group {
                if g.trim().is_empty() {
                    return Err(format!("shot {:?} has an empty group", s.name));
                }
                // The group becomes a directory under `out_dir`, so it has to be
                // one path segment and not a way out of it.
                if g.contains(['/', '\\']) || g.starts_with('.') {
                    return Err(format!(
                        "shot {:?} has group {g:?}: a group names one directory, \
                         so no separators and no leading dot",
                        s.name
                    ));
                }
            }
        }
        Ok(())
    }

    /// Every group in the manifest, sorted and deduplicated — what `--only` will
    /// take besides a shot name. Used by the unknown-selector error and by the
    /// dev panel, so the list a person is offered is the list that works.
    #[must_use]
    pub fn groups(&self) -> Vec<&str> {
        let mut g: Vec<&str> = self
            .shots
            .iter()
            .filter_map(|s| s.group.as_deref())
            .collect();
        g.sort_unstable();
        g.dedup();
        g
    }

    /// The shots named by `only` (all of them when it's empty), in manifest
    /// order. An unknown name is an error listing what *is* available — a typo
    /// would otherwise quietly regenerate nothing.
    /// The presets `--only` names: by shot name, or by [`Preset::group`].
    ///
    /// An unknown selector is an error rather than an empty run, the same rule the
    /// manifest keys follow — a typo that quietly captures nothing reads as "the
    /// feature is gone" later.
    ///
    /// **With no selector this is the ungrouped shots, not every shot.** The
    /// ungrouped ones are the documentation set — `cargo shots` regenerates the
    /// README images and that is the command's whole job. A group is a *subject*
    /// to go and look at (tunnels, bridges, doors), added so a harness run can ask
    /// for one; sweeping them into the default would make regenerating six README
    /// images a twenty-shot run that also writes files nothing publishes. Ask for
    /// a group by name, or `all` for genuinely everything.
    pub fn select(&self, only: &[String]) -> Result<Vec<&Preset>, String> {
        if only.is_empty() {
            let bare: Vec<&Preset> = self.shots.iter().filter(|s| s.group.is_none()).collect();
            // A manifest that is *all* groups (`docs/grade-shots.toml`,
            // `docs/bridge-shots.toml`) has no documentation set, and an empty
            // run does not end: the harness waits for a first shot that never
            // comes, and the client sits there streaming the default view.
            if bare.is_empty() && !self.shots.is_empty() {
                return Err(format!(
                    "every shot in this manifest is in a group; pass --only <group> \
                     (groups: {}) or --only {ALL}",
                    self.groups().join(", ")
                ));
            }
            return Ok(bare);
        }
        if only.iter().any(|w| w == ALL) {
            return Ok(self.shots.iter().collect());
        }
        let matches = |s: &Preset, want: &String| {
            &s.name == want || s.group.as_ref().is_some_and(|g| g == want)
        };
        for want in only {
            if !self.shots.iter().any(|s| matches(s, want)) {
                let names: Vec<&str> = self.shots.iter().map(|s| s.name.as_str()).collect();
                let groups = self.groups();
                return Err(format!(
                    "unknown shot or group {:?}; manifest has shots: {} — groups: {}",
                    want,
                    names.join(", "),
                    if groups.is_empty() {
                        "(none)".to_string()
                    } else {
                        groups.join(", ")
                    }
                ));
            }
        }
        Ok(self
            .shots
            .iter()
            .filter(|s| only.iter().any(|want| matches(s, want)))
            .collect())
    }

    /// Absolute-ish output path for a preset: an explicit `out`, else
    /// `<out_dir>/<name>.png` for a documentation shot and
    /// `<out_dir>/<group>/<name>.png` for a grouped one, all resolved against the
    /// manifest's directory so `cargo shots` writes to the same files from any
    /// working directory.
    ///
    /// **A group gets its own directory because `docs/img` is published.** Every
    /// file there is linked from the README; a subject shot is a thing to go and
    /// look at while working on tunnels or doors, and dropping one in beside them
    /// makes "which of these does the documentation use?" a question you have to
    /// go and answer. The ungrouped path is unchanged to the byte, so the
    /// documentation set keeps its filenames.
    pub fn out_path(&self, manifest_path: &Path, preset: &Preset) -> PathBuf {
        let base = manifest_path.parent().unwrap_or(Path::new("."));
        match &preset.out {
            Some(out) => base.join(out),
            None => {
                let dir = self.out_dir.clone().unwrap_or_else(|| PathBuf::from("."));
                let dir = match &preset.group {
                    Some(g) => dir.join(g),
                    None => dir,
                };
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
/// `group` names the subject this view is *for*, and an empty one means the
/// documentation set. It is emitted as a line rather than left for the pasting
/// person to remember, because a preset that arrives ungrouped lands in the
/// published `docs/img` and is only noticed when the README grows a picture of a
/// tunnel.
#[allow(clippy::too_many_arguments)]
pub fn preset_toml(
    name: &str,
    group: &str,
    lat: f64,
    lng: f64,
    alt: f64,
    pitch_deg: f32,
    yaw_deg: f32,
    time_of_day: f32,
    fog: f32,
    clouds: f32,
) -> String {
    let group = group.trim();
    let group_line = if group.is_empty() {
        String::new()
    } else {
        format!("group = \"{group}\"\n")
    };
    format!(
        "[[shot]]\n\
         name = \"{name}\"\n\
         {group_line}\
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

    /// **Every manifest in the tree loads**, which `docs/grade-shots.toml` did
    /// not for the whole life of the feature it exists to check.
    ///
    /// It set `time`, `fog`, `clouds` and `rain` at the top — correctly, because
    /// a group is a set of views of one subject and they have to be lit alike —
    /// and [`Manifest`] had no such keys, so `deny_unknown_fields` refused the
    /// file. The only symptom was a one-line parse error from a command nobody
    /// in CI runs, so the grade-separation image set was dead on arrival and
    /// tunnels went on being verified by hand-aimed one-off captures instead.
    /// A manifest that cannot be loaded is not a manifest, and this is cheap.
    #[test]
    fn every_checked_in_manifest_loads() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs");
        let mut seen = 0;
        for entry in std::fs::read_dir(&root).expect("docs/ reads").flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "toml") {
                let text = std::fs::read_to_string(&path).expect("a manifest reads");
                // Only the shot manifests: `docs/` also holds `perf.toml`, whose
                // scenarios are a different schema.
                if !text.contains("[[shot]]") {
                    continue;
                }
                seen += 1;
                let m: Manifest = toml::from_str(&text)
                    .unwrap_or_else(|e| panic!("{} does not parse: {e}", path.display()));
                m.validate()
                    .unwrap_or_else(|e| panic!("{} is invalid: {e}", path.display()));
            }
        }
        assert!(seen >= 2, "found {seen} shot manifests, expected at least the documentation set and the grade set");
    }

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

    /// `--only` takes a subject as readily as a name, which is what lets one
    /// manifest hold every place worth checking and a run ask for one subject.
    #[test]
    fn select_takes_a_group_as_well_as_a_name() {
        let m: Manifest = toml::from_str(
            r#"
tile_url = "x"
[[shot]]
name = "a"
group = "tunnels"
at = [1.0, 2.0, 3.0]
[[shot]]
name = "b"
group = "tunnels"
at = [1.0, 2.0, 3.0]
[[shot]]
name = "c"
group = "bridges"
at = [1.0, 2.0, 3.0]
"#,
        )
        .unwrap();
        let picked = m.select(&["tunnels".to_string()]).unwrap();
        assert_eq!(picked.len(), 2, "a group should take every shot in it");
        let picked = m.select(&["c".to_string()]).unwrap();
        assert_eq!(picked.len(), 1, "a name should still work");
        let picked = m.select(&["bridges".to_string(), "a".to_string()]).unwrap();
        assert_eq!(picked.len(), 2, "names and groups mix");
        // An unknown selector is an error, not an empty run: a typo that captures
        // nothing reads as "the feature is gone" when somebody looks later.
        let err = m.select(&["tunnel".to_string()]).unwrap_err();
        assert!(err.contains("groups: bridges, tunnels"), "{err}");
        assert_eq!(m.groups(), vec!["bridges", "tunnels"]);
    }

    /// **A bare run is the documentation set, not everything.**
    ///
    /// `cargo shots` exists to regenerate the README images, and every file it
    /// writes into `docs/img` is linked from there. Groups were added so a harness
    /// run can ask for a subject — tunnels, bridges, doors — and sweeping those
    /// into the default would turn regenerating six published images into a
    /// twenty-shot run that also writes files nothing publishes.
    #[test]
    fn a_bare_run_is_the_ungrouped_documentation_set() {
        let m: Manifest = toml::from_str(
            r#"
[[shot]]
name = "city"
at = [1.0, 2.0, 3.0]
[[shot]]
name = "mouth"
group = "tunnels"
at = [1.0, 2.0, 3.0]
"#,
        )
        .unwrap();
        let bare: Vec<&str> = m
            .select(&[])
            .unwrap()
            .iter()
            .map(|s| s.name.as_str())
            .collect();
        assert_eq!(bare, vec!["city"], "a group must be opted into");
        assert_eq!(
            m.select(&[ALL.to_string()]).unwrap().len(),
            2,
            "`all` is both"
        );
    }

    /// A manifest with no ungrouped shot refuses a bare run rather than starting
    /// a harness with nothing to capture, which never exits.
    #[test]
    fn a_bare_run_of_an_all_grouped_manifest_is_refused() {
        let m: Manifest = toml::from_str(
            r#"
[[shot]]
name = "mouth"
group = "tunnels"
at = [1.0, 2.0, 3.0]
"#,
        )
        .unwrap();
        let err = m.select(&[]).unwrap_err();
        assert!(err.contains("--only <group> (groups: tunnels)"), "{err}");
        assert_eq!(m.select(&["tunnels".to_string()]).unwrap().len(), 1);
    }

    /// A group is a directory under `out_dir`, so a subject shot cannot land
    /// among the published images — and cannot climb out of the tree either.
    #[test]
    fn a_grouped_shot_writes_into_its_own_directory() {
        let m: Manifest = toml::from_str(
            r#"
out_dir = "img"
[[shot]]
name = "city"
at = [1.0, 2.0, 3.0]
[[shot]]
name = "mouth"
group = "tunnels"
at = [1.0, 2.0, 3.0]
"#,
        )
        .unwrap();
        m.validate().unwrap();
        let path = PathBuf::from("docs/shots.toml");
        assert_eq!(
            m.out_path(&path, &m.shots[0]),
            PathBuf::from("docs/img/city.png"),
            "the documentation set keeps its filenames exactly"
        );
        assert_eq!(
            m.out_path(&path, &m.shots[1]),
            PathBuf::from("docs/img/tunnels/mouth.png")
        );
    }

    #[test]
    fn a_group_may_not_escape_its_directory_or_shadow_all() {
        for (bad, why) in [
            (r#"group = "../../etc""#, "a separator"),
            (r#"group = ".hidden""#, "a leading dot"),
            (r#"group = "all""#, "the reserved selector"),
            (r#"group = """#, "an empty group"),
        ] {
            let text = format!("[[shot]]\nname = \"a\"\n{bad}\nat = [1.0, 2.0, 3.0]\n");
            let m: Manifest = toml::from_str(&text).unwrap();
            assert!(m.validate().is_err(), "{why} must be refused: {bad}");
        }
    }

    /// The copy button has to write the group it was given, or a preset collected
    /// while working on tunnels is pasted in ungrouped and joins the published
    /// set — noticed when the README grows a picture of a tunnel.
    #[test]
    fn a_copied_preset_carries_its_group_and_parses_back() {
        let block = preset_toml(
            "mouth", "tunnels", 53.2, 6.5, 25.0, -6.0, 48.0, 12.0, 0.0, 0.0,
        );
        assert!(block.contains("group = \"tunnels\""), "{block}");
        let m: Manifest = toml::from_str(&block).expect("a copied block must parse back");
        assert_eq!(m.shots[0].group.as_deref(), Some("tunnels"));

        // Blank means the documentation set, and must emit no key at all — an
        // empty `group = ""` is refused by `validate`.
        let plain = preset_toml("city", "  ", 53.2, 6.5, 25.0, -6.0, 48.0, 12.0, 0.0, 0.0);
        assert!(!plain.contains("group"), "{plain}");
        let m: Manifest = toml::from_str(&plain).unwrap();
        m.validate().unwrap();
        assert!(m.shots[0].group.is_none());
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
        let block = preset_toml(
            "here", "", 53.2194, 6.5665, 420.0, -18.0, 35.0, 9.5, 0.1, 0.2,
        );
        let m: Manifest = toml::from_str(&block).unwrap();
        m.validate().unwrap();
        assert_eq!(m.shots[0].at[2], 420.0);
        assert_eq!(m.shots[0].look.unwrap()[0], -18.0);
    }
}

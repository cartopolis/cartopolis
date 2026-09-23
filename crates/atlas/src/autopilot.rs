//! The autonomous lane's throttle, read and written for the console. Ported from
//! cartopy's `server/autopilot.ts`.
//!
//! The autopilot (`tools/autopilot.ts`, in a container on this host) re-reads its
//! JSON config on every tick — the file *is* its control surface, so steering it
//! needs no restart and no API on its side. Writes are atomic (a temp file in the
//! same directory, then a rename): the autopilot may read at any moment, and a torn
//! parse there falls back to everything off, which reads as the lane dying.
//!
//! **Two writers.** The autopilot writes this file too — the plan a planning session
//! posted, and the issue number stamped onto a plan item once filed — through the
//! same read-merge-rename. The plan's ordering, wording and membership are the
//! console's; `issue` is the autopilot's.
//!
//! A hand-synced seam: the fields below must match `Config` in `tools/autopilot.ts`.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Long enough for a paragraph of intent, short enough for a ticket to read as one.
const MAX_FOCUS_CHARS: usize = 2000;
/// A plan is a queue a person reads. Past twenty nobody reorders it; they rewrite it.
const MAX_PLAN_ITEMS: usize = 20;
/// JavaScript's `Number.MAX_SAFE_INTEGER`, the ceiling the other writer uses.
const MAX_SAFE: i64 = 9_007_199_254_740_991;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutopilotConfig {
    /// File new tickets at all. Shipping what is already in flight continues regardless.
    pub enabled: bool,
    /// An area to work in, quoted into every ticket. Empty hands the queue back to
    /// the frontier table in the direction document.
    pub focus: String,
    /// The ordered queue under that focus, filed top down.
    pub focus_plan: Vec<PlanItem>,
    /// Feature tickets being built at once.
    pub max_open: i64,
    /// Bumped to ask for a fresh plan.
    pub plan_revision: i64,
    /// QA tickets open at once.
    pub qa_max_open: i64,
    /// QA passes a day.
    pub qa_per_day: i64,
    /// A retrospective every N days; 0 is never.
    pub retro_every_days: i64,
    /// Merge and deploy finished tickets, or leave them for review.
    pub ship: bool,
    /// New feature tickets a day.
    pub tickets_per_day: i64,
}

/// One piece of work under the focus. Matches `PlanItem` in `tools/autopilot.ts`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PlanItem {
    /// Stable key. The autopilot's ticket marker is `autopilot:focus-<id>`.
    pub id: String,
    /// The ticket it became, or 0. The autopilot writes this.
    pub issue: i64,
    pub note: String,
    /// One line; it becomes the ticket's title.
    pub title: String,
}

// ---- JavaScript coercions, because the other writer and the console are JS ----

/// `Number(value)`.
fn js_number(value: &Value) -> f64 {
    match value {
        Value::Null => 0.0,
        Value::Bool(b) => f64::from(u8::from(*b)),
        Value::Number(n) => n.as_f64().unwrap_or(f64::NAN),
        Value::String(s) => {
            let t = s.trim();
            if t.is_empty() {
                0.0
            } else {
                t.parse().unwrap_or(f64::NAN)
            }
        }
        _ => f64::NAN,
    }
}

/// `Boolean(value)`.
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0 && !f.is_nan()),
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}

/// `String(value ?? '')`.
fn js_string(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn clamp_int(value: Option<&Value>, lo: i64, hi: i64, fallback: i64) -> i64 {
    let n = value.map_or(f64::NAN, js_number).floor();
    if n.is_finite() {
        (n.clamp(lo as f64, hi as f64)) as i64
    } else {
        fallback
    }
}

fn take_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// Replace every run of characters outside `keep` with one `-`, then drop one
/// leading and one trailing `-` — the regexes the TypeScript used.
fn slugify(s: &str, keep: impl Fn(char) -> bool) -> String {
    let mut out = String::new();
    let mut in_run = false;
    for c in s.chars() {
        if keep(c) {
            out.push(c);
            in_run = false;
        } else if !in_run {
            out.push('-');
            in_run = true;
        }
    }
    let out = out.strip_prefix('-').unwrap_or(&out);
    out.strip_suffix('-').unwrap_or(out).to_string()
}

/// A slug of the title, unique against the ids already taken.
fn fresh_id(title: &str, seen: &HashSet<String>) -> String {
    let slug = take_chars(
        &slugify(&title.to_lowercase(), |c| {
            c.is_ascii_lowercase() || c.is_ascii_digit()
        }),
        40,
    );
    let slug = if slug.is_empty() {
        "item".to_string()
    } else {
        slug
    };
    let mut id = slug.clone();
    let mut n = 2;
    while seen.contains(&id) {
        id = format!("{slug}-{n}");
        n += 1;
    }
    id
}

/// Whatever is in the file, as a plan. An untitled row is dropped (the console
/// adds one on Add and a title is what becomes a ticket); an id is assigned when
/// missing or repeated, because two rows sharing one would be one ticket filed for
/// two pieces of work.
fn plan_items(raw: Option<&Value>) -> Vec<PlanItem> {
    let Some(Value::Array(entries)) = raw else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for entry in entries {
        let Value::Object(e) = entry else { continue };
        let field = |k: &str| e.get(k).map_or(String::new(), js_string);
        let title = take_chars(
            &field("title")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" "),
            200,
        );
        if title.is_empty() {
            continue;
        }
        let given = take_chars(
            &slugify(&field("id").to_lowercase(), |c| {
                c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'
            }),
            48,
        );
        let id = if !given.is_empty() && !seen.contains(&given) {
            given
        } else {
            fresh_id(&title, &seen)
        };
        seen.insert(id.clone());
        out.push(PlanItem {
            id,
            issue: clamp_int(e.get("issue"), 0, MAX_SAFE, 0),
            note: take_chars(field("note").trim(), 1000),
            title,
        });
        if out.len() >= MAX_PLAN_ITEMS {
            break;
        }
    }
    out
}

/// Whatever is in the file, normalised. Unknown fields are dropped and numbers
/// clamped; the autopilot clamps too, so nothing written here can put it in a
/// state it refuses.
fn normalise(raw: &Map<String, Value>) -> AutopilotConfig {
    AutopilotConfig {
        enabled: raw.get("enabled").is_some_and(truthy),
        focus: take_chars(
            raw.get("focus").map_or(String::new(), js_string).trim(),
            MAX_FOCUS_CHARS,
        ),
        focus_plan: plan_items(raw.get("focusPlan")),
        max_open: clamp_int(raw.get("maxOpen"), 1, 10, 2),
        plan_revision: clamp_int(raw.get("planRevision"), 0, MAX_SAFE, 0),
        qa_max_open: clamp_int(raw.get("qaMaxOpen"), 1, 10, 3),
        qa_per_day: clamp_int(raw.get("qaPerDay"), 0, 24, 0),
        retro_every_days: clamp_int(raw.get("retroEveryDays"), 0, 30, 0),
        ship: raw.get("ship").is_some_and(truthy),
        tickets_per_day: clamp_int(raw.get("ticketsPerDay"), 0, 24, 0),
    }
}

/// The incoming plan with every row that has already become a ticket held to what
/// the file says. A filed item is being built against the wording that was filed,
/// so it cannot be reworded or removed here — not as a check on the console but on
/// *a* console: a tab left open all evening saves the list as it was before any of
/// it was filed. Position is not held. `issue` always comes from the file by id.
fn hold_filed_items(current: &[PlanItem], incoming: Vec<PlanItem>) -> Vec<PlanItem> {
    let filed: HashMap<&str, &PlanItem> = current
        .iter()
        .filter(|i| i.issue > 0)
        .map(|i| (i.id.as_str(), i))
        .collect();
    let stamps: HashMap<&str, i64> = current.iter().map(|i| (i.id.as_str(), i.issue)).collect();
    let mut out: Vec<PlanItem> = incoming
        .into_iter()
        .map(|item| match filed.get(item.id.as_str()) {
            Some(locked) => (*locked).clone(),
            None => PlanItem {
                issue: stamps.get(item.id.as_str()).copied().unwrap_or(0),
                ..item
            },
        })
        .collect();
    for (at, item) in current.iter().enumerate() {
        if item.issue > 0 && !out.iter().any(|o| o.id == item.id) {
            out.insert(at.min(out.len()), item.clone());
        }
    }
    out
}

/// Fails when the file is absent or unreadable — a host without the lane.
pub fn read(path: &Path) -> Result<AutopilotConfig, String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    match serde_json::from_str::<Value>(&text).map_err(|e| e.to_string())? {
        Value::Object(raw) => Ok(normalise(&raw)),
        _ => Err("not a JSON object".into()),
    }
}

/// Merge the given fields over what the file holds now and write the result.
/// Reading first refuses to conjure a config on a host with no autopilot, and it
/// means a PATCH naming one field changes one field.
pub fn write(path: &Path, patch: &Map<String, Value>) -> Result<AutopilotConfig, String> {
    let current = read(path)?;
    let Value::Object(mut merged) = serde_json::to_value(&current).map_err(|e| e.to_string())?
    else {
        unreachable!("a struct serialises to an object")
    };
    for (key, value) in patch {
        if merged.contains_key(key) {
            merged.insert(key.clone(), value.clone());
        }
    }
    let mut next = normalise(&merged);
    if patch.contains_key("focusPlan") {
        // Through `plan_items` again so the twenty-item bound holds after a filed
        // row has been put back into a list that tried to drop it.
        let held = hold_filed_items(&current.focus_plan, next.focus_plan);
        next.focus_plan = plan_items(Some(
            &serde_json::to_value(held).map_err(|e| e.to_string())?,
        ));
    }
    let dir = path.parent().ok_or("the config path has no directory")?;
    let tmp = dir.join(format!(".autopilot.json.{}.tmp", std::process::id()));
    let body = serde_json::to_string_pretty(&next).map_err(|e| e.to_string())? + "\n";
    std::fs::write(&tmp, body).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())?;
    Ok(next)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn obj(v: Value) -> Map<String, Value> {
        match v {
            Value::Object(m) => m,
            _ => panic!(),
        }
    }

    fn file(dir: &tempfile::TempDir, name: &str, contents: Value) -> std::path::PathBuf {
        let p = dir.path().join(name);
        std::fs::write(&p, contents.to_string()).unwrap();
        p
    }

    #[test]
    fn an_absent_file_is_a_host_without_the_lane() {
        let dir = tempfile::tempdir().unwrap();
        let nowhere = dir.path().join("nowhere.json");
        assert!(read(&nowhere).is_err());
        assert!(write(&nowhere, &obj(json!({"qaPerDay": 5}))).is_err());
        assert!(!nowhere.exists());
    }

    #[test]
    fn a_patch_names_one_field_and_changes_one_field() {
        let dir = tempfile::tempdir().unwrap();
        let f = file(
            &dir,
            "a.json",
            json!({"enabled": true, "maxOpen": 2, "qaMaxOpen": 3, "qaPerDay": 12,
                   "retroEveryDays": 3, "ship": true, "ticketsPerDay": 6}),
        );
        let next = write(&f, &obj(json!({"qaPerDay": 4}))).unwrap();
        assert_eq!(next.qa_per_day, 4);
        assert_eq!(next.tickets_per_day, 6);
        assert!(next.ship);
        assert_eq!(read(&f).unwrap(), next);
        // The file is what the other writer parses: camelCase, pretty, newline.
        let text = std::fs::read_to_string(&f).unwrap();
        assert!(text.contains("\"qaPerDay\": 4") && text.ends_with("}\n"));
    }

    #[test]
    fn out_of_range_and_unknown_input_cannot_reach_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let f = file(&dir, "a.json", json!({"enabled": true}));
        let next = write(
            &f,
            &obj(json!({"qaPerDay": 999, "ticketsPerDay": -5, "wholeNewField": "no", "maxOpen": "4"})),
        )
        .unwrap();
        assert_eq!(next.qa_per_day, 24);
        assert_eq!(next.tickets_per_day, 0);
        assert_eq!(next.max_open, 4);
        assert!(!std::fs::read_to_string(&f)
            .unwrap()
            .contains("wholeNewField"));
    }

    #[test]
    fn a_file_from_before_the_focus_reads_as_no_focus() {
        let dir = tempfile::tempdir().unwrap();
        let f = file(&dir, "old.json", json!({"enabled": true, "maxOpen": 2}));
        let c = read(&f).unwrap();
        assert_eq!(c.focus, "");
        assert!(c.focus_plan.is_empty());
        assert_eq!(c.plan_revision, 0);
        assert_eq!(c.qa_max_open, 3);
    }

    #[test]
    fn ids_are_assigned_and_never_shared_and_untitled_rows_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let f = file(&dir, "a.json", json!({}));
        let next = write(
            &f,
            &obj(json!({
                "focus": "  Focus on the Steam Deck runner  ",
                "focusPlan": [{"title": "Pin the GPU driver"}, {"title": "Pin the  GPU\tdriver"},
                              {"title": "   "}, {"note": "orphan"}, "nonsense"]
            })),
        )
        .unwrap();
        assert_eq!(next.focus, "Focus on the Steam Deck runner");
        let ids: Vec<_> = next.focus_plan.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(ids, ["pin-the-gpu-driver", "pin-the-gpu-driver-2"]);
        assert_eq!(next.focus_plan[1].title, "Pin the GPU driver");
        assert!(next.focus_plan.iter().all(|i| i.issue == 0));
    }

    #[test]
    fn the_plan_is_bounded_in_length_and_in_each_field() {
        let dir = tempfile::tempdir().unwrap();
        let f = file(&dir, "a.json", json!({}));
        let items: Vec<Value> = (0..40)
            .map(|n| json!({"title": format!("item {n}"), "note": "n".repeat(4000)}))
            .collect();
        let next = write(
            &f,
            &obj(json!({"focus": "f".repeat(5000), "focusPlan": items})),
        )
        .unwrap();
        assert_eq!(next.focus.len(), 2000);
        assert_eq!(next.focus_plan.len(), 20);
        assert_eq!(next.focus_plan[0].note.len(), 1000);
    }

    #[test]
    fn a_filed_item_cannot_be_reworded_removed_or_restamped() {
        let dir = tempfile::tempdir().unwrap();
        let f = file(
            &dir,
            "locked.json",
            json!({"enabled": true, "focusPlan": [
                {"id": "shipped", "issue": 12, "note": "Where it lands", "title": "Being built"},
                {"id": "waiting", "issue": 0, "note": "", "title": "Still a plan"}
            ]}),
        );
        let next = write(
            &f,
            &obj(json!({"focusPlan": [{"id": "waiting", "issue": 0, "title": "Reworded, which is allowed"}]})),
        )
        .unwrap();
        let rows: Vec<_> = next
            .focus_plan
            .iter()
            .map(|i| (i.id.as_str(), i.title.as_str(), i.note.as_str(), i.issue))
            .collect();
        assert_eq!(
            rows,
            [
                ("shipped", "Being built", "Where it lands", 12),
                ("waiting", "Reworded, which is allowed", "", 0)
            ]
        );
        let after = write(
            &f,
            &obj(json!({"focusPlan": [{"id": "shipped", "issue": 999, "note": "no", "title": "Renamed"}]})),
        )
        .unwrap();
        assert_eq!(after.focus_plan.len(), 1);
        assert_eq!(after.focus_plan[0].title, "Being built");
        assert_eq!(after.focus_plan[0].issue, 12);
    }

    #[test]
    fn the_stamp_comes_from_the_file_not_from_the_console() {
        let dir = tempfile::tempdir().unwrap();
        let f = file(&dir, "a.json", json!({"enabled": true}));
        write(
            &f,
            &obj(json!({"focusPlan": [{"id": "one", "issue": 0, "title": "One"}]})),
        )
        .unwrap();
        // The autopilot stamps it behind the open tab's back.
        let mut raw: Value = serde_json::from_str(&std::fs::read_to_string(&f).unwrap()).unwrap();
        raw["focusPlan"][0]["issue"] = json!(77);
        std::fs::write(&f, raw.to_string()).unwrap();
        let next = write(
            &f,
            &obj(json!({"focusPlan": [{"id": "one", "issue": 0, "title": "One"}]})),
        )
        .unwrap();
        assert_eq!(next.focus_plan[0].issue, 77);
        // And reordering keeps each row's stamp.
        let mut raw: Value = serde_json::from_str(&std::fs::read_to_string(&f).unwrap()).unwrap();
        raw["focusPlan"] = json!([
            {"id": "first", "issue": 41, "note": "", "title": "First"},
            {"id": "second", "issue": 0, "note": "", "title": "Second"}
        ]);
        std::fs::write(&f, raw.to_string()).unwrap();
        let next = write(
            &f,
            &obj(json!({"focusPlan": [{"id": "second", "title": "Second"}, {"id": "first", "title": "First"}]})),
        )
        .unwrap();
        let rows: Vec<_> = next
            .focus_plan
            .iter()
            .map(|i| (i.id.as_str(), i.issue))
            .collect();
        assert_eq!(rows, [("second", 0), ("first", 41)]);
    }
}

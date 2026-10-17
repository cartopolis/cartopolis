//! Writing a [`HumanPack`] from the sources: MPFB's `data/` directory, the
//! MakeHuman system assets, and motion-capture takes cut into clips (`mocap`).
//!
//! The formats are MakeHuman's (`docs/fileformats/` in the mpfb2 repository)
//! and are parsed here from their descriptions, not from MPFB's code, which is
//! GPL; only the data is CC0.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::{
    macros, Bone, Face, HumanPack, Phenotype, Proxy, ProxyKind, ProxyRef, Skin, Target,
    BODY_VERTICES, PACK_VERSION,
};

pub type Result<T> = std::result::Result<T, String>;

fn read(path: &Path) -> Result<Vec<u8>> {
    std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))
}

fn read_text(path: &Path) -> Result<String> {
    String::from_utf8(read(path)?).map_err(|e| format!("{}: {e}", path.display()))
}

fn read_json(path: &Path) -> Result<Value> {
    serde_json::from_slice(&read(path)?).map_err(|e| format!("{}: {e}", path.display()))
}

// ── OBJ ──────────────────────────────────────────────────────────────────────

pub struct Obj {
    pub vertices: Vec<[f32; 3]>,
    pub uvs: Vec<[f32; 2]>,
    pub faces: Vec<Face>,
}

/// `v`, `vt` and `f` (`v/vt` or `v/vt/vn`, triangles or quads; larger
/// polygons are fanned into them). Everything else is skipped.
pub fn parse_obj(text: &str) -> Result<Obj> {
    let mut obj = Obj {
        vertices: Vec::new(),
        uvs: Vec::new(),
        faces: Vec::new(),
    };
    for (n, line) in text.lines().enumerate() {
        let mut it = line.split_whitespace();
        let nums = |it: std::str::SplitWhitespace| -> Result<Vec<f32>> {
            it.map(|s| {
                s.parse::<f32>()
                    .map_err(|e| format!("obj line {}: {e}", n + 1))
            })
            .collect()
        };
        match it.next() {
            Some("v") => {
                let v = nums(it)?;
                obj.vertices.push([v[0], v[1], v[2]]);
            }
            Some("vt") => {
                let v = nums(it)?;
                obj.uvs.push([v[0], v[1]]);
            }
            Some("f") => {
                let mut corners = Vec::new();
                for c in it {
                    let mut p = c.split('/');
                    let v: u32 = p
                        .next()
                        .and_then(|s| s.parse().ok())
                        .ok_or_else(|| format!("obj line {}: bad face corner {c:?}", n + 1))?;
                    let uv: u32 = p.next().and_then(|s| s.parse().ok()).unwrap_or(v);
                    corners.push((v - 1, uv - 1));
                }
                for k in 1..corners.len().saturating_sub(1) {
                    // A quad stays a quad; anything larger becomes a fan.
                    if corners.len() == 4 && k == 1 {
                        obj.faces.push(Face {
                            v: corners.map_to(|c| c.0),
                            uv: corners.map_to(|c| c.1),
                        });
                        break;
                    }
                    let tri = [corners[0], corners[k], corners[k + 1]];
                    obj.faces.push(Face {
                        v: [tri[0].0, tri[1].0, tri[2].0, Face::NONE],
                        uv: [tri[0].1, tri[1].1, tri[2].1, Face::NONE],
                    });
                }
            }
            _ => {}
        }
    }
    Ok(obj)
}

trait MapTo {
    fn map_to(&self, f: impl Fn(&(u32, u32)) -> u32) -> [u32; 4];
}

impl MapTo for Vec<(u32, u32)> {
    fn map_to(&self, f: impl Fn(&(u32, u32)) -> u32) -> [u32; 4] {
        [f(&self[0]), f(&self[1]), f(&self[2]), f(&self[3])]
    }
}

// ── Targets ──────────────────────────────────────────────────────────────────

/// A `.target` file: lines of `index dx dy dz`; blank lines and `#` comments
/// skipped.
pub fn parse_target(name: &str, text: &str) -> Result<Target> {
    let mut offsets = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with('"') {
            continue;
        }
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() < 4 {
            continue;
        }
        let num = |s: &str| {
            s.parse::<f32>()
                .map_err(|e| format!("{name}: {e} in {line:?}"))
        };
        let v: u32 = f[0]
            .parse()
            .map_err(|e| format!("{name}: {e} in {line:?}"))?;
        offsets.push((v, [num(f[1])?, num(f[2])?, num(f[3])?]));
    }
    Ok(Target::new(name, offsets))
}

fn read_target(targets_dir: &Path, name: &str) -> Result<Target> {
    let plain = targets_dir.join(format!("{name}.target"));
    let gz = targets_dir.join(format!("{name}.target.gz"));
    let text = if gz.exists() {
        let mut s = String::new();
        flate2::read::GzDecoder::new(&read(&gz)?[..])
            .read_to_string(&mut s)
            .map_err(|e| format!("{}: {e}", gz.display()))?;
        s
    } else {
        read_text(&plain)?
    };
    parse_target(name, &text)
}

// ── Vertex groups, rig, weights ──────────────────────────────────────────────

/// `basemesh_vertex_groups.json`: name → inclusive `[start, end]` ranges.
pub fn vertex_groups(json: &Value) -> Result<HashMap<String, Vec<u32>>> {
    let obj = json.as_object().ok_or("vertex groups: not an object")?;
    let mut out = HashMap::new();
    for (name, ranges) in obj {
        let mut v = Vec::new();
        for r in ranges.as_array().ok_or("vertex groups: not a list")? {
            let a = r[0].as_u64().ok_or("vertex groups: bad range")? as u32;
            let b = r[1].as_u64().ok_or("vertex groups: bad range")? as u32;
            v.extend(a..=b);
        }
        out.insert(name.clone(), v);
    }
    Ok(out)
}

fn joint_ref(spec: &Value, groups: &HashMap<String, Vec<u32>>, bone: &str) -> Result<Vec<u32>> {
    let strategy = spec["strategy"].as_str().unwrap_or("");
    let list = |v: &Value| -> Vec<u32> {
        v.as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_u64().map(|x| x as u32))
                    .collect()
            })
            .unwrap_or_default()
    };
    match strategy {
        "CUBE" => {
            let cube = spec["cube_name"]
                .as_str()
                .ok_or(format!("{bone}: CUBE without cube_name"))?;
            groups
                .get(cube)
                .cloned()
                .ok_or(format!("{bone}: no vertex group {cube}"))
        }
        "VERTEX" => Ok(vec![spec["vertex_index"]
            .as_u64()
            .ok_or(format!("{bone}: VERTEX without vertex_index"))?
            as u32]),
        "MEAN" => Ok(list(&spec["vertex_indices"])),
        other => Err(format!("{bone}: joint strategy {other:?} is not supported")),
    }
}

/// The rig, parents before children. Accepts both rig layouts: bones at the top
/// level (what MPFB ships) or under `"bones"` (what its docs describe).
pub fn parse_rig(
    rig: &Value,
    groups: &HashMap<String, Vec<u32>>,
    tpose: &HashMap<String, [f32; 3]>,
) -> Result<Vec<Bone>> {
    let bones = rig.get("bones").unwrap_or(rig);
    let obj = bones.as_object().ok_or("rig: not an object")?;
    let mut pending: Vec<(&String, &Value)> = obj
        .iter()
        .filter(|(_, v)| v.get("head").is_some())
        .collect();
    let mut out: Vec<Bone> = Vec::new();
    while !pending.is_empty() {
        let before = pending.len();
        pending.retain(|(name, b)| {
            let parent = b["parent"].as_str().unwrap_or("");
            let parent_ix = if parent.is_empty() {
                None
            } else {
                match out.iter().position(|o| o.name == parent) {
                    Some(i) => Some(i as u16),
                    None => return true,
                }
            };
            out.push(Bone {
                name: (*name).clone(),
                parent: parent_ix,
                head: Vec::new(),
                tail: Vec::new(),
                roll: b["roll"].as_f64().unwrap_or(0.0) as f32,
                tpose: tpose.get(name.as_str()).copied().unwrap_or_default(),
            });
            false
        });
        if pending.len() == before {
            return Err(format!(
                "rig: {} bones have parents that are not in the rig",
                pending.len()
            ));
        }
    }
    for b in &mut out {
        let spec = &obj[&b.name];
        b.head = joint_ref(&spec["head"], groups, &b.name)?;
        b.tail = joint_ref(&spec["tail"], groups, &b.name)?;
    }
    Ok(out)
}

/// `weights.*.json`: `{"weights": {bone: [[vertex, weight], …]}}`, inverted to
/// per-vertex lists.
pub fn parse_weights(
    json: &Value,
    bones: &[Bone],
    vertices: usize,
) -> Result<Vec<Vec<(u16, f32)>>> {
    let groups = json["weights"]
        .as_object()
        .ok_or("weights: no \"weights\" object")?;
    let mut out = vec![Vec::new(); vertices];
    for (name, list) in groups {
        let Some(bone) = bones.iter().position(|b| &b.name == name) else {
            continue;
        };
        for pair in list.as_array().ok_or("weights: not a list")? {
            let v = pair[0].as_u64().ok_or("weights: bad vertex")? as usize;
            let w = pair[1].as_f64().ok_or("weights: bad weight")? as f32;
            if v < vertices && w > 0.0 {
                out[v].push((bone as u16, w));
            }
        }
    }
    Ok(out)
}

// ── Proxies ──────────────────────────────────────────────────────────────────

/// The parts of a `.mhclo` the fitting needs.
pub struct Mhclo {
    pub obj_file: Option<String>,
    pub material: Option<String>,
    pub refs: Vec<ProxyRef>,
    pub scale: [Option<(u32, u32, f32)>; 3],
    pub delete_verts: Vec<u32>,
}

pub fn parse_mhclo(text: &str) -> Result<Mhclo> {
    let mut out = Mhclo {
        obj_file: None,
        material: None,
        refs: Vec::new(),
        scale: [None; 3],
        delete_verts: Vec::new(),
    };
    #[derive(PartialEq)]
    enum Block {
        None,
        Verts,
        Delete,
    }
    let mut block = Block::None;
    // `a - b` in a delete list can straddle a line break.
    let mut range_from: Option<u32> = None;
    let mut last: Option<u32> = None;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        let f: Vec<&str> = line.split_whitespace().collect();
        let numeric = !f.is_empty() && f[0].parse::<f64>().is_ok();
        match block {
            Block::Verts if numeric => {
                let n: Vec<f32> = f.iter().filter_map(|s| s.parse().ok()).collect();
                let r = if n.len() >= 9 {
                    ProxyRef {
                        v: [n[0] as u32, n[1] as u32, n[2] as u32],
                        w: [n[3], n[4], n[5]],
                        offset: [n[6], n[7], n[8]],
                    }
                } else {
                    ProxyRef {
                        v: [n[0] as u32, 0, 0],
                        w: [1.0, 0.0, 0.0],
                        offset: [0.0; 3],
                    }
                };
                out.refs.push(r);
                continue;
            }
            // `material` and the weight lines may sit inside the verts block
            // without ending it.
            Block::Verts
                if f.first()
                    .is_some_and(|k| *k == "material" || k.starts_with("vertexboneweights")) => {}
            Block::Delete if !f.is_empty() && (numeric || f[0] == "-") => {
                for tok in f {
                    if tok == "-" {
                        range_from = last;
                        continue;
                    }
                    let Ok(v) = tok.parse::<u32>() else { continue };
                    match range_from.take() {
                        Some(a) => out.delete_verts.extend(a + 1..=v),
                        None => out.delete_verts.push(v),
                    }
                    last = Some(v);
                }
                continue;
            }
            _ => block = Block::None,
        }
        match f.first().copied() {
            Some("verts") => block = Block::Verts,
            Some("delete_verts") => block = Block::Delete,
            Some("obj_file") => out.obj_file = f.get(1).map(|s| s.to_string()),
            Some("material") => out.material = f.get(1).map(|s| s.to_string()),
            Some(k @ ("x_scale" | "y_scale" | "z_scale")) if f.len() >= 4 => {
                let axis = match k {
                    "x_scale" => 0,
                    "y_scale" => 1,
                    _ => 2,
                };
                if let (Ok(a), Ok(b), Ok(d)) = (f[1].parse(), f[2].parse(), f[3].parse()) {
                    out.scale[axis] = Some((a, b, d));
                }
            }
            _ => {}
        }
    }
    Ok(out)
}

/// `diffuseTexture` and whether it is cut out, from a `.mhmat`.
pub fn parse_mhmat(text: &str) -> (Option<String>, bool) {
    let mut texture = None;
    let mut alpha = false;
    for line in text.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        match f.as_slice() {
            ["diffuseTexture", t, ..] => texture = Some(t.to_string()),
            ["transparent", v, ..] | ["alphaToCoverage", v, ..] => {
                alpha |= v.eq_ignore_ascii_case("true")
            }
            _ => {}
        }
    }
    (texture, alpha)
}

/// One proxy to put in a pack.
pub struct ProxySource {
    pub kind: ProxyKind,
    /// The `.mhclo`.
    pub mhclo: PathBuf,
    /// A material other than the one the `.mhclo` names (the eyes keep theirs
    /// apart from the mesh).
    pub material: Option<PathBuf>,
}

// ── The pack ─────────────────────────────────────────────────────────────────

/// The shape targets a pack carries.
pub enum TargetSet {
    /// Only what these bodies need: a pack for fixed looks.
    Bodies(Vec<Phenotype>),
    /// Every macro target (`macrodetails/`, `breast/`) whose name contains
    /// none of `skip` — what the sliders need to make any body.
    Macros { skip: Vec<String> },
}

/// Every macro target under `targets/`, by name (`macrodetails/height/…`).
fn macro_target_names(targets: &Path) -> Result<Vec<String>> {
    fn walk(dir: &Path, rel: &str, out: &mut Vec<String>) -> Result<()> {
        let entries = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let path = e.path();
            if path.is_dir() {
                walk(&path, &format!("{rel}{name}/"), out)?;
            } else if let Some(stem) = name
                .strip_suffix(".target.gz")
                .or(name.strip_suffix(".target"))
            {
                out.push(format!("{rel}{stem}"));
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    for top in ["macrodetails", "breast"] {
        walk(&targets.join(top), &format!("{top}/"), &mut out)?;
    }
    Ok(out)
}

/// Everything a pack is built from.
pub struct Sources {
    /// MPFB's `src/mpfb/data`.
    pub mpfb_data: PathBuf,
    pub skins: Vec<(String, PathBuf)>,
    pub proxies: Vec<ProxySource>,
    /// The clips, each cut from a take.
    pub cuts: Vec<crate::mocap::Cut>,
    /// Which shape targets to carry.
    pub targets: TargetSet,
    /// Where to copy the textures; the pack names them relative to it.
    pub textures_out: PathBuf,
}

fn copy_texture(src: &Path, out_dir: &Path, prefix: &str) -> Result<String> {
    let file = src
        .file_name()
        .ok_or_else(|| format!("{}: no file name", src.display()))?
        .to_string_lossy()
        .into_owned();
    let name = if file.starts_with(prefix) {
        file
    } else {
        format!("{prefix}_{file}")
    };
    std::fs::create_dir_all(out_dir).map_err(|e| format!("{}: {e}", out_dir.display()))?;
    std::fs::copy(src, out_dir.join(&name)).map_err(|e| format!("{}: {e}", src.display()))?;
    Ok(name)
}

pub fn build_pack(src: &Sources) -> Result<HumanPack> {
    let data = &src.mpfb_data;
    let base = parse_obj(&read_text(&data.join("3dobjs/base.obj"))?)?;
    let groups = vertex_groups(&read_json(
        &data.join("mesh_metadata/basemesh_vertex_groups.json"),
    )?)?;
    let tpose_json = read_json(&data.join("poses/game_engine_fk/t-pose.json"))?;
    let tpose: HashMap<String, [f32; 3]> = tpose_json["bone_rotations"]
        .as_object()
        .ok_or("t-pose: no bone_rotations")?
        .iter()
        .filter_map(|(k, v)| {
            let a = v.as_array()?;
            Some((
                k.clone(),
                [
                    a[0].as_f64()? as f32,
                    a[1].as_f64()? as f32,
                    a[2].as_f64()? as f32,
                ],
            ))
        })
        .collect();
    let bones = parse_rig(
        &read_json(&data.join("rigs/standard/rig.game_engine.json"))?,
        &groups,
        &tpose,
    )?;
    let weights = parse_weights(
        &read_json(&data.join("rigs/standard/weights.game_engine.json"))?,
        &bones,
        base.vertices.len(),
    )?;

    let mut wanted: Vec<String> = match &src.targets {
        TargetSet::Bodies(bodies) => bodies
            .iter()
            .flat_map(macros::macro_targets)
            .map(|(n, _)| n)
            .collect(),
        TargetSet::Macros { skip } => macro_target_names(&data.join("targets"))?
            .into_iter()
            .filter(|n| !skip.iter().any(|s| n.contains(s.as_str())))
            .collect(),
    };
    wanted.sort();
    wanted.dedup();
    let targets = wanted
        .iter()
        .map(|n| read_target(&data.join("targets"), n))
        .collect::<Result<Vec<_>>>()?;

    let body_faces = base
        .faces
        .iter()
        .filter(|f| {
            f.v.iter()
                .all(|&v| v == Face::NONE || (v as usize) < BODY_VERTICES)
        })
        .copied()
        .collect();

    let mut proxies = Vec::new();
    for p in &src.proxies {
        let dir = p.mhclo.parent().unwrap_or(Path::new("."));
        let clo = parse_mhclo(&read_text(&p.mhclo)?)?;
        let obj_file = clo
            .obj_file
            .clone()
            .ok_or_else(|| format!("{}: no obj_file", p.mhclo.display()))?;
        let obj = parse_obj(&read_text(&dir.join(&obj_file))?)?;
        if obj.vertices.len() != clo.refs.len() {
            return Err(format!(
                "{}: {} vertices in the obj, {} in the fitting",
                p.mhclo.display(),
                obj.vertices.len(),
                clo.refs.len()
            ));
        }
        let material = p
            .material
            .clone()
            .or_else(|| clo.material.as_ref().map(|m| dir.join(m)));
        let (texture, alpha) = match &material {
            Some(m) => {
                let (tex, alpha) = parse_mhmat(&read_text(m)?);
                let mdir = m.parent().unwrap_or(Path::new("."));
                let name = p
                    .mhclo
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let tex = tex
                    .map(|t| copy_texture(&mdir.join(t), &src.textures_out, &name))
                    .transpose()?;
                (tex, alpha)
            }
            None => (None, false),
        };
        proxies.push(Proxy {
            name: p
                .mhclo
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default(),
            kind: p.kind,
            refs: clo.refs,
            scale: clo.scale,
            uvs: obj.uvs,
            faces: obj.faces,
            delete_verts: clo.delete_verts,
            texture,
            alpha,
        });
    }

    let skins = src
        .skins
        .iter()
        .map(|(name, mhmat)| {
            let (tex, _) = parse_mhmat(&read_text(mhmat)?);
            let tex = tex.ok_or_else(|| format!("{}: no diffuseTexture", mhmat.display()))?;
            let dir = mhmat.parent().unwrap_or(Path::new("."));
            Ok(Skin {
                name: name.clone(),
                texture: copy_texture(&dir.join(tex), &src.textures_out, name)?,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    // Each take is parsed once, however many clips are cut from it.
    let mut takes: HashMap<PathBuf, crate::mocap::Motion> = HashMap::new();
    let mut clips = Vec::with_capacity(src.cuts.len());
    for cut in &src.cuts {
        if !takes.contains_key(&cut.file) {
            takes.insert(
                cut.file.clone(),
                crate::mocap::parse_bvh(&read_text(&cut.file)?)?,
            );
        }
        clips.push(crate::mocap::clip_from_motion(
            &takes[&cut.file],
            cut,
            &bones,
            30.0,
        )?);
    }

    Ok(HumanPack {
        version: PACK_VERSION,
        vertices: base.vertices,
        uvs: base.uvs,
        body_faces,
        targets,
        bones,
        weights,
        proxies,
        skins,
        clips,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_delete_list_reads_ranges_across_lines() {
        let clo =
            parse_mhclo("verts 0\n1 2 3 0.5 0.25 0.25 0 0 0\n7\ndelete_verts\n3 5 - 8\n10 -\n12\n")
                .unwrap();
        assert_eq!(clo.refs.len(), 2);
        assert_eq!(clo.refs[1].v[0], 7);
        assert_eq!(clo.delete_verts, vec![3, 5, 6, 7, 8, 10, 11, 12]);
    }

    #[test]
    fn an_obj_keeps_quads_and_fans_the_rest() {
        let obj = parse_obj("v 0 0 0\nv 1 0 0\nv 1 1 0\nv 0 1 0\nv 0 2 0\nvt 0 0\nf 1/1 2/1 3/1 4/1\nf 1/1 2/1 3/1 4/1 5/1\n").unwrap();
        assert_eq!(obj.faces.len(), 1 + 3);
        assert_eq!(obj.faces[0].v, [0, 1, 2, 3]);
        assert_eq!(obj.faces[1].v[3], Face::NONE);
    }
}

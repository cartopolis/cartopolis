//! A kit's pieces as a GLB the override store takes (`overrides::gltf_model`).
//!
//! glTF's own axes: +X east, +Y up, −Z north, so the record's default heading 0
//! keeps it as built. One node and one mesh per piece, named after it — a solid
//! piece's name ends in `wall`, which is what `Collision::Walls` keeps solid.
//! Positions are about the pieces' footprint centre; [`write`] answers where on
//! the earth that centre is, for the record's placement.

use std::collections::BTreeMap;

use serde_json::json;

use super::kit::{Kit, Piece};

/// The GLB bytes, and the `(lat, lng)` its footprint centre belongs at.
pub fn write(kit: &Kit, pieces: &[Piece]) -> Result<(Vec<u8>, (f64, f64)), String> {
    let all = pieces.iter().flat_map(|p| p.p.iter());
    let (mut lo, mut hi) = ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]);
    for q in all {
        for k in 0..3 {
            lo[k] = lo[k].min(q[k]);
            hi[k] = hi[k].max(q[k]);
        }
    }
    if !lo[0].is_finite() {
        return Err("the kit has no triangles".into());
    }
    let centre = [(lo[0] + hi[0]) / 2.0, (lo[1] + hi[1]) / 2.0];
    let plane = crate::pipeline::models::Plane::at(kit.origin.0, kit.origin.1);
    let at = plane.to_lat_lng(centre);

    let mut bin: Vec<u8> = Vec::new();
    let mut views = Vec::new();
    let mut accessors = Vec::new();
    let mut meshes = Vec::new();
    let mut nodes = Vec::new();
    let mut materials = Vec::new();
    let mut material_index: BTreeMap<&str, usize> = BTreeMap::new();
    for (name, m) in &kit.materials {
        material_index.insert(name, materials.len());
        let c = m.rgb.map(|v| srgb_to_linear(v));
        materials.push(json!({
            "name": name,
            "pbrMetallicRoughness": {
                "baseColorFactor": [c[0], c[1], c[2], 1.0],
                "metallicFactor": 0.0, "roughnessFactor": 0.9
            }
        }));
    }
    for piece in pieces.iter().filter(|p| !p.t.is_empty()) {
        // Positions, model space: (east, up, −north) about the centre.
        let start = bin.len();
        let (mut mn, mut mx) = ([f32::INFINITY; 3], [f32::NEG_INFINITY; 3]);
        for q in &piece.p {
            let v = [
                (q[0] - centre[0]) as f32,
                (q[2] - lo[2]) as f32,
                -(q[1] - centre[1]) as f32,
            ];
            for k in 0..3 {
                mn[k] = mn[k].min(v[k]);
                mx[k] = mx[k].max(v[k]);
                bin.extend_from_slice(&v[k].to_le_bytes());
            }
        }
        views.push(json!({"buffer": 0, "byteOffset": start, "byteLength": bin.len() - start, "target": 34962}));
        accessors.push(json!({
            "bufferView": views.len() - 1, "componentType": 5126, "count": piece.p.len(),
            "type": "VEC3", "min": mn, "max": mx
        }));
        let pos = accessors.len() - 1;
        let start = bin.len();
        for t in &piece.t {
            for i in t {
                bin.extend_from_slice(&i.to_le_bytes());
            }
        }
        views.push(json!({"buffer": 0, "byteOffset": start, "byteLength": bin.len() - start, "target": 34963}));
        accessors.push(json!({
            "bufferView": views.len() - 1, "componentType": 5125, "count": piece.t.len() * 3,
            "type": "SCALAR"
        }));
        let material = *material_index
            .get(piece.material.as_str())
            .ok_or_else(|| format!("no material {:?}", piece.material))?;
        meshes.push(json!({
            "name": piece.name,
            "primitives": [{"attributes": {"POSITION": pos}, "indices": accessors.len() - 1,
                            "material": material}]
        }));
        nodes.push(json!({"name": piece.name, "mesh": meshes.len() - 1}));
    }
    let doc = json!({
        "asset": {"version": "2.0", "generator": format!("cartopolis-atlas landmark kit: {}", kit.name)},
        "scene": 0,
        "scenes": [{"nodes": (0..nodes.len()).collect::<Vec<_>>()}],
        "nodes": nodes, "meshes": meshes, "materials": materials,
        "accessors": accessors, "bufferViews": views,
        "buffers": [{"byteLength": bin.len()}]
    });
    let mut text = serde_json::to_vec(&doc).map_err(|e| e.to_string())?;
    while text.len() % 4 != 0 {
        text.push(b' ');
    }
    while bin.len() % 4 != 0 {
        bin.push(0);
    }
    let total = 12 + 8 + text.len() + 8 + bin.len();
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(b"glTF");
    out.extend_from_slice(&2u32.to_le_bytes());
    out.extend_from_slice(&(total as u32).to_le_bytes());
    out.extend_from_slice(&(text.len() as u32).to_le_bytes());
    out.extend_from_slice(b"JSON");
    out.extend_from_slice(&text);
    out.extend_from_slice(&(bin.len() as u32).to_le_bytes());
    out.extend_from_slice(b"BIN\0");
    out.extend_from_slice(&bin);
    Ok((out, at))
}

fn srgb_to_linear(v: u8) -> f32 {
    let c = f32::from(v) / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::overrides::{gltf_model, Collision, Placement};

    /// What the kit writes, the override store reads back: the triangles, the
    /// colours to within rounding, and which pieces are solid.
    #[test]
    fn the_override_store_reads_what_a_kit_writes() {
        let kit = Kit::parse(
            r#"{"name": "t", "pand": "0014100010924795", "origin": [53.2194, 6.5682],
                "materials": {"brick": {"rgb": [150, 80, 60]}, "dark": {"rgb": [20, 20, 20]}},
                "parts": [{"name": "stage", "material": "brick", "shape": {"rect": {"w": 10, "d": 10}},
                           "rings": [[0, 1], [20, 1]],
                           "openings": [{"kind": "arch", "w": 2, "from": 5, "to": 15, "depth": 0.6,
                                         "material": "dark", "count": 2}]}]}"#,
        )
        .unwrap();
        let pieces = kit.mesh().unwrap();
        let (glb, at) = write(&kit, &pieces).unwrap();
        let stats = gltf_model::inspect(&glb).unwrap();
        let tris: usize = pieces.iter().map(|p| p.t.len()).sum();
        assert_eq!(stats.triangles, tris as u64);
        let placement = Placement {
            lat: Some(at.0),
            lng: Some(at.1),
            ..Placement::default()
        };
        let square = [(53.2193, 6.5681), (53.2193, 6.5683), (53.2195, 6.5683)];
        let els =
            gltf_model::elements(&glb, &kit.pand, &placement, Collision::Walls, &square).unwrap();
        assert_eq!(els.len(), 2);
        let wall = els.iter().find(|e| e.rgb == Some([150, 80, 60])).unwrap();
        assert_eq!(wall.class, "IfcWall");
        let panel = els.iter().find(|e| e.rgb == Some([20, 20, 20])).unwrap();
        assert_eq!(panel.class, "IfcBuildingElementProxy");
        let top = wall.p.iter().map(|p| p.2).fold(0.0, f32::max);
        assert!((top - 20.0).abs() < 0.01, "{top}");
        // The footprint centre lands back on the kit's origin.
        let lat = wall.p.iter().map(|p| p.0).sum::<f64>() / wall.p.len() as f64;
        assert!((lat - 53.2194).abs() < 2e-5, "{lat}");
    }
}

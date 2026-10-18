//! Fitting a proxy (eyes, brows, hair, a garment) to a shaped body.
//!
//! MakeHuman's `.mhclo` rule: each proxy vertex is a weighted sum of three
//! base-mesh vertices plus an offset, and the offset is scaled per axis by how
//! much the body has grown since the garment was authored — measured between
//! two named base vertices against the distance recorded in the file. Done in
//! the OBJ frame (decimetres), on the morphed base mesh, before anything is
//! converted to metres.

use glam::Vec3;

use crate::{Proxy, ProxyRef};

/// The per-axis scale of `proxy`'s offsets on the body `base`.
#[must_use]
pub fn offset_scale(proxy: &Proxy, base: &[Vec3]) -> Vec3 {
    let axis = |k: usize| -> f32 {
        match proxy.scale[k] {
            Some((a, b, authored)) if authored > 0.0 => {
                let (pa, pb) = (base[a as usize], base[b as usize]);
                (pa[k] - pb[k]).abs() / authored
            }
            _ => 1.0,
        }
    };
    Vec3::new(axis(0), axis(1), axis(2))
}

/// Where one proxy vertex sits on `base`.
#[must_use]
pub fn fit_vertex(r: &ProxyRef, base: &[Vec3], scale: Vec3) -> Vec3 {
    let w = r.w();
    let mut p = Vec3::ZERO;
    for k in 0..3 {
        if w[k] != 0.0 {
            p += w[k] * base[r.v[k] as usize];
        }
    }
    p + r.offset() * scale
}

/// Every vertex of `proxy`, fitted to `base`.
#[must_use]
pub fn fit(proxy: &Proxy, base: &[Vec3]) -> Vec<Vec3> {
    let scale = offset_scale(proxy, base);
    proxy
        .refs
        .iter()
        .map(|r| fit_vertex(r, base, scale))
        .collect()
}

/// A proxy vertex's skin weights, from the base vertices it hangs on: their
/// weights blended by the reference weights. Returned unsorted and untrimmed.
#[must_use]
pub fn blend_weights(r: &ProxyRef, weights: &[Vec<(u16, f32)>]) -> Vec<(u16, f32)> {
    let mut out: Vec<(u16, f32)> = Vec::new();
    let rw = r.w();
    let total: f32 = rw.iter().map(|w| w.abs()).sum();
    if total <= 0.0 {
        return out;
    }
    for k in 0..3 {
        let w = rw[k].abs() / total;
        if w == 0.0 {
            continue;
        }
        for &(bone, bw) in &weights[r.v[k] as usize] {
            match out.iter_mut().find(|(b, _)| *b == bone) {
                Some(slot) => slot.1 += w * bw,
                None => out.push((bone, w * bw)),
            }
        }
    }
    out.retain(|(_, w)| *w >= 0.001);
    out
}

/// The four strongest influences, renormalised — what a GPU skin takes.
#[must_use]
pub fn top_four(mut influences: Vec<(u16, f32)>) -> ([u16; 4], [f32; 4]) {
    influences.sort_by(|a, b| b.1.total_cmp(&a.1));
    influences.truncate(4);
    let sum: f32 = influences.iter().map(|(_, w)| w).sum();
    let mut joints = [0u16; 4];
    let mut weights = [0.0f32; 4];
    for (k, (b, w)) in influences.into_iter().enumerate() {
        joints[k] = b;
        weights[k] = if sum > 0.0 { w / sum } else { 0.0 };
    }
    if sum <= 0.0 {
        weights[0] = 1.0;
    }
    (joints, weights)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ProxyKind;

    fn proxy(refs: Vec<ProxyRef>, scale: [Option<(u32, u32, f32)>; 3]) -> Proxy {
        Proxy {
            name: "t".into(),
            kind: ProxyKind::Outfit,
            refs,
            scale,
            uvs: vec![],
            faces: vec![],
            delete_verts: vec![],
            texture: None,
            alpha: false,
            face_regions: vec![],
            region_colours: vec![],
        }
    }

    /// A garment grows with the body: its offsets scale by the measured span
    /// over the authored one, axis by axis.
    #[test]
    fn offsets_scale_with_the_body() {
        let base = vec![
            Vec3::ZERO,
            Vec3::new(4.0, 0.0, 0.0),
            Vec3::new(0.0, 1.0, 0.0),
        ];
        let r = ProxyRef::new([1, 2, 0], [0.5, 0.5, 0.0], [1.0, 1.0, 1.0]);
        let p = proxy(vec![r], [Some((0, 1, 2.0)), None, None]);
        let fitted = fit(&p, &base);
        // Mean of B1 and B2 is (2, 0.5, 0); the x offset doubles (4 / 2).
        assert_eq!(fitted[0], Vec3::new(4.0, 1.5, 1.0));
    }

    #[test]
    fn weights_blend_and_trim_to_four() {
        let weights = vec![
            vec![(0, 1.0)],
            vec![(1, 0.5), (2, 0.5)],
            vec![(3, 0.6), (4, 0.4)],
        ];
        let r = ProxyRef::new([0, 1, 2], [0.5, 0.25, 0.25], [0.0; 3]);
        let blended = blend_weights(&r, &weights);
        let total: f32 = blended.iter().map(|(_, w)| w).sum();
        assert!((total - 1.0).abs() < 1.0e-5);
        let (joints, w) = top_four(blended);
        assert_eq!(joints[0], 0, "the strongest first");
        assert!((w.iter().sum::<f32>() - 1.0).abs() < 1.0e-5);
        assert!(!joints.contains(&4), "the weakest of five dropped");
    }
}

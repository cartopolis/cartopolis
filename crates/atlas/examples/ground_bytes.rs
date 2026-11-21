//! Where a pre-cut ground cell's bytes go after Brotli, stream by stream, and
//! what a few encodings would save. A measurement, not a format.
//!
//! `cargo run -p cartopolis_atlas --example ground_bytes -- <url or file>`

use std::collections::HashMap;
use std::io::Read;

use cartopolis_geo::ground_sheets::{decode, encode};
use cartopolis_geo::vector_tiles::SurfaceMesh;

fn br(bytes: &[u8], q: i32, lgwin: i32) -> usize {
    if bytes.is_empty() {
        return 0;
    }
    let params = brotli::enc::BrotliEncoderParams {
        quality: q,
        lgwin,
        size_hint: bytes.len(),
        ..Default::default()
    };
    let mut out = Vec::new();
    brotli::BrotliCompress(&mut &bytes[..], &mut out, &params).unwrap();
    out.len()
}

#[derive(Default)]
struct W(Vec<u8>);
impl W {
    fn varint(&mut self, mut v: u64) {
        while v >= 0x80 {
            self.0.push((v as u8) | 0x80);
            v >>= 7;
        }
        self.0.push(v as u8);
    }
    fn zigzag(&mut self, v: i64) {
        self.varint(((v << 1) ^ (v >> 63)) as u64);
    }
}

fn morton(x: u32, z: u32) -> u64 {
    fn spread(mut v: u64) -> u64 {
        v &= 0xffff_ffff;
        v = (v | (v << 16)) & 0x0000_ffff_0000_ffff;
        v = (v | (v << 8)) & 0x00ff_00ff_00ff_00ff;
        v = (v | (v << 4)) & 0x0f0f_0f0f_0f0f_0f0f;
        v = (v | (v << 2)) & 0x3333_3333_3333_3333;
        v = (v | (v << 1)) & 0x5555_5555_5555_5555;
        v
    }
    spread(u64::from(x)) | (spread(u64::from(z)) << 1)
}

/// Streams of one mesh under an encoding.
#[derive(Default)]
struct Streams {
    pos: W,
    colour: W,
    other: W,
    refs: W,
}

#[derive(Clone, Copy)]
enum Order {
    FirstUse,
    Morton,
}

#[derive(Clone, Copy)]
enum Pred {
    /// Delta from the previous distinct vertex.
    Prev,
    /// Parallelogram: for the third vertex of a triangle whose other two are
    /// already in the table, predict `a + b - c` with c the previous triangle's
    /// opposite vertex — here simplified to: predict from the two other
    /// vertices of the same triangle when both are known (midpoint), else prev.
    Triangle,
}

fn split(m: &SurfaceMesh, quant: f32, order: Order, pred: Pred, s: &mut Streams) -> (usize, usize) {
    let n = m.positions.len();
    let q = |p: [f32; 3]| {
        [
            (p[0] * quant).round() as i64,
            (p[1] * quant).round() as i64,
            (p[2] * quant).round() as i64,
        ]
    };
    let col = |c: [f32; 4]| c.map(|v| (v * 1024.0).round() as i64);
    let has_col = m.colors.len() == n && n > 0;
    let has_uv = m.uvs.len() == n && n > 0;
    let has_rung = m.rungs.len() == n && n > 0;
    let flat = m.positions.iter().all(|p| p[1] == 0.0);
    let soup = m.indices.iter().enumerate().all(|(i, &v)| v as usize == i);
    let pred = if soup { pred } else { Pred::Prev };

    let key = |i: usize| -> ([i64; 3], Vec<i64>) {
        let mut bits = Vec::new();
        if has_uv {
            bits.extend(m.uvs[i].map(|v| i64::from(v.to_bits())));
        }
        if has_col {
            bits.extend(col(m.colors[i]));
        }
        if has_rung {
            bits.push(m.rungs[i].to_bits().into());
        }
        (q(m.positions[i]), bits)
    };
    let mut table: HashMap<([i64; 3], Vec<i64>), u32> = HashMap::new();
    let mut distinct: Vec<usize> = Vec::new();
    let refs: Vec<u32> = (0..n)
        .map(|i| {
            *table.entry(key(i)).or_insert_with(|| {
                distinct.push(i);
                (distinct.len() - 1) as u32
            })
        })
        .collect();

    // Table order.
    let mut perm: Vec<usize> = (0..distinct.len()).collect(); // table slot -> rank
    match order {
        Order::FirstUse => {}
        Order::Morton => {
            let mut keyed: Vec<(u64, usize)> = distinct
                .iter()
                .enumerate()
                .map(|(slot, &i)| {
                    let p = q(m.positions[i]);
                    (
                        morton((p[0] + (1 << 30)) as u32, (p[2] + (1 << 30)) as u32),
                        slot,
                    )
                })
                .collect();
            keyed.sort_unstable();
            for (rank, (_, slot)) in keyed.iter().enumerate() {
                perm[*slot] = rank;
            }
        }
    }
    let mut ranked: Vec<usize> = vec![0; distinct.len()]; // rank -> vertex index
    for (slot, &i) in distinct.iter().enumerate() {
        ranked[perm[slot]] = i;
    }

    // Positions.
    match pred {
        Pred::Prev => {
            let mut prev = [0i64; 3];
            for &i in &ranked {
                let p = q(m.positions[i]);
                for k in [0, 1, 2] {
                    if k == 1 && flat {
                        continue;
                    }
                    s.pos.zigzag(p[k] - prev[k]);
                    prev[k] = p[k];
                }
            }
        }
        Pred::Triangle => {
            // Walk vertices in mesh order; a new vertex is predicted from the
            // already-known vertices of its triangle (their mean), else prev.
            let mut seen = vec![false; distinct.len()];
            let mut prev = [0i64; 3];
            for t in 0..n / 3 {
                let tri = [refs[t * 3], refs[t * 3 + 1], refs[t * 3 + 2]];
                for (j, &r) in tri.iter().enumerate() {
                    if seen[r as usize] {
                        continue;
                    }
                    seen[r as usize] = true;
                    let p = q(m.positions[distinct[r as usize]]);
                    let known: Vec<[i64; 3]> = tri
                        .iter()
                        .enumerate()
                        .filter(|(jj, rr)| *jj != j && seen[**rr as usize])
                        .map(|(_, rr)| q(m.positions[distinct[*rr as usize]]))
                        .collect();
                    let guess = if known.is_empty() {
                        prev
                    } else {
                        let mut g = [0i64; 3];
                        for k in known.iter() {
                            for a in 0..3 {
                                g[a] += k[a];
                            }
                        }
                        g.map(|v| v / known.len() as i64)
                    };
                    for k in [0, 1, 2] {
                        if k == 1 && flat {
                            continue;
                        }
                        s.pos.zigzag(p[k] - guess[k]);
                    }
                    prev = p;
                }
            }
        }
    }

    // Colours and the rest, palette + runs as the format does, in table order.
    if has_col {
        let mut pal: HashMap<[i64; 4], u32> = HashMap::new();
        let mut run_prev: Option<u32> = None;
        let mut run_n = 0u64;
        let mut runs = 0usize;
        for &i in &ranked {
            let c = col(m.colors[i]);
            let next = pal.len() as u32;
            let idx = *pal.entry(c).or_insert_with(|| {
                c.iter().for_each(|&v| s.colour.zigzag(v));
                next
            });
            if run_prev == Some(idx) {
                run_n += 1;
            } else {
                if let Some(p) = run_prev {
                    s.colour.varint(u64::from(p));
                    s.colour.varint(run_n);
                    runs += 1;
                }
                run_prev = Some(idx);
                run_n = 1;
            }
        }
        if let Some(p) = run_prev {
            s.colour.varint(u64::from(p));
            s.colour.varint(run_n);
            runs += 1;
        }
        let _ = runs;
    }
    if has_uv {
        for &i in &ranked {
            s.other.0.extend_from_slice(&m.uvs[i][0].to_le_bytes());
            s.other.0.extend_from_slice(&m.uvs[i][1].to_le_bytes());
        }
    }
    if has_rung {
        for &i in &ranked {
            s.other
                .0
                .extend_from_slice(&m.rungs[i].to_bits().to_le_bytes());
        }
    }

    // References.
    match order {
        Order::FirstUse => {
            let mut high = 0u32;
            for r in &refs {
                if *r == high {
                    s.refs.varint(0);
                    high += 1;
                } else {
                    s.refs.varint(u64::from(high - r));
                }
            }
        }
        Order::Morton => {
            let mut prev = 0i64;
            for r in &refs {
                let v = i64::from(perm[*r as usize] as u32);
                s.refs.zigzag(v - prev);
                prev = v;
            }
        }
    }
    if !soup {
        let mut prev = 0i64;
        for &i in &m.indices {
            s.other.zigzag(i64::from(i) - prev);
            prev = i64::from(i);
        }
    }
    (n, distinct.len())
}

fn main() {
    let arg = std::env::args().nth(1).expect("url or file");
    let bytes = if arg.starts_with("http") {
        let mut v = Vec::new();
        ureq::get(&arg)
            .call()
            .unwrap()
            .into_reader()
            .read_to_end(&mut v)
            .unwrap();
        v
    } else {
        std::fs::read(&arg).unwrap()
    };
    println!(
        "file: {} bytes raw, {} br10, {} br11/24",
        bytes.len(),
        br(&bytes, 10, 22),
        br(&bytes, 11, 24)
    );
    let cut = decode(&bytes).unwrap();
    let re = encode(cut.revision, cut.fingerprint, cut.tile_m, &cut.meshes);
    println!("re-encoded: {} raw (same: {})", re.len(), re == bytes);

    // Per group: vertices, distinct, triangles, mean triangle area.
    println!(
        "\n{:<28} {:>9} {:>9} {:>9} {:>10}",
        "group", "verts", "distinct", "tris", "mean m2"
    );
    let mut tot_tris = 0usize;
    for m in &cut.meshes {
        let n = m.positions.len();
        let mut s = Streams::default();
        let (_, d) = split(m, 4000.0, Order::FirstUse, Pred::Prev, &mut s);
        let tris = m.indices.len() / 3;
        tot_tris += tris;
        let mut area = 0.0f64;
        for t in 0..tris {
            let a = m.positions[m.indices[t * 3] as usize];
            let b = m.positions[m.indices[t * 3 + 1] as usize];
            let c = m.positions[m.indices[t * 3 + 2] as usize];
            let ab = [f64::from(b[0] - a[0]), f64::from(b[2] - a[2])];
            let ac = [f64::from(c[0] - a[0]), f64::from(c[2] - a[2])];
            area += (ab[0] * ac[1] - ab[1] * ac[0]).abs() / 2.0;
        }
        println!(
            "{:<28} {:>9} {:>9} {:>9} {:>10.3}",
            format!("{:?}", m.group),
            n,
            d,
            tris,
            if tris > 0 { area / tris as f64 } else { 0.0 }
        );
    }
    println!("total triangles {tot_tris}");

    // Whole-file streams under each encoding.
    let variants: [(&str, f32, Order, Pred); 6] = [
        (
            "current  0.25mm first-use prev",
            4000.0,
            Order::FirstUse,
            Pred::Prev,
        ),
        (
            "1mm      first-use prev",
            1000.0,
            Order::FirstUse,
            Pred::Prev,
        ),
        (
            "1cm      first-use prev",
            100.0,
            Order::FirstUse,
            Pred::Prev,
        ),
        (
            "0.25mm   first-use triangle",
            4000.0,
            Order::FirstUse,
            Pred::Triangle,
        ),
        ("0.25mm   morton prev", 4000.0, Order::Morton, Pred::Prev),
        ("1mm      morton prev", 1000.0, Order::Morton, Pred::Prev),
    ];
    println!(
        "\n{:<34} {:>9} {:>9} {:>9} {:>9} {:>9}   (Brotli q10 per stream, bytes)",
        "variant", "pos", "colour", "other", "refs", "sum"
    );
    for (name, quant, order, pred) in variants {
        let mut s = Streams::default();
        for m in &cut.meshes {
            split(m, quant, order, pred, &mut s);
        }
        let p = br(&s.pos.0, 10, 22);
        let c = br(&s.colour.0, 10, 22);
        let o = br(&s.other.0, 10, 22);
        let r = br(&s.refs.0, 10, 22);
        println!(
            "{:<34} {:>9} {:>9} {:>9} {:>9} {:>9}   raw pos {} refs {}",
            name,
            p,
            c,
            o,
            r,
            p + c + o + r,
            s.pos.0.len(),
            s.refs.0.len()
        );
    }

    // Histogram of position delta varint lengths for the current encoding.
    let mut hist = [0usize; 6];
    for m in &cut.meshes {
        let mut s = Streams::default();
        split(m, 4000.0, Order::FirstUse, Pred::Prev, &mut s);
        let b = &s.pos.0;
        let mut i = 0;
        while i < b.len() {
            let mut len = 1;
            while b[i + len - 1] & 0x80 != 0 {
                len += 1;
            }
            hist[len.min(5)] += 1;
            i += len;
        }
    }
    slivers(&cut.meshes);
    widths(&cut);
    println!(
        "\nposition delta varint lengths (1..5 bytes): {:?}",
        &hist[1..]
    );
}

/// Triangle area histogram per group, and how many triangles a coarser grid
/// would collapse or flip.
#[allow(dead_code)]
pub fn slivers(meshes: &[SurfaceMesh]) {
    let bounds = [0.001f64, 0.01, 0.05, 0.5, 5.0, f64::INFINITY];
    println!(
        "\n{:<24} {:>8} {:>8} {:>8} {:>8} {:>8} {:>8} | flipped/zero at 1cm, 5mm, 1mm",
        "group", "<1e-3", "<0.01", "<0.05", "<0.5", "<5", ">=5"
    );
    let mut total = [0usize; 6];
    let mut total_tris = 0usize;
    for m in meshes {
        let mut h = [0usize; 6];
        let mut bad = [0usize; 3];
        let tris = m.indices.len() / 3;
        for t in 0..tris {
            let a = m.positions[m.indices[t * 3] as usize];
            let b = m.positions[m.indices[t * 3 + 1] as usize];
            let c = m.positions[m.indices[t * 3 + 2] as usize];
            let cross = |a: [f32; 3], b: [f32; 3], c: [f32; 3], q: f32| -> f64 {
                let qq = |v: f32| f64::from((v * q).round() / q);
                (qq(b[0]) - qq(a[0])) * (qq(c[2]) - qq(a[2]))
                    - (qq(b[2]) - qq(a[2])) * (qq(c[0]) - qq(a[0]))
            };
            let s0 = cross(a, b, c, 4000.0);
            let area = s0.abs() / 2.0;
            let k = bounds.iter().position(|&bnd| area < bnd).unwrap();
            h[k] += 1;
            for (j, q) in [100.0f32, 200.0, 1000.0].iter().enumerate() {
                let s = cross(a, b, c, *q);
                if s == 0.0 || (s > 0.0) != (s0 > 0.0) {
                    bad[j] += 1;
                }
            }
        }
        for k in 0..6 {
            total[k] += h[k];
        }
        total_tris += tris;
        println!(
            "{:<24} {:>8} {:>8} {:>8} {:>8} {:>8} {:>8} | {} {} {}",
            format!("{:?}", m.group),
            h[0],
            h[1],
            h[2],
            h[3],
            h[4],
            h[5],
            bad[0],
            bad[1],
            bad[2]
        );
    }
    println!(
        "{:<24} {:>8} {:>8} {:>8} {:>8} {:>8} {:>8}   of {total_tris}",
        "all", total[0], total[1], total[2], total[3], total[4], total[5]
    );
}

/// Width of the thin triangles (2·area / longest edge), and what dropping the
/// ones under a width would save in the file.
#[allow(dead_code)]
pub fn widths(cut: &cartopolis_geo::ground_sheets::PreCut) {
    let bounds = [0.00025f64, 0.001, 0.005, 0.02, 0.1, f64::INFINITY];
    let mut hist = [0usize; 6];
    let mut hist_paving = [0usize; 6];
    let width = |m: &SurfaceMesh, t: usize| -> f64 {
        let p = |i: usize| m.positions[m.indices[t * 3 + i] as usize];
        let (a, b, c) = (p(0), p(1), p(2));
        let e = |u: [f32; 3], v: [f32; 3]| {
            (f64::from(u[0] - v[0]).powi(2) + f64::from(u[2] - v[2]).powi(2)).sqrt()
        };
        let longest = e(a, b).max(e(b, c)).max(e(c, a));
        let area2 = (f64::from(b[0] - a[0]) * f64::from(c[2] - a[2])
            - f64::from(b[2] - a[2]) * f64::from(c[0] - a[0]))
        .abs();
        if longest > 0.0 {
            area2 / longest
        } else {
            0.0
        }
    };
    for m in &cut.meshes {
        for t in 0..m.indices.len() / 3 {
            let w = width(m, t);
            let k = bounds.iter().position(|&b| w < b).unwrap();
            hist[k] += 1;
            if matches!(m.group, cartopolis_geo::vector_tiles::SurfaceGroup::Paving) {
                hist_paving[k] += 1;
            }
        }
    }
    println!(
        "\ntriangle widths (<0.25mm, <1mm, <5mm, <2cm, <10cm, more): all {:?} paving {:?}",
        hist, hist_paving
    );
    for max_w in [0.00025f64, 0.001, 0.005, 0.02] {
        let meshes: Vec<SurfaceMesh> = cut
            .meshes
            .iter()
            .map(|m| {
                let mut out = SurfaceMesh {
                    group: m.group,
                    positions: Vec::new(),
                    indices: Vec::new(),
                    uvs: Vec::new(),
                    normals: Vec::new(),
                    colors: Vec::new(),
                    rungs: Vec::new(),
                };
                for t in 0..m.indices.len() / 3 {
                    if width(m, t) < max_w {
                        continue;
                    }
                    for k in 0..3 {
                        let i = m.indices[t * 3 + k] as usize;
                        let base = out.positions.len() as u32;
                        out.positions.push(m.positions[i]);
                        if !m.uvs.is_empty() {
                            out.uvs.push(m.uvs[i]);
                        }
                        if !m.normals.is_empty() {
                            out.normals.push(m.normals[i]);
                        }
                        if !m.colors.is_empty() {
                            out.colors.push(m.colors[i]);
                        }
                        if !m.rungs.is_empty() {
                            out.rungs.push(m.rungs[i]);
                        }
                        out.indices.push(base);
                    }
                }
                out
            })
            .collect();
        let tris: usize = meshes.iter().map(|m| m.indices.len() / 3).sum();
        let bytes = encode(cut.revision, cut.fingerprint, cut.tile_m, &meshes);
        println!(
            "drop width < {:>7.5} m: {} triangles, {} raw, {} br10",
            max_w,
            tris,
            bytes.len(),
            br(&bytes, 10, 22)
        );
    }
}

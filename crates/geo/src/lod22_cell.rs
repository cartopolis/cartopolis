//! One LoD2.2 cell file in memory, and its compact binary encoding.
//!
//! atlas's `pipeline/lod22.rs` writes every z14 cell of 3DBAG building shells
//! twice: as the JSON the client has always read (`lod22/14/{x}/{y}.json`) and,
//! beside it, in the encoding below (`lod22/14/{x}/{y}.bin`). The client's
//! `systems::lod22` asks for the binary first and falls back to the JSON. Both
//! ends use **this module's** types and codec, so the binary half of the seam is
//! one definition — and the JSON half is too, through `formats::lod22`, which
//! converts to and from this module's [`Cell`].
//!
//! # Why a binary at all
//!
//! Groningen's densest cell, 8490/5319 (7,934 buildings, 174,614 vertices,
//! 266,902 triangles), is 8.3 MB of JSON and 1.76 MB at `gzip -6`, because every
//! coordinate is decimal text and every vertex repeats its absolute latitude
//! digits. In this encoding it is 1.80 MB raw and 0.94 MB at `gzip -6`
//! (2026-09-26; the parse times and the method are in `docs/notes/atlas.md`).
//!
//! # The format, version 1
//!
//! Little-endian. A fixed 16-byte header:
//!
//! | bytes | field |
//! |-------|-------|
//! | 4 | magic `b"CL22"` |
//! | 2 | version, `u16` = 1 |
//! | 2 | flags, `u16`, reserved = 0 |
//! | 4 | `per_degree`, `u32`: quantisation steps per degree of lat/lng |
//! | 4 | `per_metre`, `u32`: quantisation steps per metre of height |
//!
//! then five varints giving the byte lengths of five **streams**, then the
//! streams back to back — `meta`, `lat`, `lng`, `h`, `tri` — which must add up to
//! exactly the rest of the file. Every number in them is an unsigned LEB128
//! varint; a signed one is zigzagged first. A string is a varint byte length and
//! that many UTF-8 bytes.
//!
//! ```text
//! meta:  origin_lat origin_lng            the first vertex, in steps
//!        n_pands { string }               ids an imported-model cell replaces
//!        n_elements { id: string  part  class: string  flags: u8
//!                     n_verts  n_tris  [ roof: u8 ]*n_tris if flags bit 0 }
//! lat:   one signed delta per vertex, from the previous vertex (the first
//!        from the origin), running through the whole file
//! lng:   the same for longitude
//! h:     one signed height per vertex, absolute
//! tri:   three index codes per triangle, element by element
//! ```
//!
//! **Streams rather than records** because each column is self-similar and the
//! columns are not like each other: gzip finds a latitude delta's repeats among
//! other latitude deltas, not interleaved with triangle indices. Measured on
//! 8490/5319, interleaving the same values cost 10 % more after `gzip -6`.
//!
//! **Latitude and longitude are deltas** from the vertex before: a shell's
//! vertices are metres apart and arrive in pairs sharing a footprint point (the
//! foot and the eaves of one wall corner), so most deltas are one or two bytes and
//! a pair's second is a single zero byte. **Height is absolute** — it is already
//! metres above the building's own base, and foot/eaves alternate `0, h, 0, h`,
//! which as deltas would be two large values instead of a zero and one.
//!
//! **An index code** names a vertex against the element's *high-water mark* `hw`
//! (one past the largest index so far) and the previous triangle `(p0, p1, p2)`:
//!
//! | code | index |
//! |------|-------|
//! | 0 | `hw` — a vertex never used before |
//! | 1, 2, 3 | `p0`, `p1`, `p2` |
//! | `4 + z` | `hw − 1 − unzigzag(z)` |
//!
//! atlas numbers a building's vertices in the order its triangles first use them,
//! and earcut emits a wall quad as `(a, b, c), (c, d, a)`, so nearly every code is
//! 0–3 and one byte, while an index in any order at all is still exact. Unless an
//! element sets flags bit 0 the roof flag rides in the low bit of each triangle's
//! first code (`code << 1 | roof`); with it, the flags are raw bytes in `meta`,
//! for a flag that is neither 0 nor 1.
//!
//! Nothing may be left over: a stream not consumed to its last byte is an error,
//! like a short read, so a file cut or spliced anywhere fails to decode rather
//! than yielding a plausible prefix.
//!
//! # Precision
//!
//! Positions are integers on the grid the header names. atlas writes
//! [`Grid::JSON`] — 10⁻⁷° and 1 cm — which is **the grid the JSON is already
//! rounded to** (`round(lat, 7)`, `round(h, 2)` in the extractor), so for the
//! data atlas produces the binary carries exactly the JSON's numbers: decoding
//! gives the nearest `f64` (and, for height, the `f32` of the nearest `f64`) to
//! the same decimal the JSON spells out. Off the grid the error is at most half a
//! step: 5·10⁻⁸° is ≤ 5.6 mm of latitude and ≤ 3.4 mm of longitude at Dutch
//! latitudes, and 5 mm of height. The tests pin both halves.
//!
//! Pure Rust with no dependencies, so it builds for wasm32 and Android as it is.

/// The four bytes every encoded cell starts with.
pub const MAGIC: [u8; 4] = *b"CL22";
/// The format version this module writes and reads.
pub const VERSION: u16 = 1;
/// Bytes before the stream lengths.
const HEADER_LEN: usize = 16;

/// One cell file: its building elements, and — for an imported-model cell — the
/// BAG ids of the shells it stands in for.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Cell {
    pub elements: Vec<Element>,
    /// Empty for every LoD2.2 cell; only an imported-model (LoD3) cell names the
    /// buildings it replaces.
    pub pands: Vec<String>,
}

/// One building part as the host sends it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Element {
    /// The BAG pand **identificatie**, verbatim (leading zeros and all), or empty
    /// when the file carries none.
    pub id: String,
    /// Which solid of a pand split into several this is; 0 for most.
    pub part: u32,
    /// The IFC class of an imported model's element; empty for a shell.
    pub class: String,
    /// Vertices as (latitude°, longitude°, metres above the building's own base).
    pub p: Vec<(f64, f64, f32)>,
    /// Triangles: three indices into `p` and a roof flag (1 = roof, 0 = wall).
    /// Carried as written — [`decode`] does not check the indices against `p`,
    /// exactly as the JSON parse does not, so the consumer's validation sees the
    /// same triangles whichever format they came in.
    pub t: Vec<(u32, u32, u32, u8)>,
}

/// The quantisation grid: steps per degree of latitude/longitude and steps per
/// metre of height.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Grid {
    pub per_degree: u32,
    pub per_metre: u32,
}

impl Grid {
    /// 10⁻⁷° and 1 cm — the rounding atlas's LoD2.2 extractor already applies to
    /// the JSON, so encoding its output on this grid loses nothing.
    pub const JSON: Grid = Grid {
        per_degree: 10_000_000,
        per_metre: 100,
    };
}

/// Why a [`Cell`] could not be encoded.
#[derive(Clone, Debug, PartialEq)]
pub enum EncodeError {
    /// A zero step count in the grid.
    ZeroGrid,
    /// A coordinate that is not finite, or too large to quantise into an `i64`.
    Unrepresentable { element: usize, vertex: usize },
}

impl std::fmt::Display for EncodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EncodeError::ZeroGrid => f.write_str("lod22 cell: zero quantisation grid"),
            EncodeError::Unrepresentable { element, vertex } => write!(
                f,
                "lod22 cell: element {element} vertex {vertex} is not a finite, quantisable coordinate"
            ),
        }
    }
}

impl std::error::Error for EncodeError {}

/// Why bytes could not be decoded as a cell. Every malformed input ends here;
/// [`decode`] never panics and never allocates more than the input can describe.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DecodeError {
    /// Not an encoded cell at all (wrong magic, or shorter than the header).
    NotACell,
    /// An encoded cell of a version this build does not read.
    Version(u16),
    /// A zero step count in the header.
    ZeroGrid,
    /// The input ended in the middle of something.
    Truncated,
    /// A value that cannot be what the encoder wrote: an over-long varint, an
    /// index out of range, invalid UTF-8, stream lengths that do not add up,
    /// bytes left over.
    Corrupt(&'static str),
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecodeError::NotACell => f.write_str("lod22 cell: not a binary cell"),
            DecodeError::Version(v) => write!(f, "lod22 cell: unsupported version {v}"),
            DecodeError::ZeroGrid => f.write_str("lod22 cell: zero quantisation grid"),
            DecodeError::Truncated => f.write_str("lod22 cell: truncated"),
            DecodeError::Corrupt(what) => write!(f, "lod22 cell: corrupt ({what})"),
        }
    }
}

impl std::error::Error for DecodeError {}

/// Whether `bytes` start like an encoded cell — the cheap check a fetcher can
/// make before storing a body, without decoding it.
#[must_use]
pub fn looks_like_cell(bytes: &[u8]) -> bool {
    bytes.len() >= HEADER_LEN && bytes[..4] == MAGIC
}

fn zigzag(v: i64) -> u64 {
    ((v << 1) ^ (v >> 63)) as u64
}

fn unzigzag(v: u64) -> i64 {
    ((v >> 1) as i64) ^ -((v & 1) as i64)
}

fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn put_str(out: &mut Vec<u8>, s: &str) {
    put_varint(out, s.len() as u64);
    out.extend_from_slice(s.as_bytes());
}

/// `x` in steps of `1 / per`, or `None` when that is not an `i64`.
fn quantise(x: f64, per: u32) -> Option<i64> {
    let q = (x * f64::from(per)).round();
    // 2⁶² keeps every delta between two such values inside an i64.
    (q.is_finite() && q.abs() < (1u64 << 62) as f64).then_some(q as i64)
}

/// The code for index `i` (see the module docs), given the high-water mark and
/// the previous triangle of the same element.
fn index_code(i: u32, hw: u32, prev: Option<[u32; 3]>) -> u64 {
    if i == hw {
        return 0;
    }
    if let Some(n) = prev.and_then(|p| p.iter().position(|&q| q == i)) {
        return 1 + n as u64;
    }
    4 + zigzag(i64::from(hw) - 1 - i64::from(i))
}

/// Encode a cell on `grid`. Fails only on a zero grid or a non-finite (or
/// absurdly large) coordinate.
pub fn encode(cell: &Cell, grid: Grid) -> Result<Vec<u8>, EncodeError> {
    if grid.per_degree == 0 || grid.per_metre == 0 {
        return Err(EncodeError::ZeroGrid);
    }
    // Quantise everything first, so an unrepresentable vertex fails before any
    // output is produced.
    let mut q: Vec<Vec<[i64; 3]>> = Vec::with_capacity(cell.elements.len());
    for (ei, el) in cell.elements.iter().enumerate() {
        let mut vs = Vec::with_capacity(el.p.len());
        for (vi, &(lat, lng, h)) in el.p.iter().enumerate() {
            let err = EncodeError::Unrepresentable {
                element: ei,
                vertex: vi,
            };
            vs.push([
                quantise(lat, grid.per_degree).ok_or(err.clone())?,
                quantise(lng, grid.per_degree).ok_or(err.clone())?,
                quantise(f64::from(h), grid.per_metre).ok_or(err)?,
            ]);
        }
        q.push(vs);
    }
    let origin = q
        .iter()
        .find_map(|vs| vs.first())
        .map_or([0, 0], |v| [v[0], v[1]]);

    let n_verts: usize = q.iter().map(Vec::len).sum();
    let n_tris: usize = cell.elements.iter().map(|e| e.t.len()).sum();
    let mut meta = Vec::with_capacity(16 + cell.elements.len() * 24);
    let mut lat = Vec::with_capacity(n_verts * 2);
    let mut lng = Vec::with_capacity(n_verts * 2);
    let mut h = Vec::with_capacity(n_verts * 2);
    let mut tri = Vec::with_capacity(n_tris * 3);

    put_varint(&mut meta, zigzag(origin[0]));
    put_varint(&mut meta, zigzag(origin[1]));
    put_varint(&mut meta, cell.pands.len() as u64);
    for p in &cell.pands {
        put_str(&mut meta, p);
    }
    put_varint(&mut meta, cell.elements.len() as u64);
    let mut prev = origin;
    for (el, vs) in cell.elements.iter().zip(&q) {
        put_str(&mut meta, &el.id);
        put_varint(&mut meta, u64::from(el.part));
        put_str(&mut meta, &el.class);
        let raw_roof = el.t.iter().any(|t| t.3 > 1);
        meta.push(u8::from(raw_roof));
        put_varint(&mut meta, vs.len() as u64);
        put_varint(&mut meta, el.t.len() as u64);
        if raw_roof {
            meta.extend(el.t.iter().map(|t| t.3));
        }
        for v in vs {
            put_varint(&mut lat, zigzag(v[0] - prev[0]));
            put_varint(&mut lng, zigzag(v[1] - prev[1]));
            put_varint(&mut h, zigzag(v[2]));
            prev = [v[0], v[1]];
        }
        let mut hw = 0u32;
        let mut prev_tri = None;
        for &(a, b, c, roof) in &el.t {
            for (n, i) in [a, b, c].into_iter().enumerate() {
                let code = index_code(i, hw, prev_tri);
                if n == 0 && !raw_roof {
                    put_varint(&mut tri, (code << 1) | u64::from(roof));
                } else {
                    put_varint(&mut tri, code);
                }
                hw = hw.max(i.saturating_add(1));
            }
            prev_tri = Some([a, b, c]);
        }
    }

    let streams = [&meta, &lat, &lng, &h, &tri];
    let body: usize = streams.iter().map(|s| s.len()).sum();
    let mut out = Vec::with_capacity(HEADER_LEN + 50 + body);
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&grid.per_degree.to_le_bytes());
    out.extend_from_slice(&grid.per_metre.to_le_bytes());
    for s in streams {
        put_varint(&mut out, s.len() as u64);
    }
    for s in streams {
        out.extend_from_slice(s);
    }
    Ok(out)
}

/// A cursor over one stream that fails instead of panicking.
struct Reader<'a> {
    buf: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, at: 0 }
    }

    fn left(&self) -> usize {
        self.buf.len() - self.at
    }

    fn byte(&mut self) -> Result<u8, DecodeError> {
        let b = *self.buf.get(self.at).ok_or(DecodeError::Truncated)?;
        self.at += 1;
        Ok(b)
    }

    fn bytes(&mut self, n: usize) -> Result<&'a [u8], DecodeError> {
        if n > self.left() {
            return Err(DecodeError::Truncated);
        }
        let s = &self.buf[self.at..self.at + n];
        self.at += n;
        Ok(s)
    }

    fn varint(&mut self) -> Result<u64, DecodeError> {
        let mut v = 0u64;
        for shift in (0..64).step_by(7) {
            let b = self.byte()?;
            let bits = u64::from(b & 0x7f);
            if shift == 63 && bits > 1 {
                return Err(DecodeError::Corrupt("varint overflows 64 bits"));
            }
            v |= bits << shift;
            if b & 0x80 == 0 {
                return Ok(v);
            }
        }
        Err(DecodeError::Corrupt("varint longer than ten bytes"))
    }

    fn signed(&mut self) -> Result<i64, DecodeError> {
        self.varint().map(unzigzag)
    }

    /// A varint that must fit a `usize`.
    fn size(&mut self) -> Result<usize, DecodeError> {
        usize::try_from(self.varint()?).map_err(|_| DecodeError::Corrupt("size"))
    }

    /// A count of things each taking at least `min_bytes` of `stream`, refused
    /// when the bytes left there could not hold that many — which is what stops a
    /// corrupt count turning into a huge allocation.
    fn count(&mut self, stream_left: usize, min_bytes: usize) -> Result<usize, DecodeError> {
        let n = self.size()?;
        match n.checked_mul(min_bytes) {
            Some(need) if need <= stream_left => Ok(n),
            _ => Err(DecodeError::Truncated),
        }
    }

    fn string(&mut self) -> Result<String, DecodeError> {
        let n = self.size()?;
        let bytes = self.bytes(n)?;
        String::from_utf8(bytes.to_vec()).map_err(|_| DecodeError::Corrupt("string is not UTF-8"))
    }
}

/// Decode an encoded cell. Returns an error — never panics — on anything that is
/// not a complete, well-formed cell of a known version.
pub fn decode(bytes: &[u8]) -> Result<Cell, DecodeError> {
    if !looks_like_cell(bytes) {
        return Err(DecodeError::NotACell);
    }
    let u16_at = |i: usize| u16::from_le_bytes([bytes[i], bytes[i + 1]]);
    let u32_at =
        |i: usize| u32::from_le_bytes([bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]);
    let version = u16_at(4);
    if version != VERSION {
        return Err(DecodeError::Version(version));
    }
    if u16_at(6) != 0 {
        return Err(DecodeError::Corrupt("unknown header flags"));
    }
    let (per_degree, per_metre) = (u32_at(8), u32_at(12));
    if per_degree == 0 || per_metre == 0 {
        return Err(DecodeError::ZeroGrid);
    }
    let (deg, met) = (f64::from(per_degree), f64::from(per_metre));

    // The five stream lengths, then the five streams, which must fill the rest
    // of the file exactly.
    let mut head = Reader::new(&bytes[HEADER_LEN..]);
    let mut lens = [0usize; 5];
    for len in &mut lens {
        *len = head.size()?;
    }
    let mut body = Reader::new(&head.buf[head.at..]);
    let total = lens
        .iter()
        .try_fold(0usize, |a, &l| a.checked_add(l))
        .ok_or(DecodeError::Corrupt("stream lengths overflow"))?;
    match total.cmp(&body.left()) {
        std::cmp::Ordering::Greater => return Err(DecodeError::Truncated),
        std::cmp::Ordering::Less => return Err(DecodeError::Corrupt("trailing bytes")),
        std::cmp::Ordering::Equal => {}
    }
    let [mut meta, mut lat, mut lng, mut h, mut tri] = lens.map(|l| {
        // Cannot fail: the lengths were just checked against what is there.
        Reader::new(body.bytes(l).unwrap_or_default())
    });

    let mut prev = [meta.signed()?, meta.signed()?];
    let n_pands = meta.count(meta.left(), 1)?;
    let mut pands = Vec::with_capacity(n_pands);
    for _ in 0..n_pands {
        pands.push(meta.string()?);
    }

    // The smallest element in `meta`: two empty strings' lengths, a part, flags
    // and two zero counts.
    let n_elements = meta.count(meta.left(), 6)?;
    let mut elements = Vec::with_capacity(n_elements);
    for _ in 0..n_elements {
        let id = meta.string()?;
        let part = u32::try_from(meta.varint()?).map_err(|_| DecodeError::Corrupt("part"))?;
        let class = meta.string()?;
        let flags = meta.byte()?;
        if flags > 1 {
            return Err(DecodeError::Corrupt("unknown element flags"));
        }
        let raw_roof = flags & 1 == 1;
        // Every vertex takes at least a byte of `h`, every triangle three of `tri`.
        let n_verts = meta.count(h.left(), 1)?;
        let n_tris = meta.count(tri.left(), 3)?;
        let roofs = if raw_roof {
            Some(meta.bytes(n_tris)?)
        } else {
            None
        };

        let mut p = Vec::with_capacity(n_verts);
        for _ in 0..n_verts {
            prev[0] = prev[0]
                .checked_add(lat.signed()?)
                .ok_or(DecodeError::Corrupt("latitude overflows"))?;
            prev[1] = prev[1]
                .checked_add(lng.signed()?)
                .ok_or(DecodeError::Corrupt("longitude overflows"))?;
            let height = h.signed()?;
            p.push((
                prev[0] as f64 / deg,
                prev[1] as f64 / deg,
                (height as f64 / met) as f32,
            ));
        }

        let mut t = Vec::with_capacity(n_tris);
        let mut hw = 0u32;
        let mut prev_tri: Option<[u32; 3]> = None;
        for n_tri in 0..n_tris {
            let mut idx = [0u32; 3];
            let mut roof = roofs.map_or(0, |r| r[n_tri]);
            for (n, slot) in idx.iter_mut().enumerate() {
                let mut code = tri.varint()?;
                if n == 0 && roofs.is_none() {
                    roof = (code & 1) as u8;
                    code >>= 1;
                }
                let i = match code {
                    0 => hw,
                    1..=3 => prev_tri.ok_or(DecodeError::Corrupt("no previous triangle"))?
                        [code as usize - 1],
                    _ => unzigzag(code - 4)
                        .checked_add(1)
                        .and_then(|d| i64::from(hw).checked_sub(d))
                        .and_then(|i| u32::try_from(i).ok())
                        .ok_or(DecodeError::Corrupt("index out of range"))?,
                };
                *slot = i;
                hw = hw.max(i.saturating_add(1));
            }
            t.push((idx[0], idx[1], idx[2], roof));
            prev_tri = Some(idx);
        }
        elements.push(Element {
            id,
            part,
            class,
            p,
            t,
        });
    }
    if [&meta, &lat, &lng, &h, &tri].iter().any(|r| r.left() != 0) {
        return Err(DecodeError::Corrupt("a stream has bytes left over"));
    }
    Ok(Cell { elements, pands })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Metres per degree of latitude, near enough for a tolerance.
    const M_PER_DEG: f64 = 111_320.0;

    /// Two buildings shaped like the real thing: footprint corners in foot/eaves
    /// pairs, triangles numbering vertices in first-use order, one split pand.
    fn sample() -> Cell {
        let parse = |s: &str| s.parse::<f64>().unwrap();
        let mut a = Element {
            id: "0014100010904218".into(),
            ..Element::default()
        };
        for (la, lo) in [
            ("53.2262221", "6.5540593"),
            ("53.2262429", "6.5540435"),
            ("53.2262345", "6.5541072"),
            ("53.2262098", "6.5541169"),
        ] {
            a.p.push((parse(la), parse(lo), "0".parse().unwrap()));
            a.p.push((parse(la), parse(lo), "6.32".parse().unwrap()));
        }
        a.t = vec![
            (0, 1, 2, 0),
            (2, 1, 3, 0),
            (2, 3, 4, 0),
            (4, 3, 5, 0),
            (1, 3, 5, 1),
            (1, 5, 7, 1),
            (6, 7, 0, 0),
        ];
        let b = Element {
            id: "0014100010903891".into(),
            part: 2,
            class: String::new(),
            p: vec![
                (53.2270001, 6.5550002, 0.0),
                (53.2270101, 6.5550002, 0.0),
                (53.2270101, 6.5550102, 12.07),
            ],
            t: vec![(0, 1, 2, 1)],
        };
        Cell {
            elements: vec![a, b],
            pands: vec![],
        }
    }

    #[test]
    fn a_cell_on_the_json_grid_round_trips_exactly() {
        let cell = sample();
        let bytes = encode(&cell, Grid::JSON).unwrap();
        assert_eq!(decode(&bytes).unwrap(), cell);
    }

    #[test]
    fn the_optional_fields_round_trip() {
        let mut cell = sample();
        cell.pands = vec!["0014100010903891".into(), "0363100012345678".into()];
        cell.elements[1].class = "IfcWallStandardCase".into();
        cell.elements[1].p[2].2 = -1.25; // a basement slab
        cell.elements[0].t[3].3 = 7; // a flag that is neither roof nor wall
        cell.elements[0].t[4] = (7, 0, 3, 1); // out of first-use order
        let bytes = encode(&cell, Grid::JSON).unwrap();
        assert_eq!(decode(&bytes).unwrap(), cell);
    }

    #[test]
    fn an_empty_cell_round_trips() {
        let bytes = encode(&Cell::default(), Grid::JSON).unwrap();
        assert_eq!(decode(&bytes).unwrap(), Cell::default());
    }

    /// Off the grid, nothing moves by more than half a step: 5.6 mm of latitude,
    /// 3.4 mm of longitude here, 5 mm of height.
    #[test]
    fn off_the_grid_nothing_moves_more_than_half_a_step() {
        let mut cell = Cell::default();
        let mut el = Element::default();
        // A deterministic spread of un-rounded coordinates across a z14 cell.
        let mut s: u64 = 0x9e37_79b9_7f4a_7c15;
        let mut next = || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            (s >> 11) as f64 / (1u64 << 53) as f64
        };
        for _ in 0..5000 {
            el.p.push((
                53.20 + 0.03 * next(),
                6.54 + 0.03 * next(),
                (40.0 * next()) as f32,
            ));
        }
        el.t = vec![(0, 1, 2, 0)];
        cell.elements.push(el);
        let back = decode(&encode(&cell, Grid::JSON).unwrap()).unwrap();
        let (mut worst_lat, mut worst_lng, mut worst_h) = (0.0f64, 0.0f64, 0.0f64);
        for (a, b) in cell.elements[0].p.iter().zip(&back.elements[0].p) {
            worst_lat = worst_lat.max((a.0 - b.0).abs() * M_PER_DEG);
            worst_lng = worst_lng.max((a.1 - b.1).abs() * M_PER_DEG * a.0.to_radians().cos());
            worst_h = worst_h.max(f64::from((a.2 - b.2).abs()));
        }
        assert!(worst_lat <= 0.0056, "latitude moved {worst_lat} m");
        assert!(worst_lng <= 0.0034, "longitude moved {worst_lng} m");
        // Half a centimetre, plus an f32 ulp at 40 m.
        assert!(worst_h <= 0.005 + 4e-6, "height moved {worst_h} m");
    }

    /// Every proper prefix of a real encoding is refused — none panics, none
    /// decodes to a plausible shorter cell.
    #[test]
    fn every_truncation_is_an_error() {
        let bytes = encode(&sample(), Grid::JSON).unwrap();
        for n in 0..bytes.len() {
            assert!(decode(&bytes[..n]).is_err(), "a {n}-byte prefix decoded");
        }
        let mut long = bytes.clone();
        long.push(0);
        assert_eq!(decode(&long), Err(DecodeError::Corrupt("trailing bytes")));
    }

    /// Flipping any single byte never panics. (Most flips are caught; a flip
    /// inside a coordinate delta is a different, valid cell, which no format
    /// without a checksum can refuse — HTTP and gzip guard the transfer.)
    #[test]
    fn corrupt_bytes_never_panic() {
        let bytes = encode(&sample(), Grid::JSON).unwrap();
        for i in 0..bytes.len() {
            for flip in [0x01u8, 0x80, 0xff] {
                let mut b = bytes.clone();
                b[i] ^= flip;
                let _ = decode(&b);
            }
        }
    }

    #[test]
    fn hostile_headers_are_refused() {
        let good = encode(&sample(), Grid::JSON).unwrap();
        assert_eq!(decode(b"{\"elements\":[]}"), Err(DecodeError::NotACell));
        let mut v = good.clone();
        v[4] = 9;
        assert_eq!(decode(&v), Err(DecodeError::Version(9)));
        let mut z = good.clone();
        z[8..12].copy_from_slice(&0u32.to_le_bytes());
        assert_eq!(decode(&z), Err(DecodeError::ZeroGrid));
        // A meta stream claiming four billion elements must not try to allocate
        // them.
        let mut meta = Vec::new();
        for v in [0, 0, 0, u64::from(u32::MAX)] {
            put_varint(&mut meta, v);
        }
        let mut huge = good[..HEADER_LEN].to_vec();
        for len in [meta.len(), 0, 0, 0, 0] {
            put_varint(&mut huge, len as u64);
        }
        huge.extend_from_slice(&meta);
        assert_eq!(decode(&huge), Err(DecodeError::Truncated));
        // …nor stream lengths that claim more than the file holds…
        let mut short = good[..HEADER_LEN].to_vec();
        for len in [u64::MAX / 2, u64::MAX / 2, 0, 0, 0] {
            put_varint(&mut short, len);
        }
        assert!(decode(&short).is_err());
        // …nor an over-long varint be read past ten bytes.
        let mut long = good[..HEADER_LEN].to_vec();
        long.extend_from_slice(&[0xff; 12]);
        assert!(matches!(decode(&long), Err(DecodeError::Corrupt(_))));
    }

    /// The codes that make the triangle stream small: a new vertex, and a
    /// vertex of the previous triangle, are each one byte.
    #[test]
    fn a_wall_quad_costs_a_byte_an_index() {
        let cell = Cell {
            elements: vec![Element {
                id: String::new(),
                part: 0,
                class: String::new(),
                p: vec![(53.0, 6.0, 0.0); 4],
                t: vec![(0, 1, 2, 0), (2, 3, 0, 0)],
            }],
            pands: vec![],
        };
        let bytes = encode(&cell, Grid::JSON).unwrap();
        // The tri stream is last: six indices, six bytes.
        assert!(bytes.ends_with(&[0, 0, 0, 3 << 1, 0, 1]));
        assert_eq!(decode(&bytes).unwrap(), cell);
    }

    #[test]
    fn non_finite_coordinates_are_refused_at_encode() {
        let mut cell = sample();
        cell.elements[1].p[1].0 = f64::NAN;
        assert_eq!(
            encode(&cell, Grid::JSON),
            Err(EncodeError::Unrepresentable {
                element: 1,
                vertex: 1
            })
        );
        assert_eq!(
            encode(
                &sample(),
                Grid {
                    per_degree: 0,
                    per_metre: 100
                }
            ),
            Err(EncodeError::ZeroGrid)
        );
    }

    #[test]
    fn zigzag_and_varints_are_inverse() {
        for v in [
            0i64,
            1,
            -1,
            63,
            -64,
            1 << 40,
            -(1 << 40),
            i64::MAX,
            i64::MIN,
        ] {
            assert_eq!(unzigzag(zigzag(v)), v);
            let mut buf = Vec::new();
            put_varint(&mut buf, zigzag(v));
            let mut r = Reader { buf: &buf, at: 0 };
            assert_eq!(r.signed().unwrap(), v);
            assert_eq!(r.left(), 0);
        }
    }
}

//! Plane rings: is a point inside, and how large is it.
//!
//! These were written out nine times across the client and geodata, in four
//! point shapes (`Vec2`, `[f32; 2]`, `(f64, f64)` and two argument orders), two
//! without the guard an empty ring needs. One copy each now, generic over the
//! point shape: anything that converts into `Vec2` (`[f32; 2]` does) or, for
//! lat/lng pairs, into `DVec2` (`(f64, f64)` does).
//!
//! A ring is open or closed (first point repeated); the closing edge of a closed
//! ring is degenerate and changes neither answer.

use glam::{DVec2, Vec2};

/// Even-odd ray cast: is `p` inside `ring`? Fewer than three points enclose
/// nothing. A point exactly on an edge may answer either way.
#[must_use]
pub fn point_in_ring<P: Copy + Into<Vec2>>(ring: &[P], p: impl Into<Vec2>) -> bool {
    let p = p.into();
    let n = ring.len();
    if n < 3 {
        return false;
    }
    let mut inside = false;
    let mut j = n - 1;
    for i in 0..n {
        let (a, b): (Vec2, Vec2) = (ring[i].into(), ring[j].into());
        if (a.y > p.y) != (b.y > p.y) {
            let x = a.x + (p.y - a.y) / (b.y - a.y) * (b.x - a.x);
            if p.x < x {
                inside = !inside;
            }
        }
        j = i;
    }
    inside
}

/// [`point_in_ring`] in `f64`, for rings still in degrees: at f32 a lat/lng
/// keeps about a metre, too coarse to tell a wall's two sides apart.
#[must_use]
pub fn point_in_ring_f64<P: Copy + Into<DVec2>>(ring: &[P], p: impl Into<DVec2>) -> bool {
    let p = p.into();
    let n = ring.len();
    if n < 3 {
        return false;
    }
    let mut inside = false;
    let mut j = n - 1;
    for i in 0..n {
        let (a, b): (DVec2, DVec2) = (ring[i].into(), ring[j].into());
        if (a.y > p.y) != (b.y > p.y) {
            let x = a.x + (p.y - a.y) / (b.y - a.y) * (b.x - a.x);
            if p.x < x {
                inside = !inside;
            }
        }
        j = i;
    }
    inside
}

/// Shoelace area, positive when the ring runs counter-clockwise in a frame
/// whose second axis is "up". In the world frame +z runs south, so read the
/// sign there with care (`doors-and-rooms.md` probes instead).
#[must_use]
pub fn signed_area<P: Copy + Into<Vec2>>(ring: &[P]) -> f32 {
    let n = ring.len();
    let mut a = 0.0;
    for i in 0..n {
        let (p, q): (Vec2, Vec2) = (ring[i].into(), ring[(i + 1) % n].into());
        a += p.perp_dot(q);
    }
    a * 0.5
}

/// The unsigned area of a ring, in its own units squared.
#[must_use]
pub fn ring_area<P: Copy + Into<Vec2>>(ring: &[P]) -> f32 {
    signed_area(ring).abs()
}

/// [`ring_area`] in `f64`, for rings in degrees: squared degrees, which only
/// compare with another area at the same latitude.
#[must_use]
pub fn ring_area_f64<P: Copy + Into<DVec2>>(ring: &[P]) -> f64 {
    let n = ring.len();
    let mut a = 0.0;
    for i in 0..n {
        let (p, q): (DVec2, DVec2) = (ring[i].into(), ring[(i + 1) % n].into());
        a += p.perp_dot(q);
    }
    (a * 0.5).abs()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SQUARE: [[f32; 2]; 4] = [[0.0, 0.0], [2.0, 0.0], [2.0, 2.0], [0.0, 2.0]];

    #[test]
    fn inside_and_out() {
        assert!(point_in_ring(&SQUARE, [1.0, 1.0]));
        assert!(!point_in_ring(&SQUARE, [3.0, 1.0]));
        assert!(!point_in_ring(&SQUARE, [1.0, -0.1]));
        let v: Vec<Vec2> = SQUARE.iter().map(|&p| p.into()).collect();
        assert!(point_in_ring(&v, Vec2::new(1.9, 0.1)));
    }

    #[test]
    fn closed_ring_answers_as_open() {
        let mut closed = SQUARE.to_vec();
        closed.push(SQUARE[0]);
        assert!(point_in_ring(&closed, [1.0, 1.0]));
        assert_eq!(ring_area(&closed), ring_area(&SQUARE));
    }

    #[test]
    fn too_few_points_enclose_nothing() {
        assert!(!point_in_ring::<[f32; 2]>(&[], [0.0, 0.0]));
        assert!(!point_in_ring(&SQUARE[..2], [1.0, 0.0]));
        assert!(!point_in_ring_f64::<(f64, f64)>(&[], (0.0, 0.0)));
    }

    #[test]
    fn f64_matches_f32() {
        let deg: Vec<(f64, f64)> = SQUARE.iter().map(|p| (p[0] as f64, p[1] as f64)).collect();
        assert!(point_in_ring_f64(&deg, (1.0, 1.0)));
        assert!(!point_in_ring_f64(&deg, (2.5, 1.0)));
        assert_eq!(ring_area_f64(&deg), 4.0);
    }

    #[test]
    fn area_sign_is_winding() {
        assert_eq!(signed_area(&SQUARE), 4.0);
        let mut cw = SQUARE;
        cw.reverse();
        assert_eq!(signed_area(&cw), -4.0);
        assert_eq!(ring_area(&cw), 4.0);
    }
}

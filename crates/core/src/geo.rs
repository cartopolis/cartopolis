//! Geodetic ↔ ECEF ↔ local ENU tangent frame.
//!
//! This is the concrete flat-sim-to-globe attachment from ADR-020/021: a sim
//! world is pinned to the WGS84 ellipsoid at a lat/lng anchor, which defines a
//! flat **East-North-Up** tangent frame everything else is measured in. All maths
//! is f64; the client maps ENU → Bevy and handles render precision separately
//! (ADR-019).

use glam::DVec3;

// WGS84 ellipsoid.
const WGS84_A: f64 = 6_378_137.0; // semi-major axis (m)
const WGS84_F: f64 = 1.0 / 298.257_223_563; // flattening
const WGS84_E2: f64 = WGS84_F * (2.0 - WGS84_F); // first eccentricity squared

/// WGS84 geodetic (°lat, °lng, m height) → ECEF metres.
#[must_use]
pub fn geodetic_to_ecef(lat_deg: f64, lng_deg: f64, height_m: f64) -> DVec3 {
    let (lat, lng) = (lat_deg.to_radians(), lng_deg.to_radians());
    let (sin_lat, cos_lat) = lat.sin_cos();
    let (sin_lng, cos_lng) = lng.sin_cos();
    let n = WGS84_A / (1.0 - WGS84_E2 * sin_lat * sin_lat).sqrt();
    DVec3::new(
        (n + height_m) * cos_lat * cos_lng,
        (n + height_m) * cos_lat * sin_lng,
        (n * (1.0 - WGS84_E2) + height_m) * sin_lat,
    )
}

/// ECEF metres → WGS84 geodetic (°lat, °lng, m height). Bowring's closed-form
/// solution — accurate to well under a metre for near-surface points, which is
/// all the globe's sub-point lookup needs.
#[must_use]
pub fn ecef_to_geodetic(ecef: DVec3) -> (f64, f64, f64) {
    let (x, y, z) = (ecef.x, ecef.y, ecef.z);
    let b = WGS84_A * (1.0 - WGS84_F); // semi-minor axis
    let ep2 = (WGS84_A * WGS84_A - b * b) / (b * b); // second eccentricity²
    let p = (x * x + y * y).sqrt();
    let lng = y.atan2(x);
    let theta = (z * WGS84_A).atan2(p * b);
    let (st, ct) = theta.sin_cos();
    let lat = (z + ep2 * b * st * st * st).atan2(p - WGS84_E2 * WGS84_A * ct * ct * ct);
    let (sin_lat, cos_lat) = lat.sin_cos();
    let n = WGS84_A / (1.0 - WGS84_E2 * sin_lat * sin_lat).sqrt();
    let height = p / cos_lat - n;
    (lat.to_degrees(), lng.to_degrees(), height)
}

/// A local East-North-Up tangent frame pinned to the ellipsoid at an anchor —
/// how the flat sim frame attaches to the globe (ADR-020/021).
#[derive(Clone, Copy, Debug)]
pub struct TangentFrame {
    /// ECEF position of the anchor (local origin).
    pub origin: DVec3,
    pub east: DVec3,
    pub north: DVec3,
    pub up: DVec3,
}

impl TangentFrame {
    /// Build the ENU frame at a geodetic anchor.
    #[must_use]
    pub fn new(lat_deg: f64, lng_deg: f64, height_m: f64) -> Self {
        let (lat, lng) = (lat_deg.to_radians(), lng_deg.to_radians());
        let (sin_lat, cos_lat) = lat.sin_cos();
        let (sin_lng, cos_lng) = lng.sin_cos();
        Self {
            origin: geodetic_to_ecef(lat_deg, lng_deg, height_m),
            east: DVec3::new(-sin_lng, cos_lng, 0.0),
            north: DVec3::new(-sin_lat * cos_lng, -sin_lat * sin_lng, cos_lat),
            up: DVec3::new(cos_lat * cos_lng, cos_lat * sin_lng, sin_lat),
        }
    }

    /// ECEF point → local ENU metres relative to the anchor (x=E, y=N, z=U).
    #[must_use]
    pub fn world_to_local(&self, ecef: DVec3) -> DVec3 {
        let d = ecef - self.origin;
        DVec3::new(d.dot(self.east), d.dot(self.north), d.dot(self.up))
    }

    /// Local ENU metres (x=E, y=N, z=U) → ECEF point.
    #[must_use]
    pub fn local_to_world(&self, enu: DVec3) -> DVec3 {
        self.origin + self.east * enu.x + self.north * enu.y + self.up * enu.z
    }

    /// Convenience: geodetic → local ENU in this frame.
    #[must_use]
    pub fn geodetic_to_local(&self, lat_deg: f64, lng_deg: f64, height_m: f64) -> DVec3 {
        self.world_to_local(geodetic_to_ecef(lat_deg, lng_deg, height_m))
    }
}

/// Horizontal distance between two geographic points, in metres.
///
/// The ENU frame at the first point, which is exact for the ranges anything here
/// asks about (a script's sensor, an emitter's reach) and needs no special case
/// at a pole or across the antimeridian — both of which a naive
/// `Δlat, Δlng · cos(lat)` gets wrong, the second one silently and by the width
/// of the planet.
#[must_use]
pub fn ground_distance_m(lat_a: f64, lng_a: f64, lat_b: f64, lng_b: f64) -> f64 {
    let frame = TangentFrame::new(lat_a, lng_a, 0.0);
    let d = frame.geodetic_to_local(lat_b, lng_b, 0.0);
    d.truncate().length()
}

/// How many metres a degree of latitude and of longitude are worth at a latitude
/// — the local scale of the map, which is what turns a distance in metres into
/// an offset in degrees.
///
/// Measured rather than derived from a spherical formula: the two numbers are one
/// degree stepped in each direction through [`ground_distance_m`], so they carry
/// the ellipsoid's flattening. The longitude figure collapses to zero at the
/// poles, which is true and is also a division waiting to happen, so it is
/// floored at a millimetre per degree.
#[must_use]
pub fn degree_scale_m(lat_deg: f64) -> (f64, f64) {
    // Stepped *towards the equator* near a pole, or the latitude probe runs off
    // the end of the coordinate system at |lat| > 89.
    let step = if lat_deg >= 0.0 { -1.0 } else { 1.0 };
    let per_lat = ground_distance_m(lat_deg, 0.0, lat_deg + step, 0.0);
    let per_lng = ground_distance_m(lat_deg, 0.0, lat_deg, 1.0);
    (per_lat.max(1e-3), per_lng.max(1e-3))
}

#[cfg(test)]
mod tests {
    use super::*;
    use approx::assert_relative_eq;

    // Groningen Grote Markt — the running example from ADR-021.
    const LAT: f64 = 53.2194;
    const LNG: f64 = 6.5665;

    #[test]
    fn ecef_equator_magnitude() {
        let p = geodetic_to_ecef(0.0, 0.0, 0.0);
        assert_relative_eq!(p.length(), WGS84_A, epsilon = 1e-6);
        assert_relative_eq!(p.x, WGS84_A, epsilon = 1e-6);
    }

    #[test]
    fn geodetic_ecef_round_trips() {
        // A spread of latitudes/longitudes/heights round-trips through ECEF and
        // back — this backs the globe's sub-point lookup (ADR-025).
        for &(lat, lng, h) in &[
            (LAT, LNG, 0.0),
            (0.0, 0.0, 0.0),
            (-33.87, 151.21, 120.0), // Sydney
            (-51.65, 18.03, 0.0),    // Southern Ocean
            (78.22, 15.65, 500.0),   // Svalbard
        ] {
            let (rlat, rlng, rh) = ecef_to_geodetic(geodetic_to_ecef(lat, lng, h));
            assert_relative_eq!(rlat, lat, epsilon = 1e-7);
            assert_relative_eq!(rlng, lng, epsilon = 1e-7);
            assert_relative_eq!(rh, h, epsilon = 1e-3);
        }
    }

    #[test]
    fn enu_basis_orthonormal() {
        let f = TangentFrame::new(LAT, LNG, 0.0);
        assert_relative_eq!(f.east.length(), 1.0, epsilon = 1e-12);
        assert_relative_eq!(f.north.length(), 1.0, epsilon = 1e-12);
        assert_relative_eq!(f.up.length(), 1.0, epsilon = 1e-12);
        assert_relative_eq!(f.east.dot(f.north), 0.0, epsilon = 1e-9);
        assert_relative_eq!(f.east.dot(f.up), 0.0, epsilon = 1e-9);
        assert_relative_eq!(f.north.dot(f.up), 0.0, epsilon = 1e-9);
    }

    #[test]
    fn local_round_trip() {
        let f = TangentFrame::new(LAT, LNG, 0.0);
        let enu = DVec3::new(123.0, -456.0, 7.0);
        let back = f.world_to_local(f.local_to_world(enu));
        assert_relative_eq!(back.x, enu.x, epsilon = 1e-6);
        assert_relative_eq!(back.y, enu.y, epsilon = 1e-6);
        assert_relative_eq!(back.z, enu.z, epsilon = 1e-6);
    }

    #[test]
    fn anchor_maps_to_local_origin() {
        let f = TangentFrame::new(LAT, LNG, 0.0);
        let local = f.geodetic_to_local(LAT, LNG, 0.0);
        assert_relative_eq!(local.length(), 0.0, epsilon = 1e-6);
    }

    #[test]
    fn eastward_step_is_real_metres() {
        let f = TangentFrame::new(LAT, LNG, 0.0);
        let local = f.geodetic_to_local(LAT, LNG + 0.001, 0.0);
        assert!(local.x > 0.0, "east increases with longitude");
        assert!(local.y.abs() < 1.0, "negligible north component");
        // ENU is a rigid rotation, so local distance == ECEF distance.
        let p0 = geodetic_to_ecef(LAT, LNG, 0.0);
        let p1 = geodetic_to_ecef(LAT, LNG + 0.001, 0.0);
        assert_relative_eq!(local.length(), (p1 - p0).length(), epsilon = 1e-6);
    }

    #[test]
    fn ground_distance_is_symmetric_and_zero_at_a_point() {
        assert_relative_eq!(ground_distance_m(LAT, LNG, LAT, LNG), 0.0, epsilon = 1e-6);
        let a = ground_distance_m(LAT, LNG, LAT + 0.01, LNG + 0.01);
        let b = ground_distance_m(LAT + 0.01, LNG + 0.01, LAT, LNG);
        assert_relative_eq!(a, b, epsilon = 1e-3);
    }

    /// The case a `Δlng · cos(lat)` formula gets wrong by 40,000 km: two points a
    /// few hundred metres apart with the antimeridian between them.
    #[test]
    fn ground_distance_crosses_the_antimeridian() {
        let d = ground_distance_m(0.0, 179.999, 0.0, -179.999);
        assert!(d < 500.0, "expected a couple of hundred metres, got {d}");
    }

    /// The scale a script uses to turn metres into degrees — a degree of latitude
    /// is ~111 km everywhere, and a degree of longitude shrinks with the cosine.
    #[test]
    fn degree_scale_matches_the_textbook_numbers() {
        let (per_lat, per_lng) = degree_scale_m(LAT);
        assert!((110_000.0..112_000.0).contains(&per_lat), "{per_lat}");
        let expected_lng = 111_320.0 * LAT.to_radians().cos();
        assert_relative_eq!(per_lng, expected_lng, max_relative = 0.01);
    }

    /// Near a pole the longitude scale collapses; a script dividing by it must
    /// not divide by zero.
    #[test]
    fn degree_scale_is_never_zero() {
        for lat in [89.999_f64, -89.999, 90.0, -90.0] {
            let (per_lat, per_lng) = degree_scale_m(lat);
            assert!(
                per_lat > 0.0 && per_lng > 0.0,
                "at {lat}: {per_lat} {per_lng}"
            );
        }
    }
}

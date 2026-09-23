//! RD New (Amersfoort, EPSG:28992) → WGS84, exactly as cartopy's pipeline did it.
//!
//! **A line-for-line port of proj4js, not a fresh implementation of the
//! projection.** Every coordinate in every file the pipelines write goes through
//! this, and the served tiles were produced by proj4js (and before that by pyproj,
//! which agreed with it to ~3e-10 degrees because neither had the NSGI grid and
//! both fell back to the same seven-parameter Helmert). A cleaner derivation
//! that differed in the last digits would change every coordinate in every
//! served cell for no gain, and a diff against the old output could no longer
//! tell a porting mistake from a rounding one. The reference points in the tests
//! were produced by cartopy's own proj4 on the VPS.
//!
//! The chain is proj4js's: oblique stereographic inverse onto the conformal
//! (Gauss) sphere, back to Bessel 1841 latitude, then geodetic → geocentric,
//! Helmert to WGS84, geocentric → geodetic on WGS84.

use std::f64::consts::{FRAC_PI_2, FRAC_PI_4, PI};

const SEC_TO_RAD: f64 = 4.848_136_811_095_359_935_899_141_023_57e-6;

/// `+lat_0` / `+lon_0` / `+k` / `+x_0` / `+y_0` of the RD New definition.
const LAT0_DEG: f64 = 52.156_160_555_555_6;
const LON0_DEG: f64 = 5.387_638_888_888_89;
const K0: f64 = 0.999_907_9;
const X0: f64 = 155_000.0;
const Y0: f64 = 463_000.0;

/// `+towgs84`, as written in the definition: metres, arc-seconds, ppm.
const TOWGS84: [f64; 7] = [
    565.4171, 50.3319, 465.5524, -0.398957, 0.343988, -1.87740, 4.0725,
];

struct Ellipsoid {
    a: f64,
    b: f64,
    es: f64,
    e: f64,
}

impl Ellipsoid {
    /// proj4js's `sphere()` + `eccentricity()`: `b` from the flattening, `es`
    /// from `a` and `b`.
    fn new(a: f64, rf: f64) -> Self {
        let b = (1.0 - 1.0 / rf) * a;
        let es = (a * a - b * b) / (a * a);
        Self {
            a,
            b,
            es,
            e: es.sqrt(),
        }
    }
}

fn bessel() -> Ellipsoid {
    Ellipsoid::new(6_377_397.155, 299.152_812_8)
}

fn wgs84() -> Ellipsoid {
    Ellipsoid::new(6_378_137.0, 298.257_223_563)
}

fn srat(esinp: f64, exp: f64) -> f64 {
    ((1.0 - esinp) / (1.0 + esinp)).powf(exp)
}

/// proj4js's `hypot`, which is not `f64::hypot` in the last bit.
fn hypot(x: f64, y: f64) -> f64 {
    let (x, y) = (x.abs(), y.abs());
    let a = x.max(y);
    let b = x.min(y) / if a != 0.0 { a } else { 1.0 };
    a * (1.0 + b.powi(2)).sqrt()
}

/// RD easting/northing → `(lat, lng)` in degrees, the order every output file uses.
#[must_use]
pub fn rd_to_lat_lng(x: f64, y: f64) -> (f64, f64) {
    let ell = bessel();
    let lat0 = LAT0_DEG.to_radians_proj();
    let long0 = LON0_DEG.to_radians_proj();

    // gauss.init
    let sphi = lat0.sin();
    let mut cphi = lat0.cos();
    cphi *= cphi;
    let rc = (1.0 - ell.es).sqrt() / (1.0 - ell.es * sphi * sphi);
    let c = (1.0 + ell.es * cphi * cphi / (1.0 - ell.es)).sqrt();
    let phic0 = (sphi / c).asin();
    let ratexp = 0.5 * c * ell.e;
    let k = (0.5 * phic0 + FRAC_PI_4).tan()
        / ((0.5 * lat0 + FRAC_PI_4).tan().powf(c) * srat(ell.e * sphi, ratexp));
    // sterea.init
    let sinc0 = phic0.sin();
    let cosc0 = phic0.cos();
    let r2 = 2.0 * rc;

    // sterea.inverse
    let mut px = (x - X0) / ell.a / K0;
    let mut py = (y - Y0) / ell.a / K0;
    let rho = hypot(px, py);
    let (lon, lat) = if rho != 0.0 {
        let cc = 2.0 * rho.atan2(r2);
        let (sinc, cosc) = (cc.sin(), cc.cos());
        let lat = (cosc * sinc0 + py * sinc * cosc0 / rho).asin();
        let lon = (px * sinc).atan2(rho * cosc0 * cosc - py * sinc0 * sinc);
        (lon, lat)
    } else {
        (0.0, phic0)
    };
    px = lon;
    py = lat;

    // gauss.inverse
    let lon = px / c;
    let num = ((0.5 * py + FRAC_PI_4).tan() / k).powf(1.0 / c);
    let mut lat = py;
    for _ in 0..20 {
        lat = 2.0 * (num * srat(ell.e * py.sin(), -0.5 * ell.e)).atan() - FRAC_PI_2;
        if (lat - py).abs() < 1e-14 {
            break;
        }
        py = lat;
    }
    let mut lon = lon + long0;
    if lon.abs() > PI {
        lon -= lon.signum() * 2.0 * PI;
    }

    // datum_transform: Bessel (7-parameter) → WGS84.
    let [dx, dy, dz, rx, ry, rz, m] = TOWGS84;
    let (rx, ry, rz) = (rx * SEC_TO_RAD, ry * SEC_TO_RAD, rz * SEC_TO_RAD);
    let m = m / 1_000_000.0 + 1.0;
    let (gx, gy, gz) = geodetic_to_geocentric(lon, lat, 0.0, ell.es, ell.a);
    let wx = m * (gx - rz * gy + ry * gz) + dx;
    let wy = m * (rz * gx + gy - rx * gz) + dy;
    let wz = m * (-ry * gx + rx * gy + gz) + dz;
    let w = wgs84();
    let (lng, lat) = geocentric_to_geodetic(wx, wy, wz, w.es, w.a, w.b);
    (lat.to_degrees_proj(), lng.to_degrees_proj())
}

fn geodetic_to_geocentric(lon: f64, lat: f64, h: f64, es: f64, a: f64) -> (f64, f64, f64) {
    let lon = if lon > PI { lon - 2.0 * PI } else { lon };
    let (sin_lat, cos_lat) = (lat.sin(), lat.cos());
    let rn = a / (1.0 - es * sin_lat * sin_lat).sqrt();
    (
        (rn + h) * cos_lat * lon.cos(),
        (rn + h) * cos_lat * lon.sin(),
        ((rn * (1.0 - es)) + h) * sin_lat,
    )
}

/// The Hannover iteration proj4js uses. Returns `(lon, lat)` in radians.
fn geocentric_to_geodetic(x: f64, y: f64, z: f64, es: f64, a: f64, b: f64) -> (f64, f64) {
    let genau = 1e-12;
    let genau2 = genau * genau;
    let p = (x * x + y * y).sqrt();
    let rr = (x * x + y * y + z * z).sqrt();
    let lon = if p / a < genau {
        if rr / a < genau {
            let _ = b;
            return (0.0, FRAC_PI_2);
        }
        0.0
    } else {
        y.atan2(x)
    };
    let ct = z / rr;
    let st = p / rr;
    let mut rx = 1.0 / (1.0 - es * (2.0 - es) * st * st).sqrt();
    let mut cphi0 = st * (1.0 - es) * rx;
    let mut sphi0 = ct * rx;
    let (mut cphi, mut sphi);
    let mut iter = 0;
    loop {
        iter += 1;
        let rn = a / (1.0 - es * sphi0 * sphi0).sqrt();
        let height = p * cphi0 + z * sphi0 - rn * (1.0 - es * sphi0 * sphi0);
        let rk = es * rn / (rn + height);
        rx = 1.0 / (1.0 - rk * (2.0 - rk) * st * st).sqrt();
        cphi = st * (1.0 - rk) * rx;
        sphi = ct * rx;
        let sdphi = sphi * cphi0 - cphi * sphi0;
        cphi0 = cphi;
        sphi0 = sphi;
        if !(sdphi * sdphi > genau2 && iter < 30) {
            break;
        }
    }
    (lon, (sphi / cphi.abs()).atan())
}

/// proj4js converts with its own constants rather than `to_radians`, and the two
/// can differ in the last bit.
trait ProjAngle {
    fn to_radians_proj(self) -> f64;
    fn to_degrees_proj(self) -> f64;
}

impl ProjAngle for f64 {
    fn to_radians_proj(self) -> f64 {
        self * 0.017_453_292_519_943_295_77
    }
    fn to_degrees_proj(self) -> f64 {
        self * 57.295_779_513_082_320_88
    }
}

/// The WGS84 envelope of an RD rectangle, from **all four** corners:
/// `[min lng, min lat, max lng, max lat]`.
///
/// RD → WGS84 rotates as well as scales, so the envelope of a rotated rectangle is
/// not the envelope of its south-west and north-east corners alone.
#[must_use]
pub fn wgs84_bbox(bbox: [f64; 4]) -> [f64; 4] {
    let [xmin, ymin, xmax, ymax] = bbox;
    let mut out = [
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    ];
    for (x, y) in [(xmin, ymin), (xmax, ymin), (xmin, ymax), (xmax, ymax)] {
        let (lat, lng) = rd_to_lat_lng(x, y);
        out = [
            out[0].min(lng),
            out[1].min(lat),
            out[2].max(lng),
            out[3].max(lat),
        ];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `[x, y, lat, lng]` from cartopy's proj4 (proj4js, the library that produced
    /// the served tiles), over a 7×7 grid spanning the whole of RD's extent.
    const REFERENCE: [[f64; 4]; 49] = [
        [13000.123, 306000.456, 50.72645541800996, 3.37610376001428],
        [13000.123, 370000.456, 51.30144729496814, 3.350870846683535],
        [13000.123, 430000.456, 51.84048268364334, 3.3265399017433244],
        [13000.123, 463000.456, 52.13693522818495, 3.3128694607820095],
        [13000.123, 520000.456, 52.648947171553544, 3.288758098222602],
        [13000.123, 582000.456, 53.20579075055305, 3.2617888485549766],
        [13000.123, 619000.456, 53.53804956124029, 3.2453119114173705],
        [60000.123, 306000.456, 50.73613163996332, 4.041531947499893],
        [60000.123, 370000.456, 51.31128349783057, 4.024642056265314],
        [60000.123, 430000.456, 51.850472556979085, 4.008355652131479],
        [60000.123, 463000.456, 52.1470111954641, 3.999204958727086],
        [60000.123, 520000.456, 52.65917456967616, 3.983065145380238],
        [60000.123, 582000.456, 53.21618691730483, 3.9650120022776982],
        [60000.123, 619000.456, 53.548548531142195, 3.953982227524296],
        [
            110000.123,
            306000.456,
            50.742213674215826,
            4.749713941334896,
        ],
        [
            110000.123,
            370000.456,
            51.317466130426084,
            4.741711747716606,
        ],
        [
            110000.123,
            430000.456,
            51.856751821644785,
            4.733995397162942,
        ],
        [110000.123, 463000.456, 52.15334459907808, 4.729659846616767],
        [110000.123, 520000.456, 52.66560319981877, 4.722012825832664],
        [110000.123, 582000.456, 53.22272167889668, 4.713459177810705],
        [110000.123, 619000.456, 53.55514794240884, 4.708233179070862],
        [
            155000.123,
            306000.456,
            50.743972748567074,
            5.387200184215664,
        ],
        [155000.123, 370000.456, 51.31925430972403, 5.387202237131587],
        [155000.123, 430000.456, 51.8585679585971, 5.3872042051305105],
        [
            155000.123,
            463000.456,
            52.15517639964434,
            5.3872053060885845,
        ],
        [155000.123, 520000.456, 52.66746255173311, 5.387207239871601],
        [155000.123, 582000.456, 53.22461173748488, 5.387209391183935],
        [155000.123, 619000.456, 53.55705670602249, 5.387210699713195],
        [200000.123, 306000.456, 50.74221186328029, 6.024686404470581],
        [200000.123, 370000.456, 51.31746430073593, 6.032692702905909],
        [200000.123, 430000.456, 51.85674997381116, 6.040412988452049],
        [200000.123, 463000.456, 52.15334274102574, 6.044750740337798],
        [
            200000.123,
            520000.456,
            52.66560132370101,
            6.0524016276502275,
        ],
        [
            200000.123,
            582000.456,
            53.222719782510644,
            6.060959577104409,
        ],
        [200000.123, 619000.456, 53.5551460336089, 6.066188192157544],
        [233000.123, 306000.456, 50.7386837463305, 6.492111492486332],
        [
            233000.123,
            370000.456,
            51.313877831911235,
            6.505984755809516,
        ],
        [233000.123, 430000.456, 51.85310745347825, 6.519362351310529],
        [
            233000.123,
            463000.456,
            52.14966881724073,
            6.5268787138373465,
        ],
        [233000.123, 520000.456, 52.66187216329874, 6.540135953667298],
        [233000.123, 582000.456, 53.21892905974192, 6.554964844948816],
        [
            233000.123,
            619000.456,
            53.551317810143786,
            6.564024749136909,
        ],
        [
            278000.123,
            306000.456,
            50.730823907724634,
            7.1293327002233084,
        ],
        [
            278000.123,
            370000.456,
            51.305888031583876,
            7.151198699766367,
        ],
        [278000.123, 430000.456, 51.84499281659191, 7.172283208271591],
        [278000.123, 463000.456, 52.14148424011673, 7.184129680419411],
        [278000.123, 520000.456, 52.65356456723099, 7.205024099569244],
        [
            278000.123,
            582000.456,
            53.210484358555576,
            7.228395256798783,
        ],
        [278000.123, 619000.456, 53.54278959250209, 7.2426740094256],
    ];

    /// Within 1e-11 degrees — about a millimetre — of proj4js everywhere in the
    /// country. The pipelines round coordinates to 1e-7 degrees, so this is far
    /// inside what can change a served byte.
    #[test]
    fn matches_proj4js_across_the_country() {
        let mut worst = 0.0f64;
        for [x, y, lat, lng] in REFERENCE {
            let (a, b) = rd_to_lat_lng(x, y);
            worst = worst.max((a - lat).abs()).max((b - lng).abs());
        }
        assert!(
            worst < 1e-11,
            "worst disagreement with proj4js: {worst:e} degrees"
        );
    }

    #[test]
    fn the_envelope_uses_all_four_corners() {
        let e = wgs84_bbox([230_000.0, 578_000.0, 238_000.0, 586_000.0]);
        for (x, y) in [
            (230_000.0, 578_000.0),
            (238_000.0, 586_000.0),
            (230_000.0, 586_000.0),
        ] {
            let (lat, lng) = rd_to_lat_lng(x, y);
            assert!(lng >= e[0] && lng <= e[2] && lat >= e[1] && lat <= e[3]);
        }
    }
}

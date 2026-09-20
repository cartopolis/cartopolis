//! Measured Dutch ground, from AHN, without an engine around it.
//!
//! The client fetches this through its own async plumbing and a Bevy resource
//! (`systems::map::height`). The **harness cannot**: it does not depend on the
//! client, and the whole point of it is to look at the ground in seconds. So the
//! two halves that are not about scheduling — what to ask AHN for, and how to
//! read the answer — live here and are shared.
//!
//! Everything here is pure and blocking. There is no cache, no retry and no task
//! pool: one request, one raster. The client keeps all of that, because it is
//! streaming while somebody flies a camera; a harness run asks once and exits.

/// WGS84 → **RD New** (EPSG:28992), the Dutch national grid, in metres.
///
/// The published polynomial about the Amersfoort datum point — a few centimetres
/// inside the Netherlands and meaningless outside it, which is exactly AHN's own
/// domain. Duplicated from nothing: the client re-exports this one.
#[must_use]
#[allow(clippy::unreadable_literal)]
pub fn wgs84_to_rd(lat: f64, lng: f64) -> [f64; 2] {
    let d_lat = 0.36 * (lat - 52.15517440);
    let d_lng = 0.36 * (lng - 5.38720621);
    let x = 155000.0 + 190094.945 * d_lng
        - 11832.228 * d_lat * d_lng
        - 114.221 * d_lng * d_lat * d_lat
        - 32.391 * d_lng * d_lng * d_lng
        - 0.705 * d_lat
        - 2.340 * d_lat * d_lat * d_lat * d_lng
        - 0.608 * d_lat * d_lng * d_lng * d_lng
        - 0.008 * d_lng * d_lng
        + 0.148 * d_lat * d_lat * d_lat * d_lng * d_lng * d_lng;
    let y = 463000.0 + 309056.544 * d_lat + 3638.893 * d_lng * d_lng + 73.077 * d_lat * d_lat
        - 157.984 * d_lat * d_lng * d_lng
        + 59.788 * d_lat * d_lat * d_lat
        + 0.433 * d_lng
        - 6.439 * d_lat * d_lat * d_lng * d_lng
        - 0.032 * d_lat * d_lng
        + 0.092 * d_lng * d_lng * d_lng * d_lng
        - 0.054 * d_lat * d_lng * d_lng * d_lng * d_lng;
    [x, y]
}

/// The WCS request for one square of ground, in RD metres.
///
/// Built as a whole URL rather than pieces, so a harness can print it and a
/// person can paste it into a browser — which is how a refusal gets diagnosed.
#[must_use]
pub fn coverage_url(min: [f64; 2], max: [f64; 2], grid: u32) -> String {
    format!(
        "https://service.pdok.nl/rws/ahn/wcs/v1_0?SERVICE=WCS&VERSION=1.0.0\
         &REQUEST=GetCoverage&COVERAGE=dtm_05m&CRS=EPSG:28992&FORMAT=GEOTIFF\
         &BBOX={:.2},{:.2},{:.2},{:.2}&WIDTH={grid}&HEIGHT={grid}",
        min[0], min[1], max[0], max[1]
    )
}

/// Heights above the service's own no-data floor are holes, not ground.
const NODATA_ABOVE: f32 = 1.0e30;

/// Read a float GeoTIFF into `grid²` samples, `NaN` where the service has no data.
///
/// **An OGC service answers a refused request with an XML exception and HTTP
/// 200**, so "not a TIFF" is the ordinary error path rather than a corruption
/// case, and the message has to carry what the service actually said.
pub fn decode_float_tiff(bytes: &[u8], grid: usize) -> Result<Vec<f32>, String> {
    if bytes.len() < 4 || !(bytes.starts_with(b"II*\0") || bytes.starts_with(b"MM\0*")) {
        let text = String::from_utf8_lossy(&bytes[..bytes.len().min(400)]).to_string();
        return Err(format!("not a GeoTIFF: {}", text.trim()));
    }
    let mut decoder = tiff::decoder::Decoder::new(std::io::Cursor::new(bytes))
        .map_err(|e| format!("tiff open: {e}"))?;
    let (w, h) = decoder
        .dimensions()
        .map_err(|e| format!("tiff dims: {e}"))?;
    if w as usize != grid || h as usize != grid {
        return Err(format!("asked for {grid}², got {w}x{h}"));
    }
    let image = decoder
        .read_image()
        .map_err(|e| format!("tiff read: {e}"))?;
    let tiff::decoder::DecodingResult::F32(mut v) = image else {
        return Err("height raster is not float32".to_string());
    };
    for s in &mut v {
        if !s.is_finite() || *s >= NODATA_ABOVE {
            *s = f32::NAN;
        }
    }
    Ok(v)
}

/// AHN's own envelope in RD metres — outside it there is no data to ask for.
#[must_use]
pub fn covers(lat: f64, lng: f64) -> bool {
    let [x, y] = wgs84_to_rd(lat, lng);
    (0.0..=290000.0).contains(&x) && (300000.0..=630000.0).contains(&y)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The datum point maps to the grid origin it is defined by.
    #[test]
    fn amersfoort_is_the_origin_of_the_grid() {
        let [x, y] = wgs84_to_rd(52.15517440, 5.38720621);
        assert!((x - 155000.0).abs() < 0.5, "x {x}");
        assert!((y - 463000.0).abs() < 0.5, "y {y}");
    }

    /// Groningen is inside the envelope and Berlin is not.
    #[test]
    fn the_envelope_is_the_netherlands() {
        assert!(covers(53.2194, 6.5665), "Groningen");
        assert!(!covers(52.52, 13.405), "Berlin");
    }

    /// An XML exception is the ordinary refusal and must be reported as itself.
    #[test]
    fn a_service_exception_is_not_a_corrupt_tiff() {
        let err = decode_float_tiff(b"<ServiceExceptionReport>no such coverage", 4)
            .expect_err("XML is not a TIFF");
        assert!(err.contains("ServiceException"), "{err}");
    }
}

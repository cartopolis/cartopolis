//! The phone's raw GNSS measurements, for the high-precision mode
//! (`docs/notes/high-precision-mode.md`).
//!
//! Android's location fix is a finished answer, metres off in a street. Under
//! it the chip measures, per satellite and per frequency, a range from the
//! code (metres of noise) and the **carrier phase** — the count of radio
//! waves, millimetres of noise but an unknown whole number of them, which a
//! nearby reference station resolves (RTK). Whether a phone reports usable
//! carrier phase at all is a property of its chip and its vendor's driver,
//! and Snapdragon's record is mixed, so before any solver is written the mode
//! measures it: [`RawStats`] counts, per band, how much of the phase was
//! valid, how often it broke, and whether the half-cycle ambiguity was
//! resolved, into the mode's log.
//!
//! The listener is Java (`GnssRaw.java`: Android hands measurements only to a
//! subclass of a framework type), which packs each measurement into a row of
//! longs; [`RawMeasurement::from_row`] reads one back. Android only; elsewhere
//! [`RawGnss`] never starts and everything still compiles.

#![cfg_attr(not(target_os = "android"), allow(dead_code))]

use std::collections::BTreeSet;

/// Longs per row — `GnssRaw.STRIDE`, and the two must change together.
pub const STRIDE: usize = 21;

/// `GnssMeasurement.ADR_STATE_*`.
pub mod adr {
    pub const VALID: i64 = 1 << 0;
    pub const RESET: i64 = 1 << 1;
    pub const CYCLE_SLIP: i64 = 1 << 2;
    pub const HALF_CYCLE_RESOLVED: i64 = 1 << 3;
}

/// `GnssMeasurement.STATE_*` bits that say the satellite's transmit time is
/// known well enough to make a pseudorange: time of week decoded or known
/// (GPS, Galileo, BeiDou, QZSS), or time of day (GLONASS).
const TIME_KNOWN: i64 = (1 << 3) | (1 << 14) | (1 << 7) | (1 << 15);

/// One satellite signal at one epoch, as `GnssRaw.java` packed it. Clock
/// fields repeat on every row of an epoch.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RawMeasurement {
    /// `CLOCK_BOOTTIME` ns of the epoch (0 when the platform does not say) —
    /// the motion sensors' clock.
    pub elapsed_ns: i64,
    pub time_nanos: i64,
    pub full_bias_nanos: i64,
    pub bias_nanos: f64,
    pub clock_discontinuities: i64,
    /// `GnssStatus.CONSTELLATION_*`.
    pub constellation: i64,
    pub svid: i64,
    /// 0 when the chip does not say.
    pub carrier_hz: f64,
    pub cn0_dbhz: f64,
    pub state: i64,
    pub received_sv_time_ns: i64,
    pub received_sv_time_uncertainty_ns: i64,
    pub time_offset_ns: f64,
    pub pseudorange_rate_mps: f64,
    pub pseudorange_rate_uncertainty_mps: f64,
    pub adr_state: i64,
    pub adr_m: f64,
    pub adr_uncertainty_m: f64,
    pub multipath: i64,
    pub agc_db: f64,
    pub drift_ns_per_s: f64,
}

impl RawMeasurement {
    #[must_use]
    pub fn from_row(r: &[i64; STRIDE]) -> Self {
        let f = |i: usize| f64::from_bits(r[i] as u64);
        Self {
            elapsed_ns: r[0],
            time_nanos: r[1],
            full_bias_nanos: r[2],
            bias_nanos: f(3),
            clock_discontinuities: r[4],
            constellation: r[5],
            svid: r[6],
            carrier_hz: f(7),
            cn0_dbhz: f(8),
            state: r[9],
            received_sv_time_ns: r[10],
            received_sv_time_uncertainty_ns: r[11],
            time_offset_ns: f(12),
            pseudorange_rate_mps: f(13),
            pseudorange_rate_uncertainty_mps: f(14),
            adr_state: r[15],
            adr_m: f(16),
            adr_uncertainty_m: f(17),
            multipath: r[18],
            agc_db: f(19),
            drift_ns_per_s: f(20),
        }
    }

    /// Read a drained array; a trailing partial row is dropped.
    #[must_use]
    pub fn from_flat(flat: &[i64]) -> Vec<Self> {
        flat.chunks_exact(STRIDE)
            .map(|c| Self::from_row(c.try_into().expect("chunks_exact gives STRIDE")))
            .collect()
    }

    #[must_use]
    pub fn band(&self) -> Band {
        Band::of(self.carrier_hz)
    }

    /// The carrier phase is there and continuous: valid, and neither reset nor
    /// slipped at this epoch.
    #[must_use]
    pub const fn phase_ok(&self) -> bool {
        self.adr_state & adr::VALID != 0 && self.adr_state & (adr::RESET | adr::CYCLE_SLIP) == 0
    }

    #[must_use]
    pub const fn time_known(&self) -> bool {
        self.state & TIME_KNOWN != 0
    }

    /// One letter per constellation, RINEX's.
    #[must_use]
    pub const fn system_letter(&self) -> char {
        match self.constellation {
            1 => 'G',
            2 => 'S',
            3 => 'R',
            4 => 'J',
            5 => 'C',
            6 => 'E',
            7 => 'I',
            _ => '?',
        }
    }
}

/// The two frequency groups a phone tracks. E5b and B2I (~1.2 GHz) count as
/// the lower band; the bands differ in what the chip does with them, not in
/// the exact frequency.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Band {
    /// L1 / E1 / B1 / G1, ~1.56–1.61 GHz.
    L1,
    /// L5 / E5a / B2a, ~1.18–1.21 GHz.
    L5,
    /// The chip did not say.
    Unknown,
}

impl Band {
    #[must_use]
    pub fn of(hz: f64) -> Self {
        if hz > 1.5e9 {
            Self::L1
        } else if hz > 1.1e9 {
            Self::L5
        } else {
            Self::Unknown
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::L1 => "L1",
            Self::L5 => "L5",
            Self::Unknown => "band?",
        }
    }
}

/// One band's tally over a log window.
#[derive(Debug, Default, Clone)]
struct BandTally {
    signals: u32,
    sats: BTreeSet<(i64, i64)>,
    cn0_sum: f64,
    time_known: u32,
    phase_valid: u32,
    phase_ok: u32,
    slips: u32,
    resets: u32,
    half_resolved: u32,
}

/// What the raw measurements showed since the last log line.
#[derive(Debug, Default, Clone)]
pub struct RawStats {
    bands: std::collections::BTreeMap<Band, BandTally>,
    systems: std::collections::BTreeMap<char, BTreeSet<i64>>,
    epochs: BTreeSet<i64>,
    first_discontinuity: Option<i64>,
    last_discontinuity: i64,
    /// Measurement age at the moment the app read it, summed, in ms.
    age_ms_sum: f64,
    age_n: u32,
    no_full_bias: u32,
}

impl RawStats {
    /// Count a batch, read at `now_ns` (`CLOCK_BOOTTIME`).
    pub fn add(&mut self, batch: &[RawMeasurement], now_ns: i64) {
        for m in batch {
            self.epochs.insert(m.time_nanos);
            if self.first_discontinuity.is_none() {
                self.first_discontinuity = Some(m.clock_discontinuities);
            }
            self.last_discontinuity = m.clock_discontinuities;
            if m.full_bias_nanos == 0 {
                self.no_full_bias += 1;
            }
            if m.elapsed_ns > 0 {
                self.age_ms_sum += (now_ns - m.elapsed_ns) as f64 / 1e6;
                self.age_n += 1;
            }
            self.systems
                .entry(m.system_letter())
                .or_default()
                .insert(m.svid);
            let t = self.bands.entry(m.band()).or_default();
            t.signals += 1;
            t.sats.insert((m.constellation, m.svid));
            t.cn0_sum += m.cn0_dbhz;
            t.time_known += u32::from(m.time_known());
            if m.adr_state & adr::VALID != 0 {
                t.phase_valid += 1;
                t.half_resolved += u32::from(m.adr_state & adr::HALF_CYCLE_RESOLVED != 0);
            }
            t.phase_ok += u32::from(m.phase_ok());
            t.slips += u32::from(m.adr_state & adr::CYCLE_SLIP != 0);
            t.resets += u32::from(m.adr_state & adr::RESET != 0);
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.epochs.is_empty()
    }

    /// One log line over `span_s` seconds, or `None` if nothing arrived.
    #[must_use]
    pub fn line(&self, span_s: f32) -> Option<String> {
        if self.is_empty() {
            return None;
        }
        let mut s = format!("raw · {:.1} epochs/s", self.epochs.len() as f32 / span_s);
        let systems: String = self
            .systems
            .iter()
            .map(|(c, sv)| format!("{c}{}", sv.len()))
            .collect::<Vec<_>>()
            .join(" ");
        s.push_str(&format!(" · sats {systems}"));
        for (band, t) in &self.bands {
            let pct = |n: u32| 100.0 * f64::from(n) / f64::from(t.signals.max(1));
            s.push_str(&format!(
                " · {} {} sats cn0 {:.0} time {:.0}% phase valid {:.0}% ok {:.0}% slip {} reset {}",
                band.label(),
                t.sats.len(),
                t.cn0_sum / f64::from(t.signals.max(1)),
                pct(t.time_known),
                pct(t.phase_valid),
                pct(t.phase_ok),
                t.slips,
                t.resets,
            ));
            if t.phase_valid > 0 {
                s.push_str(&format!(
                    " half-cycle resolved {:.0}%",
                    100.0 * f64::from(t.half_resolved) / f64::from(t.phase_valid)
                ));
            }
        }
        let jumps = self.last_discontinuity - self.first_discontinuity.unwrap_or(0);
        s.push_str(&format!(" · clock jumps {jumps}"));
        if self.no_full_bias > 0 {
            s.push_str(&format!(" · {} without full bias", self.no_full_bias));
        }
        if self.age_n > 0 {
            s.push_str(&format!(
                " · age {:.0} ms",
                self.age_ms_sum / f64::from(self.age_n)
            ));
        }
        Some(s)
    }
}

/// What `GnssMeasurementsEvent.Callback` last said, in words.
#[must_use]
pub const fn status_text(status: i32) -> &'static str {
    match status {
        0 => "not supported by this phone",
        1 => "ready",
        2 => "location is switched off",
        _ => "no word yet",
    }
}

/// The listener, registered. Dropping it unregisters.
pub struct RawGnss {
    running: bool,
}

impl RawGnss {
    #[must_use]
    pub const fn new() -> Self {
        Self { running: false }
    }

    #[must_use]
    pub const fn running(&self) -> bool {
        self.running
    }

    /// Register if not yet registered. `Err` says why not; the caller tries
    /// again later (the location permission is usually still being asked).
    pub fn ensure_started(&mut self) -> Result<(), String> {
        if self.running {
            return Ok(());
        }
        #[cfg(target_os = "android")]
        {
            match crate::platform::android::gnss_raw_start()? {
                None => {
                    self.running = true;
                    Ok(())
                }
                Some(why) => Err(why),
            }
        }
        #[cfg(not(target_os = "android"))]
        Err("raw GNSS needs a phone".into())
    }

    /// Every measurement since the last call.
    pub fn drain(&self) -> Result<Vec<RawMeasurement>, String> {
        if !self.running {
            return Ok(Vec::new());
        }
        #[cfg(target_os = "android")]
        {
            Ok(RawMeasurement::from_flat(
                &crate::platform::android::gnss_raw_drain()?,
            ))
        }
        #[cfg(not(target_os = "android"))]
        Ok(Vec::new())
    }

    /// The listener's status, as `GnssMeasurementsEvent.Callback` reports it.
    #[must_use]
    pub fn status(&self) -> i32 {
        #[cfg(target_os = "android")]
        if self.running {
            return crate::platform::android::gnss_raw_status();
        }
        -1
    }

    /// The GNSS chip, by the platform's name for it.
    #[must_use]
    pub fn hardware() -> Option<String> {
        #[cfg(target_os = "android")]
        {
            crate::platform::android::gnss_hardware()
        }
        #[cfg(not(target_os = "android"))]
        None
    }
}

impl Default for RawGnss {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for RawGnss {
    fn drop(&mut self) {
        #[cfg(target_os = "android")]
        if self.running {
            crate::platform::android::gnss_raw_stop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(svid: i64, hz: f64, adr_state: i64, time_nanos: i64) -> [i64; STRIDE] {
        let mut r = [0i64; STRIDE];
        r[0] = 1_000_000_000;
        r[1] = time_nanos;
        r[2] = -1_300_000_000_000_000_000;
        r[3] = 0.25f64.to_bits() as i64;
        r[5] = 1;
        r[6] = svid;
        r[7] = hz.to_bits() as i64;
        r[8] = 30.0f64.to_bits() as i64;
        r[9] = 1 | (1 << 3);
        r[15] = adr_state;
        r[16] = 123.456f64.to_bits() as i64;
        r[19] = f64::NAN.to_bits() as i64;
        r[20] = f64::NAN.to_bits() as i64;
        r
    }

    #[test]
    fn a_row_reads_back_what_java_packed() {
        let m = RawMeasurement::from_row(&row(11, 1_176.45e6, adr::VALID, 7));
        assert_eq!(m.svid, 11);
        assert_eq!(m.band(), Band::L5);
        assert_eq!(m.full_bias_nanos, -1_300_000_000_000_000_000);
        assert_eq!(m.bias_nanos, 0.25);
        assert_eq!(m.adr_m, 123.456);
        assert!(m.agc_db.is_nan());
        assert!(m.time_known() && m.phase_ok());
        assert_eq!(m.system_letter(), 'G');
    }

    #[test]
    fn a_partial_trailing_row_is_dropped() {
        let mut flat: Vec<i64> = row(1, 1_575.42e6, 0, 0).to_vec();
        flat.extend_from_slice(&[1, 2, 3]);
        assert_eq!(RawMeasurement::from_flat(&flat).len(), 1);
    }

    #[test]
    fn a_slipped_phase_is_valid_but_not_ok() {
        let m = RawMeasurement::from_row(&row(1, 1_575.42e6, adr::VALID | adr::CYCLE_SLIP, 0));
        assert!(!m.phase_ok());
    }

    #[test]
    fn bands_split_at_the_right_frequencies() {
        assert_eq!(Band::of(1_575.42e6), Band::L1);
        assert_eq!(Band::of(1_602.0e6), Band::L1);
        assert_eq!(Band::of(1_561.098e6), Band::L1);
        assert_eq!(Band::of(1_176.45e6), Band::L5);
        assert_eq!(Band::of(1_207.14e6), Band::L5);
        assert_eq!(Band::of(0.0), Band::Unknown);
    }

    #[test]
    fn the_stats_line_counts_phase_per_band() {
        let batch = RawMeasurement::from_flat(
            &[
                row(1, 1_575.42e6, adr::VALID | adr::HALF_CYCLE_RESOLVED, 1),
                row(2, 1_575.42e6, adr::VALID | adr::CYCLE_SLIP, 1),
                row(1, 1_176.45e6, 0, 1),
                row(1, 1_575.42e6, adr::VALID, 2),
            ]
            .concat(),
        );
        let mut stats = RawStats::default();
        assert!(stats.line(5.0).is_none());
        stats.add(&batch, 1_100_000_000);
        let line = stats.line(1.0).unwrap();
        assert!(line.contains("2.0 epochs/s"), "{line}");
        assert!(line.contains("sats G2"), "{line}");
        assert!(
            line.contains("L1 2 sats cn0 30 time 100% phase valid 100% ok 67% slip 1 reset 0 half-cycle resolved 33%"),
            "{line}"
        );
        assert!(
            line.contains("L5 1 sats cn0 30 time 100% phase valid 0% ok 0%"),
            "{line}"
        );
        assert!(line.contains("age 100 ms"), "{line}");
    }
}

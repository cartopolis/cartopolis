//! How much of each traffic pool is out at an hour of the day.
//!
//! The pools (`systems::traffic`) are a fixed size, built at startup and
//! never grown: the busiest a street gets. This says what share of that is
//! out at the scene's hour — the morning and evening rush, the quiet of the
//! small hours, a Saturday's shopping crowd — and `drive_traffic` lets the
//! rest go as they run out of road, and puts them back as the hour fills.
//!
//! **The curves are a shape, not a measurement.** They follow how Dutch city
//! traffic is generally known to run through a day (the two commuter peaks
//! for cars and bikes, a midday and afternoon peak on foot, a later evening
//! for the fatbikes' riders, a later and slower weekend morning), drawn by
//! hand. No count at any place was fitted: the traffic is invented, and so
//! is its timetable (`docs/notes/ambient-traffic.md`).

use crate::components::AgentKind;

/// The share of a pool out at each whole hour, local time, Monday to Friday.
/// Read between the hours by straight lines, round midnight too.
const WEEKDAY: [[f32; 24]; 4] = [
    // Cars: a sharp peak at eight and a broad one at five.
    [
        0.15, 0.10, 0.08, 0.08, 0.10, 0.20, 0.45, 0.85, 1.00, 0.80, 0.65, 0.65, 0.70, 0.65, 0.65,
        0.75, 0.95, 1.00, 0.80, 0.60, 0.50, 0.42, 0.32, 0.22,
    ],
    // Walkers: building through the morning to a long midday and afternoon.
    [
        0.10, 0.06, 0.04, 0.03, 0.03, 0.05, 0.12, 0.30, 0.50, 0.50, 0.60, 0.75, 0.90, 0.90, 0.85,
        0.85, 0.85, 0.80, 0.65, 0.55, 0.45, 0.35, 0.25, 0.15,
    ],
    // Cyclists: the two school-and-work peaks, sharper than the cars'.
    [
        0.08, 0.04, 0.02, 0.02, 0.03, 0.08, 0.25, 0.70, 1.00, 0.65, 0.45, 0.45, 0.55, 0.55, 0.50,
        0.60, 0.80, 1.00, 0.70, 0.45, 0.35, 0.30, 0.22, 0.14,
    ],
    // Fatbikes: after school and into the evening.
    [
        0.10, 0.04, 0.02, 0.01, 0.01, 0.02, 0.05, 0.20, 0.35, 0.20, 0.20, 0.30, 0.45, 0.45, 0.50,
        0.80, 1.00, 0.90, 0.80, 0.75, 0.70, 0.60, 0.40, 0.20,
    ],
];

/// The same for Saturday and Sunday: a late start, no rush hour, the town
/// full on foot from late morning, and more about in the small hours.
const WEEKEND: [[f32; 24]; 4] = [
    [
        0.25, 0.18, 0.12, 0.08, 0.06, 0.08, 0.12, 0.20, 0.35, 0.50, 0.65, 0.75, 0.80, 0.80, 0.80,
        0.75, 0.70, 0.65, 0.60, 0.55, 0.50, 0.45, 0.40, 0.32,
    ],
    [
        0.25, 0.18, 0.10, 0.05, 0.03, 0.03, 0.05, 0.10, 0.20, 0.40, 0.65, 0.85, 1.00, 1.00, 1.00,
        0.95, 0.85, 0.70, 0.55, 0.50, 0.50, 0.45, 0.40, 0.32,
    ],
    [
        0.20, 0.15, 0.08, 0.04, 0.02, 0.03, 0.05, 0.10, 0.20, 0.35, 0.50, 0.60, 0.70, 0.70, 0.70,
        0.65, 0.60, 0.55, 0.45, 0.40, 0.38, 0.35, 0.30, 0.25,
    ],
    [
        0.25, 0.15, 0.06, 0.02, 0.01, 0.01, 0.02, 0.05, 0.10, 0.20, 0.35, 0.50, 0.60, 0.70, 0.75,
        0.80, 0.85, 0.85, 0.80, 0.75, 0.70, 0.60, 0.50, 0.35,
    ],
];

/// The share of `kind`'s pool out at `hour` (0–24, local) on `weekday`
/// (0 Monday … 6 Sunday): at least one agent's worth of every pool, so a
/// street at four in the morning is quiet rather than dead.
#[must_use]
pub fn share(kind: AgentKind, hour: f32, weekday: u8) -> f32 {
    let row = match kind {
        AgentKind::Car => 0,
        AgentKind::Walker => 1,
        AgentKind::Cyclist => 2,
        AgentKind::Fatbike => 3,
    };
    let table = if weekday >= 5 { &WEEKEND } else { &WEEKDAY };
    let h = hour.rem_euclid(24.0);
    let i = h.floor() as usize % 24;
    let t = h - h.floor();
    let (a, b) = (table[row][i], table[row][(i + 1) % 24]);
    a + (b - a) * t
}

/// How many of a pool of `size` are out: [`share`] of it, and never none.
#[must_use]
pub fn out_of(size: usize, share: f32) -> usize {
    ((size as f32 * share.clamp(0.0, 1.0)).ceil() as usize).clamp(1, size.max(1))
}

/// The day of the week, 0 Monday … 6 Sunday, at `unix` seconds with the
/// clock `offset_secs` ahead of UTC. The epoch was a Thursday.
#[must_use]
pub fn weekday(unix: f64, offset_secs: i32) -> u8 {
    let days = ((unix as i64 + i64::from(offset_secs)).div_euclid(86_400)) + 3;
    days.rem_euclid(7) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Rush hour is busier than midday, the small hours are quiet, and the
    /// weekend has no commuter peak.
    #[test]
    fn rush_hour_is_busy_and_the_night_quiet() {
        for kind in [AgentKind::Car, AgentKind::Cyclist] {
            assert!(share(kind, 8.0, 1) > share(kind, 11.0, 1));
            assert!(share(kind, 17.0, 1) > share(kind, 14.0, 1));
            assert!(share(kind, 3.5, 1) < 0.15);
            assert!(share(kind, 8.0, 6) < share(kind, 8.0, 1), "no Sunday rush");
        }
        assert!(share(AgentKind::Walker, 13.0, 5) > share(AgentKind::Walker, 13.0, 2) * 0.99);
        assert!(share(AgentKind::Fatbike, 16.0, 2) > share(AgentKind::Fatbike, 8.0, 2));
        // Between the hours, a straight line; round midnight too.
        let half = share(AgentKind::Car, 7.5, 1);
        assert!((half - 0.5 * (0.85 + 1.0)).abs() < 1e-5);
        let late = share(AgentKind::Car, 23.5, 1);
        assert!((late - 0.5 * (0.22 + 0.15)).abs() < 1e-5);
        assert_eq!(
            share(AgentKind::Car, 24.0, 1),
            share(AgentKind::Car, 0.0, 1)
        );
    }

    #[test]
    fn a_pool_is_never_empty() {
        assert_eq!(out_of(180, 0.0), 1);
        assert_eq!(out_of(180, 1.0), 180);
        assert_eq!(out_of(180, 0.5), 90);
        assert_eq!(out_of(12, 0.01), 1);
    }

    #[test]
    fn the_days_of_the_week() {
        // 1970-01-01 was a Thursday; 2026-11-17 is a Tuesday.
        assert_eq!(weekday(0.0, 0), 3);
        assert_eq!(weekday(1_794_873_600.0, 0), 1);
        // Just before midnight UTC on a Friday is already Saturday at +2 h.
        assert_eq!(weekday(1_795_219_199.0, 0), 4);
        assert_eq!(weekday(1_795_219_199.0, 7_200), 5);
    }
}

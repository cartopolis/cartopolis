//! A scenario's time: seconds from its clock's start.
//!
//! A scenario file writes times the way a person reads them — `"22:30"` on a
//! night that runs 22:00 to 06:00, or `"+0:52"` after the start — and all of
//! them become a [`Time`] relative to the clock's start, so that a night
//! crossing midnight needs no dates and a study's "T+0:52" needs no clock.

use serde::{Deserialize, Deserializer};
use std::fmt;

/// Seconds from the scenario clock's start; negative before it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Time(pub i64);

impl Time {
    pub const ZERO: Time = Time(0);

    #[must_use]
    pub fn from_minutes(m: f64) -> Self {
        Time((m * 60.0).round() as i64)
    }

    #[must_use]
    pub fn hours(self) -> f64 {
        self.0 as f64 / 3600.0
    }

    #[must_use]
    pub fn minutes(self) -> f64 {
        self.0 as f64 / 60.0
    }

    #[must_use]
    pub fn plus(self, d: Time) -> Time {
        Time(self.0 + d.0)
    }

    #[must_use]
    pub fn minus(self, d: Time) -> Time {
        Time(self.0 - d.0)
    }
}

/// A time as the file writes it, before the clock is known.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stamp {
    /// A wall-clock time of day, `"22:30"`, in minutes past midnight: the first
    /// such moment at or after the clock's start.
    Wall(u32),
    /// An offset from the clock's start, `"+0:52"` or `"T+0:52"`, in seconds.
    After(i64),
    /// A year, `"2024"`: figures, forecasts, decades.
    Year(i32),
}

impl Stamp {
    /// Parse `"HH:MM"`, `"+H:MM"`, `"T+H:MM"` or a year `"2024"`; a span (`"1:00"`, a horizon)
    /// is read with [`parse_span`].
    pub fn parse(s: &str) -> Result<Stamp, String> {
        let t = s.trim();
        if let Some(rest) = t.strip_prefix("T+").or_else(|| t.strip_prefix('+')) {
            return parse_span(rest).map(|d| Stamp::After(d.0));
        }
        if t.len() == 4 && t.bytes().all(|b| b.is_ascii_digit()) {
            return t
                .parse()
                .map(Stamp::Year)
                .map_err(|_| format!("{s:?}: bad year"));
        }
        let (h, m) = hh_mm(t)?;
        if h >= 24 {
            return Err(format!("{s:?}: a time of day runs to 23:59"));
        }
        Ok(Stamp::Wall(h * 60 + m))
    }
}

impl<'de> Deserialize<'de> for Stamp {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Stamp::parse(&s).map_err(serde::de::Error::custom)
    }
}

/// A length of time, `"H:MM"` (`"3:00"`, `"0:45"`).
pub fn parse_span(s: &str) -> Result<Time, String> {
    let (h, m) = hh_mm(s.trim())?;
    Ok(Time(i64::from(h) * 3600 + i64::from(m) * 60))
}

fn hh_mm(s: &str) -> Result<(u32, u32), String> {
    let (h, m) = s
        .split_once(':')
        .ok_or_else(|| format!("{s:?}: expected H:MM"))?;
    let h: u32 = h.parse().map_err(|_| format!("{s:?}: bad hours"))?;
    let m: u32 = m.parse().map_err(|_| format!("{s:?}: bad minutes"))?;
    if m >= 60 {
        return Err(format!("{s:?}: minutes run to 59"));
    }
    Ok((h, m))
}

/// A span as the file writes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span(pub Time);

impl<'de> Deserialize<'de> for Span {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        parse_span(&s).map(Span).map_err(serde::de::Error::custom)
    }
}

/// A year on a clock of years: 365.25 days.
pub const YEAR: Time = Time(31_557_600);

/// The clock: where a scenario's time starts on the wall, and where it ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Clock {
    /// The start's time of day, minutes past midnight, when the file gave one;
    /// `None` for a clock of offsets only ("T+…").
    pub start_wall: Option<u32>,
    /// The start's year, for a clock of years (figures and forecasts).
    pub start_year: Option<i32>,
    pub end: Time,
}

impl Clock {
    /// A stamp on this clock.
    #[must_use]
    pub fn resolve(&self, s: Stamp) -> Time {
        match (s, self.start_wall) {
            (Stamp::After(d), _) => Time(d),
            (Stamp::Year(y), _) => Time(i64::from(y - self.start_year.unwrap_or(y)) * YEAR.0),
            (Stamp::Wall(m), Some(start)) => {
                let d = (i64::from(m) - i64::from(start)).rem_euclid(24 * 60);
                Time(d * 60)
            }
            // A wall time on a clock with none: read it as an offset.
            (Stamp::Wall(m), None) => Time(i64::from(m) * 60),
        }
    }

    /// The year `t` falls in, on a clock of years.
    #[must_use]
    pub fn year_at(&self, t: Time) -> Option<i32> {
        self.start_year
            .map(|y| (i64::from(y) + t.0.div_euclid(YEAR.0)) as i32)
    }

    /// How a time reads on this clock: `"02:10"` on a wall clock, `"T+0:52"`
    /// otherwise, and the year on a clock of years.
    #[must_use]
    pub fn label(&self, t: Time) -> String {
        if let Some(y) = self.year_at(t) {
            return y.to_string();
        }
        match self.start_wall {
            Some(start) => {
                let m = (i64::from(start) + t.0.div_euclid(60)).rem_euclid(24 * 60);
                format!("{:02}:{:02}", m / 60, m % 60)
            }
            None => {
                let sign = if t.0 < 0 { "T−" } else { "T+" };
                let m = t.0.abs() / 60;
                format!("{sign}{}:{:02}", m / 60, m % 60)
            }
        }
    }
}

impl fmt::Display for Time {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let m = self.0.abs() / 60;
        let sign = if self.0 < 0 { "-" } else { "" };
        write!(f, "{sign}{}:{:02}", m / 60, m % 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_night_crosses_midnight_without_dates() {
        let clock = Clock {
            start_wall: Some(22 * 60),
            start_year: None,
            end: Time(8 * 3600),
        };
        assert_eq!(clock.resolve(Stamp::parse("22:30").unwrap()), Time(1800));
        assert_eq!(
            clock.resolve(Stamp::parse("02:10").unwrap()),
            Time(250 * 60)
        );
        assert_eq!(clock.label(Time(250 * 60)), "02:10");
        assert_eq!(clock.label(Time(0)), "22:00");
    }

    #[test]
    fn offsets_read_as_a_study_writes_them() {
        let clock = Clock {
            start_wall: None,
            start_year: None,
            end: Time(48 * 3600),
        };
        assert_eq!(
            clock.resolve(Stamp::parse("T+0:52").unwrap()),
            Time(52 * 60)
        );
        assert_eq!(clock.label(Time(52 * 60)), "T+0:52");
    }

    #[test]
    fn a_clock_of_years_reads_in_years() {
        let clock = Clock {
            start_wall: None,
            start_year: Some(2024),
            end: Time(26 * YEAR.0),
        };
        let t = clock.resolve(Stamp::parse("2050").unwrap());
        assert_eq!(t, clock.end);
        assert_eq!(clock.label(t), "2050");
        assert_eq!(clock.label(Time::ZERO), "2024");
    }

    #[test]
    fn nonsense_is_refused() {
        assert!(Stamp::parse("25:00").is_err());
        assert!(Stamp::parse("2:75").is_err());
        assert!(Stamp::parse("soon").is_err());
    }
}

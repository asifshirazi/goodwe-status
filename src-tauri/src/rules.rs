//! The app's rules, knowing nothing about SEMS: grid state across readings, stale,
//! time left, chart gaps, and the text they turn into.

use crate::sems::GridSignal;
use serde::Serialize;
use std::time::Duration;

/// Poll every 2 minutes; never faster, it is an unofficial API.
pub const POLL_EVERY: Duration = Duration::from_secs(120);
/// Stale when `info.time` hasn't changed for this long by the app's clock...
pub const STALE_AFTER: Duration = Duration::from_secs(15 * 60);
/// ...or after this many failed polls in a row.
pub const STALE_FAILURES: u32 = 3;
/// Below this the battery counts as idle. shortcut: a fixed 50 W dead band (the real on-grid
/// idle is about +30 W, T-002 research); tune it if other inverters idle higher. ui/panel.js
/// `IDLE_KW` must stay equal to it.
pub const IDLE_WATTS: f64 = 50.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum GridState {
    /// Before any reading agreed with itself.
    Unknown,
    On,
    Off,
}

/// A disagreement keeps the previous state.
pub fn next_grid(prev: GridState, signal: GridSignal) -> GridState {
    match signal {
        GridSignal::On => GridState::On,
        GridSignal::Off => GridState::Off,
        GridSignal::Disagree => prev,
    }
}

/// Notify only on a change between two known states. The first reading after launch starts
/// from Unknown, so it never notifies.
pub fn grid_changed(prev: GridState, next: GridState) -> bool {
    prev != GridState::Unknown && next != GridState::Unknown && prev != next
}

/// `since_change`: app-clock time since `info.time` last changed (None before any reading).
pub fn is_stale(since_change: Option<Duration>, failures: u32) -> bool {
    failures >= STALE_FAILURES || since_change.is_some_and(|d| d >= STALE_AFTER)
}

/// Minutes of battery left while discharging: capacity × soc / 100 ÷ kW, rounded to 10 min.
/// None when not discharging or the capacity is unknown. No minimum-SOC reserve is taken
/// off (unknown per inverter), so the text keeps its "~".
pub fn time_left_min(capacity_kwh: Option<f64>, soc: f64, watts: f64) -> Option<u32> {
    let cap = capacity_kwh?;
    if watts < IDLE_WATTS || soc <= 0.0 {
        return None;
    }
    let min = cap * soc / 100.0 / (watts / 1000.0) * 60.0;
    // Never claim "~0m" while it still runs.
    Some((((min / 10.0).round() as u32) * 10).max(10))
}

/// "~4h 20m" / "~40m".
pub fn fmt_left(min: u32) -> String {
    match (min / 60, min % 60) {
        (0, m) => format!("~{m}m"),
        (h, m) => format!("~{h}h {m}m"),
    }
}

/// "1.0" (kW, one decimal), for "left at 1.0 kW".
pub fn fmt_kw(watts: f64) -> String {
    format!("{:.1}", watts.abs() / 1000.0)
}

/// "Discharging 998 W" / "Charging 1.2 kW" / "Battery idle". Whole watts below 1000 W.
pub fn fmt_power(watts: f64) -> String {
    if watts.abs() < IDLE_WATTS {
        return "Battery idle".into();
    }
    let verb = if watts > 0.0 { "Discharging" } else { "Charging" };
    let w = watts.abs().round();
    if w < 1000.0 {
        format!("{verb} {w} W")
    } else {
        format!("{verb} {} kW", fmt_kw(watts))
    }
}

/// "(38 min)" under an hour, "(2h 15m)" from an hour.
pub fn fmt_since_duration(min: i64) -> String {
    let min = min.max(0);
    if min < 60 {
        format!("({min} min)")
    } else {
        format!("({}h {}m)", min / 60, min % 60)
    }
}

/// Where the chart's points come from; the line breaks at gaps longer than `gap()` minutes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum ChartSource {
    /// SEMS+ series, a point every 1–2 minutes.
    Plus,
    /// The app's own readings, one per new `info.time` (about every 5 minutes).
    Own,
}

impl ChartSource {
    /// D-Y16: the gap threshold follows the source.
    pub fn gap(self) -> u32 {
        match self {
            ChartSource::Plus => 5,
            ChartSource::Own => 12,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use GridSignal as S;
    use GridState::*;

    #[test]
    fn grid_state_and_notifications() {
        // first reading sets the state, silently
        assert_eq!(next_grid(Unknown, S::Off), Off);
        assert!(!grid_changed(Unknown, Off));
        // first reading disagrees: stays unknown
        assert_eq!(next_grid(Unknown, S::Disagree), Unknown);
        // a disagreement keeps the previous state and never notifies
        assert_eq!(next_grid(On, S::Disagree), On);
        assert_eq!(next_grid(Off, S::Disagree), Off);
        assert!(!grid_changed(On, next_grid(On, S::Disagree)));
        // confirmed changes notify
        assert!(grid_changed(On, next_grid(On, S::Off)));
        assert!(grid_changed(Off, next_grid(Off, S::On)));
        assert!(!grid_changed(Off, next_grid(Off, S::Off)));
    }

    #[test]
    fn stale_rule() {
        let m = |n: u64| Some(Duration::from_secs(n * 60));
        assert!(!is_stale(None, 0));
        assert!(!is_stale(m(14), 0));
        assert!(is_stale(m(15), 0));
        assert!(!is_stale(m(1), 2));
        assert!(is_stale(m(1), 3));
        assert!(is_stale(None, 3));
    }

    #[test]
    fn time_left() {
        // the grid-off sample: 5.1 kWh × 86 % ÷ 0.998 kW = 4.39 h → ~4h 20m
        let t = time_left_min(Some(5.1), 86.0, 998.28).unwrap();
        assert_eq!((t, fmt_left(t), fmt_kw(998.28)), (260, "~4h 20m".into(), "1.0".into()));
        // under an hour
        assert_eq!(time_left_min(Some(5.1), 10.0, 800.0).map(fmt_left), Some("~40m".into()));
        // never ~0m
        assert_eq!(time_left_min(Some(5.1), 1.0, 5000.0), Some(10));
        // only while discharging, and only with a capacity
        assert_eq!(time_left_min(Some(5.1), 86.0, -1200.0), None);
        assert_eq!(time_left_min(Some(5.1), 86.0, 5.0), None);
        assert_eq!(time_left_min(None, 86.0, 998.0), None);
    }

    #[test]
    fn power_and_since_text() {
        assert_eq!(fmt_power(998.28), "Discharging 998 W");
        assert_eq!(fmt_power(-1200.0), "Charging 1.2 kW");
        assert_eq!(fmt_power(999.6), "Discharging 1.0 kW");
        assert_eq!(fmt_power(-3.0), "Battery idle");
        assert_eq!(fmt_since_duration(38), "(38 min)");
        assert_eq!(fmt_since_duration(135), "(2h 15m)");
        assert_eq!(fmt_since_duration(60), "(1h 0m)");
    }

    #[test]
    fn idle_watts_boundary() {
        assert_eq!(fmt_power(49.0), "Battery idle");
        assert_eq!(fmt_power(50.0), "Discharging 50 W");
        assert_eq!(fmt_power(-49.9), "Battery idle");
        assert_eq!(fmt_power(-50.0), "Charging 50 W");
        assert_eq!(time_left_min(Some(5.1), 86.0, 49.0), None);
        assert!(time_left_min(Some(5.1), 86.0, 50.0).is_some());
        // readout_idle_matches_battery_row (Rust half): the real on-grid idle, +30 W
        assert_eq!(fmt_power(30.0), "Battery idle");
    }

    #[test]
    fn gap_follows_the_source() {
        assert_eq!(ChartSource::Plus.gap(), 5);
        assert_eq!(ChartSource::Own.gap(), 12);
        // own readings arrive about every 5 min (5.2 here): no break with the own threshold
        assert!(5.2 < ChartSource::Own.gap() as f64 && 5.2 > ChartSource::Plus.gap() as f64);
    }
}

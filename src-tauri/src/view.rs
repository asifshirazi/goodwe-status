//! What the tray item and the panel show, built from the `Monitor`. Pure: the shell only
//! applies it. Every string is formatted here; panel.js only lays them out (hand-off § 1, § 2).

use crate::monitor::{ChartView, Monitor};
use crate::rules::{self, GridState};
use serde::Serialize;
use std::time::Instant;

/// `state` for `GW.renderPanel` (shape documented at the top of ui/panel.js).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PanelState {
    pub status: Status,
    pub battery: Option<BatteryView>,
    pub updated: Option<Updated>,
    pub notifications_off: bool,
    pub chart: Option<ChartView>,
    pub signed_in: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Status {
    pub kind: &'static str,
    pub title: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct BatteryView {
    pub soc: String,
    pub power: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub left: Option<Left>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Left {
    pub time: String,
    pub rest: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Updated {
    pub stale: bool,
    pub text: String,
}

/// The four tray images in `icons/`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayIcon {
    Plug,
    OffRed,
    Dim,
    OffDim,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TrayLook {
    pub icon: TrayIcon,
    pub title: Option<String>,
    pub tooltip: String,
}

const SIGNED_OUT_DETAIL: &str = "Sign in with your SEMS Portal account to see your battery and grid.";

/// Stale only means something once there is a reading to call old.
fn stale(m: &Monitor, now: Instant) -> bool {
    m.last.is_some() && m.stale(now)
}

fn status(m: &Monitor, now: Instant) -> Status {
    let s = |kind, title| Status { kind, title, detail: None };
    if !m.signed_in {
        return Status { detail: Some(SIGNED_OUT_DETAIL.into()), ..s("signedout", "Not signed in") };
    }
    if m.last.is_none() {
        return s("first", "Getting the first reading…");
    }
    match m.grid {
        GridState::Unknown => s("unknown", "Grid status not clear yet"),
        GridState::On => s("on", "Grid on"),
        GridState::Off => {
            // While stale, "Since …" keeps counting on the station clock.
            let detail = m.off_since.map(|o| match (o.exact, m.station_now(now)) {
                (true, Some(t)) => format!("Since {} {}", o.at.hhmm(), rules::fmt_since_duration((t.0 - o.at.0) / 60)),
                _ => format!("Since at least {}", o.at.hhmm()),
            });
            Status { detail, ..s("off", "Grid off") }
        }
    }
}

fn soc_text(soc: f64) -> String {
    format!("{}%", soc.round())
}

pub fn panel(m: &Monitor, now: Instant, notifications_off: bool) -> PanelState {
    let stale = stale(m, now);
    let shown = m.last.as_ref().filter(|_| m.signed_in);
    let battery = shown.and_then(|r| r.battery.as_ref()).map(|b| BatteryView {
        soc: soc_text(b.soc),
        power: rules::fmt_power(b.watts),
        left: rules::time_left_min(b.capacity_kwh, b.soc, b.watts).map(|min| {
            let mut rest = format!("left at {} kW", rules::fmt_kw(b.watts));
            if stale {
                rest += &format!(" · as of {}", shown.unwrap().time.hhmm());
            }
            Left { time: rules::fmt_left(min), rest }
        }),
    });
    let updated = shown.map(|r| Updated {
        stale,
        text: if stale { format!("No new data since {}", r.time.hhmm()) } else { format!("Updated {}", r.time.hhmm()) },
    });
    PanelState {
        status: status(m, now),
        battery,
        updated,
        notifications_off: notifications_off && m.signed_in,
        chart: m.chart_view(now),
        signed_in: m.signed_in,
    }
}

pub fn tray(m: &Monitor, now: Instant) -> TrayLook {
    let look = |icon, title: Option<String>, tooltip: String| TrayLook { icon, title, tooltip };
    if !m.signed_in {
        return look(TrayIcon::Dim, None, "Not signed in".into());
    }
    let Some(r) = &m.last else { return look(TrayIcon::Plug, None, "Getting the first reading…".into()) };
    let soc = r.battery.as_ref().map(|b| b.soc);
    let title = soc.map(soc_text);
    if stale(m, now) {
        let icon = if m.grid == GridState::Off { TrayIcon::OffDim } else { TrayIcon::Dim };
        return look(icon, title, format!("No new data since {}", r.time.hhmm()));
    }
    let icon = if m.grid == GridState::Off { TrayIcon::OffRed } else { TrayIcon::Plug };
    let grid = status(m, now).title;
    let tooltip = match &title {
        Some(t) => format!("{grid} · battery {t}"),
        None => grid.into(),
    };
    look(icon, title, tooltip)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::LocalTs;
    use crate::sems::{Battery, GridSignal, Reading};
    use std::time::Duration;

    fn reading(time: &str, grid: GridSignal, battery: Option<(f64, f64)>) -> Reading {
        Reading {
            time: LocalTs::parse(time).unwrap(),
            grid,
            vac1: 0.0,
            grid_status: None,
            battery: battery.map(|(soc, watts)| Battery { soc, watts, capacity_kwh: Some(5.1) }),
            utc_offset_h: Some(5.0),
        }
    }
    const MIN: Duration = Duration::from_secs(60);

    #[test]
    fn signed_out_and_first_reading() {
        let t0 = Instant::now();
        let m = Monitor::new(false);
        let p = panel(&m, t0, true);
        assert_eq!((p.status.kind, p.status.title, p.status.detail.as_deref()), ("signedout", "Not signed in", Some(SIGNED_OUT_DETAIL)));
        assert!(p.battery.is_none() && p.updated.is_none() && p.chart.is_none() && !p.signed_in);
        assert!(!p.notifications_off, "the notifications section waits for sign-in");
        assert_eq!(tray(&m, t0), TrayLook { icon: TrayIcon::Dim, title: None, tooltip: "Not signed in".into() });

        let m = Monitor::new(true);
        let p = panel(&m, t0, true);
        assert_eq!((p.status.kind, p.updated, p.notifications_off), ("first", None, true));
        assert_eq!(p.chart.unwrap().status, "loading");
        assert_eq!(tray(&m, t0), TrayLook { icon: TrayIcon::Plug, title: None, tooltip: "Getting the first reading…".into() });
    }

    #[test]
    fn grid_off_live_then_stale() {
        let t0 = Instant::now();
        let mut m = Monitor::new(true);
        m.on_reading(reading("10/18/2026 07:30:00", GridSignal::On, Some((90.0, -500.0))), t0);
        m.on_reading(reading("10/18/2026 07:42:00", GridSignal::Off, Some((88.0, 998.28))), t0);
        let t = t0 + 38 * MIN;
        m.on_reading(reading("10/18/2026 08:20:00", GridSignal::Off, Some((86.0, 998.28))), t);
        let p = panel(&m, t, false);
        assert_eq!((p.status.kind, p.status.title), ("off", "Grid off"));
        assert_eq!(p.status.detail.as_deref(), Some("Since 07:42 (38 min)"));
        let b = p.battery.unwrap();
        assert_eq!((b.soc.as_str(), b.power.as_str()), ("86%", "Discharging 998 W"));
        assert_eq!(b.left, Some(Left { time: "~4h 20m".into(), rest: "left at 1.0 kW".into() }));
        assert_eq!(p.updated, Some(Updated { stale: false, text: "Updated 08:20".into() }));
        assert_eq!(tray(&m, t), TrayLook { icon: TrayIcon::OffRed, title: Some("86%".into()), tooltip: "Grid off · battery 86%".into() });

        // no new info.time for 15 min: grey, values kept, "as of"
        let t = t0 + 135 * MIN;
        let p = panel(&m, t, false);
        assert_eq!(p.status.detail.as_deref(), Some("Since 07:42 (2h 15m)"));
        assert_eq!(p.battery.unwrap().left.unwrap().rest, "left at 1.0 kW · as of 08:20");
        assert_eq!(p.updated, Some(Updated { stale: true, text: "No new data since 08:20".into() }));
        assert_eq!(tray(&m, t), TrayLook { icon: TrayIcon::OffDim, title: Some("86%".into()), tooltip: "No new data since 08:20".into() });
    }

    #[test]
    fn grid_on_unknown_after_restart_and_no_battery() {
        let t0 = Instant::now();
        // first reading during an outage: "Since at least"
        let mut m = Monitor::new(true);
        m.on_reading(reading("10/18/2026 08:20:00", GridSignal::Off, Some((86.0, 0.0))), t0);
        let p = panel(&m, t0, false);
        assert_eq!(p.status.detail.as_deref(), Some("Since at least 08:20"));
        assert_eq!(p.battery.unwrap(), BatteryView { soc: "86%".into(), power: "Battery idle".into(), left: None });

        // first reading disagrees: not clear yet, plain plug
        let mut m = Monitor::new(true);
        m.on_reading(reading("10/18/2026 08:20:00", GridSignal::Disagree, Some((90.6, -1200.0))), t0);
        assert_eq!(panel(&m, t0, false).status.title, "Grid status not clear yet");
        assert_eq!(tray(&m, t0).tooltip, "Grid status not clear yet · battery 91%");
        assert_eq!(tray(&m, t0).icon, TrayIcon::Plug);

        // grid on, stale: dim plug
        m.on_reading(reading("10/18/2026 08:25:00", GridSignal::On, Some((91.0, -1200.0))), t0);
        assert_eq!(tray(&m, t0).tooltip, "Grid on · battery 91%");
        assert_eq!(tray(&m, t0 + 15 * MIN).icon, TrayIcon::Dim);

        // no battery: no title, grid title alone, no battery section or chart
        let mut m = Monitor::new(true);
        m.on_reading(reading("10/18/2026 08:20:00", GridSignal::Off, None), t0);
        assert_eq!(tray(&m, t0), TrayLook { icon: TrayIcon::OffRed, title: None, tooltip: "Grid off".into() });
        let p = panel(&m, t0, false);
        assert!(p.battery.is_none() && p.chart.is_none() && p.updated.is_some());
    }

    #[test]
    fn json_shape_matches_panel_js() {
        let mut m = Monitor::new(true);
        m.on_reading(reading("10/18/2026 08:20:00", GridSignal::On, Some((91.0, 0.0))), Instant::now());
        let v = serde_json::to_value(panel(&m, Instant::now(), true)).unwrap();
        for k in ["status", "battery", "updated", "notificationsOff", "chart", "signedIn"] {
            assert!(v.get(k).is_some(), "missing {k}");
        }
        assert_eq!(v["status"], serde_json::json!({ "kind": "on", "title": "Grid on" }));
        assert!(v["battery"].get("left").is_none());
    }

    /// The designer's demo day (mockups/panel/demo-data.js `day()`), as SEMS+ chart input.
    fn demo_day(end: u32, soc: f64, kw: f64, from: u32) -> Vec<(u32, f64, f64)> {
        let s0 = (soc + kw * ((end - from) as f64 / 60.0) / 5.1 * 100.0).clamp(0.0, 100.0);
        let c_rate = (100.0 - s0) / 100.0 * 5.1 / ((from - 166) as f64 / 60.0);
        (0..=end).step_by(2).map(|m| {
            let mf = m as f64;
            let (s, k) = if m < 120 { (93.0 - 30.0 * mf / 120.0, 0.76 + 0.14 * (mf / 7.0).sin()) }
                else if m < 166 { (63.0 + 37.0 * (mf - 120.0) / 46.0, if m < 124 { -1.9 } else { -3.8 }) }
                else if m < from { (100.0 - (100.0 - s0) * (mf - 166.0) / (from - 166) as f64, c_rate + 0.03 * (mf / 5.0).sin()) }
                else { (s0 + (soc - s0) * (mf - from as f64) / (end - from).max(1) as f64, kw + 0.05 * (mf / 3.0).sin()) };
            (m, s.round(), (k * 100.0).round() / 100.0)
        }).collect()
    }

    /// B-R4: every hand-off panel and menu bar state, built through Monitor + view, written as
    /// JSON for the shot renderer. Run: GW_FIXTURES=<file> cargo test … write_state_fixtures -- --ignored
    #[test]
    #[ignore]
    fn write_state_fixtures() {
        use crate::sems::ChartSeries;
        let t0 = Instant::now();
        let at = |hhmm: &str| format!("10/18/2026 {hhmm}:00");
        let day = LocalTs::parse(&at("08:20")).unwrap().day();
        let plus = |pts: Vec<(u32, f64, f64)>| Some(ChartSeries { day, points: pts });
        // Grid on at 07:30, off at 07:42, then a reading at `hhmm` (minutes after 07:42 on the app clock).
        let off = |hhmm: &str, soc: f64, w: f64| {
            let mut m = Monitor::new(true);
            m.on_reading(reading(&at("07:30"), GridSignal::On, Some((95.0, 0.0))), t0);
            m.on_reading(reading(&at("07:42"), GridSignal::Off, Some((90.0, w))), t0);
            let t = LocalTs::parse(&at(hhmm)).unwrap();
            let el = t0 + Duration::from_secs((t.0 - LocalTs::parse(&at("07:42")).unwrap().0) as u64);
            m.on_reading(reading(&at(hhmm), GridSignal::Off, Some((soc, w))), el);
            (m, el)
        };
        let on = |hhmm: &str, battery: Option<(f64, f64)>, sig: GridSignal| {
            let mut m = Monitor::new(true);
            m.on_reading(reading(&at(hhmm), sig, battery), t0);
            (m, t0)
        };
        let ok = |mut x: (Monitor, Instant), pts| { x.0.on_chart(plus(pts)); x };
        let mut out = serde_json::Map::new();
        let mut put = |name: &str, (m, now): (Monitor, Instant), later: u64, notif_off: bool| {
            let now = now + Duration::from_secs(later * 60);
            let t = tray(&m, now);
            let tray = serde_json::json!({ "icon": format!("{:?}", t.icon), "title": t.title, "tooltip": t.tooltip });
            out.insert(name.into(), serde_json::json!({ "panel": panel(&m, now, notif_off), "tray": tray }));
        };
        let d = |soc, kw| demo_day(500, soc, kw, 462);
        put("grid-off-discharging-hover", ok(off("08:20", 86.0, 998.28), d(86.0, 0.998)), 0, false);
        put("grid-on-charging", ok(on("08:20", Some((91.0, -1200.0)), GridSignal::On), d(91.0, -1.2)), 0, false);
        put("grid-on-idle", ok(on("08:20", Some((100.0, 0.0)), GridSignal::On), d(100.0, 0.0)), 0, false);
        put("grid-on-discharging", ok(on("08:20", Some((91.0, 640.0)), GridSignal::On), d(91.0, 0.64)), 0, false);
        put("grid-off-after-restart", ok(on("08:20", Some((86.0, 998.28)), GridSignal::Off), d(86.0, 0.998)), 0, false);
        put("grid-off-charging", ok(off("08:20", 86.0, -1400.0), d(86.0, -1.4)), 0, false);
        put("grid-off-idle", ok(off("08:20", 100.0, 0.0), d(100.0, 0.0)), 0, false);
        put("grid-off-over-1h", ok(off("09:57", 62.0, 1100.0), demo_day(596, 62.0, 1.1, 462)), 0, false);
        put("grid-unknown", ok(on("08:20", Some((91.0, -1200.0)), GridSignal::Disagree), d(91.0, -1.2)), 0, false);
        put("stale-grid-off", ok(off("08:20", 86.0, 998.28), d(86.0, 0.998)), 130, false);
        put("stale-grid-on", ok(on("08:20", Some((91.0, -1200.0)), GridSignal::On), d(91.0, -1.2)), 130, false);
        put("notifications-off", ok(on("08:20", Some((91.0, -1200.0)), GridSignal::On), d(91.0, -1.2)), 0, true);
        put("no-battery-grid-on", on("08:20", None, GridSignal::On), 0, false);
        put("no-battery-grid-off", off_no_battery(t0, &at), 0, false);
        put("first-reading", (Monitor::new(true), t0), 0, false);
        put("signed-out", (Monitor::new(false), t0), 0, false);
        put("chart-loading", off("08:20", 86.0, 998.28), 0, false);
        put("chart-empty-after-midnight", ok(on("00:01", Some((91.0, 640.0)), GridSignal::On), vec![]), 0, false);
        let gap: Vec<_> = demo_day(560, 80.0, 1.0, 462).into_iter().filter(|p| p.0 <= 500 || p.0 >= 540).collect();
        put("chart-gap", ok(off("09:20", 80.0, 1000.0), gap), 0, false);
        // SEMS+ failed: own readings every 5 min (the inverter's pace), from yesterday 23:58 or from 07:58.
        let own = |first: &str| {
            let mut m = Monitor::new(true);
            let (start, end) = (LocalTs::parse(first).unwrap(), LocalTs::parse(&at("08:20")).unwrap());
            let demo = d(86.0, 0.998);
            let mut t = start;
            while t.0 <= end.0 {
                let p = demo[((t.minute_of_day().min(500) / 2) as usize).min(demo.len() - 1)];
                let p = if t.day() == day { p } else { demo[0] };
                let sig = if t.day() == day && t.minute_of_day() >= 462 { GridSignal::Off } else { GridSignal::On };
                m.on_reading(reading(&fmt_ts(t), sig, Some((p.1, p.2 * 1000.0))), t0 + Duration::from_secs((t.0 - start.0) as u64));
                t = t.plus_secs(300);
            }
            m.on_chart(None);
            let now = t0 + Duration::from_secs((t.0 - 300 - start.0) as u64);
            (m, now)
        };
        put("chart-fallback-since-yesterday", own("10/17/2026 23:58:00"), 0, false);
        put("chart-fallback-since-launch", own("10/18/2026 07:58:00"), 0, false);
        // Reachable "failed": the one own reading is from yesterday (23:58), now 00:08, SEMS+ failed.
        let mut m = Monitor::new(true);
        m.on_reading(reading("10/17/2026 23:58:00", GridSignal::Off, Some((86.0, 998.28))), t0);
        m.on_chart(None);
        put("chart-failed-nothing-yet", (m, t0), 10, false);
        // menu bar: the same monitors (grid on = 91 % charging, grid off = 86 % live)
        for (k, v) in [("grid-on", "grid-on-charging"), ("grid-off", "grid-off-discharging-hover"), ("stale-grid-on", "stale-grid-on"), ("stale-grid-off", "stale-grid-off"),
            ("grid-unknown", "grid-unknown"), ("no-battery-grid-on", "no-battery-grid-on"), ("no-battery-grid-off", "no-battery-grid-off"), ("signed-out", "signed-out")] {
            let tray = out[v]["tray"].clone();
            out.insert(format!("menubar-{k}"), serde_json::json!({ "tray": tray }));
        }
        let path = std::env::var("GW_FIXTURES").unwrap_or_else(|_| std::env::temp_dir().join("gw-fixtures.json").to_string_lossy().into_owned());
        std::fs::write(&path, serde_json::to_string_pretty(&out).unwrap()).unwrap();
        println!("{} states → {path}", out.len());
    }

    fn off_no_battery(t0: Instant, at: &dyn Fn(&str) -> String) -> (Monitor, Instant) {
        let mut m = Monitor::new(true);
        m.on_reading(reading(&at("07:30"), GridSignal::On, None), t0);
        m.on_reading(reading(&at("07:42"), GridSignal::Off, None), t0);
        let el = t0 + Duration::from_secs(38 * 60);
        m.on_reading(reading(&at("08:20"), GridSignal::Off, None), el);
        (m, el)
    }

    fn fmt_ts(t: LocalTs) -> String {
        let (date, hm) = (t.date(), t.hhmm()); // date = YYYY-MM-DD
        format!("{}/{}/{} {hm}:00", &date[5..7], &date[8..10], &date[0..4])
    }
}

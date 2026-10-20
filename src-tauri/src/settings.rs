//! The Settings window (label `settings` in tauri.conf.json, hidden until opened): General,
//! Automation and Stats panes behind a toolbar, under a native Overlay title bar that shows the pane's
//! name. Rust builds the whole `settings-state` and sends it to this window only, on open and on
//! every change; the page answers with `settings_action`. Changes apply at once.

use crate::automation::{Automation, View, Which};
use crate::monitor::Monitor;
use crate::tuya::Keys;
use crate::stats::{self, StatsView};
use crate::{creds, logdir, shell, signin};
use serde::Serialize;
use serde_json::Value;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, LogicalSize, Manager, WebviewWindow};
use tauri_plugin_opener::OpenerExt;

const WIN: &str = "settings";
const WIN_W: f64 = 560.0;
/// The README's Tuya section (the connect form's link).
const HOWTO: &str = "https://github.com/asifshirazi/goodwe-status#device-automation-tuya";
/// About › Open on GitHub; release notes are under `/releases/tag/v<version>`.
const REPO: &str = "https://github.com/asifshirazi/goodwe-status";

/// The login item behind Open at login. The app's is tauri-plugin-autostart; tests use a fake.
pub trait LoginItem {
    fn is_enabled(&self) -> bool;
    fn set(&self, on: bool) -> Result<(), String>;
}

struct AutoStart<'a>(&'a AppHandle);

impl LoginItem for AutoStart<'_> {
    fn is_enabled(&self) -> bool {
        use tauri_plugin_autostart::ManagerExt;
        self.0.autolaunch().is_enabled().unwrap_or(false)
    }
    fn set(&self, on: bool) -> Result<(), String> {
        use tauri_plugin_autostart::ManagerExt;
        let al = self.0.autolaunch();
        if on { al.enable() } else { al.disable() }.map_err(|e| e.to_string())
    }
}

pub fn login_item(app: &AppHandle) -> impl LoginItem + '_ {
    AutoStart(app)
}

/// Turns the login item on or off (`want`), then reads what the system says: the switch shows
/// the real state, also when the user removed the item in System Settings or the change failed.
pub fn login_state(item: &dyn LoginItem, want: Option<bool>) -> (bool, Option<String>) {
    let err = want.and_then(|on| item.set(on).err());
    (item.is_enabled(), err)
}

#[derive(Default)]
pub struct Settings {
    /// The page has loaded and listens for `settings-open`.
    loaded: AtomicBool,
    /// An open was asked for before the page loaded.
    wanted: AtomicBool,
    /// The real login-item state, read on open, when General is shown, and after a change.
    open_at_login: AtomicBool,
}

/// The pane to show: the last one shown, else General.
pub fn pane(saved: Option<&str>) -> &'static str {
    match saved {
        Some("automation") => "automation",
        Some("stats") => "stats",
        Some("about") => "about",
        _ => "general",
    }
}

/// Settings… and ⌘, (T-022): on About while a newer version waits (the gear's dot), else the last pane.
pub fn open_pane(saved: Option<&str>, update_waiting: bool) -> &'static str {
    if update_waiting { "about" } else { pane(saved) }
}

/// The window title: the visible pane's name.
pub fn title(pane: &str) -> &'static str {
    match pane {
        "automation" => "Automation",
        "stats" => "Stats",
        "about" => "About",
        _ => "General",
    }
}

#[derive(Debug, Serialize)]
pub struct State {
    pub pane: &'static str,
    pub account: Account,
    pub general: General,
    pub stats: StatsView,
    pub about: About,
    #[serde(flatten)]
    pub auto: View,
}

/// Settings › About (T-022): the running version and the update row.
#[derive(Debug, Serialize)]
pub struct About {
    pub version: String,
    pub update: crate::updater::View,
}

#[derive(Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    pub signed_in: bool,
    /// Only while signed in and known (the Keychain may not have answered yet).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct General {
    pub open_at_login: bool,
    /// Low battery alert, in % (T-008).
    pub low_battery: u8,
    /// Show temperatures (T-015 § 2).
    pub show_temps: bool,
    /// What the inverter sends, for the switch's helper: 'both' | 'inverter' | 'none'.
    pub temps: &'static str,
    /// Show today's power cuts (T-020 § 2).
    pub show_cuts: bool,
    /// False once a reading shows no battery: no cut history, so the switch is dimmed.
    pub has_battery: bool,
}

/// The helper's case from the last reading; before the first one, 'both' (T-015 § 2).
pub fn temps_sent(m: &Monitor) -> &'static str {
    match m.last.as_ref().map(|r| (r.inverter_c, r.battery_c)) {
        Some((None, None)) => "none",
        Some((Some(_), None)) => "inverter",
        _ => "both",
    }
}

pub fn account(m: &Monitor) -> Account {
    Account { signed_in: m.signed_in, email: m.account.clone().filter(|_| m.signed_in) }
}

fn state(app: &AppHandle) -> Option<State> {
    let auto = app.try_state::<Arc<Automation>>()?;
    // The Monitor lock is let go before `view`, whose grid hook takes it again.
    let (account, temps, stats, has_battery) = {
        let m = app.state::<Arc<Mutex<Monitor>>>();
        let m = m.lock().unwrap();
        (account(&m), temps_sent(&m), stats::view(&m, std::time::Instant::now()), m.last.as_ref().is_none_or(|r| r.battery.is_some()))
    };
    let view = auto.view(account.signed_in);
    Some(State {
        pane: pane(auto.pane().as_deref()),
        account,
        general: General {
            open_at_login: app.state::<Settings>().open_at_login.load(Ordering::SeqCst),
            low_battery: auto.low_battery_pct(),
            show_temps: auto.show_temps(),
            temps,
            show_cuts: auto.show_cuts(),
            has_battery,
        },
        stats,
        about: About { version: app.package_info().version.to_string(), update: app.state::<crate::updater::Updater>().view() },
        auto: view,
    })
}

/// Sends the state to the window while it is open (shell::refresh and every automation change).
pub fn refresh(app: &AppHandle) {
    if !app.get_webview_window(WIN).is_some_and(|w| w.is_visible().unwrap_or(false)) {
        return;
    }
    if let Some(s) = state(app).and_then(|s| serde_json::to_value(s).ok()) {
        let _ = app.emit_to(WIN, "settings-state", s);
    }
}

/// Reads the login item, sets the title and, on Automation, refreshes the device list.
fn show_pane(app: &AppHandle, pane: &str) {
    let (on, _) = login_state(&login_item(app), None);
    app.state::<Settings>().open_at_login.store(on, Ordering::SeqCst);
    if let Some(w) = app.get_webview_window(WIN) {
        let _ = w.set_title(title(pane));
    }
    if pane == "automation" {
        app.state::<Arc<Automation>>().refresh_devices();
    }
}

/// The panel's cut strip (T-023): Settings on Stats, today, calendar shut; in place when it is open.
pub fn open_stats(app: &AppHandle) {
    app.state::<Arc<Automation>>().set_pane("stats");
    open(app);
}

/// Settings… in the panel, ⌘,: opens it on the last pane, or focuses it when it is open.
pub fn open(app: &AppHandle) {
    let st = app.state::<Settings>();
    if !st.loaded.load(Ordering::SeqCst) {
        st.wanted.store(true, Ordering::SeqCst);
        return;
    }
    let auto = app.state::<Arc<Automation>>();
    let waiting = app.state::<crate::updater::Updater>().available();
    let p = open_pane(auto.pane().as_deref(), waiting);
    if waiting {
        auto.set_pane(p); // the state's pane, so the page draws About
    }
    // Stats opens on today with the calendar shut, like Calendar does.
    let shown = {
        let mon = app.state::<Arc<Mutex<Monitor>>>();
        let mut m = mon.lock().unwrap();
        m.stats_cal = None;
        m.stats_day.take().is_some()
    };
    if shown {
        app.state::<Arc<tokio::sync::Notify>>().notify_one();
    }
    show_pane(app, p);
    if let Some(s) = state(app).and_then(|s| serde_json::to_value(s).ok()) {
        let _ = app.emit_to(WIN, "settings-open", s);
    }
}

/// Close button, ⌘W, Esc: a Connect in flight is dropped (nothing saved), the window hides.
pub fn close(app: &AppHandle) {
    app.state::<Arc<Automation>>().cancel_connect();
    if let Some(w) = app.get_webview_window(WIN) {
        let _ = w.hide();
    }
    // Give the keyboard back to the app the user was in, unless another window of ours is up.
    #[cfg(target_os = "macos")]
    if !["panel", "signin"].iter().any(|l| app.get_webview_window(l).is_some_and(|w| w.is_visible().unwrap_or(false))) {
        let _ = app.hide();
    }
}

#[tauri::command]
pub fn settings_ready(app: AppHandle) {
    let st = app.state::<Settings>();
    st.loaded.store(true, Ordering::SeqCst);
    if st.wanted.swap(false, Ordering::SeqCst) {
        open(&app);
    }
}

/// The page is `h` tall. The top edge stays put (AppKit keeps the bottom); at most the screen's
/// visible height − 80, then the pane scrolls. `show`: bring it forward (Accessory app: activate
/// first), centred the first time it shows.
#[tauri::command]
pub fn settings_show(app: AppHandle, win: WebviewWindow, h: f64, show: bool) {
    let visible = win.is_visible().unwrap_or(false);
    let max = win.current_monitor().ok().flatten().map_or(900.0, |m| m.work_area().size.to_logical::<f64>(m.scale_factor()).height - 80.0);
    let top = win.outer_position().ok().filter(|_| visible);
    let _ = win.set_size(LogicalSize::new(WIN_W, h.ceil().clamp(200.0, max.max(200.0))));
    match top {
        Some(p) => drop(win.set_position(p)),
        None if show => drop(win.center()),
        None => {}
    }
    if show {
        #[cfg(target_os = "macos")]
        let _ = app.show();
        let _ = win.show();
        let _ = win.set_focus();
    }
}

/// Every control of the window (spec › State and actions). Devices and Add Device… items are
/// positions; ids never reach the page. `connect` resolves with the form's error line.
#[tauri::command]
pub async fn settings_action(app: AppHandle, a: Value) -> Result<(), String> {
    let auto = app.state::<Arc<Automation>>().inner().clone();
    let dir = app.state::<shell::Shell>().log_dir.clone();
    let num = |k: &str| a[k].as_u64().map(|v| v as usize);
    let on = a["on"].as_bool().unwrap_or(false);
    match a["id"].as_str().unwrap_or_default() {
        "pane" => {
            let p = pane(a["pane"].as_str());
            auto.cancel_connect();
            auto.set_pane(p);
            show_pane(&app, p);
        }
        "signin" => signin::open(&app),
        "signout" => shell::sign_out(&app).await,
        "login" => {
            let (now, err) = login_state(&login_item(&app), Some(on));
            app.state::<Settings>().open_at_login.store(now, Ordering::SeqCst);
            logdir::line(&dir, &format!("open at login: {}", err.unwrap_or_else(|| if now { "on" } else { "off" }.into())));
        }
        "logs" => shell::open_logs(&app),
        "temps" => {
            auto.set_show_temps(on);
            app.state::<Arc<Mutex<Monitor>>>().lock().unwrap().show_temps = on;
            shell::refresh(&app);
        }
        "cuts" => {
            auto.set_show_cuts(on);
            app.state::<Arc<Mutex<Monitor>>>().lock().unwrap().show_cuts = on;
            shell::refresh(&app);
        }
        "low_battery" => {
            if let Some(p) = a["pct"].as_u64() {
                let p = auto.set_low_battery_pct(p.min(100) as u8);
                app.state::<Arc<Mutex<Monitor>>>().lock().unwrap().low_pct = p;
            }
        }
        "enable" => num("device").into_iter().for_each(|d| auto.set_on(d, on)),
        "choice" => {
            let which = if a["which"] == "on" { Which::On } else { Which::Off };
            if let (Some(d), Some(c)) = (num("device"), num("choice")) {
                auto.set_action(d, which, c);
            }
        }
        "delay" => {
            if let (Some(d), Some(s)) = (num("device"), a["delay_s"].as_u64()) {
                auto.set_delay(d, s.min(24 * 3600) as u32);
            }
        }
        "remove" => num("device").into_iter().for_each(|d| auto.remove(d)),
        "add" => num("item").into_iter().for_each(|i| auto.add(i)),
        "connect" => {
            let s = |k: &str| a[k].as_str().unwrap_or_default().trim().to_owned();
            let keys = Keys { access_id: s("access_id"), secret: a["secret"].as_str().unwrap_or_default().to_owned(), region: s("dc") };
            auto.connect(keys.clone()).await?;
            // Accepted: only now into the Keychain (off the main thread: it can wait on a prompt).
            match tauri::async_runtime::spawn_blocking(move || creds::save_tuya(&keys)).await {
                Ok(Ok(())) => {}
                _ => {
                    logdir::line(&dir, "tuya: keychain save failed");
                    auto.keys_not_saved();
                }
            }
        }
        "disconnect" => {
            auto.disconnect();
            if !matches!(tauri::async_runtime::spawn_blocking(creds::delete_tuya).await, Ok(Ok(()))) {
                logdir::line(&dir, "tuya: keychain delete failed");
            }
        }
        "howto" => drop(app.opener().open_url(HOWTO, None::<&str>)),
        "check" => crate::updater::check(&app),
        "install" => crate::updater::install(&app),
        "cancel" => crate::updater::cancel(&app),
        "github" => drop(app.opener().open_url(REPO, None::<&str>)),
        "notes" => {
            if let Some(v) = app.state::<crate::updater::Updater>().view().version {
                let _ = app.opener().open_url(format!("{REPO}/releases/tag/v{v}"), None::<&str>);
            }
        }
        "close" => close(&app),
        "day" | "today" | "calendar" | "cal_month" | "cal_day" | "retry" => {
            let wake = stats::act(&mut app.state::<Arc<Mutex<Monitor>>>().lock().unwrap(), &a, std::time::Instant::now());
            if wake {
                app.state::<Arc<tokio::sync::Notify>>().notify_one(); // the day shown goes first (§ 4.8)
            }
        }
        _ => {}
    }
    refresh(&app);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    /// A login item the user can also change behind the app's back (System Settings).
    struct Fake {
        on: Cell<bool>,
        broken: bool,
    }
    impl LoginItem for Fake {
        fn is_enabled(&self) -> bool {
            self.on.get()
        }
        fn set(&self, on: bool) -> Result<(), String> {
            if self.broken {
                return Err("denied".into());
            }
            self.on.set(on);
            Ok(())
        }
    }

    #[test]
    fn open_at_login_reflects_system_state() {
        let item = Fake { on: Cell::new(true), broken: false };
        assert_eq!(login_state(&item, None), (true, None));
        item.on.set(false); // removed in System Settings › Login Items
        assert_eq!(login_state(&item, None), (false, None), "the switch shows what the system says");
        assert_eq!(login_state(&item, Some(true)), (true, None));
        assert_eq!(login_state(&item, Some(false)), (false, None));
        // A change the system refused: the switch stays at the real state.
        let stuck = Fake { on: Cell::new(false), broken: true };
        assert_eq!(login_state(&stuck, Some(true)), (false, Some("denied".into())));
    }

    #[test]
    fn open_at_login_on_after_first_sign_in() {
        // The first sign-in turns it on in the bundled app, never under `cargo tauri dev`.
        let item = Fake { on: Cell::new(false), broken: false };
        assert!(signin::start_at_login(&item, true).is_some());
        assert!(item.is_enabled());
        let dev = Fake { on: Cell::new(false), broken: false };
        assert!(signin::start_at_login(&dev, false).is_none());
        assert!(!dev.is_enabled());
        // Already on: left alone.
        assert!(signin::start_at_login(&item, true).is_none());
        // And Settings then shows it on.
        assert_eq!(login_state(&item, None), (true, None));
    }

    #[test]
    fn pane_restored_and_title_follows_pane() {
        assert_eq!(pane(None), "general", "the first time: General");
        assert_eq!(pane(Some("automation")), "automation");
        assert_eq!(pane(Some("stats")), "stats");
        assert_eq!(pane(Some("something else")), "general");
        assert_eq!((title("general"), title("automation"), title("stats")), ("General", "Automation", "Stats"));
        assert_eq!((pane(Some("about")), title("about")), ("about", "About"));
        // T-022: the gear with its dot opens About; without it, the last pane
        assert_eq!((open_pane(Some("stats"), true), open_pane(Some("stats"), false), open_pane(None, false)), ("about", "stats", "general"));
        // The pane is saved with the settings file (automation::settings_file_has_no_secret round-trips it).
        let p = std::env::temp_dir().join(format!("gw-pane-{}.json", std::process::id()));
        let auto = Automation::new(Some(p.clone()), crate::automation::Hooks { changed: Box::new(|| {}), log: Box::new(|_| {}), grid_now: Box::new(|| None) });
        auto.set_pane("automation");
        let again = Automation::new(Some(p.clone()), crate::automation::Hooks { changed: Box::new(|| {}), log: Box::new(|_| {}), grid_now: Box::new(|| None) });
        assert_eq!(pane(again.pane().as_deref()), "automation");
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn low_battery_setting_saved_and_kept_in_range() {
        let hooks = || crate::automation::Hooks { changed: Box::new(|| {}), log: Box::new(|_| {}), grid_now: Box::new(|| None) };
        let p = std::env::temp_dir().join(format!("gw-low-{}.json", std::process::id()));
        // A settings file from before T-008 (no field): the default.
        std::fs::write(&p, r#"{"devices":[],"pane":"general"}"#).unwrap();
        let auto = Automation::new(Some(p.clone()), hooks());
        assert_eq!(auto.low_battery_pct(), 20);
        assert_eq!(auto.set_low_battery_pct(30), 30);
        assert_eq!(Automation::new(Some(p.clone()), hooks()).low_battery_pct(), 30, "saved");
        assert_eq!((auto.set_low_battery_pct(1), auto.set_low_battery_pct(90)), (5, 50));
        std::fs::write(&p, r#"{"low_battery_pct":99}"#).unwrap();
        assert_eq!(Automation::new(Some(p.clone()), hooks()).low_battery_pct(), 50, "an edited file is kept in range");
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn account_only_while_signed_in() {
        let mut m = Monitor::new(true);
        assert_eq!(account(&m), Account { signed_in: true, email: None }, "before the Keychain answers: 'Signed in'");
        m.account = Some("name@example.com".into());
        assert_eq!(account(&m).email.as_deref(), Some("name@example.com"));
        m.signed_in = false; // e.g. WrongLogin before the reset
        assert_eq!(account(&m), Account { signed_in: false, email: None });
        m.reset(false);
        assert_eq!(account(&m), Account { signed_in: false, email: None });
        assert_eq!(serde_json::to_value(account(&Monitor::new(false))).unwrap(), serde_json::json!({ "signedIn": false }));
    }

    #[test]
    fn settings_window_has_start_dragging_permission() {
        let cap: Value = serde_json::from_str(include_str!("../capabilities/default.json")).unwrap();
        assert!(cap["windows"].as_array().unwrap().iter().any(|w| w == WIN));
        assert!(cap["permissions"].as_array().unwrap().iter().any(|p| p == "core:window:allow-start-dragging"));
        // And the window itself: Overlay title bar, 560 wide, not resizable, hidden until opened.
        let conf: Value = serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        let w = conf["app"]["windows"].as_array().unwrap().iter().find(|w| w["label"] == WIN).unwrap();
        assert_eq!((w["titleBarStyle"].as_str(), w["width"].as_f64(), w["resizable"].as_bool(), w["visible"].as_bool()), (Some("Overlay"), Some(WIN_W), Some(false), Some(false)));
    }

    #[test]
    fn settings_title_strip_is_empty() {
        // D-O11: the native title bar draws the traffic lights and the pane title; the strip under it is empty.
        let js = include_str!("../../ui/settings.js");
        assert!(js.contains(r#"<div class="titlebar" data-drag></div>"#));
        for bad in [r#"class="tl""#, r#"class="tt""#] {
            assert!(!js.contains(bad), "{bad}");
        }
        // D-O10: the toolbar's gaps drag too; the tab buttons don't carry it. B-R2: our own handler starts the drag on
        // a move, so Tauri's mousedown drag region must not be used anywhere.
        assert!(js.contains(r#"role="tablist" aria-label="Settings" data-drag>"#));
        assert!(js.contains("plugin:window|start_dragging") && !js.contains("data-tauri-drag-region"));
        let css = include_str!("../../ui/settings.css");
        assert!(!css.contains("MOCK ONLY"), "the mock's fake title bar and pop-ups never ship");
    }

    #[test]
    fn settings_state_shape() {
        let auto = Automation::new(None, crate::automation::Hooks { changed: Box::new(|| {}), log: Box::new(|_| {}), grid_now: Box::new(|| None) });
        let s = State {
            pane: "general",
            account: account(&Monitor::new(false)),
            general: General { open_at_login: true, low_battery: 20, show_temps: true, temps: "both", show_cuts: true, has_battery: true },
            stats: stats::view(&Monitor::new(false), std::time::Instant::now()),
            about: About { version: "0.1.2".into(), update: crate::updater::Updates::new("0.1.2", Default::default()).view() },
            auto: auto.view(false),
        };
        assert!(auto.show_cuts(), "T-020: on by default, also for a saved file without it");
        let v = serde_json::to_value(s).unwrap();
        for k in ["pane", "account", "general", "stats", "about", "automation", "tuya", "devices", "add"] {
            assert!(v.get(k).is_some(), "missing {k}: {v}");
        }
        assert_eq!(v["general"], serde_json::json!({ "openAtLogin": true, "lowBattery": 20, "showTemps": true, "temps": "both", "showCuts": true, "hasBattery": true }));
        assert_eq!(v["about"], serde_json::json!({ "version": "0.1.2", "update": { "kind": "current" } }));
    }
}

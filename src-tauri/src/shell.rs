//! The macOS face of the app: the tray item, the panel window under it, the panel's
//! commands and the notifications. It applies `view` and never decides anything itself.

use crate::monitor::{Monitor, PollReport};
use crate::notify::{self, Permission};
use crate::view::{self, TrayIcon, TrayLook};
use crate::logdir;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::image::Image;
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{App, AppHandle, Emitter, LogicalPosition, LogicalSize, Manager, Rect, WebviewWindow, WindowEvent};
use tauri_plugin_opener::OpenerExt;

const TRAY: &str = "main";
const PANEL: &str = "panel";
const PANEL_W: f64 = 340.0;
/// A tray click this soon after a blur-close is the click that blurred the panel.
const REOPEN_GUARD: Duration = Duration::from_millis(200);
const NOTIFICATION_SETTINGS: &str = "x-apple.systempreferences:com.apple.Notifications-Settings.extension?id=dev.goodwestatus.menubar";

pub struct Shell {
    pub log_dir: PathBuf,
    last_blur: Mutex<Option<Instant>>,
    notifications_off: AtomicBool,
    applied: Mutex<Option<TrayLook>>,
}

fn icon(i: TrayIcon) -> (Image<'static>, bool) {
    let (bytes, template): (&'static [u8], bool) = match i {
        TrayIcon::Plug => (include_bytes!("../icons/plug-template@2x.png"), true),
        TrayIcon::OffRed => (include_bytes!("../icons/plug-off-red@2x.png"), false),
        TrayIcon::Dim => (include_bytes!("../icons/plug-dim-template@2x.png"), true),
        TrayIcon::OffDim => (include_bytes!("../icons/plug-off-dim-template@2x.png"), true),
    };
    (Image::from_bytes(bytes).expect("bundled tray icon"), template)
}

pub fn setup(app: &mut App, log_dir: PathBuf) -> tauri::Result<()> {
    app.manage(Shell { log_dir, last_blur: Mutex::new(None), notifications_off: AtomicBool::new(false), applied: Mutex::new(None) });
    let win = app.get_webview_window(PANEL).expect("panel window in tauri.conf.json");
    let h = app.handle().clone();
    win.on_window_event(move |e| {
        if let WindowEvent::Focused(false) = e {
            *h.state::<Shell>().last_blur.lock().unwrap() = Some(Instant::now());
            hide(&h, false);
        }
    });
    let (img, template) = icon(TrayIcon::Plug);
    // No `.menu()`: a left click opens the panel, never a menu.
    TrayIconBuilder::with_id(TRAY)
        .icon(img)
        .icon_as_template(template)
        .tooltip("Getting the first reading…")
        .on_tray_icon_event(|tray, ev| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, rect, .. } = ev {
                toggle(tray.app_handle(), rect);
            }
        })
        .build(app)?;
    refresh(app.handle());
    Ok(())
}

fn toggle(app: &AppHandle, rect: Rect) {
    let Some(win) = app.get_webview_window(PANEL) else { return };
    if win.is_visible().unwrap_or(false) {
        hide(app, true);
        return;
    }
    if app.state::<Shell>().last_blur.lock().unwrap().is_some_and(|t| t.elapsed() < REOPEN_GUARD) {
        return;
    }
    place(app, &win, rect);
    #[cfg(target_os = "macos")]
    let _ = app.show(); // undo an earlier `app.hide()`, or the window can't come forward
    let _ = win.show();
    let _ = win.set_focus();
    refresh(app);
    check_notifications(app);
}

/// Left edge = the item's left edge, 4 pt under it, kept 8 pt inside the item's screen.
fn place(app: &AppHandle, win: &WebviewWindow, rect: Rect) {
    // The screen holding the item, matched in physical pixels (each screen's own scale).
    // Not `monitor_from_point`: on macOS it wants points, while the tray rect is physical.
    let item = rect.position.to_physical::<f64>(1.0);
    let mons = app.available_monitors().unwrap_or_default();
    let idx = mons.iter().position(|m| {
        let (mp, ms) = (m.position(), m.size());
        (mp.x as f64..mp.x as f64 + ms.width as f64).contains(&item.x) && (mp.y as f64..mp.y as f64 + ms.height as f64).contains(&item.y)
    });
    // T-005: the panel once opened on the other screen; this line says where and why next time.
    let screen = idx.map_or("none".to_owned(), |i| {
        let (mp, ms) = (mons[i].position(), mons[i].size());
        format!("{} of {} at {},{} {}x{}", i + 1, mons.len(), mp.x, mp.y, ms.width, ms.height)
    });
    let isize = rect.size.to_physical::<f64>(1.0);
    let mon = idx.map(|i| mons[i].clone());
    let scale = mon.as_ref().map_or_else(|| win.scale_factor().unwrap_or(2.0), |m| m.scale_factor());
    let (p, s) = (rect.position.to_logical::<f64>(scale), rect.size.to_logical::<f64>(scale));
    let mut x = p.x;
    if let Some(m) = mon {
        let (mp, ms) = (m.position().to_logical::<f64>(scale), m.size().to_logical::<f64>(scale));
        x = x.clamp(mp.x + 8.0, (mp.x + ms.width - PANEL_W - 8.0).max(mp.x + 8.0));
    }
    let y = p.y + s.height + 4.0;
    let _ = win.set_position(LogicalPosition::new(x, y));
    let line = format!("panel placed: screen {screen} · item {},{} {}x{} · scale {scale} → {x},{y}", item.x, item.y, isize.width, isize.height);
    logdir::line(&app.state::<Shell>().log_dir, &line);
}

/// `give_back_focus`: after Esc or an action, hand the keyboard back to the app the user was in.
fn hide(app: &AppHandle, give_back_focus: bool) {
    if let Some(win) = app.get_webview_window(PANEL) {
        let _ = win.hide();
    }
    // Not while Settings is open: hiding the app would hide it too.
    #[cfg(target_os = "macos")]
    if give_back_focus && !app.get_webview_window("settings").is_some_and(|w| w.is_visible().unwrap_or(false)) {
        let _ = app.hide();
    }
}

/// Applies the current state to the tray and sends it to the panel.
pub fn refresh(app: &AppHandle) {
    let shell = app.state::<Shell>();
    let now = Instant::now();
    let (mut panel, look) = {
        let mon = app.state::<Arc<Mutex<Monitor>>>();
        let m = mon.lock().unwrap();
        (view::panel(&m, now, shell.notifications_off.load(Ordering::Relaxed)), view::tray(&m, now))
    };
    panel.update_available = app.try_state::<crate::updater::Updater>().is_some_and(|u| u.available());
    let mut applied = shell.applied.lock().unwrap();
    if let (Some(tray), true) = (app.tray_by_id(TRAY), applied.as_ref() != Some(&look)) {
        // Icon and template flag in one call: two calls flicker.
        if applied.as_ref().map(|a| a.icon) != Some(look.icon) {
            let (img, template) = icon(look.icon);
            let _ = tray.set_icon_with_as_template(Some(img), template);
        }
        let _ = tray.set_title(look.title.as_deref());
        let _ = tray.set_tooltip(Some(&look.tooltip));
        *applied = Some(look);
    }
    let _ = app.emit_to(PANEL, "state", panel);
    drop(applied);
    crate::settings::refresh(app);
}

/// Re-reads the notification setting (panel open, every poll) and redraws when it changed.
fn check_notifications(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let p = notify::permission().await;
        let off = notify::is_off(p);
        let shell = app.state::<Shell>();
        if shell.notifications_off.swap(off, Ordering::Relaxed) != off {
            logdir::line(&shell.log_dir, &format!("notifications: {p:?}"));
            refresh(&app);
        }
    });
}

/// Asks for permission once (never asked before) and redraws. The sign-in box calls this
/// right after the first sign-in succeeds.
pub fn ask_notifications(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        if notify::permission().await == Some(Permission::NotAsked) {
            notify::ask().await;
        }
        check_notifications(&app);
    });
}

/// After every poll, twice: the reading's part first (notices, Tuya, redraw), then the day calls'
/// part. Logs, notifies on a confirmed grid change and on low battery, redraws.
pub fn after_poll(app: &AppHandle, rep: PollReport) {
    let dir = &app.state::<Shell>().log_dir;
    log_report(dir, &rep);
    if let Some(d) = &rep.disagreement {
        logdir::line(dir, &format!("grid signals disagree, state kept: {d}"));
    }
    if let Some(n) = rep.notice {
        alert(dir, n.text());
        // After the alert is on its way; the lights run in their own tasks and never hold it up.
        let at = app.state::<Arc<Mutex<Monitor>>>().lock().unwrap().station_now(Instant::now());
        if let Some(at) = at {
            app.state::<Arc<crate::automation::Automation>>().grid_changed(n.grid, at);
        }
    }
    // After the grid notice when one reading brings both.
    if let Some(soc) = rep.low_battery {
        alert(dir, crate::rules::low_battery_text(soc));
    }
    refresh(app);
    check_notifications(app);
}

/// A poll's or the history walk's log lines: recoveries first (they clear old failures), then
/// this report's failures, then the stale checks.
pub fn log_report(dir: &std::path::Path, rep: &PollReport) {
    rep.oks.iter().for_each(|c| logdir::ok(dir, c));
    rep.fails.iter().for_each(|f| logdir::fail(dir, f));
    rep.notes.iter().for_each(|n| logdir::line(dir, n));
}

/// A notification with sound, logged; sent in its own task.
fn alert(dir: &std::path::Path, text: String) {
    logdir::line(dir, &format!("notice: {text}"));
    let dir = dir.to_owned();
    tauri::async_runtime::spawn(async move {
        if let Err(e) = notify::send(text).await {
            logdir::line(&dir, &format!("notification not sent: {e}"));
        }
    });
}

#[tauri::command]
pub fn panel_height(win: WebviewWindow, h: f64) {
    let _ = win.set_size(LogicalSize::new(PANEL_W, h.ceil().clamp(40.0, 900.0)));
}

/// The panel's action rows, the Refresh button and keys. Refresh keeps the panel open (you see
/// the result); every other action closes it.
#[tauri::command]
pub fn panel_action(app: AppHandle, id: String) {
    match id.as_str() {
        "refresh" => {}
        "close" => hide(&app, true),
        _ => hide(&app, false),
    }
    match id.as_str() {
        "refresh" => {
            let start = app.state::<Arc<Mutex<Monitor>>>().lock().unwrap().ask_refresh();
            let Some(start) = start else { return }; // one poll at a time
            refresh(&app); // the spin, at once
            if start {
                let _ = app.state::<tokio::sync::mpsc::Sender<()>>().try_send(());
            }
        }
        "quit" => app.exit(0),
        "settings" => crate::settings::open(&app),
        "stats" => crate::settings::open_stats(&app),
        "notif" => {
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                // Never asked: the system prompt is the way on. Asked before: System Settings.
                if notify::permission().await == Some(Permission::NotAsked) {
                    ask_notifications(&app);
                } else {
                    let _ = app.opener().open_url(NOTIFICATION_SETTINGS, None::<&str>);
                }
            });
        }
        "signin" => crate::signin::open(&app),
        // "close": hiding was all.
        _ => {}
    }
}

/// Settings › Open Log Folder: the log folder in Finder.
pub fn open_logs(app: &AppHandle) {
    let dir = app.state::<Shell>().log_dir.clone();
    let _ = std::fs::create_dir_all(&dir);
    let _ = app.opener().open_path(dir.to_string_lossy(), None::<&str>);
}

/// Settings › Sign Out: the Keychain item, then the client, then the monitor. A poll holding
/// the client is waited for, but the app signs out at once (the monitor's generation makes
/// that poll drop its result).
pub async fn sign_out(app: &AppHandle) {
    let dir = app.state::<Shell>().log_dir.clone();
    match tauri::async_runtime::spawn_blocking(crate::creds::delete).await {
        Ok(Ok(())) => {}
        // Still signed out for this run; the next launch would find the item. No account in the log.
        Ok(Err(e)) => logdir::line(&dir, &format!("sign-out: keychain delete failed: {}", keychain_kind(&e))),
        Err(_) => logdir::line(&dir, "sign-out: keychain delete failed: task"),
    }
    let client = app.state::<Arc<tokio::sync::Mutex<crate::sems::Client>>>().inner().clone();
    let mut held = client.try_lock().ok();
    if let Some(c) = held.as_mut() {
        c.sign_out();
    }
    app.state::<Arc<Mutex<Monitor>>>().lock().unwrap().reset(false);
    logdir::line(&dir, "signed out");
    refresh(app);
    if held.is_none() {
        client.lock().await.sign_out();
    }
}

/// The error's kind only: a keyring message may carry the item's attributes.
fn keychain_kind(e: &keyring::Error) -> &'static str {
    match e {
        keyring::Error::PlatformFailure(_) => "platform failure",
        keyring::Error::NoStorageAccess(_) => "no storage access",
        keyring::Error::NoEntry => "no entry",
        _ => "other",
    }
}

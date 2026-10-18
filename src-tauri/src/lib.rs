//! GoodWe Status: battery % and grid on/off in the menu bar.

pub mod automation;
pub mod clock;
pub mod creds;
pub mod logdir;
pub mod monitor;
pub mod notify;
pub mod outages;
pub mod rules;
pub mod sems;
pub mod settings;
pub mod shell;
pub mod signin;
pub mod tuya;
pub mod view;

use std::sync::{Arc, Mutex};
use tauri::Manager;

pub fn run() {
    tauri::Builder::default()
        // Tray-only app: no default app menu, or its ⌘Q/⌘W/⌘H/⌘M reach the panel and the box and
        // leave a dead tray item. Only Edit stays, so paste and password managers work in the fields.
        .enable_macos_default_menu(false)
        .menu(|h| {
            use tauri::menu::{Menu, PredefinedMenuItem as P, Submenu};
            let edit = Submenu::with_items(
                h,
                "Edit",
                true,
                &[&P::undo(h, None)?, &P::redo(h, None)?, &P::separator(h)?, &P::cut(h, None)?, &P::copy(h, None)?, &P::paste(h, None)?, &P::select_all(h, None)?],
            )?;
            // The first submenu is always the app menu on macOS: keep it empty.
            Menu::with_items(h, &[&Submenu::with_items(h, "GoodWe Status", true, &[])?, &edit])
        })
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_autostart::Builder::new().build())
        .invoke_handler(tauri::generate_handler![
            shell::panel_height,
            shell::panel_action,
            signin::signin_ready,
            signin::signin_show,
            signin::signin_cancel,
            signin::signin_submit,
            settings::settings_ready,
            settings::settings_show,
            settings::settings_action
        ])
        .setup(|app| {
            // No Dock icon, also under `cargo tauri dev` (the bundle has LSUIElement too).
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);
            let log_dir = app.path().app_log_dir()?;
            app.manage(signin::SignIn::default());
            app.manage(settings::Settings::default());
            // "Getting the first reading…" until the Keychain has answered.
            let mon = Arc::new(Mutex::new(monitor::Monitor::new(true)));
            let client = Arc::new(tokio::sync::Mutex::new(sems::Client::new(None)));
            let (refresh_tx, refresh_rx) = tokio::sync::mpsc::channel::<()>(4);
            app.manage(refresh_tx);
            app.manage(mon.clone());
            app.manage(client.clone());
            logdir::line(&log_dir, "started");
            let auto = {
                let (m, d, h) = (mon.clone(), log_dir.clone(), app.handle().clone());
                automation::Automation::new(
                    app.path().app_data_dir().ok().map(|p| p.join("settings.json")),
                    automation::Hooks {
                        changed: Box::new(move || settings::refresh(&h)),
                        log: Box::new(move |s| logdir::line(&d, s)),
                        off_now: Box::new(move || {
                            let m = m.lock().unwrap();
                            let now = std::time::Instant::now();
                            if m.grid != rules::GridState::Off || m.stale(now) {
                                return None;
                            }
                            let at = m.station_now(now)?;
                            Some(automation::OffNow { at, off_for_s: m.off_since.map_or(0, |o| at.0 - o.at.0) })
                        }),
                    },
                )
            };
            app.manage(auto.clone());
            // The tray item first: a Keychain prompt must never hold up the menu bar.
            shell::setup(app, log_dir)?;
            // The close button hides Settings (one instance, opened again by Settings…).
            let h = app.handle().clone();
            app.get_webview_window("settings").expect("settings window in tauri.conf.json").on_window_event(move |e| {
                if let tauri::WindowEvent::CloseRequested { api, .. } = e {
                    api.prevent_close();
                    settings::close(&h);
                }
            });
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                // Off the main thread: the read can wait on a Keychain prompt.
                let creds = tauri::async_runtime::spawn_blocking(creds::load).await.ok().flatten();
                // After the SEMS item, never two Keychain prompts at once.
                auto.set_keys(tauri::async_runtime::spawn_blocking(creds::load_tuya).await.ok().flatten());
                let signed_in = {
                    let mut c = client.lock().await;
                    // Unless the user already signed in through the box meanwhile.
                    if c.account().is_none() {
                        *c = sems::Client::new(creds);
                    }
                    let account = c.account();
                    let mut m = mon.lock().unwrap();
                    if m.account.is_none() {
                        m.account = account.clone();
                    }
                    account.is_some()
                };
                if !signed_in {
                    // First run, or the Keychain said no (e.g. Deny after an update): ask in the box.
                    handle.state::<signin::SignIn>().first_run.store(true, std::sync::atomic::Ordering::SeqCst);
                    mon.lock().unwrap().signed_in = false;
                    shell::refresh(&handle);
                    signin::open(&handle);
                }
                let h = handle.clone();
                monitor::run(client, mon, refresh_rx, move |rep| shell::after_poll(&h, rep)).await;
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running GoodWe Status");
}

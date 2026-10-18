//! System notifications. macOS goes through Apple's UNUserNotificationCenter
//! (`mac-usernotifications`), the only route with a real permission state and the default
//! sound (5a item 5). It needs the bundled, signed .app: under `cargo tauri dev` every call
//! returns an error and the app behaves as if it can't tell (no "Notifications are off").

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Permission {
    NotAsked,
    On,
    /// Denied, or allowed with alerts turned off.
    Off,
}

#[cfg(target_os = "macos")]
mod imp {
    use super::Permission;
    use mac_usernotifications as un;

    pub async fn permission() -> Option<Permission> {
        let s = un::get_notification_settings().await.ok()?;
        Some(match s.authorization_status {
            un::AuthorizationStatus::NotDetermined => Permission::NotAsked,
            un::AuthorizationStatus::Authorized if s.alert_enabled != un::NotificationSettingStatus::Disabled => Permission::On,
            _ => Permission::Off,
        })
    }

    pub async fn ask() {
        let _ = un::request_auth().await;
    }

    pub async fn send(title: String) -> Result<(), String> {
        un::Notification::new().title(title).default_sound().send().await.map(|_| ()).map_err(|e| e.to_string())
    }
}

// shortcut: no notifications off macOS yet; add notify-rust when Windows/Linux builds start.
#[cfg(not(target_os = "macos"))]
mod imp {
    use super::Permission;
    pub async fn permission() -> Option<Permission> {
        None
    }
    pub async fn ask() {}
    pub async fn send(_: String) -> Result<(), String> {
        Err("notifications are macOS-only for now".into())
    }
}

pub use imp::{ask, permission, send};

/// Whether the panel shows "Notifications are off". Unknown (no bundle) counts as on.
pub fn is_off(p: Option<Permission>) -> bool {
    matches!(p, Some(Permission::NotAsked | Permission::Off))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn off_states() {
        assert!(is_off(Some(Permission::Off)));
        assert!(is_off(Some(Permission::NotAsked)));
        assert!(!is_off(Some(Permission::On)));
        assert!(!is_off(None), "dev build without a bundle can't tell: don't nag");
    }
}

//! The Mac's accent colour for the pages (T-029 Q2). WKWebView's CSS `AccentColor` is always blue, so
//! Rust reads `NSColor.controlAccentColor` and sets `--mac-accent` on each page's `<html>`. tokens.css uses
//! it in light mode for switches and focus rings only (never text or a label's fill, D-O4), so no `--on-accent`.

use tauri::{AppHandle, Manager};

pub type Rgb = [u8; 3];

/// Light `--surface-3`: the darkest light ground a focus ring or switch-on sits on (B-Y7; --frame is #F1F1F3).
const GROUND: Rgb = [0xEB, 0xEB, 0xED];

/// WCAG contrast ratio of two sRGB colours.
fn contrast(a: Rgb, b: Rgb) -> f64 {
    let lum = |c: Rgb| {
        let f = |v: u8| {
            let v = v as f64 / 255.0;
            if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
        };
        0.2126 * f(c[0]) + 0.7152 * f(c[1]) + 0.0722 * f(c[2])
    };
    let (x, y) = (lum(a), lum(b));
    (x.max(y) + 0.05) / (x.min(y) + 0.05)
}

/// The script that sets `--mac-accent` on a page; an accent under 3:1 on the darkest light ground is not pushed,
/// so the ring and switch-on keep the design blue (B-O2).
pub fn script(c: Rgb) -> String {
    if contrast(c, GROUND) < 3.0 {
        return "document.documentElement.style.removeProperty('--mac-accent')".into();
    }
    format!(
        "document.documentElement.style.setProperty('--mac-accent','#{:02X}{:02X}{:02X}')",
        c[0], c[1], c[2]
    )
}

/// The accent as light mode draws it, in sRGB.
#[cfg(target_os = "macos")]
fn read() -> Option<Rgb> {
    use objc2_app_kit::{NSAppearance, NSAppearanceNameAqua, NSColor, NSColorSpace};
    let out = std::cell::Cell::new(None);
    let aqua = NSAppearance::appearanceNamed(unsafe { NSAppearanceNameAqua })?;
    aqua.performAsCurrentDrawingAppearance(&block2::StackBlock::new(|| {
        let c = NSColor::controlAccentColor().colorUsingColorSpace(&NSColorSpace::sRGBColorSpace());
        out.set(c.map(|c| [c.redComponent(), c.greenComponent(), c.blueComponent()].map(|v| (v * 255.0).round().clamp(0.0, 255.0) as u8)));
    }));
    out.get()
}

#[cfg(not(target_os = "macos"))]
fn read() -> Option<Rgb> {
    None
}

/// Reads the accent (on the main thread, AppKit's) and sets it on every page. Called when a window
/// opens and when the system colours change.
pub fn apply(app: &AppHandle) {
    let h = app.clone();
    let _ = app.run_on_main_thread(move || {
        let Some(js) = read().map(script) else { return };
        for w in h.webview_windows().values() {
            let _ = w.eval(&js);
        }
    });
}

/// Live: AppKit posts NSSystemColorsDidChangeNotification after an accent change in System Settings.
#[cfg(target_os = "macos")]
pub fn watch(app: &AppHandle) {
    use objc2_app_kit::NSSystemColorsDidChangeNotification;
    use objc2_foundation::{NSNotification, NSNotificationCenter, NSOperationQueue};
    let h = app.clone();
    let block = block2::RcBlock::new(move |_: std::ptr::NonNull<NSNotification>| apply(&h));
    let token = unsafe {
        NSNotificationCenter::defaultCenter().addObserverForName_object_queue_usingBlock(
            Some(NSSystemColorsDidChangeNotification),
            None,
            Some(&NSOperationQueue::mainQueue()),
            &block,
        )
    };
    std::mem::forget(token); // observes for the app's whole life
}

#[cfg(not(target_os = "macos"))]
pub fn watch(_: &AppHandle) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn script_sets_the_accent_as_hex() {
        assert_eq!(
            script([0x00, 0x7A, 0xFF]),
            "document.documentElement.style.setProperty('--mac-accent','#007AFF')"
        );
    }

    #[test]
    fn accents_under_3_to_1_on_surface_3_fall_back_to_the_design_blue() {
        // Light-mode values from this Mac (research Q2); graphite ≈ #989898 (D-O4: 2.88 on white).
        let drop = "document.documentElement.style.removeProperty('--mac-accent')";
        for (name, c) in [("yellow", [0xFF, 0xC7, 0x26]), ("orange", [0xF7, 0x82, 0x1B]), ("graphite", [0x98, 0x98, 0x98]), ("green", [0x62, 0xBA, 0x46]), ("pink", [0xF7, 0x4F, 0x9E])] {
            assert_eq!(script(c), drop, "{name}");
        }
        assert!(script([0x00, 0x7A, 0xFF]).contains("#007AFF"), "blue keeps");
        assert!(script([0x95, 0x3D, 0x96]).contains("#953D96"), "purple keeps");
        assert!(script([0xE0, 0x38, 0x3E]).contains("#E0383E"), "red keeps (3.67 on #EBEBED)");
        assert!(contrast([0x00, 0x7A, 0xFF], GROUND) >= 3.0, "blue 3.37 on #EBEBED");
    }
}

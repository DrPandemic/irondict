//! Best-effort OS theme detection (accent color + light/dark), so irondict
//! blends with the desktop.
//!
//! Detection order (first hit wins): the cross-desktop XDG portal
//! `org.freedesktop.appearance`, then GNOME `gsettings`, then sensible fallbacks
//! (indigo accent, light mode). It runs on a worker thread so it never blocks
//! startup; the UI is updated through the Slint event loop once a result is
//! available.

use slint::{Color, Weak};

use crate::AppWindow;

const INDIGO: (u8, u8, u8) = (0x4f, 0x46, 0xe5);

/// Detect the OS accent and light/dark preference in the background and apply
/// them to `ui` when ready. The derived palette (tints, dark flip) lives in the
/// `.slint`, so here we only push the raw accent and the dark boolean.
pub fn apply_os_theme(ui: Weak<AppWindow>) {
    std::thread::spawn(move || {
        let (r, g, b) = detect_accent();
        let dark = detect_dark();
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(ui) = ui.upgrade() {
                ui.set_os_accent(Color::from_rgb_u8(r, g, b));
                ui.set_dark(dark);
            }
        });
    });
}

fn detect_accent() -> (u8, u8, u8) {
    portal_accent().or_else(gsettings_accent).unwrap_or(INDIGO)
}

/// Whether the desktop prefers a dark color scheme (defaults to light).
///
/// `IRONDICT_DARK` overrides detection (`1`/`true`/`dark` → dark, anything else →
/// light), which is handy for testing on desktops that don't report a
/// preference.
fn detect_dark() -> bool {
    if let Ok(v) = std::env::var("IRONDICT_DARK") {
        return matches!(v.to_lowercase().as_str(), "1" | "true" | "dark" | "yes");
    }
    portal_dark().or_else(gsettings_dark).unwrap_or(false)
}

/// XDG desktop portal: `org.freedesktop.appearance` / `accent-color` → `(ddd)`.
fn portal_accent() -> Option<(u8, u8, u8)> {
    let conn = zbus::blocking::Connection::session().ok()?;
    let reply = conn
        .call_method(
            Some("org.freedesktop.portal.Desktop"),
            "/org/freedesktop/portal/desktop",
            Some("org.freedesktop.portal.Settings"),
            "Read",
            &("org.freedesktop.appearance", "accent-color"),
        )
        .ok()?;
    let value: zbus::zvariant::OwnedValue = reply.body().deserialize().ok()?;
    extract_rgb(&value)
}

/// Recursively unwrap variant layers to a struct of three doubles in `[0, 1]`.
fn extract_rgb(v: &zbus::zvariant::Value) -> Option<(u8, u8, u8)> {
    use zbus::zvariant::Value;
    match v {
        Value::Value(inner) => extract_rgb(inner),
        Value::Structure(s) => {
            let fields = s.fields();
            if fields.len() != 3 {
                return None;
            }
            let as_f64 = |x: &Value| match x {
                Value::F64(d) => Some(*d),
                _ => None,
            };
            let r = as_f64(&fields[0])?;
            let g = as_f64(&fields[1])?;
            let b = as_f64(&fields[2])?;
            if r < 0.0 || g < 0.0 || b < 0.0 {
                return None; // portal reports (-1, -1, -1) when unset
            }
            let to = |x: f64| (x.clamp(0.0, 1.0) * 255.0).round() as u8;
            Some((to(r), to(g), to(b)))
        }
        _ => None,
    }
}

/// GNOME fallback: `gsettings get org.gnome.desktop.interface accent-color`.
fn gsettings_accent() -> Option<(u8, u8, u8)> {
    let out = std::process::Command::new("gsettings")
        .args(["get", "org.gnome.desktop.interface", "accent-color"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout);
    named_accent(s.trim().trim_matches('\''))
}

/// XDG desktop portal: `org.freedesktop.appearance` / `color-scheme` → `u`,
/// where `1` means "prefer dark" (`0` = no preference, `2` = prefer light).
fn portal_dark() -> Option<bool> {
    let conn = zbus::blocking::Connection::session().ok()?;
    let reply = conn
        .call_method(
            Some("org.freedesktop.portal.Desktop"),
            "/org/freedesktop/portal/desktop",
            Some("org.freedesktop.portal.Settings"),
            "Read",
            &("org.freedesktop.appearance", "color-scheme"),
        )
        .ok()?;
    let value: zbus::zvariant::OwnedValue = reply.body().deserialize().ok()?;
    Some(extract_u32(&value)? == 1)
}

/// Recursively unwrap variant layers to a `u32`.
fn extract_u32(v: &zbus::zvariant::Value) -> Option<u32> {
    use zbus::zvariant::Value;
    match v {
        Value::Value(inner) => extract_u32(inner),
        Value::U32(n) => Some(*n),
        _ => None,
    }
}

/// GNOME fallback: `gsettings get org.gnome.desktop.interface color-scheme`,
/// which is one of `'default'`, `'prefer-dark'`, `'prefer-light'`.
fn gsettings_dark() -> Option<bool> {
    let out = std::process::Command::new("gsettings")
        .args(["get", "org.gnome.desktop.interface", "color-scheme"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout);
    Some(s.contains("prefer-dark"))
}

/// The GNOME 47 named accent palette.
fn named_accent(name: &str) -> Option<(u8, u8, u8)> {
    Some(match name {
        "blue" => (0x35, 0x84, 0xe4),
        "teal" => (0x21, 0x90, 0xa4),
        "green" => (0x3a, 0x94, 0x4a),
        "yellow" => (0xc8, 0x88, 0x00),
        "orange" => (0xed, 0x5b, 0x00),
        "red" => (0xe6, 0x2d, 0x42),
        "pink" => (0xd5, 0x61, 0x99),
        "purple" => (0x91, 0x41, 0xac),
        "slate" => (0x6f, 0x83, 0x96),
        _ => return None,
    })
}

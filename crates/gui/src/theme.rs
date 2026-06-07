//! Best-effort OS accent-color detection, so irondict blends with the desktop.
//!
//! Detection order (first hit wins): the cross-desktop XDG portal
//! `org.freedesktop.appearance` / `accent-color`, then GNOME `gsettings`, then an
//! indigo fallback. It runs on a worker thread so it never blocks startup; the
//! UI is updated through the Slint event loop once a result is available.

use slint::{Color, Weak};

use crate::AppWindow;

const INDIGO: (u8, u8, u8) = (0x4f, 0x46, 0xe5);

/// Detect the OS accent in the background and apply it to `ui` when ready.
pub fn apply_os_accent(ui: Weak<AppWindow>) {
    std::thread::spawn(move || {
        let (r, g, b) = detect_accent();
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(ui) = ui.upgrade() {
                ui.set_accent(Color::from_rgb_u8(r, g, b));
                ui.set_accent_tint(tint(r, g, b));
            }
        });
    });
}

fn detect_accent() -> (u8, u8, u8) {
    portal_accent().or_else(gsettings_accent).unwrap_or(INDIGO)
}

/// A light wash of the accent over white, for selected rows and pills.
fn tint(r: u8, g: u8, b: u8) -> Color {
    let mix = |c: u8| (0.88 * 255.0 + 0.12 * c as f32).round() as u8;
    Color::from_rgb_u8(mix(r), mix(g), mix(b))
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

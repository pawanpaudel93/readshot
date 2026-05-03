//! System tray integration (Phase A.5).
//!
//! Builds a small icon at runtime, registers it as a tray icon with a
//! two-item menu (Capture / Quit), and exposes a non-blocking
//! [`TrayController::drain`] that the iced runtime polls every few ms
//! to surface tray + menu events as typed actions. A left-click on
//! the icon itself fires Capture too, matching the muscle memory of
//! every menu-bar capture tool on the platform.
//!
//! ## macOS specifics
//!
//! tray-icon must be constructed on the main thread (the one that owns
//! the NSApplication run loop). iced 0.14's daemon runs on the main
//! thread, so calling [`install`] from `runtime::start` is correct.
//!
//! ## Tests
//!
//! There's not much that's testable headlessly here — the tray surface
//! is OS-level and you'd need a window-server to assert anything
//! meaningful. The tests we do have cover the icon-bytes generator
//! (so the buffer round-trips through `Icon::from_rgba`) and the
//! menu-id → action mapping logic.

use std::collections::HashMap;

use muda::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
use tiny_skia::{FillRule, Paint, PathBuilder, Pixmap, Stroke, Transform};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder, TrayIconEvent};

/// User intent surfaced by the tray. `Capture` triggers the region
/// overlay; `History` opens the persistent capture browser;
/// `Settings` opens the preferences window; `CheckForUpdates` opens
/// Sparkle's standard updater UI on macOS; `Quit` exits the daemon.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayAction {
    Capture,
    History,
    Settings,
    CheckForUpdates,
    Quit,
}

#[cfg(test)]
#[derive(Clone, Debug, PartialEq, Eq)]
struct TrayMenuItem {
    label: String,
    action: Option<TrayAction>,
}

#[cfg(test)]
fn menu_items(hotkey_label: Option<&str>) -> Vec<TrayMenuItem> {
    vec![
        TrayMenuItem {
            label: capture_menu_label(hotkey_label),
            action: Some(TrayAction::Capture),
        },
        TrayMenuItem {
            label: "History…".to_string(),
            action: Some(TrayAction::History),
        },
        TrayMenuItem {
            label: "Settings…".to_string(),
            action: Some(TrayAction::Settings),
        },
        TrayMenuItem {
            label: "Check for Updates…".to_string(),
            action: Some(TrayAction::CheckForUpdates),
        },
        TrayMenuItem {
            label: String::new(),
            action: None,
        },
        TrayMenuItem {
            label: "Quit".to_string(),
            action: Some(TrayAction::Quit),
        },
    ]
}

fn capture_menu_label(hotkey_label: Option<&str>) -> String {
    match hotkey_label {
        Some(k) if !k.is_empty() => format!("Capture  ({k})"),
        _ => "Capture".to_string(),
    }
}

/// Holds the live `TrayIcon` plus a map from menu-item id to the
/// action that menu item represents. Drop the controller and the
/// system tray entry disappears.
pub struct TrayController {
    _tray: TrayIcon,
    menu_ids: HashMap<MenuId, TrayAction>,
}

impl TrayController {
    /// Pop any pending tray + menu events off the static crossbeam
    /// receivers. Cheap — a non-blocking `try_recv` loop. Caller
    /// passes the result list straight into `runtime::update`.
    pub fn drain(&self) -> Vec<TrayAction> {
        let mut out = Vec::new();
        // Menu clicks are the primary intent surface. We map every
        // delivered MenuEvent through our id table.
        while let Ok(event) = MenuEvent::receiver().try_recv() {
            if let Some(action) = self.menu_ids.get(&event.id).copied() {
                out.push(action);
            }
        }
        // A left-click on the tray icon is the most common gesture
        // for menu-bar capture tools (Cleanshot, Shottr, Flameshot
        // on Linux), so we treat it as a Capture trigger. Right
        // clicks open the menu, which fires its own MenuEvents above.
        while let Ok(event) = TrayIconEvent::receiver().try_recv() {
            if let TrayIconEvent::Click {
                button: tray_icon::MouseButton::Left,
                button_state: tray_icon::MouseButtonState::Up,
                ..
            } = event
            {
                out.push(TrayAction::Capture);
            }
        }
        out
    }
}

/// Construct the tray icon + menu. Returns `None` if the OS rejects
/// the request (most common reason on Linux: no system tray /
/// libayatana-appindicator missing).
///
/// `hotkey_label` is the already-pretty-printed shortcut (e.g.
/// `"⌘⇧X"`). When provided, it's appended to the Capture menu item
/// label so the global hotkey is discoverable at a glance.
pub fn install(hotkey_label: Option<&str>) -> Option<TrayController> {
    let icon = match build_icon(32) {
        Ok(i) => i,
        Err(e) => {
            tracing::warn!(target: "readshot::tray", "icon build failed: {e}");
            return None;
        }
    };

    let menu = Menu::new();
    let capture_label = capture_menu_label(hotkey_label);
    let item_capture = MenuItem::new(capture_label, true, None);
    let item_history = MenuItem::new("History…", true, None);
    let item_settings = MenuItem::new("Settings…", true, None);
    let item_check_updates = MenuItem::new("Check for Updates…", true, None);
    let item_quit = MenuItem::new("Quit", true, None);

    let mut menu_ids = HashMap::new();
    menu_ids.insert(item_capture.id().clone(), TrayAction::Capture);
    menu_ids.insert(item_history.id().clone(), TrayAction::History);
    menu_ids.insert(item_settings.id().clone(), TrayAction::Settings);
    menu_ids.insert(item_check_updates.id().clone(), TrayAction::CheckForUpdates);
    menu_ids.insert(item_quit.id().clone(), TrayAction::Quit);

    if let Err(e) = menu.append_items(&[
        &item_capture,
        &item_history,
        &item_settings,
        &item_check_updates,
        &PredefinedMenuItem::separator(),
        &item_quit,
    ]) {
        tracing::warn!(target: "readshot::tray", "menu build failed: {e}");
        return None;
    }

    let tray = match TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("Readshot — click to capture, right-click for menu")
        .with_icon(icon)
        .with_icon_as_template(true) // macOS renders the icon monochrome / dark-mode aware.
        .build()
    {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(target: "readshot::tray", "TrayIconBuilder failed: {e}");
            return None;
        }
    };

    tracing::info!(target: "readshot::tray", "tray icon installed");
    Some(TrayController {
        _tray: tray,
        menu_ids,
    })
}

/// Generate a small RGBA icon for the menu-bar tray.
///
/// Drawn as a macOS *template image*: transparent background, solid
/// opaque black glyph. macOS sees `with_icon_as_template(true)` and
/// tints the alpha into whatever colour the menu bar wants (white
/// in dark mode, near-black in light mode), with a nice highlight
/// when the menu is open. This matches the app-icon's brand language:
/// four selection-corner brackets framing a centered lens dot.
fn build_icon(size: u32) -> Result<Icon, IconError> {
    let mut pixmap = Pixmap::new(size, size).ok_or(IconError::Pixmap)?;
    let s = size as f32;

    // Foreground paint — fully opaque black. macOS template tinting
    // uses the alpha channel only, so the colour itself is moot, but
    // black + full alpha gives us a sane fallback on platforms that
    // do *not* honour the template flag (Linux, X11 fallback paths).
    let mut fg_paint = Paint::default();
    fg_paint.set_color_rgba8(0x00, 0x00, 0x00, 0xff);
    fg_paint.anti_alias = true;

    // Selection-bracket geometry. Brackets sit `inset` from each edge;
    // each arm is `arm` long and the stroke is `stroke_w`. Numbers are
    // ratios of `size` so this scales linearly from 16 to 64+ without
    // recomputing pixel offsets.
    let inset = s * 0.18;
    let arm = s * 0.28;
    let stroke_w = (s * 0.13).max(2.0);
    let stroke = Stroke {
        width: stroke_w,
        line_cap: tiny_skia::LineCap::Round,
        line_join: tiny_skia::LineJoin::Round,
        ..Stroke::default()
    };

    let bracket_path = {
        let mut pb = PathBuilder::new();
        let left = inset;
        let right = s - inset;
        let top = inset;
        let bottom = s - inset;
        // Each bracket is two strokes that share a corner: a small
        // L-shape. We reuse the same path object to keep things tight.
        // top-left
        pb.move_to(left, top + arm);
        pb.line_to(left, top);
        pb.line_to(left + arm, top);
        // top-right
        pb.move_to(right - arm, top);
        pb.line_to(right, top);
        pb.line_to(right, top + arm);
        // bottom-left
        pb.move_to(left, bottom - arm);
        pb.line_to(left, bottom);
        pb.line_to(left + arm, bottom);
        // bottom-right
        pb.move_to(right - arm, bottom);
        pb.line_to(right, bottom);
        pb.line_to(right, bottom - arm);
        pb.finish().ok_or(IconError::Path)?
    };
    pixmap.stroke_path(
        &bracket_path,
        &fg_paint,
        &stroke,
        Transform::identity(),
        None,
    );

    // Center lens dot. Filled circle at ~14% of the icon size so it
    // reads on a 22pt menu bar without colliding with the brackets.
    let dot_radius = (s * 0.13).max(1.5);
    let dot_path = {
        let mut pb = PathBuilder::new();
        pb.push_circle(s * 0.5, s * 0.5, dot_radius);
        pb.finish().ok_or(IconError::Path)?
    };
    pixmap.fill_path(
        &dot_path,
        &fg_paint,
        FillRule::Winding,
        Transform::identity(),
        None,
    );

    // Convert tiny-skia's premultiplied RGBA to the straight RGBA
    // `tray_icon` expects. Anti-aliased edges produce partial alphas
    // so this conversion is no longer a no-op.
    let raw = pixmap.take();
    let rgba = unpremultiply(raw);
    Icon::from_rgba(rgba, size, size).map_err(IconError::Icon)
}

fn unpremultiply(mut buf: Vec<u8>) -> Vec<u8> {
    for px in buf.chunks_exact_mut(4) {
        let a = px[3] as u32;
        if a == 0 || a == 255 {
            continue;
        }
        for c in &mut px[..3] {
            *c = ((*c as u32 * 255 + a / 2) / a).min(255) as u8;
        }
    }
    buf
}

#[derive(Debug, thiserror::Error)]
enum IconError {
    #[error("could not allocate pixmap")]
    Pixmap,
    #[error("path geometry was empty")]
    Path,
    #[error(transparent)]
    Icon(#[from] tray_icon::BadIcon),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_contains_check_for_updates_before_quit() {
        let items = menu_items(None);
        let actions: Vec<TrayAction> = items.iter().filter_map(|item| item.action).collect();

        assert_eq!(
            actions,
            vec![
                TrayAction::Capture,
                TrayAction::History,
                TrayAction::Settings,
                TrayAction::CheckForUpdates,
                TrayAction::Quit,
            ]
        );
    }

    #[test]
    fn icon_builder_produces_a_valid_buffer() {
        // Doesn't actually create a system tray; just exercises the
        // pixel buffer + Icon::from_rgba path.
        let icon = build_icon(32);
        assert!(icon.is_ok(), "build_icon failed: {icon:?}");
    }

    #[test]
    fn icon_builder_supports_multiple_sizes() {
        for size in [16u32, 22, 32, 64] {
            assert!(build_icon(size).is_ok(), "size {size} failed");
        }
    }

    #[test]
    fn unpremultiply_passes_through_opaque_pixels() {
        let input = vec![100, 150, 200, 255, 50, 60, 70, 255];
        let out = unpremultiply(input.clone());
        assert_eq!(out, input);
    }

    #[test]
    fn unpremultiply_recovers_straight_rgb_for_partial_alpha() {
        // 50% premultiplied red over transparent → straight red.
        let input = vec![128, 0, 0, 128];
        let out = unpremultiply(input);
        assert!(out[0] >= 250); // close to 255 after recovery
        assert_eq!(out[3], 128);
    }
}

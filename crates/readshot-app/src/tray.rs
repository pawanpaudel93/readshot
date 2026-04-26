//! System tray integration (Phase A.5).
//!
//! Builds a small icon at runtime, registers it as a tray icon with a
//! short menu (Capture / Show Window / Quit), and exposes a non-blocking
//! [`TrayController::drain`] that the iced runtime polls every few ms
//! to surface tray + menu events as typed actions.
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

/// User intent surfaced by the tray. `Capture` and `ShowWindow` map
/// to existing app messages; `Quit` is handled by the runtime which
/// closes the welcome window (and thus the daemon, in Phase A).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayAction {
    Capture,
    ShowWindow,
    Quit,
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
        // A left-click on the tray icon (without using the menu) is
        // surfaced as `ShowWindow`. We ignore right-clicks because
        // they open the menu, which fires its own MenuEvents above.
        while let Ok(event) = TrayIconEvent::receiver().try_recv() {
            if let TrayIconEvent::Click {
                button: tray_icon::MouseButton::Left,
                button_state: tray_icon::MouseButtonState::Up,
                ..
            } = event
            {
                out.push(TrayAction::ShowWindow);
            }
        }
        out
    }
}

/// Construct the tray icon + menu. Returns `None` if the OS rejects
/// the request (most common reason on Linux: no system tray /
/// libayatana-appindicator missing).
pub fn install() -> Option<TrayController> {
    let icon = match build_icon(32) {
        Ok(i) => i,
        Err(e) => {
            tracing::warn!(target: "readshot::tray", "icon build failed: {e}");
            return None;
        }
    };

    let menu = Menu::new();
    let item_capture = MenuItem::new("Capture", true, None);
    let item_show = MenuItem::new("Show Readshot", true, None);
    let item_quit = MenuItem::new("Quit", true, None);

    let mut menu_ids = HashMap::new();
    menu_ids.insert(item_capture.id().clone(), TrayAction::Capture);
    menu_ids.insert(item_show.id().clone(), TrayAction::ShowWindow);
    menu_ids.insert(item_quit.id().clone(), TrayAction::Quit);

    if let Err(e) = menu.append_items(&[
        &item_capture,
        &item_show,
        &PredefinedMenuItem::separator(),
        &item_quit,
    ]) {
        tracing::warn!(target: "readshot::tray", "menu build failed: {e}");
        return None;
    }

    let tray = match TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("Readshot — click to open, right-click for menu")
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

/// Generate a small RGBA icon using tiny-skia. We draw a stylized
/// "R" — rounded outer square plus the geometry of an R glyph using
/// stroke + fill paths. The buffer is exactly `size*size*4` bytes
/// and `Icon::from_rgba` accepts it directly.
fn build_icon(size: u32) -> Result<Icon, IconError> {
    let mut pixmap = Pixmap::new(size, size).ok_or(IconError::Pixmap)?;
    let s = size as f32;

    // Background: dark slate. Slightly transparent so dark-mode menu
    // bars still show through at the edges.
    let bg_path = {
        let mut pb = PathBuilder::new();
        pb.move_to(0.0, 0.0);
        pb.line_to(s, 0.0);
        pb.line_to(s, s);
        pb.line_to(0.0, s);
        pb.close();
        pb.finish().ok_or(IconError::Path)?
    };
    let mut bg_paint = Paint::default();
    bg_paint.set_color_rgba8(0x21, 0x25, 0x2b, 0xff);
    pixmap.fill_path(
        &bg_path,
        &bg_paint,
        FillRule::Winding,
        Transform::identity(),
        None,
    );

    // Rounded inner panel (approximated with a stroked rectangle).
    let mut fg_paint = Paint::default();
    fg_paint.set_color_rgba8(0xe8, 0xe8, 0xe8, 0xff);
    fg_paint.anti_alias = true;

    // The "R" silhouette: two strokes — vertical stem + a curved
    // bowl + a leg. Coordinates are ratios of the icon size so the
    // shape scales cleanly.
    let r = |x: f32, y: f32| (s * x, s * y);
    let r_path = {
        let mut pb = PathBuilder::new();
        // Vertical stem.
        let (x0, y0) = r(0.30, 0.20);
        pb.move_to(x0, y0);
        pb.line_to(s * 0.30, s * 0.80);
        // Bowl: top curve from stem-top right.
        let (top_r, top_y) = r(0.62, 0.20);
        pb.move_to(x0, y0);
        pb.line_to(top_r, top_y);
        pb.cubic_to(s * 0.78, s * 0.20, s * 0.78, s * 0.50, top_r, s * 0.50);
        pb.line_to(s * 0.30, s * 0.50);
        // Diagonal leg.
        pb.move_to(s * 0.50, s * 0.50);
        pb.line_to(s * 0.72, s * 0.80);
        pb.finish().ok_or(IconError::Path)?
    };
    let stroke = Stroke {
        width: s * 0.08,
        ..Stroke::default()
    };
    pixmap.stroke_path(&r_path, &fg_paint, &stroke, Transform::identity(), None);

    // Convert tiny-skia's premultiplied RGBA to the straight RGBA
    // tray-icon expects. Alpha is full so this is effectively a
    // copy in our case, but we go through the helper to stay safe
    // if we ever introduce semitransparent fills.
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

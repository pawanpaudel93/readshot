//! System tray integration.
//!
//! Builds the tray icon (a runtime-drawn template mark on macOS, the
//! colour brand mark elsewhere), registers it as a tray icon with the
//! app's menu (Capture, Scrolling Capture, Retake Last Region, History,
//! Settings, Check for Updates and About Readshot on macOS, Quit), and
//! exposes a non-blocking [`TrayController::drain`] that the iced
//! runtime polls every few ms to surface tray + menu events as typed
//! actions. A left-click on
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

use muda::accelerator::{Accelerator, Code, Modifiers};
use muda::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
#[cfg(any(target_os = "macos", test))]
use tiny_skia::{FillRule, Paint, PathBuilder, Pixmap, Stroke, Transform};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder, TrayIconEvent};

/// User intent surfaced by the tray. `Capture` triggers the region
/// overlay; `RetakeLastRegion` repeats the last confirmed overlay
/// rectangle; `History` opens the persistent capture browser;
/// `Settings` opens the preferences window; `CheckForUpdates` opens
/// Sparkle's standard updater UI on macOS; `About` opens the standard
/// About panel (icon, name, version) on macOS; `Quit` exits the daemon.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayAction {
    Capture,
    /// Open the overlay pre-set to start a scrolling-capture session
    /// when the user confirms a region. Lets the user reach the
    /// feature without first picking a region and then hunting for
    /// the ↕ Scroll button in the quick-action toolbar.
    ScrollCapture,
    RetakeLastRegion,
    History,
    Settings,
    CheckForUpdates,
    About,
    /// "Notifications Are Off…": open System Settings › Notifications.
    /// Only in the menu once a notification was refused.
    NotificationSettings,
    Quit,
}

#[cfg(test)]
#[derive(Clone, Debug, PartialEq, Eq)]
struct TrayMenuItem {
    label: String,
    enabled: bool,
    action: Option<TrayAction>,
}

#[cfg(test)]
fn menu_items(_hotkey_label: Option<&str>, can_retake_last_region: bool) -> Vec<TrayMenuItem> {
    let mut items = vec![
        TrayMenuItem {
            label: capture_menu_label().to_string(),
            enabled: true,
            action: Some(TrayAction::Capture),
        },
        TrayMenuItem {
            label: "Scrolling Capture…".to_string(),
            enabled: true,
            action: Some(TrayAction::ScrollCapture),
        },
        TrayMenuItem {
            label: "Retake Last Region".to_string(),
            enabled: can_retake_last_region,
            action: Some(TrayAction::RetakeLastRegion),
        },
        TrayMenuItem {
            label: "History…".to_string(),
            enabled: true,
            action: Some(TrayAction::History),
        },
        TrayMenuItem {
            label: "Settings…".to_string(),
            enabled: true,
            action: Some(TrayAction::Settings),
        },
    ];

    #[cfg(target_os = "macos")]
    items.extend([
        TrayMenuItem {
            label: "Check for Updates…".to_string(),
            enabled: true,
            action: Some(TrayAction::CheckForUpdates),
        },
        TrayMenuItem {
            label: "About Readshot".to_string(),
            enabled: true,
            action: Some(TrayAction::About),
        },
    ]);

    items.extend([
        TrayMenuItem {
            label: String::new(),
            enabled: true,
            action: None,
        },
        TrayMenuItem {
            label: "Quit Readshot".to_string(),
            enabled: true,
            action: Some(TrayAction::Quit),
        },
    ]);

    items
}

fn capture_menu_label() -> &'static str {
    // macOS convention: a trailing ellipsis marks a command that needs
    // more input before it completes. Both Capture and Scrolling Capture
    // open the interactive overlay for the user to drag out a region, so
    // both take "…". Retake Last Region acts immediately and does not.
    "Capture…"
}

fn capture_menu_accelerator(raw_hotkey: Option<&str>) -> Option<Accelerator> {
    let raw_hotkey = raw_hotkey?.trim();
    if raw_hotkey.is_empty() {
        return None;
    }
    let normalized = normalize_hotkey_for_menu(raw_hotkey)?;
    match normalized.parse::<Accelerator>() {
        Ok(accelerator) => Some(accelerator),
        Err(e) => {
            tracing::debug!(
                target: readshot_core::log::cat::TRAY,
                "could not parse tray accelerator `{raw_hotkey}`: {e}",
            );
            None
        }
    }
}

fn normalize_hotkey_for_menu(raw_hotkey: &str) -> Option<String> {
    let tokens: Vec<String> = raw_hotkey
        .split(|c: char| matches!(c, '+' | '-') || c.is_whitespace())
        .filter(|token| !token.is_empty())
        .map(|token| match token.to_ascii_lowercase().as_str() {
            "cmd" | "command" | "meta" | "super" | "win" => "command".to_string(),
            "ctrl" | "control" => "control".to_string(),
            "alt" | "opt" | "option" => "alt".to_string(),
            "shift" => "shift".to_string(),
            "return" => "enter".to_string(),
            "esc" => "escape".to_string(),
            "del" => "delete".to_string(),
            "pgup" => "pageup".to_string(),
            "pgdn" => "pagedown".to_string(),
            other => other.to_string(),
        })
        .collect();
    (!tokens.is_empty()).then(|| tokens.join("+"))
}

/// Holds the live `TrayIcon` plus a map from menu-item id to the
/// action that menu item represents. Drop the controller and the
/// system tray entry disappears.
pub struct TrayController {
    tray: TrayIcon,
    menu: Menu,
    item_capture: MenuItem,
    item_retake_last_region: MenuItem,
    item_notifications_off: MenuItem,
    notifications_off_shown: std::cell::Cell<bool>,
    menu_ids: HashMap<MenuId, TrayAction>,
}

/// The tray tooltip. On macOS and Windows a left click captures and a
/// right click opens the menu (`with_menu_on_left_click(false)`); Linux
/// app indicators only ever open the menu, so it says that instead.
fn tray_tooltip(hotkey_label: Option<&str>) -> String {
    let gesture = if cfg!(target_os = "linux") {
        "open the menu to capture"
    } else {
        "click to capture · right-click for menu"
    };
    match hotkey_label {
        Some(k) if !k.is_empty() => format!("Readshot — {gesture} · {k}"),
        _ => format!("Readshot — {gesture}"),
    }
}

impl TrayController {
    pub fn set_capture_hotkey(&self, raw_hotkey: Option<&str>, hotkey_label: Option<&str>) {
        self.item_capture.set_text(capture_menu_label());
        if let Err(e) = self
            .item_capture
            .set_accelerator(capture_menu_accelerator(raw_hotkey))
        {
            tracing::debug!(target: readshot_core::log::cat::TRAY, "set capture accelerator failed: {e}");
        }
        if let Err(e) = self.tray.set_tooltip(Some(tray_tooltip(hotkey_label))) {
            tracing::debug!(target: readshot_core::log::cat::TRAY, "set_tooltip failed: {e}");
        }
    }

    pub fn set_retake_last_region_enabled(&self, enabled: bool) {
        self.item_retake_last_region.set_enabled(enabled);
    }

    /// Tell the user, once, that Readshot's notifications are off: an item
    /// above Quit that opens System Settings › Notifications. Shown the
    /// next time they open the menu; later calls do nothing.
    pub fn show_notifications_off(&self) {
        if self.notifications_off_shown.replace(true) {
            return;
        }
        // Above the separator and Quit, the last two items.
        let position = self.menu.items().len().saturating_sub(2);
        if let Err(e) = self.menu.insert(&self.item_notifications_off, position) {
            tracing::warn!(target: readshot_core::log::cat::TRAY, "menu insert failed: {e}");
        }
    }

    /// Pop any pending tray + menu events off the static crossbeam
    /// receivers. Cheap — a non-blocking `try_recv` loop. Caller
    /// passes the result list straight into `runtime::update`.
    pub fn drain(&self) -> Vec<TrayAction> {
        let mut out = Vec::new();
        let mut saw_menu_action = false;
        // Menu clicks are the primary intent surface. We map every
        // delivered MenuEvent through our id table.
        while let Ok(event) = MenuEvent::receiver().try_recv() {
            if let Some(action) = self.menu_ids.get(&event.id).copied() {
                saw_menu_action = true;
                out.push(action);
            }
        }
        // A left-click on the tray icon is the most common gesture
        // for menu-bar capture tools (Cleanshot, Shottr, Flameshot
        // on Linux), so we treat it as a Capture trigger. Right
        // clicks open the menu, which fires its own MenuEvents above.
        while let Ok(event) = TrayIconEvent::receiver().try_recv() {
            if let Some(action) = tray_icon_event_action(&event, saw_menu_action) {
                out.push(action);
            }
        }
        out
    }
}

fn tray_icon_event_action(
    event: &TrayIconEvent,
    suppress_icon_capture: bool,
) -> Option<TrayAction> {
    if suppress_icon_capture {
        return None;
    }
    match event {
        TrayIconEvent::Click {
            button: tray_icon::MouseButton::Left,
            button_state: tray_icon::MouseButtonState::Up,
            ..
        } => Some(TrayAction::Capture),
        _ => None,
    }
}

/// Construct the tray icon + menu. Returns `None` if the OS rejects
/// the request (most common reason on Linux: no system tray /
/// libayatana-appindicator missing).
///
/// `raw_hotkey` is the configured shortcut string (for the native menu
/// accelerator). `hotkey_label` is the already-pretty-printed shortcut
/// (for the tray tooltip).
pub fn install(raw_hotkey: Option<&str>, hotkey_label: Option<&str>) -> Option<TrayController> {
    let icon = match tray_icon_image() {
        Ok(i) => i,
        Err(e) => {
            tracing::warn!(target: readshot_core::log::cat::TRAY, "icon build failed: {e}");
            return None;
        }
    };

    let menu = Menu::new();
    let item_capture = MenuItem::new(
        capture_menu_label(),
        true,
        capture_menu_accelerator(raw_hotkey),
    );
    let item_scroll_capture = MenuItem::new("Scrolling Capture…", true, None);
    let item_retake_last_region = MenuItem::new("Retake Last Region", false, None);
    // ⌘Y / ⌘, only render as hint text in the menu — muda doesn't
    // dispatch them globally because the tray menu isn't the focused
    // surface. The actual keyboard handling lives in the main window
    // via iced; these accelerators exist only to display the shortcut
    // alongside the menu entry, matching native macOS apps.
    #[cfg(target_os = "macos")]
    let cmd_mod = Modifiers::META;
    #[cfg(not(target_os = "macos"))]
    let cmd_mod = Modifiers::CONTROL;
    let item_history = MenuItem::new(
        "History…",
        true,
        Some(Accelerator::new(Some(cmd_mod), Code::KeyY)),
    );
    let item_settings = MenuItem::new(
        "Settings…",
        true,
        Some(Accelerator::new(Some(cmd_mod), Code::Comma)),
    );
    #[cfg(target_os = "macos")]
    let item_check_updates = MenuItem::new("Check for Updates…", true, None);
    #[cfg(target_os = "macos")]
    let item_about = MenuItem::new("About Readshot", true, None);
    let item_quit = MenuItem::new("Quit Readshot", true, None);
    // Not in the menu until a notification is refused
    // (`TrayController::show_notifications_off`).
    let item_notifications_off = MenuItem::new(NOTIFICATIONS_OFF_LABEL, true, None);

    let mut menu_ids = HashMap::new();
    menu_ids.insert(item_capture.id().clone(), TrayAction::Capture);
    menu_ids.insert(item_scroll_capture.id().clone(), TrayAction::ScrollCapture);
    menu_ids.insert(
        item_retake_last_region.id().clone(),
        TrayAction::RetakeLastRegion,
    );
    menu_ids.insert(item_history.id().clone(), TrayAction::History);
    menu_ids.insert(item_settings.id().clone(), TrayAction::Settings);
    #[cfg(target_os = "macos")]
    menu_ids.insert(item_check_updates.id().clone(), TrayAction::CheckForUpdates);
    #[cfg(target_os = "macos")]
    menu_ids.insert(item_about.id().clone(), TrayAction::About);
    menu_ids.insert(item_quit.id().clone(), TrayAction::Quit);
    menu_ids.insert(
        item_notifications_off.id().clone(),
        TrayAction::NotificationSettings,
    );

    if let Err(e) = menu.append_items(&[
        &item_capture,
        &item_scroll_capture,
        &item_retake_last_region,
        &item_history,
        &item_settings,
    ]) {
        tracing::warn!(target: readshot_core::log::cat::TRAY, "menu build failed: {e}");
        return None;
    }
    #[cfg(target_os = "macos")]
    if let Err(e) = menu.append_items(&[&item_check_updates, &item_about]) {
        tracing::warn!(target: readshot_core::log::cat::TRAY, "menu build failed: {e}");
        return None;
    }
    if let Err(e) = menu.append_items(&[&PredefinedMenuItem::separator(), &item_quit]) {
        tracing::warn!(target: readshot_core::log::cat::TRAY, "menu build failed: {e}");
        return None;
    }

    let menu_handle = menu.clone();
    let tray = match TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        // A left click captures (`tray_click_action`); only a right click
        // opens the menu, so one click never does both.
        .with_menu_on_left_click(false)
        .with_tooltip(tray_tooltip(hotkey_label))
        .with_icon(icon)
        // macOS renders the template icon monochrome and dark-mode aware;
        // other platforms ignore the flag (they get the colour mark).
        .with_icon_as_template(true)
        .build()
    {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(target: readshot_core::log::cat::TRAY, "TrayIconBuilder failed: {e}");
            return None;
        }
    };

    tracing::info!(target: readshot_core::log::cat::TRAY, "tray icon installed");
    Some(TrayController {
        tray,
        menu: menu_handle,
        item_capture,
        item_retake_last_region,
        item_notifications_off,
        notifications_off_shown: std::cell::Cell::new(false),
        menu_ids,
    })
}

/// The tray item shown once notifications turn out to be off.
pub(crate) const NOTIFICATIONS_OFF_LABEL: &str = "Notifications Are Off — Turn On…";

/// tray-icon draws the macOS status item 18 pt tall: a 36 px bitmap is
/// pixel-exact on Retina (a 32 px one was upscaled and soft).
#[cfg(any(target_os = "macos", test))]
const MENU_BAR_ICON_PX: u32 = 36;

/// The off-macOS tray icon: the full-colour brand mark rendered from
/// `packaging/macos/icon-small.svg` by `packaging/render-icons.sh`.
#[cfg_attr(all(target_os = "macos", not(test)), allow(dead_code))]
const TRAY_COLOR_PNG: &[u8] = include_bytes!("../assets/tray-color-32.png");

/// The tray icon for this platform. macOS gets the monochrome template
/// mark, which the menu bar tints for light and dark. Windows and Linux
/// do not honour the template flag, so the black template glyph vanished
/// on their dark taskbars and panels; they get the full-colour brand mark
/// instead (a teal tile with white brackets and lens), which reads on
/// both light and dark.
fn tray_icon_image() -> Result<Icon, IconError> {
    #[cfg(target_os = "macos")]
    {
        build_icon(MENU_BAR_ICON_PX)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let (rgba, w, h) = brand_tray_rgba()?;
        Icon::from_rgba(rgba, w, h).map_err(IconError::Icon)
    }
}

/// [`TRAY_COLOR_PNG`] as straight RGBA, with its size.
#[cfg_attr(all(target_os = "macos", not(test)), allow(dead_code))]
fn brand_tray_rgba() -> Result<(Vec<u8>, u32, u32), IconError> {
    let rgba =
        image::load_from_memory_with_format(TRAY_COLOR_PNG, image::ImageFormat::Png)?.into_rgba8();
    let (w, h) = rgba.dimensions();
    Ok((rgba.into_raw(), w, h))
}

/// Generate a small RGBA icon for the menu-bar tray.
///
/// Drawn as a macOS *template image*: transparent background, solid
/// opaque black glyph. macOS sees `with_icon_as_template(true)` and
/// tints the alpha into whatever colour the menu bar wants (white
/// in dark mode, near-black in light mode), with a nice highlight
/// when the menu is open. This matches the app-icon's brand language:
/// four airy selection-corner brackets and a centered lens ring.
#[cfg(any(target_os = "macos", test))]
fn build_icon(size: u32) -> Result<Icon, IconError> {
    let rgba = build_icon_rgba(size)?;
    Icon::from_rgba(rgba, size, size).map_err(IconError::Icon)
}

#[cfg(any(target_os = "macos", test))]
fn build_icon_rgba(size: u32) -> Result<Vec<u8>, IconError> {
    let mut pixmap = Pixmap::new(size, size).ok_or(IconError::Pixmap)?;
    let s = size as f32;

    // Foreground paint — fully opaque black. macOS template tinting
    // uses the alpha channel only, so the colour itself is moot, but
    // black + full alpha gives us a sane fallback on platforms that
    // do *not* honour the template flag (Linux, X11 fallback paths).
    let mut fg_paint = Paint::default();
    fg_paint.set_color_rgba8(0x00, 0x00, 0x00, 0xff);
    fg_paint.anti_alias = true;

    // Selection brackets use the same relative placement as the full
    // colour icon, but drop the app tile. The menu bar gives us only
    // ~18 px, so negative space matters more than literal fidelity.
    let left = s * 0.16;
    let right = s * 0.84;
    let top = s * 0.16;
    let bottom = s * 0.84;
    let arm = s * 0.23;
    let bracket_stroke = Stroke {
        width: (s * 0.085).max(1.6),
        line_cap: tiny_skia::LineCap::Round,
        line_join: tiny_skia::LineJoin::Round,
        ..Stroke::default()
    };

    let bracket_path = {
        let mut pb = PathBuilder::new();
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
        &bracket_stroke,
        Transform::identity(),
        None,
    );

    // Center OCR lens: solid ring with a transparent center, matching
    // the app icon's glass lens while staying single-colour.
    let lens_path = {
        let mut pb = PathBuilder::new();
        pb.push_circle(s * 0.5, s * 0.5, (s * 0.205).max(2.7));
        pb.push_circle(s * 0.5, s * 0.5, (s * 0.125).max(1.6));
        pb.finish().ok_or(IconError::Path)?
    };
    pixmap.fill_path(
        &lens_path,
        &fg_paint,
        FillRule::EvenOdd,
        Transform::identity(),
        None,
    );

    // Convert tiny-skia's premultiplied RGBA to the straight RGBA
    // `tray_icon` expects. Anti-aliased edges produce partial alphas
    // so this conversion is no longer a no-op.
    let raw = pixmap.take();
    Ok(unpremultiply(raw))
}

#[cfg(any(target_os = "macos", test))]
fn unpremultiply(mut buf: Vec<u8>) -> Vec<u8> {
    for px in buf.as_chunks_mut::<4>().0 {
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
    #[cfg(any(target_os = "macos", test))]
    #[error("could not allocate pixmap")]
    Pixmap,
    #[cfg(any(target_os = "macos", test))]
    #[error("path geometry was empty")]
    Path,
    #[error(transparent)]
    Icon(#[from] tray_icon::BadIcon),
    #[error("could not decode the tray icon: {0}")]
    Decode(#[from] image::ImageError),
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_menu_names_the_app_and_the_tooltip_matches_the_click() {
        let items = super::menu_items(Some("⇧⌘X"), true);
        assert!(items.iter().any(|i| i.label == "Quit Readshot"));
        let tip = super::tray_tooltip(Some("⇧⌘X"));
        assert!(tip.ends_with("· ⇧⌘X"), "{tip}");
        if cfg!(target_os = "linux") {
            assert!(tip.contains("open the menu to capture"), "{tip}");
        } else {
            assert!(
                tip.contains("click to capture · right-click for menu"),
                "{tip}"
            );
        }
        assert!(!super::tray_tooltip(None).ends_with(' '));
    }

    use super::*;

    #[test]
    fn menu_actions_match_supported_platform_features() {
        let items = menu_items(None, false);
        let actions: Vec<TrayAction> = items.iter().filter_map(|item| item.action).collect();

        let mut expected = vec![
            TrayAction::Capture,
            TrayAction::ScrollCapture,
            TrayAction::RetakeLastRegion,
            TrayAction::History,
            TrayAction::Settings,
        ];
        #[cfg(target_os = "macos")]
        expected.extend([TrayAction::CheckForUpdates, TrayAction::About]);
        expected.push(TrayAction::Quit);

        assert_eq!(actions, expected);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn menu_contains_check_for_updates_before_quit() {
        let items = menu_items(None, false);
        let actions: Vec<TrayAction> = items.iter().filter_map(|item| item.action).collect();

        assert_eq!(
            actions,
            vec![
                TrayAction::Capture,
                TrayAction::ScrollCapture,
                TrayAction::RetakeLastRegion,
                TrayAction::History,
                TrayAction::Settings,
                TrayAction::CheckForUpdates,
                TrayAction::About,
                TrayAction::Quit,
            ]
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn menu_contains_check_for_updates_after_settings() {
        let items = menu_items(None, false);
        let labels: Vec<&str> = items.iter().map(|item| item.label.as_str()).collect();

        assert!(labels
            .windows(2)
            .any(|pair| pair == ["Settings…", "Check for Updates…"]));
        assert!(!labels.contains(&"Install Command Line Tools…"));
    }

    #[test]
    fn menu_contains_retake_last_region_after_capture() {
        let items = menu_items(Some("⌘⇧X"), true);
        let scroll = items
            .iter()
            .position(|item| item.action == Some(TrayAction::ScrollCapture))
            .unwrap();
        let retake = items
            .iter()
            .position(|item| item.action == Some(TrayAction::RetakeLastRegion))
            .unwrap();

        assert_eq!(items[retake].label, "Retake Last Region");
        assert!(items[retake].enabled);
        // Retake follows the Scrolling Capture entry, which itself
        // follows the primary Capture entry.
        assert_eq!(retake, scroll + 1);
    }

    #[test]
    fn capture_menu_label_uses_ellipsis_for_interactive_flow() {
        // The overlay asks for a region before the capture completes, so
        // the menu entry follows the macOS "… means more input" rule.
        assert_eq!(capture_menu_label(), "Capture…");
    }

    #[test]
    fn capture_menu_accelerator_parses_default_hotkey() {
        let accelerator = capture_menu_accelerator(Some("cmd+shift+x")).unwrap();
        let modifiers = Modifiers::SUPER | Modifiers::SHIFT;

        assert!(accelerator.matches(modifiers, Code::KeyX));
    }

    #[test]
    fn capture_menu_accelerator_accepts_preference_synonyms() {
        let accelerator = capture_menu_accelerator(Some("ctrl opt return")).unwrap();
        let modifiers = Modifiers::CONTROL | Modifiers::ALT;

        assert!(accelerator.matches(modifiers, Code::Enter));
    }

    #[test]
    fn capture_menu_accelerator_rejects_invalid_hotkey() {
        assert!(capture_menu_accelerator(Some("not a hotkey")).is_none());
        assert!(capture_menu_accelerator(Some("")).is_none());
        assert!(capture_menu_accelerator(None).is_none());
    }

    #[test]
    fn menu_disables_retake_last_region_until_region_exists() {
        let items = menu_items(Some("⌘⇧X"), false);
        let retake = items
            .iter()
            .find(|item| item.action == Some(TrayAction::RetakeLastRegion))
            .unwrap();

        assert_eq!(retake.label, "Retake Last Region");
        assert!(!retake.enabled);
    }

    #[test]
    fn tray_icon_left_click_maps_to_capture() {
        let event = TrayIconEvent::Click {
            id: tray_icon::TrayIconId::new("readshot"),
            position: tray_icon::dpi::PhysicalPosition::default(),
            rect: tray_icon::Rect::default(),
            button: tray_icon::MouseButton::Left,
            button_state: tray_icon::MouseButtonState::Up,
        };

        assert_eq!(
            tray_icon_event_action(&event, false),
            Some(TrayAction::Capture)
        );
    }

    #[test]
    fn tray_icon_click_is_ignored_when_menu_action_was_drained() {
        let event = TrayIconEvent::Click {
            id: tray_icon::TrayIconId::new("readshot"),
            position: tray_icon::dpi::PhysicalPosition::default(),
            rect: tray_icon::Rect::default(),
            button: tray_icon::MouseButton::Left,
            button_state: tray_icon::MouseButtonState::Up,
        };

        assert_eq!(tray_icon_event_action(&event, true), None);
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
    fn icon_builder_draws_airier_app_icon_mark() {
        let size = 32;
        let rgba = build_icon_rgba(size).unwrap();
        let alpha_at = |x: u32, y: u32| -> u8 {
            let i = ((y * size + x) * 4 + 3) as usize;
            rgba[i]
        };

        assert_eq!(
            alpha_at(0, 0),
            0,
            "outside the rounded tile stays transparent"
        );
        assert_eq!(
            alpha_at(size / 2, 3),
            0,
            "menu-bar mark should not draw the dense outer app tile"
        );
        assert!(
            alpha_at(5, 5) > 0,
            "top-left capture bracket should be visible"
        );
        assert!(
            alpha_at(size / 2, size / 2) == 0,
            "lens center should stay hollow"
        );
        assert!(
            alpha_at(size / 2 + 5, size / 2) > 0,
            "lens ring should be visible"
        );
    }

    #[test]
    fn the_macos_template_icon_is_pixel_exact_on_retina() {
        // 18 pt status item at 2x.
        assert_eq!(MENU_BAR_ICON_PX, 36);
        assert!(build_icon(MENU_BAR_ICON_PX).is_ok());
    }

    /// Share of the icon's pixels that stand out from a taskbar colour
    /// (composited luminance at least 0.25 away from it).
    fn visible_share(rgba: &[u8], bar: [f32; 3]) -> f32 {
        let lum = |c: [f32; 3]| 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
        let bar_l = lum(bar);
        let px = rgba.as_chunks::<4>().0;
        let visible = px
            .iter()
            .filter(|p| {
                let a = p[3] as f32 / 255.0;
                let c = [0, 1, 2].map(|i| p[i] as f32 / 255.0 * a + bar[i] * (1.0 - a));
                (lum(c) - bar_l).abs() >= 0.25
            })
            .count();
        visible as f32 / px.len() as f32
    }

    #[test]
    fn the_colour_tray_icon_reads_on_dark_and_light_taskbars() {
        let (rgba, w, h) = brand_tray_rgba().unwrap();
        assert_eq!((w, h), (32, 32));
        assert!(Icon::from_rgba(rgba.clone(), w, h).is_ok());
        // Windows 11 dark taskbar, GNOME's dark panel, a light taskbar.
        for bar in [[0.12, 0.12, 0.12], [0.0, 0.0, 0.0], [0.95, 0.95, 0.95]] {
            let share = visible_share(&rgba, bar);
            assert!(share > 0.3, "only {share} of the icon shows on {bar:?}");
        }
        // The black template glyph, by contrast, is what vanished on dark.
        let template = build_icon_rgba(32).unwrap();
        assert!(visible_share(&template, [0.12, 0.12, 0.12]) < 0.05);
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

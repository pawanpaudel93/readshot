// Extracted from runtime.rs (pure code-move). `use super::*` pulls in
// sibling/parent items; the explicit imports mirror runtime.rs's preamble.
use super::*;

use iced::window;
use iced::Task;

use crate::app::{App, Message, WindowKind};

pub(crate) fn mono_font() -> iced::Font {
    iced::Font::with_name(readshot_core::render::FONT_FAMILY)
}

/// The app id: the macOS bundle id, the Linux desktop entry's basename
/// (`np.com.pawanpaudel.readshot.desktop`) and `StartupWMClass`, and the
/// hicolor icon name. Every window carries it on Linux (Wayland `app_id`,
/// X11 `WM_CLASS`) so docks and taskbars group Readshot's windows under its
/// desktop entry and show its icon instead of a generic one.
// Read only by the Linux window settings (and the tests elsewhere).
#[cfg_attr(not(any(target_os = "linux", test)), allow(dead_code))]
pub(crate) const APP_ID: &str = "np.com.pawanpaudel.readshot";

/// The Readshot brand tile, 128 px: `packaging/macos/icon.svg` cropped to
/// its tile by `packaging/render-icons.sh`. The Welcome window shows it at
/// 64 pt (sharp at 2x), and it is every window's icon.
pub(crate) const BRAND_TILE_PNG: &[u8] = include_bytes!("../../assets/readshot-tile-128.png");

/// The Readshot icon for a window's title bar, taskbar button and Alt-Tab
/// entry (Windows, Linux). macOS has no per-window icons (the bundle icon
/// governs the Dock and app switcher), so winit ignores it there and
/// setting it is harmless. Decoded once.
pub(crate) fn app_window_icon() -> Option<window::Icon> {
    static ICON: std::sync::OnceLock<Option<window::Icon>> = std::sync::OnceLock::new();
    ICON.get_or_init(|| {
        let decoded = image::load_from_memory_with_format(BRAND_TILE_PNG, image::ImageFormat::Png)
            .map_err(|e| e.to_string())
            .and_then(|img| {
                let rgba = img.into_rgba8();
                let (w, h) = rgba.dimensions();
                window::icon::from_rgba(rgba.into_raw(), w, h).map_err(|e| e.to_string())
            });
        match decoded {
            Ok(icon) => Some(icon),
            Err(e) => {
                tracing::warn!(
                    target: readshot_core::log::cat::APP,
                    "could not decode the window icon: {e}"
                );
                None
            }
        }
    })
    .clone()
}

/// `settings` with what every Readshot window shares: the app icon and,
/// on Linux, the app id. Every `*_window_settings` constructor ends here.
pub(crate) fn with_app_identity(mut settings: window::Settings) -> window::Settings {
    settings.icon = app_window_icon();
    #[cfg(target_os = "linux")]
    {
        settings.platform_specific.application_id = APP_ID.to_string();
    }
    settings
}

/// Height of a view's own title row when the window uses
/// [`macos_fullsize_titlebar`].
pub(crate) const TITLEBAR_HEIGHT: f32 = 44.0;
/// Left inset of that title row: clears the macOS traffic lights.
#[cfg(target_os = "macos")]
pub(crate) const TITLEBAR_INSET_LEFT: f32 = 78.0;
#[cfg(not(target_os = "macos"))]
pub(crate) const TITLEBAR_INSET_LEFT: f32 = 12.0;

/// Transparent, full-size-content title bar so views draw their own top
/// row under the traffic lights (macOS only; other platforms keep theirs).
/// Every view using this must make its title row draggable via
/// `Message::WindowDragRequested`.
pub(crate) fn macos_fullsize_titlebar() -> window::settings::PlatformSpecific {
    #[cfg(target_os = "macos")]
    {
        window::settings::PlatformSpecific {
            title_hidden: true,
            titlebar_transparent: true,
            fullsize_content_view: true,
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        window::settings::PlatformSpecific::default()
    }
}

/// Whether windows built with [`macos_fullsize_titlebar`] hide the
/// native title bar, so their views draw their own title row. Off macOS
/// the native bar stays and already names and drags the window.
pub(crate) const DRAWS_OWN_TITLE_BAR: bool = cfg!(target_os = "macos");

/// `content` laid over the window's drag handle, [`TITLEBAR_HEIGHT`]
/// tall: the top row of a view whose window uses
/// [`macos_fullsize_titlebar`]. Use it directly for rows that hold
/// controls (they stay on every platform); use [`own_title_row`] for a
/// row that only names the window or clears the traffic lights.
pub(crate) fn title_drag_row<'a>(
    content: impl Into<iced::Element<'a, Message>>,
    window_id: Option<window::Id>,
) -> iced::Element<'a, Message> {
    let mut handle = iced::widget::mouse_area(
        iced::widget::Space::new()
            .width(iced::Length::Fill)
            .height(iced::Length::Fixed(TITLEBAR_HEIGHT)),
    );
    if let Some(id) = window_id {
        handle = handle.on_press(Message::WindowDragRequested(id));
    }
    iced::widget::stack![handle, content.into()]
        .width(iced::Length::Fill)
        .height(iced::Length::Fixed(TITLEBAR_HEIGHT))
        .into()
}

/// A title row that only names the window (or clears the traffic
/// lights): [`title_drag_row`] on macOS, nothing elsewhere, where it
/// would repeat the native title bar and cost 44 px.
pub(crate) fn own_title_row<'a>(
    content: impl Into<iced::Element<'a, Message>>,
    window_id: Option<window::Id>,
) -> Option<iced::Element<'a, Message>> {
    DRAWS_OWN_TITLE_BAR.then(|| title_drag_row(content, window_id))
}

pub(crate) fn welcome_window_settings() -> window::Settings {
    const W: f32 = 520.0;
    const H: f32 = 380.0;
    let size = iced::Size::new(W, H);
    with_app_identity(window::Settings {
        size,
        min_size: Some(iced::Size::new(420.0, 320.0)),
        position: centered_window_position_on_active_display(size)
            .unwrap_or(window::Position::Centered),
        resizable: true,
        decorations: true,
        transparent: false,
        visible: true,
        ..Default::default()
    })
}

/// Window settings for the annotation / actions editor. Default size
/// is wide enough to show the toolbar without wrapping and tall
/// enough to give a typical 16:9 capture comfortable headroom for
/// drawing.
pub(crate) fn editor_window_settings(
    display_bounds: Option<(f32, f32, f32, f32)>,
) -> window::Settings {
    const W: f32 = crate::editor::DEFAULT_WINDOW_WIDTH;
    const H: f32 = 760.0;
    let size = iced::Size::new(W, H);
    let position = centered_window_position(display_bounds, size)
        .or_else(|| centered_window_position_on_active_display(size))
        .unwrap_or(window::Position::Centered);
    with_app_identity(window::Settings {
        size,
        min_size: Some(iced::Size::new(720.0, 480.0)),
        position,
        resizable: true,
        decorations: true,
        transparent: false,
        visible: true,
        // The editor holds unsaved annotation work, so an OS-initiated
        // close (red X / ⌘W) must be intercepted and routed through the
        // unsaved-work confirmation instead of dropping the session.
        // `Message::WindowCloseRequested` performs the actual close.
        exit_on_close_request: false,
        platform_specific: macos_fullsize_titlebar(),
        ..Default::default()
    })
}

pub(crate) fn open_editor_window_replacing(
    state: &mut App,
    mut editor: crate::editor::EditorSession,
    display_bounds: Option<(f32, f32, f32, f32)>,
) -> Task<Message> {
    let mut tasks: Vec<Task<Message>> = Vec::new();
    // A History open still waiting would replace this editor when its
    // wait ends: it is cancelled instead (the History open itself clears
    // it before it gets here).
    super::history_ops::cancel_history_open(state, "another capture opened in the editor first");
    if let Some(old_editor) = state.editor.take() {
        if let Some(old_id) = old_editor.window_id {
            state.windows.forget(old_id);
            tasks.push(window::close(old_id));
        }
        tasks.push(super::compose_jobs::retire_editor(state, old_editor));
    }
    editor.source_display_bounds = display_bounds;
    state.editor = Some(editor);
    state.release_editor_modifiers();
    // The Text panel recognizes in the background as soon as the editor
    // opens, so the list is ready by the time the user looks at it.
    if state.preferences.editor_panel_open {
        tasks.push(request_text_recognition(state));
    }
    let (id, open_task) = window::open(editor_window_settings(display_bounds));
    register_window(state, id, WindowKind::Editor);
    tasks.push(open_task.map(Message::EditorWindowReady));
    Task::batch(tasks)
}

pub(crate) fn clear_pending_capture_state(state: &mut App) {
    state.pending_intent = None;
    state.pending_display_id = None;
    state.pending_display_scale = None;
    state.pending_display_bounds = None;
    state.pending_hide_cursor = true;
}

/// Window settings for the persistent capture browser. Standard
/// resizable window with native chrome — this isn't a transient
/// overlay, it's a regular workspace window the user stays inside
/// while triaging history.
pub(crate) fn history_window_settings() -> window::Settings {
    const W: f32 = 900.0;
    const H: f32 = 700.0;
    let size = iced::Size::new(W, H);
    with_app_identity(window::Settings {
        size,
        min_size: Some(iced::Size::new(540.0, 400.0)),
        position: centered_window_position_on_active_display(size)
            .unwrap_or(window::Position::Centered),
        resizable: true,
        decorations: true,
        transparent: false,
        visible: true,
        platform_specific: macos_fullsize_titlebar(),
        ..Default::default()
    })
}

/// Window settings for the preferences window.
pub(crate) fn settings_window_settings() -> window::Settings {
    const W: f32 = 720.0;
    const H: f32 = 640.0;
    let size = iced::Size::new(W, H);
    with_app_identity(window::Settings {
        size,
        min_size: Some(iced::Size::new(520.0, 480.0)),
        position: centered_window_position_on_active_display(size)
            .unwrap_or(window::Position::Centered),
        resizable: true,
        decorations: true,
        transparent: false,
        visible: true,
        platform_specific: macos_fullsize_titlebar(),
        ..Default::default()
    })
}

pub(crate) fn cli_tools_window_settings() -> window::Settings {
    const W: f32 = 760.0;
    const H: f32 = 620.0;
    let size = iced::Size::new(W, H);
    with_app_identity(window::Settings {
        size,
        min_size: Some(iced::Size::new(560.0, 420.0)),
        position: centered_window_position_on_active_display(size)
            .unwrap_or(window::Position::Centered),
        resizable: true,
        decorations: true,
        transparent: false,
        visible: true,
        platform_specific: macos_fullsize_titlebar(),
        ..Default::default()
    })
}

/// Window settings for one overlay window covering a single display.
///
/// Borderless transparent `AlwaysOnTop` window positioned at the
/// display's *global* logical origin and sized to its logical width
/// and height. Combining this with a transparent theme (see [`style`])
/// gives us a true see-through overlay so the user can see what they
/// are about to capture.
///
/// We avoid `fullscreen: true` here — it lets winit choose a monitor,
/// which is wrong on multi-display setups. Specific position + size
/// puts the window exactly where we want.
/// Window settings for a freshly-opened pin. Borderless,
/// always-on-top, sized to the captured image (capped at a sane max
/// so a giant 4K pin doesn't dominate the screen). The user
/// repositions by dragging anywhere on the body and dismisses via
/// the small `×` in the corner.
pub(crate) fn pin_window_settings(
    image_size: (u32, u32),
    display_bounds: Option<(f32, f32, f32, f32)>,
) -> window::Settings {
    const MAX_W: f32 = 800.0;
    const MAX_H: f32 = 600.0;
    let (iw, ih) = (image_size.0 as f32, image_size.1 as f32);
    let scale = (MAX_W / iw).min(MAX_H / ih).min(1.0);
    let w = (iw * scale).max(120.0);
    let h = (ih * scale).max(80.0);
    let size = iced::Size::new(w, h);
    let position = centered_window_position(display_bounds, size)
        .or_else(|| centered_window_position_on_active_display(size))
        .unwrap_or(window::Position::Default);
    with_app_identity(window::Settings {
        size,
        min_size: Some(iced::Size::new(120.0, 80.0)),
        position,
        resizable: true,
        decorations: false,
        transparent: false,
        visible: true,
        fullscreen: false,
        level: window::Level::AlwaysOnTop,
        closeable: false,
        minimizable: false,
        ..Default::default()
    })
}

pub(crate) fn centered_window_position(
    display_bounds: Option<(f32, f32, f32, f32)>,
    size: iced::Size,
) -> Option<window::Position> {
    display_bounds.map(|(x, y, display_w, display_h)| {
        let px = x + ((display_w - size.width).max(0.0) * 0.5);
        let py = y + ((display_h - size.height).max(0.0) * 0.5);
        window::Position::Specific(iced::Point::new(px, py))
    })
}

pub(crate) fn centered_window_position_on_active_display(
    size: iced::Size,
) -> Option<window::Position> {
    centered_window_position(active_display_bounds(), size)
}

#[cfg(target_os = "macos")]
pub(crate) fn active_display_bounds() -> Option<(f32, f32, f32, f32)> {
    use core_graphics::display::CGDisplay;
    use core_graphics::event::CGEvent;
    use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};

    let source = CGEventSource::new(CGEventSourceStateID::CombinedSessionState).ok()?;
    let cursor = CGEvent::new(source).ok()?.location();
    let (displays, count) = CGDisplay::displays_with_point(cursor, 1).ok()?;
    if count == 0 {
        return None;
    }
    let bounds = CGDisplay::new(*displays.first()?).bounds();
    Some((
        bounds.origin.x as f32,
        bounds.origin.y as f32,
        bounds.size.width as f32,
        bounds.size.height as f32,
    ))
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn active_display_bounds() -> Option<(f32, f32, f32, f32)> {
    None
}

pub(crate) const SCROLL_WINDOW_W: f32 = 360.0;
pub(crate) const SCROLL_WINDOW_H: f32 = 540.0;
pub(crate) const SCROLL_HUD_GAP: f32 = 16.0;

/// Fixed-size window beside the capture area: right, else left (top-
/// aligned), else below, else above, else the display's top-right.
/// When no strip fits full size, shrinks into the largest free strip
/// (down to 240 wide / 320 tall) before using the corner.
/// Shrinks to fit small displays. Returns logical-pixel **global**
/// coordinates (display origin added) so multi-monitor sessions land
/// the window on the correct monitor.
pub(crate) fn scroll_window_frame(
    rect: readshot_core::geom::Rect,
    display: Option<(f32, f32, f32, f32)>,
) -> (iced::Point, iced::Size) {
    let (ox, oy, dw, dh) = display.unwrap_or((0.0, 0.0, 1440.0, 900.0));
    let g = SCROLL_HUD_GAP;
    // Never larger than the display; minimums are capped likewise.
    let w0 = SCROLL_WINDOW_W.min((dw - 2.0 * g).max(1.0)).min(dw);
    let h0 = SCROLL_WINDOW_H.min((dh - g).max(1.0)).min(dh);
    let w_min = 240.0_f32.min(w0);
    let h_min = 320.0_f32.min(h0);
    let clamp_y = |y: f32, h: f32| y.clamp(0.0, (dh - h).max(0.0));
    let clamp_x = |x: f32, w: f32| x.clamp(0.0, (dw - w).max(0.0));

    // Candidate strips in preference order: right, left, below, above.
    // Side strips shrink width (keep height); top/bottom strips shrink
    // height (keep width). The largest-area candidate wins; ties keep
    // the earlier (preferred) side, so a full-size fit always beats a
    // shrunk one.
    let mut best: Option<((f32, f32), (f32, f32))> = None;
    let mut consider = |pos: (f32, f32), size: (f32, f32)| {
        let better = best.is_none_or(|(_, (bw, bh))| size.0 * size.1 > bw * bh);
        if better {
            best = Some((pos, size));
        }
    };
    let right_w = (dw - rect.right() - g).min(w0);
    if right_w >= w_min {
        consider((rect.right() + g, clamp_y(rect.y(), h0)), (right_w, h0));
    }
    let left_w = (rect.x() - g).min(w0);
    if left_w >= w_min {
        consider((rect.x() - g - left_w, clamp_y(rect.y(), h0)), (left_w, h0));
    }
    let below_h = (dh - rect.bottom() - g).min(h0);
    if below_h >= h_min {
        consider((clamp_x(rect.x(), w0), rect.bottom() + g), (w0, below_h));
    }
    let above_h = (rect.y() - g).min(h0);
    if above_h >= h_min {
        consider(
            (clamp_x(rect.x(), w0), rect.y() - g - above_h),
            (w0, above_h),
        );
    }
    let ((x, y), (w, h)) = best.unwrap_or((
        ((dw - w0 - g).max(0.0), g.min((dh - h0).max(0.0))),
        (w0, h0),
    ));
    (iced::Point::new(x + ox, y + oy), iced::Size::new(w, h))
}

/// Floating HUD shown while a scrolling-capture session is active.
/// Borderless, always-on-top, positioned outside the captured rect
/// (see [`scroll_window_frame`]) so the HUD doesn't appear in the
/// frames being stitched.
pub(crate) fn scroll_hud_window_settings(
    position: iced::Point,
    size: iced::Size,
) -> window::Settings {
    with_app_identity(window::Settings {
        size,
        min_size: Some(size),
        position: window::Position::Specific(position),
        resizable: false,
        decorations: false,
        transparent: true,
        visible: true,
        fullscreen: false,
        level: window::Level::AlwaysOnTop,
        closeable: false,
        minimizable: false,
        ..Default::default()
    })
}

/// Window settings for the transparent click-through region indicator
/// that highlights the captured rect while a scrolling-capture session
/// is running. Sized to cover the active display and positioned at
/// that display's global origin so the canvas inside can stroke the
/// rect in display-local coordinates without needing per-window
/// position math. Multi-monitor: the origin offsets ensure the
/// indicator opens on the *correct* monitor instead of always landing
/// on the primary display at (0, 0).
pub(crate) fn scroll_region_window_settings(
    display_bounds: (f32, f32, f32, f32),
) -> window::Settings {
    let (ox, oy, w, h) = display_bounds;
    with_app_identity(window::Settings {
        size: iced::Size::new(w, h),
        min_size: None,
        position: window::Position::Specific(iced::Point::new(ox, oy)),
        resizable: false,
        decorations: false,
        transparent: true,
        visible: true,
        fullscreen: false,
        level: window::Level::AlwaysOnTop,
        closeable: false,
        minimizable: false,
        ..Default::default()
    })
}

pub(crate) fn overlay_window_settings_for(
    display: &readshot_capture::DisplayInfo,
) -> window::Settings {
    let bounds = display.bounds;
    with_app_identity(window::Settings {
        size: iced::Size::new(bounds.width(), bounds.height()),
        min_size: None,
        position: window::Position::Specific(iced::Point::new(bounds.x(), bounds.y())),
        resizable: false,
        decorations: false,
        transparent: true,
        visible: true,
        fullscreen: false,
        level: window::Level::AlwaysOnTop,
        closeable: false,
        minimizable: false,
        ..Default::default()
    })
}

pub(crate) fn configure_overlay_window_after_open(id: window::Id) -> Task<Message> {
    #[cfg(target_os = "macos")]
    {
        window::run(id, |window| {
            if let Err(e) = set_macos_capture_overlay_level(window) {
                tracing::warn!(
                    target: readshot_core::log::cat::CAPTURE,
                    "could not raise capture overlay above macOS system UI: {e}"
                );
            }
        })
        .discard()
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = id;
        Task::none()
    }
}

#[cfg(target_os = "macos")]
pub(crate) fn set_macos_capture_overlay_level<W>(window: &W) -> Result<(), String>
where
    W: iced::window::raw_window_handle::HasWindowHandle + ?Sized,
{
    use iced::window::raw_window_handle::RawWindowHandle;
    use objc2::msg_send;
    use objc2::runtime::AnyObject;

    let handle = window.window_handle().map_err(|e| e.to_string())?;
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return Err("window is not an AppKit window".to_string());
    };

    // SAFETY: `window::run` gives us the live iced/winit window on the
    // event-loop thread. The raw handle guarantees `ns_view` points to
    // a valid NSView for the lifetime of the callback; asking the view
    // for its owning NSWindow and setting its level is an AppKit main
    // thread operation. `CGShieldingWindowLevel()+1` is the same class
    // of level winit uses for exclusive fullscreen, high enough to sit
    // above the menu bar and Dock during region selection.
    unsafe {
        let ns_view = handle.ns_view.as_ptr().cast::<AnyObject>();
        let ns_window: *mut AnyObject = msg_send![ns_view, window];
        if ns_window.is_null() {
            return Err("NSView has no owning NSWindow".to_string());
        }
        let level = core_graphics::display::CGShieldingWindowLevel() as isize + 1;
        let _: () = msg_send![ns_window, setLevel: level];
    }

    Ok(())
}

#[cfg(test)]
mod scroll_window_tests {
    use super::*;

    #[test]
    fn scroll_window_sits_right_of_the_region_top_aligned() {
        let rect = readshot_core::geom::Rect::from_xywh(100.0, 120.0, 600.0, 500.0).unwrap();
        let (pos, size) = scroll_window_frame(rect, Some((0.0, 0.0, 1440.0, 900.0)));
        assert_eq!(size, iced::Size::new(360.0, 540.0));
        assert_eq!(pos, iced::Point::new(716.0, 120.0));
    }

    #[test]
    fn scroll_window_falls_back_left_then_below_then_shrinks() {
        let rect = readshot_core::geom::Rect::from_xywh(900.0, 100.0, 500.0, 300.0).unwrap();
        let (pos, _) = scroll_window_frame(rect, Some((0.0, 0.0, 1440.0, 900.0)));
        assert_eq!(pos.x, 900.0 - 16.0 - 360.0);
        // Full-width region: neither side fits → below it.
        let wide = readshot_core::geom::Rect::from_xywh(0.0, 0.0, 1440.0, 300.0).unwrap();
        let (pos, _) = scroll_window_frame(wide, Some((0.0, 0.0, 1440.0, 900.0)));
        assert_eq!(pos.y, 316.0);
        let (_, size) = scroll_window_frame(rect, Some((0.0, 0.0, 1280.0, 500.0)));
        assert_eq!(size.height, 500.0 - 16.0);
    }

    #[test]
    fn scroll_window_offsets_by_display_origin() {
        let rect = readshot_core::geom::Rect::from_xywh(100.0, 120.0, 600.0, 500.0).unwrap();
        let (pos, _) = scroll_window_frame(rect, Some((1440.0, -200.0, 1440.0, 900.0)));
        assert_eq!(pos, iced::Point::new(1440.0 + 716.0, -200.0 + 120.0));
    }

    fn on_display(pos: iced::Point, size: iced::Size, d: (f32, f32, f32, f32)) -> bool {
        pos.x >= d.0 - 0.01
            && pos.y >= d.1 - 0.01
            && pos.x + size.width <= d.0 + d.2 + 0.01
            && pos.y + size.height <= d.1 + d.3 + 0.01
    }

    fn overlaps(pos: iced::Point, size: iced::Size, r: readshot_core::geom::Rect) -> bool {
        pos.x < r.right()
            && pos.x + size.width > r.x()
            && pos.y < r.bottom()
            && pos.y + size.height > r.y()
    }

    #[test]
    fn scroll_window_shrinks_into_free_strip_for_wide_tall_region() {
        let d = (0.0, 0.0, 1440.0, 900.0);
        let rect = readshot_core::geom::Rect::from_xywh(300.0, 100.0, 840.0, 700.0).unwrap();
        let (pos, size) = scroll_window_frame(rect, Some(d));
        assert!(on_display(pos, size, d), "{pos:?} {size:?}");
        assert!(!overlaps(pos, size, rect), "{pos:?} {size:?}");
        assert!(size.width >= 240.0 && size.height >= 320.0);
    }

    #[test]
    fn scroll_window_goes_above_when_only_above_fits() {
        let d = (0.0, 0.0, 1440.0, 900.0);
        let rect = readshot_core::geom::Rect::from_xywh(0.0, 600.0, 1440.0, 290.0).unwrap();
        let (pos, size) = scroll_window_frame(rect, Some(d));
        assert_eq!(size, iced::Size::new(360.0, 540.0));
        assert!(pos.y + size.height <= 600.0 - 16.0 + 0.01);
        assert!(on_display(pos, size, d));
        assert!(!overlaps(pos, size, rect));
    }

    #[test]
    fn scroll_window_stays_within_negative_origin_display() {
        let d = (-1280.0, 120.0, 1280.0, 800.0);
        let rect = readshot_core::geom::Rect::from_xywh(100.0, 100.0, 1000.0, 400.0).unwrap();
        let (pos, size) = scroll_window_frame(rect, Some(d));
        assert!(on_display(pos, size, d), "{pos:?} {size:?}");
    }

    #[test]
    fn scroll_window_true_no_fit_uses_corner_on_display() {
        let d = (0.0, 0.0, 1440.0, 900.0);
        let rect = readshot_core::geom::Rect::from_xywh(0.0, 0.0, 1440.0, 900.0).unwrap();
        let (pos, size) = scroll_window_frame(rect, Some(d));
        assert!(on_display(pos, size, d), "{pos:?} {size:?}");
        assert_eq!(pos, iced::Point::new(1440.0 - 360.0 - 16.0, 16.0));
    }
}

#[cfg(test)]
mod app_identity_tests {
    use super::*;

    /// Every window Readshot opens, through its settings constructor.
    fn every_window() -> Vec<(&'static str, window::Settings)> {
        let display = readshot_capture::DisplayInfo {
            id: "1".into(),
            bounds: readshot_core::geom::Rect::from_xywh(0.0, 0.0, 1440.0, 900.0).unwrap(),
            scale: 2.0,
            name: "Built-in".into(),
            is_primary: true,
        };
        vec![
            ("welcome", welcome_window_settings()),
            ("editor", editor_window_settings(None)),
            ("history", history_window_settings()),
            ("settings", settings_window_settings()),
            ("cli tools", cli_tools_window_settings()),
            ("pin", pin_window_settings((400, 300), None)),
            (
                "scroll hud",
                scroll_hud_window_settings(iced::Point::ORIGIN, iced::Size::new(360.0, 540.0)),
            ),
            (
                "scroll region",
                scroll_region_window_settings((0.0, 0.0, 1440.0, 900.0)),
            ),
            ("overlay", overlay_window_settings_for(&display)),
        ]
    }

    #[test]
    fn every_window_carries_the_readshot_icon() {
        for (name, settings) in every_window() {
            let icon = settings
                .icon
                .unwrap_or_else(|| panic!("{name} has no icon"));
            let (rgba, size) = icon.into_raw();
            assert_eq!((size.width, size.height), (128, 128), "{name}");
            // The brand tile, not a blank or placeholder: its centre is the
            // lens, its edge midpoints are the teal tile.
            let px = |x: u32, y: u32| {
                let i = ((y * 128 + x) * 4) as usize;
                [rgba[i], rgba[i + 1], rgba[i + 2], rgba[i + 3]]
            };
            assert_eq!(px(64, 8)[3], 255, "{name}: tile edge is opaque");
            let edge = px(64, 8);
            assert!(edge[2] > edge[0] && edge[1] > edge[0], "{name}: teal tile");
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn every_window_carries_the_app_id() {
        for (name, settings) in every_window() {
            assert_eq!(settings.platform_specific.application_id, APP_ID, "{name}");
        }
    }

    #[test]
    fn the_app_id_matches_the_bundle_and_desktop_entry() {
        let plist = include_str!("../../../../packaging/macos/Info.plist");
        assert!(plist.contains(&format!("<string>{APP_ID}</string>")));
        let desktop = include_str!("../../../../packaging/linux/readshot.desktop");
        assert!(desktop.contains(&format!("\nIcon={APP_ID}\n")));
        assert!(desktop.contains(&format!("\nStartupWMClass={APP_ID}\n")));
    }
}

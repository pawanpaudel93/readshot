// Extracted from runtime.rs (pure code-move). `use super::*` pulls in
// sibling/parent items; the explicit imports mirror runtime.rs's preamble.
use super::*;

use iced::window;
use iced::Task;

use crate::app::{App, Message, WindowKind};

pub(crate) fn mono_font() -> iced::Font {
    iced::Font::with_name(readshot_core::render::FONT_FAMILY)
}

pub(crate) fn welcome_window_settings() -> window::Settings {
    const W: f32 = 520.0;
    const H: f32 = 380.0;
    let size = iced::Size::new(W, H);
    window::Settings {
        size,
        min_size: Some(iced::Size::new(420.0, 320.0)),
        position: centered_window_position_on_active_display(size)
            .unwrap_or(window::Position::Centered),
        resizable: true,
        decorations: true,
        transparent: false,
        visible: true,
        ..Default::default()
    }
}

/// Window settings for the annotation / actions editor. Default size
/// is wide enough to show the toolbar without wrapping and tall
/// enough to give a typical 16:9 capture comfortable headroom for
/// drawing.
pub(crate) fn editor_window_settings(
    display_bounds: Option<(f32, f32, f32, f32)>,
) -> window::Settings {
    const W: f32 = 1100.0;
    const H: f32 = 760.0;
    let size = iced::Size::new(W, H);
    let position = centered_window_position(display_bounds, size)
        .or_else(|| centered_window_position_on_active_display(size))
        .unwrap_or(window::Position::Centered);
    window::Settings {
        size,
        min_size: Some(iced::Size::new(720.0, 480.0)),
        position,
        resizable: true,
        decorations: true,
        transparent: false,
        visible: true,
        ..Default::default()
    }
}

pub(crate) fn open_editor_window_replacing(
    state: &mut App,
    mut editor: crate::editor::EditorSession,
    display_bounds: Option<(f32, f32, f32, f32)>,
) -> Task<Message> {
    let mut tasks: Vec<Task<Message>> = Vec::new();
    if state.editor.is_some() {
        let coord = state.coordinator.clone();
        if let Some(old_editor) = state.editor.as_mut() {
            cancel_editor_previews(old_editor);
            commit_pending_editor_text(old_editor, &coord);
            if let Some(old_id) = old_editor.window_id {
                state.windows.forget(old_id);
                tasks.push(window::close(old_id));
            }
        }
    }
    editor.source_display_bounds = display_bounds;
    state.editor = Some(editor);
    let (id, open_task) = window::open(editor_window_settings(display_bounds));
    state.windows.register(id, WindowKind::Editor);
    tasks.push(open_task.map(Message::EditorWindowReady));
    Task::batch(tasks)
}

pub(crate) fn editor_pin_window_settings(
    editor: &crate::editor::EditorSession,
    size: (u32, u32),
) -> window::Settings {
    pin_window_settings(size, editor.source_display_bounds)
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
    window::Settings {
        size,
        min_size: Some(iced::Size::new(540.0, 400.0)),
        position: centered_window_position_on_active_display(size)
            .unwrap_or(window::Position::Centered),
        resizable: true,
        decorations: true,
        transparent: false,
        visible: true,
        ..Default::default()
    }
}

/// Window settings for the preferences window.
pub(crate) fn settings_window_settings() -> window::Settings {
    const W: f32 = 720.0;
    const H: f32 = 640.0;
    let size = iced::Size::new(W, H);
    window::Settings {
        size,
        min_size: Some(iced::Size::new(520.0, 480.0)),
        position: centered_window_position_on_active_display(size)
            .unwrap_or(window::Position::Centered),
        resizable: true,
        decorations: true,
        transparent: false,
        visible: true,
        ..Default::default()
    }
}

pub(crate) fn cli_tools_window_settings() -> window::Settings {
    const W: f32 = 760.0;
    const H: f32 = 620.0;
    let size = iced::Size::new(W, H);
    window::Settings {
        size,
        min_size: Some(iced::Size::new(560.0, 420.0)),
        position: centered_window_position_on_active_display(size)
            .unwrap_or(window::Position::Centered),
        resizable: true,
        decorations: true,
        transparent: false,
        visible: true,
        ..Default::default()
    }
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
    window::Settings {
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
    }
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

/// Logical width × height of the floating HUD shown while a scroll-
/// capture session is active. Kept as constants so the placement
/// helper below can pick a spot that doesn't overlap the capture
/// region (where it would otherwise show up in every captured frame
/// and ruin the stitch).
pub(crate) const SCROLL_HUD_WIDTH: f32 = 320.0;
pub(crate) const SCROLL_HUD_HEIGHT: f32 = 360.0;
pub(crate) const SCROLL_HUD_PREVIEW_HEIGHT: f32 = 170.0;
pub(crate) const SCROLL_HUD_GAP: f32 = 16.0;

/// Pick a HUD position that doesn't overlap the captured rect.
///
/// Order of preference: right of the rect → left of the rect → below →
/// above → fallback to the display's top-right corner clamped into
/// the display bounds. Returns logical-pixel **global** screen
/// coordinates (origin_x/y of the host display plus the in-display
/// offset) so multi-monitor sessions land the HUD on the correct
/// monitor.
pub(crate) fn scroll_hud_position(
    rect: readshot_core::geom::Rect,
    display_bounds: Option<(f32, f32, f32, f32)>,
) -> iced::Point {
    let (ox, oy, dw, dh) = display_bounds.unwrap_or((0.0, 0.0, 1440.0, 900.0));
    let w = SCROLL_HUD_WIDTH;
    let h = SCROLL_HUD_HEIGHT;
    let g = SCROLL_HUD_GAP;
    // Compute the local position first, then add the display origin
    // at the end so each branch reads the same way as before.
    let local: iced::Point = if rect.right() + g + w <= dw {
        iced::Point::new(rect.right() + g, rect.y().clamp(0.0, (dh - h).max(0.0)))
    } else if rect.x() - g - w >= 0.0 {
        iced::Point::new(rect.x() - g - w, rect.y().clamp(0.0, (dh - h).max(0.0)))
    } else if rect.bottom() + g + h <= dh {
        iced::Point::new(rect.x().clamp(0.0, (dw - w).max(0.0)), rect.bottom() + g)
    } else if rect.y() - g - h >= 0.0 {
        iced::Point::new(rect.x().clamp(0.0, (dw - w).max(0.0)), rect.y() - g - h)
    } else {
        iced::Point::new((dw - w - g).max(0.0), g)
    };
    iced::Point::new(local.x + ox, local.y + oy)
}

/// Floating HUD shown while a scrolling-capture session is active.
/// Borderless, always-on-top, positioned outside the captured rect
/// (see [`scroll_hud_position`]) so the HUD doesn't appear in the
/// frames being stitched.
pub(crate) fn scroll_hud_window_settings(position: iced::Point) -> window::Settings {
    window::Settings {
        size: iced::Size::new(SCROLL_HUD_WIDTH, SCROLL_HUD_HEIGHT),
        min_size: Some(iced::Size::new(SCROLL_HUD_WIDTH, SCROLL_HUD_HEIGHT)),
        position: window::Position::Specific(position),
        resizable: false,
        decorations: false,
        transparent: false,
        visible: true,
        fullscreen: false,
        level: window::Level::AlwaysOnTop,
        closeable: false,
        minimizable: false,
        ..Default::default()
    }
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
    window::Settings {
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
    }
}

pub(crate) fn overlay_window_settings_for(
    display: &readshot_capture::DisplayInfo,
) -> window::Settings {
    let bounds = display.bounds;
    window::Settings {
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
    }
}

pub(crate) fn configure_overlay_window_after_open(id: window::Id) -> Task<Message> {
    #[cfg(target_os = "macos")]
    {
        window::run(id, |window| {
            if let Err(e) = set_macos_capture_overlay_level(window) {
                tracing::warn!(
                    target: "readshot::overlay",
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

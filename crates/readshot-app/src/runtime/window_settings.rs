// Extracted from runtime.rs (pure code-move). `use super::*` pulls in
// sibling/parent items; the explicit imports mirror runtime.rs's preamble.
use super::*;

use iced::window;
use iced::Task;

use crate::app::{App, Message, WindowKind};

pub(crate) fn mono_font() -> iced::Font {
    iced::Font::with_name(readshot_core::render::FONT_FAMILY)
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
    const W: f32 = crate::editor::DEFAULT_WINDOW_WIDTH;
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
        // The editor holds unsaved annotation work, so an OS-initiated
        // close (red X / ⌘W) must be intercepted and routed through the
        // unsaved-work confirmation instead of dropping the session.
        // `Message::WindowCloseRequested` performs the actual close.
        exit_on_close_request: false,
        platform_specific: macos_fullsize_titlebar(),
        ..Default::default()
    }
}

pub(crate) fn open_editor_window_replacing(
    state: &mut App,
    mut editor: crate::editor::EditorSession,
    display_bounds: Option<(f32, f32, f32, f32)>,
) -> Task<Message> {
    let mut tasks: Vec<Task<Message>> = Vec::new();
    if let Some(old_editor) = state.editor.take() {
        if let Some(old_id) = old_editor.window_id {
            state.windows.forget(old_id);
            tasks.push(window::close(old_id));
        }
        tasks.push(super::compose_jobs::retire_editor(state, old_editor));
    }
    // Another editor now owns this History record: a retiring one's late
    // sync must never overwrite what the new one writes (a scrub
    // included), so it is dropped, unwritten.
    if let Some(id) = editor.source_record.as_ref().map(|r| r.id) {
        super::compose_jobs::drop_retiring_editors_of(state, id);
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
    window::Settings {
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
        platform_specific: macos_fullsize_titlebar(),
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
        platform_specific: macos_fullsize_titlebar(),
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
    window::Settings {
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

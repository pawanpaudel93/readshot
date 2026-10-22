//! Per-OS screen capture for Readshot.
//!
//! The public API is the [`Capturer`] trait plus platform-specific
//! implementations selected at compile time:
//!
//! * `macos::ScreenCaptureKitCapturer` — ScreenCaptureKit via a small
//!   Swift shim.
//! * `xcap_backend::XcapCapturer` — Windows and Linux, both wrapping
//!   the `xcap` crate (Windows.Graphics.Capture / DXGI on Windows;
//!   the xdg-desktop-portal ScreenCast portal on Wayland with an
//!   XGetImage fallback on X11).
//!
//! The optional `test-fixtures` feature exposes `fake::FakeCapturer`,
//! a deterministic fixture so higher crate tests can substitute
//! capture without an OS-level grant.
//!
//! Errors are typed as [`readshot_core::error::CaptureError`]; the CLI
//! exit-code mapper and MCP error mapper match
//! on its variants directly.

// Unsafe is only for platform FFI, and each block says why it is
// sound (AGENTS.md, "Things to avoid").
#![deny(clippy::undocumented_unsafe_blocks)]

use async_trait::async_trait;
use image::RgbaImage;
use readshot_core::error::CaptureError;
use readshot_core::geom::Rect;

#[cfg(any(test, feature = "test-fixtures"))]
pub mod fake;

#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(any(target_os = "windows", target_os = "linux"))]
pub mod xcap_backend;

/// Opaque OS handle for a display. macOS uses `CGDirectDisplayID` as a
/// decimal string; Windows uses the monitor's interface id; Linux uses
/// the portal's stream node id. Callers treat these as opaque tokens —
/// only the OS Capturer interprets them.
pub type DisplayId = String;

/// One capture request from coordinator to capturer.
///
/// `rect` is in **logical pixels** (a.k.a. points) relative to the target
/// display's bounds. `scale` is the display's HiDPI scale factor; the
/// capturer is responsible for producing an image at `rect.width * scale`
/// by `rect.height * scale` physical pixels so the result is sharp on
/// Retina / 4K displays.
#[derive(Clone, Debug)]
pub struct CaptureRequest {
    pub display_id: DisplayId,
    pub rect: Rect,
    pub scale: f32,
    pub hide_cursor: bool,
}

/// Opaque OS handle for a capturable window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowId(pub String);

/// Information about a capturable window. [`Capturer::list_windows`]
/// returns them front to back where the backend knows the stacking
/// order (macOS does), so the first window containing a point is the
/// one the user sees there ([`topmost_window_at`]).
#[derive(Clone, Debug)]
pub struct WindowInfo {
    pub id: WindowId,
    pub title: String,
    pub app_name: String,
    /// The display the window overlaps most; `None` for a window on no
    /// display (parked off screen).
    pub display_id: Option<DisplayId>,
    /// Window bounds in logical pixels.
    pub bounds: Rect,
}

/// One window in the on-screen stack, for a picker: every on-screen
/// window (panels, the Dock and the menu bar included) in front-to-back
/// order, so what covers a window blocks a pick of it; only `pickable`
/// ones (ordinary app windows, the ones [`Capturer::list_windows`]
/// returns) may be picked.
#[derive(Clone, Debug)]
pub struct StackedWindow {
    pub info: WindowInfo,
    pub pickable: bool,
}

/// Whether a window that can't be picked (a panel, the Dock, the menu
/// bar) blocks a pick of what is behind it: only when it is visible
/// (`alpha` above zero) and covers less than most of its display. A
/// transparent or display-sized overlay (screen-share border, cursor
/// highlighter, an invisible app strip) is see-through and must not stop
/// the user picking the window they see through it.
pub fn blocks_window_pick(alpha: f64, bounds: Rect, displays: &[DisplayInfo]) -> bool {
    /// Covering this much of a display makes a window an overlay.
    const OVERLAY_SHARE: f32 = 0.9;
    if alpha <= 0.01 {
        return false;
    }
    !displays.iter().any(|d| {
        let b = d.bounds;
        let w = (bounds.right().min(b.right()) - bounds.x().max(b.x())).max(0.0);
        let h = (bounds.bottom().min(b.bottom()) - bounds.y().max(b.y())).max(0.0);
        let area = b.width() * b.height();
        area > 0.0 && w * h >= OVERLAY_SHARE * area
    })
}

/// A window's place in a picker's stack: `Some(true)` pickable,
/// `Some(false)` a blocker in front of others, `None` left out.
/// `layer0` is an ordinary app window; `alpha` comes from the window
/// server's list (`None` when that list missed it or failed). A missing
/// alpha fails open: an ordinary window stays pickable, and an unknown
/// panel isn't assumed to be in the way.
pub fn stack_role(
    layer0: bool,
    alpha: Option<f64>,
    bounds: Rect,
    displays: &[DisplayInfo],
) -> Option<bool> {
    match alpha {
        None => layer0.then_some(true),
        Some(a) if a <= 0.01 => None,
        Some(_) if layer0 => Some(true),
        Some(a) => blocks_window_pick(a, bounds, displays).then_some(false),
    }
}

/// The frontmost window under a point in global logical coordinates:
/// the first in a front-to-back list whose bounds contain it.
pub fn topmost_window_at(windows: &[WindowInfo], x: f32, y: f32) -> Option<&WindowInfo> {
    windows.iter().find(|w| {
        let b = w.bounds;
        x >= b.x() && x < b.right() && y >= b.y() && y < b.bottom()
    })
}

/// Order `windows` front to back by `stacking`, the window ids from
/// front to back as the window server reports them. Windows missing
/// from it keep their relative order, behind the ones it names.
pub fn order_front_to_back(windows: &mut [WindowInfo], stacking: &[String]) {
    windows.sort_by_key(|w| {
        stacking
            .iter()
            .position(|id| *id == w.id.0)
            .unwrap_or(usize::MAX)
    });
}

/// One window capture request from coordinator to capturer.
#[derive(Clone, Debug)]
pub struct WindowCaptureRequest {
    pub window_id: WindowId,
    /// Omit native decorative shadows when the backend can control them.
    pub ignore_shadows: bool,
}

/// Crop a captured window image using a rectangle relative to the
/// window's top-left logical coordinate space.
///
/// Window capture backends return physical pixels, while window metadata is
/// reported in logical pixels. This helper derives the x/y scale from the
/// captured image size and the reported window bounds, then crops to exactly
/// the part of the requested rect inside the window (a rect with a negative
/// origin keeps only its overlap). A rect fully outside the image is
/// rejected as a typed invalid-region error.
pub fn crop_window_relative_rect(
    full: RgbaImage,
    window_bounds: Rect,
    rect_logical: Rect,
) -> Result<RgbaImage, CaptureError> {
    crop_logical_rect(
        &full,
        window_bounds.width(),
        window_bounds.height(),
        rect_logical,
        "window-relative rect is outside the captured window bounds",
    )
}

/// Crop a display capture to `rect_logical` (display-local logical
/// pixels) and return it at the requested output `scale`: the result
/// is the overlap of the rect with the display, sized
/// `overlap.width * scale` by `overlap.height * scale`.
///
/// `full` is the whole display as the backend captured it, covering
/// `logical_w` by `logical_h` logical pixels at whatever pixel ratio
/// the OS gave; that ratio is read from the image size. When it already
/// matches `scale` the crop is returned untouched, otherwise it is
/// resampled, so `--scale` / MCP `scale` mean the same on every backend.
pub fn crop_display_capture(
    full: &RgbaImage,
    logical_w: f32,
    logical_h: f32,
    rect_logical: Rect,
    scale: f32,
) -> Result<RgbaImage, CaptureError> {
    let outside =
        || CaptureError::InvalidRegion("region is outside the captured display bounds".to_string());
    let crop = crop_logical_rect(
        full,
        logical_w,
        logical_h,
        rect_logical,
        "region is outside the captured display bounds",
    )?;
    let overlap = clip_rect(rect_logical, logical_w, logical_h).ok_or_else(outside)?;
    let scale_x = capture_axis_scale(full.width(), logical_w);
    let scale_y = capture_axis_scale(full.height(), logical_h);
    if !scale.is_finite() || scale <= 0.0 {
        return Ok(crop);
    }
    let close = |a: f32| (a - scale).abs() <= SCALE_EPSILON;
    let target_w = if close(scale_x) {
        crop.width()
    } else {
        (overlap.width() * scale).round().max(1.0) as u32
    };
    let target_h = if close(scale_y) {
        crop.height()
    } else {
        (overlap.height() * scale).round().max(1.0) as u32
    };
    if (target_w, target_h) == crop.dimensions() {
        return Ok(crop);
    }
    Ok(image::imageops::resize(
        &crop,
        target_w,
        target_h,
        image::imageops::FilterType::CatmullRom,
    ))
}

/// Two pixel ratios closer than this are the same scale (no resample).
const SCALE_EPSILON: f32 = 0.01;

/// The part of `rect` inside the `0,0,width,height` area, or `None`
/// when they don't overlap.
pub fn clip_rect(rect: Rect, width: f32, height: f32) -> Option<Rect> {
    let x0 = rect.x().max(0.0);
    let y0 = rect.y().max(0.0);
    let x1 = rect.right().min(width);
    let y1 = rect.bottom().min(height);
    if x1 > x0 && y1 > y0 {
        Rect::from_ltrb(x0, y0, x1, y1)
    } else {
        None
    }
}

/// The display-local part of a requested capture rect that lies on
/// `display`: the rect a capture of it really returns. A rect partly
/// off the display is clipped to the overlap; one entirely off it is
/// a typed invalid-region error. Global rects that fit the display are
/// accepted as in [`rect_relative_to_display`].
pub fn clip_rect_to_display(rect: Rect, display: &DisplayInfo) -> Result<Rect, CaptureError> {
    let local = rect_relative_to_display(rect, display.bounds);
    clip_rect(local, display.bounds.width(), display.bounds.height()).ok_or_else(|| {
        CaptureError::InvalidRegion("region is outside the captured display bounds".to_string())
    })
}

/// Crop `full`, an image of a `logical_w` by `logical_h` logical area,
/// to exactly the part of `rect_logical` inside it. Each axis scale is
/// the image size over the logical size.
fn crop_logical_rect(
    full: &RgbaImage,
    logical_w: f32,
    logical_h: f32,
    rect_logical: Rect,
    outside: &str,
) -> Result<RgbaImage, CaptureError> {
    let scale_x = capture_axis_scale(full.width(), logical_w);
    let scale_y = capture_axis_scale(full.height(), logical_h);
    let span = |start: f32, end: f32, scale: f32, max: u32| -> Option<(u32, u32)> {
        let a = (start * scale).max(0.0);
        let b = (end * scale).min(max as f32);
        if max == 0 || b.partial_cmp(&a) != Some(std::cmp::Ordering::Greater) {
            return None;
        }
        let a = (a.round() as u32).min(max - 1);
        let b = (b.round() as u32).clamp(a + 1, max);
        Some((a, b - a))
    };
    let err = || CaptureError::InvalidRegion(outside.to_string());
    let (x0, w) = span(
        rect_logical.x(),
        rect_logical.right(),
        scale_x,
        full.width(),
    )
    .ok_or_else(err)?;
    let (y0, h) = span(
        rect_logical.y(),
        rect_logical.bottom(),
        scale_y,
        full.height(),
    )
    .ok_or_else(err)?;
    Ok(image::imageops::crop_imm(full, x0, y0, w, h).to_image())
}

fn capture_axis_scale(image_pixels: u32, logical_extent: f32) -> f32 {
    if logical_extent.is_finite() && logical_extent > 0.0 {
        image_pixels as f32 / logical_extent
    } else {
        1.0
    }
}

/// Shared safety limits for capture requests built from untrusted
/// input (CLI arguments, MCP tool calls). These live here — next to
/// [`CaptureRequest`] — as the single source of truth so the two
/// surfaces cannot silently drift apart.
///
/// Largest rect coordinate/dimension accepted, in logical pixels.
/// Bounds the worst-case capture buffer to roughly
/// `MAX_RECT_DIM² * 4 * MAX_SCALE²` bytes, keeping an adversarial
/// caller from coercing the process into multi-GB allocations.
pub const MAX_RECT_DIM: f32 = 16_384.0;

/// Largest output scale factor accepted. Anything beyond 8x of the
/// display's logical pixels is almost certainly a mistake or abuse.
pub const MAX_SCALE: f32 = 8.0;

/// Hard cap on `width * height * scale²` for one capture.
pub const MAX_CAPTURE_PIXELS: f64 = 100_000_000.0;

/// Reject a request whose post-scale pixel count exceeds
/// [`MAX_CAPTURE_PIXELS`].
pub fn validate_capture_pixels(rect: Rect, scale: f32) -> Result<(), CaptureError> {
    let pixels = rect.width() as f64 * rect.height() as f64 * scale as f64 * scale as f64;
    if pixels > MAX_CAPTURE_PIXELS {
        return Err(CaptureError::InvalidRegion(format!(
            "capture would be too large after scale ({pixels:.0} pixels > {MAX_CAPTURE_PIXELS:.0})"
        )));
    }
    Ok(())
}

/// Reject a non-finite, non-positive, or over-limit scale factor.
pub fn validate_scale(scale: f32) -> Result<(), CaptureError> {
    if !scale.is_finite() || scale <= 0.0 {
        return Err(CaptureError::InvalidRegion(
            "scale must be a positive finite number".into(),
        ));
    }
    if scale > MAX_SCALE {
        return Err(CaptureError::InvalidRegion(format!(
            "scale must not exceed {MAX_SCALE}"
        )));
    }
    Ok(())
}

/// Find a window's logical bounds by id via [`Capturer::list_windows`].
pub async fn lookup_window_bounds(
    capturer: &dyn Capturer,
    window_id: &WindowId,
) -> Result<Rect, CaptureError> {
    let windows = capturer.list_windows().await?;
    windows
        .into_iter()
        .find(|window| window.id == *window_id)
        .map(|window| window.bounds)
        .ok_or_else(|| CaptureError::WindowNotFound(window_id.0.clone()))
}

/// Information about an attached display, returned by
/// [`Capturer::list_displays`].
#[derive(Clone, Debug)]
pub struct DisplayInfo {
    pub id: DisplayId,
    /// Display bounds in logical pixels.
    pub bounds: Rect,
    pub scale: f32,
    pub name: String,
    pub is_primary: bool,
}

/// Full-display capture rect in the display-local coordinate space used by
/// [`CaptureRequest`].
pub fn display_local_bounds(display: &DisplayInfo) -> Rect {
    Rect::from_xywh(0.0, 0.0, display.bounds.width(), display.bounds.height())
        .expect("DisplayInfo bounds have positive dimensions")
}

/// Accept both the documented display-local rect contract and the older
/// de-facto global-bounds rect used by some call sites. Backends call this
/// at the edge so a global full-display rect from a listed `DisplayInfo`
/// still crops from `(0, 0)` on the selected monitor instead of from the
/// monitor's global origin inside a single-display image.
pub fn rect_relative_to_display(rect: Rect, display_bounds: Rect) -> Rect {
    let origin_is_global = display_bounds.x() != 0.0 || display_bounds.y() != 0.0;
    let rect_fits_display_local = rect.x() >= 0.0
        && rect.y() >= 0.0
        && rect.right() <= display_bounds.width()
        && rect.bottom() <= display_bounds.height();
    let rect_fits_display_global = rect.x() >= display_bounds.x()
        && rect.y() >= display_bounds.y()
        && rect.right() <= display_bounds.right()
        && rect.bottom() <= display_bounds.bottom();
    if origin_is_global && !rect_fits_display_local && rect_fits_display_global {
        Rect::from_xywh(
            rect.x() - display_bounds.x(),
            rect.y() - display_bounds.y(),
            rect.width(),
            rect.height(),
        )
        .unwrap_or(rect)
    } else {
        rect
    }
}

/// The single trait every Readshot capture backend implements.
///
/// `Send + Sync` because the capture coordinator holds a
/// `Box<dyn Capturer>` inside a `tokio::task` that may move between
/// runtime threads.
// `#[async_trait]` puts `#[must_use]` on methods that return its boxed
// (already must-use) future; newer clippy flags that as double_must_use.
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait Capturer: Send + Sync {
    async fn list_displays(&self) -> Result<Vec<DisplayInfo>, CaptureError>;
    async fn capture_region(&self, req: CaptureRequest) -> Result<RgbaImage, CaptureError>;

    async fn list_windows(&self) -> Result<Vec<WindowInfo>, CaptureError> {
        Err(CaptureError::Unsupported(
            "window capture is not supported on this platform".into(),
        ))
    }

    /// Every on-screen window front to back, for a picker
    /// ([`StackedWindow`]). The default, for backends that only know
    /// ordinary windows, offers [`Self::list_windows`] as they are.
    async fn list_window_stack(&self) -> Result<Vec<StackedWindow>, CaptureError> {
        Ok(self
            .list_windows()
            .await?
            .into_iter()
            .map(|info| StackedWindow {
                info,
                pickable: true,
            })
            .collect())
    }

    async fn capture_window(
        &self,
        _request: WindowCaptureRequest,
    ) -> Result<RgbaImage, CaptureError> {
        Err(CaptureError::Unsupported(
            "window capture is not supported on this platform".into(),
        ))
    }
}

/// Construct the platform-default Capturer for the current target. The
/// per-OS implementations live in their own modules; this function is here so
/// the composition root can ask for a Capturer without
/// branching on `cfg(target_os)` at the call site.
///
/// `#[allow(clippy::needless_return)]` is required because the cfg
/// attributes only attach to statement-position expressions, not to
/// tail expressions; an explicit `return` per arm is the canonical
/// way to express this.
#[allow(clippy::needless_return)]
pub fn default_capturer() -> Box<dyn Capturer> {
    #[cfg(target_os = "macos")]
    {
        return Box::new(macos::new());
    }
    #[cfg(any(target_os = "windows", target_os = "linux"))]
    {
        return Box::new(xcap_backend::new());
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        compile_error!(
            "readshot-capture: unsupported target_os; only macOS, Windows, and Linux are supported"
        );
    }
}

/// How long a single OS capture call may block before we give up on it.
///
/// Every real capture (even a multi-display 5K grab) returns in well
/// under a second; 30 s is purely a backstop against an OS call that
/// never returns at all (a pending TCC prompt, a wedged window server,
/// an asleep display). When it trips, the caller gets a typed error
/// instead of hanging forever.
const CAPTURE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Run a blocking per-OS capture call without wedging the async runtime.
///
/// The per-OS backends (`SCShareableContent::get`, `SCScreenshotManager`,
/// `xcap::Monitor`) are fully synchronous and can block for an unbounded
/// time. Awaiting them directly on a tokio worker parks that worker — and
/// because their internal wait has no timeout, a stalled call hangs the
/// whole runtime indefinitely (the historical `list-windows` /
/// `capture-window` hang). This helper moves the work onto a dedicated
/// blocking thread via [`tokio::task::spawn_blocking`] and bounds it with
/// [`CAPTURE_TIMEOUT`], so a stuck OS call surfaces as
/// [`CaptureError::Backend`] rather than freezing the app.
pub(crate) async fn run_capture_blocking<T, F>(op: &'static str, f: F) -> Result<T, CaptureError>
where
    F: FnOnce() -> Result<T, CaptureError> + Send + 'static,
    T: Send + 'static,
{
    run_capture_blocking_with_timeout(op, CAPTURE_TIMEOUT, f).await
}

async fn run_capture_blocking_with_timeout<T, F>(
    op: &'static str,
    timeout: std::time::Duration,
    f: F,
) -> Result<T, CaptureError>
where
    F: FnOnce() -> Result<T, CaptureError> + Send + 'static,
    T: Send + 'static,
{
    match tokio::time::timeout(timeout, tokio::task::spawn_blocking(f)).await {
        Ok(Ok(result)) => result,
        Ok(Err(join_err)) => Err(CaptureError::Backend(format!(
            "{op} capture worker thread failed: {join_err}"
        ))),
        Err(_elapsed) => Err(CaptureError::Backend(format!(
            "{op} timed out after {timeout:?}; the OS screen-capture call did not return"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(id: &str, x: f32, y: f32, w: f32, h: f32) -> WindowInfo {
        WindowInfo {
            id: WindowId(id.into()),
            title: String::new(),
            app_name: String::new(),
            display_id: Some("d".into()),
            bounds: Rect::from_xywh(x, y, w, h).unwrap(),
        }
    }

    #[test]
    fn the_frontmost_window_under_a_point_wins() {
        let windows = [
            window("front", 50.0, 50.0, 100.0, 100.0),
            window("back", 0.0, 0.0, 400.0, 300.0),
        ];
        assert_eq!(
            topmost_window_at(&windows, 60.0, 60.0).unwrap().id.0,
            "front"
        );
        assert_eq!(
            topmost_window_at(&windows, 10.0, 10.0).unwrap().id.0,
            "back"
        );
        // Right and bottom edges are outside.
        assert_eq!(
            topmost_window_at(&windows, 400.0, 10.0).map(|w| &w.id.0),
            None
        );
    }

    fn display(x: f32, w: f32, h: f32) -> DisplayInfo {
        DisplayInfo {
            id: format!("d{x}"),
            bounds: Rect::from_xywh(x, 0.0, w, h).unwrap(),
            scale: 2.0,
            name: String::new(),
            is_primary: x == 0.0,
        }
    }

    #[test]
    fn a_window_the_window_server_list_misses_is_still_offered() {
        let displays = [display(0.0, 1512.0, 982.0)];
        let window = rect(100.0, 100.0, 400.0, 300.0);
        // Missing from the CG list (or the list call failed): an
        // ordinary window stays pickable (fail open), an unknown panel
        // is not assumed to be in the way.
        assert_eq!(stack_role(true, None, window, &displays), Some(true));
        assert_eq!(stack_role(false, None, window, &displays), None);
        // Known alpha: invisible windows are left out, panels block.
        assert_eq!(stack_role(true, Some(0.0), window, &displays), None);
        assert_eq!(stack_role(true, Some(1.0), window, &displays), Some(true));
        assert_eq!(stack_role(false, Some(1.0), window, &displays), Some(false));
    }

    #[test]
    fn only_visible_partial_windows_block_a_pick() {
        let displays = [display(0.0, 1512.0, 982.0), display(1512.0, 1920.0, 1080.0)];
        // The Dock and an opaque floating panel block.
        assert!(blocks_window_pick(
            1.0,
            rect(300.0, 920.0, 900.0, 62.0),
            &displays
        ));
        assert!(blocks_window_pick(
            1.0,
            rect(100.0, 100.0, 300.0, 400.0),
            &displays
        ));
        // An invisible window (alpha 0, e.g. a browser's menu-bar strip)
        // never blocks.
        assert!(!blocks_window_pick(
            0.0,
            rect(0.0, 0.0, 1512.0, 37.0),
            &displays
        ));
        // A display-sized overlay (screen-share border, cursor
        // highlighter, annotation layer) is see-through by nature.
        assert!(!blocks_window_pick(
            1.0,
            rect(1512.0, 0.0, 1920.0, 1080.0),
            &displays
        ));
        assert!(!blocks_window_pick(
            1.0,
            rect(0.0, 0.0, 1512.0, 982.0),
            &displays
        ));
    }

    #[tokio::test]
    async fn a_backend_without_a_stack_offers_its_windows_as_pickable() {
        let fake = crate::fake::FakeCapturer::new();
        let listed = fake.list_windows().await.unwrap();
        let stack = fake.list_window_stack().await.unwrap();
        assert_eq!(stack.len(), listed.len());
        assert!(stack.iter().all(|w| w.pickable));
    }

    #[test]
    fn windows_sort_by_the_stacking_order_and_unknown_ones_go_last() {
        let mut windows = [
            window("a", 0.0, 0.0, 1.0, 1.0),
            window("x", 0.0, 0.0, 1.0, 1.0),
            window("b", 0.0, 0.0, 1.0, 1.0),
            window("y", 0.0, 0.0, 1.0, 1.0),
        ];
        order_front_to_back(&mut windows, &["b".into(), "a".into()]);
        let ids: Vec<&str> = windows.iter().map(|w| w.id.0.as_str()).collect();
        assert_eq!(ids, ["b", "a", "x", "y"]);
    }

    struct RegionOnlyCapturer;

    #[async_trait]
    impl Capturer for RegionOnlyCapturer {
        async fn list_displays(&self) -> Result<Vec<DisplayInfo>, CaptureError> {
            Ok(Vec::new())
        }

        async fn capture_region(&self, _req: CaptureRequest) -> Result<RgbaImage, CaptureError> {
            Ok(RgbaImage::new(1, 1))
        }
    }

    #[tokio::test]
    async fn run_capture_blocking_returns_value_for_fast_work() {
        // A normal, fast OS call: its result flows straight back through.
        let result = run_capture_blocking("fast", || Ok::<_, CaptureError>(42)).await;
        assert_eq!(result.unwrap(), 42);
    }

    #[tokio::test]
    async fn run_capture_blocking_propagates_backend_error() {
        let result: Result<(), CaptureError> =
            run_capture_blocking("boom", || Err(CaptureError::Backend("kaboom".into()))).await;
        assert!(matches!(result, Err(CaptureError::Backend(m)) if m == "kaboom"));
    }

    #[tokio::test]
    async fn run_capture_blocking_times_out_when_os_call_never_returns() {
        // Regression for the `list-windows` / `capture-window` hang: a
        // blocking OS call that parks its thread without returning must
        // surface a typed timeout error, not hang forever.
        let result: Result<(), CaptureError> = run_capture_blocking_with_timeout(
            "stuck",
            std::time::Duration::from_millis(20),
            || {
                std::thread::sleep(std::time::Duration::from_millis(300));
                Ok(())
            },
        )
        .await;
        match result {
            Err(CaptureError::Backend(msg)) => {
                assert!(msg.contains("timed out"), "unexpected message: {msg}");
            }
            other => panic!("expected a timeout Backend error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn default_window_capture_is_unsupported() {
        let capturer = RegionOnlyCapturer;
        let result = capturer.list_windows().await;

        assert!(matches!(result, Err(CaptureError::Unsupported(_))));
    }

    #[tokio::test]
    async fn default_window_image_capture_is_unsupported() {
        let capturer = RegionOnlyCapturer;
        let result = capturer
            .capture_window(WindowCaptureRequest {
                window_id: WindowId("missing".to_string()),
                ignore_shadows: false,
            })
            .await;

        assert!(matches!(result, Err(CaptureError::Unsupported(_))));
    }

    fn solid(w: u32, h: u32) -> RgbaImage {
        RgbaImage::from_fn(w, h, |x, y| image::Rgba([x as u8, y as u8, 0, 255]))
    }

    fn rect(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect::from_xywh(x, y, w, h).expect("test rect must be valid")
    }

    #[test]
    fn display_local_bounds_starts_at_zero() {
        let display = DisplayInfo {
            id: "side".into(),
            bounds: rect(1440.0, -120.0, 1920.0, 1080.0),
            scale: 2.0,
            name: "Side".into(),
            is_primary: false,
        };

        assert_eq!(
            display_local_bounds(&display),
            rect(0.0, 0.0, 1920.0, 1080.0)
        );
    }

    #[test]
    fn rect_relative_to_display_accepts_legacy_global_rect() {
        let local = rect_relative_to_display(
            rect(1440.0, 120.0, 1920.0, 1080.0),
            rect(1440.0, 120.0, 1920.0, 1080.0),
        );

        assert_eq!(local, rect(0.0, 0.0, 1920.0, 1080.0));
    }

    #[test]
    fn rect_relative_to_display_keeps_display_local_rect() {
        let local = rect_relative_to_display(
            rect(10.0, 20.0, 200.0, 100.0),
            rect(1440.0, 120.0, 1920.0, 1080.0),
        );

        assert_eq!(local, rect(10.0, 20.0, 200.0, 100.0));
    }

    #[test]
    fn rect_relative_to_display_prefers_ambiguous_local_rect() {
        let local = rect_relative_to_display(
            rect(1500.0, 140.0, 200.0, 100.0),
            rect(1440.0, 120.0, 1920.0, 1080.0),
        );

        assert_eq!(local, rect(1500.0, 140.0, 200.0, 100.0));
    }

    #[test]
    fn crop_window_relative_rect_crops_inside_region() {
        let cropped = crop_window_relative_rect(
            solid(100, 80),
            rect(0.0, 0.0, 100.0, 80.0),
            rect(10.0, 20.0, 30.0, 25.0),
        )
        .unwrap();

        assert_eq!(cropped.width(), 30);
        assert_eq!(cropped.height(), 25);
        assert_eq!(cropped.get_pixel(0, 0), &image::Rgba([10, 20, 0, 255]));
    }

    #[test]
    fn crop_window_relative_rect_scales_logical_rect_to_physical_pixels() {
        let cropped = crop_window_relative_rect(
            solid(200, 160),
            rect(0.0, 0.0, 100.0, 80.0),
            rect(10.0, 20.0, 30.0, 25.0),
        )
        .unwrap();

        assert_eq!(cropped.width(), 60);
        assert_eq!(cropped.height(), 50);
        assert_eq!(cropped.get_pixel(0, 0), &image::Rgba([20, 40, 0, 255]));
    }

    #[test]
    fn crop_window_relative_rect_clamps_partial_overlap() {
        let cropped = crop_window_relative_rect(
            solid(100, 80),
            rect(0.0, 0.0, 100.0, 80.0),
            rect(90.0, 70.0, 30.0, 25.0),
        )
        .unwrap();

        assert_eq!(cropped.width(), 10);
        assert_eq!(cropped.height(), 10);
    }

    #[test]
    fn crop_window_relative_rect_keeps_only_the_overlap_of_a_negative_origin() {
        // B2: x = -10, width 30 covers window x 0..20 only.
        let cropped = crop_window_relative_rect(
            solid(100, 80),
            rect(0.0, 0.0, 100.0, 80.0),
            rect(-10.0, -5.0, 30.0, 25.0),
        )
        .unwrap();

        assert_eq!(cropped.dimensions(), (20, 20));
        assert_eq!(cropped.get_pixel(0, 0), &image::Rgba([0, 0, 0, 255]));
    }

    #[test]
    fn display_crop_keeps_only_the_overlap_of_a_negative_origin() {
        // B2: logical 200x150 display at 2x; x = -100, width 200 is the
        // logical 0..100 overlap, not 0..200.
        let cropped = crop_display_capture(
            &solid(400, 300),
            200.0,
            150.0,
            rect(-100.0, 10.0, 200.0, 50.0),
            2.0,
        )
        .unwrap();

        assert_eq!(cropped.dimensions(), (200, 100));
        assert_eq!(cropped.get_pixel(0, 0), &image::Rgba([0, 20, 0, 255]));
    }

    #[test]
    fn display_crop_honours_the_requested_scale() {
        // B1: a 2x capture asked for at scale 1 comes back at 1x.
        let cropped = crop_display_capture(
            &solid(400, 300),
            200.0,
            150.0,
            rect(10.0, 20.0, 30.0, 40.0),
            1.0,
        )
        .unwrap();
        assert_eq!(cropped.dimensions(), (30, 40));

        // At the native ratio nothing is resampled.
        let native = crop_display_capture(
            &solid(400, 300),
            200.0,
            150.0,
            rect(10.0, 20.0, 30.0, 40.0),
            2.0,
        )
        .unwrap();
        assert_eq!(native.dimensions(), (60, 80));
        assert_eq!(native.get_pixel(0, 0), &image::Rgba([20, 40, 0, 255]));

        // And upscaling works too.
        let up = crop_display_capture(
            &solid(200, 150),
            200.0,
            150.0,
            rect(10.0, 20.0, 30.0, 40.0),
            3.0,
        )
        .unwrap();
        assert_eq!(up.dimensions(), (90, 120));
    }

    #[test]
    fn display_crop_uses_independent_x_and_y_ratios() {
        let cropped = crop_display_capture(
            &solid(400, 300),
            400.0,
            200.0,
            rect(10.0, 20.0, 30.0, 40.0),
            1.0,
        )
        .unwrap();
        // y is captured at 1.5x and resampled to the requested 1x.
        assert_eq!(cropped.dimensions(), (30, 40));
    }

    #[test]
    fn display_crop_rejects_a_rect_off_the_display() {
        let err = crop_display_capture(
            &solid(100, 100),
            50.0,
            50.0,
            rect(51.0, 51.0, 10.0, 10.0),
            2.0,
        )
        .unwrap_err();
        assert!(matches!(err, CaptureError::InvalidRegion(_)));
    }

    #[test]
    fn clip_rect_to_display_returns_the_overlap_or_an_error() {
        let display = DisplayInfo {
            id: "d".into(),
            bounds: rect(1512.0, 0.0, 1920.0, 1080.0),
            scale: 2.0,
            name: String::new(),
            is_primary: false,
        };
        // B3: partly off the display → the part on it.
        assert_eq!(
            clip_rect_to_display(rect(1900.0, 1060.0, 100.0, 100.0), &display).unwrap(),
            rect(1900.0, 1060.0, 20.0, 20.0)
        );
        // B2: a negative origin keeps its overlap.
        assert_eq!(
            clip_rect_to_display(rect(-100.0, 0.0, 200.0, 50.0), &display).unwrap(),
            rect(0.0, 0.0, 100.0, 50.0)
        );
        // Entirely off → invalid region.
        assert!(matches!(
            clip_rect_to_display(rect(5000.0, 5000.0, 100.0, 100.0), &display),
            Err(CaptureError::InvalidRegion(_))
        ));
        // A global rect that fits the display still maps to local.
        assert_eq!(
            clip_rect_to_display(rect(3000.0, 100.0, 50.0, 50.0), &display).unwrap(),
            rect(1488.0, 100.0, 50.0, 50.0)
        );
    }

    #[test]
    fn crop_window_relative_rect_rejects_fully_outside_rect() {
        let result = crop_window_relative_rect(
            solid(100, 80),
            rect(0.0, 0.0, 100.0, 80.0),
            rect(120.0, 90.0, 30.0, 25.0),
        );

        assert!(
            matches!(result, Err(CaptureError::InvalidRegion(message)) if message.contains("outside"))
        );
    }
}

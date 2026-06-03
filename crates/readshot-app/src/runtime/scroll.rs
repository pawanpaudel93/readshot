// Extracted from runtime.rs (pure code-move). `use super::*` pulls in
// sibling/parent items; the explicit imports mirror runtime.rs's preamble.

/// Returns the captured image instead of saving — the editor flow
/// uses this so the user can choose what to do with the bytes.
///
/// Looks up the named display so the request carries its real HiDPI
/// scale; without that, the macOS backend would render at half
/// resolution on Retina monitors.
/// Hard limits for a single scrolling-capture session. Tuned so a
/// runaway loop bounded by these caps still produces a sane stitched
/// output and doesn't gobble gigabytes of RAM. The limits below cover
/// roughly 30 s of continuous scrolling at the configured tick rate.
pub(crate) const SCROLL_MAX_FRAMES: usize = 120;
/// Maximum captures allowed in flight at once. Lets a fast scroll
/// overlap several SCK round-trips (each ~100 ms on macOS) instead
/// of serialising them, so heavy trackpad scrolls don't lose frames.
pub(crate) const SCROLL_MAX_CONCURRENT_CAPTURES: u32 = 3;
/// Minimum estimated vertical movement before a captured frame is
/// accepted into the scroll session. Smaller offsets are usually
/// duplicate frames, hover/caret animation, or a tiny inertial nudge
/// that would repeat content in the stitched output.
pub(crate) const SCROLL_MIN_ACCEPTED_MOTION_PX: u32 = 8;
/// Tick interval (ms) driving the per-frame capture loop. 120 ms ≈
/// 8.3 fps — fast enough that a brisk trackpad scroll produces many
/// frames and the session feels live. Captures are debounced inside
/// the session via `capture_in_flight`, so if the backend can't keep
/// up the loop naturally throttles to whatever rate the
/// `capture_region` future supports.
pub(crate) const SCROLL_FRAME_INTERVAL_MS: u64 = 120;

pub(crate) fn drain_ready_scroll_frames(session: &mut crate::app::ScrollSession) {
    while let Some(result) = session
        .pending_frames
        .remove(&session.next_frame_seq_to_process)
    {
        session.next_frame_seq_to_process = session.next_frame_seq_to_process.saturating_add(1);
        match result {
            Ok(image) => accept_scroll_frame_if_moved(session, image),
            Err(e) => {
                tracing::warn!(target: "readshot::scroll", "frame capture failed: {e}");
                session.no_motion_count += 1;
            }
        }
    }
}

pub(crate) fn accept_scroll_frame_if_moved(
    session: &mut crate::app::ScrollSession,
    image: image::RgbaImage,
) {
    let moved = match session.frames.last() {
        Some(prev) => frame_has_scroll_motion(prev, &image),
        None => true,
    };
    if moved {
        // Cache an iced Handle once per accepted frame so the HUD's
        // live preview doesn't re-clone ~8 MB of RGBA on every redraw.
        let handle = iced::widget::image::Handle::from_rgba(
            image.width(),
            image.height(),
            image.as_raw().clone(),
        );
        session.no_motion_count = 0;
        session.frames.push(image);
        session.frame_tick = session.frame_tick.wrapping_add(1);
        session.last_frame_at = Some(std::time::Instant::now());
        session.last_frame_handle = Some(handle);
    } else {
        session.no_motion_count += 1;
    }
    // Session never auto-stops on stillness — the user explicitly
    // clicks Stop & Stitch (or Cancel) when they're done.
}

/// Returns true only when adjacent captures appear to have vertical
/// scroll movement, not merely changed pixels. This prevents repeated
/// frames caused by blinking carets, hover states, timers, or video
/// from entering the final stitch.
pub(crate) fn frame_has_scroll_motion(a: &image::RgbaImage, b: &image::RgbaImage) -> bool {
    if a.dimensions() != b.dimensions() {
        return true;
    }
    scroll_motion_offset(a, b).is_some_and(|dy| dy >= SCROLL_MIN_ACCEPTED_MOTION_PX)
}

pub(crate) fn scroll_motion_offset(a: &image::RgbaImage, b: &image::RgbaImage) -> Option<u32> {
    if a.dimensions() != b.dimensions() {
        return None;
    }
    let (w, h) = a.dimensions();
    if w == 0 || h == 0 {
        return None;
    }
    let strip_h = (h / 8).clamp(16, 80).min(h / 3).max(1);
    if strip_h >= h {
        return None;
    }
    // Use the lower-middle content region. It usually avoids sticky
    // headers while still leaving room below to detect downward scroll.
    let template_y = ((h * 2) / 3).min(h.saturating_sub(strip_h));
    let max_motion = (h / 2).max(SCROLL_MIN_ACCEPTED_MOTION_PX);
    let lo_y_b = template_y.saturating_sub(max_motion);
    let hi_y_b = template_y;
    let stride_x: u32 = 4;
    let stride_y: u32 = 2;
    let mut best_cost = u64::MAX;
    let mut best_y_b = hi_y_b;
    // Scan from no-motion upward. Equal-cost ties keep the smaller
    // motion, so blank/repeated content does not look like a scroll.
    let mut y_b = hi_y_b;
    loop {
        let cost = sad_scroll_strip(a, template_y, b, y_b, w, strip_h, (stride_x, stride_y));
        if cost < best_cost {
            best_cost = cost;
            best_y_b = y_b;
        }
        if y_b == lo_y_b {
            break;
        }
        y_b -= 1;
    }
    Some(template_y - best_y_b)
}

pub(crate) fn sad_scroll_strip(
    a: &image::RgbaImage,
    a_y: u32,
    b: &image::RgbaImage,
    b_y: u32,
    w: u32,
    strip_h: u32,
    stride: (u32, u32),
) -> u64 {
    let (stride_x, stride_y) = stride;
    let mut sum: u64 = 0;
    let mut y = 0u32;
    while y < strip_h {
        let mut x = 0u32;
        while x < w {
            let pa = a.get_pixel(x, a_y + y).0;
            let pb = b.get_pixel(x, b_y + y).0;
            sum += diff_u8(pa[0], pb[0]) as u64
                + diff_u8(pa[1], pb[1]) as u64
                + diff_u8(pa[2], pb[2]) as u64;
            x += stride_x;
        }
        y += stride_y;
    }
    sum
}

pub(crate) fn diff_u8(a: u8, b: u8) -> u32 {
    if a > b {
        (a - b) as u32
    } else {
        (b - a) as u32
    }
}

/// Run the stitching algorithm on a background thread so it doesn't
/// block the iced event loop. Stitching a long scroll can take a
/// hundred ms or two — fine in a worker, not fine on the UI thread.
pub(crate) async fn stitch_frames_async(
    frames: Vec<image::RgbaImage>,
) -> Result<image::RgbaImage, String> {
    tokio::task::spawn_blocking(move || {
        readshot_core::scroll_stitch::stitch_scrolling(
            &frames,
            readshot_core::scroll_stitch::StitchConfig::default(),
        )
        .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

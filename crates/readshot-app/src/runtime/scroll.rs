// Extracted from runtime.rs (pure code-move). `use super::*` pulls in
// sibling/parent items; the explicit imports mirror runtime.rs's preamble.

use super::*;

use crate::app::Message;
use iced::Task;

/// Hard limits for a single scrolling-capture session. Tuned so a
/// runaway loop bounded by these caps still produces a sane stitched
/// output and doesn't gobble gigabytes of RAM. The limits below cover
/// roughly 30 s of continuous scrolling at the configured tick rate.
pub(crate) const SCROLL_MAX_FRAMES: usize = 120;
/// The way back from a failed scrolling capture.
pub(crate) const SCROLL_RETRY_HINT: &str = "Start the scrolling capture again.";
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
/// Width (px) of the downscaled live-preview strip in the scroll window.
pub(crate) const PREVIEW_STRIP_W: u32 = 340;
/// Height cap (px) of the preview strip. Only the newest rows are kept,
/// so a long page doesn't shrink the preview to a sliver.
pub(crate) const PREVIEW_STRIP_MAX_H: u32 = 4096;
/// Frames accepted this soon after a first Cancel are the scroll that was
/// already under way (trackpad momentum keeps producing frames for a
/// second or so), not the user carrying on: they don't disarm it, so
/// the second click can reach Discard.
pub(crate) const SCROLL_CANCEL_MOMENTUM_GRACE: Duration = Duration::from_millis(1500);
/// How many recent frame joins the window remembers for its join ticks.
pub(crate) const RECENT_JOINS_MAX: usize = 64;

/// Downscaled slice for the live preview: the whole frame for the first
/// one, else only the newly revealed bottom `new_rows`. Runs inside the
/// existing `spawn_blocking` judge, never on the UI thread.
pub(crate) fn preview_rows(frame: &image::RgbaImage, new_rows: Option<u32>) -> image::RgbaImage {
    use image::imageops::{crop_imm, resize, FilterType};
    let (fw, fh) = frame.dimensions();
    let rows = new_rows.map_or(fh, |n| n.min(fh));
    if rows == 0 || fw == 0 {
        return image::RgbaImage::new(PREVIEW_STRIP_W, 0);
    }
    // Only the newest PREVIEW_STRIP_MAX_H strip rows are ever kept: on a
    // narrow region (upscaled to the strip width) scale just the source
    // rows that become them, never the whole frame.
    let kept_src_rows =
        (u64::from(PREVIEW_STRIP_MAX_H) * u64::from(fw)).div_ceil(u64::from(PREVIEW_STRIP_W));
    let rows = rows.min(u32::try_from(kept_src_rows).unwrap_or(u32::MAX).max(1));
    let scale = PREVIEW_STRIP_W as f32 / fw as f32;
    let h = ((rows as f32 * scale).round() as u32).max(1);
    let src = crop_imm(frame, 0, fh - rows, fw, rows).to_image();
    resize(&src, PREVIEW_STRIP_W, h, FilterType::Triangle)
}

/// Append newly revealed preview rows under the existing strip, keeping
/// at most `PREVIEW_STRIP_MAX_H` of the newest rows.
pub(crate) fn append_preview_rows(
    strip: Option<image::RgbaImage>,
    added: image::RgbaImage,
) -> image::RgbaImage {
    let Some(prev) = strip else {
        return keep_tail(added);
    };
    if added.height() == 0 {
        return prev;
    }
    let added = keep_tail(added);
    // Only the part of `prev` that stays above `added` is copied.
    let keep_prev = prev
        .height()
        .min(PREVIEW_STRIP_MAX_H.saturating_sub(added.height()));
    let mut out = image::RgbaImage::new(PREVIEW_STRIP_W, keep_prev + added.height());
    let prev_tail =
        image::imageops::crop_imm(&prev, 0, prev.height() - keep_prev, prev.width(), keep_prev);
    image::imageops::replace(&mut out, &*prev_tail, 0, 0);
    image::imageops::replace(&mut out, &added, 0, i64::from(keep_prev));
    out
}

fn keep_tail(img: image::RgbaImage) -> image::RgbaImage {
    if img.height() <= PREVIEW_STRIP_MAX_H {
        return img;
    }
    let y = img.height() - PREVIEW_STRIP_MAX_H;
    image::imageops::crop_imm(&img, 0, y, img.width(), PREVIEW_STRIP_MAX_H).to_image()
}

/// True after a first Cancel until it is answered: Discard (a second
/// press), Keep Going, or a new frame once the momentum grace is over
/// (the user carried on scrolling; [`SCROLL_CANCEL_MOMENTUM_GRACE`]).
/// There is no time limit (WCAG 2.2.1): a slow pointer or switch user
/// can always reach Discard.
pub(crate) fn cancel_armed(session: &crate::app::ScrollSession) -> bool {
    session.cancel_armed_at.is_some()
}

/// Process in-order completed captures. Frames needing a motion
/// verdict are compared against the previous accepted frame on a
/// blocking worker — the SAD scan over a Retina-sized region is tens
/// of milliseconds of pixel reads, far too much for the UI thread at
/// 8 fps. Draining pauses while a verdict is outstanding (so frames
/// stay chronological) and resumes from the `ScrollFrameJudged` arm.
pub(crate) fn drain_ready_scroll_frames(session: &mut crate::app::ScrollSession) -> Task<Message> {
    // After Stop the frames belong to the stitch task; nothing more may
    // join the session (or its preview strip).
    if session.stopping || session.judge_in_flight.is_some() {
        return Task::none();
    }
    while let Some(result) = session
        .pending_frames
        .remove(&session.next_frame_seq_to_process)
    {
        let seq = session.next_frame_seq_to_process;
        session.next_frame_seq_to_process = seq.saturating_add(1);
        match result {
            Ok(image) => {
                let Some(prev) = session.frames.last().cloned() else {
                    // First frame — accepted unconditionally. Its
                    // preview slice is computed once, synchronously.
                    let rows = preview_rows(&image, None);
                    accept_scroll_frame(session, image, Some(rows), None);
                    continue;
                };
                session.judge_in_flight = Some(seq);
                return Task::perform(
                    async move {
                        tokio::task::spawn_blocking(move || {
                            // Motion verdict and the preview slice are
                            // both computed here, off the UI thread.
                            let same_dims = prev.dimensions() == image.dimensions();
                            let offset = scroll_motion_offset(&prev, &image);
                            let moved = !same_dims
                                || offset.is_some_and(|dy| dy >= SCROLL_MIN_ACCEPTED_MOTION_PX);
                            let rows = moved.then(|| {
                                preview_rows(&image, if same_dims { offset } else { None })
                            });
                            (image, moved, offset, rows)
                        })
                        .await
                        .ok()
                    },
                    move |judged| match judged {
                        Some((image, moved, offset, rows)) => {
                            Message::Scroll(ScrollMsg::FrameJudged {
                                seq,
                                image: Some(image),
                                moved,
                                offset,
                                rows,
                            })
                        }
                        None => Message::Scroll(ScrollMsg::FrameJudged {
                            seq,
                            image: None,
                            moved: false,
                            offset: None,
                            rows: None,
                        }),
                    },
                );
            }
            Err(e) => {
                tracing::warn!(target: readshot_core::log::cat::CAPTURE, "frame capture failed: {e}");
                session.no_motion_count += 1;
            }
        }
    }
    Task::none()
}

/// Append an accepted frame and refresh the live-preview strip.
/// `rows` is the frame's downscaled preview slice (computed off the UI
/// thread); `offset` is the measured scroll motion vs. the previous
/// accepted frame (`None` for the first frame or a size change, which
/// counts the frame's full height).
pub(crate) fn accept_scroll_frame(
    session: &mut crate::app::ScrollSession,
    image: image::RgbaImage,
    rows: Option<image::RgbaImage>,
    offset: Option<u32>,
) {
    if let Some(r) = rows {
        session.last_frame_rows_scaled = (image.height() as f32 * PREVIEW_STRIP_W as f32
            / image.width().max(1) as f32)
            .round() as u32;
        // Every slice after the first is joined to the one above it.
        if session.preview_strip.is_some() && r.height() > 0 {
            if session.recent_join_heights.len() == RECENT_JOINS_MAX {
                session.recent_join_heights.pop_front();
            }
            session.recent_join_heights.push_back(r.height());
        }
        let strip = append_preview_rows(session.preview_strip.take(), r);
        // Cache an iced Handle once per accepted frame so the window's
        // live preview doesn't re-clone the strip on every redraw.
        session.preview_handle = Some(iced::widget::image::Handle::from_rgba(
            strip.width(),
            strip.height(),
            strip.as_raw().clone(),
        ));
        session.preview_strip = Some(strip);
    }
    session.stitched_rows = session
        .stitched_rows
        .saturating_add(offset.unwrap_or(image.height()));
    session.no_motion_count = 0;
    // Scrolling on answers an armed Cancel with "keep going"; momentum
    // frames right after the click don't.
    if session
        .cancel_armed_at
        .is_some_and(|at| at.elapsed() >= SCROLL_CANCEL_MOMENTUM_GRACE)
    {
        session.cancel_armed_at = None;
    }
    session.frames.push(std::sync::Arc::new(image));
    session.frame_tick = session.frame_tick.wrapping_add(1);
    session.last_frame_at = Some(std::time::Instant::now());
    // Session never auto-stops on stillness — the user explicitly
    // clicks Stop & Stitch (or Cancel) when they're done.
}

/// Returns true only when adjacent captures appear to have vertical
/// scroll movement, not merely changed pixels. This prevents repeated
/// frames caused by blinking carets, hover states, timers, or video
/// from entering the final stitch. The judge worker inlines the same
/// rule so it can reuse the measured offset for the preview strip.
#[cfg(test)]
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
    frames: Vec<std::sync::Arc<image::RgbaImage>>,
) -> Result<image::RgbaImage, String> {
    tokio::task::spawn_blocking(move || {
        // By stop time the session is the only owner of almost every
        // frame Arc, so unwrapping is copy-free; a frame still held by
        // an in-flight judge task falls back to one clone.
        let frames: Vec<image::RgbaImage> = frames
            .into_iter()
            .map(|f| std::sync::Arc::try_unwrap(f).unwrap_or_else(|arc| (*arc).clone()))
            .collect();
        readshot_core::scroll_stitch::stitch_scrolling(
            &frames,
            readshot_core::scroll_stitch::StitchConfig::default(),
        )
        .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Close (and forget) whichever of the session's HUD and region
/// windows are still open.
fn close_scroll_windows(state: &mut App, session: &crate::app::ScrollSession) -> Task<Message> {
    let mut tasks: Vec<Task<Message>> = Vec::new();
    for id in [session.hud_window_id, session.region_window_id]
        .into_iter()
        .flatten()
    {
        state.windows.forget(id);
        tasks.push(window::close(id));
    }
    Task::batch(tasks)
}

/// Every [`ScrollMsg`]: `update` hands each `Message::Scroll` here.
pub(crate) fn handle_scroll_message(state: &mut App, message: ScrollMsg) -> Task<Message> {
    match message {
        ScrollMsg::HudWindowReady(id) => {
            if let Some(session) = state.scroll_session.as_mut() {
                session.hud_window_id = Some(id);
            }
            Task::none()
        }

        ScrollMsg::RegionWindowReady(id) => {
            // Enable mouse passthrough so scroll wheel events fall
            // through to the page underneath. Without this, the
            // always-on-top transparent window would swallow scrolls.
            window::enable_mouse_passthrough(id)
        }

        ScrollMsg::HudDragRequested => {
            match state.scroll_session.as_ref().and_then(|s| s.hud_window_id) {
                Some(id) => window::drag(id),
                None => Task::none(),
            }
        }

        ScrollMsg::Tick => on_scroll_tick(state),

        ScrollMsg::Frame { seq, result } => on_scroll_frame(state, seq, result),

        ScrollMsg::FrameJudged {
            seq,
            image,
            moved,
            offset,
            rows,
        } => on_frame_judged(state, seq, image, moved, offset, rows),

        ScrollMsg::CancelRequested => on_scroll_cancel(state),

        ScrollMsg::KeepGoing => {
            if let Some(session) = state.scroll_session.as_mut() {
                session.cancel_armed_at = None;
            }
            Task::none()
        }

        ScrollMsg::StopRequested => on_scroll_stop(state),

        ScrollMsg::Stitched(result) => on_scroll_stitched(state, result),
    }
}

/// `ScrollMsg::Tick`.
fn on_scroll_tick(state: &mut App) -> Task<Message> {
    let Some(session) = state.scroll_session.as_mut() else {
        return Task::none();
    };
    if session.stopping || session.capture_in_flight >= SCROLL_MAX_CONCURRENT_CAPTURES {
        return Task::none();
    }
    if session.frames.len() >= SCROLL_MAX_FRAMES {
        return Task::done(Message::Scroll(ScrollMsg::StopRequested));
    }
    session.capture_in_flight += 1;
    session.next_capture_seq = session.next_capture_seq.saturating_add(1);
    let seq = session.next_capture_seq;
    let coord = state.coordinator.clone();
    let request = readshot_capture::CaptureRequest {
        display_id: session.display_id.clone(),
        rect: session.rect,
        scale: session.scale,
        hide_cursor: true,
    };
    Task::perform(
        async move { coord.capture_region(request).await },
        move |result| {
            Message::Scroll(ScrollMsg::Frame {
                seq,
                result: result.map_err(|e| e.to_string()),
            })
        },
    )
}

/// `ScrollMsg::Frame`.
fn on_scroll_frame(
    state: &mut App,
    seq: u64,
    result: Result<image::RgbaImage, String>,
) -> Task<Message> {
    let Some(session) = state.scroll_session.as_mut() else {
        return Task::none();
    };
    session.capture_in_flight = session.capture_in_flight.saturating_sub(1);
    // A capture still in flight at Stop is dropped: it isn't in
    // the stitch input, so it mustn't reach the preview either.
    if session.stopping || seq < session.next_frame_seq_to_process {
        return Task::none();
    }
    session.pending_frames.insert(seq, result);
    drain_ready_scroll_frames(session)
}

/// `ScrollMsg::FrameJudged`.
fn on_frame_judged(
    state: &mut App,
    seq: u64,
    image: Option<image::RgbaImage>,
    moved: bool,
    offset: Option<u32>,
    rows: Option<image::RgbaImage>,
) -> Task<Message> {
    let Some(session) = state.scroll_session.as_mut() else {
        return Task::none();
    };
    // A verdict from a cancelled/replaced session is stale, and
    // one landing after Stop is too (Stop clears the in-flight
    // judge; `stopping` is checked explicitly as well).
    if session.stopping || session.judge_in_flight != Some(seq) {
        return Task::none();
    }
    session.judge_in_flight = None;
    match image {
        Some(image) if moved => accept_scroll_frame(session, image, rows, offset),
        _ => session.no_motion_count += 1,
    }
    drain_ready_scroll_frames(session)
}

/// `ScrollMsg::CancelRequested`.
fn on_scroll_cancel(state: &mut App) -> Task<Message> {
    // Two-click cancel guard. Discarding a session that
    // already accumulated frames is destructive (the captured
    // frames are dropped on the floor), so the first click
    // arms the cancel and the window offers "Discard N Frames"
    // + "Keep Going". A second click actually discards; Keep
    // Going (or a new frame) disarms it. The prompt has no time
    // limit (WCAG 2.2.1).
    let Some(session) = state.scroll_session.as_mut() else {
        return Task::none();
    };
    // The frames already belong to the stitch task — nothing
    // left to discard, and the window shows "Stitching…".
    if session.stopping {
        return Task::none();
    }
    // Pristine sessions (only the first auto-captured frame)
    // skip the confirm — nothing of value to lose.
    let has_real_content = session.frames.len() > 1;
    if has_real_content && !cancel_armed(session) {
        session.cancel_armed_at = Some(std::time::Instant::now());
        return Task::none();
    }
    let Some(session) = state.scroll_session.take() else {
        return Task::none();
    };
    state.last_capture_status = Some(Status::new(
        StatusTone::Notice,
        "Scrolling capture cancelled.",
    ));
    close_scroll_windows(state, &session)
}

/// `ScrollMsg::StopRequested`.
fn on_scroll_stop(state: &mut App) -> Task<Message> {
    let Some(session) = state.scroll_session.as_mut() else {
        return Task::none();
    };
    if session.stopping {
        return Task::none();
    }
    // Nothing captured yet (still Waiting): there is nothing to
    // stitch, so Stop / esc ends the session like Cancel rather
    // than reporting a failed zero-frame stitch.
    if session.frames.is_empty() && session.preview_handle.is_none() {
        let Some(session) = state.scroll_session.take() else {
            return Task::none();
        };
        state.last_capture_status = Some(Status::new(
            StatusTone::Notice,
            "Scrolling capture cancelled.",
        ));
        return close_scroll_windows(state, &session);
    }
    session.stopping = true;
    session.cancel_armed_at = None;
    session.stitch_frame_count = session.frames.len();
    let frames = std::mem::take(&mut session.frames);
    // Drop buffered captures and any outstanding verdict: they
    // aren't part of the stitch and must not touch the preview.
    session.pending_frames.clear();
    session.judge_in_flight = None;
    let region_id = session.region_window_id.take();
    // Close only the region outline now. The scroll window stays
    // open through the stitch so it can show "Stitching…" (and
    // "limit reached"); `ScrollCaptureStitched` closes it.
    let mut tasks: Vec<Task<Message>> = Vec::new();
    if let Some(id) = region_id {
        state.windows.forget(id);
        tasks.push(window::close(id));
    }
    tasks.push(Task::perform(stitch_frames_async(frames), |r| {
        Message::Scroll(ScrollMsg::Stitched(r.map_err(|e| e.to_string())))
    }));
    Task::batch(tasks)
}

/// `ScrollMsg::Stitched`.
fn on_scroll_stitched(state: &mut App, result: Result<image::RgbaImage, String>) -> Task<Message> {
    // No session means it was torn down while stitching (e.g.
    // cancelled out-of-band): the result is stale, drop it.
    let Some(session) = state.scroll_session.take() else {
        tracing::debug!(target: readshot_core::log::cat::CAPTURE, "dropping stitch result: no session");
        return Task::none();
    };
    let display_scale = session.scale;
    let display_bounds = session.display_size;
    // The scroll window stayed open through the stitch; close it
    // now on both outcomes. Stop already closed the region
    // outline, but close it too if it is somehow still live.
    let close_hud = close_scroll_windows(state, &session);
    match result {
        Ok(image) => {
            let (w, h) = (image.width(), image.height());
            // Kept in History like any other capture (when History is
            // on), through the same save and the same one OCR.
            let record = history_record_for_capture(
                &image,
                &session.display_id,
                state.preferences.history_retention,
            );
            let shared_session = record.as_ref().map(|_| crate::editor::next_session_id());
            let history_task = if record.is_some() {
                persist_history_task(
                    state.coordinator.clone(),
                    image.clone(),
                    record.clone(),
                    state.preferences.history_retention,
                    state.preferences.clone(),
                    shared_session,
                )
            } else {
                Task::none()
            };
            let mut ed = super::capture_ops::new_capture_editor(
                image,
                record,
                shared_session,
                display_scale,
            );
            ed.set_status(
                StatusTone::Success,
                format!("Scrolling capture stitched into {w}\u{a0}×\u{a0}{h}\u{a0}px."),
            );
            Task::batch([
                close_hud,
                history_task,
                open_editor_window_replacing(state, ed, display_bounds),
            ])
        }
        Err(e) => {
            state.last_capture_status = Some(Status::new(
                StatusTone::Error,
                super::history_ops::friendly_string_error_with(
                    "stitch the scrolling capture",
                    &e.to_string(),
                    Some(SCROLL_RETRY_HINT),
                ),
            ));
            tracing::warn!(target: readshot_core::log::cat::CAPTURE, "stitch failed: {e}");
            close_hud
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_narrow_region_scales_only_the_rows_the_strip_keeps() {
        // An 8 px wide, 1600 px tall frame (a 4 pt region at 2x) would
        // upscale to 340 x 68000 (about 92 MB); only the newest 4096
        // strip rows are ever kept, so only their source rows scale.
        let mut frame = image::RgbaImage::from_pixel(8, 1600, image::Rgba([0, 0, 255, 255]));
        for y in 1500..1600 {
            for x in 0..8 {
                frame.put_pixel(x, y, image::Rgba([255, 0, 0, 255]));
            }
        }
        let rows = preview_rows(&frame, None);
        assert_eq!(rows.width(), PREVIEW_STRIP_W);
        assert!(
            rows.height() <= PREVIEW_STRIP_MAX_H + PREVIEW_STRIP_W / 8,
            "{} rows scaled",
            rows.height()
        );
        // It is the bottom of the frame: the newest rows.
        assert_eq!(rows.get_pixel(0, rows.height() - 1).0, [255, 0, 0, 255]);
        // Appending keeps the cap without building the whole join.
        let strip = append_preview_rows(None, rows);
        assert_eq!(strip.height(), PREVIEW_STRIP_MAX_H);
        let more = preview_rows(&frame, Some(1000));
        let strip = append_preview_rows(Some(strip), more);
        assert_eq!(strip.height(), PREVIEW_STRIP_MAX_H);
        assert_eq!(strip.get_pixel(0, strip.height() - 1).0, [255, 0, 0, 255]);
    }

    #[test]
    fn preview_rows_scale_to_strip_width() {
        let frame = image::RgbaImage::from_pixel(680, 400, image::Rgba([255, 0, 0, 255]));
        assert_eq!(
            preview_rows(&frame, None).dimensions(),
            (PREVIEW_STRIP_W, 200)
        );
        assert_eq!(
            preview_rows(&frame, Some(100)).dimensions(),
            (PREVIEW_STRIP_W, 50)
        );
    }

    #[test]
    fn strip_appends_and_keeps_the_newest_rows_when_capped() {
        let a = image::RgbaImage::new(PREVIEW_STRIP_W, 200);
        let s = append_preview_rows(None, a.clone());
        let s = append_preview_rows(Some(s), image::RgbaImage::new(PREVIEW_STRIP_W, 50));
        assert_eq!(s.height(), 250);
        let big = image::RgbaImage::new(PREVIEW_STRIP_W, 3000);
        let s = append_preview_rows(Some(append_preview_rows(None, big.clone())), big);
        assert_eq!(s.height(), PREVIEW_STRIP_MAX_H);
    }
}

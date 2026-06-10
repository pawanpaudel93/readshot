// Extracted from runtime.rs (pure code-move). `use super::*` pulls in
// sibling/parent items; the explicit imports mirror runtime.rs's preamble.

use super::*;

use crate::app::Message;
use iced::Task;

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

/// Process in-order completed captures. Frames needing a motion
/// verdict are compared against the previous accepted frame on a
/// blocking worker — the SAD scan over a Retina-sized region is tens
/// of milliseconds of pixel reads, far too much for the UI thread at
/// 8 fps. Draining pauses while a verdict is outstanding (so frames
/// stay chronological) and resumes from the `ScrollFrameJudged` arm.
pub(crate) fn drain_ready_scroll_frames(session: &mut crate::app::ScrollSession) -> Task<Message> {
    if session.judge_in_flight.is_some() {
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
                    // First frame — accepted unconditionally.
                    accept_scroll_frame(session, image);
                    continue;
                };
                session.judge_in_flight = Some(seq);
                return Task::perform(
                    async move {
                        tokio::task::spawn_blocking(move || {
                            let moved = frame_has_scroll_motion(&prev, &image);
                            (image, moved)
                        })
                        .await
                        .ok()
                    },
                    move |judged| match judged {
                        Some((image, moved)) => Message::ScrollFrameJudged {
                            seq,
                            image: Some(image),
                            moved,
                        },
                        None => Message::ScrollFrameJudged {
                            seq,
                            image: None,
                            moved: false,
                        },
                    },
                );
            }
            Err(e) => {
                tracing::warn!(target: "readshot::scroll", "frame capture failed: {e}");
                session.no_motion_count += 1;
            }
        }
    }
    Task::none()
}

/// Append an accepted frame and refresh the HUD preview state.
pub(crate) fn accept_scroll_frame(
    session: &mut crate::app::ScrollSession,
    image: image::RgbaImage,
) {
    // Cache an iced Handle once per accepted frame so the HUD's
    // live preview doesn't re-clone ~8 MB of RGBA on every redraw.
    let handle = iced::widget::image::Handle::from_rgba(
        image.width(),
        image.height(),
        image.as_raw().clone(),
    );
    session.no_motion_count = 0;
    session.frames.push(std::sync::Arc::new(image));
    session.frame_tick = session.frame_tick.wrapping_add(1);
    session.last_frame_at = Some(std::time::Instant::now());
    session.last_frame_handle = Some(handle);
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

/// Every scroll-capture message arm, extracted from `runtime::update` so the
/// top-level dispatcher stays navigable. Routed from `update`'s
/// grouped arm; the trailing `unreachable!` only fires if a
/// non-scroll-capture message is mis-routed here.
pub(crate) fn handle_scroll_message(state: &mut App, message: Message) -> Task<Message> {
    match message {
        Message::ScrollHudWindowReady(id) => {
            if let Some(session) = state.scroll_session.as_mut() {
                session.hud_window_id = Some(id);
            }
            Task::none()
        }

        Message::ScrollRegionWindowReady(id) => {
            // Enable mouse passthrough so scroll wheel events fall
            // through to the page underneath. Without this, the
            // always-on-top transparent window would swallow scrolls.
            window::enable_mouse_passthrough(id)
        }

        Message::ScrollHudDragRequested => {
            match state.scroll_session.as_ref().and_then(|s| s.hud_window_id) {
                Some(id) => window::drag(id),
                None => Task::none(),
            }
        }

        Message::ScrollCaptureTick => {
            let Some(session) = state.scroll_session.as_mut() else {
                return Task::none();
            };
            if session.stopping || session.capture_in_flight >= SCROLL_MAX_CONCURRENT_CAPTURES {
                return Task::none();
            }
            if session.frames.len() >= SCROLL_MAX_FRAMES {
                return Task::done(Message::ScrollCaptureStopRequested);
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
                move |result| Message::ScrollCaptureFrame {
                    seq,
                    result: result.map_err(|e| e.to_string()),
                },
            )
        }

        Message::ScrollCaptureFrame { seq, result } => {
            let Some(session) = state.scroll_session.as_mut() else {
                return Task::none();
            };
            session.capture_in_flight = session.capture_in_flight.saturating_sub(1);
            if seq < session.next_frame_seq_to_process {
                return Task::none();
            }
            session.pending_frames.insert(seq, result);
            drain_ready_scroll_frames(session)
        }

        Message::ScrollFrameJudged { seq, image, moved } => {
            let Some(session) = state.scroll_session.as_mut() else {
                return Task::none();
            };
            // A verdict from a cancelled/replaced session is stale.
            if session.judge_in_flight != Some(seq) {
                return Task::none();
            }
            session.judge_in_flight = None;
            match image {
                Some(image) if moved => accept_scroll_frame(session, image),
                _ => session.no_motion_count += 1,
            }
            drain_ready_scroll_frames(session)
        }

        Message::ScrollCaptureCancelRequested => {
            // Two-click cancel guard. Discarding a session that
            // already accumulated frames is destructive (the captured
            // frames are dropped on the floor), so the first click
            // arms the cancel and updates the HUD copy to confirm.
            // A second click inside `CANCEL_ARM_WINDOW` actually
            // discards. The arm expires on the OverlayTick if the
            // user changes their mind.
            const CANCEL_ARM_WINDOW: std::time::Duration = std::time::Duration::from_millis(2500);
            let Some(session) = state.scroll_session.as_mut() else {
                return Task::none();
            };
            // Pristine sessions (only the first auto-captured frame)
            // skip the confirm — nothing of value to lose.
            let has_real_content = session.frames.len() > 1;
            let now = std::time::Instant::now();
            let armed = session
                .cancel_armed_at
                .map(|t| now.duration_since(t) < CANCEL_ARM_WINDOW)
                .unwrap_or(false);
            if has_real_content && !armed {
                session.cancel_armed_at = Some(now);
                return Task::none();
            }
            let Some(session) = state.scroll_session.take() else {
                return Task::none();
            };
            let mut tasks: Vec<Task<Message>> = Vec::new();
            for id in [session.hud_window_id, session.region_window_id]
                .into_iter()
                .flatten()
            {
                state.windows.forget(id);
                tasks.push(window::close(id));
            }
            state.last_capture_status = Some("Scroll capture cancelled.".into());
            Task::batch(tasks)
        }

        Message::ScrollCaptureStopRequested => {
            let Some(session) = state.scroll_session.as_mut() else {
                return Task::none();
            };
            if session.stopping {
                return Task::none();
            }
            session.stopping = true;
            let frames = std::mem::take(&mut session.frames);
            let hud_id = session.hud_window_id;
            let region_id = session.region_window_id;
            // Close session windows now — the stitch task runs on its
            // own and the HUD / region overlay are no longer useful.
            let mut tasks: Vec<Task<Message>> = Vec::new();
            for id in [hud_id, region_id].into_iter().flatten() {
                state.windows.forget(id);
                tasks.push(window::close(id));
            }
            tasks.push(Task::perform(stitch_frames_async(frames), |r| {
                Message::ScrollCaptureStitched(r.map_err(|e| e.to_string()))
            }));
            Task::batch(tasks)
        }

        Message::ScrollCaptureStitched(result) => {
            let session = state.scroll_session.take();
            let display_scale = session.as_ref().map(|s| s.scale).unwrap_or(1.0);
            let display_bounds = session.as_ref().and_then(|s| s.display_size);
            match result {
                Ok(image) => {
                    let (w, h) = (image.width(), image.height());
                    let mut ed =
                        crate::editor::EditorSession::new_with_display_scale(image, display_scale);
                    ed.set_status(format!("Scrolling capture stitched into {w} × {h}px."));
                    open_editor_window_replacing(state, ed, display_bounds)
                }
                Err(e) => {
                    state.last_capture_status = Some(format!("Scroll capture failed: {e}"));
                    tracing::warn!(target: "readshot::scroll", "stitch failed: {e}");
                    Task::none()
                }
            }
        }
        other => unreachable!(
            "non-scroll-capture message routed to the scroll-capture handler: {other:?}"
        ),
    }
}

//! Capture messages: the overlay, the region capture and what is done
//! with the result outside the editor.
//! Moved out of `update_message`; `use super::*` keeps the arms'
//! names resolving as they did there.

use super::*;

/// Every [`CaptureMsg`].
pub(super) fn handle_capture_message(state: &mut App, message: CaptureMsg) -> Task<Message> {
    match message {
        CaptureMsg::OverlayShiftChanged(held) => on_overlay_shift_changed(state, held),

        CaptureMsg::OpenOverlayRequested => on_open_overlay(state),

        CaptureMsg::RetakeLastRegionRequested => on_retake_last_region(state),

        CaptureMsg::OverlayDisplaysListed(Err(e)) => on_displays_list_failed(state, e),
        CaptureMsg::OverlayDisplaysListed(Ok(displays)) => on_displays_listed(state, displays),

        CaptureMsg::OverlayWindowReady(id) => configure_overlay_window_after_open(id),

        CaptureMsg::OverlaySelected {
            display_id,
            rect,
            intent,
        } => on_overlay_selected(state, display_id, rect, intent),

        CaptureMsg::OverlaySelectionChanged { display_id, rect } => {
            on_overlay_selection_changed(state, display_id, rect)
        }

        CaptureMsg::OverlayCancelled => on_overlay_cancelled(state),

        CaptureMsg::RegionRequested { display_id, rect } => {
            on_capture_region(state, display_id, rect)
        }

        CaptureMsg::RegionCaptured(result) => on_region_captured(state, result),
        CaptureMsg::OverlayCopyDone(result) => on_overlay_copy_done(state, result),
        CaptureMsg::OverlaySaveDone(result) => on_overlay_save_done(state, result),
        CaptureMsg::OverlayCopyTextDone(result) => on_overlay_copy_text_done(state, result),
        CaptureMsg::CliInteractiveWritten(result) => on_cli_interactive_written(state, result),

        CaptureMsg::WindowCaptureRequested => on_window_capture_requested(state),
        CaptureMsg::WindowPickerListed(result) => on_window_picker_listed(state, result),
        CaptureMsg::WindowPicked(id) => on_window_picked(state, id),
    }
}

/// What a window pick says where the capture backend has no window
/// capture (Windows and Linux today).
pub(super) const WINDOW_CAPTURE_UNSUPPORTED: &str =
    "Window capture isn’t available on this platform yet. Drag a region around the window instead.";

/// `CaptureMsg::WindowCaptureRequested`: list displays and windows, then
/// open the overlay as a window picker. Gated like a region capture.
pub(super) fn on_window_capture_requested(state: &mut App) -> Task<Message> {
    if state.welcome.should_show() {
        return show_or_focus_welcome(state);
    }
    let overlay_up =
        state.overlay_opening || state.windows.kinds().any(|k| k == WindowKind::Overlay);
    if state.capture_in_flight || overlay_up || state.combine_capture.is_some() {
        return Task::none();
    }
    if state.scroll_session.is_some() {
        return focus_scroll_hud(state);
    }
    state.overlay_opening = true;
    let coord = state.coordinator.clone();
    Task::perform(
        async move {
            let unsupported = |e: readshot_core::error::CaptureError| match e {
                readshot_core::error::CaptureError::Unsupported(_) => {
                    WINDOW_CAPTURE_UNSUPPORTED.to_string()
                }
                other => other.to_string(),
            };
            let windows = coord.list_window_stack().await.map_err(unsupported)?;
            let displays = coord.list_displays().await.map_err(unsupported)?;
            Ok((displays, windows))
        },
        |r| Message::Capture(CaptureMsg::WindowPickerListed(r)),
    )
}

/// `CaptureMsg::WindowPickerListed`.
pub(super) fn on_window_picker_listed(
    state: &mut App,
    result: Result<
        (
            Vec<readshot_capture::DisplayInfo>,
            Vec<readshot_capture::StackedWindow>,
        ),
        String,
    >,
) -> Task<Message> {
    match result {
        Err(e) => {
            state.overlay_opening = false;
            state.last_capture_status = Some(if e == WINDOW_CAPTURE_UNSUPPORTED {
                Status::new(StatusTone::Warning, e)
            } else {
                tracing::warn!(target: readshot_core::log::cat::CAPTURE, "could not list windows: {e}");
                Status::new(StatusTone::Error, history_ops::friendly_capture_error(&e))
            });
            Task::none()
        }
        Ok((_, windows)) if !windows.iter().any(|w| w.pickable) => {
            state.overlay_opening = false;
            state.last_capture_status = Some(Status::new(
                StatusTone::Warning,
                "No windows to capture. Open the window first, then try again.",
            ));
            Task::none()
        }
        Ok((displays, windows)) => {
            state.window_pick = Some(windows);
            state.pending_intent = Some(crate::app::CaptureIntent::Editor);
            let task = on_displays_listed(state, displays);
            // No display: nothing opened, so nothing is being picked.
            if !state.windows.kinds().any(|k| k == WindowKind::Overlay) {
                state.window_pick = None;
            }
            task
        }
    }
}

/// `CaptureMsg::WindowPicked`: capture that window into the editor
/// (and History), like a region capture of its display.
pub(super) fn on_window_picked(
    state: &mut App,
    window_id: readshot_capture::WindowId,
) -> Task<Message> {
    let Some(windows) = state.window_pick.take() else {
        return Task::none();
    };
    let picked = windows
        .into_iter()
        .find(|w| w.pickable && w.info.id == window_id)
        .map(|w| w.info);
    let display = picked.as_ref().and_then(|w| {
        state
            .overlay_displays
            .values()
            .find(|d| d.display_id == w.display_id)
            .cloned()
    });
    state.overlay_selections.clear();
    let mut tasks = close_all_overlays(state);
    let Some(picked) = picked else {
        clear_pending_capture_state(state);
        return Task::batch(tasks);
    };
    state.pending_intent = Some(crate::app::CaptureIntent::Editor);
    state.pending_display_id = Some(picked.display_id.clone());
    state.pending_display_scale = Some(display.as_ref().map_or(1.0, |d| d.scale));
    state.pending_display_bounds = display.map(|d| (d.origin_x, d.origin_y, d.width, d.height));
    state.capture_in_flight = true;
    state.last_capture_status = None;
    let coord = state.coordinator.clone();
    // The window as macOS shows it, shadow included (the CLI's default).
    let req = readshot_capture::WindowCaptureRequest {
        window_id,
        ignore_shadows: false,
    };
    tasks.push(Task::perform(
        async move { coord.capture_window(req).await },
        |r| Message::Capture(CaptureMsg::RegionCaptured(r.map_err(|e| e.to_string()))),
    ));
    Task::batch(tasks)
}

/// `CaptureMsg::OverlayShiftChanged`.
pub(super) fn on_overlay_shift_changed(state: &mut App, held: bool) -> Task<Message> {
    // No-op when state didn't actually flip — keeps the redraw
    // path from firing on every key event.
    if state.overlay_shift_held == held {
        return Task::none();
    }
    state.overlay_shift_held = held;
    // The overlay rebuilds OverlayProgram from `state` on every
    // view(); the state update itself schedules the next view,
    // so this does not need the animation tick to be active.
    Task::none()
}

/// `CaptureMsg::OpenOverlayRequested`.
pub(super) fn on_open_overlay(state: &mut App) -> Task<Message> {
    // An Add Capture whose overlay can't open gives its hidden
    // editor back at once: whenever this request opens nothing and
    // no overlay is up or on its way.
    let overlay_up =
        state.overlay_opening || state.windows.kinds().any(|k| k == WindowKind::Overlay);
    let refused = state.welcome.should_show()
        || state.capture_in_flight
        || state.scroll_session.is_some()
        || overlay_up;
    // Never once the combine's own capture is in flight (its
    // selection was made): see `abort_combine_capture`.
    let restore = if refused && !overlay_up && !combine_selection_made(state) {
        let why = if state.capture_in_flight || state.scroll_session.is_some() {
            "Couldn’t add the capture — another capture is in progress."
        } else {
            "Couldn’t add the capture. Try again."
        };
        abort_combine_capture(state, Some(why))
    } else {
        Task::none()
    };
    if state.welcome.should_show() || state.capture_in_flight || state.overlay_opening {
        return restore;
    }
    // A scroll capture is running or stitching: starting another
    // would overwrite its session and orphan its windows.
    if state.scroll_session.is_some() {
        return Task::batch([restore, focus_scroll_hud(state)]);
    }
    // Don't stack overlays.
    if state.windows.kinds().any(|k| k == WindowKind::Overlay) {
        return Task::none();
    }
    // List displays asynchronously; the result drives the
    // actual window-open work in `OverlayDisplaysListed` so we
    // can spawn one transparent overlay per monitor.
    let coord = state.coordinator.clone();
    // Cheap refresh so a scroll session started from this
    // overlay honours a reduce-motion toggle the user flipped
    // since launch.
    state.reduce_motion = os_reduce_motion();
    state.overlay_opening = true;
    Task::perform(async move { coord.list_displays().await }, |result| {
        Message::Capture(CaptureMsg::OverlayDisplaysListed(
            result.map_err(|e| e.to_string()),
        ))
    })
}

/// `CaptureMsg::RetakeLastRegionRequested`.
pub(super) fn on_retake_last_region(state: &mut App) -> Task<Message> {
    if state.welcome.should_show() {
        return show_or_focus_welcome(state);
    }
    // An Add Capture is choosing its region: a retake would land
    // in that editor instead.
    if state.capture_in_flight || state.combine_capture.is_some() {
        return Task::none();
    }
    if state.scroll_session.is_some() {
        return focus_scroll_hud(state);
    }
    let Some((display_id, last)) = state
        .last_region_display_id
        .as_ref()
        .and_then(|display_id| {
            state
                .last_regions
                .get(display_id)
                .map(|last| (display_id.clone(), *last))
        })
    else {
        state.last_capture_status = Some(Status::new(
            StatusTone::Warning,
            "No previous region to retake. Capture a region first, then use Retake Last Region.",
        ));
        return Task::none();
    };

    state
        .pending_intent
        .get_or_insert(crate::app::CaptureIntent::Editor);
    state.pending_display_id = Some(display_id.clone());
    state.pending_display_scale = Some(last.display_scale);
    state.pending_display_bounds = last.display_bounds;
    update(
        state,
        Message::Capture(CaptureMsg::RegionRequested {
            display_id,
            rect: last.rect,
        }),
    )
}

/// `CaptureMsg::OverlayDisplaysListed(Err(..))`.
pub(super) fn on_displays_list_failed(state: &mut App, e: String) -> Task<Message> {
    state.overlay_opening = false;
    clear_pending_capture_state(state);
    tracing::warn!(target: readshot_core::log::cat::CAPTURE, "could not list displays: {e}");
    state.last_capture_status = Some(Status::new(
        StatusTone::Error,
        history_ops::friendly_capture_error(&e),
    ));
    let restore = abort_combine_capture(state, Some(&combine_capture_failed_status(&e)));
    if state.cli_interactive_output.is_some() {
        Task::batch([
            restore,
            Task::done(Message::Capture(CaptureMsg::CliInteractiveWritten(Err(
                format!("could not list displays: {e}"),
            )))),
        ])
    } else {
        restore
    }
}

/// `CaptureMsg::OverlayDisplaysListed(Ok(..))`: open one overlay per display.
pub(super) fn on_displays_listed(
    state: &mut App,
    displays: Vec<readshot_capture::DisplayInfo>,
) -> Task<Message> {
    state.overlay_opening = false;
    if displays.is_empty() {
        clear_pending_capture_state(state);
        tracing::warn!(target: readshot_core::log::cat::CAPTURE, "no displays detected");
        state.last_capture_status = Some(Status::new(
            StatusTone::Error,
            "Couldn’t capture — no display was detected.",
        ));
        let restore = abort_combine_capture(
            state,
            Some("Couldn’t add the capture — no display was detected."),
        );
        return if state.cli_interactive_output.is_some() {
            Task::batch([
                restore,
                Task::done(Message::Capture(CaptureMsg::CliInteractiveWritten(Err(
                    "no displays detected".into(),
                )))),
            ])
        } else {
            restore
        };
    }
    // Spawn one borderless transparent overlay per display,
    // positioned at that display's *global* logical origin.
    // We register the (window_id, display) mapping eagerly so
    // `view()` and the OverlaySelected message handler can
    // both look up which monitor a given overlay covers.
    let mut tasks: Vec<Task<Message>> = Vec::with_capacity(displays.len());
    // Fresh overlay session — clear any leftover toolbar state
    // from a previous capture flow.
    state.overlay_selections.clear();
    state
        .pending_intent
        .get_or_insert(crate::app::CaptureIntent::Editor);
    for d in &displays {
        let settings = overlay_window_settings_for(d);
        let (id, open_task) = window::open(settings);
        state.windows.register(id, WindowKind::Overlay);
        state.overlay_displays.insert(
            id,
            crate::app::OverlayDisplay {
                display_id: d.id.clone(),
                scale: d.scale,
                origin_x: d.bounds.x(),
                origin_y: d.bounds.y(),
                width: d.bounds.width(),
                height: d.bounds.height(),
            },
        );
        tasks.push(open_task.map(|a| Message::Capture(CaptureMsg::OverlayWindowReady(a))));
    }
    Task::batch(tasks)
}

/// `CaptureMsg::OverlaySelected`: a region was chosen.
pub(super) fn on_overlay_selected(
    state: &mut App,
    display_id: readshot_capture::DisplayId,
    rect: readshot_core::geom::Rect,
    intent: crate::app::CaptureIntent,
) -> Task<Message> {
    // Close every overlay window — selection on any one of
    // them ends the multi-monitor session — before snapping
    // the screenshot so the dimming veil doesn't show up in
    // the captured pixels.
    let overlay_record = state
        .overlay_displays
        .values()
        .find(|d| d.display_id == display_id);
    let display_scale = overlay_record.map(|d| d.scale).unwrap_or(1.0);
    let display_bounds = overlay_record.map(|d| (d.origin_x, d.origin_y, d.width, d.height));
    state.last_regions.insert(
        display_id.clone(),
        crate::app::LastRegion {
            rect,
            display_scale,
            display_bounds,
        },
    );
    state.last_region_display_id = Some(display_id.clone());
    if let Some(tray) = state.tray.as_ref() {
        tray.set_retake_last_region_enabled(true);
    }
    // An Add Capture's overlay always returns to its editor, even
    // if another flow overwrote the pending intent meanwhile: the
    // hidden editor must never be stranded.
    let intent = if let Some(cc) = state.combine_capture.as_mut() {
        cc.selected = true;
        crate::app::CaptureIntent::CombineIntoEditor
    } else {
        intent
    };
    state.pending_intent = Some(intent);
    state.pending_display_id = Some(display_id.clone());
    state.pending_display_scale = Some(display_scale);
    state.pending_display_bounds = display_bounds;
    state.overlay_selections.clear();
    let mut tasks = close_all_overlays(state);
    // Scroll capture: skip the synchronous first-frame capture
    // and open the HUD + region indicator immediately. The
    // timer tick captures frame 1 on its next fire, ~120 ms
    // later, so the HUD shows up instantly instead of waiting
    // for the ~100-200 ms ScreenCaptureKit round-trip.
    if matches!(intent, crate::app::CaptureIntent::ScrollCapture) {
        let Some(bounds) = display_bounds else {
            // We set pending_intent/display_id/display_scale/
            // display_bounds above; a scroll capture with no known
            // display bounds can't proceed, so clear that state
            // instead of leaving it dangling for the next flow.
            clear_pending_capture_state(state);
            return Task::batch(tasks);
        };
        let session = crate::app::ScrollSession {
            display_id: display_id.clone(),
            rect,
            scale: display_scale,
            frames: Vec::new(),
            no_motion_count: 0,
            hud_window_id: None,
            region_window_id: None,
            display_size: Some(bounds),
            capture_in_flight: 0,
            next_capture_seq: 0,
            next_frame_seq_to_process: 1,
            pending_frames: std::collections::BTreeMap::new(),
            judge_in_flight: None,
            stopping: false,
            started_at: std::time::Instant::now(),
            last_frame_at: None,
            frame_tick: 0,
            preview_strip: None,
            preview_handle: None,
            stitched_rows: 0,
            last_frame_rows_scaled: 0,
            stitch_frame_count: 0,
            cancel_armed_at: None,
            recent_join_heights: std::collections::VecDeque::new(),
        };
        state.reduce_motion = os_reduce_motion();
        state.scroll_session = Some(session);
        // The session carries its own display; the pending
        // fields are for a one-shot region capture and would be
        // stale for whatever capture comes next.
        clear_pending_capture_state(state);
        let (region_id, region_open) = window::open(scroll_region_window_settings(bounds));
        state.windows.register(region_id, WindowKind::ScrollRegion);
        if let Some(s) = state.scroll_session.as_mut() {
            s.region_window_id = Some(region_id);
        }
        tasks.push(region_open.map(|a| Message::Scroll(ScrollMsg::RegionWindowReady(a))));
        let (hud_pos, hud_size) = scroll_window_frame(rect, Some(bounds));
        let (hud_id, hud_open) = window::open(scroll_hud_window_settings(hud_pos, hud_size));
        state.windows.register(hud_id, WindowKind::ScrollHud);
        tasks.push(hud_open.map(|a| Message::Scroll(ScrollMsg::HudWindowReady(a))));
        return Task::batch(tasks);
    }
    tasks.push(Task::done(Message::Capture(CaptureMsg::RegionRequested {
        display_id,
        rect,
    })));
    Task::batch(tasks)
}

/// `CaptureMsg::OverlaySelectionChanged`.
pub(super) fn on_overlay_selection_changed(
    state: &mut App,
    display_id: readshot_capture::DisplayId,
    rect: Option<readshot_core::geom::Rect>,
) -> Task<Message> {
    match rect {
        Some(r) => {
            state.overlay_selections.insert(display_id, r);
        }
        None => {
            state.overlay_selections.remove(&display_id);
        }
    }
    Task::none()
}

/// `CaptureMsg::OverlayCancelled`.
pub(super) fn on_overlay_cancelled(state: &mut App) -> Task<Message> {
    state.overlay_selections.clear();
    state.overlay_opening = false;
    // A stray cancel after a combine's selection must leave its
    // in-flight capture's display scale and cursor choice alone.
    if !combine_selection_made(state) {
        clear_pending_capture_state(state);
    }
    let restore = abort_combine_capture(state, None);
    if state.cli_interactive_output.is_some() {
        state.cli_interactive_output = None;
        return Task::batch(
            close_all_overlays(state)
                .into_iter()
                .chain([restore, iced::exit()]),
        );
    }
    Task::batch(
        close_all_overlays(state)
            .into_iter()
            .chain(std::iter::once(restore)),
    )
}

/// `CaptureMsg::RegionRequested`.
pub(super) fn on_capture_region(
    state: &mut App,
    display_id: readshot_capture::DisplayId,
    rect: readshot_core::geom::Rect,
) -> Task<Message> {
    let coord = state.coordinator.clone();
    let hide_cursor = state.pending_hide_cursor;
    let display_scale = state.pending_display_scale;
    state.capture_in_flight = true;
    state.last_capture_status = None;
    // Region capture lands in the editor instead of saving
    // directly — the editor decides what to do with it.
    Task::perform(
        async move {
            match display_scale {
                Some(scale) => {
                    capture_region_to_image_with_scale(coord, display_id, rect, scale, hide_cursor)
                        .await
                }
                None => capture_region_to_image(coord, display_id, rect, hide_cursor).await,
            }
        },
        |result| {
            Message::Capture(CaptureMsg::RegionCaptured(
                result.map_err(|e| e.to_string()),
            ))
        },
    )
}

/// `CaptureMsg::RegionCaptured`: act on the captured region.
pub(super) fn on_region_captured(
    state: &mut App,
    result: Result<image::RgbaImage, String>,
) -> Task<Message> {
    state.capture_in_flight = false;
    let intent = state
        .pending_intent
        .take()
        .unwrap_or(crate::app::CaptureIntent::Editor);
    let display_id = state.pending_display_id.take().unwrap_or_default();
    let display_scale = state.pending_display_scale.take().unwrap_or(1.0);
    let display_bounds = state.pending_display_bounds.take();
    state.pending_hide_cursor = true;
    // Add Capture: back to the same editor, no History record.
    if intent == crate::app::CaptureIntent::CombineIntoEditor || state.combine_capture.is_some() {
        return match result {
            Ok(image) => finish_combine_capture(state, image),
            Err(e) => {
                tracing::warn!(target: readshot_core::log::cat::CAPTURE, "region capture failed: {e}");
                let status = combine_capture_failed_status(&e);
                fail_combine_capture(state, Some(&status))
            }
        };
    }
    match result {
        Ok(image) => {
            let history_record = history_record_for_capture(
                &image,
                &display_id,
                state.preferences.history_retention,
            );
            // One OCR per capture: when history is on (a record
            // exists) its task OCRs the same base image, so an
            // editor opened for this capture shares that result.
            // Its session id is allocated up front so the history
            // task can tag the result before the editor exists.
            let shared_session = (intent == crate::app::CaptureIntent::Editor
                && history_record.is_some())
            .then(crate::editor::next_session_id);
            // Persist a history record in parallel with the
            // user-visible intent action. The history task is
            // gated on `preferences.history_retention` and
            // logs internally on failure, so it can never
            // block or fail the user-visible flow.
            let history_task = if intent == crate::app::CaptureIntent::CliInteractive
                || history_record.is_none()
            {
                Task::none()
            } else {
                persist_history_task(
                    state.coordinator.clone(),
                    image.clone(),
                    history_record.clone(),
                    state.preferences.history_retention,
                    state.preferences.clone(),
                    shared_session,
                )
            };
            let intent_task = match intent {
                crate::app::CaptureIntent::Editor => {
                    let editor =
                        new_capture_editor(image, history_record, shared_session, display_scale);
                    open_editor_window_replacing(state, editor, display_bounds)
                }
                crate::app::CaptureIntent::CopyToClipboard => {
                    Task::perform(copy_image_to_clipboard(image), |r| {
                        Message::Capture(CaptureMsg::OverlayCopyDone(r.map_err(|e| e.to_string())))
                    })
                }
                crate::app::CaptureIntent::CopyTextDirect => {
                    let coord = state.coordinator.clone();
                    let prefs = state.preferences.clone();
                    let mask = editor_ops::CopyAllMask::None;
                    Task::perform(ocr_then_copy(coord, image, prefs, false, mask), |r| {
                        Message::Capture(CaptureMsg::OverlayCopyTextDone(
                            r.map_err(|e| e.to_string()),
                        ))
                    })
                }
                crate::app::CaptureIntent::SaveDirect => {
                    let seed = preferred_save_seed_dir(&state.preferences, &state.last_save_dir);
                    let template = state.preferences.filename_template.clone();
                    let format = state.preferences.default_format;
                    Task::perform(save_image_via_picker(image, seed, template, format), |r| {
                        Message::Capture(CaptureMsg::OverlaySaveDone(r.map_err(|e| e.to_string())))
                    })
                }
                crate::app::CaptureIntent::Pin => {
                    let size = (image.width(), image.height());
                    let handle = rgba_handle(image);
                    let (id, open_task) = window::open(pin_window_settings(size, display_bounds));
                    state.windows.register(id, WindowKind::Pin);
                    state
                        .pins
                        .insert(id, crate::app::PinState::new(handle.clone()));
                    open_task.map(move |opened| {
                        Message::Editor(EditorMsg::Pin(PinMsg::WindowReady(opened, handle.clone())))
                    })
                }
                crate::app::CaptureIntent::CliInteractive => {
                    let Some(output) = state.cli_interactive_output.take() else {
                        return Task::done(Message::Capture(CaptureMsg::CliInteractiveWritten(
                            Err("missing CLI interactive output path".into()),
                        )));
                    };
                    Task::perform(write_cli_interactive_capture(image, output), |r| {
                        Message::Capture(CaptureMsg::CliInteractiveWritten(
                            r.map_err(|e| e.to_string()),
                        ))
                    })
                }
                crate::app::CaptureIntent::CombineIntoEditor => {
                    unreachable!("handled above")
                }
                crate::app::CaptureIntent::ScrollCapture => {
                    // Start a new scrolling-capture session.
                    // The first frame is the just-captured `image`;
                    // subsequent frames come from the timer-tick loop.
                    // `last_regions` was populated by OverlaySelected just
                    // before this branch ran, so the lookup is safe.
                    let Some(last) = state.last_regions.get(&display_id).copied() else {
                        state.last_capture_status = Some(Status::new(
                            StatusTone::Error,
                            format!(
                                "Couldn’t start the scrolling capture — the selected area was lost. {}",
                                scroll::SCROLL_RETRY_HINT
                            ),
                        ));
                        return Task::batch([history_task]);
                    };
                    let mut session = crate::app::ScrollSession::new(
                        display_id.clone(),
                        last.rect,
                        display_scale,
                    );
                    let rows = preview_rows(&image, None);
                    accept_scroll_frame(&mut session, image, Some(rows), None);
                    session.display_size = display_bounds;
                    state.reduce_motion = os_reduce_motion();
                    state.scroll_session = Some(session);
                    // Open the click-through region indicator
                    // first so the user sees the rect outline
                    // anchored to the page they're scrolling.
                    // The HUD opens beside it for stop/cancel.
                    let mut tasks: Vec<Task<Message>> = Vec::new();
                    if let Some(bounds) = display_bounds {
                        let (region_id, region_open) =
                            window::open(scroll_region_window_settings(bounds));
                        state.windows.register(region_id, WindowKind::ScrollRegion);
                        if let Some(s) = state.scroll_session.as_mut() {
                            s.region_window_id = Some(region_id);
                        }
                        tasks.push(
                            region_open.map(|a| Message::Scroll(ScrollMsg::RegionWindowReady(a))),
                        );
                    }
                    let (hud_pos, hud_size) = scroll_window_frame(last.rect, display_bounds);
                    let (hud_id, hud_open) =
                        window::open(scroll_hud_window_settings(hud_pos, hud_size));
                    state.windows.register(hud_id, WindowKind::ScrollHud);
                    tasks.push(hud_open.map(|a| Message::Scroll(ScrollMsg::HudWindowReady(a))));
                    Task::batch(tasks)
                }
            };
            Task::batch([history_task, intent_task])
        }
        Err(e) => {
            tracing::warn!(target: readshot_core::log::cat::CAPTURE, "region capture failed: {e}");
            state.last_capture_status = Some(Status::new(
                StatusTone::Error,
                history_ops::friendly_capture_error(&e),
            ));
            if state.cli_interactive_output.is_some() {
                Task::done(Message::Capture(CaptureMsg::CliInteractiveWritten(Err(e))))
            } else {
                Task::none()
            }
        }
    }
}

/// The editor for a new capture: backed by its History record when
/// History keeps one, and sharing that record's OCR (`shared_session`)
/// so the Text tab doesn't start a second one.
pub(super) fn new_capture_editor(
    image: image::RgbaImage,
    history_record: Option<readshot_core::CaptureRecord>,
    shared_session: Option<u64>,
    display_scale: f32,
) -> crate::editor::EditorSession {
    let mut editor = match history_record {
        Some(record) => crate::editor::EditorSession::from_history_with_display_scale(
            image,
            record,
            display_scale,
        ),
        None => crate::editor::EditorSession::new_with_display_scale(image, display_scale),
    };
    // Mark the panel Recognizing so `request_text_recognition` doesn't
    // start a second OCR.
    if let Some(session) = shared_session {
        editor.session_id = session;
        editor.text_panel.recognition = crate::editor::TextRecognition::Recognizing {
            since: std::time::Instant::now(),
        };
        editor.text_panel.recognizing_part = Some(readshot_core::document::ORIGINAL_PART);
    }
    editor
}

/// `CaptureMsg::OverlayCopyDone`.
pub(super) fn on_overlay_copy_done(state: &mut App, result: Result<(), String>) -> Task<Message> {
    state.last_capture_status = Some(match result {
        Ok(()) => Status::new(StatusTone::Success, "Copied image to clipboard."),
        Err(e) => {
            tracing::warn!(target: readshot_core::log::cat::CAPTURE, "copy image failed: {e}");
            Status::new(
                StatusTone::Error,
                history_ops::friendly_string_error_with(
                    "copy to the clipboard",
                    &e,
                    Some(history_ops::RETRY_HINT),
                ),
            )
        }
    });
    Task::none()
}

/// `CaptureMsg::OverlaySaveDone`.
pub(super) fn on_overlay_save_done(
    state: &mut App,
    result: Result<Option<std::path::PathBuf>, String>,
) -> Task<Message> {
    if let Ok(Some(path)) = &result {
        if let Some(parent) = path.parent() {
            state.last_save_dir = Some(parent.to_path_buf());
        }
    }
    state.last_capture_status = Some(match result {
        Ok(Some(p)) => Status::new(StatusTone::Success, format!("Saved to {}.", p.display())),
        Ok(None) => Status::new(StatusTone::Notice, "Save cancelled."),
        Err(e) => {
            tracing::warn!(target: readshot_core::log::cat::CAPTURE, "save failed: {e}");
            Status::new(
                StatusTone::Error,
                history_ops::friendly_string_error_with(
                    "save the image",
                    &e,
                    Some(history_ops::SAVE_HINT),
                ),
            )
        }
    });
    Task::none()
}

/// `CaptureMsg::OverlayCopyTextDone`.
pub(super) fn on_overlay_copy_text_done(
    state: &mut App,
    result: Result<String, String>,
) -> Task<Message> {
    state.last_capture_status = Some(match result {
        // Nothing was copied: a refusal, raised like the other capture
        // problems when no window shows it.
        Ok(text) if text.is_empty() => Status::new(StatusTone::Warning, "No text recognized."),
        Ok(text) => {
            let n = text.chars().count();
            Status::new(
                StatusTone::Success,
                format!("Copied {} of text.", counted(n, "character", "characters")),
            )
        }
        Err(e) => {
            tracing::warn!(target: readshot_core::log::cat::CAPTURE, "copy text failed: {e}");
            Status::new(
                StatusTone::Error,
                history_ops::friendly_string_error_with(
                    "copy the text",
                    &e,
                    Some(history_ops::RETRY_HINT),
                ),
            )
        }
    });
    Task::none()
}

/// `CaptureMsg::CliInteractiveWritten`.
pub(super) fn on_cli_interactive_written(
    state: &mut App,
    result: Result<(), String>,
) -> Task<Message> {
    if let Err(e) = result {
        tracing::warn!(target: readshot_core::log::cat::CLI, "interactive capture failed: {e}");
        record_cli_interactive_failure(&e);
    }
    state.cli_interactive_output = None;
    state.overlay_opening = false;
    clear_pending_capture_state(state);
    iced::exit()
}

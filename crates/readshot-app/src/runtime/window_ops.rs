//! Window lifecycle and the editor window's pointer, modifier and
//! size messages, plus the router into the editor handler.
//! Moved out of `update_message`; `use super::*` keeps the arms'
//! names resolving as they did there.

use super::*;

/// Every [`WindowMsg`].
pub(super) fn handle_window_message(state: &mut App, message: WindowMsg) -> Task<Message> {
    match message {
        WindowMsg::CloseRequested(id) => on_window_close_requested(state, id),
        WindowMsg::Closed(id) => on_window_closed(state, id),
        WindowMsg::Rescaled(id, scale) => on_window_rescaled(state, id, scale),
        WindowMsg::EditorPointerButton(pressed, window) => {
            on_editor_pointer_button(state, pressed, window)
        }
        WindowMsg::EditorPointerWindowUnfocused(id) => {
            on_editor_pointer_window_unfocused(state, id)
        }
        WindowMsg::EditorShiftChanged(held) => on_editor_shift_changed(state, held),
        // High-frequency editor events: handled here, not in the editor
        // arm, so they never clear the canvas geometry cache.
        WindowMsg::EditorCommandChanged(held) => on_editor_command_changed(state, held),
        // Text-panel hover fires per pointer move; its outline is drawn in
        // the canvas's uncached marks layer, so the annotation geometry
        // cache stays valid.
        WindowMsg::EditorTextLineHovered(index) => on_text_line_hovered(state, index),
        WindowMsg::EditorTextLineUnhovered(index) => on_text_line_unhovered(state, index),
        WindowMsg::EditorWindowResized(window, width) => {
            on_editor_window_resized(state, window, width)
        }
        // Only redraws the recognizing skeleton.
        WindowMsg::EditorPanelTick => Task::none(),
        WindowMsg::DragRequested(id) => window::drag(id),
        // Bookkeeping only (no history, no settle): what the stage shows.
        WindowMsg::EditorStageScrolled(x, y) => on_stage_scrolled(state, x, y),
    }
}

/// `WindowMsg::CloseRequested`.
pub(super) fn on_window_close_requested(state: &mut App, id: iced::window::Id) -> Task<Message> {
    // Only the editor window opts out of auto-close. If the
    // close request is for it, route through the same two-stage
    // unsaved-work confirmation as the in-app Discard button:
    // clean editors (and the confirmed second request) close
    // immediately; a dirty editor arms the confirmation and
    // stays open. Every other window uses iced's default
    // auto-close, so there is nothing to do here.
    let is_editor = state
        .editor
        .as_ref()
        .and_then(|ed| ed.window_id)
        .is_some_and(|editor_id| editor_id == id);
    if is_editor {
        return update(state, Message::Editor(EditorMsg::DiscardRequested));
    }
    Task::none()
}

/// `WindowMsg::Closed`.
pub(super) fn on_window_closed(state: &mut App, id: iced::window::Id) -> Task<Message> {
    // A press that began in the closed window never gets its
    // release.
    if let Some(ed) = state.editor.as_mut() {
        ed.pointer_window_gone(id);
    }
    // Always drop the id from `Windows` so helpers that
    // probe the live-window map (e.g. `show_or_focus_welcome`)
    // don't try to focus a dead window.
    state.windows.forget(id);
    state.overlay_displays.remove(&id);
    // The last overlay went away without a selection or a cancel
    // (closed by the OS): an Add Capture waiting on it gives its
    // hidden editor back. A confirmed selection marked the combine
    // (it ends when its capture completes), so that capture is left
    // to finish.
    let restore = if state.combine_capture.is_some()
        && !state.overlay_opening
        && !state.capture_in_flight
        && !combine_selection_made(state)
        && !state.windows.kinds().any(|k| k == WindowKind::Overlay)
    {
        state.overlay_selections.clear();
        abort_combine_capture(state, None)
    } else {
        Task::none()
    };
    if state.history_window_id == Some(id) {
        state.history_window_id = None;
        state.history_records.clear();
        history_images::trim_history_image_cache();
        state.history_status = None;
        // An open from it that is still waiting is cancelled.
        history_ops::cancel_history_open(state, "History was closed before it opened");
        state.history_search.clear();
        state.history_selected_id = None;
    }
    if state.settings_window_id == Some(id) {
        state.settings_window_id = None;
        state.settings_recording_hotkey = false;
        state.settings_window_hotkey_error = None;
        state.settings_hotkey_error = None;
        state.settings_hotkey_status = None;
        state.settings_status = None;
        state.settings_reset_all_pending = false;
    }
    if state.cli_tools_window_id == Some(id) {
        state.cli_tools_window_id = None;
        state.cli_tools_status = None;
    }
    let restore = match state.editor.take_if(|ed| ed.window_id == Some(id)) {
        Some(ed) => {
            // Never composes here: a step still composing finishes
            // first, off the UI thread.
            let retired = compose_jobs::retire_editor(state, ed);
            state.release_editor_modifiers();
            Task::batch([restore, retired])
        }
        None => restore,
    };
    // A scroll-capture HUD or region window closing out-of-band
    // (OS, crash, future close affordance) must tear the whole
    // session down. Otherwise `scroll_session` stays `Some` and
    // the frame-capture tick — gated on `scroll_session.is_some()`
    // — keeps firing forever against dead window ids.
    if state
        .scroll_session
        .as_ref()
        .is_some_and(|s| s.hud_window_id == Some(id) || s.region_window_id == Some(id))
    {
        if let Some(session) = state.scroll_session.take() {
            let mut tasks: Vec<Task<Message>> = Vec::new();
            for sibling in [session.hud_window_id, session.region_window_id]
                .into_iter()
                .flatten()
                .filter(|&w| w != id)
            {
                state.windows.forget(sibling);
                tasks.push(window::close(sibling));
            }
            state.last_capture_status = Some(Status::new(
                StatusTone::Notice,
                "Scrolling capture cancelled.",
            ));
            tasks.push(restore);
            return Task::batch(tasks);
        }
    }
    restore
}

/// `WindowMsg::Rescaled`.
pub(super) fn on_window_rescaled(
    state: &mut App,
    id: iced::window::Id,
    scale: f32,
) -> Task<Message> {
    if matches!(state.windows.kind(id), Some(WindowKind::Editor)) {
        if let Some(ed) = state.editor.as_mut() {
            let was_actual = ed.zoom_is_actual_size();
            ed.display_scale = scale.max(f32::EPSILON);
            if was_actual {
                ed.zoom = ed.actual_size_zoom();
            }
            // The stage relaid out without a scroll event.
            return view::pin_stage_offset(ed);
        }
    }
    Task::none()
}

/// `WindowMsg::EditorShiftChanged`.
pub(super) fn on_editor_shift_changed(state: &mut App, held: bool) -> Task<Message> {
    // Only react on an actual change so the per-keystroke modifier
    // stream doesn't force a canvas re-tessellation each key.
    if state.editor_shift_held != held {
        state.editor_shift_held = held;
        if let Some(ed) = state.editor.as_ref() {
            // Re-tessellate so a live drag preview reflects the new
            // constraint on the next redraw.
            ed.canvas_cache.clear();
        }
    }
    Task::none()
}

/// `WindowMsg::EditorCommandChanged`.
pub(super) fn on_editor_command_changed(state: &mut App, held: bool) -> Task<Message> {
    // Only the Text panel's ⌘-click reads it; nothing to redraw.
    if state.editor_command_held != held {
        state.editor_command_held = held;
    }
    Task::none()
}

/// `WindowMsg::EditorWindowResized`.
pub(super) fn on_editor_window_resized(
    state: &mut App,
    window: iced::window::Id,
    width: f32,
) -> Task<Message> {
    let mut docked_overlay = false;
    if let Some(ed) = state.editor.as_mut() {
        if ed.window_id == Some(window) {
            ed.window_width = width;
            // An overlay widened into the dock is now the docked
            // panel: fold it into the preference (as ⌥⌘P does)
            // so no stale overlay flag is left to eat an Esc.
            if width >= PANEL_DOCK_MIN_WINDOW && ed.panel_overlay_open {
                ed.panel_overlay_open = false;
                docked_overlay = true;
            }
        }
    }
    if docked_overlay {
        state.set_editor_panel_open(true);
    }
    // The stage relaid out without a scroll event.
    match state.editor.as_ref() {
        Some(ed) if ed.window_id == Some(window) => view::pin_stage_offset(ed),
        _ => Task::none(),
    }
}

/// An editor message: settle what it needs, run the editor handler,
/// then the History follow-ups.
pub(super) fn route_editor_message(state: &mut App, msg: EditorMsg) -> Task<Message> {
    // Images the message lets go of are freed off the UI thread.
    let held = state.editor.as_ref().map(compose_jobs::hold);
    // The frame the canvas computed this message's points in: the
    // one the last view showed (a step settled now, or a change
    // handled earlier in the same batch, may have moved it).
    let shown = state.editor.as_ref().map(|ed| ed.shown_frame());
    compose_jobs::settle_before(state, &msg);
    // A canvas point computed against another frame than the
    // present one is mapped into it, or the message is dropped.
    let msg = match (state.editor.as_mut(), shown) {
        (Some(ed), Some(shown))
            if compose_jobs::points_into_the_shown_image(&msg)
                && !crate::editor::same_frame(&shown, ed.model.geometry()) =>
        {
            compose_jobs::remap_stale_press(ed, &shown, msg)
        }
        (Some(ed), _) => compose_jobs::remap_same_batch_drag(ed, msg),
        (None, _) => Some(msg),
    };
    let task = match msg {
        Some(msg) => editor_ops::handle_editor_message(state, msg),
        None => Task::none(),
    };
    editor_ops::sync_owed_editor_history(state);
    if let Some(held) = held {
        held.release(state.editor.as_ref());
    }
    Task::batch([task, editor_ops::history_write_followups(state)])
}

/// `WindowMsg::EditorPointerButton`.
pub(super) fn on_editor_pointer_button(
    state: &mut App,
    pressed: bool,
    window: Option<iced::window::Id>,
) -> Task<Message> {
    if let Some(ed) = state.editor.as_mut() {
        ed.set_pointer_held(pressed.then_some(window));
    }
    Task::none()
}

/// `WindowMsg::EditorPointerWindowUnfocused`.
pub(super) fn on_editor_pointer_window_unfocused(
    state: &mut App,
    id: iced::window::Id,
) -> Task<Message> {
    if let Some(ed) = state.editor.as_mut() {
        ed.pointer_window_gone(id);
    }
    Task::none()
}

/// `WindowMsg::EditorTextLineHovered`.
pub(super) fn on_text_line_hovered(state: &mut App, index: usize) -> Task<Message> {
    if let Some(ed) = state.editor.as_mut() {
        ed.text_panel.hovered = Some(index);
    }
    Task::none()
}

/// `WindowMsg::EditorTextLineUnhovered`.
pub(super) fn on_text_line_unhovered(state: &mut App, index: usize) -> Task<Message> {
    if let Some(ed) = state.editor.as_mut() {
        if ed.text_panel.hovered == Some(index) {
            ed.text_panel.hovered = None;
        }
    }
    Task::none()
}

/// `WindowMsg::EditorStageScrolled`.
pub(super) fn on_stage_scrolled(state: &mut App, x: f32, y: f32) -> Task<Message> {
    if let Some(ed) = state.editor.as_ref() {
        ed.stage_offset.set((x, y));
    }
    Task::none()
}

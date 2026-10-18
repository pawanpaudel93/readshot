//! Editor chrome messages: the status pill, popovers and zoom.
//! Moved out of `handle_editor_message`; `use super::*` keeps the
//! arms' names resolving as they did there.

use super::*;

/// `Message::EditorStatusTick`: expire a stale status pill or key guide.
pub(super) fn on_status_tick(state: &mut App) -> Task<Message> {
    // Auto-dismiss stale, non-in-progress status pills *here* —
    // the authoritative place — so the `EditorStatusTick`
    // subscription (gated on `status.is_some() &&
    // !status_is_in_progress()`) actually stops firing once a
    // toast has expired. Previously the dismissal only happened
    // locally in `editor_view`, which can't clear `ed.status`, so
    // the 1 Hz tick + redraw ran for the entire life of the
    // editor window. An armed Discard is sticky: it waits for
    // Keep Editing, Discard or the next edit (WCAG 2.2.1).
    if let Some(ed) = state.editor.as_mut() {
        if !ed.status_is_in_progress()
            && !ed.status_is_sticky()
            && ed
                .status_set_at()
                .is_some_and(|t| t.elapsed() > crate::editor::STATUS_AUTO_DISMISS)
        {
            ed.clear_status();
        }
        // A key guide whose draft, zone or selection has ended
        // goes with it.
        if ed.key_guide_live() == Some(false) {
            ed.clear_status();
        }
    }
    Task::none()
}

/// `Message::EditorPopoverToggled`.
pub(super) fn on_popover_toggled(
    state: &mut App,
    which: crate::editor::EditorPopover,
) -> Task<Message> {
    if let Some(ed) = state.editor.as_mut() {
        if ed.popover == Some(which) {
            close_editor_popover(ed);
        } else {
            close_editor_popover(ed);
            ed.popover = Some(which);
            if which == crate::editor::EditorPopover::Color {
                ed.hex_draft = ed.model.effective_color().to_hex();
            }
        }
    }
    Task::none()
}

/// `Message::EditorZoomIn`.
pub(super) fn on_zoom_in(state: &mut App) -> Task<Message> {
    if let Some(ed) = state.editor.as_mut() {
        if ed.busy {
            return Task::none();
        }
        // Keyboard (⌘=) and the title-row "+" share this path. In
        // Fit mode the step starts from the scale the stage last
        // rendered at, resolved now: a view built before a window
        // resize would carry a stale scale.
        let next = ed.zoom.zoom_in_from_display_scale(ed.render_scale.get());
        ed.clear_eyedropper_loupe();
        return crate::runtime::view::zoom_keeping_center(ed, next);
    }
    Task::none()
}

/// `Message::EditorZoomOut`.
pub(super) fn on_zoom_out(state: &mut App) -> Task<Message> {
    if let Some(ed) = state.editor.as_mut() {
        if ed.busy {
            return Task::none();
        }
        // See `EditorZoomIn`.
        let next = ed.zoom.zoom_out_from_display_scale(ed.render_scale.get());
        ed.clear_eyedropper_loupe();
        return crate::runtime::view::zoom_keeping_center(ed, next);
    }
    Task::none()
}

/// `Message::EditorZoomActual`.
pub(super) fn on_zoom_actual(state: &mut App) -> Task<Message> {
    if let Some(ed) = state.editor.as_mut() {
        if ed.busy {
            return Task::none();
        }
        let next = ed.actual_size_zoom();
        ed.clear_eyedropper_loupe();
        return crate::runtime::view::zoom_keeping_center(ed, next);
    }
    Task::none()
}

/// `Message::EditorZoomFit`.
pub(super) fn on_zoom_fit(state: &mut App) -> Task<Message> {
    if let Some(ed) = state.editor.as_mut() {
        if ed.busy {
            return Task::none();
        }
        ed.clear_eyedropper_loupe();
        return crate::runtime::view::zoom_keeping_center(ed, crate::editor::EditorZoom::Fit);
    }
    Task::none()
}

/// `Message::EditorPopoverClosed`.
pub(super) fn on_popover_closed(state: &mut App) -> Task<Message> {
    if let Some(ed) = state.editor.as_mut() {
        close_editor_popover(ed);
    }
    Task::none()
}

/// `Message::EditorWindowReady`.
pub(super) fn on_window_ready(state: &mut App, id: iced::window::Id) -> Task<Message> {
    if let Some(ed) = &mut state.editor {
        ed.window_id = Some(id);
    }
    // The OS can open the window smaller than requested on small
    // screens; the real width decides dock vs collapse.
    window::size(id).map(move |size| Message::EditorWindowResized(id, size.width))
}

/// `Message::EditorDiscardDisarmed`: Keep Editing.
pub(super) fn on_discard_disarmed(state: &mut App) -> Task<Message> {
    if let Some(ed) = state.editor.as_mut() {
        ed.clear_discard_confirmation();
        ed.clear_status();
    }
    Task::none()
}

/// `Message::EditorStatusDismissed`.
pub(super) fn on_status_dismissed(state: &mut App) -> Task<Message> {
    if let Some(ed) = state.editor.as_mut() {
        ed.clear_status();
    }
    Task::none()
}

//! Editor keyboard messages: key presses, Tab cycling, the queued
//! field-key checks and arrow-key moves.
//! Moved out of `handle_editor_message`; `use super::*` keeps the
//! arms' names resolving as they did there.

use super::*;

/// `EditorMsg::KeyPressed`: a key the editor window saw.
pub(super) fn on_key_pressed(
    state: &mut App,
    window: iced::window::Id,
    key: iced::keyboard::Key,
    modifiers: iced::keyboard::Modifiers,
    status_ignored: bool,
) -> Task<Message> {
    let editor_window = state.editor.as_ref().and_then(|ed| ed.window_id);
    if editor_window != Some(window) {
        return Task::none();
    }
    // An Escape during a live canvas drag ends the drag only,
    // whichever of its two messages comes first.
    if let Some(ed) = state.editor.as_mut() {
        let escape = matches!(
            key,
            iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape)
        );
        let seen = ed.escape_ended_drag.take();
        if escape {
            if seen == Some(crate::editor::EscapeSeenBy::Canvas) {
                return Task::none(); // the canvas already spent it
            }
            // Captured by the canvas (`!status_ignored`): its
            // `DragEscaped` follows and must do nothing more.
            if !status_ignored && ed.canvas_drag_in_progress() {
                ed.canvas_drag_active = false;
                cancel_editor_previews(ed);
                ed.escape_ended_drag = Some(crate::editor::EscapeSeenBy::Key);
                return Task::none();
            }
        }
    }
    // An open popover owns Escape. In the running editor the
    // annotation canvas captures every Escape, so it arrives with
    // `!status_ignored` and `editor_key_message` maps it to
    // `EditorTextCancel`, which closes the popover first. This
    // branch only covers an Escape no widget captured, closing the
    // popover before it can fall through to discard.
    if status_ignored
        && matches!(
            key,
            iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape)
        )
    {
        if let Some(ed) = state.editor.as_mut() {
            if close_editor_popover(ed) || crate::runtime::text_panel::close_panel_overlay(ed) {
                return Task::none();
            }
        }
    }
    // Fallback for an Escape no widget captured during a live
    // shape/freehand drag: bump the cancel sequence so the canvas
    // drops the drag on its next event. (Normally the canvas captures
    // Escape itself and publishes `Cancelled`; Esc is never Discard.)
    if status_ignored
        && matches!(
            key,
            iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape)
        )
    {
        if let Some(ed) = state.editor.as_mut() {
            if ed.canvas_drag_active {
                ed.canvas_drag_active = false;
                ed.drag_cancel_seq = ed.drag_cancel_seq.wrapping_add(1);
                cancel_editor_previews(ed);
                return Task::none();
            }
        }
    }
    // The keyboard focus outside the canvas (Image menu, options
    // bar, side panel, Resize) gets the key first.
    if let Some(task) =
        crate::runtime::key_focus::handle_focus_key(state, &key, modifiers, status_ignored)
    {
        return task;
    }
    let message = editor_key_message(key, modifiers, status_ignored);
    // While a removed preset can still be put back, ⌘Z is its Undo
    // (even after a newer status replaced the toast).
    let restore_shown = state
        .editor
        .as_ref()
        .is_some_and(|ed| ed.preset_restore_pending());
    let message = message.map(|m| match m {
        Message::Editor(EditorMsg::Toolbar(readshot_ui::ToolbarMessage::Undo)) if restore_shown => {
            Message::Editor(EditorMsg::Beautify(BeautifyMsg::RestorePreset))
        }
        m => m,
    });
    // C from the keyboard: a crop draft to move and size with the
    // arrows, since the pointer is not there to drag one (WCAG
    // 2.1.1 / 2.5.7). The toolbar button still waits for a drag.
    let crop_key = message.as_ref().is_some_and(is_crop_key);
    // A key's field check is out: this key waits behind it, in
    // order (`EditorFieldKeyResolved` runs the queue).
    if let Some(ed) = state.editor.as_mut().filter(|ed| !ed.key_queue.is_empty()) {
        if let Some(message) = message {
            ed.key_queue.push_back(message);
        }
        return Task::none();
    }
    // While the Color or Resize popover is open its field may have
    // focus, an inline text draft owns its own field, and so does
    // the Beautify hex field while it holds a draft. iced's
    // text_input has no undo and doesn't capture Cmd+Z, nor Up/Down,
    // so undo/redo would silently rewind the drawing under the
    // field (or commit the draft and then undo it), and Up/Down
    // would move the selected annotation under the caret.
    if let Some(field_key) = message.as_ref().filter(|m| is_field_key(m)) {
        if field_owns_keys(state) {
            return Task::none();
        }
        // A click can leave the caret in the Beautify hex field
        // with nothing typed and no message: ask iced first.
        if beautify_hex_on_screen(state) {
            let field_key = field_key.clone();
            if let Some(ed) = state.editor.as_mut() {
                ed.key_queue.push_back(field_key.clone());
                return ask_hex_field(ed.session_id, field_key);
            }
        }
    }
    let task = match message {
        Some(message) => update(state, message),
        None => Task::none(),
    };
    if crop_key {
        keyboard_crop_follow_up(state);
    }
    task
}

/// `EditorMsg::NudgeSelected`: arrows move the selection or crop draft.
pub(super) fn on_nudge_selected(state: &mut App, dx: f32, dy: f32) -> Task<Message> {
    let Some(ed) = state.editor.as_mut() else {
        return Task::none();
    };
    if ed.busy {
        return Task::none();
    }
    // Don't hijack the keys when another input mode owns them:
    // while a text draft/edit is open the arrows belong to the
    // text caret (iced's single-line input ignores Up/Down, so
    // they'd otherwise reach here and move the annotation being
    // typed), and during an in-progress mouse move/resize drag a
    // nudge would cancel the drag mid-gesture.
    if ed.pending_text.is_some() || ed.move_drag.is_some() {
        return Task::none();
    }
    // An open crop draft owns the arrows: they move it (the
    // keyboard alternative to dragging it, WCAG 2.5.7).
    if let Some(draft) = ed.crop_draft {
        let frame = visible_frame(ed);
        ed.crop_draft = Some(move_crop_draft(draft, dx, dy, frame));
        return Task::none();
    }
    // Settle any in-flight preview first, then move the selection
    // by the keyboard step as one undoable edit. No status toast:
    // arrow presses are rapid and would spam the chrome.
    cancel_editor_previews(ed);
    if ed.model.nudge_selected(dx, dy) {
        edit_landed(ed, &state.coordinator, None);
    }
    Task::none()
}

/// `EditorMsg::WidthBump`: `[` / `]`.
pub(super) fn on_width_bump(state: &mut App, delta: f32) -> Task<Message> {
    if let Some(ed) = state.editor.as_mut() {
        if ed.busy {
            return Task::none();
        }
        // Text (selected, or the Text tool with nothing selected)
        // steps its size preset from the actual px; anything
        // else bumps the width the options bar shows.
        if let Some(px) = bump_text_px(&ed.model) {
            if let Some(next) = readshot_ui::editor::TextSize::step_from_px(px, delta > 0.0) {
                apply_editor_style(
                    ed,
                    &state.coordinator,
                    readshot_ui::editor::StyleMessage::TextSize(next),
                );
            }
        } else {
            let next = ed.model.effective_line_width() + delta;
            apply_editor_line_width(ed, &state.coordinator, next);
        }
    }
    Task::none()
}

/// `EditorMsg::ColorCycle`: `,` / `.`.
pub(super) fn on_color_cycle(state: &mut App, dir: i32) -> Task<Message> {
    if let Some(ed) = state.editor.as_mut() {
        if ed.busy {
            return Task::none();
        }
        let palette = readshot_ui::editor::toolbar::PALETTE;
        let current = ed.model.current_color();
        let idx = palette
            .iter()
            .position(|c| swatch_eq(*c, current))
            .unwrap_or(0) as i32;
        let len = palette.len() as i32;
        let next = ((idx + dir).rem_euclid(len)) as usize;
        apply_editor_color(ed, &state.coordinator, palette[next]);
    }
    Task::none()
}

/// `EditorMsg::CycleSelection`: Tab through the annotations.
pub(super) fn on_cycle_selection(state: &mut App, step: i32) -> Task<Message> {
    if !tab_may_cycle(state) {
        return Task::none();
    }
    // A click can leave the caret in the Beautify hex field without
    // any message reaching the app. While that field is on screen,
    // ask iced whether it has the caret (one round trip) before
    // moving the selection.
    if beautify_hex_on_screen(state) {
        // Collected, so a query no widget answers (the tab closed
        // before the next frame) still resolves, as "not focused":
        // a Tab is never dropped.
        return iced::widget::operation::is_focused(iced::widget::Id::new(
            crate::runtime::key_focus::BEAUTIFY_HEX_ID,
        ))
        .collect()
        .map(move |answers| {
            Message::Editor(EditorMsg::TabResolved {
                step,
                field_focused: tab_field_focused(&answers),
            })
        });
    }
    cycle_selection_by_tab(state, step)
}

/// `EditorMsg::FieldKeyResolved`: iced answered whether a field has the caret.
pub(super) fn on_field_key_resolved(
    state: &mut App,
    session: u64,
    message: Box<Message>,
    field_focused: bool,
) -> Task<Message> {
    // The answer belongs to the editor that asked; another one's
    // queue is never touched (the asker's went with it).
    let Some(ed) = state.editor.as_mut().filter(|ed| ed.session_id == session) else {
        return Task::none();
    };
    ed.key_queue.pop_front();
    let mut tasks = Vec::new();
    // A field may have taken the keys meanwhile (a key before
    // this one opened a draft or popover): then it is the field's.
    if !field_focused && is_field_key(&message) && !field_owns_keys(state) {
        tasks.push(update(state, *message));
    }
    // Then the keys that waited, in order, through the same guards
    // as a fresh key, until one needs its own field check.
    loop {
        let Some(next) = state
            .editor
            .as_mut()
            .filter(|ed| ed.session_id == session)
            .and_then(|ed| ed.key_queue.pop_front())
        else {
            break;
        };
        if is_field_key(&next) {
            if field_owns_keys(state) {
                continue;
            }
            if beautify_hex_on_screen(state) {
                if let Some(ed) = state.editor.as_mut() {
                    ed.key_queue.push_front(next.clone());
                }
                tasks.push(ask_hex_field(session, next));
                break;
            }
        }
        // As a fresh key: C from the keyboard opens a crop draft.
        let crop_key = is_crop_key(&next);
        tasks.push(update(state, next));
        if crop_key {
            keyboard_crop_follow_up(state);
        }
    }
    Task::batch(tasks)
}

/// `EditorMsg::TabResolved`.
pub(super) fn on_tab_resolved(state: &mut App, step: i32, field_focused: bool) -> Task<Message> {
    if field_focused {
        // Tab leaves the field; the selection stays.
        return crate::runtime::key_focus::unfocus_text_fields();
    }
    if !tab_may_cycle(state) {
        return Task::none();
    }
    cycle_selection_by_tab(state, step)
}

/// `EditorMsg::ResizeByKeys`: resize the selection or crop draft from the keyboard.
pub(super) fn on_resize_by_keys(state: &mut App, dx: f32, dy: f32) -> Task<Message> {
    let Some(ed) = state.editor.as_mut() else {
        return Task::none();
    };
    if ed.busy || ed.pending_text.is_some() || ed.move_drag.is_some() {
        return Task::none();
    }
    if let Some(draft) = ed.crop_draft {
        let frame = visible_frame(ed);
        let ratio = ed.model.effective_style().crop_aspect.ratio();
        ed.crop_draft = Some(resize_crop_draft(draft, dx, dy, ratio, frame));
        return Task::none();
    }
    let Some(handle) = ed
        .model
        .selected_annotation()
        .and_then(|i| ed.model.annotations().get(i))
        .and_then(keyboard_resize_handle)
    else {
        return Task::none();
    };
    cancel_editor_previews(ed);
    if ed.model.resize_selected_by(handle, dx, dy) {
        edit_landed(ed, &state.coordinator, None);
    }
    Task::none()
}

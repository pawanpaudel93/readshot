//! Editor text and selection edits: in-place text, crop apply, delete
//! and the Escape chain.
//! Moved out of `handle_editor_message`; `use super::*` keeps the
//! arms' names resolving as they did there.

use super::*;

/// `Message::EditorEditSelectedText`: ↵ on a selection or crop draft.
pub(super) fn on_edit_selected_text(state: &mut App) -> Task<Message> {
    // ↵ applies an open crop draft, unless a text field has focus
    // or a text draft is being typed (those own Enter).
    if state.editor.as_ref().is_some_and(|ed| {
        ed.crop_draft.is_some()
            && ed.pending_text.is_none()
            && !matches!(
                ed.popover,
                Some(crate::editor::EditorPopover::Color | crate::editor::EditorPopover::Resize)
            )
    }) {
        return handle_editor_message(state, Message::EditorCropApply);
    }
    let Some(ed) = state.editor.as_mut() else {
        return Task::none();
    };
    if ed.busy {
        return Task::none();
    }
    if let Some(edit) = ed.model.selected_text_edit() {
        cancel_editor_previews(ed);
        ed.clear_discard_confirmation();
        ed.begin_text_edit(crate::editor::PendingText {
            origin: edit.origin,
            content: edit.content,
            edit_index: Some(edit.index),
        });
    }
    Task::none()
}

/// `Message::EditorDeleteSelected`.
pub(super) fn on_delete_selected(state: &mut App) -> Task<Message> {
    let Some(ed) = state.editor.as_mut() else {
        return Task::none();
    };
    if ed.busy {
        return Task::none();
    }
    let deleting_pending_text = matches!(
        (
            ed.pending_text
                .as_ref()
                .and_then(|pending| pending.edit_index),
            ed.model.selected_annotation(),
        ),
        (Some(edit_index), Some(selected_index)) if edit_index == selected_index
    );
    cancel_editor_previews(ed);
    if deleting_pending_text {
        ed.clear_text_edit();
    }
    if ed.model.delete_selected_annotation() {
        edit_landed(
            ed,
            &state.coordinator,
            Some(format!("Deleted annotation. {UNDO_HINT}")),
        );
    }
    Task::none()
}

/// `Message::EditorTextChanged`.
pub(super) fn on_text_changed(state: &mut App, content: String) -> Task<Message> {
    if let Some(ed) = state.editor.as_mut() {
        if ed.busy {
            return Task::none();
        }
        if ed.pending_text.is_some() {
            ed.clear_discard_confirmation();
        }
        if let Some(pending) = ed.pending_text.as_mut() {
            pending.content = content.clone();
        }
        // Keep the multi-line backing store in sync so a
        // programmatic content change (e.g. tests) is reflected
        // if the inline editor re-renders.
        ed.text_edit_content = Some(iced::widget::text_editor::Content::with_text(&content));
    }
    Task::none()
}

/// `Message::EditorTextAction`.
pub(super) fn on_text_action(
    state: &mut App,
    action: iced::widget::text_editor::Action,
) -> Task<Message> {
    if let Some(ed) = state.editor.as_mut() {
        if ed.busy {
            return Task::none();
        }
        if let Some(content) = ed.text_edit_content.as_mut() {
            content.perform(action);
            // Normalise line endings so the model (render /
            // bounds) only ever sees `\n`, regardless of the
            // platform default the widget inserts.
            let text = content.text().replace("\r\n", "\n").replace('\r', "\n");
            if let Some(pending) = ed.pending_text.as_mut() {
                pending.content = text;
            }
            ed.clear_discard_confirmation();
        }
    }
    Task::none()
}

/// `Message::EditorTextCommit`.
pub(super) fn on_text_commit(state: &mut App) -> Task<Message> {
    let Some(ed) = state.editor.as_mut() else {
        return Task::none();
    };
    if ed.busy {
        return Task::none();
    }
    commit_pending_editor_text(ed, &state.coordinator);
    Task::none()
}

/// `Message::EditorCropApply`.
pub(super) fn on_crop_apply(state: &mut App) -> Task<Message> {
    let Some(ed) = state.editor.as_mut() else {
        return Task::none();
    };
    if ed.busy {
        return Task::none();
    }
    // Same order as `image_menu::prepare`: end slider/drag
    // previews, then commit any pending text, then the crop.
    cancel_editor_previews(ed);
    commit_pending_editor_text(ed, &state.coordinator);
    apply_crop_draft(ed, &state.coordinator);
    Task::none()
}

/// `Message::EditorTextCancel`: one step of the Escape chain.
pub(super) fn on_text_cancel(state: &mut App) -> Task<Message> {
    if let Some(ed) = state.editor.as_mut() {
        if close_editor_popover(ed) {
            return Task::none();
        }
        if crate::runtime::text_panel::close_panel_overlay(ed) {
            return Task::none();
        }
        // Esc chain: popover → overlay panel → eyedropper → crop draft → slider →
        // text draft → selection → panel lines.
        if ed.eyedropper.take().is_some() {
            return Task::none();
        }
        if ed.crop_draft.take().is_some() {
            return Task::none();
        }
        if ed.busy {
            return Task::none();
        }
        // One Esc = one step: a live options-bar slider drag is
        // cancelled on its own, keeping the selection (and the bar).
        if slider_preview_live(ed) {
            cancel_editor_previews(ed);
            return Task::none();
        }
        // Last step of the chain: a Text-panel line selection,
        // once the canvas has nothing left to cancel.
        if ed.pending_text.is_none()
            && ed.model.selected_annotation().is_none()
            && !ed.text_panel.selected.is_empty()
        {
            ed.text_panel.selected.clear();
            ed.text_panel.anchor = None;
            return Task::none();
        }
        let keep_selection = ed
            .pending_text
            .as_ref()
            .is_some_and(|pending| pending.edit_index.is_some());
        cancel_editor_previews(ed);
        if !keep_selection {
            ed.model.clear_selection();
        }
        ed.clear_text_edit();
    }
    Task::none()
}

/// `Message::EditorCropCancel`.
pub(super) fn on_crop_cancel(state: &mut App) -> Task<Message> {
    if let Some(ed) = state.editor.as_mut() {
        ed.crop_draft = None;
    }
    Task::none()
}

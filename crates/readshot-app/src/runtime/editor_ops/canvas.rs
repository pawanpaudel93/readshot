//! Editor canvas messages: eyedropper, drags, selection and commits.
//! Moved out of `handle_editor_message`; `use super::*` keeps the
//! arms' names resolving as they did there.

use super::*;

/// `Message::EditorCanvas`: an event from the annotation canvas.
pub(super) fn on_canvas(state: &mut App, msg: readshot_ui::CanvasMessage) -> Task<Message> {
    let Some(ed) = state.editor.as_mut() else {
        return Task::none();
    };
    if ed.busy {
        return Task::none();
    }
    let mut picked: Option<readshot_core::Rgba> = None;
    match msg {
        readshot_ui::CanvasMessage::EyedropperHover(p) => eyedropper_hover(ed, p),
        readshot_ui::CanvasMessage::EyedropperPick(p) => {
            // One pick, then the eyedropper exits. The source image
            // is sampled, so annotations drawn over it never count.
            if let Some(eye) = ed.eyedropper.take() {
                let shown = readshot_core::eyedropper::displayed_base_point(p, ed.crop_offset());
                if let Some(color) = readshot_core::eyedropper::sample(ed.model.base(), shown) {
                    match eye.target {
                        crate::editor::EyedropperTarget::Annotation => {
                            let undoable = apply_editor_color(ed, &state.coordinator, color);
                            ed.set_status(if undoable {
                                format!("Picked {}. {UNDO_HINT}", color.to_hex())
                            } else {
                                format!("Picked {}.", color.to_hex())
                            });
                            picked = Some(color);
                        }
                        crate::editor::EyedropperTarget::BeautifyBackground => {
                            // A pick replaces whatever was typed.
                            ed.beautify_hex_draft = None;
                            cancel_editor_previews(ed);
                            ed.snapshot_drag_cancelled = false;
                            let mut s = ed.model.beautify();
                            s.apply(readshot_core::beautify::BeautifyEdit::Background(
                                readshot_core::beautify::Background::Solid {
                                    rgba: color.to_rgba8(),
                                },
                            ));
                            if ed.model.set_beautify(s) {
                                ed.clear_discard_confirmation();
                                ed.set_status(format!(
                                    "Background set to {}. {UNDO_HINT}",
                                    color.to_hex()
                                ));
                            }
                        }
                    }
                }
            }
        }
        readshot_ui::CanvasMessage::DragStarted => {
            // A shape/freehand drag is now live; Escape should cancel
            // it rather than trigger discard.
            ed.canvas_drag_active = true;
            ed.escape_ended_drag = None;
            // The frame its points are in: the one shown, which is
            // the present one (a press in another was dropped).
            ed.drag_started_in = Some(crate::editor::CanvasDragFrame::Live(ed.shown_frame()));
        }
        readshot_ui::CanvasMessage::DragMoved(_) | readshot_ui::CanvasMessage::PolylineMoved(_) => {
            // Preview-only events. The canvas's own State holds
            // the drag points; a redraw is automatic.
        }
        msg @ (readshot_ui::CanvasMessage::Cancelled | readshot_ui::CanvasMessage::Escaped) => {
            ed.canvas_drag_active = false;
            ed.drag_started_in = None;
            // One Esc = one step: a live slider preview cancelled
            // by an Escape is that Escape's step, so its key press
            // (which follows) must not go on to the selection or
            // draft. Only an Escape marks it: a `Cancelled` from a
            // released click has no key press to consume.
            let slider = slider_preview_live(ed);
            cancel_editor_previews(ed);
            if slider && msg == readshot_ui::CanvasMessage::Escaped {
                ed.escape_ended_drag = Some(crate::editor::EscapeSeenBy::Canvas);
            }
        }
        readshot_ui::CanvasMessage::DragEscaped => drag_escaped(ed),
        readshot_ui::CanvasMessage::SelectPressed(p) => select_pressed(ed, &state.coordinator, p),
        readshot_ui::CanvasMessage::SelectDragged(p) => select_dragged(ed, p),
        readshot_ui::CanvasMessage::SelectReleased => select_released(ed, &state.coordinator),
        readshot_ui::CanvasMessage::RequestText(p) => {
            cancel_editor_previews(ed);
            commit_pending_editor_text(ed, &state.coordinator);
            // Text tool clicked — open the inline text-input
            // banner. The eventual Annotation::Text lands at
            // exactly the click point regardless of how long
            // the user takes to type.
            ed.begin_text_edit(crate::editor::PendingText {
                origin: p,
                content: String::new(),
                edit_index: None,
            });
        }
        readshot_ui::CanvasMessage::CommitAnnotation(annotation) => {
            commit_annotation(ed, &state.coordinator, annotation)
        }
    }
    if let Some(color) = picked {
        state.remember_custom_color(color);
    }
    Task::none()
}

/// The loupe follows the pointer while the eyedropper is on.
fn eyedropper_hover(ed: &mut crate::editor::EditorSession, p: readshot_core::PointLike) {
    if ed.eyedropper.is_some() {
        // Sample what the canvas shows: a fractional crop
        // origin is truncated by the renderer.
        let shown = readshot_core::eyedropper::displayed_base_point(p, ed.crop_offset());
        let pixels = readshot_core::eyedropper::sample_grid(
            ed.model.base(),
            shown,
            readshot_ui::editor::LOUPE_RADIUS,
        );
        if let Some(eye) = ed.eyedropper.as_mut() {
            eye.loupe = Some(readshot_ui::editor::Loupe { center: p, pixels });
        }
    }
}

/// An Escape that ended a live canvas drag.
fn drag_escaped(ed: &mut crate::editor::EditorSession) {
    ed.drag_started_in = None;
    // This Escape ends the live drag only (a re-drag over
    // an open crop draft brings the draft back; a Select
    // move or resize goes back, still selected). The next
    // Esc does the rest. The key press of the same Escape
    // may come before or after this.
    if ed.escape_ended_drag.take() == Some(crate::editor::EscapeSeenBy::Key) {
        // The key already ended the re-drag.
        ed.canvas_drag_active = false;
        return;
    }
    ed.escape_ended_drag = ed
        .canvas_drag_in_progress()
        .then_some(crate::editor::EscapeSeenBy::Canvas);
    ed.canvas_drag_active = false;
    cancel_editor_previews(ed);
}

/// A Select-tool press: grab a handle, pick an annotation or clear.
fn select_pressed(
    ed: &mut crate::editor::EditorSession,
    coord: &CaptureCoordinator,
    p: readshot_core::PointLike,
) {
    ed.escape_ended_drag = None;
    cancel_editor_previews(ed);
    commit_pending_editor_text(ed, coord);
    // A press mapped from the frame shown before hit-tests
    // at the scale it was made at, in present document px.
    let scale = ed
        .press_remap
        .as_ref()
        .filter(|r| r.view_seq == ed.canvas_view_seq.get())
        .map_or_else(|| ed.render_scale.get(), |r| r.scale);
    if let Some(handle) = ed.model.resize_handle_at_scaled(p, scale) {
        let Some(selected_index) = ed.model.selected_annotation() else {
            return;
        };
        ed.move_drag = Some(crate::editor::MoveDrag {
            baseline: ed.model.annotations().to_vec(),
            selected_index,
            start: p,
            moved: false,
            kind: crate::editor::MoveDragKind::Resize(handle),
        });
    } else if let Some(selected_index) = {
        let before = ed.model.selected_annotation();
        let picked = ed.model.select_at_scaled(p, scale);
        // A new selection explains itself while it lasts
        // (as a Tab selection does).
        if picked.is_some() && picked != before && hint_may_show(ed) {
            if let Some(kind) = ed.model.selected_kind_label() {
                ed.set_guide(
                    editor_selected_hint(kind, ed.model.selected_text_edit().is_some()),
                    crate::editor::KeyGuide::Selection,
                );
            }
        }
        picked
    } {
        // A press on an unselected magnifier's bubble,
        // source centre or rim grabs that part on this
        // first drag too, as the second drag would; only
        // elsewhere does it move the whole magnifier.
        let handle = (ed.model.selected_kind_label() == Some("Magnifier"))
            .then(|| ed.model.resize_handle_at_scaled(p, scale))
            .flatten();
        let kind = match handle {
            Some(handle) => crate::editor::MoveDragKind::Resize(handle),
            None => crate::editor::MoveDragKind::Move,
        };
        ed.move_drag = Some(crate::editor::MoveDrag {
            baseline: ed.model.annotations().to_vec(),
            selected_index,
            start: p,
            moved: false,
            kind,
        });
    } else {
        ed.move_drag = None;
    }
}

/// A Select-tool drag tick: preview the move or resize.
fn select_dragged(ed: &mut crate::editor::EditorSession, p: readshot_core::PointLike) {
    let mut background_without_selected = None;
    // A redaction dragged on a document small enough to
    // render cheaply (under 4 MP) is rendered where it is
    // on each tick, so its real effect (pixelate, blur)
    // shows while it moves; on a bigger one the canvas
    // covers it opaquely and the release renders it.
    let small = ed.renders_redaction_drags_live();
    let mut render_present = false;
    if let Some(drag) = ed.move_drag.as_mut() {
        let dx = p.x - drag.start.x;
        let dy = p.y - drag.start.y;
        if !drag.moved && dx.abs() < 0.5 && dy.abs() < 0.5 {
            return;
        }
        let changed = match drag.kind {
            crate::editor::MoveDragKind::Move => {
                ed.model.preview_move_selected_from(&drag.baseline, dx, dy)
            }
            crate::editor::MoveDragKind::Resize(handle) => {
                ed.model
                    .preview_resize_selected_from(&drag.baseline, handle, dx, dy)
            }
        };
        if changed {
            let redaction = drag
                .baseline
                .get(drag.selected_index)
                .is_some_and(readshot_core::Annotation::is_redaction);
            if small && redaction {
                render_present = true;
            } else if !drag.moved
                && can_preview_drag_annotation(&drag.baseline, drag.selected_index)
            {
                background_without_selected = Some(drag.selected_index);
            }
            drag.moved = true;
            if refresh_image_during_select_drag(&drag.baseline, drag.selected_index) {
                ed.refresh_crop_drag_preview();
            }
            // For ordinary annotations, keep the flattened image
            // handle stable while the pointer is moving.
            // Re-uploading a full RGBA texture on every Select
            // drag tick can briefly reveal the dark stage behind
            // the image. Crop is the exception: it changes the
            // rendered output geometry, so the texture must stay
            // in sync with the preview dimensions.
        }
    }
    if let Some(index) = background_without_selected {
        ed.show_without_dragged(index);
    }
    if render_present {
        ed.refresh_image();
    }
}

/// A Select-tool release: land the move or resize as one step.
fn select_released(ed: &mut crate::editor::EditorSession, coord: &CaptureCoordinator) {
    let Some(drag) = ed.move_drag.take() else {
        return;
    };
    if !drag.moved {
        // A preview tick that changed nothing (a refused
        // resize) still opened an entry; close it so it
        // can't pass for a later preview from the same
        // baseline.
        ed.model.end_annotation_preview();
        return;
    }
    if !ed.model.commit_preview_from_baseline(drag.baseline) {
        ed.refresh_image();
        return;
    }
    let verb = match drag.kind {
        crate::editor::MoveDragKind::Move => "Moved",
        crate::editor::MoveDragKind::Resize(_) => "Resized",
    };
    let status = format!("{verb} annotation. {UNDO_HINT}");
    edit_landed(ed, coord, Some(status));
}

/// A drawn annotation, unless the frame changed under its drag.
fn commit_annotation(
    ed: &mut crate::editor::EditorSession,
    coord: &CaptureCoordinator,
    annotation: readshot_core::Annotation,
) {
    // A drag's shape is in the frame it began in: if the
    // frame changed since (a pending step landed on its
    // press, or mid-drag), it would land somewhere else.
    // Pins and stamps commit on their own press (no drag),
    // in the frame on screen; a leftover drag frame never
    // applies to them.
    let from_drag = !matches!(
        annotation,
        readshot_core::Annotation::NumberedPin { .. } | readshot_core::Annotation::Stamp { .. }
    );
    let stale = match ed.drag_started_in.take() {
        Some(crate::editor::CanvasDragFrame::Dropped) => from_drag,
        Some(crate::editor::CanvasDragFrame::Live(g)) => {
            from_drag
                && ed.canvas_drag_active
                && !crate::editor::same_frame(&g, ed.model.geometry())
        }
        None => false,
    };
    if stale {
        ed.canvas_drag_active = false;
        return;
    }
    ed.canvas_drag_active = false;
    cancel_editor_previews(ed);
    if let readshot_core::Annotation::Crop { rect } = annotation {
        // Crop is a draft until Apply ↵. A tool
        // switched mid-drag must not leave a draft behind.
        if ed.model.active_tool() == readshot_ui::editor::ToolState::Crop {
            ed.crop_draft = Some(rect);
            ed.set_guide(
                format!("Press {RETURN_KEY} to crop, {ESC_KEY} to cancel."),
                crate::editor::KeyGuide::CropDraft,
            );
        } else {
            ed.crop_draft = None;
        }
        return;
    }
    let annotation = crate::runtime::text_panel::snap_highlight_annotation(ed, annotation);
    handle_commit_annotation(ed, annotation);
    sync_editor_history(ed, coord);
}

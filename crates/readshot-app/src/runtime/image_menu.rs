//! Image menu (spec §2.5, §3.10): Resize…, Rotate Left/Right, Flip
//! Horizontal/Vertical, the combine items (Add Capture Below/Beside, Add
//! Image from File; `combine.rs`) and, once there are two parts, the
//! Layout gap and alignment. Every action is
//! one undo step; annotations, crops and redactions move with the pixels
//! (`readshot_core::document`).
//!
//! Every geometry change goes through the `EditorState` APIs, which
//! validate; a [`GeometryError`] becomes a status, never a silent no-op.
//! The History sync then runs, which rebuilds the Text panel's word boxes
//! from the per-part recognitions (mapped through the transform, never
//! re-recognized on turned pixels) and rewrites the stored image in the
//! new frame.

use super::*;

use iced::widget::{column, container, row, text, text_input, toggler, Space};
use iced::{Alignment, Element, Length, Task};
use readshot_core::document::{
    Align, FlipAxis, GeometryError, Join, MAX_DOC_PIXELS, MAX_DOC_SIDE, MAX_GAP,
};

use crate::app::{App, Message};
use crate::editor::{EditorPopover, EditorSession, ResizeDraft, ResizeUnit};
use crate::icons::AppIcon;

/// Commit the text draft, drop previews and the open popover: what every
/// document-changing action does first.
pub(crate) fn prepare(ed: &mut EditorSession, coord: &crate::coordinator::CaptureCoordinator) {
    // Cancel previews first (as Undo does): a text commit moves the
    // history generation, which would make an open slider preview stale
    // and bake it into history instead of dropping it.
    cancel_editor_previews(ed);
    commit_pending_editor_text(ed, coord);
    close_editor_popover(ed);
    ed.crop_draft = None;
}

fn changed(ed: &mut EditorSession, coord: &crate::coordinator::CaptureCoordinator, status: String) {
    ed.refresh_image();
    ed.clear_eyedropper_loupe();
    ed.clear_discard_confirmation();
    ed.set_status(status);
    sync_editor_history(ed, coord);
}

/// The status for a refused geometry change; `action` is the verb
/// ("rotate", "flip", "resize").
pub(crate) fn geometry_error_status(action: &str, cropped: bool, e: GeometryError) -> String {
    // With a crop active the displayed size is the crop's, but the limit
    // applies to the whole document.
    let what = if cropped {
        "the uncropped image would be"
    } else {
        "that would be"
    };
    match e {
        GeometryError::TooLarge { width, height } => format!(
            "Couldn't {action} — {what} {width} × {height} px, and the limit is {MAX_DOC_SIDE} px on a side."
        ),
        GeometryError::TooManyPixels { pixels, .. } if cropped => format!(
            "Couldn't {action} — the uncropped image would be {pixels} px, over the {MAX_DOC_PIXELS} px limit."
        ),
        GeometryError::TooManyPixels { pixels, .. } => format!(
            "Couldn't {action} — image too large: {pixels} px is over the {MAX_DOC_PIXELS} px limit."
        ),
        GeometryError::Empty => format!("Couldn't {action} — the image has no content."),
    }
}

pub(crate) fn handle_image_message(state: &mut App, message: Message) -> Task<Message> {
    let coord = state.coordinator.clone();
    let Some(ed) = state.editor.as_mut() else {
        return Task::none();
    };
    if ed.busy {
        return Task::none();
    }
    // A held canvas drag (shape, freehand, Select move/resize) is in the
    // present frame: changing the geometry under it would land the shape
    // in the wrong place. ⌘[ / ⌘] / ⌥⌘I wait for the release.
    let geometry_change = matches!(
        message,
        Message::EditorRotate { .. }
            | Message::EditorFlip(_)
            | Message::EditorResizeOpen
            | Message::EditorResizeApply
    );
    if geometry_change && (ed.canvas_drag_active || ed.move_drag.is_some()) {
        return Task::none();
    }
    match message {
        Message::EditorRotate { clockwise } => {
            prepare(ed, &coord);
            let kind = crate::editor::ComposeKind::Rotate { clockwise };
            if let Some(task) = super::compose_jobs::queue_geometry_step(ed, kind, "rotate", |g| {
                readshot_ui::editor::EditorState::rotated_geometry(g, clockwise)
            }) {
                return task;
            }
            let cropped = ed.last_crop().is_some();
            match ed.model.rotate(clockwise) {
                Ok(true) => {
                    let dir = if clockwise { "right" } else { "left" };
                    changed(ed, &coord, format!("Rotated {dir}. ⌘Z to undo."));
                }
                Ok(false) => {}
                Err(e) => ed.set_status(geometry_error_status("rotate", cropped, e)),
            }
        }
        Message::EditorFlip(axis) => {
            prepare(ed, &coord);
            let kind = crate::editor::ComposeKind::Flip(axis);
            if let Some(task) = super::compose_jobs::queue_geometry_step(ed, kind, "flip", |g| {
                readshot_ui::editor::EditorState::flipped_geometry(g, axis)
            }) {
                return task;
            }
            let cropped = ed.last_crop().is_some();
            match ed.model.flip(axis) {
                Ok(true) => {
                    let what = match axis {
                        FlipAxis::Horizontal => "horizontally",
                        FlipAxis::Vertical => "vertically",
                    };
                    changed(ed, &coord, format!("Flipped {what}. ⌘Z to undo."));
                }
                Ok(false) => {}
                Err(e) => ed.set_status(geometry_error_status("flip", cropped, e)),
            }
        }
        Message::EditorResizeOpen => {
            // Already open (⌥⌘I while typing in a field): keep the draft.
            if ed.popover == Some(EditorPopover::Resize) {
                return Task::none();
            }
            prepare(ed, &coord);
            ed.resize_draft = Some(ResizeDraft::new(ed.effective_image_size()));
            ed.popover = Some(EditorPopover::Resize);
        }
        Message::EditorResizeWidthChanged(v) => {
            if let Some(d) = ed.resize_draft.as_mut() {
                d.set_width(&v);
            }
        }
        Message::EditorResizeHeightChanged(v) => {
            if let Some(d) = ed.resize_draft.as_mut() {
                d.set_height(&v);
            }
        }
        Message::EditorResizeUnitChanged(u) => {
            if let Some(d) = ed.resize_draft.as_mut() {
                d.set_unit(u);
            }
        }
        Message::EditorResizeLockToggled(on) => {
            if let Some(d) = ed.resize_draft.as_mut() {
                d.lock = on;
            }
        }
        Message::EditorResizeApply => {
            let Some(d) = ed.resize_draft.clone() else {
                return Task::none();
            };
            let Some((w, h)) = d.target() else {
                ed.set_status("Enter a width and height of at least 1 px.");
                return Task::none();
            };
            // The dialog's percentages and ratio refer to the size it
            // opened at; if undo/redo moved the document under it, the
            // numbers no longer mean what they say.
            if ed.effective_image_size() != d.base {
                close_editor_popover(ed);
                ed.set_status("The image changed. Open Resize again.");
                return Task::none();
            }
            let (bw, bh) = d.base;
            let (sx, sy) = (w as f32 / bw.max(1) as f32, h as f32 / bh.max(1) as f32);
            let cropped = ed.last_crop().is_some();
            let step = match ed.model.scaled_geometry(sx, sy) {
                Some(next) => ed.model.geometry_step(next),
                None => Ok(None),
            };
            match step {
                Ok(Some(step)) if ed.is_large() || super::compose_jobs::is_large(ed, &step) => {
                    close_editor_popover(ed);
                    ed.set_status("Resizing…");
                    return super::compose_jobs::queue_push(
                        ed,
                        crate::editor::ComposeKind::Resize {
                            width: w,
                            height: h,
                            sx,
                            sy,
                        },
                        step,
                    );
                }
                Ok(_) => {}
                Err(e) => {
                    ed.set_status(geometry_error_status("resize", cropped, e));
                    return Task::none();
                }
            }
            match ed.model.scale_by(sx, sy) {
                Ok(true) => {
                    close_editor_popover(ed);
                    changed(ed, &coord, format!("Resized to {w} × {h} px. ⌘Z to undo."));
                }
                Ok(false) => {
                    close_editor_popover(ed);
                }
                Err(e) => ed.set_status(geometry_error_status("resize", cropped, e)),
            }
        }
        other => unreachable!("non-image message routed to the image menu: {other:?}"),
    }
    Task::none()
}

pub(crate) fn image_menu_row<'a>(
    icon: AppIcon,
    label: &'static str,
    key: Option<&'static str>,
    msg: Option<Message>,
    p: &'static Palette,
) -> Element<'a, Message> {
    let mut r = row![
        crate::icons::icon(icon, p.t2, ICON_SIZE),
        text(label).size(TYPE_BODY).style(text_t1)
    ]
    .spacing(8)
    .align_y(Alignment::Center);
    r = r.push(Space::new().width(Length::Fill));
    if let Some(k) = key {
        r = r.push(key_chip(k));
    }
    bar_button(
        r.width(Length::Fill).into(),
        ButtonKind::Ghost,
        ButtonSize::Sm,
        msg,
    )
    .width(Length::Fill)
    .into()
}

pub(crate) fn image_popover<'a>(
    ed: &'a EditorSession,
    p: &'static Palette,
) -> Element<'a, Message> {
    const W: f32 = 260.0;
    let on = |m: Message| (!ed.busy).then_some(m);
    let menu = column![
        image_menu_row(
            AppIcon::Resize,
            "Resize…",
            Some("⌥⌘I"),
            on(Message::EditorResizeOpen),
            p
        ),
        image_menu_row(
            AppIcon::RotateLeft,
            "Rotate Left",
            Some("⌘["),
            on(Message::EditorRotate { clockwise: false }),
            p
        ),
        image_menu_row(
            AppIcon::RotateRight,
            "Rotate Right",
            Some("⌘]"),
            on(Message::EditorRotate { clockwise: true }),
            p
        ),
        image_menu_row(
            AppIcon::FlipH,
            "Flip Horizontal",
            None,
            on(Message::EditorFlip(FlipAxis::Horizontal)),
            p
        ),
        image_menu_row(
            AppIcon::FlipV,
            "Flip Vertical",
            None,
            on(Message::EditorFlip(FlipAxis::Vertical)),
            p
        ),
        menu_divider(),
        image_menu_row(
            AppIcon::AddBelow,
            "Add Capture Below…",
            None,
            on(Message::EditorCombineCaptureRequested(Join::Below)),
            p
        ),
        image_menu_row(
            AppIcon::AddBeside,
            "Add Capture Beside…",
            None,
            on(Message::EditorCombineCaptureRequested(Join::Beside)),
            p
        ),
        image_menu_row(
            AppIcon::Image,
            "Add Image from File…",
            None,
            on(Message::EditorAddImageFromFile),
            p
        ),
    ]
    .spacing(2)
    .width(Length::Fixed(W));
    if ed.model.geometry().parts.len() < 2 {
        return menu.into();
    }
    menu.push(menu_divider()).push(layout_section(ed)).into()
}

fn menu_divider<'a>() -> Element<'a, Message> {
    container(Space::new().width(Length::Fill).height(Length::Fixed(1.0)))
        .style(divider_style)
        .into()
}

/// Gap (slider: previews while dragged, one undo step on release) and
/// alignment of a combined document.
fn layout_section<'a>(ed: &'a EditorSession) -> Element<'a, Message> {
    let g = ed.model.geometry();
    // A coalesced tick not previewed yet is still what the slider shows.
    let gap = ed.layout_gap_pending.unwrap_or(g.gap);
    column![
        popover_heading("Layout"),
        row![
            text("Gap")
                .size(TYPE_CAPTION)
                .style(text_t2)
                .width(Length::Fixed(52.0)),
            iced::widget::slider(0.0..=MAX_GAP as f32, gap as f32, |v| {
                Message::EditorLayoutGapPreview(v.round() as u32)
            })
            .step(1.0)
            .on_release(Message::EditorSnapshotSliderCommit)
            .width(Length::Fill),
            text(format!("{gap} px"))
                .font(mono_font())
                .size(TYPE_CAPTION)
                .style(text_t2),
        ]
        .spacing(8)
        .align_y(Alignment::Center),
        row![
            text("Align")
                .size(TYPE_CAPTION)
                .style(text_t2)
                .width(Length::Fixed(52.0)),
            text_segment(&Align::ALL.map(|a| (a.label(), a)), g.align, |a| {
                (!ed.busy).then_some(Message::EditorLayoutAlign(a))
            }),
        ]
        .spacing(8)
        .align_y(Alignment::Center),
    ]
    .spacing(6)
    .into()
}

pub(crate) fn resize_popover<'a>(
    ed: &'a EditorSession,
    _p: &'static Palette,
) -> Element<'a, Message> {
    const W: f32 = 260.0;
    let Some(d) = ed.resize_draft.as_ref() else {
        return Space::new().into();
    };
    let field = |value: &str, on: fn(String) -> Message| {
        text_input("", value)
            .on_input(on)
            .on_submit(Message::EditorResizeApply)
            .size(TYPE_BODY)
            .width(Length::Fixed(84.0))
    };
    let unit = text_segment(
        &[("px", ResizeUnit::Pixels), ("%", ResizeUnit::Percent)],
        d.unit,
        |u| Some(Message::EditorResizeUnitChanged(u)),
    );
    let preview = match d.target() {
        Some((w, h)) => format!("{} × {} px → {w} × {h} px", d.base.0, d.base.1),
        None => "Enter a width and height of at least 1 px.".into(),
    };
    column![
        popover_heading("Resize"),
        row![
            text("Width")
                .size(TYPE_CAPTION)
                .style(text_t2)
                .width(Length::Fixed(52.0)),
            field(&d.width, Message::EditorResizeWidthChanged),
            unit
        ]
        .spacing(8)
        .align_y(Alignment::Center),
        row![
            text("Height")
                .size(TYPE_CAPTION)
                .style(text_t2)
                .width(Length::Fixed(52.0)),
            field(&d.height, Message::EditorResizeHeightChanged)
        ]
        .spacing(8)
        .align_y(Alignment::Center),
        toggler(d.lock)
            .label("Lock aspect ratio")
            .text_size(TYPE_CAPTION)
            .on_toggle(Message::EditorResizeLockToggled),
        text(preview)
            .font(mono_font())
            .size(TYPE_CAPTION)
            .style(text_t3),
        row![
            Space::new().width(Length::Fill),
            bar_button(
                text("Cancel").size(ButtonSize::Sm.text_size()).into(),
                ButtonKind::Ghost,
                ButtonSize::Sm,
                Some(Message::EditorPopoverClosed)
            ),
            bar_button(
                text("Resize").size(ButtonSize::Sm.text_size()).into(),
                ButtonKind::Secondary,
                ButtonSize::Sm,
                (!ed.busy && d.target().is_some()).then_some(Message::EditorResizeApply),
            ),
        ]
        .spacing(SP1),
    ]
    .spacing(8)
    .width(Length::Fixed(W))
    .into()
}

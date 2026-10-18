//! Image menu: Resize…, Rotate Left/Right, Flip
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
use crate::editor::{ComposeKind, EditorPopover, EditorSession, ResizeDraft, ResizeUnit};
use crate::icons::AppIcon;

/// What every history step (Image menu, combine, Undo / Redo) does
/// before it changes the document: previews are cancelled first (as
/// Undo does), then the text draft is committed, then the crop draft is
/// dropped. A text commit moves the history generation, which would make
/// an open slider preview stale and bake it into history instead of
/// dropping it, so the order matters.
pub(crate) fn before_geometry_change(
    ed: &mut EditorSession,
    coord: &crate::coordinator::CaptureCoordinator,
) {
    cancel_editor_previews(ed);
    commit_pending_editor_text(ed, coord);
    ed.crop_draft = None;
}

/// [`before_geometry_change`], and the open popover closed: what every
/// Image menu action does first.
pub(crate) fn prepare(ed: &mut EditorSession, coord: &crate::coordinator::CaptureCoordinator) {
    before_geometry_change(ed, coord);
    close_editor_popover(ed);
}

/// `n` with thousands separators ("268,435,456"), so a pixel limit can
/// be read at a glance.
pub(crate) fn group_digits(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// What a refused `kind` was trying to do, as the verb of "Couldn’t …".
fn refused_verb(kind: ComposeKind) -> std::borrow::Cow<'static, str> {
    match kind {
        ComposeKind::Resize { .. } => "resize".into(),
        ComposeKind::Align(_) => "align".into(),
        ComposeKind::Gap(_) | ComposeKind::GapPreview(_) => "change the gap".into(),
        ComposeKind::Rotate { .. } => "rotate".into(),
        ComposeKind::Flip(_) => "flip".into(),
        ComposeKind::AddPart { what, .. } => format!("add the {what}").into(),
        ComposeKind::Undo | ComposeKind::Redo | ComposeKind::Flatten => "change the image".into(),
    }
}

/// What to do after a refused geometry change: every limit is on the
/// whole (uncropped) document, so a smaller size is the way back.
fn geometry_recovery(kind: ComposeKind) -> &'static str {
    match kind {
        ComposeKind::Resize { .. } => "Choose a smaller size.",
        ComposeKind::Gap(_) | ComposeKind::GapPreview(_) => "Choose a smaller gap.",
        ComposeKind::AddPart { .. } => "Resize the image smaller first, then add it again.",
        _ => "Resize the image smaller, then try again.",
    }
}

/// The status for a refused geometry change of `kind`. It names the
/// limit with grouped digits and the next step.
pub(crate) fn geometry_refused_status(
    kind: ComposeKind,
    cropped: bool,
    e: GeometryError,
) -> String {
    // With a crop active the displayed size is the crop's, but the limit
    // applies to the whole document.
    let what = if cropped {
        "the uncropped image would be"
    } else {
        "the image would be"
    };
    let action = refused_verb(kind);
    let next = geometry_recovery(kind);
    match e {
        GeometryError::TooLarge { width, height } => format!(
            "Couldn’t {action} — {what} {}\u{a0}×\u{a0}{}\u{a0}px, and the limit is {}\u{a0}px on a side. {next}",
            group_digits(width.into()),
            group_digits(height.into()),
            group_digits(MAX_DOC_SIDE.into()),
        ),
        GeometryError::TooManyPixels { pixels, .. } => format!(
            "Couldn’t {action} — {what} {}\u{a0}px, over the {}\u{a0}px limit. {next}",
            group_digits(pixels),
            group_digits(MAX_DOC_PIXELS),
        ),
        GeometryError::Empty => format!(
            "Couldn’t {action} — the image has no content. Undo the last change, then try again."
        ),
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
            let kind = ComposeKind::Rotate { clockwise };
            if let Some(task) =
                super::compose_jobs::queue_geometry_step(ed, kind, |g| g.rotated(clockwise))
            {
                return task;
            }
            super::compose_jobs::apply_step_now(ed, &coord, kind, None);
        }
        Message::EditorFlip(axis) => {
            prepare(ed, &coord);
            let kind = ComposeKind::Flip(axis);
            if let Some(task) =
                super::compose_jobs::queue_geometry_step(ed, kind, |g| g.flipped(axis))
            {
                return task;
            }
            super::compose_jobs::apply_step_now(ed, &coord, kind, None);
        }
        Message::EditorResizeOpen => {
            // Already open (⌥⌘I while typing in a field): keep the draft.
            if ed.popover == Some(EditorPopover::Resize) {
                return Task::none();
            }
            prepare(ed, &coord);
            ed.resize_draft = Some(ResizeDraft::new(ed.effective_image_size()));
            ed.popover = Some(EditorPopover::Resize);
            // The keyboard lands in Width (WCAG 2.4.3): Tab moves on to
            // Height, the unit, the lock and the buttons.
            super::key_focus::set_focus(
                ed,
                Some(crate::editor::KeyFocus {
                    zone: crate::editor::FocusZone::Resize,
                    item: crate::editor::FocusItem::ResizeWidth,
                }),
            );
            return iced::widget::operation::focus(iced::widget::Id::new(
                super::key_focus::RESIZE_WIDTH_ID,
            ));
        }
        Message::EditorResizeWidthChanged(v) => {
            if let Some(d) = ed.resize_draft.as_mut() {
                d.set_width(&resize_field_text(&v));
            }
        }
        Message::EditorResizeHeightChanged(v) => {
            if let Some(d) = ed.resize_draft.as_mut() {
                d.set_height(&resize_field_text(&v));
            }
        }
        Message::EditorResizeUnitChanged(u) => {
            if let Some(d) = ed.resize_draft.as_mut() {
                d.set_unit(u);
            }
        }
        Message::EditorResizeLockToggled(on) => {
            if let Some(d) = ed.resize_draft.as_mut() {
                d.set_lock(on);
            }
        }
        Message::EditorResizeApply => {
            let Some(d) = ed.resize_draft.clone() else {
                return Task::none();
            };
            let Some((w, h)) = d.target() else {
                ed.set_status(StatusTone::Warning, d.problem().unwrap_or_default());
                return Task::none();
            };
            // The dialog's percentages and ratio refer to the size it
            // opened at; if undo/redo moved the document under it, the
            // numbers no longer mean what they say.
            if ed.effective_image_size() != d.base {
                close_editor_popover(ed);
                ed.set_status(StatusTone::Warning, "The image changed. Open Resize again.");
                return Task::none();
            }
            let (bw, bh) = d.base;
            let (sx, sy) = (w as f32 / bw.max(1) as f32, h as f32 / bh.max(1) as f32);
            let kind = ComposeKind::Resize {
                width: w,
                height: h,
                sx,
                sy,
            };
            let cropped = ed.last_crop().is_some();
            let step = match ed.model.scaled_geometry(sx, sy) {
                Some(next) => ed.model.geometry_step(next),
                None => Ok(None),
            };
            match step {
                Ok(Some(step)) if ed.is_large() || super::compose_jobs::is_large(ed, &step) => {
                    close_editor_popover(ed);
                    ed.set_status(StatusTone::Progress, "Resizing…");
                    return super::compose_jobs::queue_push(ed, kind, step);
                }
                Ok(_) => {}
                Err(e) => {
                    ed.set_status(StatusTone::Error, geometry_refused_status(kind, cropped, e));
                    return Task::none();
                }
            }
            // A refused size keeps the dialog open, under its status.
            let outcome = super::compose_jobs::apply_step_now(ed, &coord, kind, None);
            if outcome != super::compose_jobs::StepOutcome::Refused {
                close_editor_popover(ed);
            }
        }
        other => unreachable!("non-image message routed to the image menu: {other:?}"),
    }
    Task::none()
}

/// What a Resize field keeps of the typed text: digits and one decimal
/// separator, written `.` (a `,` from a comma-decimal locale is read as
/// one). The ⌥P / ⌥L dialog keys (unit, lock) type a symbol on macOS
/// (π, ¬) that must not land in the number.
pub(crate) fn resize_field_text(raw: &str) -> String {
    let mut seen_point = false;
    raw.chars()
        .filter_map(|c| match c {
            '.' | ',' if !seen_point => {
                seen_point = true;
                Some('.')
            }
            c if c.is_ascii_digit() => Some(c),
            _ => None,
        })
        .collect()
}

/// (icon, label) colours of an Image menu row: a disabled row (the
/// editor is busy) dims both, as the zoom segment does, so it does not
/// look clickable.
pub(crate) fn image_menu_row_colors(p: &Palette, enabled: bool) -> (Color, Color) {
    if enabled {
        (p.t2, p.t1)
    } else {
        (Color { a: 0.45, ..p.t2 }, Color { a: 0.45, ..p.t1 })
    }
}

pub(crate) fn image_menu_row<'a>(
    icon: AppIcon,
    label: &'static str,
    key: Option<&'static str>,
    msg: Option<Message>,
    focused: bool,
    p: &'static Palette,
) -> Element<'a, Message> {
    let (icon_color, label_color) = image_menu_row_colors(p, msg.is_some());
    let mut r = row![
        crate::icons::icon(icon, icon_color, ICON_SIZE),
        text(label).size(TYPE_BODY).color(label_color)
    ]
    .spacing(8)
    .align_y(Alignment::Center);
    r = r.push(Space::new().width(Length::Fill));
    if let Some(k) = key {
        r = r.push(key_chip_enabled(k, msg.is_some()));
    }
    focus_ring(
        bar_button(
            r.width(Length::Fill).into(),
            ButtonKind::Ghost,
            ButtonSize::Sm,
            msg,
        )
        .width(Length::Fill),
        focused,
    )
}

pub(crate) fn image_popover<'a>(
    ed: &'a EditorSession,
    p: &'static Palette,
) -> Element<'a, Message> {
    use crate::editor::{FocusItem as F, FocusZone};
    const W: f32 = 260.0;
    let on = |m: Message| (!ed.busy).then_some(m);
    let lit = |item: F| super::key_focus::is_focused(ed, FocusZone::ImageMenu, item);
    let k = super::key_focus::key_label;
    let menu = column![
        image_menu_row(
            AppIcon::Resize,
            "Resize…",
            Some(super::key_focus::key_label(super::key_focus::KEY_RESIZE)),
            on(Message::EditorResizeOpen),
            lit(F::MenuResize),
            p
        ),
        image_menu_row(
            AppIcon::RotateLeft,
            "Rotate Left",
            Some(ROTATE_LEFT_KEY),
            on(Message::EditorRotate { clockwise: false }),
            lit(F::MenuRotateLeft),
            p
        ),
        image_menu_row(
            AppIcon::RotateRight,
            "Rotate Right",
            Some(ROTATE_RIGHT_KEY),
            on(Message::EditorRotate { clockwise: true }),
            lit(F::MenuRotateRight),
            p
        ),
        image_menu_row(
            AppIcon::FlipH,
            "Flip Horizontal",
            Some(k(super::key_focus::KEY_FLIP_H)),
            on(Message::EditorFlip(FlipAxis::Horizontal)),
            lit(F::MenuFlipH),
            p
        ),
        image_menu_row(
            AppIcon::FlipV,
            "Flip Vertical",
            Some(k(super::key_focus::KEY_FLIP_V)),
            on(Message::EditorFlip(FlipAxis::Vertical)),
            lit(F::MenuFlipV),
            p
        ),
        menu_divider(),
        image_menu_row(
            AppIcon::AddBelow,
            "Add Capture Below…",
            None,
            on(Message::EditorCombineCaptureRequested(Join::Below)),
            lit(F::MenuAddBelow),
            p
        ),
        image_menu_row(
            AppIcon::AddBeside,
            "Add Capture Beside…",
            None,
            on(Message::EditorCombineCaptureRequested(Join::Beside)),
            lit(F::MenuAddBeside),
            p
        ),
        image_menu_row(
            AppIcon::Image,
            "Add Image from File…",
            None,
            on(Message::EditorAddImageFromFile),
            lit(F::MenuAddImage),
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
    use crate::editor::{FocusItem as F, FocusZone};
    let lit = |item: F| super::key_focus::is_focused(ed, FocusZone::ImageMenu, item);
    column![
        popover_heading("Layout"),
        focus_ring(
            row![
                text("Gap")
                    .size(TYPE_CAPTION)
                    .style(text_t2)
                    .width(Length::Fixed(52.0)),
                iced::widget::slider(0.0..=MAX_GAP as f32, gap as f32, |v| {
                    Message::EditorLayoutGapPreview(v.round() as u32)
                })
                .step(1.0_f32)
                .on_release(Message::EditorSnapshotSliderCommit)
                .width(Length::Fill),
                mono_readout(format!("{gap}\u{a0}px"), &format!("{MAX_GAP}\u{a0}px"))
                    .style(text_t2),
            ]
            .spacing(SP2)
            .padding([0.0, SP1])
            .align_y(Alignment::Center),
            lit(F::MenuGap)
        ),
        focus_ring(
            row![
                text("Align")
                    .size(TYPE_CAPTION)
                    .style(text_t2)
                    .width(Length::Fixed(52.0)),
                text_segment(&Align::ALL.map(|a| (a.label(), a)), g.align, |a| {
                    (!ed.busy).then_some(Message::EditorLayoutAlign(a))
                }),
            ]
            .spacing(SP2)
            .padding([0.0, SP1])
            .align_y(Alignment::Center),
            lit(F::MenuAlign)
        ),
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
    use crate::editor::{FocusItem as F, FocusZone};
    let lit = |item: F| super::key_focus::is_focused(ed, FocusZone::Resize, item);
    let field = |value: &str, id: &'static str, on: fn(String) -> Message| {
        text_input("", value)
            .id(iced::widget::Id::new(id))
            .on_input(on)
            .on_submit(Message::EditorResizeApply)
            .style(text_input_style)
            .size(TYPE_BODY)
            .width(Length::Fixed(84.0))
    };
    let unit = focus_ring(
        tooltip_with_key(
            text_segment(
                &[("px", ResizeUnit::Pixels), ("%", ResizeUnit::Percent)],
                d.unit,
                |u| Some(Message::EditorResizeUnitChanged(u)),
            ),
            "Unit",
            Some(RESIZE_UNIT_KEY),
            iced::widget::tooltip::Position::Bottom,
        ),
        lit(F::ResizeUnit),
    );
    let preview = match d.target() {
        Some((w, h)) => format!(
            "{}\u{a0}×\u{a0}{}\u{a0}px → {w}\u{a0}×\u{a0}{h}\u{a0}px",
            d.base.0, d.base.1
        ),
        None => d.problem().unwrap_or_default().into(),
    };
    // The preview is metadata; the validation line instructs (t2).
    let preview_style = if d.target().is_some() {
        text_t3
    } else {
        text_t2
    };
    column![
        popover_heading("Resize"),
        row![
            text("Width")
                .size(TYPE_CAPTION)
                .style(text_t2)
                .width(Length::Fixed(52.0)),
            focus_ring(
                field(
                    &d.width,
                    super::key_focus::RESIZE_WIDTH_ID,
                    Message::EditorResizeWidthChanged
                ),
                lit(F::ResizeWidth)
            ),
            unit
        ]
        .spacing(8)
        .align_y(Alignment::Center),
        row![
            text("Height")
                .size(TYPE_CAPTION)
                .style(text_t2)
                .width(Length::Fixed(52.0)),
            focus_ring(
                field(
                    &d.height,
                    super::key_focus::RESIZE_HEIGHT_ID,
                    Message::EditorResizeHeightChanged
                ),
                lit(F::ResizeHeight)
            )
        ]
        .spacing(8)
        .align_y(Alignment::Center),
        focus_ring(
            tooltip_with_key(
                toggler(d.lock)
                    .label("Lock aspect ratio")
                    .text_size(TYPE_CAPTION)
                    .on_toggle(Message::EditorResizeLockToggled),
                "Lock Aspect Ratio",
                Some(RESIZE_LOCK_KEY),
                iced::widget::tooltip::Position::Bottom,
            ),
            lit(F::ResizeLock),
        ),
        text(preview)
            .font(mono_font())
            .size(TYPE_CAPTION)
            .style(preview_style),
        row![
            Space::new().width(Length::Fill),
            focus_ring(
                bar_button(
                    text("Cancel").size(ButtonSize::Sm.text_size()).into(),
                    ButtonKind::Ghost,
                    ButtonSize::Sm,
                    Some(Message::EditorPopoverClosed)
                ),
                lit(F::ResizeCancel)
            ),
            focus_ring(
                bar_button(
                    text("Resize").size(ButtonSize::Sm.text_size()).into(),
                    ButtonKind::Secondary,
                    ButtonSize::Sm,
                    (!ed.busy && d.target().is_some()).then_some(Message::EditorResizeApply),
                ),
                lit(F::ResizeApply)
            ),
        ]
        .spacing(SP1),
    ]
    .spacing(8)
    .width(Length::Fixed(W))
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_disabled_image_menu_row_is_dimmed() {
        for p in [&DARK, &LIGHT] {
            assert_eq!(image_menu_row_colors(p, true), (p.t2, p.t1));
            let (icon, label) = image_menu_row_colors(p, false);
            assert!(icon.a <= 0.5 && label.a <= 0.5);
        }
    }
}

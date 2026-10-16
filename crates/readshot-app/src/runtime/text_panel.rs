//! Editor Text panel (spec §3.6–§3.8): recognition of the base image,
//! the Text tab, line selection and copy, smart redact and the smart
//! highlighter snap. Positions come from the OCR engine; without them
//! (Windows, Linux today) only the list and copying are offered.

use super::*;

use std::collections::BTreeSet;
use std::ops::Range;

use iced::widget::{column, container, mouse_area, row, scrollable, text, toggler, Space};
use iced::{Alignment, Color, Element, Length, Task, Theme};

use crate::app::{App, Message};
use crate::editor::{EditorSession, EditorToastAction, TextRecognition};
use crate::icons::AppIcon;
use readshot_core::geom::RectLike;
use readshot_core::text_lines::{TextDocument, TextLine};

/// Start recognizing the editor's base image unless it is already
/// running or done. The base (not the flattened output) is recognized,
/// so line boxes are in annotation coordinates and redactions can't
/// hide text from the list. Never sets `busy`.
pub(crate) fn request_text_recognition(state: &mut App) -> Task<Message> {
    let Some(ed) = state.editor.as_mut() else {
        return Task::none();
    };
    if matches!(
        ed.text_panel.recognition,
        TextRecognition::Recognizing { .. } | TextRecognition::Ready(_)
    ) {
        return Task::none();
    }
    ed.text_panel.recognition = TextRecognition::Recognizing {
        since: std::time::Instant::now(),
    };
    if ed.toast_action == Some(EditorToastAction::RetryRecognition) {
        ed.status = None;
        ed.status_set_at = None;
        ed.toast_action = None;
    }
    let session = ed.session_id;
    let req = ocr_request_for_image(ed.model.base().clone(), &state.preferences);
    let coord = state.coordinator.clone();
    Task::perform(
        async move { coord.recognise_document(req).await },
        move |r| Message::EditorTextRecognized {
            session,
            result: r.map_err(|e| e.to_string()),
        },
    )
}

/// History reopen: an engine without line positions could only
/// reproduce the stored `ocr_text`, so the panel is built from it with
/// no OCR. Engines with positions (Apple Vision) recognize again.
pub(crate) fn seed_text_from_history(ed: &mut EditorSession, engine_reports_positions: bool) {
    if engine_reports_positions {
        return;
    }
    let Some(text) = ed
        .source_record
        .as_ref()
        .and_then(|r| r.ocr_text.as_deref())
    else {
        return;
    };
    let doc = readshot_core::text_lines::TextDocument::from_plain(text);
    ed.text_panel.recognition = TextRecognition::Ready(std::sync::Arc::new(doc));
}

pub(crate) fn handle_text_panel_message(state: &mut App, message: Message) -> Task<Message> {
    match message {
        Message::EditorTextRecognize => {
            if let Some(ed) = state.editor.as_mut() {
                ed.text_panel.user_requested = true;
            }
            request_text_recognition(state)
        }
        Message::EditorTextRecognized { session, result } => {
            let panel_open = state.preferences.editor_panel_open;
            let Some(ed) = state.editor.as_mut() else {
                return Task::none();
            };
            if ed.session_id != session {
                return Task::none();
            }
            let user_requested = std::mem::take(&mut ed.text_panel.user_requested);
            match result {
                Ok(doc) => {
                    let tp = &mut ed.text_panel;
                    tp.matches = if doc.positioned {
                        readshot_core::redact::scan(&doc)
                    } else {
                        Vec::new()
                    };
                    tp.hovered = None;
                    tp.selected.clear();
                    tp.anchor = None;
                    tp.recognition = TextRecognition::Ready(std::sync::Arc::new(doc));
                }
                Err(e) => {
                    tracing::warn!(target: "readshot::editor", "text recognition failed: {e}");
                    ed.text_panel.recognition = TextRecognition::Failed(e.clone());
                    // Don't clobber a Saving…/Copying… toast; the panel's
                    // Try Again state still offers the retry. A background
                    // OCR nobody is looking at (panel closed, not asked
                    // for) fails quietly, as history OCR always did.
                    if !ed.busy && (panel_open || user_requested) {
                        ed.set_status_with_action(
                            super::history_ops::friendly_string_error_with(
                                "recognize the text",
                                &e,
                                None,
                            ),
                            EditorToastAction::RetryRecognition,
                        );
                    }
                }
            }
            Task::none()
        }
        Message::EditorPanelToggled => {
            let Some(narrow) = state
                .editor
                .as_ref()
                .map(|ed| ed.window_width < PANEL_DOCK_MIN_WINDOW)
            else {
                return Task::none();
            };
            let opening = if narrow {
                // Narrow windows: a temporary overlay; the remembered
                // preference is untouched.
                match state.editor.as_mut() {
                    Some(ed) => {
                        ed.panel_overlay_open = !ed.panel_overlay_open;
                        ed.panel_overlay_open
                    }
                    None => false,
                }
            } else {
                // Toggle what is shown: an overlay opened in a narrow
                // window that was then widened shows docked, so it counts.
                let shown = state.preferences.editor_panel_open
                    || state
                        .editor
                        .as_ref()
                        .is_some_and(|ed| ed.panel_overlay_open);
                let open = !shown;
                state.set_editor_panel_open(open);
                if let Some(ed) = state.editor.as_mut() {
                    ed.panel_overlay_open = false;
                }
                open
            };
            if let Some(ed) = state.editor.as_mut() {
                ed.text_panel.hovered = None;
                if opening {
                    // The panel takes over; a popover left open over the
                    // canvas would otherwise eat the next Esc.
                    close_editor_popover(ed);
                }
            }
            if opening {
                request_text_recognition(state)
            } else {
                Task::none()
            }
        }
        Message::EditorPanelOverlayDismissed => {
            if let Some(ed) = state.editor.as_mut() {
                close_panel_overlay(ed);
            }
            Task::none()
        }
        Message::EditorPanelTabSelected(tab) => {
            state.set_editor_panel_tab(tab);
            Task::none()
        }
        Message::EditorOpenTextSettings => {
            let _ = update(
                state,
                Message::SettingsPaneSelected(crate::app::SettingsPane::TextRecognition),
            );
            update(state, Message::OpenSettingsRequested)
        }
        Message::EditorTextLineHovered(index) => {
            if let Some(ed) = state.editor.as_mut() {
                ed.text_panel.hovered = Some(index);
            }
            Task::none()
        }
        Message::EditorTextLineUnhovered(index) => {
            if let Some(ed) = state.editor.as_mut() {
                if ed.text_panel.hovered == Some(index) {
                    ed.text_panel.hovered = None;
                }
            }
            Task::none()
        }
        Message::EditorTextLineClicked(index) => {
            let (shift, command) = (state.editor_shift_held, state.editor_command_held);
            if let Some(ed) = state.editor.as_mut() {
                let in_range = ed
                    .text_panel
                    .document()
                    .is_some_and(|d| index < d.lines.len());
                if in_range {
                    let tp = &mut ed.text_panel;
                    click_selection(&mut tp.selected, &mut tp.anchor, index, shift, command);
                }
            }
            Task::none()
        }
        Message::EditorCopyLines => {
            let remove_breaks = state.preferences.editor_copy_remove_line_breaks;
            let Some(ed) = state.editor.as_mut() else {
                return Task::none();
            };
            if ed.busy {
                return Task::none();
            }
            let indices: Vec<usize> = ed.text_panel.selected.iter().copied().collect();
            let Some(doc) = ed.text_panel.document() else {
                return Task::none();
            };
            if indices.is_empty() {
                return Task::none();
            }
            let text = readshot_core::text_lines::copy_lines(doc, &indices, remove_breaks);
            let n = indices.len();
            ed.set_status("Copying text…");
            Task::perform(copy_cached_text(text), move |r| {
                Message::EditorCopyLinesDone(r.map(|_| n).map_err(|e| e.to_string()))
            })
        }
        Message::EditorCopyLinesDone(result) => {
            if let Some(ed) = state.editor.as_mut() {
                ed.set_status(match result {
                    Ok(n) => format!("Copied {n} line{}.", if n == 1 { "" } else { "s" }),
                    Err(e) => {
                        tracing::warn!(target: "readshot::editor", "copy lines failed: {e}");
                        super::history_ops::friendly_string_error_with(
                            "copy the text",
                            &e,
                            Some(super::history_ops::RETRY_HINT),
                        )
                    }
                });
            }
            Task::none()
        }
        Message::EditorRemoveLineBreaksToggled(on) => {
            state.set_copy_remove_line_breaks(on);
            Task::none()
        }
        other => unreachable!("non-text-panel message routed to the text panel handler: {other:?}"),
    }
}

/// Line selection (spec §3.7): a click selects one line (clicking the
/// only selected line clears it); ⇧ selects the range from the anchor
/// (added to the selection when ⌘ is held too); ⌘ toggles one line.
pub(crate) fn click_selection(
    selected: &mut BTreeSet<usize>,
    anchor: &mut Option<usize>,
    index: usize,
    shift: bool,
    command: bool,
) {
    if shift {
        let from = anchor.unwrap_or(index);
        let (lo, hi) = if from <= index {
            (from, index)
        } else {
            (index, from)
        };
        if !command {
            selected.clear();
        }
        selected.extend(lo..=hi);
        anchor.get_or_insert(index);
    } else if command {
        if !selected.remove(&index) {
            selected.insert(index);
        }
        *anchor = Some(index);
    } else if selected.len() == 1 && selected.contains(&index) {
        selected.clear();
        *anchor = None;
    } else {
        selected.clear();
        selected.insert(index);
        *anchor = Some(index);
    }
}

pub(crate) fn copy_lines_label(n: usize) -> String {
    match n {
        0 => "Copy Lines".to_string(),
        1 => "Copy 1 Line".to_string(),
        n => format!("Copy {n} Lines"),
    }
}

/// `text` split into `(segment, marked)` runs; `marks` are char ranges.
pub(crate) fn line_spans(text: &str, marks: &[Range<usize>]) -> Vec<(String, bool)> {
    let mut out: Vec<(String, bool)> = Vec::new();
    for (i, c) in text.chars().enumerate() {
        let marked = marks.iter().any(|r| r.contains(&i));
        match out.last_mut() {
            Some((s, m)) if *m == marked => s.push(c),
            _ => out.push((c.to_string(), marked)),
        }
    }
    out
}

/// Copy already-recognized text; returns it for the toast.
pub(crate) async fn copy_cached_text(text: String) -> Result<String, ClipboardError> {
    if !text.is_empty() {
        copy_text_to_clipboard(text.clone()).await?;
    }
    Ok(text)
}

/// Close the narrow-window overlay panel; returns whether it was open
/// (and shown as an overlay: a wide window docks it, so it is no Esc
/// step there). Hover is dropped only when an overlay actually closes.
pub(crate) fn close_panel_overlay(ed: &mut EditorSession) -> bool {
    if !ed.panel_overlay_open || ed.window_width >= PANEL_DOCK_MIN_WINDOW {
        return false;
    }
    ed.panel_overlay_open = false;
    ed.text_panel.hovered = None;
    true
}

/// Bar widths of the recognizing skeleton, as fractions of the panel's
/// inner width (mockup section 5).
const SKELETON_WIDTHS: [f32; 5] = [0.70, 0.90, 0.55, 0.80, 0.62];
const SKELETON_PERIOD_MS: u128 = 1200;

/// Opacity of the recognizing skeleton: a 1.2 s triangle-wave pulse
/// between 0.55 and 1 (no trig), or a constant 1 under Reduce Motion.
pub(crate) fn skeleton_alpha(elapsed_ms: u128, reduce_motion: bool) -> f32 {
    if reduce_motion {
        return 1.0;
    }
    let t = (elapsed_ms % SKELETON_PERIOD_MS) as f32 / SKELETON_PERIOD_MS as f32;
    let tri = if t < 0.5 { t * 2.0 } else { 2.0 - t * 2.0 };
    0.55 + 0.45 * tri
}

/// Body of the Text tab for every recognition state (spec §3.7).
pub(crate) fn text_tab<'a>(
    state: &'a App,
    ed: &'a EditorSession,
    p: &'static Palette,
) -> Element<'a, Message> {
    let body: Element<'a, Message> = match &ed.text_panel.recognition {
        TextRecognition::Idle | TextRecognition::Recognizing { .. } => {
            recognizing_view(ed, state.reduce_motion)
        }
        TextRecognition::Failed(_) => empty_state(
            AppIcon::Alert,
            p,
            "Couldn't read text",
            "Text recognition didn't finish.",
            Some(
                bar_button(
                    text("Try Again").size(ButtonSize::Sm.text_size()).into(),
                    ButtonKind::Secondary,
                    ButtonSize::Sm,
                    Some(Message::EditorTextRecognize),
                )
                .into(),
            ),
        ),
        TextRecognition::Ready(doc) if doc.is_empty() => empty_state(
            AppIcon::ScanText,
            p,
            "No text found",
            "This capture has no readable text. Recognition languages are in Settings › Text Recognition.",
            Some(
                bar_button(
                    text("Open Settings").size(ButtonSize::Sm.text_size()).into(),
                    ButtonKind::Secondary,
                    ButtonSize::Sm,
                    Some(Message::EditorOpenTextSettings),
                )
                .into(),
            ),
        ),
        TextRecognition::Ready(doc) => lines_view(state, ed, doc, p),
    };
    container(body)
        .padding(SP3)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

fn recognizing_view<'a>(ed: &EditorSession, reduce_motion: bool) -> Element<'a, Message> {
    let elapsed = match ed.text_panel.recognition {
        TextRecognition::Recognizing { since } => since.elapsed().as_millis(),
        _ => 0,
    };
    let alpha = skeleton_alpha(elapsed, reduce_motion);
    let inner = PANEL_WIDTH - 2.0 * SP3;
    let mut col = column![text("Recognizing text…").size(TYPE_CAPTION).style(text_t2)].spacing(SP3);
    for fraction in SKELETON_WIDTHS {
        col = col.push(
            container(
                Space::new()
                    .width(Length::Fixed(inner * fraction))
                    .height(Length::Fixed(10.0)),
            )
            .style(move |theme: &Theme| {
                let p = palette_for(theme);
                container::Style {
                    background: Some(
                        Color {
                            a: p.border.a * alpha,
                            ..p.border
                        }
                        .into(),
                    ),
                    border: iced::Border {
                        radius: R_CHIP.into(),
                        ..iced::Border::default()
                    },
                    ..container::Style::default()
                }
            }),
        );
    }
    col.into()
}

fn semibold() -> iced::Font {
    iced::Font {
        weight: iced::font::Weight::Semibold,
        ..iced::Font::DEFAULT
    }
}

/// One recognized line: hover outlines it on the image, click selects
/// (⇧ range, ⌘ toggle). `marks` (char ranges) are underlined.
fn line_row<'a>(
    index: usize,
    line: &'a TextLine,
    hovered: bool,
    selected: bool,
    marks: &[Range<usize>],
    p: &'static Palette,
) -> Element<'a, Message> {
    let ink = if hovered || selected { p.t1 } else { p.t2 };
    let mark_bg = Color { a: 0.16, ..p.warn };
    let spans: Vec<iced::widget::text::Span<'a, (), iced::Font>> = line_spans(&line.text, marks)
        .into_iter()
        .map(|(segment, marked)| {
            let s = iced::widget::span(segment).color(ink);
            if marked {
                s.underline(true).background(mark_bg)
            } else {
                s
            }
        })
        .collect();
    let content = container(iced::widget::rich_text(spans).size(TYPE_CAPTION))
        .padding([6, 8])
        .width(Length::Fill)
        .style(move |theme: &Theme| {
            let p = palette_for(theme);
            container::Style {
                background: if selected {
                    Some(p.acc_tint.into())
                } else if hovered {
                    Some(p.hover.into())
                } else {
                    None
                },
                border: iced::Border {
                    radius: R_CHIP.into(),
                    ..iced::Border::default()
                },
                ..container::Style::default()
            }
        });
    mouse_area(content)
        .on_enter(Message::EditorTextLineHovered(index))
        .on_exit(Message::EditorTextLineUnhovered(index))
        .on_press(Message::EditorTextLineClicked(index))
        .interaction(iced::mouse::Interaction::Pointer)
        .into()
}

/// The recognized-text list with copy controls (spec §3.7, mockup 1).
fn lines_view<'a>(
    state: &'a App,
    ed: &'a EditorSession,
    doc: &'a TextDocument,
    p: &'static Palette,
) -> Element<'a, Message> {
    let tp = &ed.text_panel;
    let count = doc.lines.len();
    let header = row![
        text("Recognized text")
            .size(TYPE_BODY)
            .font(semibold())
            .style(text_t1),
        Space::new().width(Length::Fill),
        text(format!("{count} line{}", if count == 1 { "" } else { "s" }))
            .size(TYPE_CAPTION)
            .style(text_t3),
    ]
    .align_y(Alignment::Center);
    let mut list = column![].spacing(1);
    for (i, line) in doc.lines.iter().enumerate() {
        list = list.push(line_row(
            i,
            line,
            tp.hovered == Some(i),
            tp.selected.contains(&i),
            &[],
            p,
        ));
    }
    let mut col = column![header, scrollable(list).height(Length::Fill)].spacing(SP2);
    if !doc.positioned {
        col = col.push(
            text("This platform's text recognition doesn't report where lines are, so hover-to-locate and smart redact are off.")
                .size(TYPE_MICRO)
                .style(text_t3),
        );
    }
    let toggle = toggler(state.preferences.editor_copy_remove_line_breaks)
        .label("Remove line breaks when copying")
        .text_size(TYPE_CAPTION)
        .size(16.0)
        .on_toggle(Message::EditorRemoveLineBreaksToggled);
    let n = tp.selected.len();
    let copy_lines = bar_button(
        text(copy_lines_label(n))
            .size(ButtonSize::Sm.text_size())
            .into(),
        ButtonKind::Secondary,
        ButtonSize::Sm,
        (n > 0).then_some(Message::EditorCopyLines),
    )
    .width(Length::Fill);
    let copy_all = bar_button(
        row![
            text("Copy All").size(ButtonSize::Sm.text_size()),
            key_chip("⇧⌘C"),
        ]
        .spacing(6)
        .align_y(Alignment::Center)
        .into(),
        ButtonKind::Secondary,
        ButtonSize::Sm,
        (!ed.busy).then_some(Message::EditorCopyTextRequested),
    )
    .width(Length::Fill);
    col.push(toggle)
        .push(row![copy_lines, copy_all].spacing(SP2))
        .into()
}

/// Boxes the canvas marks for the Text panel: the hovered line and the
/// selected lines. Nothing when the panel is not on screen or the engine
/// gave no positions (hover-to-locate is macOS-only today).
pub(crate) fn text_marks(
    ed: &EditorSession,
    panel_visible: bool,
) -> (Option<RectLike>, Vec<RectLike>) {
    if !panel_visible {
        return (None, Vec::new());
    }
    let Some(doc) = ed.text_panel.positioned_document() else {
        return (None, Vec::new());
    };
    let rect = |i: usize| doc.lines.get(i).and_then(|l| l.rect);
    (
        ed.text_panel.hovered.and_then(rect),
        ed.text_panel
            .selected
            .iter()
            .filter_map(|&i| rect(i))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skeleton_is_static_under_reduce_motion() {
        for ms in [0, 300, 600, 900, 1199, 5000] {
            assert_eq!(skeleton_alpha(ms, true), 1.0);
        }
    }

    #[test]
    fn skeleton_pulse_is_a_bounded_triangle_wave() {
        assert_eq!(skeleton_alpha(0, false), 0.55);
        assert!((skeleton_alpha(600, false) - 1.0).abs() < 1e-6);
        assert_eq!(skeleton_alpha(1200, false), 0.55);
        for ms in (0..2400).step_by(37) {
            let a = skeleton_alpha(ms, false);
            assert!((0.55..=1.0).contains(&a), "{ms}: {a}");
        }
    }

    use std::collections::BTreeSet;

    fn set(items: &[usize]) -> BTreeSet<usize> {
        items.iter().copied().collect()
    }

    #[test]
    fn click_selects_one_line_and_clicking_it_again_clears() {
        let (mut sel, mut anchor) = (BTreeSet::new(), None);
        click_selection(&mut sel, &mut anchor, 2, false, false);
        assert_eq!((sel.clone(), anchor), (set(&[2]), Some(2)));
        click_selection(&mut sel, &mut anchor, 4, false, false);
        assert_eq!((sel.clone(), anchor), (set(&[4]), Some(4)));
        click_selection(&mut sel, &mut anchor, 4, false, false);
        assert_eq!((sel, anchor), (set(&[]), None));
    }

    #[test]
    fn shift_click_selects_the_range_from_the_anchor() {
        let (mut sel, mut anchor) = (BTreeSet::new(), None);
        click_selection(&mut sel, &mut anchor, 3, false, false);
        click_selection(&mut sel, &mut anchor, 1, true, false);
        assert_eq!((sel.clone(), anchor), (set(&[1, 2, 3]), Some(3)));
        click_selection(&mut sel, &mut anchor, 5, true, false);
        assert_eq!(sel, set(&[3, 4, 5]));
        // ⇧ with no anchor selects just that line and anchors it.
        let (mut sel, mut anchor) = (BTreeSet::new(), None);
        click_selection(&mut sel, &mut anchor, 2, true, false);
        assert_eq!((sel, anchor), (set(&[2]), Some(2)));
    }

    #[test]
    fn command_click_toggles_and_command_shift_adds_a_range() {
        let (mut sel, mut anchor) = (BTreeSet::new(), None);
        click_selection(&mut sel, &mut anchor, 1, false, true);
        click_selection(&mut sel, &mut anchor, 4, false, true);
        assert_eq!(sel, set(&[1, 4]));
        click_selection(&mut sel, &mut anchor, 1, false, true);
        assert_eq!((sel.clone(), anchor), (set(&[4]), Some(1)));
        click_selection(&mut sel, &mut anchor, 7, true, true);
        assert_eq!(sel, set(&[1, 2, 3, 4, 5, 6, 7]));
    }

    #[test]
    fn copy_lines_label_counts_lines() {
        assert_eq!(copy_lines_label(0), "Copy Lines");
        assert_eq!(copy_lines_label(1), "Copy 1 Line");
        assert_eq!(copy_lines_label(12), "Copy 12 Lines");
    }

    #[test]
    #[allow(clippy::single_range_in_vec_init)]
    fn line_spans_mark_char_ranges() {
        assert_eq!(
            line_spans("Email a@b.co", &[6..12]),
            vec![("Email ".to_string(), false), ("a@b.co".to_string(), true)]
        );
        assert_eq!(
            line_spans("é@x", &[1..2]),
            vec![
                ("é".to_string(), false),
                ("@".to_string(), true),
                ("x".to_string(), false)
            ]
        );
        assert!(line_spans("", &[]).is_empty());
    }
}

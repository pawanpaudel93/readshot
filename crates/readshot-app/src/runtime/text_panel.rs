//! Editor Text panel (spec §3.6–§3.8): recognition of the base image,
//! the Text tab, line selection and copy, smart redact and the smart
//! highlighter snap. Positions come from the OCR engine; without them
//! (Windows, Linux today) only the list and copying are offered.

use super::*;

use iced::widget::{column, container, scrollable, text, Space};
use iced::{Color, Element, Length, Task, Theme};

use crate::app::{App, Message};
use crate::editor::{EditorSession, EditorToastAction, TextRecognition};
use crate::icons::AppIcon;
use readshot_core::text_lines::TextDocument;

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
        other => unreachable!("non-text-panel message routed to the text panel handler: {other:?}"),
    }
}

/// Close the narrow-window overlay panel; returns whether it was open.
pub(crate) fn close_panel_overlay(ed: &mut EditorSession) -> bool {
    ed.text_panel.hovered = None;
    std::mem::take(&mut ed.panel_overlay_open)
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
        TextRecognition::Ready(doc) => lines_view(doc),
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

/// Recognized lines. Task 6 replaces this with the interactive list.
fn lines_view<'a>(doc: &'a TextDocument) -> Element<'a, Message> {
    let mut list = column![].spacing(1);
    for line in &doc.lines {
        list = list.push(
            container(text(line.text.as_str()).size(TYPE_CAPTION).style(text_t2))
                .padding([6, 8])
                .width(Length::Fill),
        );
    }
    scrollable(list).height(Length::Fill).into()
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
}

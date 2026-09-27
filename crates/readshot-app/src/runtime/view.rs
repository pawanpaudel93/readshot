// Extracted from runtime.rs (pure code-move). `use super::*` pulls in
// sibling/parent items; the explicit imports mirror runtime.rs's preamble.
use super::*;

use std::path::{Path, PathBuf};

use iced::widget::{
    button, column, container, pick_list, responsive, row, scrollable, text, Space,
};
use iced::window;
use iced::{Alignment, Color, Element, Length, Theme};

use readshot_ui::SettingsMessage;

use crate::app::{App, Message};
use crate::editor::EditorFrameStyle;
use crate::permissions::PermissionStatus;
use crate::welcome::WelcomeState;

/// Transparent click-through overlay that draws the captured rect's
/// border on screen during a scrolling-capture session.
///
/// Visual style mirrors the region-selection overlay
/// ([`crate::overlay::OverlayProgram`]): 5 % white fill inside the
/// rect so the active area reads as slightly distinct without
/// obscuring the page underneath, 3 px black halo + 1.5 px animated
/// white dashes (`[6.0, 4.0]`) around it, and a size badge at the
/// rect's top-right corner that mirrors the selector's
/// `phys_w × phys_h px · phys_x, phys_y` format. Sharing
/// `state.overlay_tick` with the selector keeps the marching-ants
/// animation in lockstep.
///
/// Mouse / scroll events pass through to the page underneath thanks
/// to `iced::window::enable_mouse_passthrough` (wired in the
/// `ScrollRegionWindowReady` handler).
pub(crate) fn scroll_region_view(state: &App) -> Element<'_, Message> {
    use iced::widget::canvas::{
        Canvas, Frame, Geometry, LineDash, Path, Program, Stroke, Text as CanvasText,
    };
    use iced::Renderer;

    let session = state.scroll_session.as_ref();
    let Some(s) = session else {
        return iced::widget::Space::new()
            .width(Length::Fill)
            .height(Length::Fill)
            .into();
    };
    let rect = s.rect;
    let scale = s.scale;
    let dash_offset = state.overlay_tick as usize;

    #[derive(Clone, Copy)]
    struct RegionProgram {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        scale: f32,
        dash_offset: usize,
    }
    impl Program<Message> for RegionProgram {
        type State = ();
        fn draw(
            &self,
            _state: &Self::State,
            renderer: &Renderer,
            _theme: &Theme,
            bounds: iced::Rectangle,
            _cursor: iced::mouse::Cursor,
        ) -> Vec<Geometry<Renderer>> {
            let mut frame = Frame::new(renderer, bounds.size());
            let path = Path::rectangle(
                iced::Point::new(self.x, self.y),
                iced::Size::new(self.w, self.h),
            );
            // 5 % white fill — same "punch out" treatment the region
            // selector uses for its committed rect, so the active
            // capture area reads as a faint highlight instead of an
            // unmarked area inside the border.
            frame.fill(&path, Color::from_rgba(1.0, 1.0, 1.0, 0.05));
            // 3 px black halo for contrast on light wallpapers.
            frame.stroke(
                &path,
                Stroke::default()
                    .with_color(Color::from_rgba(0.0, 0.0, 0.0, 0.7))
                    .with_width(3.0),
            );
            // 1.5 px animated white dashes — same marching-ants
            // pattern as the selector so both surfaces read as a
            // single visual language.
            const DASH: &[f32] = &[6.0, 4.0];
            frame.stroke(
                &path,
                Stroke {
                    line_dash: LineDash {
                        segments: DASH,
                        offset: self.dash_offset,
                    },
                    ..Stroke::default().with_color(Color::WHITE).with_width(1.5)
                },
            );
            // Size badge — physical-pixel dimensions + top-left
            // origin, anchored to the rect's top-right with a 4 px
            // pad, clamped into the visible viewport. Same shape
            // and copy as `overlay::draw`'s badge.
            let phys_w = ((self.w * self.scale).round() as i32).max(0);
            let phys_h = ((self.h * self.scale).round() as i32).max(0);
            let phys_x = (self.x * self.scale).round() as i32;
            let phys_y = (self.y * self.scale).round() as i32;
            let label = format!("{phys_w} × {phys_h}px · {phys_x}, {phys_y}");
            let badge_w = 8.0 + label.chars().count() as f32 * 7.0;
            let badge_h = 18.0;
            let pad = 4.0;
            let bx =
                (self.x + self.w - badge_w - pad).clamp(0.0, (bounds.width - badge_w).max(0.0));
            let by = (self.y + pad).clamp(0.0, (bounds.height - badge_h).max(0.0));
            let badge_path =
                Path::rectangle(iced::Point::new(bx, by), iced::Size::new(badge_w, badge_h));
            frame.fill(&badge_path, Color::from_rgba(0.0, 0.0, 0.0, 0.7));
            frame.fill_text(CanvasText {
                content: label,
                position: iced::Point::new(bx + 4.0, by + 2.0),
                color: Color::WHITE,
                size: iced::Pixels(11.0),
                font: mono_font(),
                ..Default::default()
            });
            vec![frame.into_geometry()]
        }
    }
    Canvas::new(RegionProgram {
        x: rect.x(),
        y: rect.y(),
        w: rect.width(),
        h: rect.height(),
        scale,
        dash_offset,
    })
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

/// Floating HUD showing the live state of a scrolling-capture session.
/// Renders a live preview of the most recently captured frame so the
/// user can see what's actually being captured, a frame counter with
/// capacity progress bar, the current activity status, and the
/// Stop / Cancel buttons.
pub(crate) fn scroll_hud_view(state: &App) -> Element<'_, Message> {
    use iced::widget::mouse_area;
    let session = state.scroll_session.as_ref();
    let frame_count = session.map(|s| s.frames.len()).unwrap_or(0);
    let stopping = session.map(|s| s.stopping).unwrap_or(false);
    let elapsed_secs = session
        .map(|s| s.started_at.elapsed().as_secs_f32())
        .unwrap_or(0.0);
    // Frame-arrival pop: tracks elapsed-since-last-frame and uses it
    // to drive a brief tint + grow on the counter. 220 ms gives the
    // pulse enough room to read as a deliberate animation without
    // feeling sluggish.
    let pop_t = session
        .and_then(|s| s.last_frame_at)
        .map(|t| (t.elapsed().as_millis() as f32) / 220.0)
        .unwrap_or(2.0);
    let pop_active = pop_t < 1.0;
    let pop_eased = if pop_active {
        // Quick rise (0.0–0.3) → slow decay (0.3–1.0). Reads as a
        // satisfying "thunk" rather than a fade.
        if pop_t < 0.3 {
            pop_t / 0.3
        } else {
            1.0 - (pop_t - 0.3) / 0.7
        }
    } else {
        0.0
    };
    // Recording-dot heartbeat: gentle 1.4 s sine pulse on the chip's
    // alpha so the HUD doesn't feel statically dead between frames.
    let pulse = if !stopping {
        let t = elapsed_secs * std::f32::consts::TAU / 1.4;
        0.65 + 0.35 * (0.5 + 0.5 * t.sin())
    } else {
        0.55
    };
    // Throughput indicator — frames per second over the whole
    // session. Useful diagnostic for the user wondering whether
    // they're scrolling fast enough or too fast.
    let fps = if elapsed_secs > 0.5 {
        frame_count as f32 / elapsed_secs
    } else {
        0.0
    };
    let cap = SCROLL_MAX_FRAMES;
    let progress = (frame_count as f32 / cap as f32).clamp(0.0, 1.0);

    // Header — title + recording / stitching chip with heartbeat.
    let (chip_label, chip_base): (&'static str, Color) = if stopping {
        ("Stitching", Color::from_rgba(1.0, 1.0, 1.0, 0.55))
    } else {
        ("● Recording", Color::from_rgba(0.95, 0.4, 0.4, pulse))
    };
    let chip_color = chip_base;
    let elapsed_label = format!(
        "{:02}:{:02}",
        (elapsed_secs as u32) / 60,
        (elapsed_secs as u32) % 60
    );
    // Whole header doubles as the drag handle. iced delegates to the
    // OS window drag on press so the user can reposition the HUD
    // without it stealing focus from the page they're scrolling.
    let header_row = row![
        text("Scrolling Capture")
            .size(14)
            .color(Color::from_rgba(1.0, 1.0, 1.0, 0.92)),
        Space::new().width(Length::Fill),
        text(elapsed_label)
            .size(10)
            .color(Color::from_rgba(1.0, 1.0, 1.0, 0.50)),
        Space::new().width(Length::Fixed(8.0)),
        text(chip_label).size(10).color(chip_color),
    ]
    .align_y(Alignment::Center);
    let header = mouse_area(header_row)
        .on_press(Message::ScrollHudDragRequested)
        .interaction(iced::mouse::Interaction::Grab);

    // Live preview of the most recent accepted frame so the user can
    // confirm what's in the capture region while they scroll. Letter-
    // boxed inside a fixed-size bay so HUD layout stays stable across
    // very tall / very wide rects.
    let preview: Element<'_, Message> = match session.and_then(|s| s.last_frame_handle.clone()) {
        Some(handle) => container(
            iced::widget::image(handle)
                .width(Length::Fill)
                .height(Length::Fill)
                .content_fit(iced::ContentFit::Contain),
        )
        .width(Length::Fill)
        .height(Length::Fixed(SCROLL_HUD_PREVIEW_HEIGHT))
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .style(|_| iced::widget::container::Style {
            background: Some(Color::from_rgba(0.0, 0.0, 0.0, 0.4).into()),
            border: iced::Border {
                color: accent(0.55),
                width: 1.0,
                radius: 6.0.into(),
            },
            ..Default::default()
        })
        .into(),
        None => container(
            text("Waiting for first frame…")
                .size(11)
                .color(Color::from_rgba(1.0, 1.0, 1.0, 0.45)),
        )
        .width(Length::Fill)
        .height(Length::Fixed(SCROLL_HUD_PREVIEW_HEIGHT))
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .style(|_| iced::widget::container::Style {
            background: Some(Color::from_rgba(1.0, 1.0, 1.0, 0.04).into()),
            border: iced::Border {
                color: Color::from_rgba(1.0, 1.0, 1.0, 0.12),
                width: 1.0,
                radius: 6.0.into(),
            },
            ..Default::default()
        })
        .into(),
    };

    // Frame counter — big number plus capacity tail. The number
    // grows + tints blue for ~220 ms each time a new frame lands so
    // the user gets a clear "scroll registered" pulse without
    // staring at the digits.
    let counter_color = if pop_active {
        let r = 1.0 - 0.35 * pop_eased;
        let g = 1.0 - 0.22 * pop_eased;
        let b = 1.0;
        Color::from_rgba(r, g, b, 1.0)
    } else {
        Color::WHITE
    };
    let tail_alpha = 0.55 + 0.25 * pop_eased;
    // Fixed font size — the previous `22 + 6 * pop_eased` grow
    // animation expanded the counter row and pushed everything below
    // it in the column, which iced compensated for by shrinking the
    // Length::Fixed preview. Net effect: thumbnail wobbled in size
    // every frame. Keep the text size stable and let the color flash
    // do the "scroll registered" pulse on its own.
    let counter_row = container(
        row![
            text(format!("{frame_count}")).size(26).color(counter_color),
            text(format!("/ {cap} frames"))
                .size(11)
                .color(Color::from_rgba(1.0, 1.0, 1.0, tail_alpha)),
            Space::new().width(Length::Fill),
            text(if fps > 0.0 {
                format!("{fps:.1} fps")
            } else {
                String::new()
            })
            .size(10)
            .color(Color::from_rgba(1.0, 1.0, 1.0, 0.45)),
        ]
        .spacing(6)
        .align_y(Alignment::End),
    )
    // Pin the row to a fixed height so even if a future change adds
    // a transient grow animation back, the surrounding column won't
    // reflow and squeeze the preview.
    .height(Length::Fixed(32.0))
    .width(Length::Fill);

    // Slim capacity bar — uses the same blue accent as the history
    // selected row so the chrome reads as part of the same app.
    let bar_w_max = SCROLL_HUD_WIDTH - 28.0;
    let filled_w = (bar_w_max * progress).max(2.0).min(bar_w_max);
    let bar = container(
        container(
            Space::new()
                .width(Length::Fixed(filled_w))
                .height(Length::Fixed(4.0)),
        )
        .style(|_| iced::widget::container::Style {
            background: Some(accent(0.85).into()),
            border: iced::Border {
                radius: 2.0.into(),
                ..Default::default()
            },
            ..Default::default()
        }),
    )
    .width(Length::Fixed(bar_w_max))
    .height(Length::Fixed(4.0))
    .style(|_| iced::widget::container::Style {
        background: Some(Color::from_rgba(1.0, 1.0, 1.0, 0.08).into()),
        border: iced::Border {
            radius: 2.0.into(),
            ..Default::default()
        },
        ..Default::default()
    });

    // Status line — what the user should be doing right now.
    let status_text = if stopping {
        "Stitching frames into one tall image…".to_string()
    } else {
        "Scroll the page underneath. Click Stop & Stitch (or Esc) when done.".to_string()
    };
    let status = text(status_text)
        .size(11)
        .color(Color::from_rgba(1.0, 1.0, 1.0, 0.65))
        .wrapping(iced::widget::text::Wrapping::Word);

    // Buttons — Stop (primary) + Cancel (discards session).
    let stop_btn: Element<'_, Message> = if stopping {
        button(text("Stitching…").size(12))
            .padding([6, 12])
            .style(|t, s| action_button_style(t, s, ActionKind::Secondary))
            .into()
    } else {
        button(text("Stop & Stitch").size(12))
            .padding([6, 12])
            .style(|t, s| action_button_style(t, s, ActionKind::Primary))
            .on_press(Message::ScrollCaptureStopRequested)
            .into()
    };
    // Cancel button flips to a Danger-styled "Discard N frames?" the
    // moment the user clicks it once on a session with content, so
    // the second click reads as an explicit destructive confirm
    // rather than a benign close.
    let cancel_armed = session.and_then(|s| s.cancel_armed_at).is_some();
    let cancel_btn: Element<'_, Message> = if stopping {
        Space::new().width(Length::Fixed(0.0)).into()
    } else if cancel_armed {
        button(
            text(format!(
                "Discard {} frame{}?",
                frame_count,
                if frame_count == 1 { "" } else { "s" }
            ))
            .size(12),
        )
        .padding([6, 12])
        .style(|t, s| action_button_style(t, s, ActionKind::Danger))
        .on_press(Message::ScrollCaptureCancelRequested)
        .into()
    } else {
        button(text("Cancel").size(12))
            .padding([6, 12])
            .style(|t, s| action_button_style(t, s, ActionKind::Secondary))
            .on_press(Message::ScrollCaptureCancelRequested)
            .into()
    };
    let actions = row![stop_btn, cancel_btn]
        .spacing(8)
        .align_y(Alignment::Center);

    let body = column![
        header,
        preview,
        counter_row,
        bar,
        Space::new().height(Length::Fixed(2.0)),
        status,
        Space::new().height(Length::Fixed(4.0)),
        actions,
    ]
    .spacing(7)
    .align_x(Alignment::Start);
    container(body)
        .padding(14)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(|_| iced::widget::container::Style {
            background: Some(Color::from_rgba(0.05, 0.06, 0.06, 0.96).into()),
            border: iced::Border {
                color: accent(0.28),
                width: 1.0,
                radius: 8.0.into(),
            },
            ..Default::default()
        })
        .into()
}

pub(crate) fn cli_tools_view(state: &App) -> Element<'_, Message> {
    let common = crate::cli_tools::common_commands();
    let verify = crate::cli_tools::verify_commands();
    let status = state.cli_tools_status.as_deref().unwrap_or("");

    let shell_sections =
        crate::cli_tools::Shell::ALL
            .into_iter()
            .fold(column![].spacing(12), |sections, shell| {
                sections.push(
                    container(
                        column![
                            row![
                                text(format!("For {}", shell.label())).size(18),
                                Space::new().width(Length::Fill),
                                button(text(format!("Copy {} Commands", shell.label())).size(13))
                                    .on_press(Message::CliToolsCopyRequested(shell)),
                            ]
                            .spacing(12)
                            .align_y(Alignment::Center),
                            container(
                                text(crate::cli_tools::shell_commands(shell))
                                    .size(13)
                                    .font(mono_font())
                            )
                            .padding(12)
                            .width(Length::Fill),
                        ]
                        .spacing(8),
                    )
                    .padding(10)
                    .width(Length::Fill),
                )
            });

    container(
        column![
            text("Command Line Tools").size(28),
            text("Choose your shell and copy only that command block into Terminal.").size(14),
            text(status).size(13),
            scrollable(
                column![
                    text("Run for every shell").size(18),
                    container(text(common).size(13).font(mono_font()))
                        .padding(12)
                        .width(Length::Fill),
                    shell_sections,
                    text("Verify").size(18),
                    container(text(verify).size(13).font(mono_font()))
                        .padding(12)
                        .width(Length::Fill),
                ]
                .spacing(12)
            )
            .direction(iced::widget::scrollable::Direction::Vertical(
                slim_scrollbar(),
            ))
            .spacing(10.0)
            .height(Length::Fill),
        ]
        .spacing(14)
        .padding(24)
        .width(Length::Fill)
        .height(Length::Fill),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

pub(crate) fn history_view(state: &App) -> Element<'_, Message> {
    use iced::widget::{image as image_widget, mouse_area, text_input};

    let total = state.history_records.len();
    // Apply the live search filter. Empty query → every record.
    // Match is case-insensitive substring against `ocr_text` and the
    // human-readable timestamp (so "april" / "14:32" both work).
    let q = state.history_search.trim().to_lowercase();
    let visible: Vec<&readshot_core::CaptureRecord> = if q.is_empty() {
        state.history_records.iter().collect()
    } else {
        state
            .history_records
            .iter()
            .filter(|r| record_matches(r, &q))
            .collect()
    };
    let shown = visible.len();

    // Header: search box + count line + status.
    let count_line = if total == 0 {
        "No captures yet — take one and it shows up here.".to_string()
    } else if q.is_empty() {
        format!("{total} capture{}", if total == 1 { "" } else { "s" })
    } else {
        format!("{shown} of {total} match \u{201C}{q}\u{201D}")
    };
    let search_box = text_input("Search OCR text or timestamp…", &state.history_search)
        .on_input(Message::HistorySearchChanged)
        .width(Length::Fill)
        .padding(8)
        .size(13);
    // Two-stage Clear All. Idle: a single Danger-styled "Clear All"
    // button. Armed: the button flips to "Confirm Clear" and a
    // companion "Cancel" button appears next to it so the user has
    // an obvious second-step before everything is wiped.
    let clear_button: Element<'_, Message> = if state.history_clear_all_pending {
        row![
            button(text(format!("Confirm Clear ({total})")).size(12))
                .padding([8, 10])
                .style(|t, s| action_button_style(t, s, ActionKind::Danger))
                .on_press(Message::HistoryClearAllConfirmed),
            button(text("Cancel").size(12))
                .padding([8, 10])
                .style(|t, s| action_button_style(t, s, ActionKind::Secondary))
                .on_press(Message::HistoryClearAllCancelled),
        ]
        .spacing(6)
        .align_y(Alignment::Center)
        .into()
    } else {
        let b = button(text("Clear All").size(12))
            .padding([8, 10])
            .style(|t, s| action_button_style(t, s, ActionKind::Danger));
        if total > 0 {
            b.on_press(Message::HistoryClearAllRequested).into()
        } else {
            b.into()
        }
    };
    let has_visible_text = visible.iter().any(|r| {
        r.ocr_text
            .as_deref()
            .is_some_and(|text| !text.trim().is_empty())
    });
    let copy_visible_button = {
        let b = button(text("Copy Visible Text").size(12))
            .padding([8, 10])
            .style(|t, s| action_button_style(t, s, ActionKind::Secondary));
        if has_visible_text {
            b.on_press(Message::HistoryCopyVisibleTextRequested)
        } else {
            b
        }
    };
    let search_row = row![search_box, copy_visible_button, clear_button]
        .spacing(8)
        .align_y(Alignment::Center);
    let selected = state
        .history_selected_id
        .and_then(|id| visible.iter().copied().find(|record| record.id == id));
    let selected_panel: Element<'_, Message> = if visible.is_empty() {
        Space::new().height(Length::Fixed(0.0)).into()
    } else {
        let delete_pending =
            selected.is_some() && state.history_delete_pending == selected.map(|r| r.id);
        history_selected_panel(selected, delete_pending)
    };
    let header = container(
        column![
            search_row,
            text(count_line).size(12),
            state
                .history_status
                .as_deref()
                .map(|s| text(s).size(12).color(Color::from_rgb(0.95, 0.55, 0.25)))
                .unwrap_or_else(|| text("")),
            selected_panel,
        ]
        .spacing(6),
    )
    .padding([12, 16])
    .style(editor_chrome_style);

    // Records list — filtered.
    let mut col = column![].spacing(8).padding(iced::Padding {
        top: 0.0,
        right: 16.0,
        bottom: 16.0,
        left: 16.0,
    });
    if let Some(root) = state.history_root.as_ref() {
        if visible.is_empty() {
            let empty = if total == 0 {
                empty_state_card(
                    "No captures yet",
                    "Take a screenshot and it will appear here with its image, OCR text, and quick actions.",
                    Some(("Capture", Message::OpenOverlayRequested)),
                )
            } else {
                empty_state_card(
                    "No matching captures",
                    "Try a different search or clear the filter to see the full history.",
                    Some(("Clear Search", Message::HistorySearchChanged(String::new()))),
                )
            };
            col = col.push(container(empty).padding([28, 0]));
        }
        // Render at most a page of rows. The whole widget tree is
        // built eagerly (iced 0.14 has no virtual list), so without
        // this an Unlimited-retention history would build a thumbnail
        // widget for every record on open. The search filter above
        // still scans every record; only the rendered slice is capped.
        let page_limit = state.history_page_limit.max(1);
        for r in visible.iter().take(page_limit) {
            let png_path = history_png_path(root, r);
            let preview_path = history_thumbnail_path(root, r);
            let stamp = r
                .captured_at
                .with_timezone(&chrono::Local)
                .format("%Y-%m-%d  %H:%M:%S")
                .to_string();
            let dims = format!("{} × {} px", r.width_px, r.height_px);
            let snippet = r
                .ocr_text
                .as_ref()
                .map(|t| {
                    let trimmed = t.chars().take(120).collect::<String>();
                    if t.chars().count() > 120 {
                        format!("{trimmed}…")
                    } else {
                        trimmed
                    }
                })
                .unwrap_or_else(|| "(no OCR text yet)".to_string());

            let thumb: Element<Message> = if preview_path.exists() || png_path.exists() {
                let path = if preview_path.exists() {
                    preview_path
                } else {
                    png_path
                };
                // Letterbox the thumbnail inside a fixed 132×92 frame
                // with a dim background so tall portrait captures don't
                // sit flush against the row text — the dim border reads
                // as deliberate framing rather than image stretching.
                container(
                    image_widget(image_widget::Handle::from_path(path))
                        .width(Length::Fixed(132.0))
                        .height(Length::Fixed(92.0))
                        .content_fit(iced::ContentFit::Contain),
                )
                .width(Length::Fixed(132.0))
                .height(Length::Fixed(92.0))
                .center_x(Length::Fill)
                .center_y(Length::Fill)
                .style(|_| iced::widget::container::Style {
                    background: Some(Color::from_rgba(0.0, 0.0, 0.0, 0.22).into()),
                    border: iced::Border {
                        color: Color::from_rgba(1.0, 1.0, 1.0, 0.08),
                        width: 1.0,
                        radius: 6.0.into(),
                    },
                    ..Default::default()
                })
                .into()
            } else {
                container(text("Missing"))
                    .width(Length::Fixed(132.0))
                    .height(Length::Fixed(92.0))
                    .center_x(Length::Fill)
                    .center_y(Length::Fill)
                    .into()
            };

            let meta = column![
                text(stamp).size(13),
                text(dims)
                    .size(11)
                    .color(Color::from_rgba(1.0, 1.0, 1.0, 0.6)),
                text(snippet)
                    .size(11)
                    .color(Color::from_rgba(1.0, 1.0, 1.0, 0.7)),
            ]
            .spacing(4)
            .width(Length::Fill);

            let is_selected = state.history_selected_id == Some(r.id);
            let actions = history_row_actions(r.id, is_selected);
            let row_widget = container(
                row![thumb, meta, actions]
                    .spacing(12)
                    .align_y(Alignment::Center),
            )
            .padding(10)
            .style(move |_| iced::widget::container::Style {
                background: Some(
                    if is_selected {
                        history_selected_fill()
                    } else {
                        Color::from_rgba(1.0, 1.0, 1.0, 0.045)
                    }
                    .into(),
                ),
                border: iced::Border {
                    color: if is_selected {
                        history_selected_border()
                    } else {
                        Color::from_rgba(1.0, 1.0, 1.0, 0.075)
                    },
                    width: 1.0,
                    radius: 8.0.into(),
                },
                ..Default::default()
            });
            col = col.push(
                mouse_area(row_widget)
                    .on_press(Message::HistorySelect(r.id))
                    .on_double_click(Message::HistoryOpenInEditor(r.id))
                    .interaction(iced::mouse::Interaction::Pointer),
            );
        }
        // "Show more" pager — appears whenever matches exceed the
        // rendered page. Bounded incremental loading keeps open time
        // and memory flat for very large histories.
        let remaining = visible.len().saturating_sub(page_limit);
        if remaining > 0 {
            col = col.push(
                container(
                    button(text(format!("Show more ({remaining} more)")).size(12))
                        .padding([8, 14])
                        .style(|t, s| action_button_style(t, s, ActionKind::Secondary))
                        .on_press(Message::HistoryShowMore),
                )
                .center_x(Length::Fill)
                .padding([4, 0]),
            );
        }
    } else {
        col = col.push(container(empty_state_card(
            "History unavailable",
            "Readshot could not resolve the history folder for this session.",
            None,
        )));
    }

    container(
        column![
            header,
            scrollable(col)
                .direction(iced::widget::scrollable::Direction::Vertical(
                    slim_scrollbar(),
                ))
                .spacing(10.0)
                .height(Length::Fill)
        ]
        .spacing(12)
        .padding(12),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .style(editor_shell_style)
    .into()
}

pub(crate) fn history_selected_panel<'a>(
    record: Option<&'a readshot_core::CaptureRecord>,
    delete_pending: bool,
) -> Element<'a, Message> {
    let Some(record) = record else {
        return container(
            text("Select a capture to preview details. Use ↑/↓, Enter, and Delete in this window.")
                .size(11)
                .color(Color::from_rgba(1.0, 1.0, 1.0, 0.55)),
        )
        .padding([8, 10])
        .width(Length::Fill)
        .style(history_selected_panel_style)
        .into();
    };

    let stamp = record
        .captured_at
        .with_timezone(&chrono::Local)
        .format("%Y-%m-%d %H:%M:%S")
        .to_string();
    let text_count = record
        .ocr_text
        .as_deref()
        .map(|text| text.trim().chars().count())
        .unwrap_or(0);
    let has_text = text_count > 0;
    let id = record.id;

    container(responsive(move |available| {
        let summary = column![
            text("Selected capture")
                .size(11)
                .color(settings_muted_text()),
            text(format!(
                "{} · {} x {} px · {} OCR chars",
                stamp, record.width_px, record.height_px, text_count
            ))
            .size(12),
        ]
        .spacing(3);
        let secondary = |label: &'static str, msg: Message| {
            button(text(label).size(12))
                .padding([6, 9])
                .style(|t, s| action_button_style(t, s, ActionKind::Secondary))
                .on_press(msg)
        };
        let copy_text_button: Element<'_, Message> = if has_text {
            secondary("Copy Text", Message::HistoryCopyText(id)).into()
        } else {
            button(text("Copy Text").size(12))
                .padding([6, 9])
                .style(|t, s| action_button_style(t, s, ActionKind::Secondary))
                .into()
        };
        // `Open` is the most common action on a selected row (also
        // the Enter-key default) — promote it to Primary styling so
        // the eye lands on it instead of bouncing between three
        // identical Secondary buttons.
        let open_btn = button(text("Open").size(12))
            .padding([6, 9])
            .style(|t, s| action_button_style(t, s, ActionKind::Primary))
            .on_press(Message::HistoryOpenInEditor(id));
        let primary_actions = row![
            open_btn,
            secondary("Reveal", Message::HistoryReveal(id)),
            copy_text_button,
        ]
        .spacing(6)
        .align_y(Alignment::Center);
        // Two-stage single delete, mirroring "Clear All": the idle
        // button just arms the prompt; while armed it splits into a
        // Danger "Confirm delete?" and a Secondary "Cancel" so a
        // capture can't vanish on a single stray click or keypress.
        let delete_control: Element<'_, Message> = if delete_pending {
            row![
                button(text("Confirm delete?").size(12))
                    .padding([6, 9])
                    .style(|t, s| action_button_style(t, s, ActionKind::Danger))
                    .on_press(Message::HistoryDeleteConfirmed(id)),
                button(text("Cancel").size(12))
                    .padding([6, 9])
                    .style(|t, s| action_button_style(t, s, ActionKind::Secondary))
                    .on_press(Message::HistoryDeleteCancelled),
            ]
            .spacing(6)
            .align_y(Alignment::Center)
            .into()
        } else {
            button(text("Delete").size(12))
                .padding([6, 9])
                .style(|t, s| action_button_style(t, s, ActionKind::Danger))
                .on_press(Message::HistoryDelete(id))
                .into()
        };
        let secondary_actions = row![
            secondary("Copy Image", Message::HistoryCopyImage(id)),
            secondary("Pin", Message::HistoryPin(id)),
            delete_control,
        ]
        .spacing(6)
        .align_y(Alignment::Center);
        if available.width < 600.0 {
            column![summary, primary_actions, secondary_actions]
                .spacing(8)
                .into()
        } else if available.width < 820.0 {
            row![
                summary.width(Length::Fill),
                column![primary_actions, secondary_actions].spacing(6)
            ]
            .spacing(8)
            .align_y(Alignment::Center)
            .into()
        } else {
            row![
                summary.width(Length::Fill),
                primary_actions,
                secondary_actions
            ]
            .spacing(8)
            .align_y(Alignment::Center)
            .into()
        }
    }))
    .height(Length::Shrink)
    .padding([8, 10])
    .width(Length::Fill)
    .style(history_selected_panel_style)
    .into()
}

/// Shared accent colour for the history row's selected state — used
/// by the row background, the "Selected" chip, the selected border,
/// and the chip's foreground text so all three read as the same
/// accent rather than three sibling blues drifting apart.
pub(crate) const HISTORY_SELECTED_RGB: (f32, f32, f32) =
    (READSHOT_ACCENT.r, READSHOT_ACCENT.g, READSHOT_ACCENT.b);
pub(crate) const HISTORY_SELECTED_FILL_ALPHA: f32 = 0.14;
pub(crate) const HISTORY_SELECTED_CHIP_ALPHA: f32 = 0.20;
pub(crate) const HISTORY_SELECTED_BORDER_ALPHA: f32 = 0.55;

pub(crate) fn history_selected_fill() -> Color {
    let (r, g, b) = HISTORY_SELECTED_RGB;
    Color::from_rgba(r, g, b, HISTORY_SELECTED_FILL_ALPHA)
}

pub(crate) fn history_selected_chip() -> Color {
    let (r, g, b) = HISTORY_SELECTED_RGB;
    Color::from_rgba(r, g, b, HISTORY_SELECTED_CHIP_ALPHA)
}

pub(crate) fn history_selected_border() -> Color {
    let (r, g, b) = HISTORY_SELECTED_RGB;
    Color::from_rgba(r, g, b, HISTORY_SELECTED_BORDER_ALPHA)
}

pub(crate) fn history_selected_panel_style(_theme: &Theme) -> iced::widget::container::Style {
    iced::widget::container::Style {
        background: Some(Color::from_rgba(1.0, 1.0, 1.0, 0.055).into()),
        border: iced::Border {
            color: Color::from_rgba(1.0, 1.0, 1.0, 0.10),
            width: 1.0,
            radius: 8.0.into(),
        },
        ..Default::default()
    }
}

/// Compact row-level affordance. The whole row is already pressable
/// via `mouse_area`, so non-selected rows just show a chevron hint
/// that the row leads somewhere; selected rows show a labelled chip
/// so the active row is unambiguous. The real Open / Copy / Reveal
/// commands live in the selected-capture panel above the list.
pub(crate) fn history_row_actions<'a>(
    _id: readshot_core::Uuid,
    is_selected: bool,
) -> Element<'a, Message> {
    if is_selected {
        return container(
            text("Selected")
                .size(12)
                .color(Color::from_rgb8(214, 255, 247)),
        )
        .padding([6, 10])
        .style(|_| iced::widget::container::Style {
            background: Some(history_selected_chip().into()),
            border: iced::Border {
                radius: 6.0.into(),
                ..Default::default()
            },
            ..Default::default()
        })
        .into();
    }
    container(
        text("›")
            .size(18)
            .color(Color::from_rgba(1.0, 1.0, 1.0, 0.35)),
    )
    .padding([6, 12])
    .into()
}

// Search semantics live in readshot-core (shared with the MCP
// server's search_captures tool) so the two surfaces can't drift.
pub(crate) use readshot_core::history::record_matches_query as record_matches;

pub(crate) fn visible_history_ids(
    records: &[readshot_core::CaptureRecord],
    query: &str,
) -> Vec<readshot_core::Uuid> {
    let q = query.trim().to_lowercase();
    records
        .iter()
        .filter(|r| q.is_empty() || record_matches(r, &q))
        .map(|r| r.id)
        .collect()
}

pub(crate) fn history_record_visible(
    records: &[readshot_core::CaptureRecord],
    query: &str,
    id: readshot_core::Uuid,
) -> bool {
    visible_history_ids(records, query).contains(&id)
}

pub(crate) fn preferred_history_selection(
    records: &[readshot_core::CaptureRecord],
    query: &str,
    current: Option<readshot_core::Uuid>,
) -> Option<readshot_core::Uuid> {
    let visible = visible_history_ids(records, query);
    current
        .filter(|id| visible.contains(id))
        .or_else(|| visible.first().copied())
}

pub(crate) fn adjacent_history_selection(
    records: &[readshot_core::CaptureRecord],
    query: &str,
    current: Option<readshot_core::Uuid>,
    delta: isize,
) -> Option<readshot_core::Uuid> {
    let visible = visible_history_ids(records, query);
    if visible.is_empty() {
        return None;
    }
    let current_idx = current
        .and_then(|id| visible.iter().position(|candidate| *candidate == id))
        .unwrap_or(0);
    let max = visible.len() as isize - 1;
    let next_idx = (current_idx as isize + delta).clamp(0, max) as usize;
    visible.get(next_idx).copied()
}

pub(crate) fn visible_history_text(
    records: &[readshot_core::CaptureRecord],
    query: &str,
) -> String {
    let q = query.trim().to_lowercase();
    records
        .iter()
        .filter(|r| q.is_empty() || record_matches(r, &q))
        .filter_map(|r| r.ocr_text.as_deref())
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

pub(crate) fn visible_history_text_count(
    records: &[readshot_core::CaptureRecord],
    query: &str,
) -> usize {
    let q = query.trim().to_lowercase();
    records
        .iter()
        .filter(|r| q.is_empty() || record_matches(r, &q))
        .filter(|r| {
            r.ocr_text
                .as_deref()
                .is_some_and(|text| !text.trim().is_empty())
        })
        .count()
}

/// Load + decode a PNG file off the main thread so a multi-MB Retina
/// capture doesn't stall the iced runtime. Returns the decoded image
/// in the same `RgbaImage` shape the capture pipeline produces.
pub(crate) async fn load_png_async(path: PathBuf) -> Result<image::RgbaImage, image::ImageError> {
    tokio::task::spawn_blocking(move || -> Result<image::RgbaImage, image::ImageError> {
        let dyn_img = image::ImageReader::open(&path)?
            .with_guessed_format()?
            .decode()?;
        Ok(dyn_img.to_rgba8())
    })
    .await
    .unwrap_or_else(|join_err| {
        // Surface a join failure as an IO error so the caller's
        // `Result<RgbaImage, ImageError>` handling stays uniform.
        Err(image::ImageError::IoError(std::io::Error::other(format!(
            "blocking task panicked: {join_err}"
        ))))
    })
}

pub(crate) fn history_png_path(
    root: &std::path::Path,
    record: &readshot_core::CaptureRecord,
) -> PathBuf {
    readshot_core::FsHistoryStore::abs_png_path(root, record)
}

pub(crate) fn history_thumbnail_path(
    root: &std::path::Path,
    record: &readshot_core::CaptureRecord,
) -> PathBuf {
    readshot_core::FsHistoryStore::abs_thumbnail_path(root, record)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RevealCommand {
    pub(crate) program: &'static str,
    pub(crate) args: Vec<String>,
}

pub(crate) fn reveal_command_for_path(path: &Path) -> RevealCommand {
    let path_string = path.to_string_lossy().to_string();
    #[cfg(target_os = "macos")]
    {
        RevealCommand {
            program: "open",
            args: vec!["-R".into(), path_string],
        }
    }
    #[cfg(target_os = "windows")]
    {
        // explorer.exe needs the path in the SAME token as `/select,`
        // (`/select,C:\dir\file.png`). Passed as two separate argv entries
        // Explorer ignores the selection and just opens the default folder.
        RevealCommand {
            program: "explorer",
            args: vec![format!("/select,{path_string}")],
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let dir = path
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or(path_string);
        RevealCommand {
            program: "xdg-open",
            args: vec![dir],
        }
    }
}

pub(crate) fn reveal_path(path: &Path) -> Result<(), std::io::Error> {
    let command = reveal_command_for_path(path);
    std::process::Command::new(command.program)
        .args(command.args)
        .spawn()?;
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OpenFolderCommand {
    pub(crate) program: &'static str,
    pub(crate) args: Vec<String>,
}

pub(crate) fn open_folder_command_for_path(path: &Path) -> OpenFolderCommand {
    let path_string = path.to_string_lossy().to_string();
    #[cfg(target_os = "macos")]
    {
        OpenFolderCommand {
            program: "open",
            args: vec![path_string],
        }
    }
    #[cfg(target_os = "windows")]
    {
        OpenFolderCommand {
            program: "explorer",
            args: vec![path_string],
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        OpenFolderCommand {
            program: "xdg-open",
            args: vec![path_string],
        }
    }
}

pub(crate) fn open_folder_path(path: &Path) -> Result<(), std::io::Error> {
    let command = open_folder_command_for_path(path);
    std::process::Command::new(command.program)
        .args(command.args)
        .spawn()?;
    Ok(())
}

pub(crate) fn editor_view(state: &App) -> Element<'_, Message> {
    use iced::widget::canvas::Canvas;
    use iced::widget::{stack, Space as IcedSpace};
    use readshot_ui::editor::{canvas::EditorCanvas, toolbar, ToolState};

    let Some(ed) = state.editor.as_ref() else {
        return container(empty_state_card(
            "No capture open",
            "Start a capture to annotate, copy, save, pin, or extract text.",
            Some(("Capture", Message::OpenOverlayRequested)),
        ))
        .width(Length::Fill)
        .height(Length::Fill)
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .into();
    };

    let active_tool = ed.model.active_tool();
    let active_color = ed.model.current_color();
    let line_width = ed.model.current_line_width();
    let busy = ed.busy;
    let has_selected_annotation = ed.model.selected_annotation().is_some();

    // ===== Toolbar — adaptive, icon-only =====
    // Tools stay available in every layout. Secondary controls split
    // into more rows as width tightens so the editor does not need a
    // horizontal scrollbar just to expose color / size / undo.
    let undo_depth = ed.model.undo_depth();
    let redo_depth = ed.model.redo_depth();
    let can_undo = ed.model.can_undo();
    let can_redo = ed.model.can_redo();
    let undo_tip = if undo_depth > 0 {
        format!(
            "Undo (⌘Z) · {undo_depth} action{}",
            if undo_depth == 1 { "" } else { "s" }
        )
    } else {
        "Undo (⌘Z)".to_string()
    };
    let redo_tip = if redo_depth > 0 {
        format!(
            "Redo (⌘⇧Z) · {redo_depth} action{}",
            if redo_depth == 1 { "" } else { "s" }
        )
    } else {
        "Redo (⌘⇧Z)".to_string()
    };

    let toolbar_row = container(
        responsive(move |available| {
            let layout = editor_toolbar_layout(available.width);
            let tool_groups: &[&[ToolState]] = &[
                &[ToolState::Select],
                &[
                    ToolState::Rectangle,
                    ToolState::Ellipse,
                    ToolState::Line,
                    ToolState::Arrow,
                ],
                &[ToolState::Pen, ToolState::Highlighter, ToolState::Text],
                &[ToolState::Blur, ToolState::Pixelate],
                &[ToolState::NumberedPin],
                &[ToolState::Crop],
            ];

            // Group tools into recessed rounded "segments" so related
            // tools (shapes, marker/pen/text, redaction…) read as one
            // cluster instead of a flat strip separated by hairlines.
            let mut tool_row = row![].spacing(6).align_y(Alignment::Center);
            for group in tool_groups.iter() {
                let mut segment = row![].spacing(3).align_y(Alignment::Center);
                for t in group.iter() {
                    segment = segment.push(tool_button(*t, active_tool, busy));
                }
                tool_row = tool_row.push(toolbar_segment(segment.into()));
            }

            let palette_row = toolbar::PALETTE.iter().fold(
                row![].spacing(5).align_y(Alignment::Center),
                |row, swatch| {
                    let is_selected = swatch_eq(*swatch, active_color);
                    let color = Color::from_rgba(swatch.r, swatch.g, swatch.b, swatch.a);
                    let mut btn = button(
                        IcedSpace::new()
                            .width(Length::Fixed(20.0))
                            .height(Length::Fixed(20.0)),
                    )
                    .padding(0)
                    .style(move |_theme, status| {
                        // Swap the selected ring colour to a dark stroke when the
                        // swatch itself is bright, otherwise white-on-white makes
                        // the selected swatch invisible. sRGB relative luminance.
                        let lum = 0.2126 * swatch.r + 0.7152 * swatch.g + 0.0722 * swatch.b;
                        let selected_ring = if lum > 0.72 {
                            Color::from_rgba(0.0, 0.0, 0.0, 0.85)
                        } else {
                            Color::WHITE
                        };
                        // While a save / copy / ocr task is in flight the
                        // swatch has no `on_press`; dim it to match the
                        // other disabled controls so it doesn't read as
                        // clickable.
                        let dim = if busy { 0.4 } else { 1.0 };
                        let border = if is_selected {
                            iced::Border {
                                color: fade(selected_ring, dim),
                                width: 2.5,
                                radius: 6.0.into(),
                            }
                        } else if !busy && matches!(status, button::Status::Hovered) {
                            iced::Border {
                                color: Color::from_rgba(1.0, 1.0, 1.0, 0.55),
                                width: 1.5,
                                radius: 6.0.into(),
                            }
                        } else {
                            iced::Border {
                                color: Color::from_rgba(1.0, 1.0, 1.0, 0.18 * dim),
                                width: 1.0,
                                radius: 6.0.into(),
                            }
                        };
                        button::Style {
                            background: Some(fade(color, dim).into()),
                            text_color: Color::TRANSPARENT,
                            border,
                            ..Default::default()
                        }
                    });
                    if !busy {
                        btn = btn.on_press(Message::EditorToolbar(
                            readshot_ui::ToolbarMessage::SelectColor(*swatch),
                        ));
                    }
                    row.push(btn)
                },
            );

            let width_text_alpha = if busy { 0.42 } else { 0.7 };
            let width_prefix = if has_selected_annotation {
                "Sel"
            } else {
                "New"
            };
            let width_label = text(format!("{width_prefix} {line_width:.0}px"))
                .size(11)
                .color(Color::from_rgba(1.0, 1.0, 1.0, width_text_alpha))
                .width(Length::Fixed(54.0));
            let width_control_width = editor_width_control_width(layout);
            let width_control: Element<'_, Message> = if busy {
                passive_line_width_control_sized(line_width, width_control_width)
            } else {
                iced::widget::slider(
                    toolbar::MIN_LINE_WIDTH..=toolbar::MAX_LINE_WIDTH,
                    line_width,
                    Message::EditorLineWidthPreview,
                )
                .step(0.5)
                .on_release(Message::EditorLineWidthCommit)
                .width(Length::Fixed(width_control_width))
                .into()
            };
            let undo_btn = ghost_icon_button(
                crate::editor_icons::EditorIcon::Undo,
                undo_tip.clone(),
                !busy && can_undo,
                || Message::EditorToolbar(readshot_ui::ToolbarMessage::Undo),
            );
            let redo_btn = ghost_icon_button(
                crate::editor_icons::EditorIcon::Redo,
                redo_tip.clone(),
                !busy && can_redo,
                || Message::EditorToolbar(readshot_ui::ToolbarMessage::Redo),
            );
            let width_row = row![
                width_label,
                width_control,
                IcedSpace::new().width(Length::Fill),
                undo_btn,
                redo_btn,
            ]
            .spacing(10)
            .align_y(Alignment::Center)
            .width(Length::Fill);

            let toolbar_inner: Element<'_, Message> = match layout {
                EditorToolbarLayout::Wide => row![
                    tool_row,
                    toolbar_divider(),
                    palette_row,
                    toolbar_divider(),
                    width_row,
                ]
                .spacing(10)
                .align_y(Alignment::Center)
                .padding([6, 10])
                .width(Length::Fill)
                .into(),
                EditorToolbarLayout::Stacked => column![
                    tool_row,
                    row![palette_row, toolbar_divider(), width_row]
                        .spacing(10)
                        .align_y(Alignment::Center)
                ]
                .spacing(6)
                .padding([6, 10])
                .width(Length::Fill)
                .into(),
                EditorToolbarLayout::Compact => column![tool_row, palette_row, width_row]
                    .spacing(6)
                    .padding([6, 10])
                    .width(Length::Fill)
                    .into(),
            };

            toolbar_inner
        })
        // The `responsive` widget defaults to Fill on both axes, so without an
        // explicit Shrink height it claims half the editor column (it and the
        // image area are both Fill-height children). Constrain the widget itself —
        // a Shrink wrapper alone is ineffective because the Fill child still
        // resolves to the full available height.
        .height(Length::Shrink),
    )
    .width(Length::Fill)
    .height(Length::Shrink)
    .style(editor_chrome_style);

    // ===== Image area — letterboxed image with canvas overlay =====
    // Both layers share the same container; the canvas Program knows
    // the image's effective dimensions + crop offset so cursor maps
    // to base-image coordinates regardless of zoom / letterbox /
    // crop. See `canvas_to_base` in readshot-ui.
    let image_handle = ed.image_handle.clone();
    let (image_w, image_h) = ed.effective_image_size();
    let image_offset = ed.crop_offset();
    let frame_style = ed.frame_style;
    let zoom = ed.zoom;
    let display_scale = ed.display_scale;
    let next_pin_number = ed.next_pin_number;
    let pending_text = ed.pending_text.clone();
    let editor_shift_held = state.editor_shift_held;
    let drag_cancel_seq = ed.drag_cancel_seq;
    let selected_preview = ed
        .move_drag
        .as_ref()
        .filter(|drag| {
            drag.moved && can_preview_drag_annotation(&drag.baseline, drag.selected_index)
        })
        .and_then(|drag| ed.model.annotations().get(drag.selected_index).cloned());
    // Interior-mutable handle so the responsive closure can record the
    // scale it renders at; the runtime reads it back to hit-test grabs.
    let render_scale = ed.render_scale.clone();
    let image_area_content = responsive(move |available| {
        let iw = image_w as f32;
        let ih = image_h as f32;
        let fit_scale = editor_fit_scale(available, image_w, image_h);
        let scale = zoom.explicit_scale().unwrap_or(fit_scale);
        // Record the scale this frame renders at so the runtime's press
        // handlers hit-test handles/bodies with a screen-constant radius.
        render_scale.set(scale);
        let displayed_w = (iw * scale).max(1.0);
        let displayed_h = (ih * scale).max(1.0);
        let frame_preset =
            (frame_style != EditorFrameStyle::None).then(|| framed_image_preset(frame_style));
        let frame_pad = frame_preset
            .map(|preset| preset.pad as f32 * scale)
            .unwrap_or(0.0);
        let output_w = displayed_w + frame_pad * 2.0;
        let output_h = displayed_h + frame_pad * 2.0;
        let content_w = output_w.max(available.width);
        let content_h = output_h.max(available.height);

        let filter = editor_image_filter(scale, display_scale);

        let image_layer: Element<'_, Message> = container(
            iced::widget::image(image_handle.clone())
                .width(Length::Fixed(displayed_w))
                .height(Length::Fixed(displayed_h))
                .content_fit(iced::ContentFit::Contain)
                .filter_method(filter),
        )
        .width(Length::Fixed(displayed_w))
        .height(Length::Fixed(displayed_h))
        .into();

        let canvas_program = EditorCanvas {
            active_tool,
            color: active_color,
            line_width,
            next_pin_number,
            image_size: (image_w, image_h),
            image_offset,
            display_scale: Some(scale),
            selected_bounds: editor_show_selection_chrome(has_selected_annotation)
                .then(|| ed.model.selected_bounds())
                .flatten(),
            selected_annotation: editor_show_selection_chrome(has_selected_annotation)
                .then(|| {
                    ed.model
                        .selected_annotation()
                        .and_then(|idx| ed.model.annotations().get(idx).cloned())
                })
                .flatten(),
            selected_handles: if editor_show_selection_chrome(has_selected_annotation) {
                ed.model.selected_handles()
            } else {
                Vec::new()
            },
            selected_preview: selected_preview.clone(),
            shift_held: editor_shift_held,
            cancel_seq: drag_cancel_seq,
            cache: ed.canvas_cache.clone(),
        };
        let canvas: Element<'_, readshot_ui::CanvasMessage> = Canvas::new(canvas_program)
            .width(Length::Fixed(displayed_w))
            .height(Length::Fixed(displayed_h))
            .into();
        let canvas: Element<'_, Message> = canvas.map(Message::EditorCanvas);
        let canvas_layer = container(canvas)
            .width(Length::Fixed(displayed_w))
            .height(Length::Fixed(displayed_h));

        let editable_layers: Element<'_, Message> = if let Some(pending) = pending_text.clone() {
            let geom = inline_text_editor_geometry(
                pending.origin,
                (image_w, image_h),
                image_offset,
                scale,
            );
            let input: Element<'_, Message> = if busy {
                let content = if pending.content.is_empty() {
                    "Text draft paused while working...".to_string()
                } else {
                    pending.content.clone()
                };
                container(
                    text(content)
                        .size(14)
                        .color(Color::from_rgba(1.0, 1.0, 1.0, 0.58))
                        .wrapping(iced::widget::text::Wrapping::Word),
                )
                .padding(8)
                .width(Length::Fill)
                .style(disabled_text_input_style)
                .into()
            } else if let Some(content) = ed.text_edit_content.as_ref() {
                // Multi-line text: `text_editor` lets Enter insert a
                // newline. `key_binding` remaps Escape → cancel and
                // Cmd/Ctrl+Enter → commit; plain Enter falls through to
                // the default (newline). Clicking a button also commits
                // / cancels (below).
                iced::widget::text_editor(content)
                    .placeholder("Type text… (⌘↵ to commit, Esc to cancel)")
                    .on_action(Message::EditorTextAction)
                    .key_binding(editor_text_key_binding)
                    .padding(8)
                    .size(14)
                    .into()
            } else {
                // `text_edit_content` is always `Some` while a draft is
                // open; this fallback only guards a torn state.
                container(
                    text(pending.content.clone())
                        .size(14)
                        .wrapping(iced::widget::text::Wrapping::Word),
                )
                .padding(8)
                .width(Length::Fill)
                .into()
            };
            let commit_label = editor_text_commit_label(pending.edit_index.is_some());
            let mut commit = button(text(commit_label).size(12).color(Color::WHITE))
                .padding([7, 10])
                .style(|theme, status| action_button_style(theme, status, ActionKind::Primary));
            if !busy {
                commit = commit.on_press(Message::EditorTextCommit);
            }
            let mut cancel = button(text("Cancel").size(12).color(Color::WHITE))
                .padding([7, 10])
                .style(|theme, status| action_button_style(theme, status, ActionKind::Secondary));
            if !busy {
                cancel = cancel.on_press(Message::EditorTextCancel);
            }
            let editor = container(
                row![input, commit, cancel]
                    .spacing(6)
                    .align_y(Alignment::Center),
            )
            .padding(6)
            .width(Length::Fixed(geom.width))
            .style(|_theme: &Theme| {
                let mut style = editor_chrome_style(_theme);
                style.background = Some(Color::from_rgba(0.02, 0.025, 0.025, 0.88).into());
                style.border.color = accent(0.75);
                style.border.width = 1.5;
                style
            });
            let inline_layer = container(column![
                IcedSpace::new().height(Length::Fixed(geom.y)),
                row![IcedSpace::new().width(Length::Fixed(geom.x)), editor]
            ])
            .width(Length::Fixed(displayed_w))
            .height(Length::Fixed(displayed_h));

            stack![image_layer, canvas_layer, inline_layer].into()
        } else {
            stack![image_layer, canvas_layer].into()
        };

        let editable_stack: Element<'_, Message> = container(editable_layers)
            .width(Length::Fixed(displayed_w))
            .height(Length::Fixed(displayed_h))
            .clip(true)
            .style(move |_theme: &Theme| {
                if let Some(preset) = frame_preset {
                    container::Style {
                        background: preset.mat.map(|rgba| rgba_color(rgba).into()),
                        border: iced::Border {
                            radius: (preset.radius as f32 * scale).into(),
                            color: preset.border.map(rgba_color).unwrap_or(Color::TRANSPARENT),
                            width: if preset.border.is_some() { 1.0 } else { 0.0 },
                        },
                        ..Default::default()
                    }
                } else {
                    container::Style::default()
                }
            })
            .into();

        let preview_content: Element<'_, Message> = if let Some(preset) = frame_preset {
            container(editable_stack)
                .padding(frame_pad)
                .width(Length::Fixed(output_w))
                .height(Length::Fixed(output_h))
                .style(move |_theme: &Theme| frame_preview_container_style(preset, scale))
                .into()
        } else {
            editable_stack
        };

        let content = container(preview_content)
            .width(Length::Fixed(content_w))
            .height(Length::Fixed(content_h))
            .center_x(Length::Fill)
            .center_y(Length::Fill);

        let zoom_row = row![
            text("Zoom")
                .size(11)
                .color(Color::from_rgba(1.0, 1.0, 1.0, 0.55)),
            zoom_button(
                "-",
                "Zoom out",
                Message::EditorZoomOutFromDisplayScale(scale),
                zoom.can_zoom_out_from_display_scale(scale),
                false,
                busy,
            ),
            text(zoom.label_for_display_scale(display_scale))
                .size(11)
                .color(Color::from_rgba(1.0, 1.0, 1.0, 0.72))
                .width(Length::Fixed(50.0))
                .align_x(iced::alignment::Horizontal::Center),
        ];
        let zoom_controls = container(
            zoom_row
                .push(zoom_button(
                    "+",
                    "Zoom in",
                    Message::EditorZoomInFromDisplayScale(scale),
                    zoom.can_zoom_in_from_display_scale(scale),
                    false,
                    busy,
                ))
                .push(zoom_button(
                    "1:1",
                    "Actual pixels (100%)",
                    Message::EditorZoomActual,
                    true,
                    zoom.is_actual_size_for_display_scale(display_scale),
                    busy,
                ))
                .push(zoom_button(
                    "Fit",
                    "Fit to view",
                    Message::EditorZoomFit,
                    true,
                    zoom.is_fit(),
                    busy,
                ))
                .spacing(4)
                .align_y(Alignment::Center),
        )
        .padding([3, 6])
        .style(|_theme: &Theme| container::Style {
            background: Some(Color::from_rgba(0.02, 0.025, 0.025, 0.78).into()),
            border: iced::Border {
                color: accent(0.22),
                width: 1.0,
                radius: 8.0.into(),
            },
            ..Default::default()
        });
        let zoom_layer = container(zoom_controls)
            .width(Length::Fill)
            .height(Length::Fill)
            .padding(10)
            .align_x(Alignment::Start)
            .align_y(Alignment::End);

        let frame_picker: Element<'_, Message> = if busy {
            container(
                text(frame_style.label())
                    .size(10)
                    .color(Color::from_rgba(1.0, 1.0, 1.0, 0.62)),
            )
            .width(Length::Fixed(124.0))
            .padding([4, 6])
            .style(|_| iced::widget::container::Style {
                background: Some(Color::from_rgba(1.0, 1.0, 1.0, 0.06).into()),
                border: iced::Border {
                    radius: 6.0.into(),
                    color: Color::from_rgba(1.0, 1.0, 1.0, 0.10),
                    width: 1.0,
                },
                ..Default::default()
            })
            .into()
        } else {
            pick_list(
                &EditorFrameStyle::ALL[..],
                Some(frame_style),
                Message::EditorFrameStyleChanged,
            )
            .text_size(10)
            .width(Length::Fixed(124.0))
            .into()
        };
        let frame_controls = container(
            row![
                text("Frame:")
                    .size(10)
                    .color(Color::from_rgba(1.0, 1.0, 1.0, 0.48)),
                frame_picker
            ]
            .spacing(4)
            .align_y(Alignment::Center),
        )
        .padding([2, 4])
        .style(|_theme: &Theme| container::Style {
            background: Some(Color::from_rgba(0.02, 0.025, 0.025, 0.66).into()),
            border: iced::Border {
                color: accent(0.16),
                width: 1.0,
                radius: 7.0.into(),
            },
            ..Default::default()
        });
        let frame_layer = container(frame_controls)
            .width(Length::Fill)
            .height(Length::Fill)
            .padding(8)
            .align_x(Alignment::End)
            .align_y(Alignment::Start);

        let needs_horizontal_scroll = output_w > available.width + 0.5;
        let needs_vertical_scroll = output_h > available.height + 0.5;
        let scroll_layer: Element<'_, Message> = if needs_horizontal_scroll || needs_vertical_scroll
        {
            let scroll_direction = match (needs_horizontal_scroll, needs_vertical_scroll) {
                (true, true) => iced::widget::scrollable::Direction::Both {
                    vertical: slim_scrollbar(),
                    horizontal: slim_scrollbar(),
                },
                (true, false) => iced::widget::scrollable::Direction::Horizontal(slim_scrollbar()),
                (false, true) => iced::widget::scrollable::Direction::Vertical(slim_scrollbar()),
                (false, false) => unreachable!("scroll layer only wraps overflowing content"),
            };
            scrollable(content)
                .direction(scroll_direction)
                .width(Length::Fill)
                .height(Length::Fill)
                .into()
        } else {
            container(content)
                .width(Length::Fill)
                .height(Length::Fill)
                .into()
        };

        stack![scroll_layer, zoom_layer, frame_layer].into()
    })
    .width(Length::Fill)
    .height(Length::Fill);
    let image_area = container(image_area_content)
        .width(Length::Fill)
        .height(Length::Fill)
        .padding(8)
        .style(editor_stage_style);

    // ===== Bottom row — dims/hint + actions =====
    let (img_w, img_h) = ed.effective_image_size();
    // When the user just opened the editor and hasn't drawn anything
    // yet, show a discoverable "press a key to pick a tool" hint
    // in place of the per-tool guidance — the keyboard shortcuts
    // aren't visible anywhere in the chrome until the user hovers
    // a tool button, so this is the surface that surfaces them.
    let hint_str = if let Some(kind) = ed.model.selected_kind_label() {
        editor_selected_hint(kind, ed.model.selected_text_edit().is_some())
    } else if ed.model.annotations().is_empty() && ed.status.is_none() {
        "Pick a tool above — hover any tool to see its shortcut · ⌘Z to undo".to_string()
    } else {
        tool_hint(active_tool).to_string()
    };
    let hint_text = hint_str;
    // Auto-dismiss old success / info status text so the chrome
    // doesn't stay loud forever after a successful save / copy.
    // In-progress strings (Saving…, Copying…, Recognising text…) and
    // errors are kept until the next transition overwrites them — an
    // error the user didn't act on shouldn't vanish after 4s.
    let status_text = match (ed.status.clone(), ed.status_set_at) {
        (Some(s), Some(t))
            if !ed.status_is_in_progress()
                && !ed.status_is_error()
                && t.elapsed() > crate::editor::STATUS_AUTO_DISMISS =>
        {
            tracing::trace!(target: "readshot::editor", "auto-dismissed status: {s}");
            None
        }
        (s, _) => s,
    };
    let bottom_row: Element<'_, Message> = container(
        responsive(move |available| {
            let layout = editor_bottom_layout(available.width);
            let compact = layout == EditorBottomLayout::Compact;
            let dims = text(format!("{img_w} × {img_h} px"))
                .size(11)
                .color(Color::from_rgba(1.0, 1.0, 1.0, 0.55));
            let hint_copy = if compact {
                editor_compact_hint(active_tool).to_string()
            } else {
                hint_text.clone()
            };
            let hint = text(hint_copy)
                .size(11)
                .color(Color::from_rgba(1.0, 1.0, 1.0, 0.72))
                .width(Length::Fill)
                .wrapping(if compact {
                    iced::widget::text::Wrapping::Word
                } else {
                    iced::widget::text::Wrapping::None
                });
            let toast: Element<'_, Message> = match status_text.clone() {
                Some(s) => {
                    // Use the shared classifiers so the toast colour
                    // agrees with the auto-dismiss rule. The old local
                    // check only matched "copying"/"recognising", so
                    // "Saving…" and "Choose a save location…" rendered
                    // with the success (green) style instead of the
                    // neutral in-progress style.
                    let is_error = crate::editor::status_str_is_error(&s);
                    let in_progress = !is_error && crate::editor::status_str_is_in_progress(&s);
                    let (bg, fg) = if is_error {
                        (
                            Color::from_rgba(0.85, 0.32, 0.32, 0.22),
                            Color::from_rgba(1.0, 0.78, 0.78, 1.0),
                        )
                    } else if in_progress {
                        (
                            Color::from_rgba(1.0, 1.0, 1.0, 0.10),
                            Color::from_rgba(1.0, 1.0, 1.0, 0.85),
                        )
                    } else {
                        (
                            Color::from_rgba(0.30, 0.65, 0.45, 0.24),
                            Color::from_rgba(0.78, 1.0, 0.88, 1.0),
                        )
                    };
                    container(
                        text(s)
                            .size(11)
                            .color(fg)
                            .wrapping(iced::widget::text::Wrapping::Word),
                    )
                    .padding([3, 8])
                    .style(move |_| iced::widget::container::Style {
                        background: Some(bg.into()),
                        border: iced::Border {
                            radius: 9.0.into(),
                            ..Default::default()
                        },
                        ..Default::default()
                    })
                    .into()
                }
                None => IcedSpace::new().height(Length::Fixed(0.0)).into(),
            };
            let status_area = if compact {
                container(column![dims, hint, toast].spacing(3))
                    .width(Length::Fill)
                    .clip(true)
            } else {
                let bullet = text("·")
                    .size(11)
                    .color(Color::from_rgba(1.0, 1.0, 1.0, 0.35));
                container(
                    row![
                        dims,
                        IcedSpace::new().width(Length::Fixed(8.0)),
                        bullet,
                        IcedSpace::new().width(Length::Fixed(8.0)),
                        hint,
                        IcedSpace::new().width(Length::Fixed(8.0)),
                        toast,
                    ]
                    .spacing(0)
                    .align_y(Alignment::Center),
                )
                .width(Length::Fill)
                .clip(true)
            };

            let discard = editor_action_button(
                "Discard",
                Message::EditorDiscardRequested,
                ActionKind::Danger,
                busy,
                "Close this editor. Dirty edits ask for a second click.  ⌘W",
            );
            let pin = editor_action_button(
                "Pin",
                Message::EditorPinRequested,
                ActionKind::Secondary,
                busy,
                "Keep this capture floating above other windows.  ⌘P",
            );
            let copy_text = editor_action_button(
                "Copy Text",
                Message::EditorCopyTextRequested,
                ActionKind::Secondary,
                busy,
                "Run OCR and copy the recognized text.  ⌘⇧C",
            );
            let copy_image = editor_action_button(
                "Copy Image",
                Message::EditorCopyImageRequested,
                ActionKind::Secondary,
                busy,
                "Copy the annotated image using the selected frame option.  ⌘C",
            );
            let save = editor_action_button(
                "Save",
                Message::EditorSaveRequested,
                ActionKind::Primary,
                busy,
                "Save the annotated image using the selected frame option.  ⌘S",
            );

            match layout {
                EditorBottomLayout::Wide => row![
                    status_area,
                    row![
                        discard,
                        IcedSpace::new().width(Length::Fixed(8.0)),
                        copy_text,
                        copy_image,
                        pin,
                        toolbar_divider(),
                        save,
                    ]
                    .spacing(6)
                    .align_y(Alignment::Center)
                ]
                .spacing(8)
                .align_y(Alignment::Center)
                .into(),
                EditorBottomLayout::Stacked => column![
                    status_area,
                    row![discard, copy_text, copy_image, pin, toolbar_divider(), save]
                        .spacing(6)
                        .align_y(Alignment::Center)
                ]
                .spacing(8)
                .into(),
                EditorBottomLayout::Compact => column![
                    status_area,
                    row![discard, copy_text, copy_image]
                        .spacing(6)
                        .align_y(Alignment::Center),
                    row![pin, save].spacing(6).align_y(Alignment::Center),
                ]
                .spacing(8)
                .into(),
            }
        })
        // Same trap as the toolbar: `responsive` defaults to Fill height and
        // would split the editor column with the image area. Pin the widget to
        // Shrink so the bottom bar hugs its content.
        .height(Length::Shrink),
    )
    .padding([8, 10])
    .style(editor_chrome_style)
    .height(Length::Shrink)
    .into();

    container(
        column![toolbar_row, image_area, bottom_row]
            .spacing(8)
            .padding(10)
            .align_x(Alignment::Start),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .style(editor_shell_style)
    .into()
}

pub(crate) fn editor_shell_style(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Color::from_rgb8(13, 15, 15).into()),
        ..Default::default()
    }
}

pub(crate) fn editor_chrome_style(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Color::from_rgba(1.0, 1.0, 1.0, 0.055).into()),
        border: iced::Border {
            color: Color::from_rgba(1.0, 1.0, 1.0, 0.10),
            width: 1.0,
            radius: 8.0.into(),
        },
        ..Default::default()
    }
}

pub(crate) fn editor_stage_style(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Color::from_rgb8(16, 20, 20).into()),
        border: iced::Border {
            color: Color::from_rgba(1.0, 1.0, 1.0, 0.08),
            width: 1.0,
            radius: 8.0.into(),
        },
        ..Default::default()
    }
}

/// Render a pin window — borderless, always-on-top, draggable.
/// Click anywhere on the image body initiates a native window
/// drag; a small "×" button in the corner closes it.
pub(crate) fn pin_view(state: &App, id: window::Id) -> Element<'_, Message> {
    use iced::widget::{mouse_area, stack};
    let Some(pin) = state.pins.get(&id) else {
        return container(text("(no pin)"))
            .width(Length::Fill)
            .height(Length::Fill)
            .into();
    };
    let handle = pin.handle.clone();
    let opacity = pin.opacity;
    let locked = pin.locked;
    let img = iced::widget::image(handle)
        .width(Length::Fill)
        .height(Length::Fill)
        .content_fit(iced::ContentFit::Contain)
        .opacity(opacity);
    // Double-click toggles the lock instead of destroying the pin —
    // the audit flagged the previous "double-click closes" as a
    // landmine because pin-positioning involves a lot of accidental
    // double-clicks. Close is now exclusively the `×` button.
    let drag_area = mouse_area(img).on_double_click(Message::PinLockToggled(id));
    let drag_layer: Element<'_, Message> = if locked {
        drag_area.into()
    } else {
        drag_area
            .on_press(Message::PinDragRequested(id))
            .interaction(iced::mouse::Interaction::Grab)
            .into()
    };

    let lock = button(text(if locked { "Unlock" } else { "Lock" }).size(11))
        .padding([4, 8])
        .style(|_, status| {
            let bg = match status {
                button::Status::Hovered => Color::from_rgba(0.0, 0.0, 0.0, 0.72),
                _ => Color::from_rgba(0.0, 0.0, 0.0, 0.55),
            };
            button::Style {
                background: Some(bg.into()),
                text_color: Color::WHITE,
                border: iced::Border {
                    color: Color::from_rgba(1.0, 1.0, 1.0, 0.28),
                    width: 1.0,
                    radius: 6.0.into(),
                },
                ..Default::default()
            }
        })
        .on_press(Message::PinLockToggled(id));
    let opacity_label = text(format!("{:.0}%", opacity * 100.0))
        .size(11)
        .color(Color::WHITE)
        .width(Length::Shrink);
    let opacity_slider = iced::widget::slider(0.2..=1.0, opacity, move |value| {
        Message::PinOpacityChanged(id, value)
    })
    .step(0.05)
    .width(Length::Fixed(92.0));
    let controls = row![lock, opacity_slider, opacity_label]
        .spacing(6)
        .align_y(Alignment::Center);
    // Wrap the controls row in a `mouse_area` whose `on_press` is a
    // no-op (`Message::NoOp`) so iced consumes the press at the
    // controls layer instead of letting it fall through to the drag
    // layer beneath. Without this, the gap between the Lock button
    // and the slider — and any drag-attempt along the slider track —
    // moved the entire window because the underlying drag-press
    // fired before the slider widget could lock the gesture.
    let controls_chrome = container(controls)
        .padding([5, 7])
        .style(|_| container::Style {
            background: Some(Color::from_rgba(0.0, 0.0, 0.0, 0.46).into()),
            border: iced::Border {
                color: Color::from_rgba(1.0, 1.0, 1.0, 0.16),
                width: 1.0,
                radius: 8.0.into(),
            },
            ..Default::default()
        });
    let controls_blocker = mouse_area(controls_chrome).on_press(Message::NoOp);
    let controls_layer = container(controls_blocker)
        .width(Length::Fill)
        .height(Length::Fill)
        .padding(6)
        .align_x(Alignment::Start)
        .align_y(Alignment::End);

    // Close button — sits in the top-right corner with subtle
    // styling so it's discoverable without dominating the pin.
    let close = button(
        text("×")
            .size(20)
            .color(Color::WHITE)
            .align_x(iced::alignment::Horizontal::Center)
            .align_y(iced::alignment::Vertical::Center)
            .width(Length::Fill)
            .height(Length::Fill),
    )
    .padding(0)
    .width(Length::Fixed(28.0))
    .height(Length::Fixed(28.0))
    .style(|_, status| {
        let bg = match status {
            button::Status::Hovered => Color::from_rgba(0.85, 0.25, 0.25, 0.95),
            _ => Color::from_rgba(0.0, 0.0, 0.0, 0.55),
        };
        button::Style {
            background: Some(bg.into()),
            text_color: Color::WHITE,
            border: iced::Border {
                color: Color::from_rgba(1.0, 1.0, 1.0, 0.4),
                width: 1.0,
                radius: 14.0.into(),
            },
            ..Default::default()
        }
    })
    .on_press(Message::PinClosePressed(id));
    let close_layer = container(close)
        .width(Length::Fill)
        .height(Length::Fill)
        .padding(6)
        .align_x(Alignment::End)
        .align_y(Alignment::Start);
    container(stack![drag_layer, close_layer, controls_layer])
        .width(Length::Fill)
        .height(Length::Fill)
        .style(|_| container::Style {
            background: Some(Color::from_rgb8(13, 15, 15).into()),
            border: iced::Border {
                color: accent(0.20),
                width: 1.0,
                radius: 8.0.into(),
            },
            ..Default::default()
        })
        .into()
}

/// One toolbar tool button with a hover tooltip + keyboard hint.
pub(crate) fn tool_button<'a>(
    t: readshot_ui::editor::ToolState,
    active: readshot_ui::editor::ToolState,
    busy: bool,
) -> Element<'a, Message> {
    use iced::widget::tooltip;
    let (long, key) = tool_label_and_key(t);
    let is_active = t == active;
    let icon = crate::editor_icons::editor_icon(crate::editor_icons::EditorIcon::Tool(t), !busy);
    let mut b = button(icon)
        .padding(0)
        .width(Length::Fixed(32.0))
        .height(Length::Fixed(32.0))
        .style(move |theme: &Theme, status| toolbar_button_style(theme, status, is_active));
    if !busy {
        b = b.on_press(Message::EditorToolbar(
            readshot_ui::ToolbarMessage::SelectTool(t),
        ));
    }
    let tip = container(text(format!("{long}  {key}")).size(11).color(Color::WHITE))
        .padding([4, 8])
        .style(|_| container::Style {
            background: Some(Color::from_rgba(0.0, 0.0, 0.0, 0.9).into()),
            border: iced::Border {
                color: Color::from_rgba(1.0, 1.0, 1.0, 0.15),
                width: 1.0,
                radius: 6.0.into(),
            },
            ..Default::default()
        });
    tooltip::Tooltip::new(b, tip, tooltip::Position::Bottom)
        .gap(4)
        .into()
}

/// Compact ghost-style action button used for Undo / Redo. `gen`
/// produces the message lazily so we only build it when enabled.
pub(crate) fn ghost_icon_button<'a, F>(
    icon: crate::editor_icons::EditorIcon,
    tip: impl Into<String>,
    enabled: bool,
    gen: F,
) -> Element<'a, Message>
where
    F: Fn() -> Message + 'a,
{
    let tip = tip.into();
    use iced::widget::tooltip;
    let mut b = button(crate::editor_icons::editor_icon(icon, enabled))
        .padding(0)
        .width(Length::Fixed(32.0))
        .height(Length::Fixed(32.0))
        .style(move |theme: &Theme, status| toolbar_ghost_style(theme, status, enabled));
    if enabled {
        b = b.on_press(gen());
    }
    let pop = container(text(tip).size(11).color(Color::WHITE))
        .padding([4, 8])
        .style(|_| container::Style {
            background: Some(Color::from_rgba(0.0, 0.0, 0.0, 0.9).into()),
            border: iced::Border {
                color: Color::from_rgba(1.0, 1.0, 1.0, 0.15),
                width: 1.0,
                radius: 6.0.into(),
            },
            ..Default::default()
        });
    tooltip::Tooltip::new(b, pop, tooltip::Position::Bottom)
        .gap(4)
        .into()
}

/// Recessed rounded backing for a cluster of related toolbar buttons.
/// Groups tools visually without the noise of per-button hairlines.
pub(crate) fn toolbar_segment<'a>(content: Element<'a, Message>) -> Element<'a, Message> {
    container(content)
        .padding([2, 4])
        .style(|_theme: &Theme| container::Style {
            background: Some(Color::from_rgba(0.0, 0.0, 0.0, 0.22).into()),
            border: iced::Border {
                radius: 9.0.into(),
                ..Default::default()
            },
            ..Default::default()
        })
        .into()
}

/// 1px vertical divider between toolbar groups.
pub(crate) fn toolbar_divider() -> Element<'static, Message> {
    container(iced::widget::Space::new())
        .width(Length::Fixed(1.0))
        .height(Length::Fixed(20.0))
        .style(|_theme: &Theme| container::Style {
            background: Some(Color::from_rgba(1.0, 1.0, 1.0, 0.12).into()),
            ..Default::default()
        })
        .into()
}

pub(crate) fn passive_line_width_control_sized(
    width: f32,
    control_width: f32,
) -> Element<'static, Message> {
    let fill_w = control_width * line_width_fraction(width);
    container(
        container(
            iced::widget::Space::new()
                .width(Length::Fixed(fill_w))
                .height(Length::Fixed(4.0)),
        )
        .style(|_| container::Style {
            background: Some(accent(0.45).into()),
            border: iced::Border {
                radius: 2.0.into(),
                ..Default::default()
            },
            ..Default::default()
        }),
    )
    .width(Length::Fixed(control_width))
    .height(Length::Fixed(16.0))
    .padding([6, 0])
    .style(|_| container::Style {
        background: Some(Color::from_rgba(1.0, 1.0, 1.0, 0.07).into()),
        border: iced::Border {
            radius: 7.0.into(),
            ..Default::default()
        },
        ..Default::default()
    })
    .into()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EditorToolbarLayout {
    Wide,
    Stacked,
    Compact,
}

pub(crate) fn editor_toolbar_layout(width: f32) -> EditorToolbarLayout {
    if width < 640.0 {
        EditorToolbarLayout::Compact
    } else if width < 940.0 {
        EditorToolbarLayout::Stacked
    } else {
        EditorToolbarLayout::Wide
    }
}

pub(crate) fn editor_width_control_width(layout: EditorToolbarLayout) -> f32 {
    match layout {
        EditorToolbarLayout::Wide | EditorToolbarLayout::Stacked => 140.0,
        EditorToolbarLayout::Compact => 108.0,
    }
}

pub(crate) fn line_width_fraction(width: f32) -> f32 {
    let min = readshot_ui::editor::toolbar::MIN_LINE_WIDTH;
    let max = readshot_ui::editor::toolbar::MAX_LINE_WIDTH;
    ((width.clamp(min, max) - min) / (max - min)).clamp(0.0, 1.0)
}

pub(crate) fn disabled_text_input_style(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Color::from_rgba(1.0, 1.0, 1.0, 0.045).into()),
        border: iced::Border {
            color: Color::from_rgba(1.0, 1.0, 1.0, 0.10),
            width: 1.0,
            radius: 5.0.into(),
        },
        ..Default::default()
    }
}

pub(crate) fn toolbar_button_style(
    _theme: &Theme,
    status: button::Status,
    is_active: bool,
) -> button::Style {
    // Active tool gets a pronounced fill *and* a 2 px accent border
    // so the active state is unambiguous against any backdrop. The
    // previous styling relied solely on a subtle fill change that
    // washed out next to the rest of the toolbar.
    let base = if is_active {
        accent(0.22)
    } else if matches!(status, button::Status::Disabled) {
        Color::from_rgba(0.0, 0.0, 0.0, 0.0)
    } else if matches!(status, button::Status::Hovered) {
        Color::from_rgba(1.0, 1.0, 1.0, 0.075)
    } else {
        Color::TRANSPARENT
    };
    let border_color = if is_active {
        accent(0.85)
    } else {
        Color::from_rgba(1.0, 1.0, 1.0, 0.08)
    };
    let text_color = if matches!(status, button::Status::Disabled) {
        Color::from_rgba(1.0, 1.0, 1.0, 0.35)
    } else {
        Color::WHITE
    };
    button::Style {
        background: Some(base.into()),
        text_color,
        border: iced::Border {
            radius: 6.0.into(),
            width: if is_active { 1.5 } else { 1.0 },
            color: border_color,
        },
        ..Default::default()
    }
}

pub(crate) fn toolbar_ghost_style(
    _theme: &Theme,
    status: button::Status,
    enabled: bool,
) -> button::Style {
    let bg = match (enabled, status) {
        (true, button::Status::Hovered) => Color::from_rgba(1.0, 1.0, 1.0, 0.075),
        _ => Color::TRANSPARENT,
    };
    // Mirror the active toolbar button's disabled treatment so
    // undo / redo / icon ghost buttons fade out coherently when
    // `busy` removes their `on_press`.
    let disabled_or_inactive = !enabled || matches!(status, button::Status::Disabled);
    button::Style {
        background: Some(bg.into()),
        text_color: if disabled_or_inactive {
            Color::from_rgba(1.0, 1.0, 1.0, 0.35)
        } else {
            Color::WHITE
        },
        border: iced::Border {
            radius: 6.0.into(),
            width: 1.0,
            color: Color::from_rgba(
                1.0,
                1.0,
                1.0,
                if disabled_or_inactive { 0.05 } else { 0.10 },
            ),
        },
        ..Default::default()
    }
}

pub(crate) fn zoom_button<'a>(
    label: &'static str,
    tip: &'static str,
    msg: Message,
    enabled: bool,
    selected: bool,
    busy: bool,
) -> Element<'a, Message> {
    use iced::widget::tooltip;

    let mut b = button(text(label).size(11).color(Color::WHITE))
        .padding([5, 9])
        .style(move |theme, status| zoom_button_style(theme, status, enabled, selected));
    if !busy && enabled {
        b = b.on_press(msg);
    }
    let pop = container(text(tip).size(11).color(Color::WHITE))
        .padding([4, 8])
        .style(|_| container::Style {
            background: Some(Color::from_rgba(0.0, 0.0, 0.0, 0.9).into()),
            border: iced::Border {
                color: Color::from_rgba(1.0, 1.0, 1.0, 0.15),
                width: 1.0,
                radius: 6.0.into(),
            },
            ..Default::default()
        });
    tooltip::Tooltip::new(b, pop, tooltip::Position::Bottom)
        .gap(4)
        .into()
}

pub(crate) fn zoom_button_style(
    _theme: &Theme,
    status: button::Status,
    enabled: bool,
    selected: bool,
) -> button::Style {
    let background = if selected {
        READSHOT_PRIMARY
    } else if !enabled {
        Color::from_rgba(1.0, 1.0, 1.0, 0.03)
    } else if matches!(status, button::Status::Hovered) {
        Color::from_rgba(1.0, 1.0, 1.0, 0.14)
    } else {
        Color::from_rgba(1.0, 1.0, 1.0, 0.07)
    };
    button::Style {
        background: Some(background.into()),
        text_color: if enabled {
            Color::WHITE
        } else {
            Color::from_rgba(1.0, 1.0, 1.0, 0.35)
        },
        border: iced::Border {
            radius: 6.0.into(),
            width: if selected { 1.0 } else { 0.0 },
            color: Color::from_rgba(1.0, 1.0, 1.0, 0.20),
        },
        ..Default::default()
    }
}

#[derive(Clone, Copy)]
pub(crate) enum ActionKind {
    Primary,
    Secondary,
    Danger,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EditorBottomLayout {
    Wide,
    Stacked,
    Compact,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EditorBottomAction {
    Discard,
    CopyText,
    CopyImage,
    Pin,
    Save,
}

pub(crate) fn editor_bottom_layout(width: f32) -> EditorBottomLayout {
    if width < 520.0 {
        EditorBottomLayout::Compact
    } else if width < 860.0 {
        EditorBottomLayout::Stacked
    } else {
        EditorBottomLayout::Wide
    }
}

#[cfg(test)]
pub(crate) fn editor_bottom_action_rows(
    layout: EditorBottomLayout,
) -> Vec<Vec<EditorBottomAction>> {
    use EditorBottomAction as A;
    match layout {
        EditorBottomLayout::Wide | EditorBottomLayout::Stacked => {
            vec![vec![A::Discard, A::CopyText, A::CopyImage, A::Pin, A::Save]]
        }
        EditorBottomLayout::Compact => vec![
            vec![A::Discard, A::CopyText, A::CopyImage],
            vec![A::Pin, A::Save],
        ],
    }
}

pub(crate) fn editor_action_button<'a>(
    label: &'static str,
    msg: Message,
    kind: ActionKind,
    busy: bool,
    tip: &'static str,
) -> Element<'a, Message> {
    use iced::widget::tooltip;

    let lbl = text(label).size(13).color(Color::WHITE);
    let mut b = button(lbl)
        .padding([8, 16])
        .style(move |theme, status| action_button_style(theme, status, kind));
    if !busy {
        b = b.on_press(msg);
    }
    let pop = container(text(tip).size(11).color(Color::WHITE))
        .padding([4, 8])
        .style(|_| container::Style {
            background: Some(Color::from_rgba(0.0, 0.0, 0.0, 0.9).into()),
            border: iced::Border {
                color: Color::from_rgba(1.0, 1.0, 1.0, 0.15),
                width: 1.0,
                radius: 6.0.into(),
            },
            ..Default::default()
        });
    tooltip::Tooltip::new(b, pop, tooltip::Position::Top)
        .gap(4)
        .into()
}

pub(crate) fn action_button_style(
    _theme: &Theme,
    status: button::Status,
    kind: ActionKind,
) -> button::Style {
    let (base, hover, text_color, border_color) = match kind {
        ActionKind::Primary => (
            READSHOT_PRIMARY,
            READSHOT_ACCENT,
            Color::WHITE,
            accent(0.75),
        ),
        ActionKind::Secondary => (
            Color::from_rgba(1.0, 1.0, 1.0, 0.070),
            Color::from_rgba(1.0, 1.0, 1.0, 0.115),
            Color::WHITE,
            Color::from_rgba(1.0, 1.0, 1.0, 0.10),
        ),
        ActionKind::Danger => (
            Color::from_rgba(0.55, 0.13, 0.13, 0.92),
            Color::from_rgba(0.72, 0.18, 0.18, 0.98),
            Color::WHITE,
            Color::from_rgba(1.0, 0.40, 0.40, 0.22),
        ),
    };
    // Disabled state: dim the background to ~40% of its base alpha and
    // mute the text. Without this the button looks identical whether
    // its `on_press` is wired or not — confusing for the user when
    // there's nothing to copy / nothing to clear.
    let (bg, fg, border) = match status {
        button::Status::Hovered => (hover, text_color, border_color),
        button::Status::Disabled => {
            let dim = Color {
                a: base.a * 0.4,
                ..base
            };
            let muted = Color {
                a: 0.5,
                ..text_color
            };
            let muted_border = Color {
                a: border_color.a * 0.45,
                ..border_color
            };
            (dim, muted, muted_border)
        }
        _ => (base, text_color, border_color),
    };
    button::Style {
        background: Some(bg.into()),
        text_color: fg,
        border: iced::Border {
            radius: 8.0.into(),
            width: 1.0,
            color: border,
        },
        ..Default::default()
    }
}

/// Long name + keyboard shortcut shown in the tooltip when the user
/// hovers a tool button. Keys mirror common screenshot-editor muscle
/// memory (V/R/O/L/A/P/H/T/B/X/N/C).
pub(crate) fn tool_label_and_key(
    tool: readshot_ui::editor::ToolState,
) -> (&'static str, &'static str) {
    use readshot_ui::editor::ToolState as T;
    match tool {
        T::Select => ("Select", "V"),
        T::Rectangle => ("Rectangle", "R"),
        T::Ellipse => ("Ellipse", "O"),
        T::Line => ("Line", "L"),
        T::Arrow => ("Arrow", "A"),
        T::Pen => ("Pen", "P"),
        T::Highlighter => ("Highlighter", "H"),
        T::Text => ("Text", "T"),
        T::Blur => ("Blur", "B"),
        T::Pixelate => ("Pixelate", "X"),
        T::NumberedPin => ("Numbered pin", "N"),
        T::Crop => ("Crop", "C"),
    }
}

/// Map a single character to a tool. Used by the editor keyboard
/// subscription so muscle-memory shortcuts work without modifiers.
pub(crate) fn tool_for_key(c: &str) -> Option<readshot_ui::editor::ToolState> {
    use readshot_ui::editor::ToolState as T;
    match c.to_ascii_lowercase().as_str() {
        "v" => Some(T::Select),
        "r" => Some(T::Rectangle),
        "o" => Some(T::Ellipse),
        "l" => Some(T::Line),
        "a" => Some(T::Arrow),
        "p" => Some(T::Pen),
        "h" => Some(T::Highlighter),
        "t" => Some(T::Text),
        "b" => Some(T::Blur),
        "x" => Some(T::Pixelate),
        "n" => Some(T::NumberedPin),
        "c" => Some(T::Crop),
        _ => None,
    }
}

pub(crate) fn editor_key_message(
    key: iced::keyboard::Key,
    modifiers: iced::keyboard::Modifiers,
    status_ignored: bool,
) -> Option<Message> {
    use iced::keyboard::{key::Named, Key};

    let cmd = modifiers.command();
    match (&key, cmd, modifiers.shift()) {
        (Key::Character(c), true, false) if c.eq_ignore_ascii_case("z") => {
            return Some(Message::EditorToolbar(readshot_ui::ToolbarMessage::Undo));
        }
        (Key::Character(c), true, true) if c.eq_ignore_ascii_case("z") => {
            return Some(Message::EditorToolbar(readshot_ui::ToolbarMessage::Redo));
        }
        (Key::Character(c), true, false) if status_ignored && c.eq_ignore_ascii_case("c") => {
            return Some(Message::EditorCopyImageRequested);
        }
        (Key::Character(c), true, true) if status_ignored && c.eq_ignore_ascii_case("c") => {
            return Some(Message::EditorCopyTextRequested);
        }
        (Key::Character(c), true, false) if status_ignored && c.eq_ignore_ascii_case("p") => {
            return Some(Message::EditorPinRequested);
        }
        (Key::Character(c), true, false) if c.eq_ignore_ascii_case("s") => {
            return Some(Message::EditorSaveRequested);
        }
        (Key::Character(c), true, false) if c.eq_ignore_ascii_case("w") => {
            return Some(Message::EditorDiscardRequested);
        }
        (Key::Character(c), true, _) if c == "+" || c == "=" => {
            return Some(Message::EditorZoomIn);
        }
        (Key::Character(c), true, false) if c == "-" => {
            return Some(Message::EditorZoomOut);
        }
        (Key::Character(c), true, false) if c == "0" => {
            return Some(Message::EditorZoomActual);
        }
        (Key::Named(Named::Escape), _, _) if status_ignored => {
            return Some(Message::EditorDiscardRequested);
        }
        (Key::Named(Named::Escape), _, _) => return Some(Message::EditorTextCancel),
        _ => {}
    }

    if status_ignored && !cmd && !modifiers.alt() && !modifiers.control() {
        if matches!(
            key,
            Key::Named(Named::Delete) | Key::Named(Named::Backspace)
        ) {
            return Some(Message::EditorDeleteSelected);
        }
        if matches!(key, Key::Named(Named::Enter)) {
            return Some(Message::EditorEditSelectedText);
        }
        // Arrow keys nudge the selected annotation a pixel at a time
        // (Shift = 10px for coarse positioning). The runtime no-ops when
        // nothing is selected, so this is safe to emit unconditionally.
        let nudge_step = if modifiers.shift() { 10.0 } else { 1.0 };
        if matches!(key, Key::Named(Named::ArrowLeft)) {
            return Some(Message::EditorNudgeSelected(-nudge_step, 0.0));
        }
        if matches!(key, Key::Named(Named::ArrowRight)) {
            return Some(Message::EditorNudgeSelected(nudge_step, 0.0));
        }
        if matches!(key, Key::Named(Named::ArrowUp)) {
            return Some(Message::EditorNudgeSelected(0.0, -nudge_step));
        }
        if matches!(key, Key::Named(Named::ArrowDown)) {
            return Some(Message::EditorNudgeSelected(0.0, nudge_step));
        }
        if let Key::Character(c) = &key {
            if let Some(t) = tool_for_key(c.as_str()) {
                return Some(Message::EditorToolbar(
                    readshot_ui::ToolbarMessage::SelectTool(t),
                ));
            }
            if let Some(n) = c.chars().next().and_then(|ch| ch.to_digit(10)) {
                if (1..=9).contains(&n) {
                    return Some(Message::EditorToolbar(
                        readshot_ui::ToolbarMessage::SetLineWidth(n as f32),
                    ));
                }
            }
            match c.as_str() {
                "[" => return Some(Message::EditorWidthBump(-1.0)),
                "]" => return Some(Message::EditorWidthBump(1.0)),
                "," => return Some(Message::EditorColorCycle(-1)),
                "." => return Some(Message::EditorColorCycle(1)),
                _ => {}
            }
        }
    }

    None
}

/// One-line guidance for the currently active tool — replaces the
/// blank slate the user used to face when they didn't know what
/// click would do what.
pub(crate) fn tool_hint(tool: readshot_ui::editor::ToolState) -> &'static str {
    use readshot_ui::editor::ToolState as T;
    match tool {
        T::Select => "Select — click an annotation, drag to move, Delete removes, Esc deselects.",
        T::Rectangle => "Rectangle — drag to outline a region.",
        T::Ellipse => "Ellipse — drag the bounding box.",
        T::Line => "Line — drag from start to end.",
        T::Arrow => "Arrow — drag from base toward the target.",
        T::Pen => "Pen — drag to free-draw.",
        T::Highlighter => "Highlighter — drag over text; semi-transparent.",
        T::Text => "Text — click to place; type and press Enter to commit.",
        T::Blur => "Blur — drag a region; radius scales with width.",
        T::Pixelate => "Pixelate — drag a region; block size scales with width.",
        T::NumberedPin => "Numbered pin — click to drop the next number.",
        T::Crop => "Crop — drag to keep only that region.",
    }
}

pub(crate) fn editor_compact_hint(tool: readshot_ui::editor::ToolState) -> &'static str {
    use readshot_ui::editor::ToolState as T;
    match tool {
        T::Select => "Click to select · drag to move · Delete removes",
        T::Rectangle => "Drag to draw a rectangle.",
        T::Ellipse => "Drag to draw an ellipse.",
        T::Line => "Drag start to end.",
        T::Arrow => "Drag base to target.",
        T::Pen => "Drag to free-draw.",
        T::Highlighter => "Drag over text.",
        T::Text => "Click, type, Enter.",
        T::Blur => "Drag a region to blur.",
        T::Pixelate => "Drag a region to pixelate.",
        T::NumberedPin => "Click to drop a number.",
        T::Crop => "Drag the region to keep.",
    }
}

pub(crate) fn swatch_eq(a: readshot_core::Rgba, b: readshot_core::Rgba) -> bool {
    (a.r - b.r).abs() < 1e-3
        && (a.g - b.g).abs() < 1e-3
        && (a.b - b.b).abs() < 1e-3
        && (a.a - b.a).abs() < 1e-3
}

pub(crate) fn editor_fit_scale(available: iced::Size, image_w: u32, image_h: u32) -> f32 {
    let iw = image_w as f32;
    let ih = image_h as f32;
    if iw <= 0.0 || ih <= 0.0 {
        return 1.0;
    }
    (available.width / iw)
        .min(available.height / ih)
        .max(f32::EPSILON)
}

pub(crate) fn editor_image_filter(
    scale: f32,
    display_scale: f32,
) -> iced::widget::image::FilterMethod {
    let physical_scale = scale * display_scale.max(f32::EPSILON);
    if (physical_scale - 1.0).abs() < 0.001 {
        iced::widget::image::FilterMethod::Nearest
    } else {
        iced::widget::image::FilterMethod::Linear
    }
}

/// Whether to draw the selection outline / handles. Deliberately
/// independent of the active tool: a selection stays visible and
/// editable even after switching to a drawing tool, so the color/size
/// controls keep targeting it. Kept as a named predicate so that intent
/// is explicit at the call sites.
pub(crate) fn editor_show_selection_chrome(has_selection: bool) -> bool {
    has_selection
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct InlineTextEditorGeometry {
    pub(crate) x: f32,
    pub(crate) y: f32,
    pub(crate) width: f32,
    pub(crate) height: f32,
}

const INLINE_TEXT_EDITOR_WIDTH: f32 = 260.0;
const INLINE_TEXT_EDITOR_HEIGHT: f32 = 46.0;
const INLINE_TEXT_EDITOR_MARGIN: f32 = 8.0;

/// Key bindings for the inline multi-line text editor.
///
/// - `Escape` cancels the draft (parity with the old single-line input
///   and the Cancel button).
/// - `Cmd`/`Ctrl` + `Enter` commits the draft (the Enter key alone
///   inserts a newline, which is the whole point of the multi-line
///   editor).
/// - Everything else uses `text_editor`'s default binding, so plain
///   `Enter` breaks the line and motion/edit keys behave normally.
pub(crate) fn editor_text_key_binding(
    key_press: iced::widget::text_editor::KeyPress,
) -> Option<iced::widget::text_editor::Binding<Message>> {
    use iced::keyboard::key::Named;
    use iced::keyboard::Key;
    use iced::widget::text_editor::Binding;

    if matches!(key_press.key, Key::Named(Named::Enter)) && key_press.modifiers.command() {
        return Some(Binding::Custom(Message::EditorTextCommit));
    }
    if matches!(key_press.key, Key::Named(Named::Escape)) {
        return Some(Binding::Custom(Message::EditorTextCancel));
    }
    Binding::from_key_press(key_press)
}

pub(crate) fn inline_text_editor_geometry(
    origin: readshot_core::PointLike,
    image_size: (u32, u32),
    image_offset: (f32, f32),
    scale: f32,
) -> InlineTextEditorGeometry {
    let scale = scale.max(f32::EPSILON);
    let display_w = (image_size.0 as f32 * scale).max(1.0);
    let display_h = (image_size.1 as f32 * scale).max(1.0);
    let max_width = (display_w - INLINE_TEXT_EDITOR_MARGIN * 2.0).max(1.0);
    let width = INLINE_TEXT_EDITOR_WIDTH.min(max_width);
    let height =
        INLINE_TEXT_EDITOR_HEIGHT.min((display_h - INLINE_TEXT_EDITOR_MARGIN * 2.0).max(1.0));
    let desired_x = (origin.x - image_offset.0) * scale;
    let desired_y = (origin.y - image_offset.1) * scale;
    let max_x = (display_w - width - INLINE_TEXT_EDITOR_MARGIN).max(INLINE_TEXT_EDITOR_MARGIN);
    let max_y = (display_h - height - INLINE_TEXT_EDITOR_MARGIN).max(INLINE_TEXT_EDITOR_MARGIN);

    InlineTextEditorGeometry {
        x: desired_x.clamp(INLINE_TEXT_EDITOR_MARGIN, max_x),
        y: desired_y.clamp(INLINE_TEXT_EDITOR_MARGIN, max_y),
        width,
        height,
    }
}

pub(crate) fn overlay_view(state: &App, id: window::Id) -> Element<'_, Message> {
    use iced::widget::canvas::Canvas;
    use iced::widget::stack;

    // Each overlay window's canvas needs to know which display it
    // covers so the resulting `OverlaySelected` message routes the
    // capture to the right monitor. Fall back to an empty id only as
    // a defence against a view() call before OpenOverlayRequested
    // populated the map — that path won't actually publish a useful
    // message, but it avoids an unwrap.
    let overlay_record = state.overlay_displays.get(&id);
    let display_id = overlay_record
        .map(|d| d.display_id.clone())
        .unwrap_or_default();
    let scale = overlay_record.map(|d| d.scale).unwrap_or(1.0);
    let auto_confirm_intent = overlay_auto_confirm_intent(state);
    let cli_interactive = auto_confirm_intent == Some(crate::app::CaptureIntent::CliInteractive);

    let canvas = Canvas::new(crate::overlay::OverlayProgram {
        display_id,
        // Multiply the runtime tick to advance the dash pattern
        // smoothly (each dash period is ~10 logical px).
        dash_offset: state.overlay_tick as usize,
        scale,
        auto_confirm_intent,
        shift_held: state.overlay_shift_held,
    })
    .width(Length::Fill)
    .height(Length::Fill);

    let canvas_layer = container(canvas)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(|_theme| iced::widget::container::Style {
            // Fully transparent container so the canvas's veil + selection
            // is the only thing visible. Without this the iced default
            // background paints over the wgpu transparent layer.
            background: Some(Color::TRANSPARENT.into()),
            ..Default::default()
        });

    // Floating action toolbar — only when this display has a
    // committed selection. Layered above the canvas so its buttons
    // intercept clicks before the overlay's "click outside =
    // restart drag" path sees them.
    let toolbar_layer = if cli_interactive {
        None
    } else {
        overlay_record
            .and_then(|d| {
                state
                    .overlay_selections
                    .get(&d.display_id)
                    .map(|rect| (d, rect))
            })
            .map(|(d, rect)| overlay_toolbar_layer(&d.display_id, rect, d.width, d.height))
    };

    // Keep the overlay root as a stack even before the toolbar appears.
    // The canvas owns the in-progress selection state; changing the
    // root from `canvas` to `stack(canvas, toolbar)` on mouse-up makes
    // iced rebuild that state and the just-drawn region disappears.
    if let Some(toolbar) = toolbar_layer {
        stack![canvas_layer, toolbar].into()
    } else {
        stack![canvas_layer].into()
    }
}

pub(crate) fn overlay_auto_confirm_intent(state: &App) -> Option<crate::app::CaptureIntent> {
    if matches!(
        state.pending_intent,
        Some(crate::app::CaptureIntent::CliInteractive)
    ) || state.cli_interactive_output.is_some()
    {
        Some(crate::app::CaptureIntent::CliInteractive)
    } else if matches!(
        state.pending_intent,
        Some(crate::app::CaptureIntent::ScrollCapture)
    ) {
        // Tray "Scrolling Capture" pre-sets pending_intent so the
        // mouse-up / Enter default kicks straight into a scroll
        // session, no toolbar hunting required.
        Some(crate::app::CaptureIntent::ScrollCapture)
    } else {
        None
    }
}

/// Estimated visual size of the floating overlay toolbar. Used for
/// edge-aware reflow without measuring real layout (which iced
/// doesn't expose mid-build).
pub(crate) const OVERLAY_TOOLBAR_HEIGHT: f32 = 44.0;
pub(crate) const OVERLAY_TOOLBAR_GAP: f32 = 8.0;
/// Conservative estimate of the floating toolbar's rendered width.
/// iced doesn't expose mid-layout widget measurement, so the value is
/// hand-tuned against the actual button row (7 buttons, mostly short
/// after padding + the row's internal spacing). Slight over-estimate
/// is fine — it just means the clamp engages a few px earlier.
pub(crate) const OVERLAY_TOOLBAR_WIDTH: f32 = 660.0;

/// Build a positioned action toolbar (Capture / Copy / Save / Pin /
/// Cancel) anchored to the right edge of `rect`. Falls back to
/// "above" then "inside" if "below" would clip the window.
pub(crate) fn overlay_toolbar_layer<'a>(
    display_id: &readshot_capture::DisplayId,
    rect: &readshot_core::geom::Rect,
    bounds_w: f32,
    bounds_h: f32,
) -> Element<'a, Message> {
    use crate::app::CaptureIntent;

    use iced::widget::tooltip;
    let make_btn = |label: &'static str, tip: &'static str, msg: Message| -> Element<'a, Message> {
        let btn = button(text(label).size(13).color(Color::WHITE))
            .padding([6, 10])
            .style(|_, status| {
                let base = Color::from_rgba(1.0, 1.0, 1.0, 0.0);
                let hovered = accent(0.13);
                let pressed = accent(0.22);
                let bg = match status {
                    iced::widget::button::Status::Hovered => hovered,
                    iced::widget::button::Status::Pressed => pressed,
                    _ => base,
                };
                iced::widget::button::Style {
                    background: Some(bg.into()),
                    text_color: Color::WHITE,
                    border: iced::Border {
                        radius: 4.0.into(),
                        ..Default::default()
                    },
                    ..Default::default()
                }
            })
            .on_press(msg);
        let pop = container(text(tip).size(11).color(Color::WHITE))
            .padding([4, 8])
            .style(|_| container::Style {
                background: Some(Color::from_rgba(0.0, 0.0, 0.0, 0.9).into()),
                border: iced::Border {
                    color: Color::from_rgba(1.0, 1.0, 1.0, 0.15),
                    width: 1.0,
                    radius: 6.0.into(),
                },
                ..Default::default()
            });
        tooltip::Tooltip::new(btn, pop, tooltip::Position::Bottom)
            .gap(4)
            .into()
    };

    let buttons = row![
        make_btn(
            "Capture",
            "Open in editor (Enter)",
            Message::OverlaySelected {
                display_id: display_id.clone(),
                rect: *rect,
                intent: CaptureIntent::Editor,
            },
        ),
        make_btn(
            "Copy Image",
            "Copy selection to clipboard",
            Message::OverlaySelected {
                display_id: display_id.clone(),
                rect: *rect,
                intent: CaptureIntent::CopyToClipboard,
            },
        ),
        make_btn(
            "Copy Text",
            "Run OCR and copy recognized text",
            Message::OverlaySelected {
                display_id: display_id.clone(),
                rect: *rect,
                intent: CaptureIntent::CopyTextDirect,
            },
        ),
        make_btn(
            "Save",
            "Save to default folder",
            Message::OverlaySelected {
                display_id: display_id.clone(),
                rect: *rect,
                intent: CaptureIntent::SaveDirect,
            },
        ),
        make_btn(
            "Pin",
            "Pin as always-on-top window",
            Message::OverlaySelected {
                display_id: display_id.clone(),
                rect: *rect,
                intent: CaptureIntent::Pin,
            },
        ),
        make_btn(
            "Scroll Capture",
            "Scrolling capture — capture this region repeatedly as you scroll, then stitch into one tall image",
            Message::OverlaySelected {
                display_id: display_id.clone(),
                rect: *rect,
                intent: CaptureIntent::ScrollCapture,
            },
        ),
        make_btn("Cancel", "Cancel (Esc)", Message::OverlayCancelled),
    ]
    .spacing(4)
    .align_y(Alignment::Center);

    let bar = container(buttons)
        .padding(6)
        .style(|_| iced::widget::container::Style {
            background: Some(Color::from_rgba(0.05, 0.06, 0.06, 0.88).into()),
            border: iced::Border {
                radius: 8.0.into(),
                color: accent(0.30),
                width: 1.0,
            },
            ..Default::default()
        });

    let sel_x = rect.x();
    let sel_y = rect.y();
    let sel_w = rect.width();
    let sel_h = rect.height();
    let below_y = sel_y + sel_h + OVERLAY_TOOLBAR_GAP;
    let above_y = sel_y - OVERLAY_TOOLBAR_GAP - OVERLAY_TOOLBAR_HEIGHT;
    let toolbar_y = if below_y + OVERLAY_TOOLBAR_HEIGHT <= bounds_h {
        below_y
    } else if above_y >= 0.0 {
        above_y
    } else {
        // Both placements clip — fall back to inside the selection.
        sel_y + OVERLAY_TOOLBAR_GAP
    };

    // Anchor the toolbar's right edge at the selection's right edge,
    // but clamp so the toolbar never extends past the left edge of the
    // overlay when the selection sits near the left of the screen.
    let desired_right = (sel_x + sel_w).clamp(OVERLAY_TOOLBAR_WIDTH.min(bounds_w), bounds_w);
    let right_pad = (bounds_w - desired_right).max(0.0);

    container(bar)
        .width(Length::Fill)
        .height(Length::Fill)
        .padding(iced::Padding {
            top: toolbar_y.max(0.0),
            right: right_pad,
            bottom: 0.0,
            left: 0.0,
        })
        .align_x(Alignment::End)
        .align_y(Alignment::Start)
        .into()
}

/// Preferences window. v1 = a single General tab with the controls
/// users need regularly. The other `SettingsTab` variants stay on the
/// enum but don't render until they have content worth showing.
pub(crate) fn settings_view(state: &App) -> Element<'_, Message> {
    use iced::widget::{button, pick_list, text_input, toggler};
    use readshot_core::{ExportFormat, HistoryRetention, OcrEngineChoice, UpdateChannel};

    // `pick_list` borrows its options for the duration of the
    // returned `Element`, so a `'static` slice keeps the lifetime
    // story simple.
    const RETENTION_OPTIONS: [HistoryRetention; 4] = [
        HistoryRetention::Off,
        HistoryRetention::Last50,
        HistoryRetention::Last30Days,
        HistoryRetention::Unlimited,
    ];
    const FORMAT_OPTIONS: [ExportFormat; 3] =
        [ExportFormat::Png, ExportFormat::Jpeg, ExportFormat::Webp];
    const ENGINE_OPTIONS: [OcrEngineChoice; 2] =
        [OcrEngineChoice::Native, OcrEngineChoice::Tesseract];
    const CHANNEL_OPTIONS: [UpdateChannel; 2] = [UpdateChannel::Stable, UpdateChannel::Beta];

    let header = row![
        column![
            text("Settings").size(30),
            text("Capture, files, permissions, and startup")
                .size(13)
                .color(settings_muted_text()),
        ]
        .spacing(3),
        Space::new().width(Length::Fill),
    ]
    .align_y(Alignment::Center);

    let pretty = pretty_hotkey(&state.preferences.capture_hotkey);
    let hotkey_hint = if state.settings_recording_hotkey {
        state
            .settings_hotkey_error
            .clone()
            .unwrap_or_else(|| "Press a modifier shortcut now. Escape cancels.".to_string())
    } else if let Some(status) = &state.settings_hotkey_status {
        status.clone()
    } else if readshot_ui::hotkey::parse(&state.preferences.capture_hotkey).is_err() {
        format!(
            "Couldn't read `{}` — try `cmd+shift+x` style.",
            state.preferences.capture_hotkey
        )
    } else if !state.capture_hotkey_registered {
        // Parsed-OK but registration failed, e.g. another app already
        // owns the chord. Tell the user so they pick another one.
        format!("{pretty} — couldn't grab globally; try a different chord.")
    } else {
        format!("Currently bound to {pretty}.")
    };
    let hotkey_label = if state.settings_recording_hotkey {
        "Press shortcut…".to_string()
    } else {
        pretty_hotkey(&state.preferences.capture_hotkey)
    };

    let startup_row: Element<'_, Message> = toggler(state.preferences.launch_at_login)
        .label("Open Readshot at login")
        .on_toggle(|v| Message::Settings(SettingsMessage::SetLaunchAtLogin(v)))
        .into();

    let save_folder_value = if state.preferences.save_folder.as_os_str().is_empty() {
        "Platform default".to_string()
    } else {
        state.preferences.save_folder.display().to_string()
    };
    let filename_template = state.preferences.filename_template.clone();
    let permission_status = state.coordinator.pre_capture_gate();
    let (permission_title, permission_hint) = permission_settings_summary(permission_status);

    let history_retention = state.preferences.history_retention;
    let settings_recording_hotkey = state.settings_recording_hotkey;
    let permission_control: Element<'_, Message> = responsive(move |available| {
        let status = column![
            text(permission_title).size(13),
            text(permission_hint).size(11).color(settings_muted_text()),
        ]
        .spacing(6)
        .width(Length::Fill);

        if matches!(permission_status, PermissionStatus::NotApplicable) {
            return status.into();
        }

        let open_button = button(text("Open Settings"))
            .style(|t, s| action_button_style(t, s, ActionKind::Secondary))
            .on_press(Message::OpenPermissionSettingsRequested);
        // Recheck button forces a fresh permission poll so the user
        // can confirm a System Settings change without quitting and
        // relaunching the app. Drives the existing PermissionPoll
        // handler with the latest probe result.
        let recheck_button = button(text("Recheck"))
            .style(|t, s| action_button_style(t, s, ActionKind::Primary))
            .on_press(Message::PermissionTick);
        let actions = row![recheck_button, open_button]
            .spacing(8)
            .align_y(Alignment::Center);
        if available.width < 460.0 {
            column![status, actions].spacing(10).into()
        } else {
            row![status, actions]
                .spacing(10)
                .align_y(Alignment::Center)
                .into()
        }
    })
    .height(Length::Shrink)
    .into();
    let permission_section = settings_section("Permissions", "Capture access", permission_control);
    let capture_section = settings_section(
        "Capture",
        "Shortcut and history",
        responsive(move |available| {
            let hotkey_control: Element<'_, Message> = responsive({
                let hotkey_label = hotkey_label.clone();
                move |available| {
                    let value = setting_value_box(hotkey_label.clone(), settings_recording_hotkey);
                    // Record / Cancel are exclusive: while a recording
                    // is armed, the user needs an obvious exit that
                    // doesn't require pressing a real shortcut.
                    // Previously the Record button was a no-op once
                    // armed, leaving Esc as the only way out (and Esc
                    // wasn't documented).
                    let primary_btn = if settings_recording_hotkey {
                        button(text("Cancel"))
                            .style(|t, s| action_button_style(t, s, ActionKind::Secondary))
                            .on_press(Message::SettingsHotkeyRecordingCancelled)
                    } else {
                        button(text("Record"))
                            .style(|t, s| action_button_style(t, s, ActionKind::Primary))
                            .on_press(Message::SettingsStartHotkeyRecording)
                    };
                    let actions = row![
                        primary_btn,
                        button(text("Reset"))
                            .style(|t, s| action_button_style(t, s, ActionKind::Secondary))
                            .on_press(Message::Settings(SettingsMessage::SetCaptureHotkey(
                                default_capture_hotkey().into()
                            ),)),
                    ]
                    .spacing(8)
                    .align_y(Alignment::Center);

                    let main: Element<'_, Message> = if available.width < 360.0 {
                        column![value, actions].spacing(8).into()
                    } else {
                        row![value, actions]
                            .spacing(8)
                            .align_y(Alignment::Center)
                            .into()
                    };
                    if settings_recording_hotkey {
                        column![
                            main,
                            text("Press the new shortcut, or Esc to cancel.")
                                .size(11)
                                .color(accent(0.85)),
                        ]
                        .spacing(6)
                        .into()
                    } else {
                        main
                    }
                }
            })
            .height(Length::Shrink)
            .into();
            let hotkey_row = settings_field("Capture hotkey", hotkey_hint.clone(), hotkey_control);
            let retention_control: Element<'_, Message> =
                pick_list(&RETENTION_OPTIONS[..], Some(history_retention), |r| {
                    Message::Settings(SettingsMessage::SetHistoryRetention(r))
                })
                .into();
            let retention_row = settings_field(
                "History retention",
                "Searchable archive of captures. Off keeps everything in-memory only.",
                retention_control,
            );

            if available.width < 560.0 {
                column![hotkey_row, retention_row].spacing(14).into()
            } else {
                row![
                    column![hotkey_row].width(Length::FillPortion(3)),
                    column![retention_row].width(Length::FillPortion(2)),
                ]
                .spacing(18)
                .align_y(Alignment::Start)
                .into()
            }
        })
        .height(Length::Shrink)
        .into(),
    );
    let files_section = settings_section(
        "Files",
        "Save location and naming",
        responsive(move |available| {
            let save_folder_control: Element<'_, Message> = column![
                setting_value_box(save_folder_value.clone(), false),
                row![
                    button(text("Choose"))
                        .style(|t, s| action_button_style(t, s, ActionKind::Primary))
                        .on_press(Message::SettingsChooseSaveFolderRequested),
                    button(text("Open"))
                        .style(|t, s| action_button_style(t, s, ActionKind::Secondary))
                        .on_press(Message::SettingsOpenSaveFolderRequested),
                    button(text("Reset"))
                        .style(|t, s| action_button_style(t, s, ActionKind::Secondary))
                        .on_press(Message::Settings(SettingsMessage::SetSaveFolder(
                            PathBuf::new()
                        ))),
                ]
                .spacing(8)
                .align_y(Alignment::Center),
            ]
            .spacing(8)
            .into();
            let save_folder_row = settings_field(
                "Default save folder",
                "Starting folder for Save. Platform default uses your system screenshots location.",
                save_folder_control,
            );
            let filename_input =
                text_input("Screenshot {YYYY-MM-DD at HH.mm.ss}", &filename_template)
                    .on_input(|s| Message::Settings(SettingsMessage::SetFilenameTemplate(s)))
                    .padding([8, 10]);
            let preview_template = if filename_template.trim().is_empty() {
                "Screenshot {YYYY-MM-DD at HH.mm.ss}".to_string()
            } else {
                filename_template.clone()
            };
            let preview_name = default_save_filename(&preview_template, chrono::Utc::now());
            let preview_line = text(format!("Saves as: {preview_name}"))
                .size(11)
                .color(settings_muted_text());
            let filename_control: Element<'_, Message> =
                column![filename_input, preview_line].spacing(4).into();
            let filename_row = settings_field(
                "Filename template",
                "Tokens: {YYYY-MM-DD at HH.mm.ss} · {YYYY-MM-DD} · {HH.mm.ss} · {YYYY} · {timestamp}",
                filename_control,
            );

            if available.width < 620.0 {
                column![save_folder_row, filename_row].spacing(14).into()
            } else {
                row![
                    column![save_folder_row].width(Length::FillPortion(3)),
                    column![filename_row].width(Length::FillPortion(2)),
                ]
                .spacing(18)
                .align_y(Alignment::Start)
                .into()
            }
        })
        .height(Length::Shrink)
        .into(),
    );
    // --- Text recognition (OCR) ---------------------------------------
    let ocr_engine = state.preferences.ocr_engine_choice;
    let ocr_supported = state.coordinator.supported_languages();
    let ocr_selected = state.preferences.ocr_languages.clone();
    let ocr_section = settings_section(
        "Text recognition",
        "OCR engine and languages",
        column![
            settings_field(
                "OCR engine",
                "System default uses your platform's built-in recognizer. Takes effect after restart.",
                pick_list(&ENGINE_OPTIONS[..], Some(ocr_engine), |c| {
                    Message::Settings(SettingsMessage::SetOcrEngine(c))
                })
                .into(),
            ),
            settings_field(
                "Recognition languages",
                "Leave all off to let the recognizer choose. Applies to your next capture.",
                ocr_language_control(&ocr_supported, &ocr_selected),
            ),
        ]
        .spacing(14)
        .into(),
    );

    // --- Export --------------------------------------------------------
    let default_format = state.preferences.default_format;
    let export_section = settings_section(
        "Export",
        "Default image format",
        settings_field(
            "Default format",
            "Format used when you Save a capture.",
            pick_list(&FORMAT_OPTIONS[..], Some(default_format), |f| {
                Message::Settings(SettingsMessage::SetDefaultFormat(f))
            })
            .into(),
        ),
    );

    // --- Advanced ------------------------------------------------------
    let update_channel = state.preferences.update_channel;
    let debug_logging = state.preferences.debug_logging;
    let debug_toggle: Element<'_, Message> = toggler(debug_logging)
        .label("Verbose debug logging")
        .on_toggle(|v| Message::Settings(SettingsMessage::SetDebugLogging(v)))
        .into();
    let advanced_section = settings_section(
        "Advanced",
        "Updates and diagnostics",
        column![
            settings_field(
                "Update channel",
                "Beta receives pre-release builds sooner. Takes effect after restart.",
                pick_list(&CHANNEL_OPTIONS[..], Some(update_channel), |c| {
                    Message::Settings(SettingsMessage::SetUpdateChannel(c))
                })
                .into(),
            ),
            settings_field(
                "Diagnostics",
                "Logs at DEBUG level. Takes effect after restart.",
                debug_toggle,
            ),
        ]
        .spacing(14)
        .into(),
    );

    let reset_pending = state.settings_reset_all_pending;
    let reset_status = state
        .settings_status
        .as_deref()
        .unwrap_or("Return all preferences to the shipped defaults.")
        .to_string();
    let reset_controls: Element<'_, Message> = responsive(move |available| {
        let copy = if reset_pending {
            "Resets hotkey, save folder, filename template, retention, OCR, and startup options. History records are kept."
                .to_string()
        } else {
            reset_status.clone()
        };
        let label = text(copy).size(12).color(settings_muted_text());

        if reset_pending {
            let actions = row![
                button(text("Reset all"))
                    .padding([8, 14])
                    .style(|t, s| action_button_style(t, s, ActionKind::Danger))
                    .on_press(Message::SettingsResetAllConfirmed),
                button(text("Cancel"))
                    .padding([8, 14])
                    .style(|t, s| action_button_style(t, s, ActionKind::Secondary))
                    .on_press(Message::SettingsResetAllCancelled),
            ]
            .spacing(8)
            .align_y(Alignment::Center);

            if available.width < 460.0 {
                column![label, actions].spacing(8).into()
            } else {
                row![label, Space::new().width(Length::Fill), actions]
                    .spacing(8)
                    .align_y(Alignment::Center)
                    .into()
            }
        } else {
            let action =
                button(text("Reset all settings")).on_press(Message::SettingsResetAllRequested);
            if available.width < 460.0 {
                column![label, action].spacing(8).into()
            } else {
                row![label, Space::new().width(Length::Fill), action]
                    .spacing(8)
                    .align_y(Alignment::Center)
                    .into()
            }
        }
    })
    .height(Length::Shrink)
    .into();
    let app_section = settings_section(
        "App",
        "Startup behavior",
        column![startup_row, reset_controls].spacing(12).into(),
    );

    let body = container(
        column![
            header,
            permission_section,
            capture_section,
            files_section,
            ocr_section,
            export_section,
            advanced_section,
            app_section,
        ]
        .spacing(16)
        .max_width(720),
    )
    .width(Length::Fill)
    .center_x(Length::Fill)
    // The scrollbar sits at the window edge. Because iced reserves a
    // scrollbar lane on the right, equal content padding reads as
    // right-heavy; offset the inner padding so the settings column
    // appears optically centered.
    .padding(iced::Padding {
        top: 26.0,
        right: 24.0,
        bottom: 26.0,
        left: 40.0,
    });

    container(
        scrollable(body)
            .direction(iced::widget::scrollable::Direction::Vertical(
                slim_scrollbar(),
            ))
            .spacing(10.0)
            .height(Length::Fill),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .style(editor_shell_style)
    .into()
}

pub(crate) fn settings_section<'a>(
    title: &'a str,
    subtitle: &'a str,
    content: Element<'a, Message>,
) -> Element<'a, Message> {
    container(
        column![
            responsive(move |available| {
                if available.width < 420.0 {
                    column![
                        text(title).size(16),
                        text(subtitle).size(11).color(settings_muted_text()),
                    ]
                    .spacing(3)
                    .into()
                } else {
                    row![
                        text(title).size(16),
                        Space::new().width(Length::Fill),
                        text(subtitle).size(11).color(settings_muted_text()),
                    ]
                    .align_y(Alignment::Center)
                    .into()
                }
            }),
            content,
        ]
        .spacing(14),
    )
    .width(Length::Fill)
    .padding(16)
    .style(settings_section_style)
    .into()
}

pub(crate) fn settings_field<'a>(
    label: &'a str,
    hint: impl Into<String>,
    control: Element<'a, Message>,
) -> Element<'a, Message> {
    column![
        text(label).size(13),
        control,
        text(hint.into()).size(11).color(settings_muted_text()),
    ]
    .spacing(7)
    .into()
}

/// Simple multi-select for OCR languages: one checkbox per language the
/// active engine reports as supported. Toggling rebuilds the ordered list
/// (insertion order becomes recognition priority). An empty supported
/// list — e.g. the Linux ocrs engine, which reports a single language —
/// renders an explanatory note instead.
pub(crate) fn ocr_language_control(
    supported: &[String],
    selected: &[String],
) -> Element<'static, Message> {
    if supported.is_empty() {
        return setting_value_box(
            "No languages reported by the recognizer.".to_string(),
            false,
        );
    }
    let mut items: Vec<Element<'static, Message>> = Vec::with_capacity(supported.len());
    for lang in supported {
        let lang = lang.clone();
        let checked = selected.iter().any(|l| l == &lang);
        let selected_now = selected.to_vec();
        let toggle_lang = lang.clone();
        items.push(
            iced::widget::checkbox(checked)
                .label(lang)
                .size(16)
                .text_size(13)
                .on_toggle(move |now| {
                    let mut next = selected_now.clone();
                    if now {
                        if !next.iter().any(|l| l == &toggle_lang) {
                            next.push(toggle_lang.clone());
                        }
                    } else {
                        next.retain(|l| l != &toggle_lang);
                    }
                    Message::Settings(SettingsMessage::SetOcrLanguages(next))
                })
                .into(),
        );
    }
    iced::widget::Column::with_children(items).spacing(6).into()
}

pub(crate) fn setting_value_box(value: String, active: bool) -> Element<'static, Message> {
    container(
        text(value)
            .size(14)
            .color(if active {
                Color::from_rgb8(214, 255, 247)
            } else {
                Color::from_rgba(1.0, 1.0, 1.0, 0.82)
            })
            .width(Length::Fill),
    )
    .width(Length::Fill)
    .padding([9, 12])
    .style(move |theme: &Theme| {
        let palette = theme.extended_palette();
        let border_color = if active {
            palette.primary.base.color
        } else {
            Color::from_rgba(1.0, 1.0, 1.0, 0.12)
        };
        iced::widget::container::Style {
            background: Some(
                if active {
                    accent(0.10)
                } else {
                    Color::from_rgba(1.0, 1.0, 1.0, 0.055)
                }
                .into(),
            ),
            border: iced::Border {
                color: border_color,
                width: if active { 2.0 } else { 1.0 },
                radius: 7.0.into(),
            },
            ..Default::default()
        }
    })
    .into()
}

pub(crate) fn empty_state_card<'a>(
    title: &'static str,
    body: &'static str,
    action: Option<(&'static str, Message)>,
) -> Element<'a, Message> {
    let mut content = column![
        text(title).size(18),
        text(body)
            .size(12)
            .color(settings_muted_text())
            .width(Length::Fill),
    ]
    .spacing(8)
    .max_width(420);

    if let Some((label, msg)) = action {
        content = content.push(
            button(text(label).size(13).color(Color::WHITE))
                .padding([8, 16])
                .style(|theme, status| action_button_style(theme, status, ActionKind::Primary))
                .on_press(msg),
        );
    }

    container(content)
        .padding(18)
        .width(Length::Fill)
        .style(settings_section_style)
        .into()
}

pub(crate) fn settings_section_style(_theme: &Theme) -> iced::widget::container::Style {
    iced::widget::container::Style {
        background: Some(Color::from_rgba(1.0, 1.0, 1.0, 0.055).into()),
        border: iced::Border {
            color: Color::from_rgba(1.0, 1.0, 1.0, 0.10),
            width: 1.0,
            radius: 8.0.into(),
        },
        ..Default::default()
    }
}

pub(crate) fn settings_muted_text() -> Color {
    Color::from_rgba(1.0, 1.0, 1.0, 0.60)
}

pub(crate) fn permission_settings_summary(
    status: PermissionStatus,
) -> (&'static str, &'static str) {
    match status {
        PermissionStatus::Granted => (
            "Screen Recording is allowed",
            "Readshot can open the capture overlay and recognise text from screenshots.",
        ),
        PermissionStatus::Denied => (
            "Screen Recording needs attention",
            "Enable Readshot in macOS System Settings. If it is already enabled, restart Readshot so macOS refreshes the grant.",
        ),
        PermissionStatus::NotApplicable => (
            "No extra capture permission required",
            "This platform does not need a separate Screen Recording grant.",
        ),
    }
}

pub(crate) fn slim_scrollbar() -> iced::widget::scrollable::Scrollbar {
    iced::widget::scrollable::Scrollbar::new()
        .width(7.0)
        .scroller_width(4.0)
        .margin(2.0)
}

pub(crate) fn welcome_view(state: &App) -> Element<'_, Message> {
    let hero = column![
        text("Readshot").size(32),
        text("Capture, search, find again.")
            .size(13)
            .color(settings_muted_text()),
        container(Space::new())
            .width(Length::Fixed(44.0))
            .height(Length::Fixed(2.0))
            .style(|_| container::Style {
                background: Some(accent(0.85).into()),
                border: iced::Border {
                    radius: 1.0.into(),
                    ..Default::default()
                },
                ..Default::default()
            }),
    ]
    .spacing(7)
    .align_x(Alignment::Center);

    let card: Element<'_, Message> = match state.welcome {
        WelcomeState::Pending => welcome_pending_card(),
        WelcomeState::AwaitingGrant | WelcomeState::Denied => welcome_awaiting_card(state.welcome),
        WelcomeState::Granted => welcome_granted_card(state),
    };

    let toast: Element<'_, Message> = match &state.last_capture_status {
        Some(s) => text(s)
            .size(11)
            .color(Color::from_rgba(1.0, 1.0, 1.0, 0.55))
            .into(),
        None => Space::new().height(Length::Fixed(0.0)).into(),
    };

    let inner = column![
        hero,
        Space::new().height(Length::Fixed(20.0)),
        card,
        Space::new().height(Length::Fixed(10.0)),
        toast,
    ]
    .max_width(420)
    .align_x(Alignment::Center);

    container(inner)
        .width(Length::Fill)
        .height(Length::Fill)
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .padding(28)
        .style(editor_shell_style)
        .into()
}

/// Severity tint for a welcome card. `Neutral` is the default
/// glassy chrome; `Blocked` warms it up with an amber accent so the
/// Denied state reads as "needs your attention" instead of looking
/// the same as the cold Pending state.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum WelcomeCardTone {
    Neutral,
    Blocked,
}

/// Shared chrome for every welcome card — soft semi-transparent
/// background, subtle border, generous padding so the action button
/// has room to breathe. `tone` lets the Denied card stand out.
pub(crate) fn welcome_card<'a>(content: Element<'a, Message>) -> Element<'a, Message> {
    welcome_card_with_tone(content, WelcomeCardTone::Neutral)
}

pub(crate) fn welcome_card_with_tone<'a>(
    content: Element<'a, Message>,
    tone: WelcomeCardTone,
) -> Element<'a, Message> {
    container(content)
        .padding(20)
        .width(Length::Fill)
        .style(move |_| {
            let (bg, border) = match tone {
                WelcomeCardTone::Neutral => (
                    Color::from_rgba(1.0, 1.0, 1.0, 0.055),
                    Color::from_rgba(1.0, 1.0, 1.0, 0.12),
                ),
                WelcomeCardTone::Blocked => (
                    Color::from_rgba(0.95, 0.55, 0.30, 0.10),
                    Color::from_rgba(0.95, 0.55, 0.30, 0.55),
                ),
            };
            iced::widget::container::Style {
                background: Some(bg.into()),
                border: iced::Border {
                    radius: 8.0.into(),
                    color: border,
                    width: 1.0,
                },
                ..Default::default()
            }
        })
        .into()
}

pub(crate) fn welcome_pending_card<'a>() -> Element<'a, Message> {
    let body = column![
        text("Allow Screen Recording").size(18),
        text(
            "macOS will pop a permission prompt. Click \"Open System Settings\" \
             inside it and toggle Readshot on — that's all we need."
        )
        .size(13)
        .color(Color::from_rgba(1.0, 1.0, 1.0, 0.7)),
        Space::new().height(Length::Fixed(6.0)),
        button(text("Allow Screen Recording").size(14))
            .padding([10, 18])
            .style(|t, s| action_button_style(t, s, ActionKind::Primary))
            .on_press(Message::GrantPermissionRequested),
    ]
    .spacing(10)
    .align_x(Alignment::Center);
    welcome_card(body.into())
}

pub(crate) fn welcome_awaiting_card<'a>(state: WelcomeState) -> Element<'a, Message> {
    let (title, body_copy) = welcome_permission_guidance(state);
    let is_denied = matches!(state, WelcomeState::Denied);
    let header_label = if is_denied {
        format!("Attention: {title}")
    } else {
        title.to_string()
    };
    let header_color = if is_denied {
        Color::from_rgba(1.0, 0.78, 0.55, 0.95)
    } else {
        Color::WHITE
    };
    let body = column![
        text(header_label).size(18).color(header_color),
        text(body_copy)
            .size(13)
            .color(Color::from_rgba(1.0, 1.0, 1.0, 0.7)),
        Space::new().height(Length::Fixed(6.0)),
        row![
            button(text("Open System Settings").size(14))
                .padding([10, 18])
                .style(|t, s| action_button_style(t, s, ActionKind::Primary))
                .on_press(Message::OpenPermissionSettingsRequested),
            button(text("Restart Readshot").size(14))
                .padding([10, 16])
                .style(|t, s| action_button_style(t, s, ActionKind::Secondary))
                .on_press(Message::RestartRequested),
        ]
        .spacing(8),
    ]
    .spacing(10)
    .align_x(Alignment::Center);
    let tone = if is_denied {
        WelcomeCardTone::Blocked
    } else {
        WelcomeCardTone::Neutral
    };
    welcome_card_with_tone(body.into(), tone)
}

pub(crate) fn welcome_permission_guidance(state: WelcomeState) -> (&'static str, &'static str) {
    match state {
        WelcomeState::Denied => (
            "Permission still blocked",
            "If Readshot is already toggled on in System Settings, restart now so macOS gives this process the new Screen Recording grant. Otherwise open System Settings and enable Readshot first.",
        ),
        _ => (
            "Waiting for permission",
            "Enable Readshot in System Settings. If macOS offers Quit & Reopen, accept it; otherwise use Restart Readshot after toggling the permission on.",
        ),
    }
}

pub(crate) fn welcome_granted_card(state: &App) -> Element<'_, Message> {
    let mut capture_btn = button(welcome_button_label("Capture Screen", 14, 136.0, 40.0))
        .padding(0)
        .style(|t, s| action_button_style(t, s, ActionKind::Primary));
    if !state.capture_in_flight {
        capture_btn = capture_btn.on_press(Message::OpenOverlayRequested);
    }
    let history_btn = button(welcome_button_label("Show History", 13, 104.0, 34.0))
        .padding(0)
        .style(|t, s| action_button_style(t, s, ActionKind::Secondary))
        .on_press(Message::OpenHistoryRequested);
    let settings_btn = button(welcome_button_label("Open Settings", 13, 110.0, 34.0))
        .padding(0)
        .style(|t, s| action_button_style(t, s, ActionKind::Secondary))
        .on_press(Message::OpenSettingsRequested);
    let hotkey = pretty_hotkey(&state.preferences.capture_hotkey);
    let body = column![
        text("You're all set.").size(17),
        text(format!(
            "Press {hotkey} or click the menu-bar icon to capture. Every \
             capture lands in History — searchable by anything visible \
             in the image."
        ))
        .size(13)
        .color(Color::from_rgba(1.0, 1.0, 1.0, 0.7)),
        Space::new().height(Length::Fixed(6.0)),
        capture_btn,
        Space::new().height(Length::Fixed(2.0)),
        row![history_btn, settings_btn]
            .spacing(10)
            .align_y(Alignment::Center),
    ]
    .spacing(10)
    .align_x(Alignment::Center);
    welcome_card(body.into())
}

pub(crate) fn welcome_button_label<'a>(
    label: &'static str,
    size: u32,
    width: f32,
    height: f32,
) -> Element<'a, Message> {
    container(
        text(label)
            .size(size)
            .line_height(iced::widget::text::LineHeight::Relative(1.0)),
    )
    .width(Length::Fixed(width))
    .height(Length::Fixed(height))
    .align_x(iced::alignment::Horizontal::Center)
    .align_y(iced::alignment::Vertical::Center)
    .into()
}

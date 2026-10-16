// Extracted from runtime.rs (pure code-move). `use super::*` pulls in
// sibling/parent items; the explicit imports mirror runtime.rs's preamble.
use super::*;

use std::path::{Path, PathBuf};

use iced::widget::{button, column, container, responsive, row, scrollable, text, Space};
use iced::window;
use iced::{Alignment, Color, Element, Length, Theme};

use readshot_ui::SettingsMessage;

use crate::app::{App, Message, SettingsPane};
use crate::editor::EditorFrameStyle;
use crate::permissions::PermissionStatus;
use crate::welcome::WelcomeState;

// Type scale. One ramp so font sizes read as roles instead of scattered
// magic numbers (the view used twelve distinct literals). 12 is the
// floor for readable text (the shared kit's Xs buttons use TYPE_MICRO).
const TEXT_CAPTION: f32 = TYPE_CAPTION; // captions, hints, tooltips, HUD readouts
const TEXT_BODY_SM: f32 = TYPE_BODY; // default labels, most buttons, list text
const TEXT_BODY: f32 = TYPE_BODY; // emphasised body, primary CTAs, HUD title

/// Transparent click-through overlay that draws the captured rect's
/// border on screen during a scrolling-capture session.
///
/// Static outline matching the region-selection overlay
/// ([`crate::overlay::OverlayProgram`]): a 3 px `rgba(0,0,0,.45)` halo
/// under a 1 px white stroke, no fill, plus a `{w} × {h}` size badge
/// inside the rect's top-left corner. Nothing animates, so no redraw
/// tick is needed.
///
/// Mouse / scroll events pass through to the page underneath thanks
/// to `iced::window::enable_mouse_passthrough` (wired in the
/// `ScrollRegionWindowReady` handler).
pub(crate) fn scroll_region_view(state: &App) -> Element<'_, Message> {
    use iced::widget::canvas::{
        Canvas, Frame, Geometry, Path, Program, Stroke, Text as CanvasText,
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

    #[derive(Clone, Copy)]
    struct RegionProgram {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        scale: f32,
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
            // Same edge treatment as the capture overlay: 3 px black
            // halo for contrast on light wallpapers, then 1 px white.
            frame.stroke(
                &path,
                Stroke::default()
                    .with_color(Color::from_rgba(0.0, 0.0, 0.0, 0.45))
                    .with_width(3.0),
            );
            frame.stroke(
                &path,
                Stroke::default().with_color(Color::WHITE).with_width(1.0),
            );
            // Size badge in physical pixels, inside the top-left corner.
            let phys_w = ((self.w * self.scale).round() as i32).max(0);
            let phys_h = ((self.h * self.scale).round() as i32).max(0);
            let label = format!("{phys_w} × {phys_h}");
            let badge_w = 16.0 + label.chars().count() as f32 * 7.0;
            let badge_h = 22.0;
            let inset = 6.0;
            let bx = (self.x + inset).clamp(0.0, (bounds.width - badge_w).max(0.0));
            let by = (self.y + inset).clamp(0.0, (bounds.height - badge_h).max(0.0));
            let badge_path = Path::rounded_rectangle(
                iced::Point::new(bx, by),
                iced::Size::new(badge_w, badge_h),
                6.0.into(),
            );
            frame.fill(&badge_path, Color::from_rgba8(28, 28, 30, 0.90));
            frame.fill_text(CanvasText {
                content: label,
                position: iced::Point::new(bx + 8.0, by + 4.0),
                color: Color::from_rgb8(0xED, 0xED, 0xED),
                size: iced::Pixels(11.5),
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
    })
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

/// Recording-dot heartbeat alpha for the HUD "● Recording" chip — a
/// gentle 1.4 s sine pulse so the HUD doesn't feel statically dead
/// between frames. Held at constant full alpha under OS reduce-motion,
/// where the chip must read as a static cue.
pub(crate) fn hud_recording_pulse(elapsed_secs: f32, reduce_motion: bool) -> f32 {
    if reduce_motion {
        return 1.0;
    }
    let t = elapsed_secs * std::f32::consts::TAU / 1.4;
    // Floor well above transparent so the chip never dims into
    // unreadable territory — it breathes within 0.9..=1.0.
    0.9 + 0.1 * (0.5 + 0.5 * t.sin())
}

/// What the scroll window shows right now. One window, every state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ScrollHudState {
    /// No frame yet: "Scroll the page to start".
    Waiting,
    /// Frames arriving; the stitched preview grows from the bottom.
    Recording,
    /// Cancel pressed once with frames to lose: Discard / Keep Going.
    Armed,
    /// Stopped at `SCROLL_MAX_FRAMES` and stitching (amber).
    Limit,
    /// Stopped by the user and stitching.
    Stitching,
}

pub(crate) fn scroll_hud_state(s: &crate::app::ScrollSession) -> ScrollHudState {
    if s.stopping {
        if s.stitch_frame_count >= SCROLL_MAX_FRAMES {
            ScrollHudState::Limit
        } else {
            ScrollHudState::Stitching
        }
    } else if cancel_armed(s) {
        ScrollHudState::Armed
    } else if s.preview_handle.is_none() {
        ScrollHudState::Waiting
    } else {
        ScrollHudState::Recording
    }
}

/// Window width below which the scroll window drops the header's
/// elapsed time, puts the output size on its own footer line, and uses
/// short button labels. 12a can shrink the window to 240 wide.
const SCROLL_HUD_COMPACT_BELOW: f32 = 300.0;

pub(crate) fn scroll_hud_compact(window_w: f32) -> bool {
    window_w < SCROLL_HUD_COMPACT_BELOW
}

/// Window width below which the armed Discard button drops its frame
/// count: "Discard 120 Frames" overflows half the footer under ~340 px,
/// and the caption above already names the count.
const SCROLL_DISCARD_SHORT_BELOW: f32 = 340.0;

pub(crate) fn scroll_discard_label(window_w: f32, frame_count: usize) -> String {
    if window_w < SCROLL_DISCARD_SHORT_BELOW {
        "Discard".to_string()
    } else {
        let plural = if frame_count == 1 { "" } else { "s" };
        format!("Discard {frame_count} Frame{plural}")
    }
}

/// Key chip on the Stop & Stitch button. Hidden while Waiting (the
/// button is disabled and esc cancels there) and in the compact
/// layout, where the label alone fits.
pub(crate) fn scroll_stop_key_chip(hud: ScrollHudState, compact: bool) -> Option<&'static str> {
    (hud == ScrollHudState::Recording && !compact).then_some("esc")
}

/// Distances (stage px, measured up from the bottom of the strip) of the
/// joins between recent preview slices. `joins` is oldest-first in strip
/// px; the newest slice sits at the bottom, so the first tick is its
/// height, the next adds the slice above it, and so on.
pub(crate) fn join_tick_offsets(joins: &std::collections::VecDeque<u32>, stage_w: f32) -> Vec<f32> {
    let k = stage_w / PREVIEW_STRIP_W as f32;
    let mut acc = 0.0;
    joins
        .iter()
        .rev()
        .map(|h| {
            acc += *h as f32 * k;
            acc
        })
        .collect()
}

/// Stage background behind the preview (spec mockup `#0C0C0D`).
const SCROLL_STAGE_BG: Color = Color::from_rgb(12.0 / 255.0, 12.0 / 255.0, 13.0 / 255.0);

/// Canvas layer over the live strip: the teal "on screen now" outline
/// around the newest frame, and small ticks at the left edge where
/// frames were joined. Coordinates are bottom-relative.
#[derive(Debug)]
struct StageMarks {
    /// Height of the newest frame in stage px (0 = no outline).
    outline_h: f32,
    /// Join distances up from the bottom, in stage px.
    ticks: Vec<f32>,
}

impl<Message> iced::widget::canvas::Program<Message> for StageMarks {
    type State = ();

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &iced::Renderer,
        _theme: &Theme,
        bounds: iced::Rectangle,
        _cursor: iced::mouse::Cursor,
    ) -> Vec<iced::widget::canvas::Geometry> {
        use iced::widget::canvas::{Frame, Path, Stroke};
        use iced::{Point, Size};
        let mut frame = Frame::new(renderer, bounds.size());
        let (w, h) = (bounds.width, bounds.height);
        // Ticks: a dark and a light hairline so they read on any page.
        for &dy in &self.ticks {
            let y = (h - dy).round();
            if y < 0.0 {
                break;
            }
            for (off, color) in [
                (0.0, Color::from_rgba(0.0, 0.0, 0.0, 0.55)),
                (1.0, Color::from_rgba(1.0, 1.0, 1.0, 0.6)),
            ] {
                frame.stroke(
                    &Path::line(
                        Point::new(0.0, y + off - 0.5),
                        Point::new(6.0, y + off - 0.5),
                    ),
                    Stroke::default().with_color(color).with_width(1.0),
                );
            }
        }
        if self.outline_h > 0.0 {
            let oh = self.outline_h.min(h);
            let inset = 1.0;
            frame.stroke(
                &Path::rounded_rectangle(
                    Point::new(inset, h - oh + inset),
                    Size::new((w - 2.0 * inset).max(0.0), (oh - 2.0 * inset).max(0.0)),
                    8.0.into(),
                ),
                Stroke::default().with_color(DARK.acc).with_width(1.5),
            );
        }
        vec![frame.into_geometry()]
    }
}

/// The live stitched strip at full stage width, bottom-anchored so the
/// newest rows stay in view, with the outline/tick layer on top.
fn scroll_live_stage<'a>(
    handle: iced::widget::image::Handle,
    strip_h: u32,
    last_frame_rows_scaled: u32,
    joins: std::collections::VecDeque<u32>,
) -> Element<'a, Message> {
    use iced::widget::scrollable::{Direction, Scrollbar};
    responsive(move |size| {
        let stage_w = size.width.max(1.0);
        let k = stage_w / PREVIEW_STRIP_W as f32;
        let img_h = (strip_h as f32 * k).max(1.0);
        // Short strips sit at the bottom of the stage, like the mockup.
        let content_h = img_h.max(size.height);
        let image_layer = container(
            iced::widget::image(handle.clone())
                .width(Length::Fixed(stage_w))
                .height(Length::Fixed(img_h))
                .content_fit(iced::ContentFit::Fill),
        )
        .width(Length::Fixed(stage_w))
        .height(Length::Fixed(content_h))
        .align_y(iced::alignment::Vertical::Bottom);
        let marks = iced::widget::Canvas::new(StageMarks {
            outline_h: last_frame_rows_scaled as f32 * k,
            ticks: join_tick_offsets(&joins, stage_w),
        })
        .width(Length::Fixed(stage_w))
        .height(Length::Fixed(content_h));
        scrollable(iced::widget::stack![image_layer, marks])
            .anchor_bottom()
            .direction(Direction::Vertical(
                Scrollbar::new().width(0).scroller_width(0),
            ))
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    })
    .into()
}

/// Stitching / limit: the whole page zoomed to fit, dimmed, with a
/// static "Joining N frames" pill (no spinner animation, so nothing to
/// freeze under Reduce Motion).
fn scroll_stitching_stage<'a>(
    handle: Option<iced::widget::image::Handle>,
    frame_count: usize,
) -> Element<'a, Message> {
    let ring = container(
        Space::new()
            .width(Length::Fixed(14.0))
            .height(Length::Fixed(14.0)),
    )
    .style(|_| container::Style {
        border: iced::Border {
            color: DARK.acc,
            width: 2.0,
            radius: 7.0.into(),
        },
        ..Default::default()
    });
    let pill = container(
        row![
            ring,
            text(format!("Joining {frame_count} frames"))
                .size(TYPE_BODY)
                .color(DARK.t1)
                .wrapping(iced::widget::text::Wrapping::None),
        ]
        .spacing(10)
        .align_y(Alignment::Center),
    )
    .padding([9, 14])
    .style(|_| container::Style {
        background: Some(Color::from_rgba(28.0 / 255.0, 28.0 / 255.0, 30.0 / 255.0, 0.95).into()),
        border: iced::Border {
            color: DARK.border,
            width: 1.0,
            radius: 999.0.into(),
        },
        ..Default::default()
    });
    let veil = container(pill)
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .style(|_| container::Style {
            background: Some(
                Color {
                    a: 0.35,
                    ..SCROLL_STAGE_BG
                }
                .into(),
            ),
            ..Default::default()
        });
    match handle {
        Some(h) => iced::widget::stack![
            container(
                iced::widget::image(h)
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .content_fit(iced::ContentFit::Contain),
            )
            .padding(14)
            .width(Length::Fill)
            .height(Length::Fill),
            veil,
        ]
        .into(),
        None => veil.into(),
    }
}

/// A full-height footer button that takes `portion` of the row and never
/// wraps its label.
fn scroll_hud_button<'a>(
    label: String,
    kind: ButtonKind,
    msg: Option<Message>,
    chip: Option<&'static str>,
    portion: u16,
) -> Element<'a, Message> {
    let mut content = row![text(label)
        .size(ButtonSize::Lg.text_size())
        .font(if kind == ButtonKind::Primary {
            semibold()
        } else {
            iced::Font::DEFAULT
        })
        .wrapping(iced::widget::text::Wrapping::None)]
    .spacing(SP2)
    .align_y(Alignment::Center);
    if let Some(k) = chip {
        content = content.push(key_chip_on_primary(k));
    }
    bar_button(
        container(content).center_x(Length::Fill).into(),
        kind,
        ButtonSize::Lg,
        msg,
    )
    .width(Length::FillPortion(portion))
    .into()
}

/// The scrolling-capture window (dark only; colors from `DARK`). The
/// image is the largest thing in every state: the stitched page grows
/// from the bottom while recording, and is zoomed to fit while
/// stitching. Lays out at any size 12a can pick, 240×320 to 360×540 —
/// nothing here is sized from `SCROLL_WINDOW_W/H`.
pub(crate) fn scroll_hud_view(state: &App) -> Element<'_, Message> {
    responsive(move |size| scroll_hud_body(state, size.width)).into()
}

fn scroll_hud_body(state: &App, window_w: f32) -> Element<'_, Message> {
    use iced::widget::mouse_area;
    use iced::widget::tooltip::Position;
    let Some(session) = state.scroll_session.as_ref() else {
        return container(Space::new())
            .width(Length::Fill)
            .height(Length::Fill)
            .into();
    };
    let compact = scroll_hud_compact(window_w);
    let hud = scroll_hud_state(session);
    let stopping = session.stopping;
    // `frames` is drained into the stitch task at stop time; the stitch
    // count keeps the window's number honest while stitching.
    let frame_count = if stopping {
        session.stitch_frame_count
    } else {
        session.frames.len()
    };
    let elapsed_secs = session.started_at.elapsed().as_secs_f32();

    // ---- Header: dot, title, elapsed, close. Doubles as drag handle.
    let dot_base = match hud {
        ScrollHudState::Recording | ScrollHudState::Armed => DARK.acc,
        ScrollHudState::Waiting | ScrollHudState::Stitching => DARK.t3,
        ScrollHudState::Limit => DARK.warn,
    };
    let dot_color = match hud {
        ScrollHudState::Recording | ScrollHudState::Armed => Color {
            a: hud_recording_pulse(elapsed_secs, state.reduce_motion),
            ..dot_base
        },
        _ => dot_base,
    };
    let dot = container(
        container(
            Space::new()
                .width(Length::Fixed(8.0))
                .height(Length::Fixed(8.0)),
        )
        .style(move |_| container::Style {
            background: Some(dot_color.into()),
            border: iced::Border {
                radius: 4.0.into(),
                ..Default::default()
            },
            ..Default::default()
        }),
    )
    .center_x(Length::Fixed(16.0))
    .center_y(Length::Fixed(16.0))
    .style(move |_| container::Style {
        background: Some(
            Color {
                a: 0.15,
                ..dot_base
            }
            .into(),
        ),
        border: iced::Border {
            radius: 8.0.into(),
            ..Default::default()
        },
        ..Default::default()
    });
    let title = if stopping {
        "Stitching…"
    } else {
        "Scrolling Capture"
    };
    let mut header_row = row![
        dot,
        text(title)
            .size(TYPE_BODY)
            .font(semibold())
            .color(DARK.t1)
            .wrapping(iced::widget::text::Wrapping::None),
    ]
    .spacing(10)
    .align_y(Alignment::Center);
    let show_elapsed = !compact
        && matches!(
            hud,
            ScrollHudState::Recording | ScrollHudState::Armed | ScrollHudState::Limit
        );
    if show_elapsed {
        let secs = elapsed_secs as u32;
        header_row = header_row.push(
            text(format!("{:02}:{:02}", secs / 60, secs % 60))
                .size(TYPE_CAPTION)
                .font(mono_font())
                .color(DARK.t3),
        );
    }
    header_row = header_row
        .push(Space::new().width(Length::Fill))
        .push(icon_button(
            crate::icons::AppIcon::Close,
            DARK.t2,
            "Cancel",
            Some("⇧esc"),
            (!stopping).then_some(Message::ScrollCaptureCancelRequested),
            false,
            ICON_BTN,
            Position::Bottom,
        ));
    let header = mouse_area(
        container(header_row)
            .padding(iced::Padding {
                top: 0.0,
                right: 10.0,
                bottom: 0.0,
                left: 16.0,
            })
            .width(Length::Fill)
            .height(Length::Fixed(48.0))
            .center_y(Length::Fixed(48.0)),
    )
    .on_press(Message::ScrollHudDragRequested)
    .interaction(iced::mouse::Interaction::Grab);

    // ---- Stage: the image.
    let stage_content: Element<'_, Message> = match (hud, session.preview_handle.clone()) {
        (ScrollHudState::Stitching | ScrollHudState::Limit, handle) => {
            scroll_stitching_stage(handle, frame_count)
        }
        (ScrollHudState::Recording | ScrollHudState::Armed, Some(handle)) => scroll_live_stage(
            handle,
            session.preview_strip.as_ref().map_or(0, |s| s.height()),
            session.last_frame_rows_scaled,
            session.recent_join_heights.clone(),
        ),
        _ => container(empty_state(
            crate::icons::AppIcon::Scroll,
            &DARK,
            "Scroll the page to start",
            "Each new part of the page appears here, joined to the last.",
            None,
        ))
        .padding(16)
        .width(Length::Fill)
        .height(Length::Fill)
        .into(),
    };
    let stage = container(
        container(stage_content)
            .width(Length::Fill)
            .height(Length::Fill)
            .clip(true)
            .style(|_| container::Style {
                background: Some(SCROLL_STAGE_BG.into()),
                border: iced::Border {
                    color: DARK.border_subtle,
                    width: 1.0,
                    radius: 10.0.into(),
                },
                ..Default::default()
            }),
    )
    .padding([0, 10])
    .width(Length::Fill)
    .height(Length::Fill);

    // ---- Footer: count, output size, limit bar, buttons.
    let limit = hud == ScrollHudState::Limit;
    let count = text(frame_count.to_string())
        .size(TYPE_DISPLAY)
        .font(iced::Font {
            weight: iced::font::Weight::Semibold,
            ..mono_font()
        })
        .color(if frame_count == 0 { DARK.t3 } else { DARK.t1 });
    let label = text(if limit {
        "frames · limit reached"
    } else {
        "frames"
    })
    .size(TYPE_BODY)
    .color(DARK.t3)
    .wrapping(iced::widget::text::Wrapping::None);
    let count_row = row![count, label].spacing(SP2).align_y(Alignment::End);
    let dims = (session.stitched_rows > 0).then(|| {
        text(format!(
            "{} × {} px",
            (session.rect.width() * session.scale).round() as u32,
            session.stitched_rows
        ))
        .size(TYPE_CAPTION)
        .font(mono_font())
        .color(DARK.t3)
        .wrapping(iced::widget::text::Wrapping::None)
    });
    let stats: Element<'_, Message> = match dims {
        Some(d) if compact => column![count_row, d].spacing(2).into(),
        Some(d) => row![count_row, Space::new().width(Length::Fill), d]
            .align_y(Alignment::End)
            .into(),
        None => count_row.into(),
    };

    let progress = (frame_count as f32 / SCROLL_MAX_FRAMES as f32).clamp(0.0, 1.0);
    let filled = (progress * 1000.0).round() as u16;
    let bar_color = if limit { DARK.warn } else { DARK.acc };
    let mut bar_row = row![].width(Length::Fill).height(Length::Fixed(3.0));
    if filled > 0 {
        bar_row = bar_row.push(
            container(Space::new().height(Length::Fixed(3.0)))
                .width(Length::FillPortion(filled))
                .style(move |_| container::Style {
                    background: Some(bar_color.into()),
                    border: iced::Border {
                        radius: 2.0.into(),
                        ..Default::default()
                    },
                    ..Default::default()
                }),
        );
    }
    if filled < 1000 {
        bar_row = bar_row.push(Space::new().width(Length::FillPortion(1000 - filled)));
    }
    let bar = container(bar_row)
        .width(Length::Fill)
        .height(Length::Fixed(3.0))
        .style(|_| container::Style {
            background: Some(Color::from_rgba(1.0, 1.0, 1.0, 0.08).into()),
            border: iced::Border {
                radius: 2.0.into(),
                ..Default::default()
            },
            ..Default::default()
        });

    let plural = if frame_count == 1 { "" } else { "s" };
    let mut footer = column![
        stats,
        Space::new().height(Length::Fixed(10.0)),
        bar,
        Space::new().height(Length::Fixed(12.0)),
    ]
    .width(Length::Fill);
    let buttons: Element<'_, Message> = match hud {
        ScrollHudState::Armed => {
            footer = footer.push(
                text(format!(
                    "Press again within {:.1} s to discard {frame_count} frame{plural}.",
                    CANCEL_ARM_WINDOW.as_secs_f32()
                ))
                .size(TYPE_CAPTION)
                .color(DARK.danger)
                .wrapping(iced::widget::text::Wrapping::Word),
            );
            footer = footer.push(Space::new().height(Length::Fixed(10.0)));
            row![
                scroll_hud_button(
                    scroll_discard_label(window_w, frame_count),
                    ButtonKind::DangerSolid,
                    Some(Message::ScrollCaptureCancelRequested),
                    None,
                    1,
                ),
                scroll_hud_button(
                    "Keep Going".to_string(),
                    ButtonKind::Primary,
                    Some(Message::ScrollCaptureKeepGoing),
                    None,
                    1,
                ),
            ]
            .spacing(SP2)
            .into()
        }
        ScrollHudState::Stitching | ScrollHudState::Limit => row![
            scroll_hud_button("Cancel".to_string(), ButtonKind::Secondary, None, None, 2),
            scroll_hud_button("Stitching…".to_string(), ButtonKind::Primary, None, None, 3),
        ]
        .spacing(SP2)
        .into(),
        ScrollHudState::Waiting | ScrollHudState::Recording => {
            let waiting = hud == ScrollHudState::Waiting;
            row![
                tooltip_with_key(
                    scroll_hud_button(
                        "Cancel".to_string(),
                        ButtonKind::Secondary,
                        Some(Message::ScrollCaptureCancelRequested),
                        None,
                        2,
                    ),
                    "Cancel and discard",
                    Some("⇧esc"),
                    Position::Top,
                ),
                scroll_hud_button(
                    "Stop & Stitch".to_string(),
                    ButtonKind::Primary,
                    (!waiting).then_some(Message::ScrollCaptureStopRequested),
                    scroll_stop_key_chip(hud, compact),
                    3,
                ),
            ]
            .spacing(SP2)
            .into()
        }
    };
    let footer = container(footer.push(buttons))
        .padding(iced::Padding {
            top: 12.0,
            right: 14.0,
            bottom: 14.0,
            left: 14.0,
        })
        .width(Length::Fill);

    container(
        column![header, stage, footer]
            .width(Length::Fill)
            .height(Length::Fill),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .style(|_| container::Style {
        background: Some(DARK.float.into()),
        border: iced::Border {
            color: DARK.float_border,
            width: 1.0,
            radius: R_WINDOW.into(),
        },
        ..Default::default()
    })
    .into()
}

/// A command block: monospace text on the `raised` surface, radius 8,
/// with a hairline border so it reads on white cards in light mode too.
fn cli_code_panel<'a>(commands: impl Into<String>) -> Element<'a, Message> {
    container(
        text(commands.into())
            .size(TYPE_CAPTION)
            .font(mono_font())
            .style(text_t1),
    )
    .padding([10, 12])
    .width(Length::Fill)
    .style(|theme: &Theme| {
        let p = palette_for(theme);
        container::Style {
            background: Some(p.raised.into()),
            border: iced::Border {
                color: p.border_subtle,
                width: 1.0,
                radius: R_BUTTON.into(),
            },
            ..Default::default()
        }
    })
    .into()
}

/// One CLI Tools card: a heading (with an optional copy button on the
/// right) above its command block.
fn cli_card<'a>(
    heading: String,
    copy: Option<(String, Message)>,
    commands: impl Into<String>,
) -> Element<'a, Message> {
    let mut head = row![text(heading)
        .size(TYPE_BODY)
        .font(semibold())
        .style(text_t1)
        .width(Length::Fill)]
    .spacing(SP3)
    .align_y(Alignment::Center);
    if let Some((label, msg)) = copy {
        head = head.push(bar_button(
            text(label).size(ButtonSize::Sm.text_size()).into(),
            ButtonKind::Secondary,
            ButtonSize::Sm,
            Some(msg),
        ));
    }
    container(column![head, cli_code_panel(commands)].spacing(SP2))
        .padding([12, 14])
        .width(Length::Fill)
        .style(group_style)
        .into()
}

pub(crate) fn cli_tools_view(state: &App) -> Element<'_, Message> {
    use iced::widget::{mouse_area, stack};

    let status = state.cli_tools_status.as_deref().unwrap_or("");

    // --- Title row (transparent full-size title bar on macOS) ---
    let title_content = container(
        text("Command Line Tools")
            .size(TYPE_BODY)
            .font(semibold())
            .style(text_t1),
    )
    .padding(iced::Padding {
        left: TITLEBAR_INSET_LEFT,
        right: 12.0,
        ..iced::Padding::ZERO
    })
    .width(Length::Fill)
    .height(Length::Fixed(TITLEBAR_HEIGHT))
    .align_y(Alignment::Center);
    // The row's empty background is the window's drag handle.
    let mut drag_handle = mouse_area(
        Space::new()
            .width(Length::Fill)
            .height(Length::Fixed(TITLEBAR_HEIGHT)),
    );
    if let Some(id) = state.cli_tools_window_id {
        drag_handle = drag_handle.on_press(Message::WindowDragRequested(id));
    }
    let title_row = stack![drag_handle, title_content]
        .width(Length::Fill)
        .height(Length::Fixed(TITLEBAR_HEIGHT));

    // --- Body: same order as before, restyled as cards ---
    let mut cards =
        column![
            text("Choose your shell and copy only that command block into Terminal.")
                .size(TYPE_BODY)
                .style(text_t2),
        ]
        .spacing(SP3);
    if !status.is_empty() {
        cards = cards.push(text(status).size(TYPE_CAPTION).style(text_t2));
    }
    cards = cards.push(cli_card(
        "Run for every shell".to_string(),
        None,
        crate::cli_tools::common_commands(),
    ));
    for shell in crate::cli_tools::Shell::ALL {
        cards = cards.push(cli_card(
            format!("For {}", shell.label()),
            Some((
                format!("Copy {} Commands", shell.label()),
                Message::CliToolsCopyRequested(shell),
            )),
            crate::cli_tools::shell_commands(shell),
        ));
    }
    cards = cards.push(cli_card(
        "Verify".to_string(),
        None,
        crate::cli_tools::verify_commands(),
    ));
    let body = scrollable(container(cards).width(Length::Fill).padding(iced::Padding {
        top: 2.0,
        right: 22.0,
        bottom: 22.0,
        left: 22.0,
    }))
    .direction(iced::widget::scrollable::Direction::Vertical(
        slim_scrollbar(),
    ))
    .height(Length::Fill);

    container(column![title_row, body])
        .width(Length::Fill)
        .height(Length::Fill)
        .style(bg_style)
        .into()
}

/// History layout (spec §3.3): list column width at full size, and the
/// 16:10 row thumbnail.
const HISTORY_LIST_WIDTH: f32 = 340.0;
const HISTORY_THUMB_W: f32 = 64.0;
const HISTORY_THUMB_H: f32 = 40.0;
/// Below this detail-pane width the action row splits into two lines.
const HISTORY_ACTIONS_STACK_BELOW: f32 = 400.0;

/// Reveal action label per platform.
#[cfg(target_os = "macos")]
const HISTORY_REVEAL_LABEL: &str = "Reveal in Finder";
#[cfg(not(target_os = "macos"))]
const HISTORY_REVEAL_LABEL: &str = "Show in Folder";

// Key labels for History's own bindings (see the History keyboard
// subscription in `history_key_message`, runtime.rs). Delete is ⌘⌫ (Move
// to Trash; ⌦ also works, plain ⌫ edits the search field); ⌘C copies the
// image and ⇧⌘C the text, as in the overlay and editor.
const HISTORY_KEY_DELETE: &str = "⌘⌫";
const HISTORY_KEY_COPY_TEXT: &str = "⇧⌘C";
const HISTORY_KEY_COPY_IMAGE: &str = "⌘C";

/// Widget id of the History search field, focused when the window opens.
pub(crate) const HISTORY_SEARCH_ID: &str = "history-search";

/// Human timestamp for History: "Today 14:03", "Yesterday 09:10",
/// "Sep 25, 14:03" this year, "Sep 25, 2025" for older captures. The
/// search still matches the full ISO-style timestamp.
pub(crate) fn history_timestamp_label(
    at: chrono::DateTime<chrono::Local>,
    now: chrono::DateTime<chrono::Local>,
) -> String {
    use chrono::Datelike;
    let day = at.date_naive();
    let today = now.date_naive();
    if day == today {
        format!("Today {}", at.format("%H:%M"))
    } else if today.pred_opt() == Some(day) {
        format!("Yesterday {}", at.format("%H:%M"))
    } else if day.year() == today.year() {
        at.format("%b %-d, %H:%M").to_string()
    } else {
        at.format("%b %-d, %Y").to_string()
    }
}

/// Compact age for list rows: "now", "2m", "3h", "Yesterday", "Sep 22",
/// "Dec 31, 2025". The detail pane shows the fuller
/// [`history_timestamp_label`].
pub(crate) fn history_age_label(
    at: chrono::DateTime<chrono::Local>,
    now: chrono::DateTime<chrono::Local>,
) -> String {
    use chrono::Datelike;
    let day = at.date_naive();
    let today = now.date_naive();
    if day == today {
        let mins = (now - at).num_minutes().max(0);
        if mins < 1 {
            "now".to_string()
        } else if mins < 60 {
            format!("{mins}m")
        } else {
            format!("{}h", mins / 60)
        }
    } else if today.pred_opt() == Some(day) {
        "Yesterday".to_string()
    } else if day.year() == today.year() {
        at.format("%b %-d").to_string()
    } else {
        at.format("%b %-d, %Y").to_string()
    }
}

/// First non-blank OCR line, trimmed — the row title.
pub(crate) fn history_first_line(text: Option<&str>) -> Option<&str> {
    text?.lines().map(str::trim).find(|l| !l.is_empty())
}

pub(crate) fn history_word_count(text: Option<&str>) -> usize {
    text.map_or(0, |t| t.split_whitespace().count())
}

/// Split `text` into `(segment, is_match)` runs for a case-insensitive
/// `query`. An empty query yields the whole text unmatched.
pub(crate) fn highlight_spans(text: &str, query: &str) -> Vec<(String, bool)> {
    // Lowercasing can change a character's byte length (Kelvin sign,
    // dotted İ), which would make the lowercase byte offsets invalid for
    // `text`. Such text is shown without highlights rather than panic.
    if text
        .chars()
        .any(|c| c.to_lowercase().map(char::len_utf8).sum::<usize>() != c.len_utf8())
    {
        return vec![(text.to_string(), false)];
    }
    let q = query.trim();
    if q.is_empty() {
        return vec![(text.to_string(), false)];
    }
    let lower = text.to_lowercase();
    let ql = q.to_lowercase();
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(pos) = lower[i..].find(&ql) {
        let start = i + pos;
        if start > i {
            out.push((text[i..start].to_string(), false));
        }
        out.push((text[start..start + ql.len()].to_string(), true));
        i = start + ql.len();
    }
    if i < text.len() {
        out.push((text[i..].to_string(), false));
    }
    out
}

/// Text with search matches on an `acc_tint` background.
fn highlighted_text<'a>(
    content: &str,
    query: &str,
    p: &'static Palette,
    base: Color,
    size: f32,
) -> iced::widget::text::Rich<'a, (), Message> {
    let spans: Vec<iced::widget::text::Span<'a, (), iced::Font>> = highlight_spans(content, query)
        .into_iter()
        .map(|(segment, hit)| {
            let s = iced::widget::span(segment);
            if hit {
                s.color(p.t1).background(p.acc_tint)
            } else {
                s.color(base)
            }
        })
        .collect();
    iced::widget::rich_text(spans).size(size)
}

fn semibold() -> iced::Font {
    iced::Font {
        weight: iced::font::Weight::Semibold,
        ..iced::Font::DEFAULT
    }
}

fn hairline() -> Element<'static, Message> {
    container(Space::new().width(Length::Fill).height(Length::Fixed(1.0)))
        .width(Length::Fill)
        .height(Length::Fixed(1.0))
        .style(|theme| container::Style {
            background: Some(palette_for(theme).border_subtle.into()),
            ..container::Style::default()
        })
        .into()
}

fn vhairline() -> Element<'static, Message> {
    container(Space::new().width(Length::Fixed(1.0)).height(Length::Fill))
        .width(Length::Fixed(1.0))
        .height(Length::Fill)
        .style(|theme| container::Style {
            background: Some(palette_for(theme).border_subtle.into()),
            ..container::Style::default()
        })
        .into()
}

pub(crate) fn history_view(state: &App) -> Element<'_, Message> {
    use iced::widget::tooltip::Position;
    use iced::widget::{mouse_area, stack, text_input};

    let p = palette(state.theme_mode);
    let total = state.history_records.len();
    // Filter once per frame: case-insensitive match on OCR text and
    // the timestamp (shared with the MCP server's search).
    let q = state.history_search.trim().to_lowercase();
    let visible: Vec<&readshot_core::CaptureRecord> = state
        .history_records
        .iter()
        .filter(|r| q.is_empty() || record_matches(r, &q))
        .collect();
    let shown = visible.len();
    let visible_text_count = visible
        .iter()
        .filter(|r| r.ocr_text.as_deref().is_some_and(|t| !t.trim().is_empty()))
        .count();

    // --- Title row: heading · search · Copy Visible Text · Clear All ---
    let count_caption = if total == 0 {
        String::new()
    } else {
        format!("{shown} of {total}")
    };
    // Room on the right of the input for the overlaid count caption.
    let caption_room = count_caption.chars().count() as f32 * 7.0 + 14.0;
    let search_input = text_input("Search text", &state.history_search)
        .id(iced::widget::Id::new(HISTORY_SEARCH_ID))
        .on_input(Message::HistorySearchChanged)
        .width(Length::Fill)
        .padding(iced::Padding {
            top: 6.0,
            bottom: 6.0,
            left: 30.0,
            right: caption_room,
        })
        .size(TYPE_BODY)
        .style(text_input_style);
    // Icon and count draw over the input; neither takes the cursor, so
    // clicks still reach the field.
    let search_decor = row![
        crate::icons::icon(crate::icons::AppIcon::Search, p.t3, 14.0),
        Space::new().width(Length::Fill),
        text(count_caption).size(TYPE_CAPTION).style(text_t3),
    ]
    .align_y(Alignment::Center)
    .padding([0, 10])
    .width(Length::Fill)
    .height(Length::Fill);
    let search = container(stack![search_input, search_decor])
        .width(Length::Fill)
        .max_width(340);

    let copy_visible: Element<'_, Message> = tooltip_with_key(
        bar_button(
            text("Copy Visible Text")
                .size(ButtonSize::Sm.text_size())
                .into(),
            ButtonKind::Ghost,
            ButtonSize::Sm,
            (visible_text_count > 0).then_some(Message::HistoryCopyVisibleTextRequested),
        ),
        format!(
            "Copy text from {visible_text_count} visible capture{}",
            if visible_text_count == 1 { "" } else { "s" }
        ),
        None,
        Position::Bottom,
    );
    // Two-stage Clear All: the idle button arms; armed shows a solid
    // danger confirm plus Cancel.
    let clear_controls: Element<'_, Message> = if state.history_clear_all_pending {
        row![
            bar_button(
                text(format!("Confirm Clear ({total})"))
                    .size(ButtonSize::Sm.text_size())
                    .into(),
                ButtonKind::DangerSolid,
                ButtonSize::Sm,
                Some(Message::HistoryClearAllConfirmed),
            ),
            bar_button(
                text("Cancel").size(ButtonSize::Sm.text_size()).into(),
                ButtonKind::Ghost,
                ButtonSize::Sm,
                Some(Message::HistoryClearAllCancelled),
            ),
        ]
        .spacing(SP1)
        .align_y(Alignment::Center)
        .into()
    } else {
        bar_button(
            text("Clear All").size(ButtonSize::Sm.text_size()).into(),
            ButtonKind::Ghost,
            ButtonSize::Sm,
            (total > 0).then_some(Message::HistoryClearAllRequested),
        )
        .into()
    };
    let title_content = row![
        text("History")
            .size(TYPE_BODY)
            .font(semibold())
            .style(text_t1),
        container(search).center_x(Length::Fill),
        copy_visible,
        clear_controls,
    ]
    .spacing(SP3)
    .align_y(Alignment::Center)
    .padding(iced::Padding {
        left: TITLEBAR_INSET_LEFT,
        right: 12.0,
        ..iced::Padding::ZERO
    })
    .width(Length::Fill)
    .height(Length::Fixed(TITLEBAR_HEIGHT));
    // The transparent title bar is covered by this row; its empty
    // background is the window's drag handle.
    let mut drag_handle = mouse_area(
        Space::new()
            .width(Length::Fill)
            .height(Length::Fixed(TITLEBAR_HEIGHT)),
    );
    if let Some(id) = state.history_window_id {
        drag_handle = drag_handle.on_press(Message::WindowDragRequested(id));
    }
    let title_row = stack![drag_handle, title_content]
        .width(Length::Fill)
        .height(Length::Fixed(TITLEBAR_HEIGHT));

    // --- Body ---
    let body: Element<'_, Message> = if state.history_root.is_none() {
        empty_state(
            crate::icons::AppIcon::Alert,
            p,
            "History unavailable",
            "Readshot could not resolve the history folder for this session.",
            None,
        )
    } else if shown == 0 {
        if total == 0 {
            empty_state(
                crate::icons::AppIcon::History,
                p,
                "No captures yet",
                "Take a capture and it appears here with its image, OCR text, and quick actions.",
                Some(
                    bar_button(
                        text("Capture").size(ButtonSize::Md.text_size()).into(),
                        ButtonKind::Primary,
                        ButtonSize::Md,
                        Some(Message::OpenOverlayRequested),
                    )
                    .into(),
                ),
            )
        } else {
            empty_state(
                crate::icons::AppIcon::Search,
                p,
                "No matching captures",
                "Try a different search or clear the filter to see the full history.",
                Some(
                    bar_button(
                        text("Clear Search").size(ButtonSize::Md.text_size()).into(),
                        ButtonKind::Secondary,
                        ButtonSize::Md,
                        Some(Message::HistorySearchChanged(String::new())),
                    )
                    .into(),
                ),
            )
        }
    } else {
        responsive(move |size| history_split(state, p, &visible, size.width)).into()
    };
    // The split view shows status above its key legend; empty states
    // show it under the message so e.g. "Cleared …" is still seen.
    let body: Element<'_, Message> = match history_status_toast(state, p) {
        Some(t) if shown == 0 || state.history_root.is_none() => {
            column![body, container(t).center_x(Length::Fill).padding(SP4)].into()
        }
        _ => body,
    };

    container(column![title_row, hairline(), body])
        .width(Length::Fill)
        .height(Length::Fill)
        .style(surface_style)
        .into()
}

/// List | detail split. The list keeps 340 px until the window is too
/// narrow for a usable detail pane, then shrinks.
fn history_status_toast(state: &App, p: &'static Palette) -> Option<Element<'static, Message>> {
    let status = state.history_status.as_deref()?;
    let kind = status_toast_kind(status);
    // Errors stay until dismissed (History has no auto-expiry).
    let action = (kind == ToastKind::Error).then(|| {
        icon_button(
            crate::icons::AppIcon::Close,
            p.t2,
            "Dismiss",
            None,
            Some(Message::HistoryStatusDismissed),
            false,
            ICON_BTN_XS,
            iced::widget::tooltip::Position::Top,
        )
    });
    Some(toast(kind, status.to_string(), action))
}

fn history_split<'a>(
    state: &'a App,
    p: &'static Palette,
    visible: &[&'a readshot_core::CaptureRecord],
    width: f32,
) -> Element<'a, Message> {
    use iced::widget::{image as image_widget, mouse_area};

    let list_w = if width >= HISTORY_LIST_WIDTH * 2.2 {
        HISTORY_LIST_WIDTH
    } else {
        (width * 0.45).clamp(200.0, HISTORY_LIST_WIDTH)
    };
    let detail_w = (width - list_w - 1.0).max(0.0);
    let query = state.history_search.trim();
    let Some(root) = state.history_root.as_ref() else {
        return Space::new().into();
    };

    // Render at most a page of rows. iced 0.14 has no virtual list, so
    // an Unlimited-retention history would otherwise build a thumbnail
    // widget for every record on open.
    let page_limit = state.history_page_limit.max(1);
    let now = chrono::Local::now();
    let mut list = column![].spacing(2).padding(SP2);
    for r in visible.iter().take(page_limit) {
        let png_path = history_png_path(root, r);
        let preview_path = history_thumbnail_path(root, r);
        let thumb: Element<'_, Message> = if preview_path.exists() || png_path.exists() {
            let path = if preview_path.exists() {
                preview_path
            } else {
                png_path
            };
            image_widget(image_widget::Handle::from_path(path))
                .width(Length::Fixed(HISTORY_THUMB_W))
                .height(Length::Fixed(HISTORY_THUMB_H))
                .content_fit(iced::ContentFit::Cover)
                .border_radius(R_CHIP)
                .into()
        } else {
            container(crate::icons::icon(crate::icons::AppIcon::Image, p.t3, 14.0))
                .width(Length::Fixed(HISTORY_THUMB_W))
                .height(Length::Fixed(HISTORY_THUMB_H))
                .center_x(Length::Fixed(HISTORY_THUMB_W))
                .center_y(Length::Fixed(HISTORY_THUMB_H))
                .style(|theme| container::Style {
                    background: Some(palette_for(theme).hover.into()),
                    border: iced::Border {
                        radius: R_CHIP.into(),
                        ..iced::Border::default()
                    },
                    ..container::Style::default()
                })
                .into()
        };
        let title: Element<'_, Message> = match history_first_line(r.ocr_text.as_deref()) {
            Some(line) => highlighted_text(line, query, p, p.t1, TYPE_BODY)
                .font(iced::Font {
                    weight: iced::font::Weight::Medium,
                    ..iced::Font::DEFAULT
                })
                .wrapping(text::Wrapping::None)
                .into(),
            None => text("No text yet")
                .size(TYPE_BODY)
                .style(text_t3)
                .wrapping(text::Wrapping::None)
                .into(),
        };
        let sub = text(format!("{} × {}", r.width_px, r.height_px))
            .font(mono_font())
            .size(TYPE_MICRO)
            .style(text_t3)
            .wrapping(text::Wrapping::None);
        let meta = container(column![title, sub].spacing(2))
            .width(Length::Fill)
            .clip(true);
        let age = text(history_age_label(
            r.captured_at.with_timezone(&chrono::Local),
            now,
        ))
        .size(TYPE_CAPTION)
        .style(text_t3)
        .wrapping(text::Wrapping::None);
        let is_selected = state.history_selected_id == Some(r.id);
        let row_widget = container(
            row![thumb, meta, age]
                .spacing(SP3)
                .align_y(Alignment::Center),
        )
        .padding(SP2)
        .width(Length::Fill)
        .style(move |theme| {
            let p = palette_for(theme);
            container::Style {
                background: is_selected.then(|| history_selected_fill(p).into()),
                border: iced::Border {
                    color: if is_selected {
                        history_selected_border(p)
                    } else {
                        Color::TRANSPARENT
                    },
                    width: if is_selected { 1.0 } else { 0.0 },
                    radius: 9.0.into(),
                },
                ..container::Style::default()
            }
        });
        list = list.push(
            mouse_area(row_widget)
                .on_press(Message::HistorySelect(r.id))
                .on_double_click(Message::HistoryOpenInEditor(r.id))
                .interaction(iced::mouse::Interaction::Pointer),
        );
    }
    // "Show More" pager whenever matches exceed the rendered page.
    let remaining = visible.len().saturating_sub(page_limit);
    if remaining > 0 {
        list = list.push(
            container(bar_button(
                text(format!("Show More ({remaining} more)"))
                    .size(ButtonSize::Sm.text_size())
                    .into(),
                ButtonKind::Ghost,
                ButtonSize::Sm,
                Some(Message::HistoryShowMore),
            ))
            .center_x(Length::Fill)
            .padding([SP1, 0.0]),
        );
    }

    let legend_item = |key: &'static str, label: &'static str| -> Element<'_, Message> {
        row![key_chip(key), text(label).size(TYPE_CAPTION).style(text_t3)]
            .spacing(5)
            .align_y(Alignment::Center)
            .into()
    };
    let footer = container(
        row![
            legend_item("↑↓", "Move"),
            legend_item("↵", "Open"),
            legend_item(HISTORY_KEY_DELETE, "Delete"),
            legend_item("⌘F", "Search"),
        ]
        .spacing(14)
        .wrap()
        .vertical_spacing(SP1),
    )
    .padding([10, 14])
    .width(Length::Fill);

    let mut list_col = column![scrollable(list)
        .direction(iced::widget::scrollable::Direction::Vertical(
            slim_scrollbar(),
        ))
        .height(Length::Fill)];
    if let Some(t) = history_status_toast(state, p) {
        list_col = list_col.push(container(t).width(Length::Fill).padding(SP2));
    }
    list_col = list_col.push(hairline()).push(footer);
    let list_pane = container(list_col)
        .width(Length::Fixed(list_w))
        .height(Length::Fill)
        .style(bg_style);

    let selected = state
        .history_selected_id
        .and_then(|id| visible.iter().copied().find(|record| record.id == id));
    let delete_pending = selected.is_some_and(|r| state.history_delete_pending == Some(r.id));
    let detail = history_selected_panel(
        selected,
        root,
        query,
        p,
        delete_pending,
        detail_w - 2.0 * SP4 < HISTORY_ACTIONS_STACK_BELOW,
    );

    row![list_pane, vhairline(), detail]
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

/// Detail pane: preview, recognized text, one action row.
pub(crate) fn history_selected_panel<'a>(
    record: Option<&'a readshot_core::CaptureRecord>,
    root: &Path,
    query: &str,
    p: &'static Palette,
    delete_pending: bool,
    stack_actions: bool,
) -> Element<'a, Message> {
    use crate::icons::AppIcon;
    use iced::widget::image as image_widget;
    use iced::widget::tooltip::Position;

    let Some(record) = record else {
        return empty_state(
            AppIcon::History,
            p,
            "No capture selected",
            "Select a capture to see its image and text.",
            None,
        );
    };
    let id = record.id;

    let png_path = history_png_path(root, record);
    let thumb_path = history_thumbnail_path(root, record);
    let preview: Element<'a, Message> = if png_path.exists() || thumb_path.exists() {
        let path = if png_path.exists() {
            png_path
        } else {
            thumb_path
        };
        container(
            image_widget(image_widget::Handle::from_path(path))
                .width(Length::Fill)
                .height(Length::Fill)
                .content_fit(iced::ContentFit::Contain),
        )
        .padding(SP2)
        .into()
    } else {
        empty_state(
            AppIcon::Image,
            p,
            "Image file missing",
            "The image for this capture was moved or deleted. Its text is still here.",
            None,
        )
    };
    let preview = container(preview)
        .width(Length::Fill)
        .height(Length::FillPortion(5))
        .clip(true)
        .style(history_selected_panel_style);

    let stamp = history_timestamp_label(
        record.captured_at.with_timezone(&chrono::Local),
        chrono::Local::now(),
    );
    let ocr = record
        .ocr_text
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty());
    let words = history_word_count(ocr);
    let heading = row![
        text("Recognized Text")
            .size(TYPE_BODY)
            .font(semibold())
            .style(text_t1),
        Space::new().width(Length::Fill),
        text(format!(
            "{stamp} · {words} word{}",
            if words == 1 { "" } else { "s" }
        ))
        .size(TYPE_CAPTION)
        .style(text_t3)
        .wrapping(text::Wrapping::None),
    ]
    .spacing(SP2)
    .align_y(Alignment::Center);
    let text_body: Element<'a, Message> = match ocr {
        Some(t) => scrollable(
            container(
                highlighted_text(t, query, p, p.t2, TYPE_CAPTION)
                    .wrapping(text::Wrapping::WordOrGlyph)
                    .width(Length::Fill),
            )
            .padding([10, 12]),
        )
        .direction(iced::widget::scrollable::Direction::Vertical(
            slim_scrollbar(),
        ))
        .height(Length::Fill)
        .into(),
        None => container(
            text("No text recognized.")
                .size(TYPE_CAPTION)
                .style(text_t3),
        )
        .padding([10, 12])
        .into(),
    };
    let text_box = container(text_body)
        .width(Length::Fill)
        .height(Length::FillPortion(4))
        .style(history_selected_panel_style);

    // Two-stage delete, like Clear All: the icon arms; armed shows a
    // solid "Delete Capture" plus Cancel.
    let delete_control: Element<'a, Message> = if delete_pending {
        row![
            bar_button(
                text("Delete Capture")
                    .size(ButtonSize::Sm.text_size())
                    .into(),
                ButtonKind::DangerSolid,
                ButtonSize::Sm,
                Some(Message::HistoryDeleteConfirmed(id)),
            ),
            bar_button(
                text("Cancel").size(ButtonSize::Sm.text_size()).into(),
                ButtonKind::Ghost,
                ButtonSize::Sm,
                Some(Message::HistoryDeleteCancelled),
            ),
        ]
        .spacing(SP1)
        .align_y(Alignment::Center)
        .into()
    } else {
        icon_button(
            AppIcon::Trash,
            p.t2,
            "Delete Capture",
            Some(HISTORY_KEY_DELETE),
            Some(Message::HistoryDelete(id)),
            false,
            ICON_BTN,
            Position::Top,
        )
    };
    let left_actions = row![
        delete_control,
        icon_button(
            AppIcon::Folder,
            p.t2,
            HISTORY_REVEAL_LABEL,
            None,
            Some(Message::HistoryReveal(id)),
            false,
            ICON_BTN,
            Position::Top,
        ),
        icon_button(
            AppIcon::Open,
            p.t2,
            "Open in Editor",
            Some("↵"),
            Some(Message::HistoryOpenInEditor(id)),
            false,
            ICON_BTN,
            Position::Top,
        ),
    ]
    .spacing(2)
    .align_y(Alignment::Center);
    let has_text = ocr.is_some();
    let text_fg = if has_text {
        p.t1
    } else {
        Color {
            a: p.t1.a * 0.45,
            ..p.t1
        }
    };
    let copy_text = tooltip_with_key(
        bar_button(
            row![
                crate::icons::icon(AppIcon::ScanText, text_fg, ICON_SIZE),
                text("Copy Text").size(ButtonSize::Md.text_size()),
            ]
            .spacing(6)
            .align_y(Alignment::Center)
            .into(),
            ButtonKind::Secondary,
            ButtonSize::Md,
            has_text.then_some(Message::HistoryCopyText(id)),
        ),
        "Copy Text",
        Some(HISTORY_KEY_COPY_TEXT),
        Position::Top,
    );
    let copy_image = tooltip_with_key(
        bar_button(
            row![
                crate::icons::icon(AppIcon::Copy, p.pri_fg, ICON_SIZE),
                text("Copy Image").size(ButtonSize::Md.text_size()),
            ]
            .spacing(6)
            .align_y(Alignment::Center)
            .into(),
            ButtonKind::Primary,
            ButtonSize::Md,
            Some(Message::HistoryCopyImage(id)),
        ),
        "Copy Image",
        Some(HISTORY_KEY_COPY_IMAGE),
        Position::Top,
    );
    let pin = icon_button(
        AppIcon::Pin,
        p.t2,
        "Pin",
        None,
        Some(Message::HistoryPin(id)),
        false,
        ICON_BTN,
        Position::Top,
    );
    let actions: Element<'a, Message> = if stack_actions {
        column![
            row![left_actions, Space::new().width(Length::Fill), pin].align_y(Alignment::Center),
            row![Space::new().width(Length::Fill), copy_text, copy_image]
                .spacing(SP2)
                .align_y(Alignment::Center),
        ]
        .spacing(SP2)
        .into()
    } else {
        row![
            left_actions,
            Space::new().width(Length::Fill),
            pin,
            copy_text,
            copy_image,
        ]
        .spacing(SP2)
        .align_y(Alignment::Center)
        .into()
    };

    column![preview, heading, text_box, actions]
        .spacing(SP3)
        .padding(SP4)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

/// Selected list row: accent tint fill with a 35 % accent hairline.
pub(crate) fn history_selected_fill(p: &Palette) -> Color {
    p.acc_tint
}

pub(crate) fn history_selected_border(p: &Palette) -> Color {
    Color { a: 0.35, ..p.acc }
}

/// Preview and text boxes in the detail pane.
pub(crate) fn history_selected_panel_style(theme: &Theme) -> iced::widget::container::Style {
    let p = palette_for(theme);
    iced::widget::container::Style {
        background: Some(p.bg.into()),
        border: iced::Border {
            color: p.border_subtle,
            width: 1.0,
            radius: R_BUTTON.into(),
        },
        ..Default::default()
    }
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
    use crate::editor::EditorPopover;
    use crate::icons::AppIcon;
    use iced::widget::canvas::Canvas;
    use iced::widget::tooltip::Position;
    use iced::widget::{mouse_area, opaque, stack, Space as IcedSpace};
    use readshot_ui::editor::canvas::EditorCanvas;

    let Some(ed) = state.editor.as_ref() else {
        return container(empty_state(
            AppIcon::Image,
            palette(state.theme_mode),
            "No capture open",
            "Start a capture to annotate, copy, save, pin, or extract text.",
            Some(
                bar_button(
                    text("Capture").size(ButtonSize::Md.text_size()).into(),
                    ButtonKind::Primary,
                    ButtonSize::Md,
                    Some(Message::OpenOverlayRequested),
                )
                .into(),
            ),
        ))
        .width(Length::Fill)
        .height(Length::Fill)
        .style(bg_style)
        .into();
    };

    let active_tool = ed.model.active_tool();
    let active_color = ed.model.current_color();
    let line_width = ed.model.current_line_width();
    let busy = ed.busy;
    let has_selected_annotation = ed.model.selected_annotation().is_some();

    // ===== Chrome — floating bars over the stage =====
    // Title row (zoom), tool bar (tools · color · width), hint, action
    // bar (history · frame · outputs) and at most one popover all float
    // over the image as `stack!` layers. Color, width and frame live in
    // popovers, so both bars stay one row down to the 720 px minimum.
    let p = palette(state.theme_mode);
    let mono = mono_font();
    let dimmed = |c: Color| Color { a: c.a * 0.45, ..c };
    let icon_color = |enabled: bool| if enabled { p.t2 } else { dimmed(p.t2) };
    let popover = ed.popover;
    let can_undo = ed.model.can_undo();
    let can_redo = ed.model.can_redo();

    // --- Tool bar ---
    let mut tool_bar = row![].spacing(2).align_y(Alignment::Center);
    for (i, group) in readshot_ui::editor::toolbar::TOOL_GROUPS.iter().enumerate() {
        if i > 0 {
            tool_bar = tool_bar.push(vsep());
        }
        for &t in group.iter() {
            let (label, key) = tool_label_and_key(t);
            let selected = t == active_tool;
            let color = if busy {
                dimmed(p.t2)
            } else if selected {
                p.acc_text
            } else {
                p.t2
            };
            let btn = icon_button(
                AppIcon::Tool(t),
                color,
                label,
                Some(key),
                (!busy).then_some(Message::EditorToolbar(
                    readshot_ui::ToolbarMessage::SelectTool(t),
                )),
                selected,
                ICON_BTN,
                Position::Bottom,
            );
            tool_bar = tool_bar.push(if tool_shows_new_dot(&state.preferences, t) {
                with_new_tool_dot(btn)
            } else {
                btn
            });
        }
    }
    let tool_layer = container(container(tool_bar).padding(4).style(floating_bar_style))
        .width(Length::Fill)
        .height(Length::Fill)
        .padding(iced::Padding {
            top: TITLEBAR_HEIGHT + 8.0,
            ..iced::Padding::ZERO
        })
        .align_x(Alignment::Center)
        .align_y(Alignment::Start);

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
    // Marks only while the panel is on screen (docked or overlay).
    let panel_visible = matches!(
        editor_panel_mode(state, ed),
        PanelMode::Docked | PanelMode::Overlay
    );
    let (text_hover, text_selected) = text_marks(ed, panel_visible);
    let image_area_content = responsive(move |stage| {
        // The stage is already inset clear of the floating bars.
        let available = stage;
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
            style: ed.model.tool_style(),
            eyedropper: ed.eyedropper.is_some(),
            loupe: ed.eyedropper.as_ref().and_then(|e| e.loupe.clone()),
            text_hover,
            text_selected: text_selected.clone(),
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
                    "Text draft paused while working…".to_string()
                } else {
                    pending.content.clone()
                };
                container(
                    text(content)
                        .size(TEXT_BODY)
                        .style(text_t2)
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
                    .placeholder("Type text… (⌘↵ to commit, esc to cancel)")
                    .on_action(Message::EditorTextAction)
                    .key_binding(editor_text_key_binding)
                    .padding(8)
                    .size(TEXT_BODY)
                    .style(inline_text_editor_style)
                    .into()
            } else {
                // `text_edit_content` is always `Some` while a draft is
                // open; this fallback only guards a torn state.
                container(
                    text(pending.content.clone())
                        .size(TEXT_BODY)
                        .style(text_t1)
                        .wrapping(iced::widget::text::Wrapping::Word),
                )
                .padding(8)
                .width(Length::Fill)
                .into()
            };
            let commit_label = editor_text_commit_label(pending.edit_index.is_some());
            let mut commit = button(text(commit_label).size(TEXT_CAPTION))
                .padding([7, 10])
                .style(|theme, status| action_button_style(theme, status, ActionKind::Primary));
            if !busy {
                commit = commit.on_press(Message::EditorTextCommit);
            }
            let mut cancel = button(text("Cancel").size(TEXT_CAPTION))
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
            .style(inline_text_box_style);
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

        let needs_horizontal_scroll = output_w > available.width + 0.5;
        let needs_vertical_scroll = output_h > available.height + 0.5;
        if needs_horizontal_scroll || needs_vertical_scroll {
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
        }
    })
    .width(Length::Fill)
    .height(Length::Fill);
    let stage = container(image_area_content)
        .width(Length::Fill)
        .height(Length::Fill)
        .padding(iced::Padding {
            top: EDITOR_STAGE_PAD_TOP,
            bottom: EDITOR_STAGE_PAD_BOTTOM,
            left: EDITOR_STAGE_PAD_SIDE,
            right: EDITOR_STAGE_PAD_SIDE,
        })
        .style(bg_style);

    // --- Title row: capture size + zoom ---
    let (img_w, img_h) = ed.effective_image_size();
    // In Fit mode the scale is only known once the stage has laid out
    // (it depends on the stage size), and iced re-runs layout — not
    // `view` — on a window resize. So the zoom readout is built inside a
    // `responsive`, which runs during layout after the stage (the stack's
    // first layer) has recorded its scale.
    let live_scale = ed.render_scale.clone();
    let mode = editor_panel_mode(state, ed);
    let panel_hidden = matches!(mode, PanelMode::Hidden | PanelMode::Collapsed);
    let title_content = responsive(move |_size| {
        let shown_scale = zoom.explicit_scale().unwrap_or_else(|| live_scale.get());
        let mut title = row![
            text("Capture").size(TYPE_BODY).style(text_t2),
            text(format!("{img_w} × {img_h} px"))
                .font(mono)
                .size(TYPE_CAPTION)
                .style(text_t3),
            IcedSpace::new().width(Length::Fill),
        ];
        if panel_hidden {
            title = title.push(icon_button(
                AppIcon::Sidebar,
                p.t2,
                "Show panel",
                Some("⌥⌘P"),
                Some(Message::EditorPanelToggled),
                false,
                ICON_BTN_SM,
                Position::Bottom,
            ));
        }
        title
            .push(editor_zoom_segment(
                shown_scale,
                zoom,
                display_scale,
                busy,
                p,
                mono,
            ))
            .spacing(10)
            .align_y(Alignment::Center)
            .padding(iced::Padding {
                left: TITLEBAR_INSET_LEFT,
                right: 12.0,
                ..iced::Padding::ZERO
            })
            .width(Length::Fill)
            .height(Length::Fixed(TITLEBAR_HEIGHT))
            .into()
    })
    .width(Length::Fill)
    .height(Length::Fixed(TITLEBAR_HEIGHT));
    // The transparent title bar is covered by our content, so the row's
    // empty background is the window's drag handle.
    let mut drag_handle = mouse_area(
        IcedSpace::new()
            .width(Length::Fill)
            .height(Length::Fixed(TITLEBAR_HEIGHT)),
    );
    if let Some(id) = ed.window_id {
        drag_handle = drag_handle.on_press(Message::WindowDragRequested(id));
    }
    let title_layer = container(stack![drag_handle, title_content])
        .width(Length::Fill)
        .height(Length::Fill)
        .align_y(Alignment::Start);

    // --- Options bar under the tool bar (falls back to the hint line) ---
    let options_layer: Element<'_, Message> = if ed.eyedropper.is_some() {
        container(
            text("Click the image to pick a color · esc cancels")
                .size(TYPE_CAPTION)
                .style(text_t3),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .padding(iced::Padding {
            top: EDITOR_HINT_TOP,
            ..iced::Padding::ZERO
        })
        .align_x(Alignment::Center)
        .align_y(Alignment::Start)
        .into()
    } else {
        match editor_options_bar(ed, p) {
            Some(bar) => container(bar)
                .width(Length::Fill)
                .height(Length::Fill)
                .padding(iced::Padding {
                    top: EDITOR_OPTIONS_TOP,
                    ..iced::Padding::ZERO
                })
                .align_x(Alignment::Center)
                .align_y(Alignment::Start)
                .into(),
            None => {
                let hint_str = if let Some(kind) = ed.model.selected_kind_label() {
                    editor_selected_hint(kind, ed.model.selected_text_edit().is_some())
                } else {
                    editor_compact_hint(active_tool).to_string()
                };
                container(text(hint_str).size(TYPE_CAPTION).style(text_t3))
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .padding(iced::Padding {
                        top: EDITOR_HINT_TOP,
                        ..iced::Padding::ZERO
                    })
                    .align_x(Alignment::Center)
                    .align_y(Alignment::Start)
                    .into()
            }
        }
    };

    // --- Status toast above the action bar ---
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
    let discard_armed = ed.discard_pending_at.is_some();
    let toast_layer = status_text.map(|s| {
        let kind = editor_toast_kind(
            &s,
            crate::editor::status_str_is_in_progress(&s),
            discard_armed,
        );
        let action: Option<Element<'_, Message>> = if discard_armed {
            Some(
                row![
                    bar_button(
                        text("Keep Editing").size(ButtonSize::Sm.text_size()).into(),
                        ButtonKind::Secondary,
                        ButtonSize::Sm,
                        Some(Message::EditorDiscardDisarmed),
                    ),
                    bar_button(
                        text("Discard").size(ButtonSize::Sm.text_size()).into(),
                        ButtonKind::DangerSolid,
                        ButtonSize::Sm,
                        Some(Message::EditorDiscardRequested),
                    ),
                ]
                .spacing(SP2)
                .into(),
            )
        } else if let Some(act) = ed.visible_toast_action() {
            let (label, key, msg) = toast_action_parts(act);
            let mut content = row![text(label).size(ButtonSize::Xs.text_size())]
                .spacing(6)
                .align_y(Alignment::Center);
            if let Some(k) = key {
                content = content.push(key_chip(k));
            }
            let mut r = row![bar_button(
                content.into(),
                ButtonKind::Secondary,
                ButtonSize::Xs,
                (!busy).then_some(msg),
            )]
            .spacing(SP1)
            .align_y(Alignment::Center);
            if kind == ToastKind::Error {
                r = r.push(icon_button(
                    AppIcon::Close,
                    p.t2,
                    "Dismiss",
                    None,
                    Some(Message::EditorStatusDismissed),
                    false,
                    ICON_BTN_XS,
                    Position::Top,
                ));
            }
            Some(r.into())
        } else if kind == ToastKind::Error {
            Some(icon_button(
                AppIcon::Close,
                p.t2,
                "Dismiss",
                None,
                Some(Message::EditorStatusDismissed),
                false,
                ICON_BTN_XS,
                Position::Top,
            ))
        } else {
            None
        };
        container(toast(kind, s, action))
            .width(Length::Fill)
            .height(Length::Fill)
            .padding(iced::Padding {
                bottom: EDITOR_TOAST_BOTTOM,
                ..iced::Padding::ZERO
            })
            .align_x(Alignment::Center)
            .align_y(Alignment::End)
    });

    // --- Action bar: history · frame · outputs · discard ---
    let undo_enabled = !busy && can_undo;
    let redo_enabled = !busy && can_redo;
    let frame_open = popover == Some(EditorPopover::Frame);
    let frame_btn = tooltip_with_key(
        bar_button(
            row![
                crate::icons::icon(AppIcon::Frame, p.t2, 15.0),
                text(frame_style.label()).size(ButtonSize::Sm.text_size()),
                crate::icons::icon(AppIcon::Chevron, p.t3, 13.0),
            ]
            .spacing(6)
            .align_y(Alignment::Center)
            .into(),
            if frame_open {
                ButtonKind::Secondary
            } else {
                ButtonKind::Ghost
            },
            ButtonSize::Sm,
            (!busy).then_some(Message::EditorPopoverToggled(EditorPopover::Frame)),
        ),
        "Frame style for Copy, Save and Pin",
        None,
        Position::Top,
    );
    let copy_image = tooltip_with_key(
        bar_button(
            row![
                crate::icons::icon(
                    AppIcon::Copy,
                    if busy { dimmed(p.pri_fg) } else { p.pri_fg },
                    ICON_SIZE
                ),
                text("Copy Image").size(ButtonSize::Md.text_size()),
            ]
            .spacing(6)
            .align_y(Alignment::Center)
            .into(),
            ButtonKind::Primary,
            ButtonSize::Md,
            (!busy).then_some(Message::EditorCopyImageRequested),
        ),
        "Copy Image",
        Some("⌘C"),
        Position::Top,
    );
    let action_bar = row![
        icon_button(
            AppIcon::Undo,
            icon_color(undo_enabled),
            "Undo",
            Some("⌘Z"),
            undo_enabled.then_some(Message::EditorToolbar(readshot_ui::ToolbarMessage::Undo)),
            false,
            ICON_BTN,
            Position::Top,
        ),
        icon_button(
            AppIcon::Redo,
            icon_color(redo_enabled),
            "Redo",
            Some("⇧⌘Z"),
            redo_enabled.then_some(Message::EditorToolbar(readshot_ui::ToolbarMessage::Redo)),
            false,
            ICON_BTN,
            Position::Top,
        ),
        vsep(),
        frame_btn,
        vsep(),
        icon_button(
            AppIcon::Pin,
            icon_color(!busy),
            "Pin",
            Some("⌘P"),
            (!busy).then_some(Message::EditorPinRequested),
            false,
            ICON_BTN,
            Position::Top,
        ),
        icon_button(
            AppIcon::Save,
            icon_color(!busy),
            "Save",
            Some("⌘S"),
            (!busy).then_some(Message::EditorSaveRequested),
            false,
            ICON_BTN,
            Position::Top,
        ),
        icon_button(
            AppIcon::ScanText,
            icon_color(!busy),
            "Copy Text",
            Some("⇧⌘C"),
            (!busy).then_some(Message::EditorCopyTextRequested),
            false,
            ICON_BTN,
            Position::Top,
        ),
        copy_image,
        vsep(),
        icon_button(
            AppIcon::Close,
            icon_color(!busy),
            "Discard",
            Some("⌘W"),
            (!busy).then_some(Message::EditorDiscardRequested),
            false,
            ICON_BTN,
            Position::Top,
        ),
    ]
    .spacing(2)
    .align_y(Alignment::Center);
    let action_layer = container(container(action_bar).padding(4).style(floating_bar_style))
        .width(Length::Fill)
        .height(Length::Fill)
        .padding(iced::Padding {
            bottom: EDITOR_ACTION_BAR_BOTTOM,
            ..iced::Padding::ZERO
        })
        .align_x(Alignment::Center)
        .align_y(Alignment::End);

    let mut layers: Vec<Element<'_, Message>> = vec![
        stage.into(),
        title_layer.into(),
        tool_layer.into(),
        options_layer,
    ];
    if let Some(toast_layer) = toast_layer {
        layers.push(toast_layer.into());
    }
    layers.push(action_layer.into());

    // --- Popover: a click-outside scrim, then the card ---
    if let Some(which) = popover {
        layers.push(
            mouse_area(IcedSpace::new().width(Length::Fill).height(Length::Fill))
                .on_press(Message::EditorPopoverClosed)
                .interaction(iced::mouse::Interaction::Idle)
                .into(),
        );
        let body = match which {
            EditorPopover::Color => color_popover(
                ed.model.effective_color(),
                &state.preferences.editor_recent_colors,
                &ed.hex_draft,
                busy,
                p,
            ),
            EditorPopover::Width => width_popover(
                ed.model.effective_line_width(),
                ed.model.effective_color(),
                busy,
                mono,
            ),
            EditorPopover::Frame => frame_popover(frame_style, busy, p),
        };
        // `opaque` keeps clicks on the card's padding from reaching the
        // scrim underneath (which would close it).
        let card = opaque(container(body).padding(10).style(popover_style));
        let placed = match which {
            // Color and width open from the options bar: under it.
            EditorPopover::Color | EditorPopover::Width => container(card)
                .padding(iced::Padding {
                    top: EDITOR_POPOVER_TOP,
                    ..iced::Padding::ZERO
                })
                .align_y(Alignment::Start),
            // Frame opens from the action bar: above it.
            EditorPopover::Frame => container(card)
                .padding(iced::Padding {
                    bottom: EDITOR_TOAST_BOTTOM,
                    ..iced::Padding::ZERO
                })
                .align_y(Alignment::End),
        };
        layers.push(
            placed
                .width(Length::Fill)
                .height(Length::Fill)
                .align_x(Alignment::Center)
                .into(),
        );
    }

    // Everything above is the canvas area. The side panel docks beside
    // it (the stack is narrower, so its centered bars follow the canvas).
    // In narrow windows it is collapsed (title-row button only), or open
    // as a temporary overlay that a click outside closes.
    let canvas_area: Element<'_, Message> = iced::widget::Stack::with_children(layers)
        .width(Length::Fill)
        .height(Length::Fill)
        .into();
    let scrim = |msg: Message| {
        mouse_area(IcedSpace::new().width(Length::Fill).height(Length::Fill))
            .on_press(msg)
            .interaction(iced::mouse::Interaction::Idle)
    };
    match mode {
        PanelMode::Hidden | PanelMode::Collapsed => canvas_area,
        PanelMode::Docked => {
            let panel = side_panel(state, ed, p, false);
            // While a popover is open, a click on the panel closes it, as
            // a click on the canvas does.
            let panel: Element<'_, Message> = if ed.popover.is_some() {
                container(stack![panel, scrim(Message::EditorPopoverClosed)])
                    .width(Length::Fixed(PANEL_WIDTH))
                    .height(Length::Fill)
                    .into()
            } else {
                panel
            };
            row![canvas_area, panel]
                .width(Length::Fill)
                .height(Length::Fill)
                .into()
        }
        PanelMode::Overlay => stack![
            canvas_area,
            scrim(Message::EditorPanelOverlayDismissed),
            container(opaque(side_panel(state, ed, p, true)))
                .width(Length::Fill)
                .height(Length::Fill)
                .align_x(Alignment::End),
        ]
        .into(),
    }
}

/// Stage insets. Top: title row, tool bar and the hint line above the
/// image (design-editor.html puts the image top at 138). Bottom: the
/// action bar plus the status toast above it, so neither covers the
/// image edge. Sides: breathing room.
pub(crate) const EDITOR_STAGE_PAD_TOP: f32 = 138.0;
pub(crate) const EDITOR_STAGE_PAD_BOTTOM: f32 = 104.0;
pub(crate) const EDITOR_STAGE_PAD_SIDE: f32 = 46.0;
/// Tool hint sits under the tool bar.
pub(crate) const EDITOR_HINT_TOP: f32 = 104.0;
/// Color / width popovers open just under the options bar.
pub(crate) const EDITOR_POPOVER_TOP: f32 = 140.0;
/// Action bar distance from the window's bottom edge.
pub(crate) const EDITOR_ACTION_BAR_BOTTOM: f32 = 20.0;
/// Toasts and the frame popover sit just above the action bar.
pub(crate) const EDITOR_TOAST_BOTTOM: f32 = 66.0;

/// Label, key chip and message of a status-toast button.
pub(crate) fn toast_action_parts(
    action: crate::editor::EditorToastAction,
) -> (&'static str, Option<&'static str>, Message) {
    use crate::editor::EditorToastAction as A;
    match action {
        A::UndoRedaction { .. } => (
            "Undo",
            Some("⌘Z"),
            Message::EditorToolbar(readshot_ui::ToolbarMessage::Undo),
        ),
        A::RetryRecognition => ("Retry", None, Message::EditorTextRecognize),
    }
}

/// Toast kind for the editor status line. Armed discard wins (amber),
/// then errors, then in-flight work; everything else is a success.
pub(crate) fn editor_toast_kind(status: &str, in_progress: bool, discard_armed: bool) -> ToastKind {
    if discard_armed {
        ToastKind::Warn
    } else if in_progress && !crate::editor::status_str_is_error(status) {
        ToastKind::Info
    } else {
        status_toast_kind(status)
    }
}

/// Toast kind for a settled status line (History, Welcome, editor):
/// failures are Error; "No …", "… not found" and "… cancelled" are
/// neutral notices (Info) rather than successes; the rest are Ok.
pub(crate) fn status_toast_kind(status: &str) -> ToastKind {
    if crate::editor::status_str_is_error(status) {
        return ToastKind::Error;
    }
    let lower = status.to_lowercase();
    if lower.starts_with("no ") || lower.contains("not found") || lower.contains("cancelled") {
        ToastKind::Info
    } else {
        ToastKind::Ok
    }
}

/// Title-row zoom controls: − % + | 1:1 Fit. `shown_scale` is the scale
/// the stage is displaying (explicit zoom, or the live fit scale).
fn editor_zoom_segment(
    shown_scale: f32,
    zoom: crate::editor::EditorZoom,
    display_scale: f32,
    busy: bool,
    p: &'static Palette,
    mono: iced::Font,
) -> Element<'static, Message> {
    use crate::icons::AppIcon;
    use iced::widget::tooltip::Position;
    let icon_color = |enabled: bool| {
        if enabled {
            p.t2
        } else {
            Color {
                a: p.t2.a * 0.45,
                ..p.t2
            }
        }
    };
    let can_zoom_out = !busy && zoom.can_zoom_out_from_display_scale(shown_scale);
    let can_zoom_in = !busy && zoom.can_zoom_in_from_display_scale(shown_scale);
    let selected_kind = |on: bool| {
        if on {
            ButtonKind::Secondary
        } else {
            ButtonKind::Ghost
        }
    };
    row![
        icon_button(
            AppIcon::Minus,
            icon_color(can_zoom_out),
            "Zoom out",
            Some("⌘−"),
            can_zoom_out.then_some(Message::EditorZoomOut),
            false,
            ICON_BTN_SM,
            Position::Bottom,
        ),
        text(format!(
            "{:.0}%",
            shown_scale * display_scale.max(f32::EPSILON) * 100.0
        ))
        .font(mono)
        .size(TYPE_CAPTION)
        .style(text_t2)
        .width(Length::Fixed(48.0))
        .align_x(iced::alignment::Horizontal::Center),
        icon_button(
            AppIcon::Plus,
            icon_color(can_zoom_in),
            "Zoom in",
            Some("⌘="),
            can_zoom_in.then_some(Message::EditorZoomIn),
            false,
            ICON_BTN_SM,
            Position::Bottom,
        ),
        vsep(),
        tooltip_with_key(
            bar_button(
                text("1:1").size(ButtonSize::Xs.text_size()).into(),
                selected_kind(zoom.is_actual_size_for_display_scale(display_scale)),
                ButtonSize::Xs,
                (!busy).then_some(Message::EditorZoomActual),
            ),
            "Actual pixels",
            Some("⌘0"),
            Position::Bottom,
        ),
        tooltip_with_key(
            bar_button(
                text("Fit").size(ButtonSize::Xs.text_size()).into(),
                selected_kind(zoom.is_fit()),
                ButtonSize::Xs,
                (!busy).then_some(Message::EditorZoomFit),
            ),
            "Fit to view",
            None,
            Position::Bottom,
        ),
    ]
    .spacing(2)
    .align_y(Alignment::Center)
    .into()
}

pub(crate) fn swatch_color(c: readshot_core::Rgba) -> Color {
    Color::from_rgba(c.r, c.g, c.b, c.a)
}

/// Line width for display: whole numbers without a decimal, half steps
/// with one (`3`, `2.5`) — the slider moves in 0.5 steps.
pub(crate) fn line_width_label(width: f32) -> String {
    if (width - width.round()).abs() < 0.05 {
        format!("{width:.0}")
    } else {
        format!("{width:.1}")
    }
}

/// Text (or icon + text) button in the shared kit's style and height.
/// `msg: None` renders it disabled.
pub(crate) fn bar_button<'a>(
    content: Element<'a, Message>,
    kind: ButtonKind,
    size: ButtonSize,
    msg: Option<Message>,
) -> iced::widget::Button<'a, Message> {
    let mut b = button(container(content).center_y(Length::Fill))
        .height(Length::Fixed(size.height()))
        .padding([0.0, size.padding_x()])
        .style(move |t, s| button_style(t, s, kind));
    if let Some(m) = msg {
        b = b.on_press(m);
    }
    b
}

pub(crate) fn popover_heading<'a>(label: &'a str) -> iced::widget::Text<'a> {
    text(label)
        .size(TYPE_CAPTION)
        .style(text_t2)
        .font(iced::Font {
            weight: iced::font::Weight::Semibold,
            ..iced::Font::DEFAULT
        })
}

fn width_popover<'a>(
    line_width: f32,
    color: readshot_core::Rgba,
    busy: bool,
    mono: iced::Font,
) -> Element<'a, Message> {
    use readshot_ui::editor::toolbar::{MAX_LINE_WIDTH, MIN_LINE_WIDTH};
    const W: f32 = 220.0;
    let control: Element<'a, Message> = if busy {
        passive_line_width_control_sized(line_width, W)
    } else {
        iced::widget::slider(
            MIN_LINE_WIDTH..=MAX_LINE_WIDTH,
            line_width,
            Message::EditorLineWidthPreview,
        )
        .step(0.5)
        .on_release(Message::EditorLineWidthCommit)
        .width(Length::Fixed(W))
        .into()
    };
    let stroke = swatch_color(color);
    let preview = container(
        container(
            Space::new()
                .width(Length::Fixed(150.0))
                .height(Length::Fixed(line_width.clamp(1.0, 26.0))),
        )
        .style(move |_theme: &Theme| container::Style {
            background: Some(stroke.into()),
            border: iced::Border {
                radius: 2.0.into(),
                ..Default::default()
            },
            ..Default::default()
        }),
    )
    .width(Length::Fixed(W))
    .height(Length::Fixed(34.0))
    .center_x(Length::Fixed(W))
    .center_y(Length::Fixed(34.0))
    .style(|theme: &Theme| container::Style {
        background: Some(palette_for(theme).hover.into()),
        border: iced::Border {
            radius: 7.0.into(),
            ..Default::default()
        },
        ..Default::default()
    });
    column![
        row![
            popover_heading("Line width"),
            Space::new().width(Length::Fill),
            text(format!("{} px", line_width_label(line_width)))
                .font(mono)
                .size(TYPE_CAPTION)
                .style(text_t2),
        ]
        .align_y(Alignment::Center)
        .width(Length::Fixed(W)),
        control,
        preview,
    ]
    .spacing(8)
    .into()
}

fn frame_popover<'a>(
    current: EditorFrameStyle,
    busy: bool,
    p: &'static Palette,
) -> Element<'a, Message> {
    const W: f32 = 240.0;
    let mut list = column![].spacing(2).width(Length::Fixed(W));
    for style in EditorFrameStyle::ALL {
        let mut r = row![text(style.label()).size(TYPE_BODY).style(text_t1)]
            .spacing(8)
            .align_y(Alignment::Center);
        r = r.push(Space::new().width(Length::Fill));
        if style == current {
            r = r.push(crate::icons::icon(
                crate::icons::AppIcon::CheckCircle,
                p.acc_text,
                14.0,
            ));
        }
        list = list.push(
            bar_button(
                r.width(Length::Fill).into(),
                ButtonKind::Ghost,
                ButtonSize::Sm,
                (!busy).then_some(Message::EditorFrameStyleChanged(style)),
            )
            .width(Length::Fill),
        );
    }
    column![
        popover_heading("Frame style"),
        list,
        text("Applies to Copy Image, Save and Pin. The canvas previews it.")
            .size(TYPE_CAPTION)
            .style(text_t3)
            .width(Length::Fixed(W))
            .wrapping(iced::widget::text::Wrapping::Word),
    ]
    .spacing(6)
    .into()
}

pub(crate) fn editor_chrome_style(theme: &Theme) -> container::Style {
    let p = palette_for(theme);
    container::Style {
        background: Some(p.float.into()),
        border: iced::Border {
            color: p.border,
            width: 1.0,
            radius: 8.0.into(),
        },
        ..Default::default()
    }
}

/// The floating box around the inline text editor: editor chrome with
/// a focus-colored (teal) border, readable over any image.
pub(crate) fn inline_text_box_style(theme: &Theme) -> container::Style {
    let mut style = editor_chrome_style(theme);
    style.border.color = palette_for(theme).acc;
    style.border.width = 1.5;
    style
}

/// The inline `text_editor` itself: transparent over the box, palette
/// text colors so it reads in light and dark.
pub(crate) fn inline_text_editor_style(
    theme: &Theme,
    _status: iced::widget::text_editor::Status,
) -> iced::widget::text_editor::Style {
    let p = palette_for(theme);
    iced::widget::text_editor::Style {
        background: Color::TRANSPARENT.into(),
        border: iced::Border {
            color: Color::TRANSPARENT,
            width: 0.0,
            radius: 5.0.into(),
        },
        placeholder: p.t3,
        value: p.t1,
        selection: p.acc_tint,
    }
}

/// Render a pin window — borderless, always-on-top, draggable.
/// Click anywhere on the image body initiates a native window
/// drag. Close lives in the hover control bar (and esc / ⌘W).
pub(crate) fn pin_view(state: &App, id: window::Id) -> Element<'_, Message> {
    use crate::icons::AppIcon;
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
    // double-clicks. Close is the hover bar's close button or esc / ⌘W.
    let drag_area = mouse_area(img).on_double_click(Message::PinLockToggled(id));
    let drag_layer: Element<'_, Message> = if locked {
        drag_area.into()
    } else {
        drag_area
            .on_press(Message::PinDragRequested(id))
            .interaction(iced::mouse::Interaction::Grab)
            .into()
    };

    // Hover-only control bar, top-right. Frameless: the pin window is
    // exactly image-sized, so the native window shadow is the only
    // chrome. Keyboard users get everything the bar offers via
    // `pin_key_action` (L, -/+, Esc, arrows), so the bar is a pointer
    // convenience only. Show/hide is instant (no animation, so nothing
    // for Reduce Motion to suppress).
    let hovered = state.hovered_pin == Some(id);
    let controls_layer: Element<'_, Message> = responsive(move |available| {
        let pos = iced::widget::tooltip::Position::Bottom;
        let lock_name = if locked { "Unlock" } else { "Lock" };
        let lock = icon_button(
            AppIcon::Lock,
            DARK.t1,
            lock_name,
            Some("L"),
            Some(Message::PinLockToggled(id)),
            locked,
            ICON_BTN_SM,
            pos,
        );
        if locked && !hovered {
            // Resting locked state: a small badge, no bar.
            let badge = container(crate::icons::icon(AppIcon::Lock, DARK.t1, 13.0))
                .center_x(Length::Fixed(24.0))
                .center_y(Length::Fixed(24.0))
                .style(|_| container::Style {
                    background: Some(Color::from_rgba(0.0, 0.0, 0.0, 0.6).into()),
                    border: iced::Border {
                        radius: 7.0.into(),
                        ..Default::default()
                    },
                    ..Default::default()
                });
            return container(badge)
                .width(Length::Fill)
                .height(Length::Fill)
                .padding(8)
                .align_x(Alignment::End)
                .align_y(Alignment::Start)
                .into();
        }
        if !hovered {
            return Space::new().into();
        }
        let close = icon_button(
            AppIcon::Close,
            DARK.t1,
            "Close",
            Some("esc"),
            Some(Message::PinClosePressed(id)),
            false,
            ICON_BTN_SM,
            pos,
        );
        let step = |delta: f32| {
            Message::PinOpacityChanged(
                id,
                (opacity + delta).clamp(PIN_OPACITY_MIN, PIN_OPACITY_MAX),
            )
        };
        let bar: Element<'_, Message> = if locked {
            // Locked + hovered: only the unlock control.
            row![lock].align_y(Alignment::Center).into()
        } else if pin_controls_compact(available.width) {
            row![lock, close].align_y(Alignment::Center).into()
        } else {
            row![
                lock,
                vsep(),
                icon_button(
                    AppIcon::Minus,
                    DARK.t1,
                    "Less opaque",
                    Some("-"),
                    Some(step(-PIN_OPACITY_STEP)),
                    false,
                    ICON_BTN_SM,
                    pos,
                ),
                text(format!("{:.0}%", opacity * 100.0))
                    .size(TEXT_CAPTION)
                    .font(iced::Font::MONOSPACE)
                    .color(DARK.t2),
                icon_button(
                    AppIcon::Plus,
                    DARK.t1,
                    "More opaque",
                    Some("+"),
                    Some(step(PIN_OPACITY_STEP)),
                    false,
                    ICON_BTN_SM,
                    pos,
                ),
                vsep(),
                close,
            ]
            .spacing(2)
            .align_y(Alignment::Center)
            .into()
        };
        let chrome = container(bar).padding(3).style(|theme| container::Style {
            border: iced::Border {
                radius: 9.0.into(),
                ..floating_bar_style(theme).border
            },
            ..floating_bar_style(theme)
        });
        // No-op `mouse_area` so a press on the bar is consumed here
        // instead of falling through to the drag layer beneath.
        let controls_blocker = mouse_area(chrome).on_press(Message::NoOp);
        container(controls_blocker)
            .width(Length::Fill)
            .height(Length::Fill)
            .padding(8)
            .align_x(Alignment::End)
            .align_y(Alignment::Start)
            .into()
    })
    .into();

    mouse_area(
        container(stack![drag_layer, controls_layer])
            .width(Length::Fill)
            .height(Length::Fill),
    )
    .on_enter(Message::PinHovered(id, true))
    .on_exit(Message::PinHovered(id, false))
    .into()
}

/// Below this pin width the hover bar drops the opacity controls and
/// keeps only Lock and Close (keyboard -/+ still adjusts opacity).
pub(crate) fn pin_controls_compact(width: f32) -> bool {
    width < 230.0
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
        .style(|theme: &Theme| container::Style {
            background: Some(
                Color {
                    a: 0.45,
                    ..palette_for(theme).acc
                }
                .into(),
            ),
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
    .style(line_width_track_style)
    .into()
}

/// Track behind the busy line-width bar.
pub(crate) fn line_width_track_style(theme: &Theme) -> container::Style {
    container::Style {
        background: Some(palette_for(theme).hover.into()),
        border: iced::Border {
            radius: 7.0.into(),
            ..Default::default()
        },
        ..Default::default()
    }
}

pub(crate) fn line_width_fraction(width: f32) -> f32 {
    let min = readshot_ui::editor::toolbar::MIN_LINE_WIDTH;
    let max = readshot_ui::editor::toolbar::MAX_LINE_WIDTH;
    ((width.clamp(min, max) - min) / (max - min)).clamp(0.0, 1.0)
}

pub(crate) fn disabled_text_input_style(theme: &Theme) -> container::Style {
    let p = palette_for(theme);
    container::Style {
        background: Some(p.hover.into()),
        border: iced::Border {
            color: p.border,
            width: 1.0,
            radius: 5.0.into(),
        },
        ..Default::default()
    }
}

#[derive(Clone, Copy)]
pub(crate) enum ActionKind {
    Primary,
    Secondary,
    /// Filled red: the confirming step of a destructive action.
    Danger,
    /// Outlined red: a destructive action's first, arming step. Keeps
    /// Discard / Delete / Clear All from being the loudest control.
    DangerQuiet,
}

pub(crate) fn action_button_style(
    theme: &Theme,
    status: button::Status,
    kind: ActionKind,
) -> button::Style {
    let k = match kind {
        ActionKind::Primary => ButtonKind::Primary,
        ActionKind::Secondary => ButtonKind::Secondary,
        ActionKind::Danger => ButtonKind::DangerSolid,
        ActionKind::DangerQuiet => ButtonKind::Danger,
    };
    button_style(theme, status, k)
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
        T::Spotlight => ("Spotlight", "S"),
        T::NumberedPin => ("Numbered pin", "N"),
        T::Stamp => ("Stamp", "E"),
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
        "s" => Some(T::Spotlight),
        "n" => Some(T::NumberedPin),
        "e" => Some(T::Stamp),
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
        (Key::Character(c), true, false) if modifiers.alt() && c.eq_ignore_ascii_case("p") => {
            return Some(Message::EditorPanelToggled);
        }
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
        // The annotation canvas captures every Escape (publishing
        // `Cancelled` for any live drag), so this is a real Esc press.
        // EditorTextCancel is its single cancel action: it closes an open
        // popover, else exits the eyedropper, else cancels the text draft,
        // else clears the selection.
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
            if c.eq_ignore_ascii_case("i") {
                return Some(Message::EditorEyedropperToggled);
            }
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

// --- Keyboard maps for pointer-only windows (WCAG 2.1.1) ------------
//
// iced 0.14 buttons, togglers and pick-lists cannot take keyboard focus
// (no `Focused` status, no key handling in `iced_widget` button.rs) and
// iced ships no a11y feature, so every pointer-only control the app owns
// needs an explicit per-window keyboard map — the same shape the editor
// (`editor_key_message`), History (runtime subscription) and overlay
// (`overlay.rs`) already use. Each map is a pure `key -> …` fn so it can
// be unit-tested without iced.

/// Slider step / bounds for a pin's image opacity. Shared by the
/// [`pin_view`] slider and the keyboard `-`/`+` adjust so they can't
/// drift apart.
pub(crate) const PIN_OPACITY_MIN: f32 = 0.2;
pub(crate) const PIN_OPACITY_MAX: f32 = 1.0;
pub(crate) const PIN_OPACITY_STEP: f32 = 0.05;

/// Keyboard command for a focused pin window. Resolved against live pin
/// state (current opacity, window position) in the runtime so the map
/// itself stays pure and testable.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum PinKeyAction {
    Close,
    ToggleLock,
    OpacityUp,
    OpacityDown,
    /// Nudge the window by `(dx, dy)` logical pixels.
    Nudge(f32, f32),
}

/// Map a key press in a focused pin window to a [`PinKeyAction`].
/// Esc / ⌘W close, `L` toggles lock, `-`/`+`/`=` step opacity, and the
/// arrow keys nudge the window (1 px, Shift = 10 px, as in the overlay
/// and editor).
pub(crate) fn pin_key_action(
    key: &iced::keyboard::Key,
    modifiers: iced::keyboard::Modifiers,
) -> Option<PinKeyAction> {
    use iced::keyboard::{key::Named, Key};
    if matches!(key, Key::Named(Named::Escape)) {
        return Some(PinKeyAction::Close);
    }
    if modifiers.command() {
        // ⌘W closes; no other ⌘-chord is a pin command.
        if let Key::Character(c) = key {
            if c.eq_ignore_ascii_case("w") {
                return Some(PinKeyAction::Close);
            }
        }
        return None;
    }
    if modifiers.alt() || modifiers.control() {
        return None;
    }
    // Arrow keys move the borderless pin (which otherwise only drags).
    let step = if modifiers.shift() { 10.0 } else { 1.0 };
    match key {
        Key::Named(Named::ArrowLeft) => return Some(PinKeyAction::Nudge(-step, 0.0)),
        Key::Named(Named::ArrowRight) => return Some(PinKeyAction::Nudge(step, 0.0)),
        Key::Named(Named::ArrowUp) => return Some(PinKeyAction::Nudge(0.0, -step)),
        Key::Named(Named::ArrowDown) => return Some(PinKeyAction::Nudge(0.0, step)),
        // `+` is Shift+`=` on most layouts, so the opacity keys ignore
        // Shift; Lock is a plain letter so it must not fire on Shift+L.
        Key::Character(c) => match c.as_str() {
            "+" | "=" => return Some(PinKeyAction::OpacityUp),
            "-" | "_" => return Some(PinKeyAction::OpacityDown),
            _ if !modifiers.shift() && c.eq_ignore_ascii_case("l") => {
                return Some(PinKeyAction::ToggleLock)
            }
            _ => {}
        },
        _ => {}
    }
    None
}

/// Keyboard command for the settings window. Toggles / cycles are
/// resolved against the live `Preferences` in the runtime (so the map
/// doesn't need to know the current pick-list value); the destructive
/// commands resolve against the two-stage arm state so a single
/// keypress can never wipe settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SettingsKeyAction {
    Close,
    ToggleLaunchAtLogin,
    ToggleDebugLogging,
    CycleRetention,
    CycleEngine,
    CycleFormat,
    /// Arm "Reset all settings"; a second press confirms.
    ResetAll,
    /// Arm "Uninstall Readshot"; a second press confirms.
    Uninstall,
    /// ⌘1–⌘7: show that sidebar page.
    SelectPane(SettingsPane),
}

impl SettingsKeyAction {
    /// The pane that shows the setting this shortcut changes. The
    /// runtime switches there first so a change is never off-screen.
    pub(crate) fn owning_pane(self) -> Option<SettingsPane> {
        match self {
            Self::ToggleLaunchAtLogin | Self::ResetAll | Self::Uninstall => Some(SettingsPane::App),
            Self::ToggleDebugLogging => Some(SettingsPane::Diagnostics),
            Self::CycleRetention => Some(SettingsPane::Capture),
            Self::CycleEngine => Some(SettingsPane::TextRecognition),
            Self::CycleFormat => Some(SettingsPane::Export),
            Self::Close | Self::SelectPane(_) => None,
        }
    }
}

/// Map a key press in the settings window to a [`SettingsKeyAction`].
/// Every shortcut is ⌘-prefixed (so plain typing in the filename field
/// is never stolen) except Esc, which closes. The destructive commands
/// use ⇧⌘ to sit apart from the everyday toggles.
pub(crate) fn settings_key_action(
    key: &iced::keyboard::Key,
    modifiers: iced::keyboard::Modifiers,
) -> Option<SettingsKeyAction> {
    use iced::keyboard::{key::Named, Key};
    // Esc leaves the window (no modifier — matches the app convention).
    if matches!(key, Key::Named(Named::Escape))
        && !modifiers.command()
        && !modifiers.alt()
        && !modifiers.control()
    {
        return Some(SettingsKeyAction::Close);
    }
    // Everything else needs ⌘ (⌃ on non-macOS) and nothing exotic.
    if !modifiers.command() || modifiers.alt() || extra_command_modifier(modifiers) {
        return None;
    }
    let Key::Character(c) = key else {
        return None;
    };
    let c = c.to_ascii_lowercase();
    if modifiers.shift() {
        return match c.as_str() {
            "r" => Some(SettingsKeyAction::ResetAll),
            "u" => Some(SettingsKeyAction::Uninstall),
            _ => None,
        };
    }
    match c.as_str() {
        "w" => Some(SettingsKeyAction::Close),
        "l" => Some(SettingsKeyAction::ToggleLaunchAtLogin),
        "d" => Some(SettingsKeyAction::ToggleDebugLogging),
        "r" => Some(SettingsKeyAction::CycleRetention),
        "e" => Some(SettingsKeyAction::CycleEngine),
        "f" => Some(SettingsKeyAction::CycleFormat),
        d if d.len() == 1 && d.as_bytes()[0].is_ascii_digit() => {
            SettingsPane::from_digit(d.as_bytes()[0] - b'0').map(SettingsKeyAction::SelectPane)
        }
        _ => None,
    }
}

/// Primary action of the welcome window for the currently-shown card.
/// Mirrors the cards' own buttons: Pending → Allow, Awaiting/Denied →
/// Open System Settings, Granted → Capture (disabled while one is in
/// flight, so `None` then). Enter triggers it.
pub(crate) fn welcome_primary_message(
    welcome: WelcomeState,
    capture_in_flight: bool,
) -> Option<Message> {
    match welcome {
        WelcomeState::Pending => Some(Message::GrantPermissionRequested),
        WelcomeState::AwaitingGrant | WelcomeState::Denied => {
            Some(Message::OpenPermissionSettingsRequested)
        }
        WelcomeState::Granted => (!capture_in_flight).then_some(Message::OpenOverlayRequested),
    }
}

/// Map a key press in the welcome window: Enter activates the card's
/// primary action. Reset Permission is deliberately *not* bound to a
/// bare key (it is mildly destructive).
pub(crate) fn welcome_key_message(
    welcome: WelcomeState,
    capture_in_flight: bool,
    key: &iced::keyboard::Key,
    modifiers: iced::keyboard::Modifiers,
) -> Option<Message> {
    use iced::keyboard::{key::Named, Key};
    if modifiers.command() || modifiers.alt() || modifiers.control() || modifiers.shift() {
        return None;
    }
    match key {
        Key::Named(Named::Enter) => welcome_primary_message(welcome, capture_in_flight),
        _ => None,
    }
}

/// Map a key press while a scrolling-capture HUD is up. Esc stops &
/// stitches (as the Stop button); Shift+Esc or ⌘. discards the session
/// — the same `ScrollCaptureCancelRequested` the Cancel button sends,
/// which arms on the first press and confirms on the second.
pub(crate) fn scroll_key_message(
    key: &iced::keyboard::Key,
    modifiers: iced::keyboard::Modifiers,
) -> Option<Message> {
    use iced::keyboard::{key::Named, Key};
    match key {
        Key::Named(Named::Escape) if modifiers.shift() => {
            Some(Message::ScrollCaptureCancelRequested)
        }
        Key::Named(Named::Escape) => Some(Message::ScrollCaptureStopRequested),
        Key::Character(c) if c.as_str() == "." && modifiers.command() => {
            Some(Message::ScrollCaptureCancelRequested)
        }
        _ => None,
    }
}

pub(crate) fn editor_compact_hint(tool: readshot_ui::editor::ToolState) -> &'static str {
    use readshot_ui::editor::ToolState as T;
    match tool {
        T::Select => "Click to select · drag to move · ⌫ removes",
        T::Rectangle => "Drag to draw a rectangle.",
        T::Ellipse => "Drag to draw an ellipse.",
        T::Line => "Drag start to end.",
        T::Arrow => "Drag base to target.",
        T::Pen => "Drag to free-draw.",
        T::Highlighter => "Drag over text.",
        T::Text => "Click, type, Enter.",
        T::Blur => "Drag a region to blur.",
        T::Pixelate => "Drag a region to pixelate.",
        T::Spotlight => "Drag an area to spotlight; the rest dims.",
        T::NumberedPin => "Click to drop a number.",
        T::Stamp => "Click to place a stamp.",
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
    // Let Escape through uncaptured: the canvas captures it and the
    // keyboard subscription turns it into the one EditorTextCancel for this
    // press. (iced calls this binding even while the editor is unfocused,
    // so emitting EditorTextCancel here as well made one press cancel
    // twice — e.g. close the popover *and* drop the draft.)
    if matches!(key_press.key, Key::Named(Named::Escape)) {
        return None;
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
    let sel_y = rect.y();
    let bar_rect = crate::overlay::toolbar_rect(
        bounds_w,
        bounds_h,
        iced::Rectangle {
            x: rect.x(),
            y: sel_y,
            width: rect.width(),
            height: rect.height(),
        },
    );
    let toolbar_y = bar_rect.y;

    // Tooltips open away from the selection.
    let tip_pos = if toolbar_y < sel_y {
        tooltip::Position::Top
    } else {
        tooltip::Position::Bottom
    };
    use crate::icons::AppIcon;
    let sel = |intent| Message::OverlaySelected {
        display_id: display_id.clone(),
        rect: *rect,
        intent,
    };
    let fg = DARK.t2;
    let edit = tooltip_with_key(
        button(
            row![
                crate::icons::icon(AppIcon::Edit, DARK.pri_fg, 15.0),
                text("Edit").size(TYPE_BODY)
            ]
            .spacing(7)
            .align_y(Alignment::Center),
        )
        .height(Length::Fixed(BTN_MD))
        .padding([0, 12])
        .style(|t, s| button_style(t, s, ButtonKind::Primary))
        .on_press(sel(CaptureIntent::Editor)),
        "Open in Editor",
        Some("\u{21b5}"),
        tip_pos,
    );
    let buttons = row![
        edit,
        icon_button(
            AppIcon::Copy,
            fg,
            "Copy Image",
            Some("\u{2318}C"),
            Some(sel(CaptureIntent::CopyToClipboard)),
            false,
            ICON_BTN,
            tip_pos
        ),
        icon_button(
            AppIcon::ScanText,
            fg,
            "Copy Text",
            Some("\u{21e7}\u{2318}C"),
            Some(sel(CaptureIntent::CopyTextDirect)),
            false,
            ICON_BTN,
            tip_pos
        ),
        icon_button(
            AppIcon::Save,
            fg,
            "Save",
            Some("\u{2318}S"),
            Some(sel(CaptureIntent::SaveDirect)),
            false,
            ICON_BTN,
            tip_pos
        ),
        icon_button(
            AppIcon::Pin,
            fg,
            "Pin",
            Some("\u{2318}P"),
            Some(sel(CaptureIntent::Pin)),
            false,
            ICON_BTN,
            tip_pos
        ),
        icon_button(
            AppIcon::Scroll,
            fg,
            "Scrolling Capture",
            None,
            Some(sel(CaptureIntent::ScrollCapture)),
            false,
            ICON_BTN,
            tip_pos
        ),
        vsep(),
        icon_button(
            AppIcon::Close,
            fg,
            "Cancel",
            Some("esc"),
            Some(Message::OverlayCancelled),
            false,
            ICON_BTN,
            tip_pos
        ),
    ]
    .spacing(2)
    .align_y(Alignment::Center);
    let bar = container(buttons).padding(4).style(floating_bar_style);

    // Right edge follows the selection (clamped on screen) -- see
    // `toolbar_rect`.
    let right_pad = (bounds_w - (bar_rect.x + bar_rect.width)).max(0.0);

    container(bar)
        .width(Length::Fill)
        .height(Length::Fill)
        .padding(iced::Padding {
            top: toolbar_y,
            right: right_pad,
            bottom: 0.0,
            left: 0.0,
        })
        .align_x(Alignment::End)
        .align_y(Alignment::Start)
        .into()
}

/// Settings window: a 200 px sidebar of seven panes (⌘1–⌘7) beside the
/// selected pane's grouped rows. Every control, message, and keyboard
/// path from the old single-page layout is kept; only placement moved.
pub(crate) fn settings_view(state: &App) -> Element<'_, Message> {
    use iced::widget::tooltip::Position;
    use iced::widget::{mouse_area, stack};

    let p = palette(state.theme_mode);

    // Window drag handle for one of the two title strips (the window
    // uses a transparent full-size title bar on macOS).
    let drag_handle = || {
        let mut handle = mouse_area(
            Space::new()
                .width(Length::Fill)
                .height(Length::Fixed(TITLEBAR_HEIGHT)),
        );
        if let Some(id) = state.settings_window_id {
            handle = handle.on_press(Message::WindowDragRequested(id));
        }
        handle
    };

    // --- Sidebar -------------------------------------------------------
    const PANE_KEYS: [&str; 7] = ["⌘1", "⌘2", "⌘3", "⌘4", "⌘5", "⌘6", "⌘7"];
    let mut nav = column![].spacing(1).padding([4, 8]);
    for (pane, key) in SettingsPane::ALL.into_iter().zip(PANE_KEYS) {
        let selected = pane == state.settings_pane;
        let label_style: fn(&Theme) -> iced::widget::text::Style =
            if selected { text_t1 } else { text_t2 };
        let mut item = row![
            crate::icons::icon(
                pane.icon(),
                if selected { p.acc_text } else { p.t2 },
                ICON_SIZE
            ),
            text(pane.label())
                .size(TYPE_BODY)
                .font(if selected {
                    medium()
                } else {
                    iced::Font::DEFAULT
                })
                .style(label_style),
            Space::new().width(Length::Fill),
        ]
        .spacing(SP2)
        .align_y(Alignment::Center);
        if pane == SettingsPane::Permissions {
            match state.permission_status {
                PermissionStatus::Granted => {
                    item = item.push(crate::icons::icon(
                        crate::icons::AppIcon::CheckCircle,
                        p.ok,
                        14.0,
                    ));
                }
                PermissionStatus::Denied => {
                    item = item.push(crate::icons::icon(
                        crate::icons::AppIcon::Alert,
                        p.warn,
                        14.0,
                    ));
                }
                // Nothing to grant on this platform: no badge.
                PermissionStatus::NotApplicable => {}
            }
        }
        let entry = button(container(item).center_y(Length::Fill))
            .width(Length::Fill)
            .height(Length::Fixed(32.0))
            .padding([0, 10])
            .style(move |t, s| settings_nav_style(t, s, selected))
            .on_press(Message::SettingsPaneSelected(pane));
        nav = nav.push(tooltip_with_key(
            entry,
            pane.label(),
            Some(key),
            Position::Right,
        ));
    }
    let footer = column![
        row![
            key_chip("esc"),
            text("/").size(TYPE_CAPTION).style(text_t3),
            key_chip("⌘W"),
            text("Close").size(TYPE_CAPTION).style(text_t3)
        ]
        .spacing(SP2)
        .align_y(Alignment::Center),
        text(concat!("Readshot ", env!("CARGO_PKG_VERSION")))
            .size(TYPE_CAPTION)
            .style(text_t3),
    ]
    .spacing(6)
    .padding([12, 16]);
    let sidebar = container(column![
        drag_handle(),
        nav,
        Space::new().height(Length::Fill),
        footer
    ])
    .width(Length::Fixed(200.0))
    .height(Length::Fill)
    .style(surface_style);

    // --- Selected pane -------------------------------------------------
    let pane = state.settings_pane;
    let body: Vec<Element<'_, Message>> = match pane {
        SettingsPane::Permissions => settings_permissions_pane(state),
        SettingsPane::Capture => settings_capture_pane(state),
        SettingsPane::Files => settings_files_pane(state),
        SettingsPane::TextRecognition => settings_ocr_pane(state),
        SettingsPane::Export => settings_export_pane(state),
        SettingsPane::Diagnostics => settings_diagnostics_pane(state),
        SettingsPane::App => settings_app_pane(state),
    };
    let title_content = container(
        text(pane.label())
            .size(TYPE_HEADING)
            .font(semibold())
            .style(text_t1),
    )
    .padding([0, 22])
    .width(Length::Fill)
    .height(Length::Fixed(TITLEBAR_HEIGHT))
    .align_y(Alignment::Center);
    let title_row = stack![drag_handle(), title_content]
        .width(Length::Fill)
        .height(Length::Fixed(TITLEBAR_HEIGHT));
    let pane_body = scrollable(
        container(iced::widget::Column::with_children(body).spacing(SP2))
            .width(Length::Fill)
            .padding(iced::Padding {
                top: 2.0,
                right: 22.0,
                bottom: 22.0,
                left: 22.0,
            }),
    )
    .direction(iced::widget::scrollable::Direction::Vertical(
        slim_scrollbar(),
    ))
    .height(Length::Fill);

    container(row![
        sidebar,
        vhairline(),
        column![title_row, pane_body]
            .width(Length::Fill)
            .height(Length::Fill),
    ])
    .width(Length::Fill)
    .height(Length::Fill)
    .style(bg_style)
    .into()
}

fn medium() -> iced::Font {
    iced::Font {
        weight: iced::font::Weight::Medium,
        ..iced::Font::DEFAULT
    }
}

/// Sidebar entry: accent tint when selected (teal marks selection only),
/// hover fill otherwise.
fn settings_nav_style(theme: &Theme, status: button::Status, selected: bool) -> button::Style {
    let p = palette_for(theme);
    let bg = if selected {
        p.acc_tint
    } else if matches!(status, button::Status::Hovered | button::Status::Pressed) {
        p.hover
    } else {
        Color::TRANSPARENT
    };
    button::Style {
        background: Some(bg.into()),
        text_color: if selected { p.t1 } else { p.t2 },
        border: iced::Border {
            radius: 7.0.into(),
            ..iced::Border::default()
        },
        ..button::Style::default()
    }
}

/// Small caption above a group ("Shortcut", "Danger zone").
fn settings_group_heading<'a>(
    label: &'a str,
    style: fn(&Theme) -> iced::widget::text::Style,
) -> Element<'a, Message> {
    container(text(label).size(TYPE_CAPTION).font(semibold()).style(style))
        .padding(iced::Padding {
            top: 10.0,
            right: 0.0,
            bottom: 0.0,
            left: 2.0,
        })
        .into()
}

/// Rounded group of rows with hairlines between them.
fn settings_group<'a>(rows: Vec<Element<'a, Message>>) -> Element<'a, Message> {
    let mut col = column![].width(Length::Fill);
    for (i, r) in rows.into_iter().enumerate() {
        if i > 0 {
            col = col.push(hairline());
        }
        col = col.push(r);
    }
    container(col).width(Length::Fill).style(group_style).into()
}

/// A group row whose control is too wide to sit beside its label (value
/// boxes with buttons, text fields, checkbox lists): label and help on
/// top, the control full-width below. Same padding as [`group_row`].
fn settings_block<'a>(
    label: &'a str,
    help: impl Into<String>,
    control: Element<'a, Message>,
) -> Element<'a, Message> {
    let help = help.into();
    let mut head = column![text(label).size(TYPE_BODY).style(text_t1)].spacing(2);
    if !help.is_empty() {
        head = head.push(
            text(help)
                .size(TYPE_CAPTION)
                .style(text_t3)
                .wrapping(iced::widget::text::Wrapping::Word),
        );
    }
    column![head, control]
        .spacing(SP2)
        .padding([10, 14])
        .width(Length::Fill)
        .into()
}

fn settings_permissions_pane(state: &App) -> Vec<Element<'_, Message>> {
    // Never probe from view(): it runs on every redraw.
    let permission_status = state.permission_status;
    let (permission_title, permission_hint) = permission_settings_summary(permission_status);
    let permission_control: Element<'_, Message> = responsive(move |available| {
        let status = column![
            text(permission_title).size(TEXT_BODY_SM).style(text_t1),
            text(permission_hint)
                .size(TEXT_CAPTION)
                .style(text_t3)
                .wrapping(iced::widget::text::Wrapping::Word),
        ]
        .spacing(6)
        .width(Length::Fill);

        if matches!(permission_status, PermissionStatus::NotApplicable) {
            return status.into();
        }

        let open_button = button(text("Open Settings").size(TEXT_BODY_SM))
            .padding([6, 12])
            .style(|t, s| action_button_style(t, s, ActionKind::Secondary))
            .on_press(Message::OpenPermissionSettingsRequested);
        // Recheck button forces a fresh permission poll so the user
        // can confirm a System Settings change without quitting and
        // relaunching the app. Drives the existing PermissionPoll
        // handler with the latest probe result.
        let recheck_button = button(text("Recheck").size(TEXT_BODY_SM))
            .padding([6, 12])
            .style(|t, s| action_button_style(t, s, ActionKind::Secondary))
            .on_press(Message::PermissionTick);
        // Clears a grant macOS still holds for an earlier, differently
        // signed version, then asks again.
        let reset_button = button(text("Reset Permission").size(TEXT_BODY_SM))
            .padding([6, 12])
            .style(|t, s| action_button_style(t, s, ActionKind::Secondary))
            .on_press(Message::ResetPermissionRequested);
        // Once access is granted there is nothing to recheck or reset.
        let actions = if matches!(permission_status, PermissionStatus::Granted) {
            row![open_button]
        } else {
            row![recheck_button, reset_button, open_button]
        }
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
    vec![
        settings_group_heading("Capture access", text_t3),
        settings_group(vec![container(permission_control)
            .padding([10, 14])
            .width(Length::Fill)
            .into()]),
    ]
}

fn settings_capture_pane(state: &App) -> Vec<Element<'_, Message>> {
    use iced::widget::pick_list;
    use iced::widget::tooltip::Position;
    use readshot_core::HistoryRetention;

    // `pick_list` borrows its options for the duration of the
    // returned `Element`, so a `'static` slice keeps the lifetime
    // story simple.
    const RETENTION_OPTIONS: [HistoryRetention; 4] = [
        HistoryRetention::Off,
        HistoryRetention::Last50,
        HistoryRetention::Last30Days,
        HistoryRetention::Unlimited,
    ];

    let pretty = pretty_hotkey(&state.preferences.capture_hotkey);
    let mut hotkey_hint = if state.settings_recording_hotkey {
        state
            .settings_hotkey_error
            .clone()
            .unwrap_or_else(|| "Press a modifier shortcut now. Escape cancels.".to_string())
    } else if let Some(err) = &state.settings_hotkey_error {
        // A rejected chord (e.g. a conflict with a reserved hotkey) or a
        // registration failure. Show it even when recording has ended.
        err.clone()
    } else if let Some(status) = &state.settings_hotkey_status {
        status.clone()
    } else if readshot_ui::hotkey::parse(&state.preferences.capture_hotkey).is_err() {
        format!(
            "Couldn't read `{}` — try `cmd+shift+x` style.",
            state.preferences.capture_hotkey
        )
    } else if let Some(reserved) = capture_hotkey_conflict(&state.preferences.capture_hotkey) {
        format!("{pretty} is reserved for the {reserved} shortcut. Pick another chord.")
    } else if !state.capture_hotkey_registered {
        // Parsed-OK but registration failed, e.g. another app already
        // owns the chord. Tell the user so they pick another one.
        format!("{pretty} — couldn't grab globally; try a different chord.")
    } else {
        String::new()
    };
    // A fixed hotkey (History / Settings) that couldn't be registered is
    // surfaced here so the failure isn't silent.
    if !state.fixed_hotkey_failures.is_empty() {
        let names = state.fixed_hotkey_failures.join(" and ");
        let plural = if state.fixed_hotkey_failures.len() > 1 {
            "shortcuts"
        } else {
            "shortcut"
        };
        let warning = format!("⚠ The {names} {plural} couldn't be registered globally.");
        hotkey_hint = if hotkey_hint.is_empty() {
            warning
        } else {
            format!("{hotkey_hint}  {warning}")
        };
    }
    if hotkey_hint.is_empty() {
        hotkey_hint = "Opens the capture overlay from any app.".to_string();
    }
    let hotkey_label = if state.settings_recording_hotkey {
        "Press shortcut…".to_string()
    } else {
        pretty
    };
    let settings_recording_hotkey = state.settings_recording_hotkey;
    let hotkey_control: Element<'_, Message> = responsive(move |available| {
        let value = setting_value_box(hotkey_label.clone(), settings_recording_hotkey);
        // Record / Cancel are exclusive: while a recording is armed, the
        // user needs an obvious exit that doesn't require pressing a
        // real shortcut.
        let primary_btn = if settings_recording_hotkey {
            button(text("Cancel").size(TEXT_BODY_SM))
                .padding([6, 12])
                .style(|t, s| action_button_style(t, s, ActionKind::Secondary))
                .on_press(Message::SettingsHotkeyRecordingCancelled)
        } else {
            button(text("Record").size(TEXT_BODY_SM))
                .padding([6, 12])
                .style(|t, s| action_button_style(t, s, ActionKind::Secondary))
                .on_press(Message::SettingsStartHotkeyRecording)
        };
        let actions = row![
            primary_btn,
            button(text("Reset").size(TEXT_BODY_SM))
                .padding([6, 12])
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
                text("Press the new shortcut, or esc to cancel.")
                    .size(TEXT_CAPTION)
                    .style(text_acc),
            ]
            .spacing(6)
            .into()
        } else {
            main
        }
    })
    .height(Length::Shrink)
    .into();

    let retention_control = tooltip_with_key(
        pick_list(
            &RETENTION_OPTIONS[..],
            Some(state.preferences.history_retention),
            |r| Message::Settings(SettingsMessage::SetHistoryRetention(r)),
        )
        .text_size(TEXT_BODY_SM)
        .padding([7, 10]),
        "Next retention option",
        Some("⌘R"),
        Position::Top,
    );

    vec![
        settings_group_heading("Shortcut", text_t3),
        settings_group(vec![settings_block(
            "Capture hotkey",
            hotkey_hint,
            hotkey_control,
        )]),
        settings_group_heading("History", text_t3),
        settings_group(vec![group_row(
            "History retention",
            "Searchable archive of captures. Off keeps everything in-memory only.",
            retention_control,
        )]),
    ]
}

fn settings_files_pane(state: &App) -> Vec<Element<'_, Message>> {
    use iced::widget::text_input;

    let save_folder_value = if state.preferences.save_folder.as_os_str().is_empty() {
        "Platform default".to_string()
    } else {
        state.preferences.save_folder.display().to_string()
    };
    let filename_template = state.preferences.filename_template.clone();

    let save_folder_control: Element<'_, Message> = column![
        setting_value_box(save_folder_value, false),
        row![
            button(text("Choose").size(TEXT_BODY_SM))
                .padding([6, 12])
                .style(|t, s| action_button_style(t, s, ActionKind::Secondary))
                .on_press(Message::SettingsChooseSaveFolderRequested),
            button(text("Open").size(TEXT_BODY_SM))
                .padding([6, 12])
                .style(|t, s| action_button_style(t, s, ActionKind::Secondary))
                .on_press(Message::SettingsOpenSaveFolderRequested),
            button(text("Reset").size(TEXT_BODY_SM))
                .padding([6, 12])
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
    let preview_template = if filename_template.trim().is_empty() {
        "Screenshot {YYYY-MM-DD at HH.mm.ss}".to_string()
    } else {
        filename_template.clone()
    };
    let preview_name = default_save_filename(&preview_template, chrono::Utc::now());
    let filename_input = text_input("Screenshot {YYYY-MM-DD at HH.mm.ss}", &filename_template)
        .on_input(|s| Message::Settings(SettingsMessage::SetFilenameTemplate(s)))
        .size(TEXT_BODY_SM)
        .padding([8, 10])
        .style(text_input_style);
    let preview_line = text(format!("Saves as: {preview_name}"))
        .size(TEXT_CAPTION)
        .style(text_t3);
    let filename_control: Element<'_, Message> =
        column![filename_input, preview_line].spacing(4).into();

    vec![
        settings_group_heading("Save location and naming", text_t3),
        settings_group(vec![
            settings_block(
                "Default save folder",
                "Starting folder for Save. Platform default uses your system screenshots location.",
                save_folder_control,
            ),
            settings_block(
                "Filename template",
                "Tokens: {YYYY-MM-DD at HH.mm.ss} · {YYYY-MM-DD} · {HH.mm.ss} · {YYYY} · {timestamp}",
                filename_control,
            ),
        ]),
    ]
}

fn settings_ocr_pane(state: &App) -> Vec<Element<'_, Message>> {
    use iced::widget::pick_list;
    use iced::widget::tooltip::Position;
    use readshot_core::OcrEngineChoice;

    const ENGINE_OPTIONS: [OcrEngineChoice; 2] =
        [OcrEngineChoice::Native, OcrEngineChoice::Tesseract];

    let ocr_supported = state.coordinator.supported_languages();
    let ocr_selected = state.preferences.ocr_languages.clone();
    let engine_control = tooltip_with_key(
        pick_list(
            &ENGINE_OPTIONS[..],
            Some(state.preferences.ocr_engine_choice),
            |c| Message::Settings(SettingsMessage::SetOcrEngine(c)),
        )
        .text_size(TEXT_BODY_SM)
        .padding([7, 10]),
        "Next OCR engine",
        Some("⌘E"),
        Position::Top,
    );
    vec![
        settings_group_heading("OCR engine and languages", text_t3),
        settings_group(vec![
            group_row(
                "OCR engine",
                "System default uses your platform's built-in recognizer. Takes effect after restart.",
                engine_control,
            ),
            settings_block(
                "Recognition languages",
                "Leave all off to let the recognizer choose. Applies to your next capture.",
                ocr_language_control(&ocr_supported, &ocr_selected),
            ),
        ]),
    ]
}

fn settings_export_pane(state: &App) -> Vec<Element<'_, Message>> {
    use iced::widget::pick_list;
    use iced::widget::tooltip::Position;
    use readshot_core::ExportFormat;

    const FORMAT_OPTIONS: [ExportFormat; 3] =
        [ExportFormat::Png, ExportFormat::Jpeg, ExportFormat::Webp];

    let format_control = tooltip_with_key(
        pick_list(
            &FORMAT_OPTIONS[..],
            Some(state.preferences.default_format),
            |f| Message::Settings(SettingsMessage::SetDefaultFormat(f)),
        )
        .text_size(TEXT_BODY_SM)
        .padding([7, 10]),
        "Next format",
        Some("⌘F"),
        Position::Top,
    );
    vec![
        settings_group_heading("Default image format", text_t3),
        settings_group(vec![group_row(
            "Default format",
            "Format used when you Save a capture.",
            format_control,
        )]),
    ]
}

fn settings_diagnostics_pane(state: &App) -> Vec<Element<'_, Message>> {
    use iced::widget::toggler;
    use iced::widget::tooltip::Position;

    // No update-channel picker: the appcast publishes no channel-tagged
    // items and the Sparkle bridge sets no `allowedChannels`, so a
    // Stable/Beta choice would change nothing.
    let debug_toggle = tooltip_with_key(
        toggler(state.preferences.debug_logging)
            .on_toggle(|v| Message::Settings(SettingsMessage::SetDebugLogging(v))),
        "Toggle debug logging",
        Some("⌘D"),
        Position::Top,
    );
    vec![
        settings_group_heading("Logs for troubleshooting", text_t3),
        settings_group(vec![group_row(
            "Verbose debug logging",
            "Logs at DEBUG level. Takes effect after restart.",
            debug_toggle,
        )]),
    ]
}

fn settings_app_pane(state: &App) -> Vec<Element<'_, Message>> {
    use iced::widget::toggler;
    use iced::widget::tooltip::Position;

    let startup_toggle = tooltip_with_key(
        toggler(state.preferences.launch_at_login)
            .on_toggle(|v| Message::Settings(SettingsMessage::SetLaunchAtLogin(v))),
        "Toggle open at login",
        Some("⌘L"),
        Position::Top,
    );

    // Both destructive rows keep the two-step arm/confirm pattern: the
    // idle button (outlined red) arms, the armed step shows a filled-red
    // confirm plus Cancel. ⇧⌘R / ⇧⌘U press the same two steps.
    let reset_row: Element<'_, Message> = if state.settings_reset_all_pending {
        settings_block(
            "Reset All Settings",
            "Resets hotkey, save folder, filename template, retention, OCR, and startup options. History records are kept.",
            row![
                tooltip_with_key(
                    button(text("Confirm Reset").size(TEXT_BODY_SM))
                        .padding([6, 12])
                        .style(|t, s| action_button_style(t, s, ActionKind::Danger))
                        .on_press(Message::SettingsResetAllConfirmed),
                    "Confirm reset",
                    Some("⇧⌘R"),
                    Position::Top,
                ),
                button(text("Cancel").size(TEXT_BODY_SM))
                    .padding([6, 12])
                    .style(|t, s| action_button_style(t, s, ActionKind::Secondary))
                    .on_press(Message::SettingsResetAllCancelled),
            ]
            .spacing(8)
            .align_y(Alignment::Center)
            .into(),
        )
    } else {
        let reset_status = state
            .settings_status
            .as_deref()
            .unwrap_or("Return all preferences to the shipped defaults.")
            .to_string();
        group_row(
            "Reset All Settings",
            reset_status,
            tooltip_with_key(
                button(text("Reset…").size(TEXT_BODY_SM))
                    .padding([6, 12])
                    .style(|t, s| action_button_style(t, s, ActionKind::DangerQuiet))
                    .on_press(Message::SettingsResetAllRequested),
                "Reset all settings",
                Some("⇧⌘R"),
                Position::Top,
            ),
        )
    };

    let uninstall_row: Element<'_, Message> = if state.settings_uninstall_pending {
        let purge_checkbox = iced::widget::checkbox(state.settings_uninstall_purge)
            .label("Also delete history and preferences")
            .size(16)
            .text_size(TEXT_BODY_SM)
            .on_toggle(Message::SettingsUninstallPurgeToggled);
        let actions = row![
            tooltip_with_key(
                button(text("Uninstall").size(TEXT_BODY_SM))
                    .padding([6, 12])
                    .style(|t, s| action_button_style(t, s, ActionKind::Danger))
                    .on_press(Message::SettingsUninstallConfirmed),
                "Confirm uninstall",
                Some("⇧⌘U"),
                Position::Top,
            ),
            button(text("Cancel").size(TEXT_BODY_SM))
                .padding([6, 12])
                .style(|t, s| action_button_style(t, s, ActionKind::Secondary))
                .on_press(Message::SettingsUninstallCancelled),
        ]
        .spacing(8)
        .align_y(Alignment::Center);
        settings_block(
            "Uninstall Readshot",
            "Removes the launch-at-login item, the readshot command-line tools, and the \
             Screen Recording permission, then moves Readshot to the Trash.",
            column![purge_checkbox, actions].spacing(10).into(),
        )
    } else {
        group_row(
            "Uninstall Readshot",
            "Remove Readshot and its command-line tools from this Mac.",
            tooltip_with_key(
                button(text("Uninstall…").size(TEXT_BODY_SM))
                    .padding([6, 12])
                    .style(|t, s| action_button_style(t, s, ActionKind::DangerQuiet))
                    .on_press(Message::SettingsUninstallRequested),
                "Uninstall Readshot",
                Some("⇧⌘U"),
                Position::Top,
            ),
        )
    };

    vec![
        settings_group_heading("Startup", text_t3),
        settings_group(vec![group_row(
            "Open Readshot at login",
            "",
            startup_toggle,
        )]),
        settings_group_heading("Danger zone", text_danger),
        settings_group(vec![reset_row, uninstall_row]),
    ]
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
        // Show a human-readable name (e.g. "Chinese (Simplified)") but
        // keep storing the raw BCP-47 code that Vision reports.
        let display = ocr_language_display_name(&lang);
        items.push(
            iced::widget::checkbox(checked)
                .label(display)
                .size(16)
                .text_size(TEXT_BODY_SM)
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

/// Human-readable display name for a BCP-47 language code that Apple
/// Vision reports from `supportedRecognitionLanguages`. The stored
/// preference keeps the raw code; this only affects what the OCR
/// language checkboxes show. Unknown codes fall back to the raw code so
/// a new OS language never renders blank.
pub(crate) fn ocr_language_display_name(code: &str) -> String {
    let name = match code {
        "en-US" => "English (US)",
        "fr-FR" => "French",
        "it-IT" => "Italian",
        "de-DE" => "German",
        "es-ES" => "Spanish",
        "pt-BR" => "Portuguese (Brazil)",
        "zh-Hans" => "Chinese (Simplified)",
        "zh-Hant" => "Chinese (Traditional)",
        "yue-Hans" => "Cantonese (Simplified)",
        "yue-Hant" => "Cantonese (Traditional)",
        "ko-KR" => "Korean",
        "ja-JP" => "Japanese",
        "ru-RU" => "Russian",
        "uk-UA" => "Ukrainian",
        "th-TH" => "Thai",
        "vi-VT" | "vi-VN" => "Vietnamese",
        "ar-SA" => "Arabic",
        "ars-SA" => "Arabic (Najdi)",
        "tr-TR" => "Turkish",
        "id-ID" => "Indonesian",
        "cs-CZ" => "Czech",
        "da-DK" => "Danish",
        "nl-NL" => "Dutch",
        "no-NO" => "Norwegian",
        "nb-NO" => "Norwegian Bokmål",
        "nn-NO" => "Norwegian Nynorsk",
        "ms-MY" => "Malay",
        "pl-PL" => "Polish",
        "ro-RO" => "Romanian",
        "sv-SE" => "Swedish",
        other => other,
    };
    name.to_string()
}

pub(crate) fn setting_value_box(value: String, active: bool) -> Element<'static, Message> {
    container(
        text(value)
            .size(TEXT_BODY_SM)
            .style(text_t1)
            .width(Length::Fill),
    )
    .width(Length::Fill)
    .padding([9, 12])
    .style(move |theme: &Theme| {
        // Active (recording a hotkey) is a focus state: accent ring.
        let p = palette_for(theme);
        iced::widget::container::Style {
            background: Some(if active { p.acc_tint } else { p.raised }.into()),
            border: iced::Border {
                color: if active { p.acc } else { p.border },
                width: if active { 2.0 } else { 1.0 },
                radius: R_BUTTON.into(),
            },
            ..Default::default()
        }
    })
    .into()
}

pub(crate) fn permission_settings_summary(
    status: PermissionStatus,
) -> (&'static str, &'static str) {
    match status {
        PermissionStatus::Granted => (
            "Screen Recording is allowed",
            "Readshot can open the capture overlay and recognize text from screenshots.",
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
    use crate::icons::AppIcon;
    use iced::widget::tooltip::Position;
    let p = palette(state.theme_mode);

    let mut col = column![]
        .spacing(SP4)
        .align_x(Alignment::Center)
        .width(Length::Fill);

    match state.welcome {
        WelcomeState::Pending => {
            col = col.push(welcome_brand(p, WelcomeTile::App));
            col = col.push(welcome_status_group(
                AppIcon::Shield,
                p.warn,
                "Screen Recording access needed",
                "macOS asks once. Readshot only reads the screen when you capture.",
            ));
        }
        WelcomeState::AwaitingGrant | WelcomeState::Denied => {
            let (title, help) = welcome_permission_guidance(state.welcome);
            col = col.push(welcome_brand(p, WelcomeTile::App));
            col = col.push(welcome_status_group(AppIcon::Alert, p.warn, title, help));
        }
        WelcomeState::Granted => {
            col = col.push(welcome_brand(p, WelcomeTile::Done));
            let hotkey = pretty_hotkey(&state.preferences.capture_hotkey);
            col = col.push(
                row![
                    text("Press").size(TYPE_BODY).style(text_t2),
                    key_chip(hotkey),
                    text("anywhere to capture.").size(TYPE_BODY).style(text_t2),
                ]
                .spacing(SP2)
                .align_y(Alignment::Center),
            );
        }
    }

    if let Some(s) = &state.last_capture_status {
        let kind = status_toast_kind(s);
        let action = (kind == ToastKind::Error).then(|| {
            icon_button(
                AppIcon::Close,
                p.t2,
                "Dismiss",
                None,
                Some(Message::WelcomeStatusDismissed),
                false,
                ICON_BTN_XS,
                Position::Top,
            )
        });
        col = col.push(toast(kind, s.clone(), action));
    }

    let mut actions = column![]
        .spacing(SP4)
        .align_x(Alignment::Center)
        .width(Length::Fill);
    match state.welcome {
        WelcomeState::Pending => {
            actions = actions.push(welcome_button(
                None,
                "Allow Screen Recording",
                ButtonKind::Primary,
                ButtonSize::Lg,
                Some("\u{21B5}"),
                p,
                Some(Message::GrantPermissionRequested),
            ));
            actions = actions.push(welcome_reset_footer());
        }
        WelcomeState::AwaitingGrant | WelcomeState::Denied => {
            actions = actions.push(welcome_button(
                None,
                "Open System Settings",
                ButtonKind::Primary,
                ButtonSize::Lg,
                Some("\u{21B5}"),
                p,
                Some(Message::OpenPermissionSettingsRequested),
            ));
            actions = actions.push(welcome_button(
                None,
                "Restart Readshot",
                ButtonKind::Secondary,
                ButtonSize::Md,
                None,
                p,
                Some(Message::RestartRequested),
            ));
            actions = actions.push(welcome_reset_footer());
        }
        WelcomeState::Granted => {
            actions = actions.push(welcome_button(
                Some(AppIcon::Crosshair),
                "Capture Screen",
                ButtonKind::Primary,
                ButtonSize::Lg,
                (!state.capture_in_flight).then_some("\u{21B5}"),
                p,
                (!state.capture_in_flight).then_some(Message::OpenOverlayRequested),
            ));
            actions = actions.push(
                row![
                    welcome_button(
                        Some(AppIcon::History),
                        "Show History",
                        ButtonKind::Secondary,
                        ButtonSize::Md,
                        None,
                        p,
                        Some(Message::OpenHistoryRequested),
                    ),
                    welcome_button(
                        Some(AppIcon::Gear),
                        "Open Settings",
                        ButtonKind::Secondary,
                        ButtonSize::Md,
                        None,
                        p,
                        Some(Message::OpenSettingsRequested),
                    ),
                ]
                .spacing(SP2),
            );
        }
    }

    // Upper content scrolls; the action area stays pinned so the primary
    // action is visible at the minimum window height.
    let body = column![
        scrollable(col.width(Length::Fill))
            .height(Length::Fill)
            .direction(scrollable::Direction::Vertical(slim_scrollbar())),
        actions,
    ]
    .spacing(SP4)
    .width(Length::Fill)
    .height(Length::Fill);
    container(body)
        .width(Length::Fill)
        .height(Length::Fill)
        .padding(iced::Padding {
            top: 56.0,
            right: 40.0,
            bottom: 32.0,
            left: 40.0,
        })
        .style(surface_style)
        .into()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum WelcomeTile {
    /// Dark rounded app tile with the scan icon (permission states).
    App,
    /// Green circle with a check (granted).
    Done,
}

/// Tile plus the title (and, for the permission states, the tagline).
fn welcome_brand(p: &'static Palette, tile: WelcomeTile) -> Element<'static, Message> {
    use crate::icons::AppIcon;
    let (glyph, color, radius, fill) = match tile {
        WelcomeTile::App => (
            AppIcon::ScanText,
            p.acc_text,
            16.0,
            Color::from_rgb8(0x14, 0x18, 0x1E),
        ),
        WelcomeTile::Done => (AppIcon::CheckCircle, p.ok, 32.0, Color { a: 0.14, ..p.ok }),
    };
    let tile_el = container(crate::icons::icon(glyph, color, 30.0))
        .width(Length::Fixed(64.0))
        .height(Length::Fixed(64.0))
        .center_x(Length::Fixed(64.0))
        .center_y(Length::Fixed(64.0))
        .style(move |_| container::Style {
            background: Some(fill.into()),
            border: iced::Border {
                radius: radius.into(),
                ..Default::default()
            },
            ..Default::default()
        });
    let (title, tagline) = match tile {
        WelcomeTile::App => ("Readshot", Some("Capture, search, find again.")),
        WelcomeTile::Done => ("You're all set.", None),
    };
    let mut c = column![
        tile_el,
        text(title).size(TYPE_TITLE).font(semibold()).style(text_t1),
    ]
    .spacing(SP3)
    .align_x(Alignment::Center);
    if let Some(t) = tagline {
        c = c.push(text(t).size(TYPE_BODY).style(text_t2));
    }
    c.into()
}

/// Status group: icon, bold title, help text.
fn welcome_status_group<'a>(
    glyph: crate::icons::AppIcon,
    color: Color,
    title: &'a str,
    help: &'a str,
) -> Element<'a, Message> {
    container(
        row![
            crate::icons::icon(glyph, color, 20.0),
            column![
                text(title).size(TYPE_BODY).font(semibold()).style(text_t1),
                text(help)
                    .size(TYPE_CAPTION)
                    .style(text_t2)
                    .wrapping(iced::widget::text::Wrapping::Word),
            ]
            .spacing(2)
            .width(Length::Fill),
        ]
        .spacing(SP3),
    )
    .padding([SP3, SP4])
    .width(Length::Fill)
    .style(group_style)
    .into()
}

/// "Already on in System Settings but still blocked?" + ghost link.
fn welcome_reset_footer<'a>() -> Element<'a, Message> {
    row![
        text("Already on in System Settings but still blocked?")
            .size(TYPE_CAPTION)
            .style(text_t3),
        button(text("Reset Permission").size(TYPE_CAPTION))
            .padding([2, 6])
            .style(|t, s| button_style(t, s, ButtonKind::Ghost))
            .on_press(Message::ResetPermissionRequested),
    ]
    .spacing(SP1)
    .align_y(Alignment::Center)
    .into()
}

/// Full-width (Lg / primary column) or content-width button in the kit
/// style: optional leading icon, label, optional trailing key chip.
/// `msg: None` renders it disabled.
fn welcome_button<'a>(
    glyph: Option<crate::icons::AppIcon>,
    label: &'static str,
    kind: ButtonKind,
    size: ButtonSize,
    key: Option<&'static str>,
    p: &'static Palette,
    msg: Option<Message>,
) -> Element<'a, Message> {
    let fg = match kind {
        ButtonKind::Primary => p.pri_fg,
        ButtonKind::Ghost => p.t2,
        _ => p.t1,
    };
    let mut content = row![].spacing(SP2).align_y(Alignment::Center);
    if let Some(g) = glyph {
        content = content.push(crate::icons::icon(g, fg, ICON_SIZE));
    }
    content = content.push(text(label).size(size.text_size()).font(semibold()));
    if let Some(k) = key {
        content = content.push(if kind == ButtonKind::Primary {
            key_chip_on_primary(k)
        } else {
            key_chip(k)
        });
    }
    let full = size == ButtonSize::Lg || key.is_some();
    let mut b = button(
        container(content)
            .center_x(Length::Fill)
            .center_y(Length::Fill),
    )
    .height(Length::Fixed(size.height()))
    .padding([0.0, size.padding_x()])
    .style(move |t, s| button_style(t, s, kind));
    if full {
        b = b.width(Length::Fill);
    }
    if let Some(m) = msg {
        b = b.on_press(m);
    }
    b.into()
}

pub(crate) fn welcome_permission_guidance(state: WelcomeState) -> (&'static str, &'static str) {
    match state {
        WelcomeState::Denied => (
            "Permission still blocked",
            "Switch Readshot on in System Settings, then restart Readshot. If it is already on, the grant may belong to an earlier version — use Reset Permission and switch it on again.",
        ),
        _ => (
            "Waiting for permission",
            "Enable Readshot in System Settings. If macOS offers Quit & Reopen, accept it; otherwise use Restart Readshot after toggling the permission on.",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::ocr_language_display_name;
    use super::{
        highlight_spans, history_age_label, history_first_line, history_selected_border,
        history_selected_fill, history_word_count,
    };
    use crate::runtime::{DARK, LIGHT};

    #[test]
    fn no_hardcoded_white_text_left_in_themed_views() {
        let src = include_str!("view.rs");
        let needle = concat!(".color(Color::", "WHITE)"); // split so this test doesn't match itself
        let count = src.matches(needle).count();
        assert_eq!(count, 0, "use text_t1/text_t2/... so light mode works");
    }

    #[test]
    fn every_tool_tooltip_names_a_key() {
        for t in readshot_ui::editor::toolbar::TOOL_ORDER {
            let (name, key) = super::tool_label_and_key(t);
            assert!(!name.is_empty() && key.len() == 1, "{t:?}");
        }
    }

    #[test]
    fn pin_controls_compact_below_230() {
        assert!(super::pin_controls_compact(229.0));
        assert!(!super::pin_controls_compact(230.0));
    }

    #[test]
    fn highlight_spans_marks_case_insensitive_matches() {
        let spans = highlight_spans("Quarterly overview", "quarter");
        assert_eq!(
            spans,
            vec![
                ("Quarter".to_string(), true),
                ("ly overview".to_string(), false)
            ]
        );
        assert_eq!(highlight_spans("abc", ""), vec![("abc".to_string(), false)]);
        assert_eq!(
            highlight_spans("a-a", "a"),
            vec![("a".into(), true), ("-".into(), false), ("a".into(), true)]
        );
        // Lowercasing changes byte lengths here; must not panic.
        assert_eq!(
            highlight_spans("\u{212A}İİ", "i"),
            vec![("\u{212A}İİ".to_string(), false)]
        );
    }

    #[test]
    fn history_age_label_is_compact() {
        use chrono::TimeZone;
        let now = chrono::Local
            .with_ymd_and_hms(2026, 9, 27, 14, 0, 0)
            .unwrap();
        let ago = |mins: i64| now - chrono::Duration::minutes(mins);
        assert_eq!(history_age_label(ago(0), now), "now");
        assert_eq!(history_age_label(ago(2), now), "2m");
        assert_eq!(history_age_label(ago(90), now), "1h");
        assert_eq!(history_age_label(ago(60 * 20), now), "Yesterday");
        assert_eq!(history_age_label(ago(60 * 24 * 5), now), "Sep 22");
        let old = chrono::Local
            .with_ymd_and_hms(2025, 12, 31, 9, 0, 0)
            .unwrap();
        assert_eq!(history_age_label(old, now), "Dec 31, 2025");
    }

    #[test]
    fn history_first_line_skips_blank_lines() {
        assert_eq!(
            history_first_line(Some("\n  \n  Quarterly overview \nmore")),
            Some("Quarterly overview")
        );
        assert_eq!(history_first_line(Some("   \n ")), None);
        assert_eq!(history_first_line(None), None);
    }

    #[test]
    fn history_word_count_counts_whitespace_separated_words() {
        assert_eq!(
            history_word_count(Some("Revenue $482k\n· Active  users")),
            5
        );
        assert_eq!(history_word_count(None), 0);
    }

    #[test]
    fn history_selection_colors_read_tokens() {
        assert_eq!(history_selected_fill(&DARK), DARK.acc_tint);
        assert_eq!(history_selected_fill(&LIGHT), LIGHT.acc_tint);
        let b = history_selected_border(&DARK);
        assert_eq!((b.r, b.g, b.b), (DARK.acc.r, DARK.acc.g, DARK.acc.b));
        assert!((b.a - 0.35).abs() < 1e-6);
    }

    #[test]
    fn ocr_language_display_name_maps_known_codes() {
        assert_eq!(ocr_language_display_name("en-US"), "English (US)");
        assert_eq!(ocr_language_display_name("zh-Hans"), "Chinese (Simplified)");
        assert_eq!(
            ocr_language_display_name("zh-Hant"),
            "Chinese (Traditional)"
        );
        assert_eq!(ocr_language_display_name("pt-BR"), "Portuguese (Brazil)");
        assert_eq!(ocr_language_display_name("nb-NO"), "Norwegian Bokmål");
        assert_eq!(ocr_language_display_name("vi-VN"), "Vietnamese");
        assert_eq!(ocr_language_display_name("vi-VT"), "Vietnamese");
    }

    #[test]
    fn ocr_language_display_name_falls_back_to_raw_code() {
        // An unrecognised / future OS code renders verbatim, never blank.
        assert_eq!(ocr_language_display_name("xx-YY"), "xx-YY");
        assert_eq!(ocr_language_display_name(""), "");
    }

    #[test]
    fn toast_actions_name_their_key() {
        use crate::editor::EditorToastAction as A;
        let (label, key, msg) = super::toast_action_parts(A::UndoRedaction { undo_depth: 0 });
        assert_eq!((label, key), ("Undo", Some("⌘Z")));
        assert!(matches!(
            msg,
            crate::app::Message::EditorToolbar(readshot_ui::ToolbarMessage::Undo)
        ));
        let (label, _, msg) = super::toast_action_parts(A::RetryRecognition);
        assert_eq!(label, "Retry");
        assert!(matches!(msg, crate::app::Message::EditorTextRecognize));
    }
}

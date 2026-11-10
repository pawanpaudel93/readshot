// Extracted from runtime.rs (pure code-move). `use super::*` pulls in
// sibling/parent items; the explicit imports mirror runtime.rs's preamble.
use super::*;

use std::path::{Path, PathBuf};

use iced::widget::{button, column, container, responsive, row, scrollable, text, Space};
use iced::window;
use iced::{Alignment, Color, Element, Length, Theme};

use readshot_ui::SettingsMessage;

use crate::app::{App, Message, SettingsPane};
use crate::permissions::PermissionStatus;
use crate::welcome::WelcomeState;

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
            let label = size_px(phys_w, phys_h);
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
            frame.fill(
                &badge_path,
                Color {
                    a: 0.90,
                    ..DARK.float
                },
            );
            frame.fill_text(CanvasText {
                content: label,
                position: iced::Point::new(bx + 8.0, by + 4.0),
                color: DARK.t1,
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
        format!("Discard {}", counted(frame_count, "Frame", "Frames"))
    }
}

/// The armed footer's "go on" button: "Keep Going" (semibold 13 px,
/// ≈ 75 px) overflows half the 240 px footer's ≈ 70 px, so the compact
/// window says "Resume".
pub(crate) fn scroll_keep_going_label(window_w: f32) -> &'static str {
    if scroll_hud_compact(window_w) {
        "Resume"
    } else {
        "Keep Going"
    }
}

/// The armed Cancel caption. It stays until answered, so it names both
/// answers and no time limit.
pub(crate) fn scroll_discard_prompt(frame_count: usize) -> String {
    format!(
        "Press Discard or {} to throw away {}, or keep going.",
        SHIFT_ESC_KEY,
        counted(frame_count, "frame", "frames")
    )
}

/// Key chip on the Stop & Stitch button. Hidden while Waiting (the
/// button is disabled and esc cancels there) and in the compact
/// layout, where the label alone fits. Also hidden when esc couldn't be
/// registered as a global key (`esc_global` false): it would only work
/// once the HUD has focus, while the user scrolls another app.
pub(crate) fn scroll_stop_key_chip(
    hud: ScrollHudState,
    compact: bool,
    esc_global: bool,
) -> Option<&'static str> {
    (hud == ScrollHudState::Recording && !compact && esc_global).then_some(ESC_KEY)
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
/// static "Stitching N frames" pill (no spinner animation, so nothing to
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
            text(format!(
                "Stitching {}",
                counted(frame_count, "frame", "frames")
            ))
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

    let header = scroll_hud_header(state, session, hud, compact);
    let stage = scroll_hud_stage(session, hud, frame_count);
    let footer = scroll_hud_footer(state, session, hud, compact, window_w, frame_count);

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

/// The scrolling-capture window's header: dot, title, elapsed time and
/// Cancel. It doubles as the window's drag handle.
fn scroll_hud_header<'a>(
    state: &'a App,
    session: &'a crate::app::ScrollSession,
    hud: ScrollHudState,
    compact: bool,
) -> Element<'a, Message> {
    use iced::widget::mouse_area;
    use iced::widget::tooltip::Position;

    let stopping = session.stopping;
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
            crate::icons::action::DISMISS,
            DARK.t2,
            "Cancel",
            Some(SHIFT_ESC_KEY),
            (!stopping).then_some(Message::Scroll(ScrollMsg::CancelRequested)),
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
    .on_press(Message::Scroll(ScrollMsg::HudDragRequested))
    .interaction(iced::mouse::Interaction::Grab);
    header.into()
}

/// The scrolling-capture window's stage: the stitched page while
/// recording, zoomed to fit while stitching, or the empty state.
fn scroll_hud_stage(
    session: &crate::app::ScrollSession,
    hud: ScrollHudState,
    frame_count: usize,
) -> Element<'_, Message> {
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
            "Each new part of the page appears here, stitched to the last.",
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
    stage.into()
}

/// The scrolling-capture window's footer: frame count, output size, the
/// limit bar and the buttons for the state.
fn scroll_hud_footer<'a>(
    state: &'a App,
    session: &'a crate::app::ScrollSession,
    hud: ScrollHudState,
    compact: bool,
    window_w: f32,
    frame_count: usize,
) -> Element<'a, Message> {
    use iced::widget::tooltip::Position;

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
            "{}\u{a0}×\u{a0}{}\u{a0}px",
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
                text(scroll_discard_prompt(frame_count))
                    .size(TYPE_CAPTION)
                    .color(DARK.danger)
                    .wrapping(iced::widget::text::Wrapping::Word),
            );
            footer = footer.push(Space::new().height(Length::Fixed(10.0)));
            row![
                scroll_hud_button(
                    scroll_discard_label(window_w, frame_count),
                    ButtonKind::DangerSolid,
                    Some(Message::Scroll(ScrollMsg::CancelRequested)),
                    None,
                    1,
                ),
                scroll_hud_button(
                    scroll_keep_going_label(window_w).to_string(),
                    ButtonKind::Primary,
                    Some(Message::Scroll(ScrollMsg::KeepGoing)),
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
            if scroll_too_fast(session) {
                footer = footer.push(
                    text(SCROLL_TOO_FAST_HINT)
                        .size(TYPE_CAPTION)
                        .color(DARK.warn)
                        .wrapping(iced::widget::text::Wrapping::Word),
                );
                footer = footer.push(Space::new().height(Length::Fixed(10.0)));
            }
            row![
                tooltip_with_key(
                    scroll_hud_button(
                        "Cancel".to_string(),
                        ButtonKind::Secondary,
                        Some(Message::Scroll(ScrollMsg::CancelRequested)),
                        None,
                        2,
                    ),
                    "Cancel and Discard",
                    Some(SHIFT_ESC_KEY),
                    Position::Top,
                ),
                scroll_hud_button(
                    "Stop & Stitch".to_string(),
                    ButtonKind::Primary,
                    (!waiting).then_some(Message::Scroll(ScrollMsg::StopRequested)),
                    scroll_stop_key_chip(hud, compact, scroll_escape_globals(state).0),
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
    footer.into()
}

/// History layout: list column width at full size, and the
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

/// Widget id of the History search field, focused when the window opens.
pub(crate) const HISTORY_SEARCH_ID: &str = "history-search";
/// The idle Clear All button: outlined danger, like Settings' Reset and
/// Uninstall (`ActionKind::DangerQuiet`), so every arming step of a
/// destructive action reads the same.
pub(crate) const HISTORY_CLEAR_ALL_KIND: ButtonKind = ButtonKind::Danger;

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

/// How many characters of a list title fit beside the thumbnail and the
/// age in a `list_w` list: the list and row padding (2 × 8 each), the
/// thumbnail, two 12 px gaps and ≈ 64 px of age, at ≈ 7.5 px per 13 px
/// character.
pub(crate) fn history_title_budget(list_w: f32) -> usize {
    let room = list_w - 4.0 * SP2 - HISTORY_THUMB_W - 2.0 * SP3 - 64.0;
    (room / 7.5).floor().max(4.0) as usize
}

/// `line` cut to `budget` characters with a trailing "…" when it is
/// longer, so a cut title shows that it continues.
pub(crate) fn ellipsize(line: &str, budget: usize) -> std::borrow::Cow<'_, str> {
    if line.chars().count() <= budget {
        return std::borrow::Cow::Borrowed(line);
    }
    let kept: String = line.chars().take(budget.saturating_sub(1)).collect();
    std::borrow::Cow::Owned(format!("{}…", kept.trim_end()))
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

pub(crate) fn semibold() -> iced::Font {
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
    // History image handles asked for in this build stay cached for it.
    super::history_images::begin_history_view_build();

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

    let title_row = history_title_row(state, p, total, shown, visible_text_count);
    let body = history_body(state, p, visible, total);

    // An armed Clear All states its consequence before the second press.
    let mut page = column![title_row, hairline()];
    if state.history_prompt.is_clear_all() {
        page = page.push(
            container(toast(ToastKind::Warn, history_clear_warning(total), None))
                .width(Length::Fill)
                .padding([SP2, SP4]),
        );
    }
    container(page.push(body))
        .width(Length::Fill)
        .height(Length::Fill)
        .style(surface_style)
        .into()
}

/// The History title row: heading, search with its count, Copy Visible
/// Text and Clear All. It is also the window's drag handle.
fn history_title_row<'a>(
    state: &'a App,
    p: &'static Palette,
    total: usize,
    shown: usize,
    visible_text_count: usize,
) -> Element<'a, Message> {
    use iced::widget::tooltip::Position;
    use iced::widget::{stack, text_input};

    // --- Title row: heading · search · Copy Visible Text · Clear All ---
    let count_caption = if total == 0 {
        String::new()
    } else {
        format!("{shown} of {total}")
    };
    // Room on the right of the input for the overlaid count caption. It is
    // set in the mono face, so its width is exact: no digit overlaps
    // typed text.
    let caption_room = history_count_room(&count_caption);
    let search_input = text_input("Search text", &state.history_search)
        .id(iced::widget::Id::new(HISTORY_SEARCH_ID))
        .on_input(|a| Message::History(HistoryMsg::SearchChanged(a)))
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
        text(count_caption)
            .font(mono_font())
            .size(TYPE_CAPTION)
            .style(text_t3),
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
            (visible_text_count > 0)
                .then_some(Message::History(HistoryMsg::CopyVisibleTextRequested)),
        ),
        if visible_text_count == 1 {
            "Copy Text from 1 Visible Capture".to_string()
        } else {
            format!("Copy Text from {visible_text_count} Visible Captures")
        },
        Some(HISTORY_COPY_VISIBLE_KEY),
        Position::Bottom,
    );
    // Two-stage Clear All: the idle (outlined danger) button arms; armed
    // shows a solid danger confirm plus Cancel.
    let clear_controls: Element<'_, Message> = if state.history_prompt.is_clear_all() {
        row![
            tooltip_with_key(
                bar_button(
                    text(history_clear_confirm_label(total))
                        .size(ButtonSize::Sm.text_size())
                        .into(),
                    ButtonKind::DangerSolid,
                    ButtonSize::Sm,
                    Some(Message::History(HistoryMsg::ClearAllConfirmed)),
                ),
                "Delete All Permanently",
                Some(HISTORY_CLEAR_ALL_KEY),
                Position::Bottom,
            ),
            tooltip_with_key(
                bar_button(
                    text("Cancel").size(ButtonSize::Sm.text_size()).into(),
                    ButtonKind::Ghost,
                    ButtonSize::Sm,
                    Some(Message::History(HistoryMsg::ClearAllCancelled)),
                ),
                "Cancel",
                Some(ESC_KEY),
                Position::Bottom,
            ),
        ]
        .spacing(SP1)
        .align_y(Alignment::Center)
        .into()
    } else {
        tooltip_with_key(
            bar_button(
                text("Clear All").size(ButtonSize::Sm.text_size()).into(),
                HISTORY_CLEAR_ALL_KIND,
                ButtonSize::Sm,
                (total > 0).then_some(Message::History(HistoryMsg::ClearAllRequested)),
            ),
            "Clear All",
            Some(HISTORY_CLEAR_ALL_KEY),
            Position::Bottom,
        )
    };
    // While Clear All asks, Copy Visible Text steps aside so the search
    // field keeps its room at the 540 px minimum.
    // Off macOS the native title bar already says "History".
    let mut title_content = row![]
        .push(DRAWS_OWN_TITLE_BAR.then(|| {
            text("History")
                .size(TYPE_HEADING)
                .font(semibold())
                .style(text_t1)
        }))
        .push(container(search).center_x(Length::Fill));
    if !state.history_prompt.is_clear_all() {
        title_content = title_content.push(copy_visible);
    }
    let title_content = title_content
        .push(clear_controls)
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
    // background is the window's drag handle. It holds the search and
    // Clear controls, so it stays on every platform.
    let title_row = title_drag_row(title_content, state.history_window_id);
    title_row
}

/// The History body: the list and detail split, or the empty state that
/// applies (no History folder, no captures, no matches) with the status
/// toast under it.
fn history_body<'a>(
    state: &'a App,
    p: &'static Palette,
    visible: Vec<&'a readshot_core::CaptureRecord>,
    total: usize,
) -> Element<'a, Message> {
    let shown = visible.len();
    // --- Body ---
    let body: Element<'_, Message> = if state.history_root.is_none() {
        empty_state(
            crate::icons::AppIcon::Alert,
            p,
            "History unavailable",
            "Readshot couldn’t find a folder to keep history in. Quit and reopen Readshot to try again.",
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
                        Some(Message::Capture(CaptureMsg::OpenOverlayRequested)),
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
                        Some(Message::History(HistoryMsg::SearchChanged(String::new()))),
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
    body
}

/// The search field's right padding that keeps typed text clear of the
/// overlaid "N of M" count: the count's exact mono width (0.6 em per
/// glyph), the decor row's 10 px inset and a 6 px gap.
pub(crate) fn history_count_room(caption: &str) -> f32 {
    caption.chars().count() as f32 * TYPE_CAPTION * MONO_ADVANCE + 16.0
}

/// The armed Clear All button: what it deletes, counted.
pub(crate) fn history_clear_confirm_label(total: usize) -> String {
    if total == 1 {
        "Delete 1 Capture".to_string()
    } else {
        format!("Delete {total} Captures")
    }
}

/// The line shown while Clear All waits for its second press.
pub(crate) fn history_clear_warning(total: usize) -> String {
    let what = if total == 1 {
        "1 capture".to_string()
    } else {
        format!("all {total} captures")
    };
    format!(
        "This deletes {what}, images and text, permanently. Press the button again to confirm, or {ESC_KEY} to cancel."
    )
}

/// The caption beside an armed single delete.
pub(crate) const HISTORY_DELETE_WARNING: &str = "This deletes the image and its text permanently.";

/// List | detail split. The list keeps 340 px until the window is too
/// narrow for a usable detail pane, then shrinks.
fn history_status_toast(state: &App, p: &'static Palette) -> Option<Element<'static, Message>> {
    let status = state.history_status.as_ref()?;
    let kind = status_toast_kind(status.tone);
    // Errors and warnings stay until dismissed (History has no
    // auto-expiry).
    let action = toast_is_dismissable(kind).then(|| {
        icon_button(
            crate::icons::action::DISMISS,
            p.t2,
            "Dismiss",
            None,
            Some(Message::History(HistoryMsg::StatusDismissed)),
            false,
            ICON_BTN_XS,
            iced::widget::tooltip::Position::Top,
        )
    });
    Some(toast(kind, status.text.clone(), action))
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
        let shown = history_thumbnail_path(root, r)
            .filter(|p| p.exists())
            .or_else(|| history_png_path(root, r).filter(|p| p.exists()));
        let thumb: Element<'_, Message> = if let Some(path) = shown {
            container(
                image_widget(super::history_images::history_image_handle(&path))
                    .width(Length::Fixed(HISTORY_THUMB_W - 2.0))
                    .height(Length::Fixed(HISTORY_THUMB_H - 2.0))
                    .content_fit(iced::ContentFit::Cover)
                    .border_radius(R_CHIP - 1.0),
            )
            .padding(1)
            .style(|theme| capture_outline_style(theme, R_CHIP))
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
            Some(line) => highlighted_text(
                &ellipsize(line, history_title_budget(list_w)),
                query,
                p,
                p.t1,
                TYPE_BODY,
            )
            .font(iced::Font {
                weight: iced::font::Weight::Medium,
                ..iced::Font::DEFAULT
            })
            .wrapping(text::Wrapping::None)
            .into(),
            None => text(if super::history_ops::history_text_hidden(r) {
                super::history_ops::HIDDEN_TEXT_LIST_LABEL
            } else {
                "No text yet"
            })
            .size(TYPE_BODY)
            .style(text_t3)
            .wrapping(text::Wrapping::None)
            .into(),
        };
        let sub = text(size_px(r.width_px, r.height_px))
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
                    width: if is_selected { SELECTED_OUTLINE } else { 0.0 },
                    radius: R_BUTTON.into(),
                },
                ..container::Style::default()
            }
        });
        list = list.push(
            mouse_area(row_widget)
                .on_press(Message::History(HistoryMsg::Select(r.id)))
                .on_double_click(Message::History(HistoryMsg::OpenInEditor(r.id)))
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
                Some(Message::History(HistoryMsg::ShowMore)),
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
            legend_item(RETURN_KEY, "Open"),
            legend_item(HISTORY_DELETE_KEY, "Delete"),
            legend_item(HISTORY_SEARCH_KEY, "Search"),
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
    let delete_pending =
        selected.is_some_and(|r| state.history_prompt.delete_target() == Some(r.id));
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

    let preview = history_selected_preview(record, root, p);
    let ocr = record
        .ocr_text
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty());
    let (heading, text_box) = history_selected_text(record, ocr, query, p);
    let (delete_confirm, actions) =
        history_selected_actions(id, p, delete_pending, stack_actions, ocr.is_some());

    let mut panel = column![preview, heading, text_box];
    if let Some(confirm) = delete_confirm {
        panel = panel.push(confirm);
    }
    panel
        .push(actions)
        .spacing(SP3)
        .padding(SP4)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

/// The detail pane's preview: the capture fitted to the pane, or why
/// there is none to show.
fn history_selected_preview<'a>(
    record: &'a readshot_core::CaptureRecord,
    root: &Path,
    p: &'static Palette,
) -> Element<'a, Message> {
    use crate::icons::AppIcon;
    use iced::widget::image as image_widget;

    let shown = history_png_path(root, record)
        .filter(|p| p.exists())
        .or_else(|| history_thumbnail_path(root, record).filter(|p| p.exists()));
    let preview: Element<'a, Message> = if let Some(path) = shown {
        // Scale the capture to fill the pane (up or down, aspect kept) as
        // `ContentFit::Contain` did, but size the image box to the fitted
        // size so the 1 px outline hugs the capture, not the letterbox.
        let handle = super::history_images::history_image_handle(&path);
        let natural = (record.width_px as f32, record.height_px as f32);
        container(responsive(move |size| {
            let (w, h) = contain_fit(natural, (size.width - 2.0, size.height - 2.0));
            container(
                container(
                    image_widget(handle.clone())
                        .width(Length::Fixed(w))
                        .height(Length::Fixed(h))
                        .content_fit(iced::ContentFit::Contain),
                )
                .padding(1)
                .style(|theme| capture_outline_style(theme, 0.0)),
            )
            .center_x(Length::Fill)
            .center_y(Length::Fill)
            .into()
        }))
        .padding(SP2)
        .into()
    } else if record.scrub_pending {
        empty_state(
            AppIcon::Image,
            p,
            "Redaction being saved",
            "The image stays hidden until its redacted version is saved.",
            None,
        )
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
    preview.into()
}

/// The detail pane's recognized-text heading (time stamp, word count)
/// and its text box, with `query` highlighted.
fn history_selected_text<'a>(
    record: &'a readshot_core::CaptureRecord,
    ocr: Option<&'a str>,
    query: &str,
    p: &'static Palette,
) -> (Element<'a, Message>, Element<'a, Message>) {
    let stamp = history_timestamp_label(
        record.captured_at.with_timezone(&chrono::Local),
        chrono::Local::now(),
    );
    let words = history_word_count(ocr);
    let heading = row![
        text("Recognized text")
            .size(TYPE_BODY)
            .font(semibold())
            .style(text_t1),
        Space::new().width(Length::Fill),
        text(format!("{stamp} · {}", counted(words, "word", "words")))
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
            text(if super::history_ops::history_text_hidden(record) {
                super::history_ops::HIDDEN_TEXT_NOTE
            } else {
                "No text recognized."
            })
            .size(TYPE_CAPTION)
            .style(text_t2),
        )
        .padding([10, 12])
        .into(),
    };
    let text_box = container(text_body)
        .width(Length::Fill)
        .height(Length::FillPortion(4))
        .style(history_selected_panel_style);
    (heading.into(), text_box.into())
}

/// The detail pane's actions for capture `id`, and the armed delete's
/// confirmation row while a delete is armed.
fn history_selected_actions<'a>(
    id: readshot_core::Uuid,
    p: &'static Palette,
    delete_pending: bool,
    stack_actions: bool,
    has_text: bool,
) -> (Option<Element<'a, Message>>, Element<'a, Message>) {
    use crate::icons::AppIcon;
    use iced::widget::tooltip::Position;

    // Two-stage delete, like Clear All: the icon arms; armed shows its
    // consequence and a solid "Delete Capture" plus Cancel on a row of
    // their own, so nothing is clipped at the minimum width.
    let delete_confirm: Option<Element<'a, Message>> = delete_pending.then(|| {
        column![
            text(HISTORY_DELETE_WARNING)
                .size(TYPE_CAPTION)
                .style(text_danger)
                .wrapping(text::Wrapping::Word),
            row![
                Space::new().width(Length::Fill),
                bar_button(
                    text("Delete Capture")
                        .size(ButtonSize::Sm.text_size())
                        .into(),
                    ButtonKind::DangerSolid,
                    ButtonSize::Sm,
                    Some(Message::History(HistoryMsg::DeleteConfirmed(id))),
                ),
                bar_button(
                    text("Cancel").size(ButtonSize::Sm.text_size()).into(),
                    ButtonKind::Ghost,
                    ButtonSize::Sm,
                    Some(Message::History(HistoryMsg::DeleteCancelled)),
                ),
            ]
            .spacing(SP1)
            .align_y(Alignment::Center),
        ]
        .spacing(SP2)
        .into()
    });
    let delete_control: Element<'a, Message> = icon_button(
        crate::icons::action::DELETE,
        p.t2,
        "Delete Capture",
        Some(HISTORY_DELETE_KEY),
        (!delete_pending).then_some(Message::History(HistoryMsg::Delete(id))),
        delete_pending,
        ICON_BTN,
        Position::Top,
    );
    let left_actions = row![
        delete_control,
        icon_button(
            AppIcon::Folder,
            p.t2,
            HISTORY_REVEAL_LABEL,
            Some(HISTORY_REVEAL_KEY),
            Some(Message::History(HistoryMsg::Reveal(id))),
            false,
            ICON_BTN,
            Position::Top,
        ),
        // The same pencil as the capture overlay's Edit, which opens the
        // same editor (the external-link glyph implied leaving the app).
        icon_button(
            crate::icons::action::OPEN_IN_EDITOR,
            p.t2,
            "Open in Editor",
            Some(RETURN_KEY),
            Some(Message::History(HistoryMsg::OpenInEditor(id))),
            false,
            ICON_BTN,
            Position::Top,
        ),
    ]
    .spacing(2)
    .align_y(Alignment::Center);
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
                crate::icons::icon(crate::icons::action::COPY_TEXT, text_fg, ICON_SIZE),
                text("Copy Text").size(ButtonSize::Md.text_size()),
            ]
            .spacing(6)
            .align_y(Alignment::Center)
            .into(),
            ButtonKind::Secondary,
            ButtonSize::Md,
            has_text.then_some(Message::History(HistoryMsg::CopyText(id))),
        ),
        "Copy Text",
        Some(COPY_TEXT_KEY),
        Position::Top,
    );
    let copy_image = tooltip_with_key(
        bar_button(
            row![
                crate::icons::icon(crate::icons::action::COPY_IMAGE, p.pri_fg, ICON_SIZE),
                text("Copy Image").size(ButtonSize::Md.text_size()),
            ]
            .spacing(6)
            .align_y(Alignment::Center)
            .into(),
            ButtonKind::Primary,
            ButtonSize::Md,
            Some(Message::History(HistoryMsg::CopyImage(id))),
        ),
        "Copy Image",
        Some(COPY_IMAGE_KEY),
        Position::Top,
    );
    let pin = icon_button(
        crate::icons::action::PIN,
        p.t2,
        "Pin",
        Some(PIN_KEY),
        Some(Message::History(HistoryMsg::Pin(id))),
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
    (delete_confirm, actions)
}

/// Selected list row: accent tint fill with a 35 % accent hairline.
/// The size `natural` takes when scaled (up or down, aspect kept) to fit
/// inside `area`; the whole area when either is empty.
pub(crate) fn contain_fit(natural: (f32, f32), area: (f32, f32)) -> (f32, f32) {
    let (aw, ah) = (area.0.max(0.0), area.1.max(0.0));
    let (nw, nh) = natural;
    if nw <= 0.0 || nh <= 0.0 || aw <= 0.0 || ah <= 0.0 {
        return (aw, ah);
    }
    let k = (aw / nw).min(ah / nh);
    (nw * k, nh * k)
}

/// A 1 px `border` outline around a capture image, so a white
/// screenshot does not dissolve into a white surface in Light mode.
pub(crate) fn capture_outline_style(theme: &Theme, radius: f32) -> iced::widget::container::Style {
    iced::widget::container::Style {
        border: iced::Border {
            color: palette_for(theme).border,
            width: 1.0,
            radius: radius.into(),
        },
        ..iced::widget::container::Style::default()
    }
}

pub(crate) fn history_selected_fill(p: &Palette) -> Color {
    p.acc_tint
}

/// The selected row's outline, drawn `SELECTED_OUTLINE` wide: the
/// `acc_text` selection cue (≥ 3:1 on the list in both themes).
pub(crate) fn history_selected_border(p: &Palette) -> Color {
    p.acc_text
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

pub(crate) fn editor_view(state: &App) -> Element<'_, Message> {
    use crate::icons::AppIcon;

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
                    Some(Message::Capture(CaptureMsg::OpenOverlayRequested)),
                )
                .into(),
            ),
        ))
        .width(Length::Fill)
        .height(Length::Fill)
        .style(bg_style)
        .into();
    };

    // ===== Chrome — floating bars over the stage =====
    // Title row (zoom), tool bar (tools · color · width), hint, action
    // bar (history · frame · outputs) and at most one popover all float
    // over the image as `stack!` layers. Color, width and frame live in
    // popovers, so both bars stay one row down to the 720 px minimum.
    let p = palette(state.theme_mode);
    let mode = editor_panel_mode(state, ed);
    let mut layers: Vec<Element<'_, Message>> = vec![
        editor_stage(state, ed),
        editor_title_layer(ed, p, mode),
        editor_tool_layer(state, ed, p),
        editor_options_layer(ed, p),
    ];
    if let Some(toast_layer) = editor_toast_layer(ed, p) {
        layers.push(toast_layer);
    }
    layers.push(editor_action_layer(ed, p));

    // --- Popover: a click-outside scrim, then the card ---
    if let Some(which) = ed.popover {
        editor_popover_layers(state, ed, p, which, &mut layers);
    }
    editor_with_panel(state, ed, p, mode, layers)
}

/// A disabled control's color: `c` at 45 % of its alpha.
fn dimmed(c: Color) -> Color {
    Color { a: c.a * 0.45, ..c }
}

/// The floating tool bar: every tool, in its groups.
fn editor_tool_layer<'a>(
    state: &'a App,
    ed: &'a EditorSession,
    p: &'static Palette,
) -> Element<'a, Message> {
    use crate::icons::AppIcon;
    use iced::widget::tooltip::Position;

    let active_tool = ed.model.active_tool();
    let busy = ed.busy;
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
            let btn = icon_button_hinted(
                AppIcon::Tool(t),
                color,
                label,
                Some(key),
                Some(editor_compact_hint(t)),
                (!busy).then_some(Message::Editor(EditorMsg::Toolbar(
                    readshot_ui::ToolbarMessage::SelectTool(t),
                ))),
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
    tool_layer.into()
}

/// What the stage's canvas takes from this build of the view (computed
/// once, not on every layout of the stage).
struct StageCanvasInputs {
    pending: Vec<readshot_core::Annotation>,
    bubble_covers: Vec<readshot_core::RectLike>,
    selected_preview: Option<readshot_core::Annotation>,
    selected_preview_rendered: bool,
    shift_held: bool,
    text_hover: Option<readshot_core::RectLike>,
    text_selected: Vec<readshot_core::RectLike>,
    part_rects: Vec<readshot_core::RectLike>,
}

/// The stage: the letterboxed image with the annotation canvas over it,
/// framed by Beautify, scrolled when it overflows, and inset clear of
/// the floating bars.
fn editor_stage<'a>(state: &'a App, ed: &'a EditorSession) -> Element<'a, Message> {
    // ===== Image area — letterboxed image with canvas overlay =====
    // Both layers share the same container; the canvas Program knows
    // the image's effective dimensions + crop offset so cursor maps
    // to base-image coordinates regardless of zoom / letterbox /
    // crop. See `canvas_to_base` in readshot-ui.
    let image_handle = ed.image_handle.clone();
    let (image_w, image_h) = ed.effective_image_size();
    let image_offset = ed.crop_offset();
    // A render still in flight leaves the previous one on screen: where
    // it sits in the present frame (same document, maybe another crop).
    let shown_part = shown_part_of_frame(ed, image_offset, (image_w, image_h));
    // Edits not yet in that render: the canvas draws them over it.
    let pending = ed.pending_overlay();
    // Bubbles that may magnify what such an edit hides: covered.
    let bubble_covers = ed.magnifier_bubble_covers();
    let beautify = ed.model.beautify();
    let zoom = ed.zoom;
    let display_scale = ed.display_scale;
    let editor_shift_held = state.editor_shift_held;
    let selected_preview = ed
        .move_drag
        .as_ref()
        .filter(|drag| {
            drag.moved && can_preview_drag_annotation(&drag.baseline, drag.selected_index)
        })
        .and_then(|drag| ed.model.annotations().get(drag.selected_index).cloned());
    let selected_preview_rendered = selected_preview.is_some() && ed.shows_present_render();
    // Interior-mutable handle so the responsive closure can record the
    // scale it renders at; the runtime reads it back to hit-test grabs.
    let render_scale = ed.render_scale.clone();
    // Marks only while the panel is on screen (docked or overlay).
    let panel_visible = matches!(
        editor_panel_mode(state, ed),
        PanelMode::Docked | PanelMode::Overlay
    );
    let (text_hover, text_selected) = text_marks(ed, panel_visible);
    let stage_offset = ed.stage_offset.clone();
    let stage_size = ed.stage_size.clone();
    // A new canvas program, in the present frame.
    ed.note_canvas_built();
    // Combined images (document px): a new magnifier bubble avoids the
    // gutters between them.
    let part_rects: Vec<readshot_core::RectLike> = {
        let g = ed.model.geometry();
        if g.parts.len() > 1 {
            g.parts.iter().filter_map(|p| g.part_rect(p.id)).collect()
        } else {
            Vec::new()
        }
    };
    let inputs = StageCanvasInputs {
        pending,
        bubble_covers,
        selected_preview,
        selected_preview_rendered,
        shift_held: editor_shift_held,
        text_hover,
        text_selected,
        part_rects,
    };
    let image_area_content = responsive(move |stage| {
        // The stage is already inset clear of the floating bars.
        let available = stage;
        stage_size.set((available.width, available.height));
        let iw = image_w as f32;
        let ih = image_h as f32;
        let m = preview_metrics(
            image_w,
            image_h,
            &beautify,
            available,
            zoom.explicit_scale(),
        );
        let scale = m.scale;
        // Record the scale this frame renders at so the runtime's press
        // handlers hit-test handles/bodies with a screen-constant radius.
        render_scale.set(scale);
        let displayed_w = (iw * scale).max(1.0);
        let displayed_h = (ih * scale).max(1.0);
        let framed = !beautify.is_identity();
        let (output_w, output_h) = (m.output_w, m.output_h);
        let content_w = output_w.max(available.width);
        let content_h = output_h.max(available.height);

        let filter = editor_image_filter(scale, display_scale);
        let needs_horizontal_scroll = output_w > available.width + 0.5;
        let needs_vertical_scroll = output_h > available.height + 0.5;
        if !(needs_horizontal_scroll || needs_vertical_scroll) {
            // No scrollable this frame: one shown later starts at 0, 0.
            stage_offset.set((0.0, 0.0));
        } else {
            // What this frame shows (a zoom or relayout without a scroll
            // event clamps it); `pin_stage_offset` holds the scrollable
            // to it.
            stage_offset.set(clamp_stage_offset(
                stage_offset.get(),
                (content_w, content_h),
                (available.width, available.height),
            ));
        }
        let viewport = stage_viewport_in_document(StageViewport {
            offset: stage_offset.get(),
            available: (available.width, available.height),
            content: (content_w, content_h),
            output: (output_w, output_h),
            pad: if framed {
                (m.pad.left, m.pad.top)
            } else {
                (0.0, 0.0)
            },
            scale,
            image_offset,
        });

        let image_layer = stage_image_layer(
            &image_handle,
            shown_part,
            scale,
            (displayed_w, displayed_h),
            filter,
        );
        let canvas_layer =
            stage_canvas_layer(ed, &inputs, scale, viewport, (displayed_w, displayed_h));
        let editable_layers = stage_editable_layers(
            ed,
            image_layer,
            canvas_layer,
            scale,
            (displayed_w, displayed_h),
        );

        let editable_stack: Element<'_, Message> = container(editable_layers)
            .width(Length::Fixed(displayed_w))
            .height(Length::Fixed(displayed_h))
            .clip(true)
            .style(move |_theme: &Theme| {
                if framed {
                    beautify_image_style(&beautify, m.out_scale)
                } else {
                    container::Style::default()
                }
            })
            .into();

        let preview_content: Element<'_, Message> = if framed {
            container(editable_stack)
                .padding(m.pad)
                .width(Length::Fixed(output_w))
                .height(Length::Fixed(output_h))
                .style(move |_theme: &Theme| beautify_canvas_style(&beautify, m.out_scale))
                .into()
        } else {
            editable_stack
        };

        let content = container(preview_content)
            .width(Length::Fixed(content_w))
            .height(Length::Fixed(content_h))
            .center_x(Length::Fill)
            .center_y(Length::Fill);

        stage_scroll_layer(content, needs_horizontal_scroll, needs_vertical_scroll)
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
    stage.into()
}

/// The image on the stage: the present render, or the visible part of
/// the previous one while a render is in flight.
fn stage_image_layer<'a>(
    image_handle: &iced::widget::image::Handle,
    shown_part: Option<Option<ShownPart>>,
    scale: f32,
    (displayed_w, displayed_h): (f32, f32),
    filter: iced::widget::image::FilterMethod,
) -> Element<'a, Message> {
    let image: Element<'_, Message> = match shown_part {
        None => iced::widget::image(image_handle.clone())
            .width(Length::Fixed(displayed_w))
            .height(Length::Fixed(displayed_h))
            .content_fit(iced::ContentFit::Contain)
            .filter_method(filter)
            .into(),
        Some(None) => Space::new().into(),
        Some(Some(part)) => iced::widget::pin(
            iced::widget::image(image_handle.clone())
                .crop(part.region)
                .width(Length::Fixed(part.size.0 * scale))
                .height(Length::Fixed(part.size.1 * scale))
                .content_fit(iced::ContentFit::Fill)
                .filter_method(filter),
        )
        .x(part.at.0 * scale)
        .y(part.at.1 * scale)
        .width(Length::Fixed(displayed_w))
        .height(Length::Fixed(displayed_h))
        .into(),
    };
    let image_layer: Element<'_, Message> = container(image)
        .width(Length::Fixed(displayed_w))
        .height(Length::Fixed(displayed_h))
        .into();
    image_layer
}

/// The annotation canvas over the stage's image, at `scale`.
fn stage_canvas_layer<'a>(
    ed: &'a EditorSession,
    inputs: &StageCanvasInputs,
    scale: f32,
    viewport: Option<readshot_core::RectLike>,
    (displayed_w, displayed_h): (f32, f32),
) -> Element<'a, Message> {
    use iced::widget::canvas::Canvas;
    use readshot_ui::editor::canvas::EditorCanvas;

    let has_selected_annotation = ed.model.selected_annotation().is_some();
    let canvas_program = EditorCanvas {
        active_tool: ed.model.active_tool(),
        color: ed.model.current_color(),
        line_width: ed.model.current_line_width(),
        next_pin_number: ed.next_pin_number,
        image_size: ed.effective_image_size(),
        image_offset: ed.crop_offset(),
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
        selected_preview: inputs.selected_preview.clone(),
        selected_preview_rendered: inputs.selected_preview_rendered,
        pending: inputs.pending.clone(),
        bubble_covers: inputs.bubble_covers.clone(),
        shift_held: inputs.shift_held,
        cancel_seq: ed.drag_cancel_seq,
        style: ed.model.tool_style(),
        eyedropper: ed.eyedropper.is_some(),
        loupe: ed.eyedropper.as_ref().and_then(|e| e.loupe.clone()),
        text_hover: inputs.text_hover,
        text_selected: inputs.text_selected.clone(),
        // The old draft hides while a new drag replaces it.
        crop_draft: if ed.canvas_drag_active {
            None
        } else {
            ed.crop_draft
        },
        viewport,
        parts: inputs.part_rects.clone(),
        cache: ed.canvas_cache.clone(),
    };
    let canvas: Element<'_, readshot_ui::CanvasMessage> = Canvas::new(canvas_program)
        .width(Length::Fixed(displayed_w))
        .height(Length::Fixed(displayed_h))
        .into();
    let canvas: Element<'_, Message> = canvas.map(|a| Message::Editor(EditorMsg::Canvas(a)));
    let canvas_layer = container(canvas)
        .width(Length::Fixed(displayed_w))
        .height(Length::Fixed(displayed_h));
    canvas_layer.into()
}

/// The stage's image and canvas, with the inline text draft's editor
/// over them while one is open.
fn stage_editable_layers<'a>(
    ed: &'a EditorSession,
    image_layer: Element<'a, Message>,
    canvas_layer: Element<'a, Message>,
    scale: f32,
    (displayed_w, displayed_h): (f32, f32),
) -> Element<'a, Message> {
    use iced::widget::{stack, Space as IcedSpace};

    let (image_w, image_h) = ed.effective_image_size();
    let image_offset = ed.crop_offset();
    let busy = ed.busy;
    if let Some(pending) = ed.pending_text.clone() {
        let geom =
            inline_text_editor_geometry(pending.origin, (image_w, image_h), image_offset, scale);
        let input: Element<'_, Message> = if busy {
            let content = if pending.content.is_empty() {
                "Text draft paused while working…".to_string()
            } else {
                pending.content.clone()
            };
            container(
                text(content)
                    .size(TYPE_BODY)
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
                .placeholder(TEXT_PLACEHOLDER)
                .on_action(|a| Message::Editor(EditorMsg::TextAction(a)))
                .key_binding(editor_text_key_binding)
                .padding(8)
                .size(TYPE_BODY)
                .style(inline_text_editor_style)
                .into()
        } else {
            // `text_edit_content` is always `Some` while a draft is
            // open; this fallback only guards a torn state.
            container(
                text(pending.content.clone())
                    .size(TYPE_BODY)
                    .style(text_t1)
                    .wrapping(iced::widget::text::Wrapping::Word),
            )
            .padding(8)
            .width(Length::Fill)
            .into()
        };
        let commit_label = editor_text_commit_label(pending.edit_index.is_some());
        let mut commit = button(text(commit_label).size(TYPE_CAPTION))
            .padding([SP2, SP3])
            .style(|theme, status| action_button_style(theme, status, ActionKind::Primary));
        if !busy {
            commit = commit.on_press(Message::Editor(EditorMsg::TextCommit));
        }
        let mut cancel = button(text("Cancel").size(TYPE_CAPTION))
            .padding([SP2, SP3])
            .style(|theme, status| action_button_style(theme, status, ActionKind::Secondary));
        if !busy {
            cancel = cancel.on_press(Message::Editor(EditorMsg::TextCancel));
        }
        let editor = container(
            row![input, commit, cancel]
                .spacing(SP2)
                .align_y(Alignment::Center),
        )
        .padding(SP2)
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
    }
}

/// The stage's content in a scrollable when it overflows the stage.
fn stage_scroll_layer<'a>(
    content: iced::widget::Container<'a, Message>,
    needs_horizontal_scroll: bool,
    needs_vertical_scroll: bool,
) -> Element<'a, Message> {
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
            .id(iced::widget::Id::new(EDITOR_STAGE_ID))
            .direction(scroll_direction)
            .on_scroll(|v| {
                let o = v.absolute_offset();
                Message::Window(WindowMsg::EditorStageScrolled(o.x, o.y))
            })
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    } else {
        container(content)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }
}

/// The title row: capture size, the Image menu, Show Panel and zoom.
fn editor_title_layer<'a>(
    ed: &'a EditorSession,
    p: &'static Palette,
    mode: PanelMode,
) -> Element<'a, Message> {
    use crate::editor::EditorPopover;
    use crate::icons::AppIcon;
    use iced::widget::tooltip::Position;

    let mono = mono_font();
    let busy = ed.busy;
    let popover = ed.popover;
    let zoom = ed.zoom;
    let display_scale = ed.display_scale;
    // --- Title row: capture size + zoom ---
    let size_label = editor_size_label(ed);
    // In Fit mode the scale is only known once the stage has laid out
    // (it depends on the stage size), and iced re-runs layout — not
    // `view` — on a window resize. So the zoom readout is built inside a
    // `responsive`, which runs during layout after the stage (the stack's
    // first layer) has recorded its scale.
    let live_scale = ed.render_scale.clone();
    let panel_hidden = matches!(mode, PanelMode::Hidden | PanelMode::Collapsed);
    let image_menu_open = matches!(popover, Some(EditorPopover::Image | EditorPopover::Resize));
    let title_content = responsive(move |_size| {
        let shown_scale = zoom.explicit_scale().unwrap_or_else(|| live_scale.get());
        let mut title = row![
            text("Capture").size(TYPE_BODY).style(text_t2),
            // The label takes the slack and clips, so a long size never
            // pushes the buttons (or Fit) off the row; its tooltip always
            // has the whole value.
            tooltip_with_key(
                container(
                    text(size_label.clone())
                        .font(mono)
                        .size(TYPE_CAPTION)
                        .style(text_t3)
                        .wrapping(iced::widget::text::Wrapping::None),
                )
                .width(Length::Fill)
                .clip(true),
                size_label.clone(),
                None,
                Position::Bottom,
            ),
        ];
        title = title.push(tooltip_with_key(
            bar_button(
                row![
                    crate::icons::icon(AppIcon::Image, p.t2, 15.0),
                    text("Image").size(ButtonSize::Sm.text_size()),
                    crate::icons::icon(AppIcon::Chevron, p.t3, 13.0),
                ]
                .spacing(SP1)
                .align_y(Alignment::Center)
                .into(),
                if image_menu_open {
                    ButtonKind::Secondary
                } else {
                    ButtonKind::Ghost
                },
                ButtonSize::Sm,
                (!busy).then_some(Message::Editor(EditorMsg::PopoverToggled(
                    EditorPopover::Image,
                ))),
            ),
            "Image: Resize, Rotate, Flip",
            Some(super::key_focus::key_label(
                super::key_focus::KEY_IMAGE_MENU,
            )),
            Position::Bottom,
        ));
        if panel_hidden {
            title = title.push(icon_button(
                AppIcon::Sidebar,
                p.t2,
                "Show Panel",
                Some(super::key_focus::key_label(super::key_focus::KEY_PANEL)),
                Some(Message::Editor(EditorMsg::TextPanel(
                    TextPanelMsg::PanelToggled,
                ))),
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
            ))
            .spacing(SP2)
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
    // empty background is the window's drag handle (the shared title row;
    // it holds controls, so it stays on every platform).
    let title_layer = container(super::window_settings::title_drag_row(
        title_content,
        ed.window_id,
    ))
    .width(Length::Fill)
    .height(Length::Fill)
    .align_y(Alignment::Start);
    title_layer.into()
}

/// The options bar under the tool bar, or the hint line in its place.
fn editor_options_layer<'a>(ed: &'a EditorSession, p: &'static Palette) -> Element<'a, Message> {
    let active_tool = ed.model.active_tool();
    // --- Options bar under the tool bar (falls back to the hint line) ---
    let options_layer: Element<'_, Message> = if ed.eyedropper.is_some() {
        container(text(EYEDROPPER_HINT).size(TYPE_CAPTION).style(text_t2))
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
                    editor_compact_hint(active_tool)
                };
                container(text(hint_str).size(TYPE_CAPTION).style(text_t2))
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
    options_layer
}

/// The status toast above the action bar, while a status is showing.
fn editor_toast_layer<'a>(
    ed: &'a EditorSession,
    p: &'static Palette,
) -> Option<Element<'a, Message>> {
    use iced::widget::tooltip::Position;

    let busy = ed.busy;
    // --- Status toast above the action bar ---
    // Auto-dismiss old success / info status text so the chrome
    // doesn't stay loud forever after a successful save / copy.
    // In-progress statuses (Saving…, Copying…, Recognising text…) and
    // errors are kept until the next transition overwrites them — an
    // error the user didn't act on shouldn't vanish after 4s.
    let shown_status = match ed.status.as_ref() {
        // A key guide whose draft, zone or selection ended is stale.
        Some(_) if ed.key_guide_live() == Some(false) => None,
        Some(s)
            if !ed.status_is_in_progress()
                && !ed.status_is_sticky()
                && s.set_at.elapsed() > crate::editor::STATUS_AUTO_DISMISS =>
        {
            tracing::trace!(target: readshot_core::log::cat::EDITOR, "auto-dismissed status: {}", s.text);
            None
        }
        s => s,
    };
    let discard_armed = ed.discard_armed();
    let toast_layer = shown_status.map(|status| {
        let kind = status_toast_kind(status.tone);
        let s = status.text.clone();
        let action: Option<Element<'_, Message>> = if discard_armed {
            Some(
                row![
                    bar_button(
                        text("Keep Editing").size(ButtonSize::Sm.text_size()).into(),
                        ButtonKind::Secondary,
                        ButtonSize::Sm,
                        Some(Message::Editor(EditorMsg::DiscardDisarmed)),
                    ),
                    bar_button(
                        text("Discard").size(ButtonSize::Sm.text_size()).into(),
                        ButtonKind::DangerSolid,
                        ButtonSize::Sm,
                        Some(Message::Editor(EditorMsg::DiscardRequested)),
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
            if toast_is_dismissable(kind) {
                r = r.push(icon_button(
                    crate::icons::action::DISMISS,
                    p.t2,
                    "Dismiss",
                    None,
                    Some(Message::Editor(EditorMsg::StatusDismissed)),
                    false,
                    ICON_BTN_XS,
                    Position::Top,
                ));
            }
            Some(r.into())
        } else if toast_is_dismissable(kind) {
            Some(icon_button(
                crate::icons::action::DISMISS,
                p.t2,
                "Dismiss",
                None,
                Some(Message::Editor(EditorMsg::StatusDismissed)),
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
    toast_layer.map(Into::into)
}

/// The floating action bar: undo / redo, the outputs and Discard.
fn editor_action_layer<'a>(ed: &'a EditorSession, p: &'static Palette) -> Element<'a, Message> {
    use crate::icons::AppIcon;
    use iced::widget::tooltip::Position;

    let busy = ed.busy;
    let icon_color = |enabled: bool| if enabled { p.t2 } else { dimmed(p.t2) };
    // Queued steps count: the buttons show where undo / redo will be.
    let (can_undo, can_redo) = super::compose_jobs::can_undo_redo(ed);
    // --- Action bar: history · outputs · discard ---
    let undo_enabled = !busy && can_undo;
    let redo_enabled = !busy && can_redo;
    let copy_image = tooltip_with_key(
        bar_button(
            row![
                crate::icons::icon(
                    crate::icons::action::COPY_IMAGE,
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
            (!busy).then_some(Message::Editor(EditorMsg::CopyImageRequested)),
        ),
        "Copy Image",
        Some(COPY_IMAGE_KEY),
        Position::Top,
    );
    let action_bar = row![
        icon_button(
            AppIcon::Undo,
            icon_color(undo_enabled),
            "Undo",
            Some(UNDO_KEY),
            undo_enabled.then_some(Message::Editor(EditorMsg::Toolbar(
                readshot_ui::ToolbarMessage::Undo
            ))),
            false,
            ICON_BTN,
            Position::Top,
        ),
        icon_button(
            AppIcon::Redo,
            icon_color(redo_enabled),
            "Redo",
            Some(REDO_KEY),
            redo_enabled.then_some(Message::Editor(EditorMsg::Toolbar(
                readshot_ui::ToolbarMessage::Redo
            ))),
            false,
            ICON_BTN,
            Position::Top,
        ),
        vsep(),
        icon_button(
            crate::icons::action::PIN,
            icon_color(!busy),
            "Pin",
            Some(PIN_KEY),
            (!busy).then_some(Message::Editor(EditorMsg::PinRequested)),
            false,
            ICON_BTN,
            Position::Top,
        ),
        icon_button(
            crate::icons::action::SAVE,
            icon_color(!busy),
            "Save",
            Some(SAVE_KEY),
            (!busy).then_some(Message::Editor(EditorMsg::SaveRequested)),
            false,
            ICON_BTN,
            Position::Top,
        ),
        icon_button(
            crate::icons::action::COPY_TEXT,
            icon_color(!busy),
            "Copy Text",
            Some(COPY_TEXT_KEY),
            (!busy).then_some(Message::Editor(EditorMsg::CopyTextRequested)),
            false,
            ICON_BTN,
            Position::Top,
        ),
        copy_image,
        vsep(),
        // Discard closes the editor without its edits; the capture stays
        // in History, so the ×, not the trash (see `action::DISCARD`).
        icon_button(
            crate::icons::action::DISCARD,
            icon_color(!busy),
            "Discard",
            Some(CLOSE_KEY),
            (!busy).then_some(Message::Editor(EditorMsg::DiscardRequested)),
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
    action_layer.into()
}

/// The open popover: a click-outside scrim, then the card.
fn editor_popover_layers<'a>(
    state: &'a App,
    ed: &'a EditorSession,
    p: &'static Palette,
    which: crate::editor::EditorPopover,
    layers: &mut Vec<Element<'a, Message>>,
) {
    use crate::editor::EditorPopover;
    use iced::widget::{mouse_area, opaque, Space as IcedSpace};

    let mono = mono_font();
    let busy = ed.busy;
    layers.push(
        mouse_area(IcedSpace::new().width(Length::Fill).height(Length::Fill))
            .on_press(Message::Editor(EditorMsg::PopoverClosed))
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
        EditorPopover::Image => image_popover(ed, p),
        EditorPopover::Resize => resize_popover(ed, p),
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
            .align_y(Alignment::Start)
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(Alignment::Center),
        // Image and Resize open under the title row, at the right.
        EditorPopover::Image | EditorPopover::Resize => container(card)
            .padding(iced::Padding {
                top: TITLEBAR_HEIGHT + 4.0,
                right: 12.0,
                ..iced::Padding::ZERO
            })
            .align_y(Alignment::Start)
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(Alignment::End),
    };
    layers.push(placed.into());
}

/// The canvas area's layers with the side panel docked beside them, or
/// floating over them, as the window's width allows.
fn editor_with_panel<'a>(
    state: &'a App,
    ed: &'a EditorSession,
    p: &'static Palette,
    mode: PanelMode,
    layers: Vec<Element<'a, Message>>,
) -> Element<'a, Message> {
    use iced::widget::{mouse_area, opaque, stack, Space as IcedSpace};

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
                container(stack![
                    panel,
                    scrim(Message::Editor(EditorMsg::PopoverClosed))
                ])
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
            scrim(Message::Editor(EditorMsg::TextPanel(
                TextPanelMsg::PanelOverlayDismissed
            ))),
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

/// Title-row zoom controls: − % + | 1:1 Fit. `shown_scale` is the scale
/// the stage is displaying (explicit zoom, or the live fit scale).
fn editor_zoom_segment(
    shown_scale: f32,
    zoom: crate::editor::EditorZoom,
    display_scale: f32,
    busy: bool,
    p: &'static Palette,
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
            "Zoom Out",
            Some(ZOOM_OUT_KEY),
            can_zoom_out.then_some(Message::Editor(EditorMsg::ZoomOut)),
            false,
            ICON_BTN_SM,
            Position::Bottom,
        ),
        // Right-aligned in a box sized for the widest zoom (4× on a 2×
        // display), so the % sign holds its place as the value changes.
        container(
            mono_readout(
                format!(
                    "{:.0}%",
                    shown_scale * display_scale.max(f32::EPSILON) * 100.0
                ),
                "800%",
            )
            .style(text_t2),
        )
        .padding([0.0, SP1]),
        icon_button(
            AppIcon::Plus,
            icon_color(can_zoom_in),
            "Zoom In",
            Some(ZOOM_IN_KEY),
            can_zoom_in.then_some(Message::Editor(EditorMsg::ZoomIn)),
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
                (!busy).then_some(Message::Editor(EditorMsg::ZoomActual)),
            ),
            "Actual Pixels",
            Some(ZOOM_ACTUAL_KEY),
            Position::Bottom,
        ),
        tooltip_with_key(
            bar_button(
                text("Fit").size(ButtonSize::Xs.text_size()).into(),
                selected_kind(zoom.is_fit()),
                ButtonSize::Xs,
                (!busy).then_some(Message::Editor(EditorMsg::ZoomFit)),
            ),
            "Fit to View",
            Some(ZOOM_FIT_KEY),
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
        iced::widget::slider(MIN_LINE_WIDTH..=MAX_LINE_WIDTH, line_width, |a| {
            Message::Editor(EditorMsg::LineWidthPreview(a))
        })
        .step(0.5_f32)
        .on_release(Message::Editor(EditorMsg::LineWidthCommit))
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
            text(format!("{}\u{a0}px", line_width_label(line_width)))
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

/// Title-row size: the image, and the beautified output when it differs.
pub(crate) fn editor_size_label(ed: &crate::editor::EditorSession) -> String {
    let (w, h) = ed.effective_image_size();
    let s = ed.model.beautify();
    if s.is_identity() {
        return format!("{w}\u{a0}×\u{a0}{h}\u{a0}px");
    }
    match readshot_core::beautify::checked_output_geometry(w, h, &s) {
        Ok(g) => format!(
            "{w}\u{a0}×\u{a0}{h} → {}\u{a0}×\u{a0}{}\u{a0}px",
            g.width, g.height
        ),
        // The panel says why and what to change.
        Err(_) => format!("{w}\u{a0}×\u{a0}{h}\u{a0}px → canvas too large"),
    }
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
    let drag_area =
        mouse_area(img).on_double_click(Message::Editor(EditorMsg::Pin(PinMsg::LockToggled(id))));
    let drag_layer: Element<'_, Message> = if locked {
        drag_area.into()
    } else {
        drag_area
            .on_press(Message::Editor(EditorMsg::Pin(PinMsg::DragRequested(id))))
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
            Some(Message::Editor(EditorMsg::Pin(PinMsg::LockToggled(id)))),
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
            crate::icons::action::DISMISS,
            DARK.t1,
            "Close",
            Some(ESC_KEY),
            Some(Message::Editor(EditorMsg::Pin(PinMsg::ClosePressed(id)))),
            false,
            ICON_BTN_SM,
            pos,
        );
        let step = |delta: f32| {
            Message::Editor(EditorMsg::Pin(PinMsg::OpacityChanged(
                id,
                (opacity + delta).clamp(PIN_OPACITY_MIN, PIN_OPACITY_MAX),
            )))
        };
        let bar: Element<'_, Message> =
            if !pin_hover_controls(locked, available.width).contains(&PinControl::Opacity) {
                // Locked (opacity is locked too) or narrow: Lock and Close.
                // A locked pin still closes in one click.
                row![lock, close].align_y(Alignment::Center).into()
            } else {
                row![
                    lock,
                    vsep(),
                    icon_button(
                        AppIcon::Minus,
                        DARK.t1,
                        "Less Opaque",
                        Some("-"),
                        Some(step(-PIN_OPACITY_STEP)),
                        false,
                        ICON_BTN_SM,
                        pos,
                    ),
                    text(format!("{:.0}%", opacity * 100.0))
                        .size(TYPE_CAPTION)
                        .font(iced::Font::MONOSPACE)
                        .color(DARK.t2),
                    icon_button(
                        AppIcon::Plus,
                        DARK.t1,
                        "More Opaque",
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
    .on_enter(Message::Editor(EditorMsg::Pin(PinMsg::Hovered(id, true))))
    .on_exit(Message::Editor(EditorMsg::Pin(PinMsg::Hovered(id, false))))
    .into()
}

/// Below this pin width the hover bar drops the opacity controls and
/// keeps only Lock and Close (keyboard -/+ still adjusts opacity).
pub(crate) fn pin_controls_compact(width: f32) -> bool {
    width < 230.0
}

/// A control of a pin's hover bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PinControl {
    Lock,
    Opacity,
    Close,
}

/// What a hovered pin's bar shows. Close is always there: a locked pin
/// closes in one click, as esc / ⌘W close it from the keyboard.
pub(crate) fn pin_hover_controls(locked: bool, width: f32) -> &'static [PinControl] {
    if locked || pin_controls_compact(width) {
        &[PinControl::Lock, PinControl::Close]
    } else {
        &[PinControl::Lock, PinControl::Opacity, PinControl::Close]
    }
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
        T::NumberedPin => ("Numbered Pin", "N"),
        T::Stamp => ("Stamp", "E"),
        T::Magnifier => ("Magnifier", "M"),
        T::Crop => ("Crop", "C"),
    }
}

/// Each tool's how-to: a sentence ending in a
/// full stop, keys named as in chips (`⌫`, `↵`, `esc`; the Ctrl form off
/// macOS). Shown in the tool's tooltip, under the tool bar when there is
/// no options bar, and as a notice when the tool is chosen.
pub(crate) fn editor_compact_hint(tool: readshot_ui::editor::ToolState) -> String {
    use readshot_ui::editor::ToolState as T;
    match tool {
        T::Select => format!("Click to select, drag to move. {DELETE_KEY} removes the selection."),
        T::Rectangle => "Drag to draw a rectangle.".into(),
        T::Ellipse => "Drag to draw an ellipse.".into(),
        T::Line => "Drag from start to end.".into(),
        T::Arrow => "Drag from the base to the target.".into(),
        T::Pen => "Drag to draw freehand.".into(),
        T::Highlighter => "Drag over text to highlight it.".into(),
        T::Text => format!("Click, type, then press {COMMIT_TEXT_KEY}."),
        T::Blur => "Drag over an area to blur it. Blur can sometimes be reversed; for passwords, draw a filled Rectangle instead.".into(),
        T::Pixelate => "Drag over an area to pixelate it. Pixelation can sometimes be reversed; for passwords, draw a filled Rectangle instead.".into(),
        T::Spotlight => "Drag an area to spotlight; the rest dims.".into(),
        T::NumberedPin => "Click to drop the next number.".into(),
        T::Stamp => "Click to place a stamp.".into(),
        T::Magnifier => "Drag out the area to magnify.".into(),
        T::Crop => "Drag the area to keep.".into(),
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
        return Some(Binding::Custom(Message::Editor(EditorMsg::TextCommit)));
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
    // Capture Window…: the overlay picks a window instead of a region.
    if let (Some(windows), Some(d)) = (state.window_pick.as_ref(), overlay_record) {
        let picker = Canvas::new(crate::overlay::WindowPickProgram {
            windows: crate::overlay::pickable_windows(
                windows,
                (d.origin_x, d.origin_y, d.width, d.height),
            ),
        })
        .width(Length::Fill)
        .height(Length::Fill);
        return container(picker)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(|_theme| iced::widget::container::Style {
                background: Some(Color::TRANSPARENT.into()),
                ..Default::default()
            })
            .into();
    }
    let auto_confirm_intent = overlay_auto_confirm_intent(state);

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
    let toolbar_layer = overlay_record
        .and_then(|d| overlay_action_bar_rect(state, &d.display_id).map(|rect| (d, rect)))
        .map(|(d, rect)| overlay_toolbar_layer(&d.display_id, rect, d.width, d.height));

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

/// The selection the overlay's action bar sits on for `display_id`, or
/// `None` when no bar is drawn: nothing is selected there, or the
/// overlay confirms on its own (CLI, tray Scrolling Capture, Add
/// Capture), which never shows one.
pub(crate) fn overlay_action_bar_rect<'a>(
    state: &'a App,
    display_id: &str,
) -> Option<&'a readshot_core::geom::Rect> {
    if overlay_auto_confirm_intent(state).is_some() {
        return None;
    }
    state.overlay_selections.get(display_id)
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
    } else if matches!(
        state.pending_intent,
        Some(crate::app::CaptureIntent::CombineIntoEditor)
    ) || state.combine_capture.is_some()
    {
        // Add Capture: the drag release confirms at once, no toolbar.
        Some(crate::app::CaptureIntent::CombineIntoEditor)
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
    let sel = |intent| {
        Message::Capture(CaptureMsg::OverlaySelected {
            display_id: display_id.clone(),
            rect: *rect,
            intent,
        })
    };
    let fg = DARK.t2;
    let edit = tooltip_with_key(
        // The button is a fixed size, so centre its content both ways:
        // a button lays its content out from the top-left.
        button(
            container(
                row![
                    crate::icons::icon(crate::icons::action::OPEN_IN_EDITOR, DARK.pri_fg, 15.0),
                    text("Edit").size(TYPE_BODY)
                ]
                .spacing(7)
                .align_y(Alignment::Center),
            )
            .center_x(Length::Fill)
            .center_y(Length::Fill),
        )
        .height(Length::Fixed(BTN_MD))
        .width(Length::Fixed(crate::overlay::OVERLAY_EDIT_W))
        .padding(0)
        .style(|t, s| button_style(t, s, ButtonKind::Primary))
        .on_press(sel(CaptureIntent::Editor)),
        "Open in Editor",
        Some(RETURN_KEY),
        tip_pos,
    );
    let buttons = row![
        edit,
        icon_button(
            crate::icons::action::COPY_IMAGE,
            fg,
            "Copy Image",
            Some(COPY_IMAGE_KEY),
            Some(sel(CaptureIntent::CopyToClipboard)),
            false,
            ICON_BTN,
            tip_pos
        ),
        icon_button(
            crate::icons::action::COPY_TEXT,
            fg,
            "Copy Text",
            Some(COPY_TEXT_KEY),
            Some(sel(CaptureIntent::CopyTextDirect)),
            false,
            ICON_BTN,
            tip_pos
        ),
        icon_button(
            crate::icons::action::SAVE,
            fg,
            "Save",
            Some(SAVE_KEY),
            Some(sel(CaptureIntent::SaveDirect)),
            false,
            ICON_BTN,
            tip_pos
        ),
        icon_button(
            crate::icons::action::PIN,
            fg,
            "Pin",
            Some(PIN_KEY),
            Some(sel(CaptureIntent::Pin)),
            false,
            ICON_BTN,
            tip_pos
        ),
        icon_button(
            AppIcon::Scroll,
            fg,
            "Scrolling Capture",
            Some(SCROLL_CAPTURE_KEY),
            Some(sel(CaptureIntent::ScrollCapture)),
            false,
            ICON_BTN,
            tip_pos
        ),
        vsep(),
        icon_button(
            crate::icons::action::DISMISS,
            fg,
            "Cancel",
            Some(ESC_KEY),
            Some(Message::Capture(CaptureMsg::OverlayCancelled)),
            false,
            ICON_BTN,
            tip_pos
        ),
    ]
    .spacing(crate::overlay::OVERLAY_TOOLBAR_SPACING)
    .align_y(Alignment::Center);
    // Drawn at the size placement assumes (`toolbar_rect`).
    let bar = container(buttons)
        .padding(crate::overlay::OVERLAY_TOOLBAR_PAD)
        .width(Length::Fixed(crate::overlay::OVERLAY_TOOLBAR_WIDTH))
        .height(Length::Fixed(crate::overlay::OVERLAY_TOOLBAR_HEIGHT))
        .style(floating_bar_style);

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

    let p = palette(state.theme_mode);

    // --- Sidebar -------------------------------------------------------
    const PANE_KEYS: [&str; 7] = SETTINGS_PANE_KEYS;
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
            .on_press(Message::Settings(SettingsMsg::PaneSelected(pane)));
        nav = nav.push(tooltip_with_key(
            entry,
            pane.label(),
            Some(key),
            Position::Right,
        ));
    }
    let footer = column![
        row![
            key_chip("Tab"),
            text("Next control").size(TYPE_CAPTION).style(text_t3)
        ]
        .spacing(SP2)
        .align_y(Alignment::Center),
        row![
            key_chip(ESC_KEY),
            text("/").size(TYPE_CAPTION).style(text_t3),
            key_chip(CLOSE_KEY),
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
    // The sidebar's top strip clears the traffic lights on macOS; off
    // macOS (native title bar) it is a plain strip of the same height, so
    // the first section lines up with the pane's heading row, which
    // stays on every platform.
    let sidebar_top: Element<'_, Message> = own_title_row(Space::new(), state.settings_window_id)
        .unwrap_or_else(|| Space::new().height(Length::Fixed(TITLEBAR_HEIGHT)).into());
    let sidebar = container(
        column![]
            .push(sidebar_top)
            .push(nav)
            .push(Space::new().height(Length::Fill))
            .push(footer),
    )
    .width(Length::Fixed(200.0))
    .height(Length::Fill)
    .style(surface_style);

    // --- Selected pane -------------------------------------------------
    let pane = state.settings_pane;
    let body: Vec<Element<'_, Message>> = match pane {
        // Uninstalled: nothing left to change; only the outcome and Quit.
        _ if state.uninstalled => settings_app_pane(state),
        SettingsPane::Permissions => settings_permissions_pane(state),
        SettingsPane::Capture => settings_capture_pane(state),
        SettingsPane::Files => settings_files_pane(state),
        SettingsPane::TextRecognition => settings_ocr_pane(state),
        SettingsPane::Export => settings_export_pane(state),
        SettingsPane::Diagnostics => settings_diagnostics_pane(state),
        SettingsPane::App => settings_app_pane(state),
    };
    let mut title_line = row![text(pane.label())
        .size(TYPE_HEADING)
        .font(semibold())
        .style(text_t1)]
    .spacing(SP3)
    .align_y(Alignment::Center);
    if let Some(feedback) = state.settings_key_feedback.as_deref() {
        // What the last shortcut changed, so ⌘F / ⌘R never act silently.
        title_line = title_line.push(Space::new().width(Length::Fill)).push(
            text(feedback.to_string())
                .size(TYPE_CAPTION)
                .style(text_t2)
                .wrapping(iced::widget::text::Wrapping::None),
        );
    }
    let title_content = container(title_line)
        .padding([0, 22])
        .width(Length::Fill)
        .height(Length::Fixed(TITLEBAR_HEIGHT))
        .align_y(Alignment::Center);
    // The pane's heading, not the window's name: kept on every platform.
    let title_row = title_drag_row(title_content, state.settings_window_id);
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

    let content = column![title_row, pane_body]
        .width(Length::Fill)
        .height(Length::Fill);
    let layout: Element<'_, Message> = if settings_sidebar_shown(state) {
        row![sidebar, vhairline(), content].into()
    } else {
        content.into()
    };
    container(layout)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(bg_style)
        .into()
}

/// The pane sidebar, hidden once Readshot is uninstalled: no other pane
/// has anything left to change.
pub(crate) fn settings_sidebar_shown(state: &App) -> bool {
    !state.uninstalled
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
            radius: R_BUTTON.into(),
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
    settings_block_with(label, help, None, control)
}

/// A [`group_row`] whose help line is an error: set in `danger` with an
/// Alert glyph, as [`settings_block_with`] does.
fn group_row_error<'a>(
    label: &'a str,
    error: String,
    danger: Color,
    control: Element<'a, Message>,
) -> Element<'a, Message> {
    let line = row![
        crate::icons::icon(crate::icons::AppIcon::Alert, danger, 14.0),
        text(error)
            .size(TYPE_CAPTION)
            .style(text_danger)
            .wrapping(iced::widget::text::Wrapping::Word)
    ]
    .spacing(6)
    .align_y(Alignment::Start);
    let left = column![text(label).size(TYPE_BODY).style(text_t1), line].spacing(2);
    row![left.width(Length::Fill), control]
        .spacing(SP3)
        .align_y(Alignment::Center)
        .padding([10, 14])
        .into()
}

/// [`settings_block`] whose help line can be an error: with
/// `error_icon` (the palette's danger colour) it is set in `danger` with
/// an Alert glyph, not in the neutral help grey.
fn settings_block_with<'a>(
    label: &'a str,
    help: impl Into<String>,
    error_icon: Option<Color>,
    control: Element<'a, Message>,
) -> Element<'a, Message> {
    let help_is_error = error_icon.is_some();
    let help = help.into();
    let mut head = column![text(label).size(TYPE_BODY).style(text_t1)].spacing(2);
    if !help.is_empty() {
        let line = text(help)
            .size(TYPE_CAPTION)
            .style(if help_is_error { text_danger } else { text_t3 })
            .wrapping(iced::widget::text::Wrapping::Word);
        let help_line: Element<'a, Message> = if let Some(c) = error_icon {
            row![
                crate::icons::icon(crate::icons::AppIcon::Alert, c, 14.0),
                line
            ]
            .spacing(6)
            .align_y(Alignment::Start)
            .into()
        } else {
            line.into()
        };
        head = head.push(help_line);
    }
    column![head, control]
        .spacing(SP2)
        .padding([10, 14])
        .width(Length::Fill)
        .into()
}

/// Whether the Settings focus ring is on `item`.
fn settings_lit(state: &App, item: crate::app::SettingsFocus) -> bool {
    state.settings_focus == Some(item)
}

/// Width the Settings permission actions need on one row: Recheck
/// (≈ 80 px), Reset Permission (≈ 135 px) and Open System Settings
/// (≈ 160 px) at 13 px with their padding, plus two 8 px gaps.
pub(crate) const PERMISSION_ACTIONS_ROW_MIN: f32 = 400.0;

pub(crate) fn permission_actions_stack(width: f32) -> bool {
    width < PERMISSION_ACTIONS_ROW_MIN
}

fn settings_permissions_pane(state: &App) -> Vec<Element<'_, Message>> {
    use crate::app::SettingsFocus as F;
    // Never probe from view(): it runs on every redraw.
    let permission_status = state.permission_status;
    let (lit_open, lit_recheck, lit_reset) = (
        settings_lit(state, F::PermissionOpen),
        settings_lit(state, F::PermissionRecheck),
        settings_lit(state, F::PermissionReset),
    );
    let (permission_title, permission_hint) = permission_settings_summary(permission_status);
    let permission_control: Element<'_, Message> = responsive(move |available| {
        let status = column![
            text(permission_title).size(TYPE_BODY).style(text_t1),
            text(permission_hint)
                .size(TYPE_CAPTION)
                .style(text_t3)
                .wrapping(iced::widget::text::Wrapping::Word),
        ]
        .spacing(6)
        .width(Length::Fill);

        if matches!(permission_status, PermissionStatus::NotApplicable) {
            return status.into();
        }

        let open_button = focus_ring(
            button(text("Open System Settings").size(TYPE_BODY))
                .padding([6, 12])
                .style(|t, s| action_button_style(t, s, ActionKind::Secondary))
                .on_press(Message::App(AppMsg::OpenPermissionSettingsRequested)),
            lit_open,
        );
        // Recheck button forces a fresh permission poll so the user
        // can confirm a System Settings change without quitting and
        // relaunching the app. Drives the existing PermissionPoll
        // handler with the latest probe result.
        let recheck_button = focus_ring(
            button(text("Recheck").size(TYPE_BODY))
                .padding([6, 12])
                .style(|t, s| action_button_style(t, s, ActionKind::Secondary))
                .on_press(Message::App(AppMsg::PermissionTick)),
            lit_recheck,
        );
        // Clears a grant macOS still holds for an earlier, differently
        // signed version, then asks again.
        let reset_button = focus_ring(
            button(text("Reset Permission").size(TYPE_BODY))
                .padding([6, 12])
                .style(|t, s| action_button_style(t, s, ActionKind::Secondary))
                .on_press(Message::App(AppMsg::ResetPermissionRequested)),
            lit_reset,
        );
        // Once access is granted there is nothing to recheck or reset.
        // Three actions need ≈ 333 px; below that they go one per line
        // so none is clipped at the 520 px window minimum.
        let actions: Element<'_, Message> =
            if matches!(permission_status, PermissionStatus::Granted) {
                open_button
            } else if permission_actions_stack(available.width) {
                column![recheck_button, reset_button, open_button]
                    .spacing(8)
                    .into()
            } else {
                row![recheck_button, reset_button, open_button]
                    .spacing(8)
                    .align_y(Alignment::Center)
                    .into()
            };
        // The status keeps at least ≈ 180 px beside the actions, or goes
        // above them.
        if available.width < PERMISSION_ACTIONS_ROW_MIN + 180.0 {
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

/// Whether the Settings recorder is armed for `target`.
fn recording_target(state: &App, target: crate::app::HotkeyTarget) -> bool {
    state.settings_mode.is_recording_hotkey() && state.settings_recording_target == target
}

/// The help line under the window-capture shortcut, and whether it is a
/// problem.
pub(crate) fn window_hotkey_hint(state: &App) -> (String, bool) {
    let chord = state.preferences.window_capture_hotkey.trim();
    if recording_target(state, crate::app::HotkeyTarget::WindowCapture) {
        return match &state.settings_window_hotkey_error {
            Some(e) => (e.clone(), true),
            None => (String::new(), false),
        };
    }
    if let Some(e) = &state.settings_window_hotkey_error {
        return (e.clone(), true);
    }
    if chord.is_empty() {
        return (
            "Not set. Record one to pick a window to capture from any app.".to_string(),
            false,
        );
    }
    let pretty = pretty_hotkey(chord);
    if readshot_ui::hotkey::parse(chord).is_err() {
        return (
            "Couldn’t read that shortcut. Press Record to choose a new one.".to_string(),
            true,
        );
    }
    if let Some(clash) = window_hotkey_conflict(&state.preferences) {
        return (
            format!("{pretty} is already {clash}. Choose a different shortcut."),
            true,
        );
    }
    if !state.window_hotkey_registered {
        return (
            format!("Another app may be using {pretty}. Choose a different shortcut."),
            true,
        );
    }
    ("Opens the window picker from any app.".to_string(), false)
}

/// Whether the capture-hotkey help line reports a problem (a rejected or
/// unregistered chord) rather than help. Mirrors the branch order that
/// builds the line.
pub(crate) fn capture_hotkey_hint_is_error(state: &App) -> bool {
    let chord = &state.preferences.capture_hotkey;
    state.settings_hotkey_error.is_some()
        || (!recording_target(state, crate::app::HotkeyTarget::Capture)
            && state.settings_hotkey_status.is_none()
            && (readshot_ui::hotkey::parse(chord).is_err()
                || capture_hotkey_conflict(chord).is_some()
                || !state.capture_hotkey_registered))
}

/// The help line under the capture shortcut: a problem when there is
/// one, else what the shortcut does. While recording, the instruction
/// ("Press the new shortcut, or esc to cancel.") is shown once, under the
/// control, never here as well.
pub(crate) fn capture_hotkey_hint(state: &App) -> String {
    let pretty = pretty_hotkey(&state.preferences.capture_hotkey);
    let mut hotkey_hint = if recording_target(state, crate::app::HotkeyTarget::Capture) {
        // The one recording instruction sits under the control; this
        // line keeps only a problem, or the plain description.
        state.settings_hotkey_error.clone().unwrap_or_default()
    } else if let Some(err) = &state.settings_hotkey_error {
        // A rejected chord (e.g. a conflict with a reserved hotkey) or a
        // registration failure. Show it even when recording has ended.
        err.clone()
    } else if let Some(status) = &state.settings_hotkey_status {
        status.clone()
    } else if readshot_ui::hotkey::parse(&state.preferences.capture_hotkey).is_err() {
        format!(
            "Couldn’t read the saved shortcut “{}”. Press Record to choose a new one.",
            state.preferences.capture_hotkey
        )
    } else if let Some(reserved) = capture_hotkey_conflict(&state.preferences.capture_hotkey) {
        format!("{pretty} is reserved for the {reserved} shortcut. Choose a different shortcut.")
    } else if !state.capture_hotkey_registered {
        // Parsed-OK but registration failed, e.g. another app already
        // owns the chord. Tell the user so they pick another one.
        format!("Another app may be using {pretty}. Choose a different shortcut.")
    } else {
        String::new()
    };
    if hotkey_hint.is_empty() {
        hotkey_hint = "Opens the capture overlay from any app.".to_string();
    }
    hotkey_hint
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
    let hotkey_hint = capture_hotkey_hint(state);
    let hotkey_hint_is_error = capture_hotkey_hint_is_error(state);
    let recording_capture = state.settings_mode.is_recording_hotkey()
        && state.settings_recording_target == crate::app::HotkeyTarget::Capture;
    let hotkey_label = if recording_capture {
        "Press shortcut…".to_string()
    } else {
        pretty
    };
    let settings_recording_hotkey = recording_capture;
    let lit_record = settings_lit(state, crate::app::SettingsFocus::HotkeyRecord);
    let lit_reset = settings_lit(state, crate::app::SettingsFocus::HotkeyReset);
    let hotkey_control: Element<'_, Message> = responsive(move |available| {
        let value = setting_value_box(hotkey_label.clone(), settings_recording_hotkey);
        // Record / Cancel are exclusive: while a recording is armed, the
        // user needs an obvious exit that doesn't require pressing a
        // real shortcut.
        let primary_btn: Element<'_, Message> = if settings_recording_hotkey {
            button(text("Cancel").size(TYPE_BODY))
                .padding([6, 12])
                .style(|t, s| action_button_style(t, s, ActionKind::Secondary))
                .on_press(Message::Settings(SettingsMsg::HotkeyRecordingCancelled))
                .into()
        } else {
            tooltip_with_key(
                button(text("Record").size(TYPE_BODY))
                    .padding([6, 12])
                    .style(|t, s| action_button_style(t, s, ActionKind::Secondary))
                    .on_press(Message::Settings(SettingsMsg::StartHotkeyRecording)),
                "Record New Shortcut",
                Some(SETTINGS_RECORD_HOTKEY_KEY),
                Position::Top,
            )
        };
        let actions = row![
            focus_ring(primary_btn, lit_record),
            focus_ring(
                button(text("Reset").size(TYPE_BODY))
                    .padding([6, 12])
                    .style(|t, s| action_button_style(t, s, ActionKind::Secondary))
                    .on_press(Message::Settings(SettingsMsg::Edit(
                        SettingsMessage::SetCaptureHotkey(default_capture_hotkey().into()),
                    ))),
                lit_reset
            ),
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
                text(format!("Press the new shortcut, or {ESC_KEY} to cancel."))
                    .size(TYPE_CAPTION)
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

    let retention_control = focus_ring(
        tooltip_with_key(
            pick_list(
                &RETENTION_OPTIONS[..],
                Some(state.preferences.history_retention),
                |r| Message::Settings(SettingsMsg::Edit(SettingsMessage::SetHistoryRetention(r))),
            )
            .text_size(TYPE_BODY)
            .padding([7, 10]),
            "Next Retention Option",
            Some(SETTINGS_RETENTION_KEY),
            Position::Top,
        ),
        settings_lit(state, crate::app::SettingsFocus::Retention),
    );

    let mut shortcut_rows = vec![settings_block_with(
        "Capture shortcut",
        hotkey_hint,
        hotkey_hint_is_error.then_some(palette(state.theme_mode).danger),
        hotkey_control,
    )];
    if crate::tray::WINDOW_CAPTURE_IN_MENU {
        let (hint, is_error) = window_hotkey_hint(state);
        shortcut_rows.push(settings_block_with(
            "Window capture shortcut",
            hint,
            is_error.then_some(palette(state.theme_mode).danger),
            window_hotkey_control(state),
        ));
    }
    vec![
        settings_group_heading("Shortcut", text_t3),
        settings_group(shortcut_rows),
        settings_group_heading("History", text_t3),
        settings_group(vec![group_row(
            "History retention",
            RETENTION_HELP,
            retention_control,
        )]),
    ]
}

/// The window-capture shortcut's value box, Record (Cancel while
/// recording) and Clear.
fn window_hotkey_control(state: &App) -> Element<'_, Message> {
    let recording = recording_target(state, crate::app::HotkeyTarget::WindowCapture);
    let chord = state.preferences.window_capture_hotkey.trim();
    let value = if recording {
        "Press shortcut…".to_string()
    } else if chord.is_empty() {
        "Not set".to_string()
    } else {
        pretty_hotkey(chord)
    };
    let primary: Element<'_, Message> = if recording {
        button(text("Cancel").size(TYPE_BODY))
            .padding([6, 12])
            .style(|t, s| action_button_style(t, s, ActionKind::Secondary))
            .on_press(Message::Settings(SettingsMsg::HotkeyRecordingCancelled))
            .into()
    } else {
        button(text("Record").size(TYPE_BODY))
            .padding([6, 12])
            .style(|t, s| action_button_style(t, s, ActionKind::Secondary))
            .on_press(Message::Settings(SettingsMsg::StartWindowHotkeyRecording))
            .into()
    };
    let clear = button(text("Clear").size(TYPE_BODY))
        .padding([6, 12])
        .style(|t, s| action_button_style(t, s, ActionKind::Secondary))
        .on_press_maybe((!chord.is_empty()).then(|| {
            Message::Settings(SettingsMsg::Edit(SettingsMessage::SetWindowCaptureHotkey(
                String::new(),
            )))
        }));
    let main = row![
        setting_value_box(value, recording),
        focus_ring(
            primary,
            settings_lit(state, crate::app::SettingsFocus::WindowHotkeyRecord)
        ),
        focus_ring(
            clear,
            settings_lit(state, crate::app::SettingsFocus::WindowHotkeyClear)
        ),
    ]
    .spacing(8)
    .align_y(Alignment::Center);
    if recording {
        column![
            main,
            text(format!("Press the new shortcut, or {ESC_KEY} to cancel."))
                .size(TYPE_CAPTION)
                .style(text_acc),
        ]
        .spacing(6)
        .into()
    } else {
        main.into()
    }
}

/// What each retention choice does, including the deletion it causes.
pub(crate) const RETENTION_HELP: &str = "Off saves no new captures and keeps the ones you have. \
     Last 50 captures and Last 30 days permanently delete older captures at the next capture.";

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
            focus_ring(
                button(text("Choose").size(TYPE_BODY))
                    .padding([6, 12])
                    .style(|t, s| action_button_style(t, s, ActionKind::Secondary))
                    .on_press(Message::Settings(SettingsMsg::ChooseSaveFolderRequested)),
                settings_lit(state, crate::app::SettingsFocus::FolderChoose)
            ),
            focus_ring(
                button(text("Open").size(TYPE_BODY))
                    .padding([6, 12])
                    .style(|t, s| action_button_style(t, s, ActionKind::Secondary))
                    .on_press(Message::Settings(SettingsMsg::OpenSaveFolderRequested)),
                settings_lit(state, crate::app::SettingsFocus::FolderOpen)
            ),
            focus_ring(
                button(text("Reset").size(TYPE_BODY))
                    .padding([6, 12])
                    .style(|t, s| action_button_style(t, s, ActionKind::Secondary))
                    .on_press(Message::Settings(SettingsMsg::Edit(
                        SettingsMessage::SetSaveFolder(PathBuf::new())
                    ))),
                settings_lit(state, crate::app::SettingsFocus::FolderReset)
            ),
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
    let preview_name = default_save_filename(
        &preview_template,
        chrono::Local::now(),
        state.preferences.default_format,
    );
    let filename_input = focus_ring(
        text_input("Screenshot {YYYY-MM-DD at HH.mm.ss}", &filename_template)
            .id(iced::widget::Id::new(settings_ops::SETTINGS_FILENAME_ID))
            .on_input(|s| {
                Message::Settings(SettingsMsg::Edit(SettingsMessage::SetFilenameTemplate(s)))
            })
            .size(TYPE_BODY)
            .padding([8, 10])
            .style(text_input_style),
        settings_lit(state, crate::app::SettingsFocus::FilenameField),
    );
    let preview_line = text(format!("Saves as: {preview_name}"))
        .size(TYPE_CAPTION)
        .style(text_t3);
    let filename_control: Element<'_, Message> =
        column![filename_input, preview_line].spacing(4).into();

    vec![
        settings_group_heading("Save location and naming", text_t3),
        settings_group(vec![
            match &state.settings_folder_status {
                Some(error) => settings_block_with(
                    "Default save folder",
                    error.clone(),
                    Some(palette(state.theme_mode).danger),
                    save_folder_control,
                ),
                None => settings_block(
                    "Default save folder",
                    "Starting folder for Save. Platform default uses your system screenshots location.",
                    save_folder_control,
                ),
            },
            settings_block(
                "Filename template",
                "Tokens: {YYYY-MM-DD at HH.mm.ss} · {YYYY-MM-DD} · {HH.mm.ss} · {YYYY} · {timestamp}",
                filename_control,
            ),
        ]),
    ]
}

fn settings_ocr_pane(state: &App) -> Vec<Element<'_, Message>> {
    // One engine per platform (Apple Vision, Windows.Media.Ocr, ocrs), so
    // there is no engine to choose; only the languages.
    let ocr_supported = state.coordinator.supported_languages();
    let ocr_selected = state.preferences.ocr_languages.clone();
    vec![
        settings_group_heading("Text recognition", text_t3),
        settings_group(vec![settings_block(
            "Recognition languages",
            "Leave all off to let the recognizer choose. Applies to your next capture.",
            ocr_language_control(
                &ocr_supported,
                &ocr_selected,
                match state.settings_focus {
                    Some(crate::app::SettingsFocus::OcrLanguage(i)) => Some(i),
                    _ => None,
                },
            ),
        )]),
    ]
}

fn settings_export_pane(state: &App) -> Vec<Element<'_, Message>> {
    use iced::widget::pick_list;
    use iced::widget::tooltip::Position;
    use readshot_core::ExportFormat;

    const FORMAT_OPTIONS: [ExportFormat; 3] =
        [ExportFormat::Png, ExportFormat::Jpeg, ExportFormat::Webp];

    let format_control = focus_ring(
        tooltip_with_key(
            pick_list(
                &FORMAT_OPTIONS[..],
                Some(state.preferences.default_format),
                |f| Message::Settings(SettingsMsg::Edit(SettingsMessage::SetDefaultFormat(f))),
            )
            .text_size(TYPE_BODY)
            .padding([7, 10]),
            "Next Format",
            Some(SETTINGS_FORMAT_KEY),
            Position::Top,
        ),
        settings_lit(state, crate::app::SettingsFocus::Format),
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
    let debug_toggle = focus_ring(
        tooltip_with_key(
            toggler(state.preferences.debug_logging).on_toggle(|v| {
                Message::Settings(SettingsMsg::Edit(SettingsMessage::SetDebugLogging(v)))
            }),
            "Toggle Debug Logging",
            Some(SETTINGS_DEBUG_LOGGING_KEY),
            Position::Top,
        ),
        settings_lit(state, crate::app::SettingsFocus::DebugLogging),
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

    use crate::app::SettingsFocus as F;
    let startup_toggle = focus_ring(
        tooltip_with_key(
            toggler(state.preferences.launch_at_login).on_toggle(|v| {
                Message::Settings(SettingsMsg::Edit(SettingsMessage::SetLaunchAtLogin(v)))
            }),
            "Toggle Open at Login",
            Some(SETTINGS_LAUNCH_AT_LOGIN_KEY),
            Position::Top,
        ),
        settings_lit(state, F::LaunchAtLogin),
    );

    // Both destructive rows keep the two-step arm/confirm pattern: the
    // idle button (outlined red) arms, the armed step shows a filled-red
    // confirm plus Cancel. ⇧⌘R / ⇧⌘U press the same two steps.
    let reset_row: Element<'_, Message> = if state.settings_mode.is_confirming_reset_all() {
        settings_block(
            "Reset all settings",
            "Resets the capture shortcut, save folder, filename template, retention, OCR, and startup options. History records are kept.",
            row![
                focus_ring(
                    tooltip_with_key(
                        button(text("Confirm Reset").size(TYPE_BODY))
                            .padding([6, 12])
                            .style(|t, s| action_button_style(t, s, ActionKind::Danger))
                            .on_press(Message::Settings(SettingsMsg::ResetAllConfirmed)),
                        "Confirm Reset",
                        Some(SETTINGS_RESET_ALL_KEY),
                        Position::Top,
                    ),
                    settings_lit(state, F::ResetConfirm)
                ),
                focus_ring(
                    button(text("Cancel").size(TYPE_BODY))
                        .padding([6, 12])
                        .style(|t, s| action_button_style(t, s, ActionKind::Secondary))
                        .on_press(Message::Settings(SettingsMsg::ResetAllCancelled)),
                    settings_lit(state, F::ResetCancel)
                ),
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
            "Reset all settings",
            reset_status,
            focus_ring(
                tooltip_with_key(
                    button(text("Reset…").size(TYPE_BODY))
                        .padding([6, 12])
                        .style(|t, s| action_button_style(t, s, ActionKind::DangerQuiet))
                        .on_press(Message::Settings(SettingsMsg::ResetAllRequested)),
                    "Reset All Settings",
                    Some(SETTINGS_RESET_ALL_KEY),
                    Position::Top,
                ),
                settings_lit(state, F::ResetAll),
            ),
        )
    };

    let uninstall_row: Element<'_, Message> =
        if let Some(uninstall_purge) = state.settings_mode.uninstall_purge() {
            let purge_checkbox = focus_ring(
                container(
                    iced::widget::checkbox(uninstall_purge)
                        .label("Also delete history and preferences")
                        .size(16)
                        .text_size(TYPE_BODY)
                        .on_toggle(|a| Message::Settings(SettingsMsg::UninstallPurgeToggled(a))),
                )
                .padding([2, 4]),
                settings_lit(state, F::UninstallPurge),
            );
            let actions = row![
                focus_ring(
                    tooltip_with_key(
                        button(text("Uninstall").size(TYPE_BODY))
                            .padding([6, 12])
                            .style(|t, s| action_button_style(t, s, ActionKind::Danger))
                            .on_press(Message::Settings(SettingsMsg::UninstallConfirmed)),
                        "Confirm Uninstall",
                        Some(SETTINGS_UNINSTALL_KEY),
                        Position::Top,
                    ),
                    settings_lit(state, F::UninstallConfirm)
                ),
                focus_ring(
                    button(text("Cancel").size(TYPE_BODY))
                        .padding([6, 12])
                        .style(|t, s| action_button_style(t, s, ActionKind::Secondary))
                        .on_press(Message::Settings(SettingsMsg::UninstallCancelled)),
                    settings_lit(state, F::UninstallCancel)
                ),
            ]
            .spacing(8)
            .align_y(Alignment::Center);
            settings_block(
                "Uninstall Readshot",
                crate::uninstall::settings_uninstall_copy(cfg!(target_os = "macos")).detail,
                column![purge_checkbox, actions].spacing(10).into(),
            )
        } else {
            group_row(
                "Uninstall Readshot",
                crate::uninstall::settings_uninstall_copy(cfg!(target_os = "macos")).summary,
                focus_ring(
                    tooltip_with_key(
                        button(text("Uninstall…").size(TYPE_BODY))
                            .padding([6, 12])
                            .style(|t, s| action_button_style(t, s, ActionKind::DangerQuiet))
                            .on_press(Message::Settings(SettingsMsg::UninstallRequested)),
                        "Uninstall Readshot",
                        Some(SETTINGS_UNINSTALL_KEY),
                        Position::Top,
                    ),
                    settings_lit(state, F::Uninstall),
                ),
            )
        };

    if state.uninstalled {
        return vec![
            settings_group_heading("Danger zone", text_danger),
            settings_group(uninstalled_row(state).into_iter().collect()),
        ];
    }
    vec![
        settings_group_heading("Startup", text_t3),
        settings_group(vec![match &state.settings_launch_at_login_error {
            Some(why) => group_row_error(
                "Open Readshot at login",
                why.clone(),
                palette(state.theme_mode).danger,
                startup_toggle,
            ),
            None => group_row("Open Readshot at login", "", startup_toggle),
        }]),
        settings_group_heading("Danger zone", text_danger),
        settings_group(match uninstalled_row(state) {
            // Uninstalled: only the outcome and Quit; nothing else to do.
            Some(row) => vec![row],
            None => vec![reset_row, uninstall_row],
        }),
    ]
}

/// After an uninstall that left the program (`App::uninstalled`): what
/// was removed and what is left, in its tone, under the Uninstall
/// heading, with Quit Readshot.
/// The Uninstall row's status line, and whether it is an error. Only an
/// Error is red with the alert glyph; Progress, Warning and Success stay
/// neutral, led by their own toast glyph (• ! ✓), so a step that is
/// running or worked never reads as a failure.
pub(crate) fn uninstall_status_help(tone: StatusTone, text: &str) -> (String, bool) {
    if tone == StatusTone::Error {
        (text.to_string(), true)
    } else {
        let glyph = toast_glyph(status_toast_kind(tone));
        (format!("{glyph} {text}"), false)
    }
}

fn uninstalled_row(state: &App) -> Option<Element<'_, Message>> {
    let result = state.settings_uninstall_result.as_ref()?;
    let (help, error) = uninstall_status_help(result.tone, &result.text);
    Some(settings_block_with(
        "Uninstall Readshot",
        help,
        error.then_some(palette(state.theme_mode).danger),
        // The default button (↵ presses it); disabled while the steps run.
        focus_ring(
            button(text("Quit Readshot").size(TYPE_BODY))
                .padding([6, 12])
                .style(|t, s| action_button_style(t, s, ActionKind::Primary))
                .on_press_maybe(
                    super::settings_ops::uninstall_finished(state)
                        .then_some(Message::Settings(SettingsMsg::QuitAfterUninstall)),
                ),
            super::settings_ops::uninstall_finished(state),
        ),
    ))
}

/// Simple multi-select for OCR languages: one checkbox per language the
/// active engine reports as supported. Toggling rebuilds the ordered list
/// (insertion order becomes recognition priority). An empty supported
/// list — e.g. the Linux ocrs engine, which reports a single language —
/// renders an explanatory note instead.
pub(crate) fn ocr_language_control(
    supported: &[String],
    selected: &[String],
    focused: Option<usize>,
) -> Element<'static, Message> {
    if supported.is_empty() {
        return setting_value_box(
            "No languages reported by the recognizer.".to_string(),
            false,
        );
    }
    let mut items: Vec<Element<'static, Message>> = Vec::with_capacity(supported.len());
    for (i, lang) in supported.iter().enumerate() {
        let lang = lang.clone();
        let checked = selected.iter().any(|l| l == &lang);
        let selected_now = selected.to_vec();
        let toggle_lang = lang.clone();
        // Show a human-readable name (e.g. "Chinese (Simplified)") but
        // keep storing the raw BCP-47 code that Vision reports.
        let display = ocr_language_display_name(&lang);
        items.push(focus_ring(
            container(
                iced::widget::checkbox(checked)
                    .label(display)
                    .size(16)
                    .text_size(TYPE_BODY)
                    .on_toggle(move |now| {
                        let mut next = selected_now.clone();
                        if now {
                            if !next.iter().any(|l| l == &toggle_lang) {
                                next.push(toggle_lang.clone());
                            }
                        } else {
                            next.retain(|l| l != &toggle_lang);
                        }
                        Message::Settings(SettingsMsg::Edit(SettingsMessage::SetOcrLanguages(next)))
                    }),
            )
            .padding([2, 4]),
            focused == Some(i),
        ));
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
            .size(TYPE_BODY)
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
                crate::icons::action::SCREEN_RECORDING,
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
        let kind = status_toast_kind(s.tone);
        let action = toast_is_dismissable(kind).then(|| {
            icon_button(
                crate::icons::action::DISMISS,
                p.t2,
                "Dismiss",
                None,
                Some(Message::Welcome(WelcomeMsg::StatusDismissed)),
                false,
                ICON_BTN_XS,
                Position::Top,
            )
        });
        col = col.push(toast(kind, s.text.clone(), action));
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
                Some(RETURN_KEY),
                p,
                Some(Message::App(AppMsg::GrantPermissionRequested)),
            ));
            actions = actions.push(welcome_reset_footer());
        }
        WelcomeState::AwaitingGrant | WelcomeState::Denied => {
            actions = actions.push(welcome_button(
                None,
                "Open System Settings",
                ButtonKind::Primary,
                ButtonSize::Lg,
                Some(RETURN_KEY),
                p,
                Some(Message::App(AppMsg::OpenPermissionSettingsRequested)),
            ));
            actions = actions.push(tooltip_with_key(
                welcome_button(
                    None,
                    "Restart Readshot",
                    ButtonKind::Secondary,
                    ButtonSize::Md,
                    None,
                    p,
                    Some(Message::App(AppMsg::RestartRequested)),
                ),
                "Restart Readshot",
                Some(WELCOME_RESTART_KEY),
                iced::widget::tooltip::Position::Top,
            ));
            actions = actions.push(welcome_reset_footer());
        }
        WelcomeState::Granted => {
            actions = actions.push(welcome_button(
                Some(AppIcon::Crosshair),
                "Capture Screen",
                ButtonKind::Primary,
                ButtonSize::Lg,
                (!state.capture_in_flight).then_some(RETURN_KEY),
                p,
                (!state.capture_in_flight)
                    .then_some(Message::Capture(CaptureMsg::OpenOverlayRequested)),
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
                        Some(Message::History(HistoryMsg::OpenRequested)),
                    ),
                    welcome_button(
                        Some(crate::icons::action::SETTINGS),
                        "Open Settings",
                        ButtonKind::Secondary,
                        ButtonSize::Md,
                        None,
                        p,
                        Some(Message::Settings(SettingsMsg::OpenRequested)),
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
    /// The Readshot app icon (permission states).
    App,
    /// Green circle with a check (granted).
    Done,
}

/// The Readshot brand tile as an image handle, built once so the image
/// is uploaded once rather than on every redraw.
fn brand_tile_handle() -> iced::widget::image::Handle {
    static HANDLE: std::sync::OnceLock<iced::widget::image::Handle> = std::sync::OnceLock::new();
    HANDLE
        .get_or_init(|| {
            iced::widget::image::Handle::from_bytes(super::window_settings::BRAND_TILE_PNG)
        })
        .clone()
}

/// Tile plus the title (and, for the permission states, the tagline).
fn welcome_brand(p: &'static Palette, tile: WelcomeTile) -> Element<'static, Message> {
    use crate::icons::AppIcon;
    const TILE: f32 = 64.0;
    let tile_el: Element<'static, Message> = match tile {
        // The real app icon (teal tile, brackets, lens), rendered from
        // packaging/macos/icon.svg at 128 px so it is sharp at 2x.
        WelcomeTile::App => iced::widget::image(brand_tile_handle())
            .width(Length::Fixed(TILE))
            .height(Length::Fixed(TILE))
            .into(),
        WelcomeTile::Done => {
            let fill = Color { a: 0.14, ..p.ok };
            container(crate::icons::icon(AppIcon::CheckCircle, p.ok, 32.0))
                .width(Length::Fixed(TILE))
                .height(Length::Fixed(TILE))
                .center_x(Length::Fixed(TILE))
                .center_y(Length::Fixed(TILE))
                .style(move |_| container::Style {
                    background: Some(fill.into()),
                    border: iced::Border {
                        radius: (TILE / 2.0).into(),
                        ..Default::default()
                    },
                    ..Default::default()
                })
                .into()
        }
    };
    let (title, tagline) = match tile {
        WelcomeTile::App => ("Readshot", Some("Capture, search, find again.")),
        WelcomeTile::Done => ("You’re all set.", None),
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

/// Width the Welcome reset footer needs on one line: the question
/// (≈ 300 px at 12 px) plus the Reset Permission button (≈ 116 px) and
/// the gap. Narrower than this, the button goes under the question so it
/// is never clipped (Welcome's 420 px minimum leaves ≈ 340 px).
pub(crate) const WELCOME_FOOTER_ROW_MIN: f32 = 430.0;

pub(crate) fn welcome_footer_stacks(width: f32) -> bool {
    width < WELCOME_FOOTER_ROW_MIN
}

fn welcome_reset_footer<'a>() -> Element<'a, Message> {
    responsive(|size| {
        let question = text("Already on in System Settings but still blocked?")
            .size(TYPE_CAPTION)
            .style(text_t3)
            .align_x(iced::alignment::Horizontal::Center);
        let reset = tooltip_with_key(
            button(text("Reset Permission").size(TYPE_CAPTION))
                .padding([2, 6])
                .style(|t, s| button_style(t, s, ButtonKind::Ghost))
                .on_press(Message::App(AppMsg::ResetPermissionRequested)),
            "Reset Permission",
            Some(WELCOME_RESET_PERMISSION_KEY),
            iced::widget::tooltip::Position::Top,
        );
        let footer: Element<'a, Message> = if welcome_footer_stacks(size.width) {
            column![question, reset]
                .spacing(SP1)
                .align_x(Alignment::Center)
                .into()
        } else {
            row![question, reset]
                .spacing(SP1)
                .align_y(Alignment::Center)
                .into()
        };
        container(footer).center_x(Length::Fill).into()
    })
    .height(Length::Shrink)
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

/// What [`stage_viewport_in_document`] needs about the editor stage.
pub(crate) struct StageViewport {
    /// The scrollable's offset (content px).
    pub offset: (f32, f32),
    /// The stage's size (screen px).
    pub available: (f32, f32),
    /// The scrolled content's size: the output, at least the stage.
    pub content: (f32, f32),
    /// The (beautified) output's size, centred in the content.
    pub output: (f32, f32),
    /// The image's top-left inside the output.
    pub pad: (f32, f32),
    /// Screen px per document px.
    pub scale: f32,
    /// The displayed image's top-left in document px (a crop's origin).
    pub image_offset: (f32, f32),
}

/// The stage scrollable's id: zoom and relayout pin its offset to the
/// one the editor records ([`pin_stage_offset`]).
pub(crate) const EDITOR_STAGE_ID: &str = "editor-stage";

/// `offset` clamped as the stage scrollable clamps it for `content` in an
/// `available` stage.
pub(crate) fn clamp_stage_offset(
    offset: (f32, f32),
    content: (f32, f32),
    available: (f32, f32),
) -> (f32, f32) {
    (
        offset.0.clamp(0.0, (content.0 - available.0).max(0.0)),
        offset.1.clamp(0.0, (content.1 - available.1).max(0.0)),
    )
}

/// Where the image sits in the stage's scrolled content at one zoom.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct StageFrame {
    /// Screen px per document px.
    pub scale: f32,
    /// The scrolled content: the output, at least the stage.
    pub content: (f32, f32),
    /// The (beautified) output, centred in the content.
    pub output: (f32, f32),
    /// The image's top-left inside the output.
    pub pad: (f32, f32),
}

impl StageFrame {
    /// As the editor view lays out an `image` px document in an
    /// `available` stage at `explicit_zoom` (`None`: fit).
    pub(crate) fn of(
        image: (u32, u32),
        beautify: &readshot_core::beautify::BeautifySettings,
        available: (f32, f32),
        explicit_zoom: Option<f32>,
    ) -> Self {
        let m = preview_metrics(
            image.0,
            image.1,
            beautify,
            iced::Size::new(available.0, available.1),
            explicit_zoom,
        );
        Self {
            scale: m.scale,
            content: (m.output_w.max(available.0), m.output_h.max(available.1)),
            output: (m.output_w, m.output_h),
            pad: if beautify.is_identity() {
                (0.0, 0.0)
            } else {
                (m.pad.left, m.pad.top)
            },
        }
    }

    /// The image's top-left in content px.
    fn image_origin(&self) -> (f32, f32) {
        (
            (self.content.0 - self.output.0) * 0.5 + self.pad.0,
            (self.content.1 - self.output.1) * 0.5 + self.pad.1,
        )
    }
}

/// The scroll offset after a zoom from `old` to `new` that keeps the
/// document point at the stage's centre where it is on screen (clamped as
/// the scrollable clamps it).
pub(crate) fn offset_keeping_center(
    offset: (f32, f32),
    available: (f32, f32),
    old: &StageFrame,
    new: &StageFrame,
) -> (f32, f32) {
    if !(old.scale.is_finite() && old.scale > 0.0 && new.scale.is_finite()) {
        return clamp_stage_offset(offset, new.content, available);
    }
    let (ox, oy) = clamp_stage_offset(offset, old.content, available);
    let (ix, iy) = old.image_origin();
    let cx = (ox + available.0 * 0.5 - ix) / old.scale;
    let cy = (oy + available.1 * 0.5 - iy) / old.scale;
    let (nx, ny) = new.image_origin();
    clamp_stage_offset(
        (
            cx * new.scale + nx - available.0 * 0.5,
            cy * new.scale + ny - available.1 * 0.5,
        ),
        new.content,
        available,
    )
}

/// Zoom `ed` to `next`, keeping the point at the stage's centre fixed, and
/// pin the scrollable to the offset that does it.
pub(crate) fn zoom_keeping_center(
    ed: &mut EditorSession,
    next: crate::editor::EditorZoom,
) -> Task<Message> {
    let available = ed.stage_size.get();
    if available.0 > 0.0 && available.1 > 0.0 {
        let image = ed.effective_image_size();
        let beautify = ed.model.beautify();
        let old = StageFrame::of(image, &beautify, available, ed.zoom.explicit_scale());
        let new = StageFrame::of(image, &beautify, available, next.explicit_scale());
        ed.stage_offset.set(offset_keeping_center(
            ed.stage_offset.get(),
            available,
            &old,
            &new,
        ));
    }
    ed.zoom = next;
    pin_stage_offset(ed)
}

/// After a zoom or relayout (no scroll event): set the stage scrollable to
/// the offset the editor recorded (clamped to what was on screen by the
/// last `view`). The scrollable otherwise keeps an older, larger offset it
/// had clamped away and shows it again once the content grows, while the
/// recorded offset (where a new magnifier bubble may go) still says what
/// the last scroll event said.
pub(crate) fn pin_stage_offset(ed: &EditorSession) -> Task<Message> {
    let (x, y) = ed.stage_offset.get();
    iced::widget::operation::scroll_to(
        iced::widget::Id::new(EDITOR_STAGE_ID),
        iced::widget::operation::AbsoluteOffset {
            x: Some(x),
            y: Some(y),
        },
    )
}

/// The part of the document the stage shows (document px): the stage
/// rect at the scroll offset (clamped as the scrollable clamps it), mapped
/// through the image's place in the content; the canvas cuts it to the
/// image. `None` when the scale or stage size is unusable.
pub(crate) fn stage_viewport_in_document(v: StageViewport) -> Option<readshot_core::RectLike> {
    if !(v.scale.is_finite() && v.scale > 0.0) {
        return None;
    }
    let (ox, oy) = clamp_stage_offset(v.offset, v.content, v.available);
    // The image's top-left in content px.
    let ix = (v.content.0 - v.output.0) * 0.5 + v.pad.0;
    let iy = (v.content.1 - v.output.1) * 0.5 + v.pad.1;
    let x0 = (ox - ix) / v.scale + v.image_offset.0;
    let y0 = (oy - iy) / v.scale + v.image_offset.1;
    let w = v.available.0 / v.scale;
    let h = v.available.1 / v.scale;
    (w > 0.0 && h > 0.0).then(|| readshot_core::RectLike::new(x0, y0, w, h))
}

#[cfg(test)]
mod tests {
    use super::ocr_language_display_name;
    use super::{ellipsize, history_title_budget, HISTORY_LIST_WIDTH};
    use super::{
        highlight_spans, history_age_label, history_first_line, history_selected_border,
        history_selected_fill, history_word_count, uninstall_status_help,
    };
    use crate::runtime::{DARK, LIGHT};

    #[test]
    fn the_stage_viewport_maps_scroll_and_zoom_into_document_px() {
        use super::{stage_viewport_in_document, StageViewport};
        use readshot_core::RectLike;
        // A 2000 × 1000 image at 2× in an 800 × 600 stage, scrolled to
        // (1000, 400): document x 500–900, y 200–500.
        let v = |offset, pad, image_offset| StageViewport {
            offset,
            available: (800.0, 600.0),
            content: (4000.0 + 2.0 * pad, 2000.0 + 2.0 * pad),
            output: (4000.0 + 2.0 * pad, 2000.0 + 2.0 * pad),
            pad: (pad, pad),
            scale: 2.0,
            image_offset,
        };
        assert_eq!(
            stage_viewport_in_document(v((1000.0, 400.0), 0.0, (0.0, 0.0))),
            Some(RectLike::new(500.0, 200.0, 400.0, 300.0))
        );
        // Beautify padding and a crop's origin shift it; an offset past
        // the end is clamped as the scrollable clamps it.
        assert_eq!(
            stage_viewport_in_document(v((1e9, 0.0), 100.0, (10.0, 20.0))),
            Some(RectLike::new(
                10.0 + (3400.0 - 100.0) / 2.0,
                20.0 - 50.0,
                400.0,
                300.0
            ))
        );
        // Not scrolling: the output centred in a bigger stage.
        let fit = StageViewport {
            offset: (0.0, 0.0),
            available: (800.0, 600.0),
            content: (800.0, 600.0),
            output: (400.0, 200.0),
            pad: (0.0, 0.0),
            scale: 0.2,
            image_offset: (0.0, 0.0),
        };
        assert_eq!(
            stage_viewport_in_document(fit),
            Some(RectLike::new(-1000.0, -1000.0, 4000.0, 3000.0))
        );
    }

    #[test]
    fn a_zoom_without_a_scroll_event_keeps_the_viewport_on_screen() {
        use super::{clamp_stage_offset, stage_viewport_in_document, StageViewport};
        use readshot_core::RectLike;
        // Scrolled to the far corner of a 2000 × 1000 image at 2× ...
        let at = |offset, scale: f32| StageViewport {
            offset,
            available: (800.0, 600.0),
            content: (2000.0 * scale, 1000.0 * scale),
            output: (2000.0 * scale, 1000.0 * scale),
            pad: (0.0, 0.0),
            scale,
            image_offset: (0.0, 0.0),
        };
        let far = (3200.0, 1400.0);
        assert_eq!(
            stage_viewport_in_document(at(far, 2.0)),
            Some(RectLike::new(1600.0, 700.0, 400.0, 300.0))
        );
        // ... then zoomed out to 0.5×: the stage shows the clamped offset,
        // which the view records (and the zoom pins the scrollable to).
        let recorded = clamp_stage_offset(far, (1000.0, 500.0), (800.0, 600.0));
        assert_eq!(recorded, (200.0, 0.0));
        let shown = stage_viewport_in_document(at(recorded, 0.5)).unwrap();
        assert_eq!(shown, RectLike::new(400.0, 0.0, 1600.0, 1200.0));
        // Zoomed back in, the viewport (where a new bubble may go) is the
        // part of the image the pinned scrollable shows, inside the image.
        let back = stage_viewport_in_document(at(recorded, 2.0)).unwrap();
        assert_eq!(back, RectLike::new(100.0, 0.0, 400.0, 300.0));
        assert!(back.x + back.width <= 2000.0 && back.y + back.height <= 1000.0);
    }

    #[test]
    fn a_zoom_keeps_the_point_at_the_stage_centre_fixed() {
        use super::{offset_keeping_center, StageFrame};
        // A 1000 × 500 image in an 800 × 600 stage, no beautify.
        let at = |scale: f32| StageFrame {
            scale,
            content: ((1000.0 * scale).max(800.0), (500.0 * scale).max(600.0)),
            output: (1000.0 * scale, 500.0 * scale),
            pad: (0.0, 0.0),
        };
        // At 1×, scrolled 100 px right: the centre shows document
        // (500, 250) (the image is centred vertically, 50 px down).
        let offset = offset_keeping_center((100.0, 0.0), (800.0, 600.0), &at(1.0), &at(2.0));
        // At 2× that point is at content (1000, 500): the centre stays on it.
        assert_eq!(offset, (600.0, 200.0));
        // Back to 1×: the same point, the same offset as before.
        assert_eq!(
            offset_keeping_center(offset, (800.0, 600.0), &at(2.0), &at(1.0)),
            (100.0, 0.0)
        );
        // Zoomed out to fit nothing scrolls.
        assert_eq!(
            offset_keeping_center(offset, (800.0, 600.0), &at(2.0), &at(0.5)),
            (0.0, 0.0)
        );
        // Matches what the view lays out.
        let b = readshot_core::beautify::BeautifySettings::default();
        assert_eq!(
            StageFrame::of((1000, 500), &b, (800.0, 600.0), Some(2.0)),
            at(2.0)
        );
    }

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
    fn long_list_titles_end_in_an_ellipsis() {
        assert_eq!(ellipsize("short", 10), "short");
        assert_eq!(ellipsize("exactly10!", 10), "exactly10!");
        assert_eq!(
            ellipsize("Quarterly overview of revenue", 12),
            "Quarterly o…"
        );
        assert_eq!(ellipsize("Quarterly overview", 12).chars().count(), 12);
        // A cut never leaves a space before the ellipsis.
        assert_eq!(ellipsize("Quarterly overview", 11), "Quarterly…");
        // The narrowest list (200 px) still shows a few words.
        assert!(history_title_budget(200.0) >= 4);
        assert!(history_title_budget(HISTORY_LIST_WIDTH) > history_title_budget(200.0));
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

    /// B#5 (fix round 1): the Blur and Pixelate hints warn that they can
    /// be reversed and point at what works on every platform, a filled
    /// Rectangle (the Text tab's Solid block is macOS only, and only shows
    /// when something sensitive was found).
    #[test]
    fn the_weak_tool_hints_point_at_a_filled_rectangle() {
        use readshot_ui::editor::ToolState as T;
        assert_eq!(
            super::editor_compact_hint(T::Blur),
            "Drag over an area to blur it. Blur can sometimes be reversed; for passwords, draw a filled Rectangle instead."
        );
        assert_eq!(
            super::editor_compact_hint(T::Pixelate),
            "Drag over an area to pixelate it. Pixelation can sometimes be reversed; for passwords, draw a filled Rectangle instead."
        );
    }

    /// A#4: only an uninstall Error is shown as one.
    #[test]
    fn only_an_uninstall_error_reads_as_an_error() {
        use crate::editor::StatusTone as T;
        assert_eq!(
            uninstall_status_help(T::Error, "Couldn’t remove it."),
            ("Couldn’t remove it.".to_string(), true)
        );
        assert_eq!(
            uninstall_status_help(T::Progress, "Uninstalling Readshot…"),
            ("• Uninstalling Readshot…".to_string(), false)
        );
        assert_eq!(
            uninstall_status_help(T::Warning, "Done, but one step was skipped."),
            ("! Done, but one step was skipped.".to_string(), false)
        );
        assert_eq!(
            uninstall_status_help(T::Success, "Readshot was uninstalled."),
            ("✓ Readshot was uninstalled.".to_string(), false)
        );
    }

    #[test]
    fn history_selection_colors_read_tokens() {
        assert_eq!(history_selected_fill(&DARK), DARK.acc_tint);
        assert_eq!(history_selected_fill(&LIGHT), LIGHT.acc_tint);
        // A#5: the outline is the only mark of where ↑/↓ and ⌦ act, so
        // it meets 3:1 against the list and the row's own tint.
        for p in [&DARK, &LIGHT] {
            let b = history_selected_border(p);
            let tint = crate::runtime::tokens::over(history_selected_fill(p), p.bg);
            for bg in [p.bg, tint] {
                let r = crate::runtime::tokens::contrast_ratio(b, bg);
                assert!(r >= 3.0, "selected row outline on {bg:?} is {r:.2}:1");
            }
        }
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
        let (label, key, msg) = super::toast_action_parts(A::UndoRedaction { generation: 0 });
        assert_eq!((label, key), ("Undo", Some(crate::runtime::UNDO_KEY)));
        assert!(matches!(
            msg,
            crate::app::Message::Editor(crate::app::EditorMsg::Toolbar(
                readshot_ui::ToolbarMessage::Undo
            ))
        ));
        let (label, _, msg) = super::toast_action_parts(A::RetryRecognition);
        assert_eq!(label, "Retry");
        assert!(matches!(
            msg,
            crate::app::Message::Editor(crate::app::EditorMsg::TextPanel(
                crate::app::TextPanelMsg::Recognize
            ))
        ));
    }
}

/// The part of a shown render that falls inside the present frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ShownPart {
    /// Its pixels in the shown image.
    pub region: iced::Rectangle<u32>,
    /// Where they go in the present frame (document px).
    pub at: (f32, f32),
    /// Their size (document px).
    pub size: (f32, f32),
}

/// Where the shown render sits in the present frame (`offset`, `size`):
/// `None` when it is exactly that frame (drawn as is); `Some(None)` when
/// none of it is inside; else the visible part. A render of another
/// document can't be placed: drawn as is (geometry changes land composed,
/// so this doesn't happen).
pub(crate) fn shown_part_of_frame(
    ed: &crate::editor::EditorSession,
    offset: (f32, f32),
    size: (u32, u32),
) -> Option<Option<ShownPart>> {
    let shown = &ed.shown;
    if !std::sync::Arc::ptr_eq(&shown.base, &ed.model.shared_base())
        || (shown.offset == offset && shown.size == size)
    {
        return None;
    }
    // Document px → the shown image's px (a placeholder is scaled down).
    let k = shown.scale;
    let x0 = shown.offset.0.max(offset.0);
    let y0 = shown.offset.1.max(offset.1);
    let x1 = (shown.offset.0 + shown.size.0 as f32).min(offset.0 + size.0 as f32);
    let y1 = (shown.offset.1 + shown.size.1 as f32).min(offset.1 + size.1 as f32);
    if x1 - x0 < 1.0 || y1 - y0 < 1.0 {
        return Some(None);
    }
    let (iw, ih) = (
        ((shown.size.0 as f32) * k).round() as u32,
        ((shown.size.1 as f32) * k).round() as u32,
    );
    let rx = ((x0 - shown.offset.0) * k).round().max(0.0) as u32;
    let ry = ((y0 - shown.offset.1) * k).round().max(0.0) as u32;
    let rw = (((x1 - x0) * k).round() as u32)
        .min(iw.saturating_sub(rx))
        .max(1);
    let rh = (((y1 - y0) * k).round() as u32)
        .min(ih.saturating_sub(ry))
        .max(1);
    Some(Some(ShownPart {
        region: iced::Rectangle {
            x: rx,
            y: ry,
            width: rw,
            height: rh,
        },
        at: (x0 - offset.0, y0 - offset.1),
        size: (x1 - x0, y1 - y0),
    }))
}

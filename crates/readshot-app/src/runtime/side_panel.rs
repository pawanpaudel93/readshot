//! Editor side panel (spec §2.3): right, 300 px, tabs (Text,
//! Beautify), hide/show ⌥⌘P (Ctrl+Shift+P off macOS), open state and
//! tab remembered.
//! Docks and reflows the canvas when the window is wide enough. Below
//! `PANEL_DOCK_MIN_WINDOW` it collapses to the title-row "Show panel"
//! button, which opens a temporary overlay (esc / the panel key / click outside
//! close it), so the bars keep their one-row, unobscured 720 px layout.

use super::*;

use iced::widget::tooltip::Position;
use iced::widget::{button, column, container, row, text, Space};
use iced::{Alignment, Border, Color, Element, Length, Shadow, Theme, Vector};

use crate::app::{App, Message};
use crate::editor::{EditorSession, PanelTab};
use crate::icons::AppIcon;

pub(crate) const PANEL_WIDTH: f32 = 300.0;
/// Narrowest window that docks the panel: the editor's 720 px design
/// minimum for the canvas plus the panel.
pub(crate) const PANEL_DOCK_MIN_WINDOW: f32 = 720.0 + PANEL_WIDTH;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PanelMode {
    /// Wide window, panel closed: title row shows "Show panel".
    Hidden,
    /// Beside the canvas; the canvas and its bars reflow.
    Docked,
    /// Narrow window: only the title-row "Show panel" button.
    Collapsed,
    /// Narrow window, opened on demand: floats over the canvas's right
    /// edge until esc, the panel key (⌥⌘P / Ctrl+Shift+P) or a click
    /// outside.
    Overlay,
}

pub(crate) fn panel_mode(open: bool, overlay_open: bool, window_width: f32) -> PanelMode {
    if window_width >= PANEL_DOCK_MIN_WINDOW {
        if open || overlay_open {
            PanelMode::Docked
        } else {
            PanelMode::Hidden
        }
    } else if overlay_open {
        PanelMode::Overlay
    } else {
        PanelMode::Collapsed
    }
}

pub(crate) fn editor_panel_mode(state: &App, ed: &EditorSession) -> PanelMode {
    panel_mode(
        state.preferences.editor_panel_open,
        ed.panel_overlay_open,
        ed.window_width,
    )
}

fn tab_icon(tab: PanelTab) -> AppIcon {
    match tab {
        PanelTab::Text => AppIcon::ScanText,
        PanelTab::Beautify => AppIcon::Sparkle,
    }
}

/// Tabs follow the mockup's `.ptab`: neutral hover fill for the current
/// tab (a tab is navigation, not a selection, so no teal).
fn panel_tab_style(theme: &Theme, status: button::Status, selected: bool) -> button::Style {
    let p = palette_for(theme);
    let lit = selected || matches!(status, button::Status::Hovered);
    button::Style {
        background: Some(if lit { p.hover } else { Color::TRANSPARENT }.into()),
        text_color: if selected { p.t1 } else { p.t2 },
        border: Border {
            radius: R_BUTTON.into(),
            ..Border::default()
        },
        ..button::Style::default()
    }
}

/// The current tab is also set semibold, so it is not told apart by
/// fill and text colour alone (WCAG 1.4.1).
pub(crate) fn panel_tab_font(selected: bool) -> iced::Font {
    iced::Font {
        weight: if selected {
            iced::font::Weight::Semibold
        } else {
            iced::font::Weight::Normal
        },
        ..iced::Font::DEFAULT
    }
}

pub(crate) fn divider_style(theme: &Theme) -> container::Style {
    container::Style {
        background: Some(palette_for(theme).border_subtle.into()),
        ..container::Style::default()
    }
}

fn panel_style(theme: &Theme, overlay: bool) -> container::Style {
    let p = palette_for(theme);
    container::Style {
        background: Some(p.surface.into()),
        shadow: if overlay {
            Shadow {
                color: p.shadow,
                offset: Vector::new(-8.0, 0.0),
                blur_radius: 24.0,
            }
        } else {
            Shadow::default()
        },
        ..container::Style::default()
    }
}

/// The whole panel: tab header (with Hide panel, ⌥⌘P / Ctrl+Shift+P) over the current
/// tab's body.
pub(crate) fn side_panel<'a>(
    state: &'a App,
    ed: &'a EditorSession,
    p: &'static Palette,
    overlay: bool,
) -> Element<'a, Message> {
    let current = PanelTab::from_id(&state.preferences.editor_panel_tab);
    let mut tabs = row![].spacing(SP1).align_y(Alignment::Center);
    for tab in PanelTab::ALL {
        let selected = tab == current;
        let label = row![
            crate::icons::icon(tab_icon(tab), if selected { p.t1 } else { p.t2 }, ICON_SIZE),
            text(tab.label())
                .size(TYPE_BODY)
                .font(panel_tab_font(selected)),
        ]
        .spacing(6)
        .align_y(Alignment::Center);
        let mut b = button(container(label).center_y(Length::Fill))
            .height(Length::Fixed(BTN_SM))
            .padding([0.0, 10.0])
            .style(move |t, s| panel_tab_style(t, s, selected));
        if !selected {
            b = b.on_press(Message::EditorPanelTabSelected(tab));
        }
        tabs = tabs.push(b);
    }
    let tabs_key = match current {
        PanelTab::Text => super::key_focus::KEY_BEAUTIFY_TAB,
        PanelTab::Beautify => super::key_focus::KEY_TEXT_TAB,
    };
    let tabs = focus_ring(
        tooltip_with_key(
            tabs,
            format!("Switch to {}", super::key_focus::other_tab_label(current)),
            Some(super::key_focus::key_label(tabs_key)),
            Position::Bottom,
        ),
        super::key_focus::is_focused(
            ed,
            crate::editor::FocusZone::Panel,
            crate::editor::FocusItem::PanelTabs,
        ),
    );
    let header = row![
        tabs,
        Space::new().width(Length::Fill),
        icon_button(
            AppIcon::Sidebar,
            p.t2,
            "Hide Panel",
            Some(super::key_focus::key_label(super::key_focus::KEY_PANEL)),
            Some(Message::EditorPanelToggled),
            false,
            ICON_BTN_SM,
            Position::Bottom,
        ),
    ]
    .align_y(Alignment::Center)
    .padding([0.0, SP2])
    .height(Length::Fixed(TITLEBAR_HEIGHT));
    let body: Element<'a, Message> = match current {
        PanelTab::Text => text_tab(state, ed, p),
        PanelTab::Beautify => super::beautify_panel::beautify_tab(state, ed, p),
    };
    let hline =
        container(Space::new().width(Length::Fill).height(Length::Fixed(1.0))).style(divider_style);
    let vline =
        container(Space::new().width(Length::Fixed(1.0)).height(Length::Fill)).style(divider_style);
    container(row![
        vline,
        column![header, hline, body]
            .width(Length::Fill)
            .height(Length::Fill),
    ])
    .width(Length::Fixed(PANEL_WIDTH))
    .height(Length::Fill)
    .style(move |theme| panel_style(theme, overlay))
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panel_docks_from_1020_px_and_collapses_below() {
        assert_eq!(panel_mode(false, false, 1400.0), PanelMode::Hidden);
        assert_eq!(panel_mode(true, false, 1100.0), PanelMode::Docked);
        assert_eq!(panel_mode(true, false, 1020.0), PanelMode::Docked);
        assert_eq!(panel_mode(false, true, 1100.0), PanelMode::Docked);
        // Narrow: collapsed to the title-row button whatever the
        // preference; the overlay only while explicitly opened.
        assert_eq!(panel_mode(true, false, 1019.0), PanelMode::Collapsed);
        assert_eq!(panel_mode(false, false, 720.0), PanelMode::Collapsed);
        assert_eq!(panel_mode(true, true, 720.0), PanelMode::Overlay);
        assert_eq!(panel_mode(false, true, 720.0), PanelMode::Overlay);
    }

    #[test]
    fn the_default_editor_window_docks_the_panel() {
        assert_eq!(
            panel_mode(true, false, crate::editor::DEFAULT_WINDOW_WIDTH),
            PanelMode::Docked
        );
    }

    #[test]
    fn the_current_tab_is_neutral_not_teal() {
        let s = panel_tab_style(
            &readshot_theme_for(ThemeMode::Dark),
            button::Status::Active,
            true,
        );
        assert_eq!(s.background, Some(DARK.hover.into()));
        assert_eq!(s.text_color, DARK.t1);
    }

    #[test]
    fn the_current_tab_is_also_marked_by_weight() {
        assert_eq!(panel_tab_font(true).weight, iced::font::Weight::Semibold);
        assert_eq!(panel_tab_font(false).weight, iced::font::Weight::Normal);
    }
}

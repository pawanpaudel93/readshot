//! Beautify tab (spec §3.9): presets, background, padding, corner radius,
//! shadow, canvas size, Auto Balance, Save Preset. Every change is an
//! undo step on the editor model; sliders preview live and commit once
//! on release.

use super::*;

use iced::widget::tooltip::Position;
use iced::widget::{column, container, row, text, text_input, toggler, Space};
use iced::{Alignment, Element, Length, Task, Theme};
use readshot_core::beautify::{
    Background, BeautifyEdit, BuiltinPreset, CanvasSize, GRADIENTS, MAX_CORNER_RADIUS, MAX_PADDING,
    MAX_SHADOW_LEVEL, SOLIDS,
};

use crate::app::{App, Message};
use crate::editor::{EditorSession, EyedropperState, EyedropperTarget};
use crate::icons::AppIcon;

fn set(ed: &mut EditorSession, s: readshot_core::beautify::BeautifySettings, what: &str) {
    if ed.model.set_beautify(s) {
        ed.clear_discard_confirmation();
        ed.set_status(format!("{what}. ⌘Z to undo."));
    }
}

pub(crate) fn handle_beautify_message(state: &mut App, message: Message) -> Task<Message> {
    if let Message::EditorBeautifySavePreset = message {
        let Some(s) = state.editor.as_ref().map(|ed| ed.model.beautify()) else {
            return Task::none();
        };
        let saved = state.save_beautify_preset(s);
        if let Some(ed) = state.editor.as_mut() {
            ed.set_status(match saved {
                Some(name) => format!("Saved {name}."),
                None => "You can keep up to 12 presets. Remove one to save another.".into(),
            });
        }
        return Task::none();
    }
    if let Message::EditorBeautifyDeletePreset(i) = message {
        state.delete_beautify_preset(i);
        return Task::none();
    }
    let saved = state.preferences.editor_beautify_presets.clone();
    let Some(ed) = state.editor.as_mut() else {
        return Task::none();
    };
    if ed.busy {
        return Task::none();
    }
    match message {
        Message::EditorBeautifySavedPresetApplied(i) => {
            if let Some(p) = saved.get(i) {
                cancel_editor_previews(ed);
                set(ed, p.settings.clamped(), &format!("Applied {}", p.name));
            }
        }
        Message::EditorBeautifyEdit(edit) => {
            cancel_editor_previews(ed);
            let mut s = ed.model.beautify();
            s.apply(edit);
            set(ed, s, "Beautify updated");
        }
        Message::EditorBeautifySliderPreview(edit) => {
            if ed.snapshot_drag_baseline.is_none() {
                ed.snapshot_drag_baseline = Some(ed.model.snapshot());
            }
            let mut next = ed.snapshot_drag_baseline.clone().expect("set above");
            next.doc.beautify.apply(edit);
            // A refused preview (invalid geometry) changes nothing.
            let _ = ed.model.preview_snapshot(next);
        }
        Message::EditorBeautifyHexChanged(raw) => ed.beautify_hex_draft = raw,
        Message::EditorBeautifyHexSubmitted => {
            match readshot_core::Rgba::from_hex(&ed.beautify_hex_draft) {
                Some(c) => {
                    cancel_editor_previews(ed);
                    let mut s = ed.model.beautify();
                    s.apply(BeautifyEdit::Background(Background::Solid {
                        rgba: c.to_rgba8(),
                    }));
                    set(ed, s, &format!("Background set to {}", c.to_hex()));
                }
                None => ed.set_status("Couldn't read that color. Use a hex value like #4F46E5."),
            }
        }
        Message::EditorBeautifyPickBackground => {
            if ed.canvas_drag_active || ed.move_drag.is_some() {
                return Task::none();
            }
            close_editor_popover(ed);
            ed.eyedropper = Some(EyedropperState {
                loupe: None,
                target: EyedropperTarget::BeautifyBackground,
            });
        }
        other => unreachable!("non-beautify message routed to the beautify handler: {other:?}"),
    }
    Task::none()
}

fn section<'a>(title: &'static str, body: Element<'a, Message>) -> Element<'a, Message> {
    column![text(title).size(TYPE_CAPTION).style(text_t3), body]
        .spacing(SP1)
        .into()
}

fn swatch<'a>(
    bg: Background,
    selected: bool,
    busy: bool,
    tip: &'static str,
) -> Element<'a, Message> {
    let fill = container(
        Space::new()
            .width(Length::Fixed(20.0))
            .height(Length::Fixed(20.0)),
    )
    .style(move |theme: &Theme| container::Style {
        background: background_color_or_gradient(bg).or(Some(palette_for(theme).hover.into())),
        border: iced::Border {
            radius: 4.0.into(),
            width: 1.0,
            color: palette_for(theme).border_subtle,
        },
        ..Default::default()
    });
    let b = segment_button(
        fill.into(),
        selected,
        (!busy).then_some(Message::EditorBeautifyEdit(BeautifyEdit::Background(bg))),
    );
    tooltip_with_key(b, tip, None, Position::Bottom)
}

fn slider_row<'a>(
    label: &'static str,
    max: u32,
    value: u32,
    readout: String,
    busy: bool,
    make: fn(u32) -> BeautifyEdit,
) -> Element<'a, Message> {
    let mut r = row![text(label)
        .size(TYPE_CAPTION)
        .style(text_t2)
        .width(Length::Fixed(72.0))]
    .spacing(SP1)
    .align_y(Alignment::Center);
    if !busy {
        r = r.push(
            iced::widget::slider(0.0..=max as f32, value as f32, move |v| {
                Message::EditorBeautifySliderPreview(make(v.round() as u32))
            })
            .step(1.0)
            .on_release(Message::EditorSnapshotSliderCommit)
            .width(Length::Fill),
        );
    }
    r.push(
        text(readout)
            .font(mono_font())
            .size(TYPE_CAPTION)
            .style(text_t2)
            .width(Length::Fixed(40.0)),
    )
    .into()
}

pub(crate) fn beautify_tab<'a>(
    state: &'a App,
    ed: &'a EditorSession,
    p: &'static Palette,
) -> Element<'a, Message> {
    let s = ed.model.beautify();
    let busy = ed.busy;
    let current = s.matching_builtin();
    // Presets: built-ins, then saved ones (each with a remove ×).
    let mut presets = row![].spacing(SP1);
    for preset in BuiltinPreset::ALL {
        presets = presets.push(segment_button(
            text(preset.label()).size(ButtonSize::Xs.text_size()).into(),
            current == Some(preset),
            (!busy).then_some(Message::EditorBeautifyPreset(preset)),
        ));
    }
    for (i, saved) in state.preferences.editor_beautify_presets.iter().enumerate() {
        presets = presets.push(
            row![
                segment_button(
                    text(saved.name.clone())
                        .size(ButtonSize::Xs.text_size())
                        .into(),
                    current.is_none() && saved.settings.clamped() == s,
                    (!busy).then_some(Message::EditorBeautifySavedPresetApplied(i)),
                ),
                icon_button(
                    AppIcon::Close,
                    p.t3,
                    "Remove preset",
                    None,
                    (!busy).then_some(Message::EditorBeautifyDeletePreset(i)),
                    false,
                    ICON_BTN_XS,
                    Position::Bottom,
                ),
            ]
            .align_y(Alignment::Center),
        );
    }
    // Background: transparent, 5 gradients, 6 solids, custom hex, eyedropper.
    let mut bg_row = row![swatch(
        Background::Transparent,
        s.background == Background::Transparent,
        busy,
        "Transparent"
    )]
    .spacing(SP1);
    for g in GRADIENTS {
        let bg = Background::Gradient { gradient: g };
        bg_row = bg_row.push(swatch(bg, s.background == bg, busy, "Gradient"));
    }
    for c in SOLIDS {
        let bg = Background::Solid { rgba: c };
        bg_row = bg_row.push(swatch(bg, s.background == bg, busy, "Solid color"));
    }
    let custom = row![
        text_input("#RRGGBB", &ed.beautify_hex_draft)
            .on_input(Message::EditorBeautifyHexChanged)
            .on_submit(Message::EditorBeautifyHexSubmitted)
            .size(TYPE_CAPTION)
            .width(Length::Fixed(96.0)),
        icon_button(
            AppIcon::Dropper,
            p.t2,
            "Pick from image",
            Some("I"),
            (!busy).then_some(Message::EditorBeautifyPickBackground),
            false,
            ICON_BTN_SM,
            Position::Bottom,
        ),
    ]
    .spacing(SP1)
    .align_y(Alignment::Center);
    let canvas = text_segment(&CanvasSize::ALL.map(|c| (c.label(), c)), s.canvas, |c| {
        (!busy).then_some(Message::EditorBeautifyEdit(BeautifyEdit::Canvas(c)))
    });
    let balance = toggler(s.balance)
        .label("Auto Balance (center)")
        .text_size(TYPE_CAPTION)
        .on_toggle_maybe(
            (!busy).then_some(|v| Message::EditorBeautifyEdit(BeautifyEdit::Balance(v))),
        );
    let save = bar_button(
        text("Save Preset").size(ButtonSize::Sm.text_size()).into(),
        ButtonKind::Secondary,
        ButtonSize::Sm,
        (!busy).then_some(Message::EditorBeautifySavePreset),
    );
    let level = usize::from(s.shadow_level());
    let body = column![
        section("Presets", presets.wrap().vertical_spacing(SP1).into()),
        section(
            "Background",
            column![bg_row.wrap().vertical_spacing(SP1), custom]
                .spacing(SP1)
                .into()
        ),
        slider_row(
            "Padding",
            MAX_PADDING,
            s.padding,
            format!("{}", s.padding),
            busy,
            BeautifyEdit::Padding
        ),
        slider_row(
            "Corners",
            MAX_CORNER_RADIUS,
            s.corner_radius,
            format!("{}", s.corner_radius),
            busy,
            BeautifyEdit::CornerRadius
        ),
        slider_row(
            "Shadow",
            u32::from(MAX_SHADOW_LEVEL),
            level as u32,
            ["None", "Subtle", "Soft", "Medium", "Strong"][level].to_string(),
            busy,
            |v| BeautifyEdit::ShadowLevel(v as u8)
        ),
        section("Canvas size", canvas),
        balance,
        row![Space::new().width(Length::Fill), save],
    ]
    .spacing(SP3)
    .padding(SP3);
    iced::widget::scrollable(body).height(Length::Fill).into()
}

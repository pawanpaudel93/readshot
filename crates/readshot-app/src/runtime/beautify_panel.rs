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
    MAX_SAVED_PRESETS, MAX_SHADOW_LEVEL, SOLIDS,
};

use crate::app::{App, Message};
use crate::editor::{EditorSession, EyedropperState, EyedropperTarget};
use crate::icons::AppIcon;

fn set(ed: &mut EditorSession, s: readshot_core::beautify::BeautifySettings, what: &str) {
    if ed.model.set_beautify(s) {
        ed.clear_discard_confirmation();
        ed.set_status(format!("{what}. {UNDO_HINT}"));
    }
}

pub(crate) fn handle_beautify_message(state: &mut App, message: Message) -> Task<Message> {
    if let Message::EditorBeautifySavePreset = message {
        let Some(s) = state.editor.as_ref().map(|ed| ed.model.beautify()) else {
            return Task::none();
        };
        let saved = state.save_beautify_preset(s);
        if let Some(ed) = state.editor.as_mut() {
            match saved {
                Some(name) => ed.set_status(format!("Saved {name}.")),
                None => ed.set_warning(format!(
                    "You can keep up to {MAX_SAVED_PRESETS} presets. Remove one to save another."
                )),
            }
        }
        return Task::none();
    }
    if let Message::EditorBeautifyDeletePreset(i) = message {
        // Removing is immediate and persisted, so the toast offers Undo.
        if let Some(removed) = state.delete_beautify_preset(i) {
            if let Some(ed) = state.editor.as_mut() {
                let status = format!("Removed {}.", removed.name);
                ed.set_status_with_action(status, crate::editor::EditorToastAction::RestorePreset);
                ed.removed_beautify_preset = Some((i, removed));
            }
        }
        return Task::none();
    }
    if let Message::EditorBeautifyRestorePreset = message {
        let pending = state.editor.as_mut().and_then(|ed| {
            (ed.visible_toast_action() == Some(crate::editor::EditorToastAction::RestorePreset))
                .then(|| ed.removed_beautify_preset.take())
                .flatten()
        });
        if let Some((i, preset)) = pending {
            let name = preset.name.clone();
            let restored = state.restore_beautify_preset(i, preset);
            if let Some(ed) = state.editor.as_mut() {
                if restored {
                    ed.set_status(format!("Restored {name}."));
                } else {
                    ed.set_status(format!(
                        "Couldn't restore {name}: you can keep up to {MAX_SAVED_PRESETS} presets."
                    ));
                }
            }
        }
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
                ed.snapshot_drag_cancelled = false;
                set(ed, p.settings.clamped(), &format!("Applied {}", p.name));
            }
        }
        Message::EditorBeautifyEdit(edit) => {
            cancel_editor_previews(ed);
            ed.snapshot_drag_cancelled = false;
            let mut s = ed.model.beautify();
            s.apply(edit);
            set(ed, s, "Beautify updated");
        }
        Message::EditorBeautifySliderPreview(edit) => {
            // The slider is still held after Esc / undo cancelled its drag:
            // ticks until release must not start a new preview.
            if ed.snapshot_drag_cancelled {
                return Task::none();
            }
            // A released gap drag still composing is kept first; this is
            // a new drag with its own baseline.
            if ed.gap_commit_on_land {
                cancel_editor_previews(ed);
            }
            if ed.snapshot_drag_baseline.is_none() {
                // Settle any other open preview before taking the baseline.
                cancel_editor_previews(ed);
                ed.snapshot_drag_baseline = Some(ed.model.snapshot());
            }
            let mut next = ed.snapshot_drag_baseline.clone().expect("set above");
            next.doc.beautify.apply(edit);
            // A refused preview (invalid geometry) changes nothing.
            let _ = ed.model.preview_snapshot(next);
        }
        Message::EditorBeautifyHexChanged(raw) => ed.beautify_hex_draft = Some(raw),
        Message::EditorBeautifyHexSubmitted => {
            match readshot_core::Rgba::from_hex(&ed.beautify_hex_shown()) {
                Some(c) => {
                    // Applied: the field follows the background again.
                    ed.beautify_hex_draft = None;
                    cancel_editor_previews(ed);
                    ed.snapshot_drag_cancelled = false;
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
            // Below the dock width the panel floats over the canvas; the
            // first canvas click must pick, not just dismiss it.
            close_panel_overlay(ed);
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

/// The shadow slider's readout, one label per level.
const SHADOW_LABELS: [&str; 5] = ["None", "Subtle", "Soft", "Medium", "Strong"];

/// Width of a slider readout: fits the widest one any slider shows
/// (shadow names, the padding and corner numbers) at the caption size,
/// plus a pixel either side so rounding never wraps or clips it.
fn readout_width() -> f32 {
    let widest = SHADOW_LABELS
        .iter()
        .map(|l| l.chars().count())
        .chain([MAX_PADDING, MAX_CORNER_RADIUS].map(|n| n.to_string().len()))
        .max()
        .unwrap_or(0);
    widest as f32 * TYPE_CAPTION * MONO_ADVANCE + 2.0
}

fn slider_row<'a>(
    focused: bool,
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
    focus_ring(
        r.push(
            text(readout)
                .font(mono_font())
                .size(TYPE_CAPTION)
                .style(text_t2)
                .width(Length::Fixed(readout_width())),
        )
        .padding([2, 4]),
        focused,
    )
}

pub(crate) fn beautify_tab<'a>(
    state: &'a App,
    ed: &'a EditorSession,
    p: &'static Palette,
) -> Element<'a, Message> {
    use crate::editor::{FocusItem as F, FocusZone};
    let lit = |item: F| super::key_focus::is_focused(ed, FocusZone::Panel, item);
    let s = ed.model.beautify();
    let busy = ed.busy;
    let current = s.matching_builtin();
    // Presets: built-ins, then saved ones (each with a remove ×).
    let mut presets = row![].spacing(SP1);
    for preset in BuiltinPreset::ALL {
        presets = presets.push(focus_ring(
            segment_button(
                text(preset.label()).size(ButtonSize::Xs.text_size()).into(),
                current == Some(preset),
                (!busy).then_some(Message::EditorBeautifyPreset(preset)),
            ),
            lit(F::BeautifyPreset(preset)),
        ));
    }
    for (i, saved) in state.preferences.editor_beautify_presets.iter().enumerate() {
        presets = presets.push(
            row![
                focus_ring(
                    segment_button(
                        text(saved.name.clone())
                            .size(ButtonSize::Xs.text_size())
                            .into(),
                        current.is_none() && saved.settings.clamped() == s,
                        (!busy).then_some(Message::EditorBeautifySavedPresetApplied(i)),
                    ),
                    lit(F::BeautifySaved(i)),
                ),
                focus_ring(
                    icon_button(
                        AppIcon::Close,
                        p.t3,
                        "Remove Preset",
                        None,
                        (!busy).then_some(Message::EditorBeautifyDeletePreset(i)),
                        false,
                        ICON_BTN_XS,
                        Position::Bottom,
                    ),
                    lit(F::BeautifyRemoveSaved(i)),
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
        bg_row = bg_row.push(swatch(bg, s.background == bg, busy, "Solid Color"));
    }
    let custom = row![
        focus_ring(
            text_input("#RRGGBB", &ed.beautify_hex_shown())
                .id(iced::widget::Id::new(super::key_focus::BEAUTIFY_HEX_ID))
                .on_input(Message::EditorBeautifyHexChanged)
                .on_submit(Message::EditorBeautifyHexSubmitted)
                .style(text_input_style)
                .size(TYPE_CAPTION)
                .width(Length::Fixed(96.0)),
            lit(F::BeautifyHex),
        ),
        focus_ring(
            icon_button(
                AppIcon::Dropper,
                p.t2,
                "Pick from Image",
                None,
                (!busy).then_some(Message::EditorBeautifyPickBackground),
                false,
                ICON_BTN_SM,
                Position::Bottom,
            ),
            lit(F::BeautifyPick),
        ),
    ]
    .spacing(SP1)
    .align_y(Alignment::Center);
    let canvas = focus_ring(
        text_segment(&CanvasSize::ALL.map(|c| (c.label(), c)), s.canvas, |c| {
            (!busy).then_some(Message::EditorBeautifyEdit(BeautifyEdit::Canvas(c)))
        }),
        lit(F::BeautifyCanvas),
    );
    let balance = focus_ring(
        toggler(s.balance)
            .label("Center automatically")
            .text_size(TYPE_CAPTION)
            .on_toggle_maybe(
                (!busy).then_some(|v| Message::EditorBeautifyEdit(BeautifyEdit::Balance(v))),
            ),
        lit(F::BeautifyBalance),
    );
    let save = focus_ring(
        bar_button(
            text("Save Preset").size(ButtonSize::Sm.text_size()).into(),
            ButtonKind::Secondary,
            ButtonSize::Sm,
            (!busy && state.preferences.editor_beautify_presets.len() < MAX_SAVED_PRESETS)
                .then_some(Message::EditorBeautifySavePreset),
        ),
        lit(F::BeautifySave),
    );
    let level = usize::from(s.shadow_level());
    let body = column![
        section("Presets", presets.wrap().vertical_spacing(SP1).into()),
        section(
            "Background",
            column![
                focus_ring(
                    bg_row.wrap().vertical_spacing(SP1),
                    lit(F::BeautifyBackground)
                ),
                custom
            ]
            .spacing(SP1)
            .into()
        ),
        slider_row(
            lit(F::BeautifyPadding),
            "Padding",
            MAX_PADDING,
            s.padding,
            format!("{}", s.padding),
            busy,
            BeautifyEdit::Padding
        ),
        slider_row(
            lit(F::BeautifyCorners),
            "Corners",
            MAX_CORNER_RADIUS,
            s.corner_radius,
            format!("{}", s.corner_radius),
            busy,
            BeautifyEdit::CornerRadius
        ),
        slider_row(
            lit(F::BeautifyShadow),
            "Shadow",
            u32::from(MAX_SHADOW_LEVEL),
            level as u32,
            SHADOW_LABELS[level].to_string(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_slider_readout_fits_its_width() {
        let w = readout_width();
        let labels = SHADOW_LABELS
            .iter()
            .map(|l| l.to_string())
            .chain([MAX_PADDING, MAX_CORNER_RADIUS].map(|n| n.to_string()));
        for label in labels {
            let needed = label.chars().count() as f32 * TYPE_CAPTION * MONO_ADVANCE;
            assert!(needed < w, "{label:?} needs {needed} px of {w}");
        }
        // "Medium" and "Strong" did not fit the old fixed 40 px.
        let medium = "Medium".len() as f32 * TYPE_CAPTION * MONO_ADVANCE;
        assert!(medium > 40.0 && medium < w);
    }
}

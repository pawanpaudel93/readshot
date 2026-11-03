//! Editor keyboard focus (WCAG 2.1.1 Keyboard, 2.4.3 Focus Order, 2.4.7
//! Focus Visible).
//!
//! iced 0.14 buttons, sliders, togglers and pick-lists can't take keyboard
//! focus, so the editor keeps its own: [`crate::editor::KeyFocus`]. `None`
//! is the canvas, where the tool letters, nudge and Tab through the
//! annotations work. The other places are zones of pointer-only controls:
//!
//! - **Image menu** (⇧⌘I opens it with the first row focused),
//! - **options bar** and **side panel** (⌃Tab / F6 walk canvas → bar →
//!   panel → canvas; ⌃⇧Tab / ⇧F6 go back; ⌥⌘T / ⌥⌘B open a panel tab),
//! - **Resize dialog** (⌥⌘I opens it with Width focused).
//!
//! Inside a zone the keys are the same everywhere: Tab / ⇧Tab or ↓ / ↑
//! move between controls, ← / → change the focused value (⇧ = ×10 on
//! sliders), ↵ or Space press the control, esc leaves the zone (a popover
//! zone closes, and the focus goes back to where it came from). The
//! focused control carries the focus ring (`ui_kit::focus_ring`).
//! The Text panel's line list is a list box: ↑ / ↓ move the line cursor
//! (⇧ extends the selection), Space selects, ↵ or ⌘C copy, R redacts.

use super::*;

use iced::keyboard::{key::Named, Key, Modifiers};
use iced::Task;
use readshot_core::beautify::{
    Background, BeautifyEdit, BuiltinPreset, CanvasSize, GRADIENTS, SOLIDS,
};
use readshot_core::document::{Align, FlipAxis, Join, MAX_GAP};
use readshot_core::{ArrowHeads, FillMode, MagnifierShape, SpotlightShape, StampKind};
use readshot_ui::editor::style::{CropAspect, LinePathKind, StyleMessage, TextSize};
use readshot_ui::editor::OptionControl;

use crate::app::{App, Message};
use crate::editor::{EditorPopover, EditorSession, FocusItem, FocusZone, KeyFocus, PanelTab};

/// Widget ids of the text fields a focus move hands the caret to.
pub(crate) const RESIZE_WIDTH_ID: &str = "resize-width";
pub(crate) const RESIZE_HEIGHT_ID: &str = "resize-height";
pub(crate) const BEAUTIFY_HEX_ID: &str = "beautify-hex";
pub(crate) const COLOR_HEX_ID: &str = "color-hex";
/// The Text panel's line list (scrolled to follow the line cursor).
pub(crate) const TEXT_LINES_ID: &str = "text-lines";
/// No widget has this id: focusing it takes the caret out of every
/// text field (iced's focus operation unfocuses all the others).
const NO_TEXT_FOCUS_ID: &str = "editor-no-text-focus";

/// Key labels of the editor's focus and menu keys (tooltips, chips,
/// README).
pub(crate) const KEY_IMAGE_MENU: (&str, &str) = ("⇧⌘I", "Ctrl+Shift+I");
pub(crate) const KEY_FLIP_H: (&str, &str) = ("⇧⌘H", "Ctrl+Shift+H");
pub(crate) const KEY_FLIP_V: (&str, &str) = ("⇧⌘V", "Ctrl+Shift+V");
pub(crate) const KEY_TEXT_TAB: (&str, &str) = ("⌥⌘T", "Ctrl+Shift+T");
pub(crate) const KEY_BEAUTIFY_TAB: (&str, &str) = ("⌥⌘B", "Ctrl+Shift+B");
pub(crate) const KEY_PANEL: (&str, &str) = ("⌥⌘P", "Ctrl+Shift+P");
pub(crate) const KEY_RESIZE: (&str, &str) = ("⌥⌘I", "Ctrl+Shift+R");

/// The letter of the Resize chord: ⌥⌘I on macOS, Ctrl+Shift+R elsewhere
/// (Ctrl+Shift+I is the Image menu there, as ⇧⌘I is on macOS).
pub(crate) const RESIZE_LETTER: &str = if cfg!(target_os = "macos") { "i" } else { "r" };

/// The editor's "second-level" chord on `letter`: ⌥⌘ on macOS,
/// Ctrl+Shift elsewhere. Off macOS, Ctrl+Alt is AltGr on many European
/// layouts (so typing would trigger it) and a desktop chord (Ctrl+Alt+T
/// opens a terminal on GNOME and KDE).
pub(crate) fn editor_alt_chord(key: &Key, m: Modifiers, letter: &str) -> bool {
    let Key::Character(c) = key else {
        return false;
    };
    if !c.eq_ignore_ascii_case(letter) || !m.command() {
        return false;
    }
    if cfg!(target_os = "macos") {
        m.alt() && !m.shift() && !m.control()
    } else {
        m.shift() && !m.alt() && !m.logo()
    }
}
pub(crate) const KEY_NEXT_ZONE: (&str, &str) = ("⌃Tab", "Ctrl+Tab");

pub(crate) fn key_label(k: (&'static str, &'static str)) -> &'static str {
    platform_key(k.0, k.1)
}

/// Take the caret out of every text field.
pub(crate) fn unfocus_text_fields<T: Send + 'static>() -> Task<T> {
    iced::widget::operation::focus(iced::widget::Id::new(NO_TEXT_FOCUS_ID))
}

fn focus_field<T: Send + 'static>(id: &'static str) -> Task<T> {
    iced::widget::operation::focus(iced::widget::Id::new(id))
}

/// The text field behind a focus item, if it is one.
fn field_id(item: FocusItem) -> Option<&'static str> {
    match item {
        FocusItem::ResizeWidth => Some(RESIZE_WIDTH_ID),
        FocusItem::ResizeHeight => Some(RESIZE_HEIGHT_ID),
        FocusItem::BeautifyHex => Some(BEAUTIFY_HEX_ID),
        _ => None,
    }
}

/// Whether `item` in `zone` is where the keyboard is (draws the ring).
pub(crate) fn is_focused(ed: &EditorSession, zone: FocusZone, item: FocusItem) -> bool {
    ed.key_focus == Some(KeyFocus { zone, item })
}

/// The controls of `zone`, in focus order (the order they are drawn).
pub(crate) fn zone_items(state: &App, ed: &EditorSession, zone: FocusZone) -> Vec<FocusItem> {
    use FocusItem as F;
    match zone {
        FocusZone::ImageMenu => {
            let mut v = vec![
                F::MenuResize,
                F::MenuRotateLeft,
                F::MenuRotateRight,
                F::MenuFlipH,
                F::MenuFlipV,
                F::MenuAddBelow,
                F::MenuAddBeside,
                F::MenuAddImage,
            ];
            if ed.model.geometry().parts.len() >= 2 {
                v.extend([F::MenuGap, F::MenuAlign]);
            }
            v
        }
        FocusZone::OptionsBar => {
            if ed.eyedropper.is_some() {
                return Vec::new();
            }
            visible_option_controls(ed)
                .into_iter()
                .map(F::Option)
                .collect()
        }
        FocusZone::Panel => panel_items(state, ed),
        FocusZone::Resize => vec![
            F::ResizeWidth,
            F::ResizeHeight,
            F::ResizeUnit,
            F::ResizeLock,
            F::ResizeCancel,
            F::ResizeApply,
        ],
    }
}

fn panel_items(state: &App, ed: &EditorSession) -> Vec<FocusItem> {
    use FocusItem as F;
    let mut v = vec![F::PanelTabs];
    match PanelTab::from_id(&state.preferences.editor_panel_tab) {
        PanelTab::Beautify => {
            v.extend(BuiltinPreset::ALL.map(F::BeautifyPreset));
            for i in 0..state.preferences.editor_beautify_presets.len() {
                v.extend([F::BeautifySaved(i), F::BeautifyRemoveSaved(i)]);
            }
            v.extend([
                F::BeautifyBackground,
                F::BeautifyHex,
                F::BeautifyPick,
                F::BeautifyPadding,
                F::BeautifyCorners,
                F::BeautifyShadow,
                F::BeautifyCanvas,
                F::BeautifyBalance,
                F::BeautifySave,
            ]);
        }
        PanelTab::Text => match &ed.text_panel.recognition {
            crate::editor::TextRecognition::Failed(_) => v.push(F::TextRetry),
            crate::editor::TextRecognition::Ready(doc) if doc.is_empty() => {
                v.push(F::TextOpenSettings)
            }
            crate::editor::TextRecognition::Ready(doc) => {
                if ed.text_panel.positioned_document().is_some()
                    && !ed.text_panel.matches.is_empty()
                {
                    v.extend([F::TextRedactStyle, F::TextRedactAll]);
                }
                if !doc.lines.is_empty() {
                    v.push(F::TextLines);
                }
                v.extend([F::TextLineBreaks, F::TextCopyLines, F::TextCopyAll]);
            }
            _ => {}
        },
    }
    v
}

/// Whether `zone` is on screen, so its focus ring can be seen.
fn zone_on_screen(state: &App, ed: &EditorSession, zone: FocusZone) -> bool {
    match zone {
        FocusZone::ImageMenu => ed.popover == Some(EditorPopover::Image),
        FocusZone::Resize => ed.popover == Some(EditorPopover::Resize),
        FocusZone::OptionsBar => true,
        FocusZone::Panel => matches!(
            editor_panel_mode(state, ed),
            PanelMode::Docked | PanelMode::Overlay
        ),
    }
}

/// The Color popover's hex field holds something other than the current
/// colour: typed, not yet submitted.
pub(crate) fn color_hex_pending(ed: &EditorSession) -> bool {
    let current = ed.model.effective_color().to_hex();
    !ed.hex_draft.is_empty() && !ed.hex_draft.eq_ignore_ascii_case(&current)
}

/// The zones ⌃Tab / F6 walk, in order, that can take focus now.
fn cycle_zones(state: &App, ed: &EditorSession) -> Vec<FocusZone> {
    let mut zones = Vec::new();
    if !zone_items(state, ed, FocusZone::OptionsBar).is_empty() {
        zones.push(FocusZone::OptionsBar);
    }
    if matches!(
        editor_panel_mode(state, ed),
        PanelMode::Docked | PanelMode::Overlay
    ) {
        zones.push(FocusZone::Panel);
    }
    zones
}

/// The zone after (or before) `current` in the ⌃Tab walk; `None` is the
/// canvas, which sits at both ends.
pub(crate) fn next_zone(
    zones: &[FocusZone],
    current: Option<FocusZone>,
    forward: bool,
) -> Option<FocusZone> {
    // Positions: canvas = 0, zones = 1..=n.
    let n = zones.len() + 1;
    let at = current
        .and_then(|z| zones.iter().position(|x| *x == z))
        .map_or(0, |i| i + 1);
    let next = if forward {
        (at + 1) % n
    } else {
        (at + n - 1) % n
    };
    (next > 0).then(|| zones[next - 1])
}

/// What a key does inside a zone ([`focus_key`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum FocusKey {
    Next,
    Prev,
    /// Change the value: ±1 step, ±10 with ⇧.
    Adjust(i32),
    Activate,
    Leave,
    LineMove {
        delta: i32,
        extend: bool,
    },
    LineToggle,
    LineCopy,
    LineRedact,
    GapStep(i32),
    AlignStep(i32),
    ToggleUnit,
    ToggleLock,
}

/// Map a key press inside a zone. `captured`: a text field took the key
/// (it keeps its caret keys; only Tab, which fields ignore, moves on).
pub(crate) fn focus_key(
    focus: KeyFocus,
    key: &Key,
    m: Modifiers,
    captured: bool,
) -> Option<FocusKey> {
    let lines = focus.item == FocusItem::TextLines;
    // Resize: ⌥P switches px / %, ⌥L locks the aspect, even while a
    // field has the caret (the fields accept digits only).
    if focus.zone == FocusZone::Resize && m.alt() && !m.command() && !m.control() {
        if let Key::Character(c) = key {
            if c.eq_ignore_ascii_case("p") {
                return Some(FocusKey::ToggleUnit);
            }
            if c.eq_ignore_ascii_case("l") {
                return Some(FocusKey::ToggleLock);
            }
        }
    }
    if m.command() || m.alt() || m.control() {
        // ⌘C copies the selected lines while the line list is focused.
        if lines && m.command() && !m.alt() && !m.shift() {
            if let Key::Character(c) = key {
                if c.eq_ignore_ascii_case("c") {
                    return Some(FocusKey::LineCopy);
                }
            }
        }
        return None;
    }
    if matches!(key, Key::Named(Named::Tab)) {
        return Some(if m.shift() {
            FocusKey::Prev
        } else {
            FocusKey::Next
        });
    }
    if captured {
        return None;
    }
    let step = if m.shift() { 10 } else { 1 };
    match key {
        Key::Named(Named::ArrowDown) if lines => Some(FocusKey::LineMove {
            delta: 1,
            extend: m.shift(),
        }),
        Key::Named(Named::ArrowUp) if lines => Some(FocusKey::LineMove {
            delta: -1,
            extend: m.shift(),
        }),
        Key::Named(Named::ArrowDown) => Some(FocusKey::Next),
        Key::Named(Named::ArrowUp) => Some(FocusKey::Prev),
        Key::Named(Named::ArrowRight) => Some(FocusKey::Adjust(step)),
        Key::Named(Named::ArrowLeft) => Some(FocusKey::Adjust(-step)),
        Key::Named(Named::Enter) if lines => Some(FocusKey::LineCopy),
        Key::Named(Named::Space) if lines => Some(FocusKey::LineToggle),
        Key::Named(Named::Enter | Named::Space) => Some(FocusKey::Activate),
        Key::Named(Named::Escape) => Some(FocusKey::Leave),
        Key::Character(c) if lines && c.eq_ignore_ascii_case("r") => Some(FocusKey::LineRedact),
        Key::Character(c) if focus.zone == FocusZone::ImageMenu => match c.as_str() {
            "-" | "_" => Some(FocusKey::GapStep(-1)),
            "+" | "=" => Some(FocusKey::GapStep(1)),
            "," | "<" => Some(FocusKey::AlignStep(-1)),
            "." | ">" => Some(FocusKey::AlignStep(1)),
            _ => None,
        },
        _ => None,
    }
}

/// ⌃Tab / ⌃⇧Tab or F6 / ⇧F6: `Some(forward)`. ⌃ is the physical
/// Control key on every platform: on macOS it is not ⌘ (so ⌃⌘Tab is not
/// it); elsewhere it is the command modifier.
pub(crate) fn zone_cycle_key(key: &Key, m: Modifiers) -> Option<bool> {
    let logo_free = !cfg!(target_os = "macos") || !m.logo();
    match key {
        Key::Named(Named::Tab) if m.control() && !m.alt() && logo_free => Some(!m.shift()),
        Key::Named(Named::F6) if !m.command() && !m.alt() && !m.control() => Some(!m.shift()),
        _ => None,
    }
}

/// Put the focus on `next` (or the canvas), keeping the Text panel's
/// line cursor only while its list has the focus.
pub(crate) fn set_focus(ed: &mut EditorSession, next: Option<KeyFocus>) {
    if next.map(|f| f.item) != Some(FocusItem::TextLines) {
        ed.text_panel.cursor = None;
    } else if ed.text_panel.cursor.is_none() {
        ed.text_panel.cursor = Some(0);
    }
    ed.key_focus = next;
}

/// The task that moves the caret to match the focus: into a field the
/// focus is on, out of every field otherwise.
fn caret_task(focus: Option<KeyFocus>) -> Task<Message> {
    match focus.and_then(|f| field_id(f.item)) {
        Some(id) => focus_field(id),
        None => unfocus_text_fields(),
    }
}

/// Handle a key press against the editor's keyboard focus. `Some` when
/// the key was used; `None` lets it fall through to the canvas map
/// (`editor_key_message`).
pub(crate) fn handle_focus_key(
    state: &mut App,
    key: &Key,
    m: Modifiers,
    status_ignored: bool,
) -> Option<Task<Message>> {
    let ed = state.editor.as_ref()?;
    // The Color and Width popovers are one stop: Tab (or ⇧Tab) leaves
    // them, back to the bar control that opened them (or the canvas). It
    // never reaches the canvas map, where it would change the selection
    // under a hex value being typed.
    if ed.key_focus.is_none()
        && matches!(
            ed.popover,
            Some(EditorPopover::Color | EditorPopover::Width)
        )
        && matches!(key, Key::Named(Named::Tab))
        && !m.command()
        && !m.alt()
        && !m.control()
    {
        // A hex typed but not submitted is applied, as ↵ would (an
        // invalid one keeps the popover open and says why). Esc stays
        // cancel.
        if ed.popover == Some(EditorPopover::Color) && color_hex_pending(ed) {
            let task = update(state, Message::Editor(EditorMsg::HexSubmitted));
            let ed = state.editor.as_ref()?;
            if ed.popover.is_some() {
                return Some(task);
            }
            return Some(Task::batch([task, caret_task(ed.key_focus)]));
        }
        let ed = state.editor.as_mut()?;
        close_editor_popover(ed);
        return Some(caret_task(ed.key_focus));
    }
    // A pointer-opened Image menu or Resize dialog: ↓ / ↑ / Tab enter it.
    if ed.key_focus.is_none() && !m.command() && !m.alt() && !m.control() {
        let zone = match ed.popover {
            Some(EditorPopover::Image) => Some(FocusZone::ImageMenu),
            Some(EditorPopover::Resize) => Some(FocusZone::Resize),
            _ => None,
        };
        let entering = matches!(
            key,
            Key::Named(Named::ArrowDown | Named::ArrowUp | Named::Tab)
        );
        if let (Some(zone), true) = (zone, entering) {
            let items = zone_items(state, ed, zone);
            let back = matches!(key, Key::Named(Named::ArrowUp))
                || (matches!(key, Key::Named(Named::Tab)) && m.shift());
            let item = if back { items.last() } else { items.first() };
            let focus = item.map(|&item| KeyFocus { zone, item });
            let ed = state.editor.as_mut()?;
            set_focus(ed, focus);
            return Some(caret_task(focus));
        }
    }
    let focus = ed.key_focus?;
    // The zone went away under the focus (panel hidden, popover closed by
    // a click): the keys belong to the canvas again.
    if !zone_on_screen(state, ed, focus.zone) {
        let ed = state.editor.as_mut()?;
        set_focus(ed, None);
        return None;
    }
    let items = zone_items(state, ed, focus.zone);
    // The control may have gone (tool switched, preset removed): move to
    // the first one left, or back to the canvas.
    let focus = if items.contains(&focus.item) {
        focus
    } else {
        let fixed = items.first().map(|&item| KeyFocus {
            zone: focus.zone,
            item,
        });
        let ed = state.editor.as_mut()?;
        set_focus(ed, fixed);
        fixed?
    };
    let cmd = focus_key(focus, key, m, !status_ignored)?;
    // A popover over the bar or panel (Color, Width) owns Esc: the chain
    // closes it and puts the focus back.
    if cmd == FocusKey::Leave {
        let ed = state.editor.as_mut()?;
        if ed.popover.is_some() {
            return None;
        }
        set_focus(ed, None);
        return Some(unfocus_text_fields());
    }
    let pos = items.iter().position(|i| *i == focus.item).unwrap_or(0);
    Some(match cmd {
        FocusKey::Next | FocusKey::Prev => {
            let n = items.len();
            let next = if cmd == FocusKey::Next {
                (pos + 1) % n
            } else {
                (pos + n - 1) % n
            };
            let next = Some(KeyFocus {
                zone: focus.zone,
                item: items[next],
            });
            let ed = state.editor.as_mut()?;
            set_focus(ed, next);
            caret_task(next)
        }
        FocusKey::Activate => activate(state, focus, pos),
        FocusKey::Adjust(step) => match adjust_message(state, focus.item, step) {
            Some(msgs) => run_all(state, msgs),
            None => Task::none(),
        },
        FocusKey::GapStep(d) => run_all(state, gap_messages(state, d)),
        FocusKey::AlignStep(d) => run_all(state, align_message(state, d).into_iter().collect()),
        FocusKey::ToggleUnit => run_all(state, adjust_message(state, FocusItem::ResizeUnit, 1)?),
        FocusKey::ToggleLock => run_all(state, adjust_message(state, FocusItem::ResizeLock, 1)?),
        FocusKey::LineMove { delta, extend } => {
            let ed = state.editor.as_mut()?;
            move_line_cursor(ed, delta, extend);
            // Keep the cursor line in view: scroll the list to the same
            // fraction of its length.
            let n = ed.text_panel.document().map_or(0, |d| d.lines.len());
            match ed.text_panel.cursor {
                Some(i) if n > 1 => iced::widget::operation::snap_to(
                    iced::widget::Id::new(TEXT_LINES_ID),
                    iced::widget::operation::RelativeOffset {
                        x: 0.0,
                        y: i as f32 / (n - 1) as f32,
                    },
                ),
                _ => Task::none(),
            }
        }
        FocusKey::LineToggle => {
            let ed = state.editor.as_mut()?;
            if let Some(i) = ed.text_panel.cursor {
                let tp = &mut ed.text_panel;
                super::text_panel::click_selection(
                    &mut tp.selected,
                    &mut tp.anchor,
                    i,
                    false,
                    true,
                );
            }
            Task::none()
        }
        FocusKey::LineCopy => {
            let ed = state.editor.as_mut()?;
            if ed.text_panel.selected.is_empty() {
                if let Some(i) = ed.text_panel.cursor {
                    ed.text_panel.selected.insert(i);
                    ed.text_panel.anchor = Some(i);
                }
            }
            update(
                state,
                Message::Editor(EditorMsg::TextPanel(TextPanelMsg::CopyLines)),
            )
        }
        FocusKey::LineRedact => match state.editor.as_ref()?.text_panel.cursor {
            Some(i) => update(
                state,
                Message::Editor(EditorMsg::TextPanel(TextPanelMsg::RedactLine(i))),
            ),
            None => Task::none(),
        },
        FocusKey::Leave => unreachable!("handled above"),
    })
}

fn run_all(state: &mut App, msgs: Vec<Message>) -> Task<Message> {
    Task::batch(msgs.into_iter().map(|m| update(state, m)))
}

/// Move the Text panel's line cursor; with `extend`, ⇧-click selection
/// from the anchor (the old cursor when there is none).
pub(crate) fn move_line_cursor(ed: &mut EditorSession, delta: i32, extend: bool) {
    let Some(n) = ed.text_panel.document().map(|d| d.lines.len()) else {
        return;
    };
    if n == 0 {
        return;
    }
    let old = ed.text_panel.cursor;
    let at = old.map_or(if delta > 0 { -1 } else { n as i64 }, |c| c as i64);
    let next = (at + i64::from(delta)).clamp(0, n as i64 - 1) as usize;
    ed.text_panel.cursor = Some(next);
    if extend {
        let tp = &mut ed.text_panel;
        if tp.anchor.is_none() {
            tp.anchor = old.or(Some(next));
        }
        super::text_panel::click_selection(&mut tp.selected, &mut tp.anchor, next, true, false);
    }
}

/// Press the focused control.
fn activate(state: &mut App, focus: KeyFocus, pos: usize) -> Task<Message> {
    use FocusItem as F;
    let Some(ed) = state.editor.as_ref() else {
        return Task::none();
    };
    let busy = ed.busy;
    let msg: Option<Message> = match focus.item {
        F::MenuResize => Some(Message::Editor(EditorMsg::Image(ImageMsg::ResizeOpen))),
        F::MenuRotateLeft => Some(Message::Editor(EditorMsg::Image(ImageMsg::Rotate {
            clockwise: false,
        }))),
        F::MenuRotateRight => Some(Message::Editor(EditorMsg::Image(ImageMsg::Rotate {
            clockwise: true,
        }))),
        F::MenuFlipH => Some(Message::Editor(EditorMsg::Image(ImageMsg::Flip(
            FlipAxis::Horizontal,
        )))),
        F::MenuFlipV => Some(Message::Editor(EditorMsg::Image(ImageMsg::Flip(
            FlipAxis::Vertical,
        )))),
        F::MenuAddBelow => Some(Message::Editor(EditorMsg::Combine(
            CombineMsg::CaptureRequested(Join::Below),
        ))),
        F::MenuAddBeside => Some(Message::Editor(EditorMsg::Combine(
            CombineMsg::CaptureRequested(Join::Beside),
        ))),
        F::MenuAddImage => Some(Message::Editor(EditorMsg::Combine(
            CombineMsg::AddImageFromFile,
        ))),
        F::Option(OptionControl::Color) | F::Option(OptionControl::Width) => {
            let which = if focus.item == F::Option(OptionControl::Color) {
                EditorPopover::Color
            } else {
                EditorPopover::Width
            };
            if busy {
                return Task::none();
            }
            let task = update(state, Message::Editor(EditorMsg::PopoverToggled(which)));
            let Some(ed) = state.editor.as_mut() else {
                return task;
            };
            if ed.popover == Some(which) {
                // The popover has the keys now; esc closes it and puts the
                // focus back on this control.
                ed.key_focus_return = Some(focus);
                ed.key_focus = None;
                if which == EditorPopover::Color {
                    return Task::batch([task, focus_field(COLOR_HEX_ID)]);
                }
            }
            return task;
        }
        F::Option(OptionControl::CropApply) => Some(Message::Editor(EditorMsg::CropApply)),
        F::PanelTabs => {
            let current = PanelTab::from_id(&state.preferences.editor_panel_tab);
            Some(Message::Editor(EditorMsg::TextPanel(
                TextPanelMsg::PanelTabSelected(other_tab(current)),
            )))
        }
        F::BeautifyPreset(p) => Some(Message::Editor(EditorMsg::BeautifyPreset(p))),
        F::BeautifySaved(i) => Some(Message::Editor(EditorMsg::Beautify(
            BeautifyMsg::SavedPresetApplied(i),
        ))),
        F::BeautifyRemoveSaved(i) => Some(Message::Editor(EditorMsg::Beautify(
            BeautifyMsg::DeletePreset(i),
        ))),
        F::BeautifyHex => return focus_field(BEAUTIFY_HEX_ID),
        F::BeautifyPick => Some(Message::Editor(EditorMsg::Beautify(
            BeautifyMsg::PickBackground,
        ))),
        F::BeautifySave => Some(Message::Editor(EditorMsg::Beautify(
            BeautifyMsg::SavePreset,
        ))),
        F::TextRetry => Some(Message::Editor(EditorMsg::TextPanel(
            TextPanelMsg::Recognize,
        ))),
        F::TextOpenSettings => Some(Message::Editor(EditorMsg::TextPanel(
            TextPanelMsg::OpenTextSettings,
        ))),
        F::TextRedactAll => Some(Message::Editor(EditorMsg::TextPanel(
            TextPanelMsg::RedactAll,
        ))),
        F::TextCopyLines => Some(Message::Editor(EditorMsg::TextPanel(
            TextPanelMsg::CopyLines,
        ))),
        F::TextCopyAll => Some(Message::Editor(EditorMsg::CopyTextRequested)),
        F::ResizeWidth | F::ResizeHeight => {
            return field_id(focus.item).map_or_else(Task::none, focus_field)
        }
        F::ResizeCancel => Some(Message::Editor(EditorMsg::PopoverClosed)),
        F::ResizeApply => Some(Message::Editor(EditorMsg::Image(ImageMsg::ResizeApply))),
        // Values: Space / ↵ step them forward, as → does.
        other => {
            return match adjust_message(state, other, 1) {
                Some(msgs) => run_all(state, msgs),
                None => Task::none(),
            }
        }
    };
    let Some(msg) = msg else {
        return Task::none();
    };
    if busy {
        return Task::none();
    }
    let presets_before = state.preferences.editor_beautify_presets.len();
    let task = update(state, msg);
    // A removed preset moves the ring off Remove, onto the preset now in
    // its place (or the one before), so a second press never removes
    // the next preset too.
    if let F::BeautifyRemoveSaved(i) = focus.item {
        let left = state.preferences.editor_beautify_presets.len();
        let removed = left < presets_before;
        if let Some(ed) = state.editor.as_mut().filter(|_| removed && left > 0) {
            if ed.key_focus == Some(focus) {
                set_focus(
                    ed,
                    Some(KeyFocus {
                        zone: focus.zone,
                        item: F::BeautifySaved(i.min(left - 1)),
                    }),
                );
            }
        }
    }
    // A removed preset (or another change) can take the control away:
    // stay at the same place in the list.
    if let Some(ed) = state.editor.as_ref() {
        if ed.key_focus == Some(focus) {
            let items = zone_items(state, ed, focus.zone);
            if !items.contains(&focus.item) {
                let next = items
                    .get(pos.min(items.len().saturating_sub(1)))
                    .map(|&item| KeyFocus {
                        zone: focus.zone,
                        item,
                    });
                if let Some(ed) = state.editor.as_mut() {
                    set_focus(ed, next);
                }
            }
        }
    }
    task
}

/// The label of the tab the tab switch goes to (its tooltip).
pub(crate) fn other_tab_label(t: PanelTab) -> &'static str {
    other_tab(t).label()
}

fn other_tab(t: PanelTab) -> PanelTab {
    match t {
        PanelTab::Text => PanelTab::Beautify,
        PanelTab::Beautify => PanelTab::Text,
    }
}

/// The value `step` places after `current` in `all` (wrapping).
pub(crate) fn cycle<T: Copy + PartialEq>(all: &[T], current: T, step: i32) -> T {
    let n = all.len() as i64;
    if n == 0 {
        return current;
    }
    let at = all.iter().position(|v| *v == current).unwrap_or(0) as i64;
    let dir = step.signum() as i64;
    all[(at + dir).rem_euclid(n) as usize]
}

const FILLS: [FillMode; 3] = [FillMode::Outline, FillMode::Filled, FillMode::Both];
const LINE_PATHS: [LinePathKind; 3] = [
    LinePathKind::Straight,
    LinePathKind::Curved,
    LinePathKind::HandDrawn,
];
const HEADS: [ArrowHeads; 2] = [ArrowHeads::End, ArrowHeads::Both];
const SPOTLIGHT_SHAPES: [SpotlightShape; 2] = [SpotlightShape::Rectangle, SpotlightShape::Ellipse];
const MAGNIFIER_SHAPES: [MagnifierShape; 2] =
    [MagnifierShape::Circle, MagnifierShape::RoundedSquare];
const MAGNIFIER_ZOOMS: [f32; 3] = [2.0, 3.0, 4.0];

/// The backgrounds the Beautify swatches offer, in their order.
pub(crate) fn beautify_backgrounds() -> Vec<Background> {
    let mut v = vec![Background::Transparent];
    v.extend(
        GRADIENTS
            .iter()
            .map(|&gradient| Background::Gradient { gradient }),
    );
    v.extend(SOLIDS.iter().map(|&rgba| Background::Solid { rgba }));
    v
}

/// The message(s) that change the focused value by `step` (← / →).
fn adjust_message(state: &App, item: FocusItem, step: i32) -> Option<Vec<Message>> {
    use FocusItem as F;
    let ed = state.editor.as_ref()?;
    if ed.busy {
        return None;
    }
    let dir = step.signum();
    let style = ed.model.effective_style();
    let s = |m: StyleMessage| Some(vec![Message::Editor(EditorMsg::Style(m))]);
    let b = |e: BeautifyEdit| {
        Some(vec![Message::Editor(EditorMsg::Beautify(
            BeautifyMsg::Edit(e),
        ))])
    };
    match item {
        F::MenuGap => Some(gap_messages(state, step)),
        F::MenuAlign => align_message(state, dir).map(|m| vec![m]),
        F::Option(c) => match c {
            OptionControl::Color => Some(vec![Message::Editor(EditorMsg::ColorCycle(dir))]),
            OptionControl::Width => Some(vec![Message::Editor(EditorMsg::WidthBump(dir as f32))]),
            OptionControl::Fill => s(StyleMessage::Fill(cycle(&FILLS, style.fill, dir))),
            OptionControl::Dash => s(StyleMessage::Dashed(!style.dashed)),
            OptionControl::CornerRadius => s(StyleMessage::CornerRadius(
                (style.corner_radius + step as f32)
                    .clamp(0.0, readshot_ui::editor::style::MAX_CORNER_RADIUS),
            )),
            OptionControl::LinePath => s(StyleMessage::LinePath(cycle(
                &LINE_PATHS,
                style.line_path,
                dir,
            ))),
            OptionControl::Heads => s(StyleMessage::Heads(cycle(&HEADS, style.heads, dir))),
            OptionControl::TextSize => s(StyleMessage::TextSize(cycle(
                &TextSize::ALL,
                style.text_size,
                dir,
            ))),
            OptionControl::TextPill => s(StyleMessage::TextPill(!style.text_pill)),
            OptionControl::TextBold => s(StyleMessage::TextBold(!style.text_bold)),
            OptionControl::Dim => s(StyleMessage::Dim((style.dim + 0.05 * step as f32).clamp(
                readshot_ui::editor::style::MIN_DIM,
                readshot_ui::editor::style::MAX_DIM,
            ))),
            OptionControl::SpotlightShape => s(StyleMessage::SpotlightShape(cycle(
                &SPOTLIGHT_SHAPES,
                style.spotlight_shape,
                dir,
            ))),
            OptionControl::StampPicker => s(StyleMessage::Stamp(cycle(
                &StampKind::ALL,
                style.stamp,
                dir,
            ))),
            OptionControl::SnapToText => s(StyleMessage::SnapToText(!style.snap_to_text)),
            OptionControl::CropAspect => s(StyleMessage::CropAspect(cycle(
                &CropAspect::ALL,
                style.crop_aspect,
                dir,
            ))),
            OptionControl::MagnifierZoom => s(StyleMessage::MagnifierZoom(cycle(
                &MAGNIFIER_ZOOMS,
                style.magnifier_zoom,
                dir,
            ))),
            OptionControl::MagnifierShape => s(StyleMessage::MagnifierShape(cycle(
                &MAGNIFIER_SHAPES,
                style.magnifier_shape,
                dir,
            ))),
            OptionControl::CropApply => None,
        },
        F::PanelTabs => {
            let current = PanelTab::from_id(&state.preferences.editor_panel_tab);
            Some(vec![Message::Editor(EditorMsg::TextPanel(
                TextPanelMsg::PanelTabSelected(other_tab(current)),
            ))])
        }
        F::BeautifyBackground => {
            let all = beautify_backgrounds();
            b(BeautifyEdit::Background(cycle(
                &all,
                ed.model.beautify().background,
                dir,
            )))
        }
        F::BeautifyPadding => {
            let v = ed.model.beautify().padding as i64 + i64::from(step);
            b(BeautifyEdit::Padding(
                v.clamp(0, i64::from(readshot_core::beautify::MAX_PADDING)) as u32,
            ))
        }
        F::BeautifyCorners => {
            let v = ed.model.beautify().corner_radius as i64 + i64::from(step);
            b(BeautifyEdit::CornerRadius(
                v.clamp(0, i64::from(readshot_core::beautify::MAX_CORNER_RADIUS)) as u32,
            ))
        }
        F::BeautifyShadow => {
            let v = i64::from(ed.model.beautify().shadow_level()) + i64::from(dir);
            b(BeautifyEdit::ShadowLevel(
                v.clamp(0, i64::from(readshot_core::beautify::MAX_SHADOW_LEVEL)) as u8,
            ))
        }
        F::BeautifyCanvas => b(BeautifyEdit::Canvas(cycle(
            &CanvasSize::ALL,
            ed.model.beautify().canvas,
            dir,
        ))),
        F::BeautifyBalance => b(BeautifyEdit::Balance(!ed.model.beautify().balance)),
        F::TextRedactStyle => Some(vec![Message::Editor(EditorMsg::TextPanel(
            TextPanelMsg::RedactStyleChanged(cycle(
                &readshot_core::redact::RedactStyle::ALL,
                ed.text_panel.redact_style,
                dir,
            )),
        ))]),
        F::TextLineBreaks => Some(vec![Message::Editor(EditorMsg::TextPanel(
            TextPanelMsg::RemoveLineBreaksToggled(
                !state.preferences.editor_copy_remove_line_breaks,
            ),
        ))]),
        F::ResizeUnit => {
            let d = ed.resize_draft.as_ref()?;
            let next = match d.unit {
                crate::editor::ResizeUnit::Pixels => crate::editor::ResizeUnit::Percent,
                crate::editor::ResizeUnit::Percent => crate::editor::ResizeUnit::Pixels,
            };
            Some(vec![Message::Editor(EditorMsg::Image(
                ImageMsg::ResizeUnitChanged(next),
            ))])
        }
        F::ResizeLock => {
            let d = ed.resize_draft.as_ref()?;
            Some(vec![Message::Editor(EditorMsg::Image(
                ImageMsg::ResizeLockToggled(!d.lock),
            ))])
        }
        _ => None,
    }
}

/// The gap changed by `step` px as one undo step: a preview and its
/// commit, as a slider drag and release do.
fn gap_messages(state: &App, step: i32) -> Vec<Message> {
    let Some(ed) = state.editor.as_ref() else {
        return Vec::new();
    };
    let g = ed.model.geometry();
    if ed.busy || g.parts.len() < 2 {
        return Vec::new();
    }
    let gap = ed.layout_gap_pending.unwrap_or(g.gap);
    let next = (i64::from(gap) + i64::from(step)).clamp(0, i64::from(MAX_GAP)) as u32;
    if next == gap {
        return Vec::new();
    }
    vec![
        Message::Editor(EditorMsg::Combine(CombineMsg::GapPreview(next))),
        Message::Editor(EditorMsg::SnapshotSliderCommit),
    ]
}

fn align_message(state: &App, dir: i32) -> Option<Message> {
    let ed = state.editor.as_ref()?;
    let g = ed.model.geometry();
    if ed.busy || g.parts.len() < 2 {
        return None;
    }
    Some(Message::Editor(EditorMsg::Combine(CombineMsg::Align(
        cycle(&Align::ALL, g.align, dir),
    ))))
}

/// The status shown when the keyboard enters a zone: how to work it and
/// how to leave.
pub(crate) fn zone_hint(zone: FocusZone) -> String {
    let name = match zone {
        FocusZone::ImageMenu => "Image menu",
        FocusZone::OptionsBar => "Options bar",
        FocusZone::Panel => "Side panel",
        FocusZone::Resize => "Resize",
    };
    format!(
        "{name}: Tab moves, ←→ change, {RETURN_KEY} presses, {ESC_KEY} leaves. {} goes to the next area.",
        key_label(KEY_NEXT_ZONE)
    )
}

/// Every [`FocusMsg`]: `handle_editor_message` hands each
/// `EditorMsg::Focus` here.
pub(crate) fn handle_focus_message(state: &mut App, message: FocusMsg) -> Task<Message> {
    match message {
        FocusMsg::ImageMenuKey => {
            let Some(ed) = state.editor.as_mut() else {
                return Task::none();
            };
            if ed.busy {
                return Task::none();
            }
            if ed.popover == Some(EditorPopover::Image) {
                close_editor_popover(ed);
                return unfocus_text_fields();
            }
            close_editor_popover(ed);
            ed.popover = Some(EditorPopover::Image);
            set_focus(
                ed,
                Some(KeyFocus {
                    zone: FocusZone::ImageMenu,
                    item: FocusItem::MenuResize,
                }),
            );
            unfocus_text_fields()
        }
        FocusMsg::PanelTabKey(tab) => {
            let Some(ed) = state.editor.as_ref() else {
                return Task::none();
            };
            let mode = editor_panel_mode(state, ed);
            let mut tasks = Vec::new();
            if matches!(mode, PanelMode::Hidden | PanelMode::Collapsed) {
                tasks.push(update(
                    state,
                    Message::Editor(EditorMsg::TextPanel(TextPanelMsg::PanelToggled)),
                ));
            }
            tasks.push(update(
                state,
                Message::Editor(EditorMsg::TextPanel(TextPanelMsg::PanelTabSelected(tab))),
            ));
            if let Some(ed) = state.editor.as_mut() {
                close_editor_popover(ed);
                set_focus(
                    ed,
                    Some(KeyFocus {
                        zone: FocusZone::Panel,
                        item: FocusItem::PanelTabs,
                    }),
                );
            }
            tasks.push(unfocus_text_fields());
            Task::batch(tasks)
        }
        FocusMsg::ZoneCycle(forward) => {
            let Some(ed) = state.editor.as_ref() else {
                return Task::none();
            };
            // A popover zone (Image menu, Resize) is modal: Tab moves
            // inside it and esc leaves it.
            if ed.popover.is_some() {
                return Task::none();
            }
            let zones = cycle_zones(state, ed);
            let current = ed.key_focus.map(|f| f.zone);
            let next = next_zone(&zones, current, forward).and_then(|zone| {
                let items = zone_items(state, ed, zone);
                let item = if forward { items.first() } else { items.last() };
                item.map(|&item| KeyFocus { zone, item })
            });
            if let Some(ed) = state.editor.as_mut() {
                set_focus(ed, next);
                if let Some(f) = next {
                    ed.set_guide(zone_hint(f.zone), crate::editor::KeyGuide::Zone);
                }
            }
            caret_task(next)
        }
    }
}

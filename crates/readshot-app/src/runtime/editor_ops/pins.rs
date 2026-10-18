//! Pin window messages.
//! Moved out of `handle_editor_message`; `use super::*` keeps the
//! arms' names resolving as they did there.

use super::*;

/// `Message::PinWindowReady`.
pub(super) fn on_pin_window_ready(
    state: &mut App,
    id: iced::window::Id,
    handle: iced::widget::image::Handle,
) -> Task<Message> {
    // The eager insert above already covers most cases; this
    // re-key handles the (rare) scenario where the iced
    // runtime hands us a different id than the one returned
    // by `window::open` synchronously. Idempotent insert.
    state
        .pins
        .entry(id)
        .or_insert_with(|| crate::app::PinState::new(handle));
    Task::none()
}

/// `Message::PinClosePressed`.
pub(super) fn on_pin_close_pressed(state: &mut App, id: iced::window::Id) -> Task<Message> {
    state.windows.forget(id);
    state.pins.remove(&id);
    if state.hovered_pin == Some(id) {
        state.hovered_pin = None;
    }
    window::close(id)
}

/// `Message::PinKeyPressed`.
pub(super) fn on_pin_key_pressed(
    state: &mut App,
    window: iced::window::Id,
    key: iced::keyboard::Key,
    modifiers: iced::keyboard::Modifiers,
) -> Task<Message> {
    // The keyboard subscription is global; only act when the
    // focused window is actually a live pin. Copy the opacity out
    // so the immutable pin borrow is dropped before `update`
    // needs `&mut state`.
    let Some(opacity) = state.pins.get(&window).map(|pin| pin.opacity) else {
        return Task::none();
    };
    match pin_key_action(&key, modifiers) {
        Some(PinKeyAction::Close) => update(state, Message::PinClosePressed(window)),
        Some(PinKeyAction::ToggleLock) => update(state, Message::PinLockToggled(window)),
        Some(PinKeyAction::OpacityUp) => {
            let next = (opacity + PIN_OPACITY_STEP).clamp(PIN_OPACITY_MIN, PIN_OPACITY_MAX);
            update(state, Message::PinOpacityChanged(window, next))
        }
        Some(PinKeyAction::OpacityDown) => {
            let next = (opacity - PIN_OPACITY_STEP).clamp(PIN_OPACITY_MIN, PIN_OPACITY_MAX);
            update(state, Message::PinOpacityChanged(window, next))
        }
        Some(PinKeyAction::Nudge(dx, dy)) => {
            // iced exposes no relative move; read the current
            // logical position, then move_to the offset point.
            window::position(window)
                .and_then(move |p| window::move_to(window, iced::Point::new(p.x + dx, p.y + dy)))
        }
        None => Task::none(),
    }
}

/// `Message::PinHovered(id, true)`.
pub(super) fn on_pin_hovered(state: &mut App, id: iced::window::Id) -> Task<Message> {
    state.hovered_pin = Some(id);
    Task::none()
}

/// `Message::PinHovered(id, false)`.
pub(super) fn on_pin_unhovered(state: &mut App, id: iced::window::Id) -> Task<Message> {
    if state.hovered_pin == Some(id) {
        state.hovered_pin = None;
    }
    Task::none()
}

/// `Message::PinOpacityChanged`.
pub(super) fn on_pin_opacity_changed(
    state: &mut App,
    id: iced::window::Id,
    opacity: f32,
) -> Task<Message> {
    if let Some(pin) = state.pins.get_mut(&id) {
        pin.opacity = opacity.clamp(0.2, 1.0);
    }
    Task::none()
}

/// `Message::PinLockToggled`.
pub(super) fn on_pin_lock_toggled(state: &mut App, id: iced::window::Id) -> Task<Message> {
    if let Some(pin) = state.pins.get_mut(&id) {
        pin.locked = !pin.locked;
    }
    Task::none()
}

/// `Message::EditorPinRendered`: a beautified pin's image is ready.
pub(super) fn on_pin_rendered(
    state: &mut App,
    result: Result<image::RgbaImage, String>,
    display_bounds: Option<(f32, f32, f32, f32)>,
) -> Task<Message> {
    match result {
        Ok(img) => open_pin_window(state, img, display_bounds),
        Err(e) => {
            tracing::warn!(target: readshot_core::log::cat::EDITOR, "pin render failed: {e}");
            state.last_capture_status = Some(e);
            Task::none()
        }
    }
}

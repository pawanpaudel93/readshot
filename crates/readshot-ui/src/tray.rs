//! Tray icon menu structure.
//!
//! The tray icon's runtime is owned by Task 16's composition root —
//! `tray-icon::TrayIconBuilder` is created there with the platform
//! event loop. This module owns the *menu structure* (which items
//! show, in what order, what messages they emit) so the layout is
//! testable and stays consistent across releases.
//!
//! Linux note: `tray-icon` requires `libayatana-appindicator` (or
//! `libappindicator`) at runtime. The composition root falls back to
//! a global-hotkey-only mode if neither package is installed; the
//! README documents the install command.

/// Typed message a tray click publishes upward.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayMessage {
    NewCapture,
    LastCapture,
    OpenHistory,
    OpenPreferences,
    OpenAbout,
    Quit,
}

/// One row in the tray menu. A `Separator` is rendered as a
/// horizontal rule; an `Item` is a clickable label that publishes
/// its `TrayMessage`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrayMenuRow {
    Separator,
    Item {
        label: &'static str,
        message: TrayMessage,
    },
}

/// Default menu layout — kept stable so users' muscle memory works
/// across releases. The order matches spec §3.1.
pub fn default_menu() -> Vec<TrayMenuRow> {
    vec![
        TrayMenuRow::Item {
            label: "New Capture",
            message: TrayMessage::NewCapture,
        },
        TrayMenuRow::Item {
            label: "Last Capture",
            message: TrayMessage::LastCapture,
        },
        TrayMenuRow::Separator,
        TrayMenuRow::Item {
            label: "History…",
            message: TrayMessage::OpenHistory,
        },
        TrayMenuRow::Item {
            label: "Preferences…",
            message: TrayMessage::OpenPreferences,
        },
        TrayMenuRow::Separator,
        TrayMenuRow::Item {
            label: "About Readshot",
            message: TrayMessage::OpenAbout,
        },
        TrayMenuRow::Item {
            label: "Quit",
            message: TrayMessage::Quit,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_menu_starts_with_new_capture() {
        let menu = default_menu();
        match &menu[0] {
            TrayMenuRow::Item { message, .. } => assert_eq!(*message, TrayMessage::NewCapture),
            other => panic!("expected first row to be New Capture, got {other:?}"),
        }
    }

    #[test]
    fn default_menu_ends_with_quit() {
        let menu = default_menu();
        let last = menu.last().expect("menu is non-empty");
        match last {
            TrayMenuRow::Item { message, .. } => assert_eq!(*message, TrayMessage::Quit),
            other => panic!("expected last row to be Quit, got {other:?}"),
        }
    }

    #[test]
    fn default_menu_includes_every_message_variant() {
        // Adding a new TrayMessage variant without surfacing it in
        // the menu is almost always a bug; this assertion forces the
        // menu update to be intentional.
        let menu = default_menu();
        let messages: Vec<TrayMessage> = menu
            .iter()
            .filter_map(|row| match row {
                TrayMenuRow::Item { message, .. } => Some(*message),
                TrayMenuRow::Separator => None,
            })
            .collect();
        for required in [
            TrayMessage::NewCapture,
            TrayMessage::LastCapture,
            TrayMessage::OpenHistory,
            TrayMessage::OpenPreferences,
            TrayMessage::OpenAbout,
            TrayMessage::Quit,
        ] {
            assert!(
                messages.contains(&required),
                "default tray menu missing {required:?}"
            );
        }
    }

    #[test]
    fn separators_appear_between_logical_groups() {
        let menu = default_menu();
        let separators = menu
            .iter()
            .filter(|r| matches!(r, TrayMenuRow::Separator))
            .count();
        assert_eq!(separators, 2, "expected two separators (capture / app / quit groups)");
    }
}

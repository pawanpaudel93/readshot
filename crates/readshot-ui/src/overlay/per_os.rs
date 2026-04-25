//! OS-specific window attributes for the capture overlay.
//!
//! Each helper returns an [`OverlayWindowAttrs`] value that the iced
//! daemon (Task 16) translates into the matching `iced::window::Settings`
//! plus any platform-level shims the toolkit doesn't expose directly:
//!
//! * macOS — `NSWindow.Level.screenSaver`. iced 0.14 doesn't surface
//!   the level at the public API; Task 16 reaches through the
//!   `WindowAttributesExtMacOS` extension trait from `winit` once iced
//!   gives us the underlying handle. Until then this struct documents
//!   the intent.
//! * Windows — `WS_EX_TOPMOST`. iced exposes `level: WindowLevel::AlwaysOnTop`
//!   directly; we forward that.
//! * Linux — Wayland compositors with `wlr-layer-shell` get the overlay
//!   layer; X11 sets `_NET_WM_STATE_ABOVE`. iced selects the underlying
//!   path automatically based on the runtime windowing system.
//!
//! Every function is pure so it can be unit-tested without an event
//! loop or windowing system; Task 16 covers the integration.

/// Attributes that determine how the overlay window stacks and
/// renders. Two pieces of information per OS: stacking level and
/// whether the toolkit understands transparency natively.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OverlayWindowAttrs {
    pub level: OverlayWindowLevel,
    pub transparent: bool,
    pub decorated: bool,
}

/// Stacking level for the overlay window. Maps to:
/// * `Normal` → default app window
/// * `AlwaysOnTop` → above other normal windows
/// * `OverEverything` → screen-saver / drop-down level on macOS,
///   `wlr-layer-shell` overlay layer on Wayland. The strongest level
///   we use; reserved for the capture overlay.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OverlayWindowLevel {
    Normal,
    AlwaysOnTop,
    OverEverything,
}

pub fn macos_overlay_attrs() -> OverlayWindowAttrs {
    OverlayWindowAttrs {
        level: OverlayWindowLevel::OverEverything,
        transparent: true,
        decorated: false,
    }
}

pub fn windows_overlay_attrs() -> OverlayWindowAttrs {
    OverlayWindowAttrs {
        level: OverlayWindowLevel::AlwaysOnTop,
        transparent: true,
        decorated: false,
    }
}

pub fn linux_overlay_attrs() -> OverlayWindowAttrs {
    OverlayWindowAttrs {
        level: OverlayWindowLevel::OverEverything,
        transparent: true,
        decorated: false,
    }
}

/// Pick the appropriate per-OS attributes for the current target. Task
/// 16 calls this once per attached display to produce the iced window
/// settings.
///
/// `#[allow(clippy::needless_return)]` because cfg attributes attach to
/// statement-position expressions, not to tail expressions; explicit
/// returns are the canonical idiom here.
#[allow(clippy::needless_return)]
pub fn current_overlay_attrs() -> OverlayWindowAttrs {
    #[cfg(target_os = "macos")]
    {
        return macos_overlay_attrs();
    }
    #[cfg(target_os = "windows")]
    {
        return windows_overlay_attrs();
    }
    #[cfg(target_os = "linux")]
    {
        return linux_overlay_attrs();
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        compile_error!("readshot-ui: unsupported target_os");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_os_uses_undecorated_transparent_overlay() {
        for attrs in [
            macos_overlay_attrs(),
            windows_overlay_attrs(),
            linux_overlay_attrs(),
        ] {
            assert!(
                attrs.transparent,
                "overlay must be transparent so we can dim around the selection"
            );
            assert!(
                !attrs.decorated,
                "overlay must be undecorated — no title bar / borders"
            );
        }
    }

    #[test]
    fn macos_and_linux_use_strongest_stacking_level() {
        assert_eq!(
            macos_overlay_attrs().level,
            OverlayWindowLevel::OverEverything,
            "macOS overlay sits at NSWindow.Level.screenSaver"
        );
        assert_eq!(
            linux_overlay_attrs().level,
            OverlayWindowLevel::OverEverything,
            "Linux overlay uses wlr-layer-shell overlay layer where available"
        );
    }

    #[test]
    fn windows_uses_always_on_top() {
        // Windows doesn't expose a stronger-than-topmost level at the
        // user-app layer; AlwaysOnTop is the practical equivalent.
        assert_eq!(
            windows_overlay_attrs().level,
            OverlayWindowLevel::AlwaysOnTop
        );
    }
}

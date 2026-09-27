//! Runtime app lifecycle events delivered outside iced.
//!
//! On macOS, opening an already-running `LSUIElement` app from Finder
//! does not start a second process. Launch Services sends the running
//! process a reopen AppleEvent instead. We queue that callback here so
//! the iced runtime can open/focus the ready window on its next tick.

use std::sync::atomic::{AtomicUsize, Ordering};

static REOPEN_REQUESTS: AtomicUsize = AtomicUsize::new(0);

pub fn queue_reopen_request() {
    REOPEN_REQUESTS.fetch_add(1, Ordering::AcqRel);
}

pub fn take_reopen_requests() -> usize {
    REOPEN_REQUESTS.swap(0, Ordering::AcqRel)
}

pub fn install_platform_handler() -> Result<(), AppEventInstallError> {
    platform::install()
}

#[derive(Debug, thiserror::Error)]
pub enum AppEventInstallError {
    #[error("app lifecycle events are only available on macOS")]
    Unsupported,
    #[cfg(target_os = "macos")]
    #[error("NSAppleEventManager class is unavailable")]
    MissingAppleEventManager,
    #[cfg(target_os = "macos")]
    #[error("NSAppleEventManager returned nil")]
    NilAppleEventManager,
}

#[cfg(target_os = "macos")]
mod platform {
    use std::ffi::CStr;
    use std::sync::OnceLock;

    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject, NSObject};
    use objc2::{define_class, msg_send, sel, AnyThread};

    use super::AppEventInstallError;

    static HANDLER: OnceLock<usize> = OnceLock::new();

    /// The reopen AppleEvent handler is a main-thread-only Objective-C
    /// object, stored as a raw `usize` in a `OnceLock` (which erases the
    /// `!Send`/`!Sync` markers). Assert the invariant at runtime in debug
    /// builds; `MainThreadMarker::new()` is `Some` only on the main
    /// thread. Release behaviour is unchanged.
    fn assert_main_thread() {
        debug_assert!(
            objc2::MainThreadMarker::new().is_some(),
            "reopen AppleEvent handler installed off the main thread"
        );
    }

    // AppleEvents.h: kCoreEventClass = 'aevt', kAEReopenApplication = 'rapp'.
    const K_CORE_EVENT_CLASS: u32 = 0x6165_7674;
    const K_AE_REOPEN_APPLICATION: u32 = 0x7261_7070;
    const NS_APPLE_EVENT_MANAGER_CLASS: &CStr = c"NSAppleEventManager";

    define_class!(
        #[unsafe(super(NSObject))]
        #[name = "ReadshotAppEventHandler"]
        struct AppEventHandler;

        impl AppEventHandler {
            #[unsafe(method(handleReopenEvent:withReplyEvent:))]
            unsafe fn handle_reopen_event(&self, _event: &AnyObject, _reply_event: &AnyObject) {
                super::queue_reopen_request();
                tracing::info!(target: "readshot::app-events", "queued reopen AppleEvent");
            }
        }
    );

    impl AppEventHandler {
        fn new() -> Retained<Self> {
            // SAFETY: `AppEventHandler` is an NSObject subclass defined
            // above, and `init` returns the normal retained object.
            unsafe { msg_send![Self::alloc(), init] }
        }
    }

    pub fn install() -> Result<(), AppEventInstallError> {
        assert_main_thread();
        if HANDLER.get().is_some() {
            return Ok(());
        }

        let manager_class = AnyClass::get(NS_APPLE_EVENT_MANAGER_CLASS)
            .ok_or(AppEventInstallError::MissingAppleEventManager)?;

        // SAFETY: NSAppleEventManager is a Foundation singleton. The
        // handler registration runs during iced startup on the app
        // thread, before reopen events are delivered.
        let manager: *mut AnyObject = unsafe { msg_send![manager_class, sharedAppleEventManager] };
        let manager =
            unsafe { manager.as_ref() }.ok_or(AppEventInstallError::NilAppleEventManager)?;

        let handler = AppEventHandler::new();
        // SAFETY: The selector signature matches
        // `handleReopenEvent:withReplyEvent:`: two object parameters
        // and a void return. The event class/id pair is the standard
        // macOS reopen AppleEvent (`aevt`/`rapp`).
        unsafe {
            let _: () = msg_send![
                manager,
                setEventHandler: &*handler,
                andSelector: sel!(handleReopenEvent:withReplyEvent:),
                forEventClass: K_CORE_EVENT_CLASS,
                andEventID: K_AE_REOPEN_APPLICATION
            ];
        }

        let raw = Retained::into_raw(handler) as usize;
        if HANDLER.set(raw).is_err() {
            // SAFETY: `raw` came from `Retained::into_raw` immediately
            // above and was not stored when `set` returned Err.
            unsafe {
                drop(Retained::from_raw(raw as *mut AppEventHandler));
            }
        }

        tracing::info!(target: "readshot::app-events", "registered reopen AppleEvent handler");
        Ok(())
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use super::AppEventInstallError;

    pub fn install() -> Result<(), AppEventInstallError> {
        Err(AppEventInstallError::Unsupported)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queued_reopen_requests_drain_once() {
        let _ = take_reopen_requests();
        queue_reopen_request();
        queue_reopen_request();

        assert_eq!(take_reopen_requests(), 2);
        assert_eq!(take_reopen_requests(), 0);
    }
}

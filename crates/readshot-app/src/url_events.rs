//! Runtime delivery queue for `readshot://` URL events.
//!
//! `main.rs` can hand a launch-time URL directly to the iced runtime,
//! but macOS delivers later URL opens to an AppKit AppleEvent handler.
//! That callback runs outside iced's update function, so it only
//! parses and queues the action. `runtime::subscription` polls this
//! queue and feeds actions back through the normal message path.

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};

use crate::url_scheme::{self, UrlAction, UrlParseError};

static ACTIONS: OnceLock<Mutex<VecDeque<UrlAction>>> = OnceLock::new();
#[cfg(test)]
static TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

#[derive(Debug, thiserror::Error)]
pub enum UrlEventInstallError {
    #[error("running-app URL delivery is only available on macOS")]
    Unsupported,
    #[cfg(target_os = "macos")]
    #[error("NSAppleEventManager class is unavailable")]
    MissingAppleEventManager,
    #[cfg(target_os = "macos")]
    #[error("NSAppleEventManager returned nil")]
    NilAppleEventManager,
}

fn queue() -> &'static Mutex<VecDeque<UrlAction>> {
    ACTIONS.get_or_init(|| Mutex::new(VecDeque::new()))
}

/// Parse a delivered URL and enqueue the resulting action for iced to
/// drain on its next URL-event tick.
pub fn deliver_url_string(raw: &str) -> Result<(), UrlParseError> {
    let action = url_scheme::parse(raw)?;
    queue()
        .lock()
        .expect("url event queue poisoned")
        .push_back(action);
    Ok(())
}

/// Drain every queued action in delivery order.
pub fn drain_actions() -> Vec<UrlAction> {
    let mut guard = queue().lock().expect("url event queue poisoned");
    guard.drain(..).collect()
}

pub fn install_platform_handler() -> Result<(), UrlEventInstallError> {
    platform::install()
}

#[cfg(test)]
pub fn clear_for_tests() {
    queue().lock().expect("url event queue poisoned").clear();
}

#[cfg(test)]
pub fn lock_for_tests() -> std::sync::MutexGuard<'static, ()> {
    TEST_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .expect("url event test lock poisoned")
}

#[cfg(target_os = "macos")]
mod platform {
    use std::ffi::{c_char, CStr};
    use std::sync::OnceLock;

    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject, NSObject};
    use objc2::{define_class, msg_send, sel, AnyThread};

    use super::UrlEventInstallError;

    static HANDLER: OnceLock<usize> = OnceLock::new();

    /// The AppleEvent handler is a main-thread-only Objective-C object,
    /// stored as a raw `usize` in a `OnceLock` (which erases the
    /// `!Send`/`!Sync` markers). Assert the invariant at runtime in debug
    /// builds; `MainThreadMarker::new()` is `Some` only on the main
    /// thread. Release behaviour is unchanged.
    fn assert_main_thread() {
        debug_assert!(
            objc2::MainThreadMarker::new().is_some(),
            "readshot:// URL AppleEvent handler installed off the main thread"
        );
    }

    // InternetConfig.h: kInternetEventClass = 'GURL', kAEGetURL = 'GURL'.
    const K_INTERNET_EVENT_CLASS: u32 = 0x4755_524c;
    const K_AE_GET_URL: u32 = 0x4755_524c;
    // AppleEvents.h: keyDirectObject = '----'.
    const KEY_DIRECT_OBJECT: u32 = 0x2d2d_2d2d;
    const NS_APPLE_EVENT_MANAGER_CLASS: &CStr = c"NSAppleEventManager";

    define_class!(
        #[unsafe(super(NSObject))]
        #[name = "ReadshotUrlEventHandler"]
        struct UrlEventHandler;

        impl UrlEventHandler {
            #[unsafe(method(handleGetURLEvent:withReplyEvent:))]
            unsafe fn handle_get_url_event(&self, event: &AnyObject, _reply_event: &AnyObject) {
                if let Some(url) = unsafe { url_from_apple_event(event) } {
                    match super::deliver_url_string(&url) {
                        Ok(()) => tracing::info!(
                            target: "readshot::url",
                            "queued readshot:// AppleEvent action",
                        ),
                        Err(e) => tracing::warn!(
                            target: "readshot::url",
                            "ignoring AppleEvent URL `{url}`: {e}",
                        ),
                    }
                } else {
                    tracing::warn!(
                        target: "readshot::url",
                        "ignoring AppleEvent without a direct-object URL",
                    );
                }
            }
        }
    );

    impl UrlEventHandler {
        fn new() -> Retained<Self> {
            // SAFETY: `UrlEventHandler` is an NSObject subclass defined
            // above, and `init` returns the normal retained object.
            unsafe { msg_send![Self::alloc(), init] }
        }
    }

    pub fn install() -> Result<(), UrlEventInstallError> {
        assert_main_thread();
        if HANDLER.get().is_some() {
            return Ok(());
        }

        let manager_class = AnyClass::get(NS_APPLE_EVENT_MANAGER_CLASS)
            .ok_or(UrlEventInstallError::MissingAppleEventManager)?;

        // SAFETY: NSAppleEventManager is a Foundation singleton. The
        // handler registration runs during iced startup on the app
        // thread, before URL events are delivered.
        let manager: *mut AnyObject = unsafe { msg_send![manager_class, sharedAppleEventManager] };
        let manager =
            unsafe { manager.as_ref() }.ok_or(UrlEventInstallError::NilAppleEventManager)?;

        let handler = UrlEventHandler::new();
        // SAFETY: The selector signature matches
        // `handleGetURLEvent:withReplyEvent:`: two object parameters
        // and a void return. The event class/id pair is the standard
        // macOS URL-open AppleEvent (`GURL`/`GURL`).
        unsafe {
            let _: () = msg_send![
                manager,
                setEventHandler: &*handler,
                andSelector: sel!(handleGetURLEvent:withReplyEvent:),
                forEventClass: K_INTERNET_EVENT_CLASS,
                andEventID: K_AE_GET_URL
            ];
        }

        let raw = Retained::into_raw(handler) as usize;
        if HANDLER.set(raw).is_err() {
            // SAFETY: `raw` came from `Retained::into_raw` immediately
            // above and was not stored when `set` returned Err.
            unsafe {
                drop(Retained::from_raw(raw as *mut UrlEventHandler));
            }
        }

        tracing::info!(target: "readshot::url", "registered readshot:// AppleEvent handler");
        Ok(())
    }

    unsafe fn url_from_apple_event(event: &AnyObject) -> Option<String> {
        // SAFETY: The incoming object is an NSAppleEventDescriptor
        // supplied by AppKit. `descriptorForKeyword:` returns the
        // direct-object descriptor that contains the URL string.
        let descriptor: *mut AnyObject =
            unsafe { msg_send![event, descriptorForKeyword: KEY_DIRECT_OBJECT] };
        let descriptor = unsafe { descriptor.as_ref()? };

        // SAFETY: `stringValue` returns an autoreleased NSString for
        // text descriptors. We use it only during this callback.
        let string: *mut AnyObject = unsafe { msg_send![descriptor, stringValue] };
        let string = unsafe { string.as_ref()? };

        // SAFETY: `UTF8String` returns a NUL-terminated pointer valid
        // as long as the NSString is alive, which it is for the rest
        // of this method.
        let bytes: *const c_char = unsafe { msg_send![string, UTF8String] };
        if bytes.is_null() {
            return None;
        }
        Some(
            unsafe { CStr::from_ptr(bytes) }
                .to_string_lossy()
                .into_owned(),
        )
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use super::UrlEventInstallError;

    pub fn install() -> Result<(), UrlEventInstallError> {
        Err(UrlEventInstallError::Unsupported)
    }
}

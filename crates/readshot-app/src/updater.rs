//! Sparkle auto-update bridge.
//!
//! The release pipeline publishes a Sparkle appcast, but the running
//! macOS app still needs to host Sparkle's updater controller and wire
//! the tray menu's "Check for Updates…" command to it. Keep the
//! Objective-C boundary isolated here so the rest of the app stays
//! ordinary Rust.

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum UpdaterError {
    #[error("Sparkle updates are only available in the macOS app bundle")]
    Unsupported,
    #[cfg(target_os = "macos")]
    #[error("could not load Sparkle.framework: {0}")]
    FrameworkLoad(String),
    #[cfg(target_os = "macos")]
    #[error("Sparkle class SPUStandardUpdaterController is unavailable")]
    MissingControllerClass,
    #[cfg(target_os = "macos")]
    #[error("Sparkle returned a nil updater controller")]
    NilController,
    #[cfg(target_os = "macos")]
    #[error("Sparkle updater has not been installed")]
    NotInstalled,
}

#[cfg(target_os = "macos")]
mod platform {
    use std::ffi::{CStr, CString};
    use std::sync::{Mutex, OnceLock};

    use objc2::msg_send;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject};

    use super::UpdaterError;

    static CONTROLLER: OnceLock<usize> = OnceLock::new();
    static LAST_INSTALL_ERROR: OnceLock<Mutex<Option<UpdaterError>>> = OnceLock::new();

    const SPARKLE_CONTROLLER_CLASS: &CStr = c"SPUStandardUpdaterController";
    const FRAMEWORK_CANDIDATES: &[&str] = &[
        "@executable_path/../Frameworks/Sparkle.framework/Sparkle",
        "/Applications/Sparkle.app/Contents/SharedSupport/Sparkle.framework/Sparkle",
    ];

    pub fn install() -> Result<(), UpdaterError> {
        let result = install_controller();
        record_install_result(&result);
        result
    }

    fn install_controller() -> Result<(), UpdaterError> {
        if CONTROLLER.get().is_some() {
            return Ok(());
        }

        load_sparkle_framework()?;
        let class =
            AnyClass::get(SPARKLE_CONTROLLER_CLASS).ok_or(UpdaterError::MissingControllerClass)?;

        // SAFETY: Sparkle documents SPUStandardUpdaterController as a
        // main-thread API. `runtime::start` calls this during iced
        // daemon startup on the app thread before tray events are
        // processed. We pass nil delegates and keep the updater
        // stopped until the user explicitly picks "Check for
        // Updates…"; Readshot is otherwise offline by default.
        let controller: Option<Retained<AnyObject>> = unsafe {
            let allocated: *mut AnyObject = msg_send![class, alloc];
            let controller: *mut AnyObject = msg_send![
                allocated,
                initWithStartingUpdater: false,
                updaterDelegate: Option::<&AnyObject>::None,
                userDriverDelegate: Option::<&AnyObject>::None
            ];
            Retained::from_raw(controller)
        };

        let controller = controller.ok_or(UpdaterError::NilController)?;
        configure_manual_checks_only(&controller);
        let raw = Retained::into_raw(controller) as usize;
        if CONTROLLER.set(raw).is_err() {
            // Another caller won the race. Reclaim the extra +1 retain
            // so repeated install attempts do not leak more controllers.
            // SAFETY: `raw` came from `Retained::into_raw` immediately
            // above and has not been shared when `set` returns Err.
            unsafe {
                drop(Retained::from_raw(raw as *mut AnyObject));
            }
        }

        Ok(())
    }

    pub fn check_for_updates() -> Result<(), UpdaterError> {
        check_or_install(
            || CONTROLLER.get().is_some(),
            install,
            invoke_check_for_updates,
            stored_install_error_or_not_installed(),
        )
    }

    fn invoke_check_for_updates() -> Result<(), UpdaterError> {
        let raw = *CONTROLLER.get().ok_or(UpdaterError::NotInstalled)?;

        // SAFETY: `raw` is a process-lifetime +1 retained
        // SPUStandardUpdaterController created by `install`. The tray
        // action runs on the same app thread that owns the menu event
        // pump, which satisfies Sparkle's main-thread requirement.
        unsafe {
            let controller = &*(raw as *mut AnyObject);
            let _: () = msg_send![controller, startUpdater];
            configure_manual_checks_only(controller);
            let _: () = msg_send![controller, checkForUpdates: Option::<&AnyObject>::None];
        }

        Ok(())
    }

    fn configure_manual_checks_only(controller: &AnyObject) {
        // SAFETY: `updater` is a non-null SPUUpdater owned by
        // SPUStandardUpdaterController. The property setter is
        // main-thread-only; every caller of this helper runs from the
        // iced app thread.
        unsafe {
            let updater: *mut AnyObject = msg_send![controller, updater];
            if !updater.is_null() {
                let _: () = msg_send![updater, setAutomaticallyChecksForUpdates: false];
            }
        }
    }

    fn check_or_install<HasController, InstallController, CheckController>(
        has_controller: HasController,
        mut install_controller: InstallController,
        check_controller: CheckController,
        missing_controller_error: UpdaterError,
    ) -> Result<(), UpdaterError>
    where
        HasController: Fn() -> bool,
        InstallController: FnMut() -> Result<(), UpdaterError>,
        CheckController: FnOnce() -> Result<(), UpdaterError>,
    {
        if !has_controller() {
            install_controller()?;
        }
        if has_controller() {
            check_controller()
        } else {
            Err(missing_controller_error)
        }
    }

    fn record_install_result(result: &Result<(), UpdaterError>) {
        let mut guard = last_install_error()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *guard = result.as_ref().err().cloned();
    }

    fn stored_install_error_or_not_installed() -> UpdaterError {
        last_install_error()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
            .unwrap_or(UpdaterError::NotInstalled)
    }

    fn last_install_error() -> &'static Mutex<Option<UpdaterError>> {
        LAST_INSTALL_ERROR.get_or_init(|| Mutex::new(None))
    }

    fn load_sparkle_framework() -> Result<(), UpdaterError> {
        let mut errors = Vec::new();
        for path in FRAMEWORK_CANDIDATES {
            match dlopen_framework(path) {
                Ok(()) => return Ok(()),
                Err(e) => errors.push(format!("{path}: {e}")),
            }
        }
        Err(UpdaterError::FrameworkLoad(errors.join("; ")))
    }

    fn dlopen_framework(path: &str) -> Result<(), String> {
        let c_path = CString::new(path).map_err(|e| e.to_string())?;
        // SAFETY: `c_path` is a valid NUL-terminated string for the
        // duration of the call. We intentionally keep the framework
        // loaded for the process lifetime because Sparkle owns classes
        // registered with the Objective-C runtime.
        let handle = unsafe { libc::dlopen(c_path.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
        if handle.is_null() {
            Err(dlerror_string())
        } else {
            Ok(())
        }
    }

    fn dlerror_string() -> String {
        // SAFETY: `dlerror` returns either null or a thread-local
        // C string owned by libSystem until the next dlopen/dlerror
        // call on this thread.
        unsafe {
            let err = libc::dlerror();
            if err.is_null() {
                "unknown dlopen error".to_string()
            } else {
                CStr::from_ptr(err).to_string_lossy().into_owned()
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use std::cell::Cell;

        use super::*;

        #[test]
        fn manual_check_retries_install_when_controller_is_missing() {
            let installed = Cell::new(false);
            let install_attempts = Cell::new(0);
            let checks = Cell::new(0);

            let result = check_or_install(
                || installed.get(),
                || {
                    install_attempts.set(install_attempts.get() + 1);
                    installed.set(true);
                    Ok(())
                },
                || {
                    checks.set(checks.get() + 1);
                    Ok(())
                },
                UpdaterError::NotInstalled,
            );

            assert_eq!(result, Ok(()));
            assert_eq!(install_attempts.get(), 1);
            assert_eq!(checks.get(), 1);
        }

        #[test]
        fn manual_check_surfaces_retry_install_error() {
            let result = check_or_install(
                || false,
                || Err(UpdaterError::FrameworkLoad("missing Sparkle".to_string())),
                || panic!("check should not run without a controller"),
                UpdaterError::NotInstalled,
            );

            assert_eq!(
                result,
                Err(UpdaterError::FrameworkLoad("missing Sparkle".to_string()))
            );
        }

        #[test]
        fn manual_check_surfaces_previous_install_error_if_retry_does_not_install_controller() {
            let result = check_or_install(
                || false,
                || Ok(()),
                || panic!("check should not run without a controller"),
                UpdaterError::FrameworkLoad("startup dlopen failure".to_string()),
            );

            assert_eq!(
                result,
                Err(UpdaterError::FrameworkLoad(
                    "startup dlopen failure".to_string()
                ))
            );
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use super::UpdaterError;

    pub fn install() -> Result<(), UpdaterError> {
        Err(UpdaterError::Unsupported)
    }

    pub fn check_for_updates() -> Result<(), UpdaterError> {
        Err(UpdaterError::Unsupported)
    }
}

pub fn install() -> Result<(), UpdaterError> {
    platform::install()
}

pub fn check_for_updates() -> Result<(), UpdaterError> {
    platform::check_for_updates()
}

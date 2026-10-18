// Readshot's macOS notifications, posted through UNUserNotificationCenter so
// they carry Readshot's name and icon and are listed under Readshot in System
// Settings › Notifications. (The previous `osascript display notification`
// path attributed every notification to Script Editor.)
//
// Compiled into a static library by crates/readshot-app/build.rs and called
// from runtime/platform.rs through one C entry point.

import Foundation
import UserNotifications

/// Rust's logger: `(level, message)`, level 0 = info, 1 = warn, 2 = the
/// notification was not shown because notifications are off for Readshot
/// (`message` is then the body, which Rust shows in the app), 3 =
/// notifications are on (a post was accepted, or a check found them
/// allowed; `message` is empty), 4 = a check found them still off. Called
/// from UserNotifications' completion queues, so it must be thread-safe
/// (it is: it only forwards to `tracing` and atomics).
public typealias ReadshotNotifyLog = @convention(c) (Int32, UnsafePointer<CChar>?) -> Void

/// Posts were handed to the notification center (authorization is checked,
/// and asked for once, asynchronously; a denial is reported through `log`).
private let posted: Int32 = 0
/// Not running from an app bundle (`cargo run`, tests): nothing was posted.
/// UNUserNotificationCenter raises an Objective-C exception without one.
private let unbundled: Int32 = 1
/// A null title or body.
private let invalidArgument: Int32 = 2

private let levelOff: Int32 = 2
private let levelOn: Int32 = 3
private let levelStillOff: Int32 = 4

/// Shows banners even while Readshot is the active app (its Welcome or
/// Settings window has focus): by default the system silently files them
/// in Notification Center instead.
private final class ForegroundPresenter: NSObject, UNUserNotificationCenterDelegate {
    func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        willPresent notification: UNNotification,
        withCompletionHandler completionHandler: @escaping (UNNotificationPresentationOptions) -> Void
    ) {
        completionHandler([.banner, .list])
    }
}

// The center keeps only a weak reference to its delegate.
private let presenter = ForegroundPresenter()
private let installDelegate: Void = {
    UNUserNotificationCenter.current().delegate = presenter
}()

private func runningFromAppBundle() -> Bool {
    let bundle = Bundle.main
    return bundle.bundleIdentifier != nil && bundle.bundleURL.pathExtension == "app"
}

private func emit(_ log: ReadshotNotifyLog?, _ level: Int32, _ message: String) {
    guard let log else { return }
    message.withCString { log(level, $0) }
}

@_cdecl("readshot_notify_post")
public func readshotNotifyPost(
    _ title: UnsafePointer<CChar>?,
    _ body: UnsafePointer<CChar>?,
    _ log: ReadshotNotifyLog?
) -> Int32 {
    guard let title, let body else {
        return invalidArgument
    }
    guard runningFromAppBundle() else {
        return unbundled
    }
    let titleText = String(cString: title)
    let bodyText = String(cString: body)
    _ = installDelegate
    let center = UNUserNotificationCenter.current()

    func add() {
        let content = UNMutableNotificationContent()
        content.title = titleText
        content.body = bodyText
        let request = UNNotificationRequest(
            identifier: UUID().uuidString,
            content: content,
            trigger: nil
        )
        center.add(request) { error in
            if let error {
                emit(log, 1, "could not post notification \"\(titleText)\": \(error.localizedDescription)")
            } else {
                emit(log, levelOn, "")
            }
        }
    }

    center.getNotificationSettings { settings in
        switch settings.authorizationStatus {
        case .authorized, .provisional:
            add()
        case .notDetermined:
            // First notification ever: macOS asks the user once; the answer
            // is remembered and later calls land in the cases above.
            center.requestAuthorization(options: [.alert]) { granted, error in
                if granted {
                    add()
                } else {
                    if let error {
                        emit(log, 0, "notification authorization failed: \(error.localizedDescription)")
                    }
                    emit(log, levelOff, bodyText)
                }
            }
        default:
            // Denied in System Settings › Notifications: respect it.
            emit(log, levelOff, bodyText)
        }
    }
    return posted
}

/// Ask whether Readshot's notifications are allowed now (the user may have
/// turned them back on in System Settings): `log` gets level 3 if they
/// are, 4 if not. Never prompts. Returns 0, or 1 outside an app bundle
/// (nothing checked).
@_cdecl("readshot_notify_check")
public func readshotNotifyCheck(_ log: ReadshotNotifyLog?) -> Int32 {
    guard runningFromAppBundle() else {
        return unbundled
    }
    UNUserNotificationCenter.current().getNotificationSettings { settings in
        switch settings.authorizationStatus {
        case .authorized, .provisional:
            emit(log, levelOn, "")
        default:
            emit(log, levelStillOff, "")
        }
    }
    return posted
}

// Readshot's macOS notifications, posted through UNUserNotificationCenter so
// they carry Readshot's name and icon and are listed under Readshot in System
// Settings › Notifications. (The previous `osascript display notification`
// path attributed every notification to Script Editor.)
//
// Compiled into a static library by crates/readshot-app/build.rs and called
// from runtime/platform.rs through one C entry point.

import Foundation
import UserNotifications

/// Rust's logger: `(level, message)`, level 0 = info, 1 = warn. Called from
/// UserNotifications' completion queues, so it must be thread-safe (it is:
/// it only forwards to `tracing`).
public typealias ReadshotNotifyLog = @convention(c) (Int32, UnsafePointer<CChar>?) -> Void

/// Posts were handed to the notification center (authorization is checked,
/// and asked for once, asynchronously; a denial is reported through `log`).
private let posted: Int32 = 0
/// Not running from an app bundle (`cargo run`, tests): nothing was posted.
/// UNUserNotificationCenter raises an Objective-C exception without one.
private let unbundled: Int32 = 1
/// A null title or body.
private let invalidArgument: Int32 = 2

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
                    let reason = error.map { ": \($0.localizedDescription)" } ?? ""
                    emit(log, 0, "notifications not allowed\(reason); not shown: \(titleText) — \(bodyText)")
                }
            }
        default:
            // Denied in System Settings › Notifications: respect it.
            emit(log, 0, "notifications are turned off for Readshot; not shown: \(titleText) — \(bodyText)")
        }
    }
    return posted
}

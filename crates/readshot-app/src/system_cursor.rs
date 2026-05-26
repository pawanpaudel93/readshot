//! Platform cursor helpers for capture overlays.

use std::sync::atomic::{AtomicBool, Ordering};

static OVERLAY_CURSOR_PUSHED: AtomicBool = AtomicBool::new(false);

pub fn push_crosshair_for_overlay() {
    if OVERLAY_CURSOR_PUSHED.swap(true, Ordering::AcqRel) {
        return;
    }
    platform_push_crosshair();
}

pub fn pop_after_overlay() {
    if !OVERLAY_CURSOR_PUSHED.swap(false, Ordering::AcqRel) {
        return;
    }
    platform_pop();
}

pub fn set_crosshair_for_overlay() {
    if OVERLAY_CURSOR_PUSHED.load(Ordering::Acquire) {
        platform_set_crosshair();
    }
}

#[cfg(target_os = "macos")]
fn platform_push_crosshair() {
    let cursor = macos::new_cursor();
    cursor.push();
    cursor.set();
    macos::store_cursor(cursor);
}

#[cfg(target_os = "macos")]
fn platform_set_crosshair() {
    macos::with_cursor(|cursor| cursor.set());
}

#[cfg(target_os = "macos")]
fn platform_pop() {
    objc2_app_kit::NSCursor::pop_class();
    macos::clear_cursor();
}

#[cfg(target_os = "macos")]
mod macos {
    use std::cell::RefCell;

    use objc2::rc::Retained;
    use objc2::AnyThread;
    use objc2_app_kit::{NSBezierPath, NSColor, NSCursor, NSImage};
    use objc2_foundation::{NSPoint, NSSize};

    thread_local! {
        static OVERLAY_CURSOR: RefCell<Option<Retained<NSCursor>>> = const { RefCell::new(None) };
    }

    #[allow(deprecated)]
    pub fn new_cursor() -> Retained<NSCursor> {
        let size = NSSize {
            width: 24.0,
            height: 24.0,
        };
        let image = NSImage::initWithSize(NSImage::alloc(), size);
        image.lockFocus();
        draw_crosshair(2.8, NSColor::blackColor());
        draw_crosshair(1.1, NSColor::whiteColor());
        image.unlockFocus();

        NSCursor::initWithImage_hotSpot(NSCursor::alloc(), &image, point(12.0, 12.0))
    }

    pub fn store_cursor(cursor: Retained<NSCursor>) {
        OVERLAY_CURSOR.with(|slot| {
            *slot.borrow_mut() = Some(cursor);
        });
    }

    pub fn clear_cursor() {
        OVERLAY_CURSOR.with(|slot| {
            *slot.borrow_mut() = None;
        });
    }

    pub fn with_cursor(f: impl FnOnce(&NSCursor)) {
        OVERLAY_CURSOR.with(|slot| {
            if let Some(cursor) = slot.borrow().as_deref() {
                f(cursor);
            }
        });
    }

    fn draw_crosshair(width: f64, color: Retained<NSColor>) {
        color.set();
        let path = NSBezierPath::bezierPath();
        path.setLineWidth(width);
        path.moveToPoint(point(12.0, 3.0));
        path.lineToPoint(point(12.0, 9.0));
        path.moveToPoint(point(12.0, 15.0));
        path.lineToPoint(point(12.0, 21.0));
        path.moveToPoint(point(3.0, 12.0));
        path.lineToPoint(point(9.0, 12.0));
        path.moveToPoint(point(15.0, 12.0));
        path.lineToPoint(point(21.0, 12.0));
        path.stroke();
    }

    const fn point(x: f64, y: f64) -> NSPoint {
        NSPoint { x, y }
    }
}

#[cfg(not(target_os = "macos"))]
fn platform_push_crosshair() {}

#[cfg(not(target_os = "macos"))]
fn platform_set_crosshair() {}

#[cfg(not(target_os = "macos"))]
fn platform_pop() {}

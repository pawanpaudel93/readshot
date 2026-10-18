//! Test-only counters for work the UI thread must not do: composes,
//! display renders and frees. Production code calls the `count_*`
//! functions at the places that do the work; they compile to nothing
//! outside tests. Kept in one place so test instrumentation does not
//! spread through the modules it watches.

use std::cell::Cell;

thread_local! {
    static COMPOSES: Cell<u64> = const { Cell::new(0) };
    static UI_RENDERS: Cell<u64> = const { Cell::new(0) };
    static SENT_TO_DROPPER: Cell<u64> = const { Cell::new(0) };
}

fn bump(counter: &'static std::thread::LocalKey<Cell<u64>>) {
    counter.with(|c| c.set(c.get() + 1));
}

/// A document composed on this thread (a cached base composes nothing).
pub(crate) fn count_compose() {
    bump(&COMPOSES);
}

/// How many documents were composed on this thread.
pub(crate) fn composes_on_this_thread() -> u64 {
    COMPOSES.with(Cell::get)
}

/// A document render made by the editor's display on this thread.
pub(crate) fn count_ui_render() {
    bump(&UI_RENDERS);
}

/// How many document renders the display made on this thread.
pub(crate) fn ui_renders_on_this_thread() -> u64 {
    UI_RENDERS.with(Cell::get)
}

/// Something sent to the dropper thread from this thread.
pub(crate) fn count_sent_to_dropper() {
    bump(&SENT_TO_DROPPER);
}

/// How many times this thread sent something to the dropper thread.
pub(crate) fn sent_to_dropper() -> u64 {
    SENT_TO_DROPPER.with(Cell::get)
}

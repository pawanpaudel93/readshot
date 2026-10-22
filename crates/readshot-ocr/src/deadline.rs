//! A runtime-free timeout for one future.
//!
//! The Windows engine awaits a WinRT `IAsyncOperation` and has no tokio
//! (the crate pulls tokio in only for macOS and Linux), so it bounds
//! the call with [`with_deadline`]: a small timer thread wakes the task
//! when the deadline passes, and the next poll reports
//! [`DeadlineElapsed`]. Pure std, so it builds and is tested on every
//! platform.

use std::future::{Future, IntoFuture};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// The future didn't finish in time.
#[derive(Debug, PartialEq, Eq)]
pub struct DeadlineElapsed;

/// `inner` (anything awaitable, such as a WinRT `IAsyncOperation`), or
/// [`DeadlineElapsed`] once `limit` has passed.
pub fn with_deadline<F: IntoFuture>(inner: F, limit: Duration) -> Deadline<F::IntoFuture> {
    Deadline {
        inner: Box::pin(inner.into_future()),
        deadline: Instant::now() + limit,
        timer: None,
    }
}

pub struct Deadline<F> {
    inner: Pin<Box<F>>,
    deadline: Instant,
    timer: Option<Timer>,
}

struct Timer {
    waker: Arc<Mutex<Waker>>,
    done: Arc<AtomicBool>,
    thread: JoinHandle<()>,
}

impl<F: Future> Future for Deadline<F> {
    type Output = Result<F::Output, DeadlineElapsed>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if let Poll::Ready(value) = self.inner.as_mut().poll(cx) {
            return Poll::Ready(Ok(value));
        }
        let deadline = self.deadline;
        if Instant::now() >= deadline {
            return Poll::Ready(Err(DeadlineElapsed));
        }
        match &self.timer {
            Some(timer) => {
                if let Ok(mut w) = timer.waker.lock() {
                    w.clone_from(cx.waker());
                }
            }
            None => {
                let waker = Arc::new(Mutex::new(cx.waker().clone()));
                let done = Arc::new(AtomicBool::new(false));
                let (w, d) = (Arc::clone(&waker), Arc::clone(&done));
                let thread = std::thread::spawn(move || {
                    loop {
                        if d.load(Ordering::Acquire) {
                            return;
                        }
                        let now = Instant::now();
                        if now >= deadline {
                            break;
                        }
                        std::thread::park_timeout(deadline - now);
                    }
                    if let Ok(w) = w.lock() {
                        w.wake_by_ref();
                    }
                });
                self.timer = Some(Timer {
                    waker,
                    done,
                    thread,
                });
            }
        }
        Poll::Pending
    }
}

impl<F> Drop for Deadline<F> {
    /// Finished or abandoned early: let the timer thread go now rather
    /// than at the deadline.
    fn drop(&mut self) {
        if let Some(timer) = self.timer.take() {
            timer.done.store(true, Ordering::Release);
            timer.thread.thread().unpark();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ready on its third poll, waking itself each time.
    struct ReadyAfter(u8);

    impl Future for ReadyAfter {
        type Output = &'static str;
        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            if self.0 == 0 {
                return Poll::Ready("done");
            }
            self.0 -= 1;
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    }

    #[tokio::test]
    async fn a_future_that_finishes_in_time_gives_its_value() {
        let got = with_deadline(ReadyAfter(2), Duration::from_secs(5)).await;
        assert_eq!(got, Ok("done"));
    }

    #[tokio::test]
    async fn a_future_that_never_finishes_times_out() {
        let started = Instant::now();
        let got = with_deadline(std::future::pending::<()>(), Duration::from_millis(100)).await;
        assert_eq!(got, Err(DeadlineElapsed));
        assert!(started.elapsed() >= Duration::from_millis(100));
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}

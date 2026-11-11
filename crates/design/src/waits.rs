//! Time a call spends waiting on the network (fetching an icon, a font, an
//! image) inside a snippet's host calls: kept off the snippet's time limit,
//! up to a cap per call so nothing hangs it.

use std::cell::RefCell;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

thread_local! {
    static CURRENT: RefCell<Option<Arc<Waits>>> = const { RefCell::new(None) };
}

/// One call's waits on the network.
pub struct Waits {
    cap: Duration,
    waited: Mutex<Duration>,
}

impl Waits {
    /// Waits of at most `cap` in all.
    pub fn new(cap: Duration) -> Arc<Self> {
        Arc::new(Self { cap, waited: Mutex::new(Duration::ZERO) })
    }

    /// How long the call has waited so far.
    pub fn waited(&self) -> Duration {
        *self.waited.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Runs `run` with what [`network`] waits on this thread counted here.
    pub fn during<T>(self: &Arc<Self>, run: impl FnOnce() -> T) -> T {
        /// Puts back what was counted before, even when `run` panics.
        struct Outer(Option<Arc<Waits>>);
        impl Drop for Outer {
            fn drop(&mut self) {
                CURRENT.with(|current| *current.borrow_mut() = self.0.take());
            }
        }
        let _outer = Outer(CURRENT.with(|current| current.borrow_mut().replace(self.clone())));
        run()
    }
}

/// Runs `wait`, a fetch over the network, counting its time as waiting
/// for the call running on this thread, if any. Refuses, with the cap,
/// once that call has waited its cap.
pub fn network<T>(wait: impl FnOnce() -> T) -> Result<T, Duration> {
    let Some(waits) = CURRENT.with(|current| current.borrow().clone()) else {
        return Ok(wait());
    };
    if waits.waited() >= waits.cap {
        return Err(waits.cap);
    }
    let started = Instant::now();
    let done = wait();
    *waits.waited.lock().unwrap_or_else(|e| e.into_inner()) += started.elapsed();
    Ok(done)
}

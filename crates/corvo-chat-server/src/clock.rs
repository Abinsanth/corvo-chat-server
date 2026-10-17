//! The server's time source. Everything stored or sent as a time comes from
//! here, so tests can move time without sleeping.

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Unix seconds, from the system or set by hand in tests.
#[derive(Clone)]
pub struct Clock(Option<Arc<AtomicI64>>);

impl Clock {
    pub fn system() -> Clock {
        Clock(None)
    }

    /// A clock that stands still at `start` until [`Clock::advance`].
    pub fn manual(start: i64) -> Clock {
        Clock(Some(Arc::new(AtomicI64::new(start))))
    }

    pub fn now(&self) -> i64 {
        match &self.0 {
            Some(t) => t.load(Ordering::SeqCst),
            None => SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64),
        }
    }

    /// Moves a manual clock forward. Panics on a system clock.
    pub fn advance(&self, secs: i64) {
        match &self.0 {
            Some(t) => {
                t.fetch_add(secs, Ordering::SeqCst);
            }
            None => panic!("cannot advance the system clock"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Clock;

    #[test]
    fn manual_clock_advances() {
        let clock = Clock::manual(1_000);
        let shared = clock.clone();
        assert_eq!(clock.now(), 1_000);
        shared.advance(60);
        assert_eq!(clock.now(), 1_060);
        assert!(Clock::system().now() > 1_700_000_000);
    }
}

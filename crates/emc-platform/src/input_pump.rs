//! Coalesced wake ownership and bounded native message-loop work slices.
use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

#[derive(Default)]
pub(crate) struct WakeGate(AtomicBool);
impl WakeGate {
    pub fn request(&self) -> bool {
        !self.0.swap(true, Ordering::AcqRel)
    }
    /// Clear before inspecting commands: a racing producer can then post another
    /// wake, and a nonempty continuation explicitly requests its own wake.
    pub fn received(&self) {
        self.0.store(false, Ordering::Release);
    }
}

pub(crate) struct WorkSlice {
    started: Instant,
    completed: usize,
}
impl WorkSlice {
    pub fn new(now: Instant) -> Self {
        Self {
            started: now,
            completed: 0,
        }
    }
    pub fn complete(&mut self, now: Instant) -> bool {
        self.completed += 1;
        self.completed >= 32
            || now.saturating_duration_since(self.started) >= Duration::from_millis(2)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn producer_races_cannot_erase_a_wake_after_receiver_acknowledges() {
        let gate = WakeGate::default();
        assert!(gate.request());
        for _ in 0..1_000 {
            assert!(!gate.request());
        }
        gate.received();
        assert!(gate.request()); // Producer after ack owns next wake.
        assert!(!gate.request()); // Continuation shares, does not erase, that wake.
        gate.received();
        assert!(gate.request());
    }
    #[test]
    fn continuous_refill_cannot_exceed_the_count_or_time_slice() {
        let now = Instant::now();
        let mut count = WorkSlice::new(now);
        for _ in 0..31 {
            assert!(!count.complete(now));
        }
        assert!(count.complete(now));
        let mut time = WorkSlice::new(now);
        assert!(!time.complete(now + Duration::from_millis(1)));
        assert!(time.complete(now + Duration::from_millis(2)));
    }
}

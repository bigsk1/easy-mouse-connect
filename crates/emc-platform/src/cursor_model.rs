//! Small platform-independent rules for the asynchronous cursor renderer.
use std::{
    sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering},
    time::{Duration, Instant},
};

pub(crate) const RENDER_PROGRESS_BUDGET: Duration = Duration::from_millis(200);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CursorRequirement {
    NativeVisible,
    FallbackRequired,
    SamplingUnavailable,
}
impl CursorRequirement {
    pub fn from_sample(sample: Option<(bool, bool, bool)>) -> Self {
        match sample {
            Some((true, true, true)) => Self::NativeVisible,
            Some(_) => Self::FallbackRequired,
            None => Self::SamplingUnavailable,
        }
    }
    pub fn permits(self, renderer_healthy: bool, progress_age: Duration) -> bool {
        match self {
            Self::NativeVisible => true,
            Self::FallbackRequired => renderer_healthy && progress_age <= RENDER_PROGRESS_BUDGET,
            Self::SamplingUnavailable => false,
        }
    }
}

/// One attempt per deadline, with a capped delay. No busy loop or concurrent
/// renderer is needed to recover repeatedly failing native resources.
#[derive(Default)]
pub(crate) struct RetryBackoff {
    next: Option<Instant>,
    failures: u32,
}
impl RetryBackoff {
    pub fn ready(&self, now: Instant) -> bool {
        self.next.is_none_or(|next| now >= next)
    }
    pub fn failed(&mut self, now: Instant) {
        let delay = Duration::from_millis((100_u64 << self.failures.min(5)).min(2000));
        self.failures = self.failures.saturating_add(1);
        self.next = Some(now + delay);
    }
    pub fn recovered(&mut self) {
        self.next = None;
        self.failures = 0;
    }
}

/// One outstanding latest-state notification, independent of packet count.
#[derive(Default)]
pub(crate) struct LatestWake(AtomicBool);
impl LatestWake {
    pub fn request(&self) -> bool {
        !self.0.swap(true, Ordering::AcqRel)
    }
    pub fn consumed(&self) {
        self.0.store(false, Ordering::Release);
    }
}

/// Bit zero is receiving/active; higher bits identify the current lifecycle.
/// A hidden old frame cannot be valid again after another activation.
#[derive(Default)]
pub(crate) struct CursorLifecycle(AtomicU64);

impl CursorLifecycle {
    pub fn activate(&self) -> bool {
        self.0.fetch_or(1, Ordering::AcqRel) & 1 == 0
    }
    pub fn hide(&self) -> bool {
        self.0
            .try_update(Ordering::AcqRel, Ordering::Acquire, |old| {
                (old & 1 == 1).then_some((old.wrapping_add(2)) & !1)
            })
            .is_ok()
    }
    pub fn current(&self) -> Option<u64> {
        let value = self.0.load(Ordering::Acquire);
        (value & 1 == 1).then_some(value)
    }
    pub fn is_current(&self, frame: u64) -> bool {
        self.current() == Some(frame)
    }
}

/// A short sampling failure must reach the input controller even if the desktop
/// becomes readable before the next controller poll. Bit zero is unavailable;
/// bit one is an unacknowledged fault for the current receiving lifecycle.
#[derive(Default)]
pub(crate) struct CursorSamplingHealth(AtomicU8);
impl CursorSamplingHealth {
    pub fn record_failure(&self) -> bool {
        self.0.fetch_or(3, Ordering::AcqRel) & 1 == 0
    }
    pub fn readable(&self) {
        self.0.fetch_and(!1, Ordering::AcqRel);
    }
    pub fn unavailable(&self) -> bool {
        self.0.load(Ordering::Acquire) & 1 != 0
    }
    pub fn acknowledge_fault(&self) -> bool {
        self.0.fetch_and(!2, Ordering::AcqRel) & 2 != 0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CursorGeometry {
    pub width: i32,
    pub height: i32,
    pub hotspot_x: i32,
    pub hotspot_y: i32,
}

impl CursorGeometry {
    pub fn new(width: i32, height: i32, hotspot_x: u32, hotspot_y: u32) -> Option<Self> {
        if !(1..=512).contains(&width)
            || !(1..=512).contains(&height)
            || hotspot_x >= width as u32
            || hotspot_y >= height as u32
        {
            return None;
        }
        Some(Self {
            width,
            height,
            hotspot_x: hotspot_x as i32,
            hotspot_y: hotspot_y as i32,
        })
    }
    pub fn origin(self, x: i32, y: i32) -> (i32, i32) {
        (
            x.saturating_sub(self.hotspot_x),
            y.saturating_sub(self.hotspot_y),
        )
    }
}

/// Visibility and cursor identity are independent. A hidden valid resize or
/// I-beam handle is still the shape to copy; only null means no candidate.
pub(crate) fn shape_candidate(handle: usize, _showing: bool) -> Option<usize> {
    (handle != 0).then_some(handle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hiding_and_reentering_never_revalidates_an_old_frame() {
        let lifecycle = CursorLifecycle::default();
        assert_eq!(lifecycle.current(), None);
        assert!(lifecycle.activate());
        let old = lifecycle.current().unwrap();
        assert!(!lifecycle.activate());
        assert!(lifecycle.is_current(old));
        lifecycle.hide();
        assert!(!lifecycle.is_current(old));
        assert!(lifecycle.activate());
        assert!(!lifecycle.is_current(old));
        assert_ne!(lifecycle.current(), Some(old));
    }

    #[test]
    fn repeated_updates_coalesce_into_the_same_active_lifecycle() {
        let lifecycle = CursorLifecycle::default();
        lifecycle.activate();
        let frame = lifecycle.current().unwrap();
        for _ in 0..1000 {
            assert!(!lifecycle.activate());
        }
        assert!(lifecycle.is_current(frame));
        lifecycle.hide();
        lifecycle.hide();
        assert_eq!(lifecycle.current(), None);
    }

    #[test]
    fn native_image_and_hotspot_keep_resize_center_aligned() {
        let resize = CursorGeometry::new(48, 48, 24, 24).unwrap();
        assert_eq!(resize.origin(-1920, 300), (-1944, 276));
        let arrow = CursorGeometry::new(64, 64, 3, 2).unwrap();
        assert_eq!(arrow.origin(100, -200), (97, -202));
        assert!(CursorGeometry::new(32, 32, 32, 0).is_none());
        assert!(CursorGeometry::new(0, 32, 0, 0).is_none());
        assert!(CursorGeometry::new(513, 32, 0, 0).is_none());
    }

    #[test]
    fn hiding_on_another_thread_invalidates_an_already_sampled_frame() {
        use std::sync::{Arc, Barrier};
        let lifecycle = Arc::new(CursorLifecycle::default());
        let barrier = Arc::new(Barrier::new(2));
        lifecycle.activate();
        let sampled = lifecycle.current().unwrap();
        let other = lifecycle.clone();
        let gate = barrier.clone();
        let controller = std::thread::spawn(move || {
            other.hide();
            other.activate();
            gate.wait();
        });
        barrier.wait();
        assert!(!lifecycle.is_current(sampled));
        assert!(lifecycle.current().is_some());
        controller.join().unwrap();
    }

    #[test]
    fn hotspot_subtraction_cannot_wrap_at_extreme_virtual_coordinates() {
        let geometry = CursorGeometry::new(512, 512, 511, 511).unwrap();
        assert_eq!(geometry.origin(i32::MIN, i32::MIN), (i32::MIN, i32::MIN));
        assert_eq!(
            geometry.origin(i32::MAX, i32::MAX),
            (i32::MAX - 511, i32::MAX - 511)
        );
    }

    #[test]
    fn short_sampling_failure_stays_pending_after_desktop_recovers() {
        let health = CursorSamplingHealth::default();
        assert!(health.record_failure());
        assert!(!health.record_failure());
        health.readable();
        assert!(!health.unavailable());
        assert!(health.acknowledge_fault());
        assert!(!health.acknowledge_fault());
        assert!(health.record_failure());
    }

    #[test]
    fn ended_lifecycle_ack_does_not_claim_sampling_recovered() {
        let health = CursorSamplingHealth::default();
        health.record_failure();
        assert!(health.acknowledge_fault());
        assert!(health.unavailable());
        health.readable();
        assert!(!health.unavailable());
        assert!(!health.acknowledge_fault());
    }

    #[test]
    fn normal_visible_cursor_does_not_require_a_healthy_optional_renderer() {
        let native = CursorRequirement::from_sample(Some((true, true, true)));
        assert!(native.permits(false, Duration::from_secs(99)));
        assert!(!CursorRequirement::from_sample(None).permits(true, Duration::ZERO));
    }

    #[test]
    fn hidden_or_absent_mouse_requires_healthy_recent_renderer_progress() {
        for sample in [
            (true, false, true),
            (false, true, true),
            (true, true, false),
        ] {
            let required = CursorRequirement::from_sample(Some(sample));
            assert_eq!(required, CursorRequirement::FallbackRequired);
            assert!(!required.permits(false, Duration::ZERO));
            assert!(required.permits(true, Duration::from_millis(200)));
            assert!(!required.permits(true, Duration::from_millis(201)));
        }
    }

    #[test]
    fn resource_recovery_never_revalidates_the_failed_receiving_generation() {
        let lifecycle = CursorLifecycle::default();
        let health = CursorSamplingHealth::default();
        lifecycle.activate();
        let old_frame = lifecycle.current().unwrap();
        health.record_failure();
        lifecycle.hide();
        // Idle prewarming waits for its retry deadline, then can clear
        // unavailable without reactivating ownership or erasing the controller's
        // pending failure notification.
        let now = Instant::now();
        let mut retry = RetryBackoff::default();
        retry.failed(now);
        assert!(!retry.ready(now));
        assert!(retry.ready(now + Duration::from_millis(100)));
        health.readable();
        assert_eq!(lifecycle.current(), None);
        assert!(health.acknowledge_fault());
        lifecycle.activate();
        assert!(!lifecycle.is_current(old_frame));
    }

    #[test]
    fn retry_deadlines_are_bounded_and_success_resets_the_delay() {
        let mut retry = RetryBackoff::default();
        let mut now = Instant::now();
        assert!(retry.ready(now));
        for delay in [100, 200, 400, 800, 1600, 2000, 2000, 2000] {
            retry.failed(now);
            assert!(!retry.ready(now));
            assert!(!retry.ready(now + Duration::from_millis(delay - 1)));
            now += Duration::from_millis(delay);
            assert!(retry.ready(now));
        }
        retry.recovered();
        assert!(retry.ready(now));
        retry.failed(now);
        assert!(retry.ready(now + Duration::from_millis(100)));
    }

    #[test]
    fn movement_wakes_coalesce_until_worker_consumes_latest_state() {
        let wake = LatestWake::default();
        assert!(wake.request());
        for _ in 0..1000 {
            assert!(!wake.request());
        }
        wake.consumed();
        assert!(wake.request());
        assert!(!wake.request());
        wake.consumed();
        assert!(wake.request());
    }

    #[test]
    fn hidden_cursor_shape_is_not_replaced_with_the_arrow() {
        assert_eq!(shape_candidate(123, false), Some(123));
        assert_eq!(shape_candidate(123, true), Some(123));
        assert_eq!(shape_candidate(0, false), None);
    }
}

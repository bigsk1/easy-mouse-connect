//! Source-only raw delivery progress, not a per-hook timestamp bijection.
//! RAWMOUSE movement may coalesce; GetMessageTime describes message queuing.
//! https://learn.microsoft.com/en-us/windows/win32/api/winuser/ns-winuser-rawmouse
//! https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-getmessagetime
use std::time::{Duration, Instant};
const RAW_PROGRESS_TIMEOUT: Duration = Duration::from_millis(250);
const RECENT_RAW_ORDER_WINDOW: Duration = Duration::from_millis(16);

pub(crate) struct RawCaptureHealth {
    relative_motion_seen: bool,
    last_raw: Option<Instant>,
    pending_since: Option<Instant>,
}
impl RawCaptureHealth {
    pub fn new(_: Instant) -> Self {
        Self {
            relative_motion_seen: false,
            last_raw: None,
            pending_since: None,
        }
    }
    pub fn can_capture(&self) -> bool {
        self.relative_motion_seen
    }
    pub fn begin(&mut self, _: Instant) -> bool {
        if !self.can_capture() {
            return false;
        }
        self.pending_since = None;
        true
    }
    /// Call only for accepted physical relative mouse packets, never keyboard,
    /// reflected/tagged input, or unsupported absolute devices. Any packet can
    /// establish delivery progress; only actual relative motion rearms capture.
    pub fn raw_progress(&mut self, now: Instant, _message_time: u32, relative_motion: bool) {
        self.relative_motion_seen |= relative_motion;
        self.last_raw = Some(now);
        self.pending_since = None;
    }
    pub fn suppressed(&mut self, now: Instant, _message_time: u32) {
        // Raw-before-hook and coalesced delivery can acknowledge the observation
        // without exact timestamp equality. A single missing packet inside this
        // recent window cannot be proved individually; this is progress health.
        if self
            .last_raw
            .is_some_and(|at| now.saturating_duration_since(at) <= RECENT_RAW_ORDER_WINDOW)
        {
            return;
        }
        self.pending_since.get_or_insert(now);
    }
    pub fn stalled(&self, now: Instant) -> bool {
        self.pending_since
            .is_some_and(|at| now.saturating_duration_since(at) > RAW_PROGRESS_TIMEOUT)
    }
    /// Routing boundaries require fresh physical relative movement before entry.
    pub fn stop(&mut self) {
        self.invalidate();
    }
    pub fn invalidate(&mut self) {
        self.relative_motion_seen = false;
        self.last_raw = None;
        self.pending_since = None;
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn ready(now: Instant) -> RawCaptureHealth {
        let mut health = RawCaptureHealth::new(now);
        health.raw_progress(now, 10, true);
        assert!(health.begin(now));
        health
    }
    #[test]
    fn coalesced_and_offset_packets_clear_old_suspicion_in_either_order() {
        let now = Instant::now();
        let mut health = ready(now);
        for ms in (20..1_000).step_by(10) {
            let at = now + Duration::from_millis(ms);
            health.suppressed(at, ms as u32);
            health.suppressed(at, ms as u32 + 1);
            health.raw_progress(at + Duration::from_millis(1), ms as u32 + 37, true);
            health.suppressed(at + Duration::from_millis(2), ms as u32 + 2);
        }
        assert!(!health.stalled(now + Duration::from_secs(120)));
    }
    #[test]
    fn raw_before_hook_then_idle_typing_does_not_invent_a_mouse_stall() {
        let now = Instant::now();
        let mut health = ready(now);
        health.raw_progress(now, 100, true);
        health.suppressed(now + Duration::from_millis(2), 97);
        assert!(!health.stalled(now + Duration::from_secs(120)));
    }
    #[test]
    fn sparse_missing_raw_after_long_idle_releases_without_more_activity() {
        let now = Instant::now();
        let mut health = ready(now);
        let later = now + Duration::from_secs(60);
        health.suppressed(later, 20);
        assert!(!health.stalled(later + Duration::from_millis(250)));
        assert!(health.stalled(later + Duration::from_millis(251)));
    }
    #[test]
    fn further_hooks_never_extend_the_first_pending_deadline() {
        let now = Instant::now();
        let mut health = ready(now);
        let first = now + Duration::from_millis(20);
        for ms in 0..=250 {
            health.suppressed(first + Duration::from_millis(ms), ms as u32);
        }
        assert!(health.stalled(first + Duration::from_millis(251)));
        health.raw_progress(first + Duration::from_millis(252), 2, false);
        assert!(!health.stalled(first + Duration::from_secs(1)));
    }
    #[test]
    fn stop_or_failure_requires_new_relative_motion_not_old_samples_or_button_only() {
        let now = Instant::now();
        let mut health = ready(now);
        health.stop();
        assert!(!health.begin(now));
        health.raw_progress(now, 11, false);
        assert!(!health.begin(now));
        health.raw_progress(now, 12, true);
        assert!(health.begin(now));
        health.invalidate();
        assert!(!health.begin(now));
    }
    #[test]
    fn timestamp_rollover_is_irrelevant_to_monotonic_delivery_progress() {
        let now = Instant::now();
        let mut health = ready(now);
        health.suppressed(now + Duration::from_millis(30), u32::MAX);
        health.raw_progress(now + Duration::from_millis(31), 0, true);
        health.suppressed(now + Duration::from_millis(32), 1);
        assert!(!health.stalled(now + Duration::from_secs(1)));
    }
}

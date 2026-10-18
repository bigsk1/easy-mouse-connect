//! Spatial intent gates for source handoff and receiver return. These do not
//! buffer forwarded movement or impose a timed dwell. Raw/hook message times
//! are not treated as event identities. Source admission uses a current cursor
//! sample for each physical raw packet, without a preceding hook observation.
use crate::geometry::{HandoffSite, MonitorLayout};
use std::time::{Duration, Instant};

const RETURN_INTENT_AGE: Duration = Duration::from_millis(100);
const OUTWARD_PRESSURE: i64 = 4;
const ENTRY_TOLERANCE: i64 = 6;
const ENTRY_CLEARANCE: i32 = 8;

fn outward_dominant(dx: i32, dy: i32, left: bool) -> bool {
    (if left { dx < 0 } else { dx > 0 }) && i64::from(dx).abs() > i64::from(dy).abs()
}

#[derive(Default)]
pub(crate) struct SourceHandoff {
    site: Option<HandoffSite>,
    pressure: i64,
}
impl SourceHandoff {
    pub fn clear(&mut self) {
        self.site = None;
        self.pressure = 0;
    }
    /// Called with a current cursor sample during an accepted physical raw
    /// packet. Zero/tangent samples cannot enter, but retain pressure on the same
    /// valid side. Elapsed idle time is not evidence that the edge became invalid.
    /// The transition packet stays local; no motion is buffered or delayed here.
    pub fn raw(
        &mut self,
        point: (i32, i32),
        dx: i32,
        _dy: i32,
        left: bool,
        layout: &MonitorLayout,
    ) -> Option<(i32, i32)> {
        let Some(site) = layout.handoff_site(point.0, point.1, left) else {
            self.clear();
            return None;
        };
        if self.site != Some(site) {
            self.site = Some(site);
            self.pressure = 0;
        }
        let outward = if left { -i64::from(dx) } else { i64::from(dx) };
        if outward < 0 {
            self.clear();
            return None;
        }
        if outward == 0 {
            return None;
        }
        self.pressure = self.pressure.saturating_add(outward);
        if self.pressure < OUTWARD_PRESSURE {
            return None;
        }
        self.clear();
        Some(point)
    }
}

struct Entry {
    point: (i32, i32),
    from_left: bool,
    settled: bool,
    inward_seen: bool,
}
struct OutwardIntent {
    at: Instant,
    pressure: i64,
}
#[derive(Default)]
pub(crate) struct ReceiverReturn {
    entry: Option<Entry>,
    intent: Option<OutwardIntent>,
}
impl ReceiverReturn {
    pub fn clear(&mut self) {
        self.entry = None;
        self.intent = None;
    }
    pub fn entered(&mut self, from_left: bool, point: (i32, i32)) {
        self.entry = Some(Entry {
            point,
            from_left,
            settled: false,
            inward_seen: false,
        });
        self.intent = None;
    }
    /// Observe only successfully inserted remote movement. Keep an eligible
    /// intent briefly until a later OS sample reflects asynchronous SendInput;
    /// sampling an unchanged interior point must not consume it too early.
    pub fn motion(&mut self, now: Instant, dx: i32, dy: i32) {
        let Some(entry) = self.entry.as_mut() else {
            return;
        };
        if outward_dominant(dx, dy, !entry.from_left) {
            entry.inward_seen = true;
        }
        if !outward_dominant(dx, dy, entry.from_left) {
            self.intent = None;
            return;
        }
        let previous = self
            .intent
            .take()
            .filter(|intent| now.saturating_duration_since(intent.at) <= RETURN_INTENT_AGE);
        let pressure = previous
            .map_or(0, |intent| intent.pressure)
            .saturating_add(i64::from(dx).abs());
        self.intent = Some(OutwardIntent { at: now, pressure });
    }
    pub fn sample(&mut self, now: Instant, point: (i32, i32), layout: &MonitorLayout) -> bool {
        let Some(entry) = self.entry.as_mut() else {
            return false;
        };
        if !entry.settled {
            let near_entry = (i64::from(point.0) - i64::from(entry.point.0)).abs()
                <= ENTRY_TOLERANCE
                && (i64::from(point.1) - i64::from(entry.point.1)).abs() <= ENTRY_TOLERANCE;
            let inward_progress = entry.inward_seen
                && layout
                    .edge_clearance(point.0, point.1, entry.from_left)
                    .is_some_and(|distance| distance >= ENTRY_CLEARANCE);
            if near_entry || inward_progress {
                entry.settled = true;
                // Outward input queued before observing the entry warp is not a
                // return gesture. A new movement after settling must request it.
                self.intent = None;
            }
            return false;
        }
        if self
            .intent
            .as_ref()
            .is_some_and(|intent| now.saturating_duration_since(intent.at) > RETURN_INTENT_AGE)
        {
            self.intent = None;
        }
        if layout.on_peer_edge(point.0, point.1, entry.from_left)
            && !layout.handoff_edge(point.0, point.1, entry.from_left)
        {
            // A monitor shoulder/corner or unoccupied point cannot preserve an
            // old horizontal intent for a later vertical slide into a valid edge.
            self.intent = None;
            return false;
        }
        if layout.handoff_edge(point.0, point.1, entry.from_left)
            && self
                .intent
                .as_ref()
                .is_some_and(|intent| intent.pressure >= OUTWARD_PRESSURE)
        {
            self.clear();
            return true;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Desktop;
    fn layout() -> MonitorLayout {
        let monitor = Desktop {
            left: -1800,
            top: -200,
            width: 1800,
            height: 1200,
        };
        MonitorLayout::new(vec![monitor], monitor)
    }
    #[test]
    fn source_raw_first_clamped_slow_diagonal_and_zero_packets_need_no_hook_or_clock() {
        let layout = layout();
        for (left, point, sign) in [(true, (-1800, 300), -1), (false, (-1, 300), 1)] {
            let mut source = SourceHandoff::default();
            // A clamped point can stay identical for every physical raw packet.
            // No hook observation or timestamp is needed, even after long idle.
            assert_eq!(source.raw(point, sign, 20, left, &layout), None);
            assert_eq!(source.raw(point, 0, 0, left, &layout), None);
            assert_eq!(source.raw(point, 0, 50, left, &layout), None);
            assert_eq!(source.raw(point, sign, 1, left, &layout), None);
            assert_eq!(source.raw(point, 0, -30, left, &layout), None);
            assert_eq!(source.raw(point, 2 * sign, 50, left, &layout), Some(point));
            assert_eq!(source.raw(point, sign, 0, left, &layout), None);
        }
    }
    #[test]
    fn source_equal_and_steep_diagonals_cross_but_tangent_alone_never_enters() {
        let layout = layout();
        for (left, point, sign) in [(true, (-1800, 300), -1), (false, (-1, 300), 1)] {
            for dy in [-100, -4, 4, 100] {
                let mut source = SourceHandoff::default();
                for _ in 0..10 {
                    assert_eq!(source.raw(point, 0, dy, left, &layout), None);
                }
                assert_eq!(source.raw(point, 4 * sign, dy, left, &layout), Some(point));
            }
        }
    }
    #[test]
    fn source_inward_corner_interior_and_lifecycle_clear_pressure() {
        let layout = layout();
        let point = (-1, 300);
        for invalid in [(-2, 300), (-1, -200), (-1, 999), (100, 300)] {
            let mut source = SourceHandoff::default();
            assert_eq!(source.raw(point, 2, 0, false, &layout), None);
            assert_eq!(source.raw(invalid, 0, 0, false, &layout), None);
            assert_eq!(source.raw(point, 2, 0, false, &layout), None);
            assert_eq!(source.raw(point, 2, 0, false, &layout), Some(point));
        }
        let mut source = SourceHandoff::default();
        assert_eq!(source.raw(point, 2, 0, false, &layout), None);
        assert_eq!(source.raw(point, -1, 30, false, &layout), None);
        assert_eq!(source.raw(point, 2, 0, false, &layout), None);
        source.clear();
        assert_eq!(source.raw(point, 2, 0, false, &layout), None);
        assert_eq!(source.raw(point, 2, 0, false, &layout), Some(point));
    }
    #[test]
    fn source_pressure_belongs_to_one_side_and_continuous_span() {
        let monitor = |left, top, width, height| Desktop {
            left,
            top,
            width,
            height,
        };
        let screens = MonitorLayout::new(
            vec![monitor(0, 0, 1600, 900), monitor(0, -1000, 1600, 900)],
            monitor(0, 0, 1600, 900),
        );
        let mut source = SourceHandoff::default();
        assert_eq!(source.raw((0, 300), -2, 0, true, &screens), None);
        // Disconnected segments with the same x are distinct ownership sites.
        assert_eq!(source.raw((0, -500), -2, 0, true, &screens), None);
        // The opposite side has independent pressure, even at the same height.
        assert_eq!(source.raw((1599, -500), 2, 0, false, &screens), None);
        assert_eq!(
            source.raw((1599, -500), 2, 0, false, &screens),
            Some((1599, -500))
        );
        let aligned = MonitorLayout::new(
            vec![monitor(0, -900, 1600, 900), monitor(0, 0, 1600, 900)],
            monitor(0, 0, 1600, 900),
        );
        let mut source = SourceHandoff::default();
        assert_eq!(source.raw((0, -20), -2, 0, true, &aligned), None);
        assert_eq!(source.raw((0, 20), 0, 40, true, &aligned), None);
        assert_eq!(source.raw((0, 20), -2, 0, true, &aligned), Some((0, 20)));
    }
    #[test]
    fn continued_source_outward_motion_is_receiver_inward_in_both_directions() {
        let now = Instant::now();
        let layout = layout();
        for (peer_on_left, source_dx) in [(true, -10), (false, 10)] {
            let from_left = !peer_on_left;
            let entry = layout.entry(from_left, 30_000, 12);
            let edge = (if from_left { -1800 } else { -1 }, entry.1);
            let mut receiver = ReceiverReturn::default();
            receiver.entered(from_left, entry);
            // Even a stale sampled edge position cannot turn continuation into
            // return intent. Source outward points toward the receiver interior.
            receiver.motion(now, source_dx, 0);
            assert!(!receiver.sample(now, edge, &layout));
            assert!(!receiver.sample(now, entry, &layout));
            for dy in [0, 10, 100] {
                receiver.motion(now, source_dx, dy);
                assert!(!receiver.sample(now, edge, &layout));
            }
            // An actual fresh reversal still requests return after entry settles.
            receiver.motion(now, -source_dx, 0);
            assert!(receiver.sample(now, edge, &layout));
        }
    }
    #[test]
    fn receiver_never_returns_from_a_pre_entry_position_or_unsettled_outward_input() {
        let now = Instant::now();
        let layout = layout();
        let mut receiver = ReceiverReturn::default();
        receiver.entered(true, (-1788, 300));
        receiver.motion(now, -20, 0);
        assert!(!receiver.sample(now, (-1800, 300), &layout));
        assert!(!receiver.sample(now, (-1788, 300), &layout)); // Entry acknowledged.
        assert!(!receiver.sample(now, (-1800, 300), &layout)); // Old intent discarded.
    }
    #[test]
    fn receiver_fresh_intent_waits_for_os_position_and_is_consumed_once() {
        let now = Instant::now();
        let layout = layout();
        let mut receiver = ReceiverReturn::default();
        receiver.entered(true, (-1788, 300));
        assert!(!receiver.sample(now, (-1788, 300), &layout));
        receiver.motion(now, -4, 0);
        assert!(!receiver.sample(now, (-1788, 300), &layout)); // OS has not applied it yet.
        assert!(receiver.sample(now + Duration::from_millis(10), (-1800, 300), &layout));
        assert!(!receiver.sample(now + Duration::from_millis(11), (-1800, 300), &layout));
    }
    #[test]
    fn receiver_stale_direction_and_vertical_motion_cannot_return() {
        let now = Instant::now();
        let layout = layout();
        let mut receiver = ReceiverReturn::default();
        receiver.entered(true, (-1788, 300));
        receiver.sample(now, (-1788, 300), &layout);
        receiver.motion(now, -5, 0);
        assert!(!receiver.sample(now + Duration::from_millis(101), (-1800, 300), &layout));
        receiver.motion(now, -5, 0);
        receiver.motion(now, 0, 30);
        assert!(!receiver.sample(now, (-1800, 300), &layout));
    }
    #[test]
    fn receiver_corner_contact_does_not_carry_pressure_to_a_later_side_sample() {
        let now = Instant::now();
        let layout = layout();
        let mut receiver = ReceiverReturn::default();
        receiver.entered(true, (-1788, 300));
        receiver.sample(now, (-1788, 300), &layout);
        receiver.motion(now, -20, -2);
        assert!(!receiver.sample(now, (-1800, -200), &layout));
        assert!(!receiver.sample(now, (-1800, 300), &layout));
        receiver.motion(now, -4, 0);
        assert!(receiver.sample(now, (-1800, 300), &layout));
    }
    #[test]
    fn receiver_reentry_discards_previous_owner_motion_and_handles_negative_right_edge() {
        let now = Instant::now();
        let layout = layout();
        let mut receiver = ReceiverReturn::default();
        receiver.entered(true, (-1788, 300));
        receiver.sample(now, (-1788, 300), &layout);
        receiver.motion(now, -10, 0);
        receiver.entered(false, (-13, 300));
        assert!(!receiver.sample(now, (-1, 300), &layout));
        receiver.sample(now, (-13, 300), &layout);
        receiver.motion(now, 4, 0);
        assert!(receiver.sample(now, (-1, 300), &layout));
    }
}

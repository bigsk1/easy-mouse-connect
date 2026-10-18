//! Bounded, content-free native recovery policies. Device handles stay local and
//! ephemeral; neither identities nor individual events are exported.
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    time::{Duration, Instant},
};

const DELIVERY_WINDOW: Duration = Duration::from_millis(250);
const MAX_DEVICES: usize = 64;

/// Evidence of raw activity without LL callbacks is a suspicion, never proof
/// Windows removed a hook. Coalesced packets need not have one callback each.
pub(crate) struct HookDelivery {
    recent_hooks: VecDeque<(u32, Instant)>,
    pending: Option<(Instant, Instant)>,
}
impl Default for HookDelivery {
    fn default() -> Self {
        Self {
            recent_hooks: VecDeque::with_capacity(512),
            pending: None,
        }
    }
}
impl HookDelivery {
    pub fn hook(&mut self, now: Instant, timestamp: u32) {
        self.prune(now);
        self.pending = None;
        if !self.recent_hooks.iter().any(|(time, _)| *time == timestamp) {
            if self.recent_hooks.len() == 512 {
                self.recent_hooks.pop_front();
            }
            self.recent_hooks.push_back((timestamp, now));
        }
    }
    pub fn raw(&mut self, now: Instant, timestamp: u32) {
        self.prune(now);
        if self.recent_hooks.iter().any(|(time, _)| *time == timestamp) {
            return;
        }
        if let Some((first, last)) = &mut self.pending {
            if now.saturating_duration_since(*last) > DELIVERY_WINDOW {
                *first = now;
            }
            *last = now;
        } else {
            self.pending = Some((now, now));
        }
    }
    pub fn suspected_stall(&self) -> bool {
        self.pending
            .is_some_and(|(first, last)| last.saturating_duration_since(first) > DELIVERY_WINDOW)
    }
    pub fn clear(&mut self) {
        self.recent_hooks.clear();
        self.pending = None;
    }
    fn prune(&mut self, now: Instant) {
        while self
            .recent_hooks
            .front()
            .is_some_and(|(_, at)| now.saturating_duration_since(*at) > Duration::from_secs(1))
        {
            self.recent_hooks.pop_front();
        }
    }
}

/// Net physical displacement, not the injected pointer's current screen point.
/// Tiny back-and-forth sensor noise does not accumulate indefinitely.
#[derive(Default)]
pub(crate) struct ReclaimMotion {
    started: Option<Instant>,
    dx: i32,
    dy: i32,
}
impl ReclaimMotion {
    pub fn clear(&mut self) {
        self.started = None;
        self.dx = 0;
        self.dy = 0;
    }
    pub fn moved(&mut self, now: Instant, dx: i32, dy: i32) -> bool {
        if self
            .started
            .is_none_or(|at| now.saturating_duration_since(at) > Duration::from_millis(200))
        {
            self.clear();
            self.started = Some(now);
        }
        self.dx = self.dx.saturating_add(dx).clamp(-64, 64);
        self.dy = self.dy.saturating_add(dy).clamp(-64, 64);
        self.dx * self.dx + self.dy * self.dy >= 64
    }
}

#[derive(Default)]
struct DeviceState {
    keys: BTreeSet<u16>,
    buttons: [bool; 5],
    contributed: bool,
}
pub(crate) struct DeviceLedger {
    devices: BTreeMap<usize, DeviceState>,
    uncertain: bool,
    last_mouse: Option<usize>,
    releases: VecDeque<(ReleasedInput, u32, Instant)>,
    key_holders: [u8; 256],
    button_holders: [u8; 5],
}
impl Default for DeviceLedger {
    fn default() -> Self {
        Self {
            devices: BTreeMap::new(),
            uncertain: false,
            last_mouse: None,
            releases: VecDeque::with_capacity(512),
            key_holders: [0; 256],
            button_holders: [0; 5],
        }
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum ReleasedInput {
    Key(u16),
    Button(usize),
}
pub(crate) struct RemovedDevice {
    pub reset: bool,
    pub keys: Vec<u16>,
    pub buttons: Vec<usize>,
}
impl DeviceLedger {
    fn release(&mut self, input: ReleasedInput, timestamp: u32, now: Instant) {
        if self.releases.iter().any(|(old, time, at)| {
            *old == input
                && *time == timestamp
                && now.saturating_duration_since(*at) <= Duration::from_secs(1)
        }) {
            return;
        }
        if self.releases.len() == 512 {
            self.releases.pop_front();
        }
        self.releases.push_back((input, timestamp, now));
    }
    pub fn key_release(&mut self, vk: u16, timestamp: u32, now: Instant) {
        self.release(ReleasedInput::Key(vk), timestamp, now);
    }
    pub fn button_release(&mut self, index: usize, timestamp: u32, now: Instant) {
        self.release(ReleasedInput::Button(index), timestamp, now);
    }
    fn matches_release(&self, input: ReleasedInput, timestamp: u32, now: Instant) -> bool {
        self.releases.iter().any(|(old, time, at)| {
            *old == input
                && *time == timestamp
                && now.saturating_duration_since(*at) <= Duration::from_secs(1)
        })
    }
    pub fn keep_key_hold(&self, vk: u16, timestamp: u32, now: Instant) -> bool {
        let count = self.key_holders.get(usize::from(vk)).copied().unwrap_or(0);
        count > 1 || (count > 0 && self.matches_release(ReleasedInput::Key(vk), timestamp, now))
    }
    pub fn keep_button_hold(&self, index: usize, timestamp: u32, now: Instant) -> bool {
        let count = self.button_holders[index];
        count > 1
            || (count > 0 && self.matches_release(ReleasedInput::Button(index), timestamp, now))
    }
    pub fn arrived(&mut self, device: usize) {
        let _ = self.state(device);
    }
    fn state(&mut self, device: usize) -> Option<&mut DeviceState> {
        if !self.devices.contains_key(&device) && self.devices.len() >= MAX_DEVICES {
            self.uncertain = true;
            return None;
        }
        Some(self.devices.entry(device).or_default())
    }
    pub fn key(&mut self, device: usize, vk: u16, down: bool, sending: bool) {
        if vk > 255 {
            self.uncertain = true;
            return;
        }
        if let Some(state) = self.state(device) {
            let changed = if down {
                state.keys.insert(vk)
            } else {
                state.keys.remove(&vk)
            };
            state.contributed |= sending;
            if changed {
                if down {
                    self.key_holders[usize::from(vk)] += 1;
                } else {
                    self.key_holders[usize::from(vk)] -= 1;
                }
            }
        }
    }
    pub fn mouse(&mut self, device: usize, changed: &[(usize, bool)], motion: bool, sending: bool) {
        if motion {
            self.last_mouse = Some(device);
        }
        if let Some(state) = self.state(device) {
            let mut differences = [0i8; 5];
            for &(button, down) in changed {
                if state.buttons[button] != down {
                    differences[button] += if down { 1 } else { -1 };
                }
                state.buttons[button] = down;
            }
            state.contributed |= sending;
            for (index, difference) in differences.into_iter().enumerate() {
                self.button_holders[index] =
                    self.button_holders[index].saturating_add_signed(difference);
            }
        }
    }
    pub fn begin_source(&mut self) {
        self.end_source();
        if let Some(device) = self.last_mouse {
            if let Some(state) = self.devices.get_mut(&device) {
                state.contributed = true;
            }
        }
    }
    pub fn end_source(&mut self) {
        for state in self.devices.values_mut() {
            state.contributed = false;
        }
    }
    pub fn has_key(&self, vk: u16) -> bool {
        self.key_holders
            .get(usize::from(vk))
            .is_some_and(|count| *count > 0)
    }
    pub fn has_button(&self, index: usize) -> bool {
        self.button_holders[index] > 0
    }
    pub fn any_held(&self) -> bool {
        self.key_holders.iter().any(|count| *count > 0)
            || self.button_holders.iter().any(|count| *count > 0)
    }
    pub fn remove(&mut self, device: usize, unattributed_holds: bool) -> RemovedDevice {
        if self.last_mouse == Some(device) {
            self.last_mouse = None;
        }
        let Some(state) = self.devices.remove(&device) else {
            return RemovedDevice {
                reset: true,
                keys: vec![],
                buttons: vec![],
            };
        };
        let reset = self.uncertain
            || unattributed_holds
            || state.contributed
            || !state.keys.is_empty()
            || state.buttons.iter().any(|held| *held);
        for key in &state.keys {
            self.key_holders[usize::from(*key)] -= 1;
        }
        for (index, held) in state.buttons.iter().enumerate() {
            if *held {
                self.button_holders[index] -= 1;
            }
        }
        // Another physical keyboard/mouse can still hold the same input.
        let keys = state
            .keys
            .into_iter()
            .filter(|key| !self.has_key(*key))
            .collect();
        let buttons = state
            .buttons
            .into_iter()
            .enumerate()
            .filter_map(|(i, held)| (held && !self.has_button(i)).then_some(i))
            .collect();
        RemovedDevice {
            reset,
            keys,
            buttons,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hook_health_accepts_both_orders_and_idle_does_not_imply_removal() {
        let now = Instant::now();
        let mut health = HookDelivery::default();
        health.hook(now, u32::MAX);
        health.raw(now, u32::MAX);
        health.raw(now, 0);
        health.hook(now, 0);
        assert!(!health.suspected_stall());
        health.raw(now + Duration::from_secs(5), 1);
        assert!(!health.suspected_stall()); // One packet followed by idle is inconclusive.
        health.hook(now + Duration::from_secs(6), 2);
        assert!(!health.suspected_stall());
    }
    #[test]
    fn sustained_raw_without_hooks_requests_recovery_and_callback_clears_it() {
        let now = Instant::now();
        let mut health = HookDelivery::default();
        health.raw(now, 1);
        health.raw(now + Duration::from_millis(100), 2);
        health.raw(now + Duration::from_millis(251), 2);
        assert!(health.suspected_stall());
        health.hook(now + Duration::from_millis(252), 2);
        assert!(!health.suspected_stall());
        health.clear();
        assert!(health.recent_hooks.is_empty());
    }
    #[test]
    fn sparse_unmatched_raw_first_packets_after_long_idle_do_not_imply_a_stall() {
        let now = Instant::now();
        let mut health = HookDelivery::default();
        health.raw(now, 1);
        health.raw(now + Duration::from_secs(120), 2);
        assert!(!health.suspected_stall());
        // The corresponding hook may follow the raw packet on return from idle.
        health.hook(now + Duration::from_secs(120), 2);
        assert!(!health.suspected_stall());
        health.raw(now + Duration::from_secs(180), 3);
        assert!(!health.suspected_stall());
    }
    #[test]
    fn coalesced_raw_packets_do_not_require_one_hook_per_packet() {
        let now = Instant::now();
        let mut health = HookDelivery::default();
        for timestamp in 0..2_000 {
            let at = now + Duration::from_millis(timestamp.into());
            health.raw(at, timestamp);
            if timestamp % 8 == 0 {
                health.hook(at, timestamp);
            }
            assert!(!health.suspected_stall());
        }
        assert!(health.recent_hooks.len() <= 512);
    }
    #[test]
    fn tiny_jitter_does_not_reclaim_but_intentional_motion_does() {
        let now = Instant::now();
        let mut motion = ReclaimMotion::default();
        for _ in 0..1_000 {
            assert!(!motion.moved(now, 1, -1));
            assert!(!motion.moved(now, -1, 1));
        }
        assert!(motion.moved(now, 8, 0));
        motion.clear();
        assert!(!motion.moved(now, 4, 0));
        assert!(!motion.moved(now + Duration::from_millis(201), 4, 0));
        assert!(motion.moved(now + Duration::from_millis(202), 4, 0));
    }
    #[test]
    fn unrelated_known_device_removal_does_not_end_a_source_session() {
        let mut devices = DeviceLedger::default();
        devices.mouse(1, &[], true, false);
        devices.key(2, 65, true, false);
        devices.key(2, 65, false, false);
        devices.begin_source();
        assert!(!devices.remove(2, false).reset);
        assert!(devices.remove(1, false).reset);
    }
    #[test]
    fn held_or_unknown_device_removal_is_conservative_and_other_holds_survive() {
        let mut devices = DeviceLedger::default();
        devices.key(1, 0xa2, true, true);
        devices.key(2, 0xa2, true, true);
        let removed = devices.remove(1, false);
        assert!(removed.reset);
        assert!(removed.keys.is_empty());
        let removed = devices.remove(2, false);
        assert!(removed.reset);
        assert_eq!(removed.keys, vec![0xa2]);
        assert!(devices.remove(99, false).reset);
        devices.arrived(3);
        devices.mouse(3, &[(0, true)], false, false);
        let removed = devices.remove(3, false);
        assert!(removed.reset);
        assert_eq!(removed.buttons, vec![0]);
    }
    #[test]
    fn overflow_and_unattributed_holds_cannot_silently_ignore_removals() {
        let mut devices = DeviceLedger::default();
        for device in 0..=MAX_DEVICES {
            devices.mouse(device, &[], false, false);
        }
        assert_eq!(devices.devices.len(), MAX_DEVICES);
        assert!(devices.remove(0, false).reset);
        let mut devices = DeviceLedger::default();
        devices.mouse(1, &[], false, false);
        assert!(devices.remove(1, true).reset);
    }
    #[test]
    fn two_devices_shared_key_and_button_survive_both_raw_hook_delivery_orders() {
        use crate::keyboard_state::LocalInputState;
        let now = Instant::now();
        for raw_first in [true, false] {
            let mut devices = DeviceLedger::default();
            let mut physical = LocalInputState::default();
            for device in [1, 2] {
                devices.key(device, 0xa2, true, true);
                devices.mouse(device, &[(0, true)], false, true);
            }
            physical.raw_key_down(0xa2);
            physical.protect_suppressed_key_down(0xa2);
            physical.button(0, true);
            physical.protect_suppressed_button_down(0);
            if raw_first {
                devices.key(1, 0xa2, false, false);
                devices.key_release(0xa2, 10, now);
                devices.mouse(1, &[(0, false)], false, false);
                devices.button_release(0, 10, now);
            }
            let update = physical.key_with_remaining_hold(
                0xa2,
                false,
                true,
                devices.keep_key_hold(0xa2, 10, now),
            );
            assert!(!update.reclaim && update.ignored_key_up_reclaim);
            physical.button_with_remaining_hold(0, false, devices.keep_button_hold(0, 10, now));
            if !raw_first {
                devices.key(1, 0xa2, false, false);
                devices.key_release(0xa2, 10, now);
                devices.mouse(1, &[(0, false)], false, false);
                devices.button_release(0, 10, now);
            }
            assert!(devices.any_held() && physical.any_held());
            physical.reconcile(false, false, false, |_| {
                panic!("other physical holder must stay protected")
            });
            assert_eq!(
                physical.union_counts(|vk| devices.has_key(vk), |i| devices.has_button(i)),
                (1, 1)
            );
            // The final device's LL UP may arrive first. Raw admission still
            // blocks until its authentic raw UP establishes no holder remains.
            physical.key_with_remaining_hold(
                0xa2,
                false,
                false,
                devices.keep_key_hold(0xa2, 20, now),
            );
            physical.button_with_remaining_hold(0, false, devices.keep_button_hold(0, 20, now));
            assert!(devices.any_held());
            devices.key(2, 0xa2, false, false);
            devices.mouse(2, &[(0, false)], false, false);
            if !devices.has_key(0xa2) {
                physical.release_key(0xa2);
            }
            if !devices.has_button(0) {
                physical.button(0, false);
            }
            assert!(!devices.any_held() && !physical.any_held());
        }
    }
    #[test]
    fn raw_only_down_blocks_admission_and_union_counts_until_hardware_release() {
        use crate::keyboard_state::LocalInputState;
        let mut devices = DeviceLedger::default();
        let mut physical = LocalInputState::default();
        devices.key(1, 0xa2, true, true);
        devices.key(1, 0xa2, true, true); // Autorepeat is one holder.
        devices.mouse(1, &[(0, true)], false, true);
        assert!(devices.any_held());
        assert_eq!(
            physical.union_counts(|vk| devices.has_key(vk), |i| devices.has_button(i)),
            (1, 1)
        );
        physical.raw_key_down(0xa2);
        physical.button(0, true);
        assert!(physical.any_held());
        devices.key(1, 0xa2, false, false);
        devices.mouse(1, &[(0, false)], false, false);
        physical.release_key(0xa2);
        physical.button(0, false);
        assert!(!devices.any_held());
        assert_eq!(
            physical.union_counts(|vk| devices.has_key(vk), |i| devices.has_button(i)),
            (0, 0)
        );
    }
}

//! Physical bookkeeping and local-only stale-state reconciliation.
use std::collections::BTreeMap;

#[derive(Default)]
pub(crate) struct LocalInputState {
    // True means a DOWN was swallowed by our hook. Windows' asynchronous state
    // cannot prove that this physical hold has ended, even after local reclaim.
    keys: BTreeMap<u16, bool>,
    buttons: [bool; 5],
    protected_buttons: [bool; 5],
}

pub(crate) struct KeyUpdate {
    pub reclaim: bool,
    pub ignored_key_up_reclaim: bool,
    pub panic: bool,
}
#[derive(Default, Debug, PartialEq, Eq)]
pub(crate) struct Reconciled {
    pub keys: usize,
    pub buttons: usize,
}

impl LocalInputState {
    #[cfg(test)]
    pub fn key(&mut self, vk: u16, down: bool, receiving: bool) -> KeyUpdate {
        self.key_with_remaining_hold(vk, down, receiving, false)
    }
    pub fn key_with_remaining_hold(
        &mut self,
        vk: u16,
        down: bool,
        receiving: bool,
        remaining_hold: bool,
    ) -> KeyUpdate {
        if down {
            // Autorepeat after reclaim must not erase the origin protection.
            self.keys.entry(vk).or_insert(false);
        } else if remaining_hold {
            // Another physical device still holds this input. Its logical OS
            // state may nevertheless become UP after the current device's UP.
            self.keys.insert(vk, true);
        } else {
            self.keys.remove(&vk);
        }
        let ctrl = self
            .keys
            .keys()
            .any(|key| matches!(*key, 0x11 | 0xa2 | 0xa3));
        let alt = self
            .keys
            .keys()
            .any(|key| matches!(*key, 0x12 | 0xa4 | 0xa5));
        KeyUpdate {
            reclaim: receiving && down,
            ignored_key_up_reclaim: receiving && !down,
            panic: down && vk == 0x1b && ctrl && alt,
        }
    }
    pub fn button(&mut self, index: usize, down: bool) {
        self.buttons[index] = down;
        if !down {
            self.protected_buttons[index] = false;
        }
    }
    pub fn button_with_remaining_hold(&mut self, index: usize, down: bool, remaining_hold: bool) {
        if !down && remaining_hold {
            self.buttons[index] = true;
            self.protected_buttons[index] = true;
        } else {
            self.button(index, down);
        }
    }
    pub fn protect_suppressed_key_down(&mut self, vk: u16) {
        if let Some(protected) = self.keys.get_mut(&vk) {
            *protected = true;
        }
    }
    pub fn protect_suppressed_button_down(&mut self, index: usize) {
        if self.buttons[index] {
            self.protected_buttons[index] = true;
        }
    }
    #[cfg(test)]
    pub fn key_count(&self) -> usize {
        self.keys.len()
    }
    #[cfg(test)]
    pub fn button_count(&self) -> usize {
        self.buttons.iter().filter(|held| **held).count()
    }
    pub fn any_held(&self) -> bool {
        !self.keys.is_empty() || self.buttons.iter().any(|held| *held)
    }
    pub fn raw_key_down(&mut self, vk: u16) {
        self.keys.entry(vk).or_insert(false);
    }
    pub fn union_counts(
        &self,
        raw_key: impl Fn(u16) -> bool,
        raw_button: impl Fn(usize) -> bool,
    ) -> (usize, usize) {
        let keys = self.keys.len()
            + (0..=255)
                .filter(|vk| raw_key(*vk) && !self.keys.contains_key(vk))
                .count();
        let buttons = self
            .buttons
            .iter()
            .enumerate()
            .filter(|(index, held)| **held || raw_button(*index))
            .count();
        (keys, buttons)
    }
    pub fn protected_key_count(&self) -> usize {
        self.keys.values().filter(|protected| **protected).count()
    }
    pub fn protected_button_count(&self) -> usize {
        self.protected_buttons
            .iter()
            .filter(|protected| **protected)
            .count()
    }
    pub fn has_unattributed_holds(
        &self,
        key_known: impl Fn(u16) -> bool,
        button_known: impl Fn(usize) -> bool,
    ) -> bool {
        self.keys.keys().any(|vk| !key_known(*vk))
            || self
                .buttons
                .iter()
                .enumerate()
                .any(|(i, held)| *held && !button_known(i))
    }
    pub fn release_key(&mut self, vk: u16) {
        self.keys.remove(&vk);
    }
    #[cfg(test)]
    pub fn clear(&mut self) {
        self.keys.clear();
        self.buttons.fill(false);
        self.protected_buttons.fill(false);
    }
    /// Query only outside low-level hooks and while locally owned. Suppressed
    /// source keys and outstanding receiver injections invalidate OS samples.
    /// Suppressed-origin holds survive local reclaim/disconnect and are never
    /// sampled or pruned. An observed physical UP or device removal clears them.
    /// This method only prunes entries; it never adds keys from sampled state.
    pub fn reconcile(
        &mut self,
        sending: bool,
        receiving: bool,
        injected_held: bool,
        mut is_down: impl FnMut(u16) -> bool,
    ) -> Reconciled {
        if sending || receiving || injected_held {
            return Reconciled::default();
        }
        let old_keys = self.keys.len();
        self.keys.retain(|vk, protected| *protected || is_down(*vk));
        let mut buttons = 0;
        for (index, (held, vk)) in self.buttons.iter_mut().zip([1, 2, 4, 5, 6]).enumerate() {
            if *held && !self.protected_buttons[index] && !is_down(vk) {
                *held = false;
                buttons += 1;
            }
        }
        Reconciled {
            keys: old_keys - self.keys.len(),
            buttons,
        }
    }
}

pub(crate) fn normalize_raw_key(vk: u16, scan: u16, extended: bool) -> Option<u16> {
    if vk == 0 || vk >= 255 || scan == 255 {
        return None;
    }
    Some(match vk {
        0x10 => {
            if scan == 0x36 {
                0xa1
            } else {
                0xa0
            }
        }
        0x11 => {
            if extended {
                0xa3
            } else {
                0xa2
            }
        }
        0x12 => {
            if extended {
                0xa5
            } else {
                0xa4
            }
        }
        _ => vk,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cleanup_key_up_does_not_reclaim_but_physical_key_down_does() {
        let mut state = LocalInputState::default();
        let up = state.key(0x41, false, true);
        assert!(!up.reclaim);
        assert!(up.ignored_key_up_reclaim);
        assert!(state.key(0x41, true, true).reclaim);
    }
    #[test]
    fn raw_modifier_normalization_and_anonymous_hold_attribution_are_bounded() {
        assert_eq!(normalize_raw_key(0x11, 0x1d, false), Some(0xa2));
        assert_eq!(normalize_raw_key(0x11, 0x1d, true), Some(0xa3));
        assert_eq!(normalize_raw_key(0x12, 0x38, true), Some(0xa5));
        assert_eq!(normalize_raw_key(0x10, 0x36, false), Some(0xa1));
        assert_eq!(normalize_raw_key(255, 0xff, false), None);
        let mut state = LocalInputState::default();
        state.key(0xa2, true, false);
        state.protect_suppressed_key_down(0xa2);
        state.button(0, true);
        state.protect_suppressed_button_down(0);
        assert_eq!(state.protected_key_count(), 1);
        assert_eq!(state.protected_button_count(), 1);
        assert!(!state.has_unattributed_holds(|vk| vk == 0xa2, |index| index == 0));
        assert!(state.has_unattributed_holds(|_| false, |_| true));
        state.release_key(0xa2);
        state.button(0, false);
        assert!(!state.any_held());
    }
    #[test]
    fn missed_local_key_and_button_releases_are_pruned_but_real_holds_remain() {
        let mut state = LocalInputState::default();
        state.key(0xa2, true, false);
        state.key(0x41, true, false);
        state.button(0, true);
        assert!(state.any_held());
        let pruned = state.reconcile(false, false, false, |vk| vk == 0x41);
        assert_eq!(
            pruned,
            Reconciled {
                keys: 1,
                buttons: 1
            }
        );
        assert_eq!(state.key_count(), 1);
        assert_eq!(state.button_count(), 0);
        state.reconcile(false, false, false, |_| false);
        assert!(!state.any_held());
    }
    #[test]
    fn asynchronous_samples_never_prune_suppressed_or_injected_keys() {
        let mut state = LocalInputState::default();
        state.key(0x41, true, false);
        for (sending, receiving, injected) in [
            (true, false, false),
            (false, true, false),
            (false, false, true),
        ] {
            assert_eq!(
                state.reconcile(sending, receiving, injected, |_| panic!("must not query")),
                Reconciled::default()
            );
            assert_eq!(state.key_count(), 1);
        }
    }
    #[test]
    fn device_removal_clears_stale_handoff_blockers_and_panic_modifiers() {
        let mut state = LocalInputState::default();
        state.key(0xa2, true, false);
        state.key(0xa4, true, false);
        assert!(state.key(0x1b, true, false).panic);
        state.button(3, true);
        state.clear();
        assert!(!state.any_held());
        assert!(!state.key(0x1b, true, false).panic);
    }

    #[test]
    fn suppressed_ctrl_survives_reclaim_and_idle_even_if_windows_reports_up() {
        let mut state = LocalInputState::default();
        state.key(0xa2, true, false);
        state.protect_suppressed_key_down(0xa2);
        state.key(0x41, true, false); // Local pass-through missed its UP.
        let pruned = state.reconcile(false, false, false, |vk| {
            assert_ne!(vk, 0xa2, "suppressed Ctrl must never be sampled");
            false
        });
        assert_eq!(pruned.keys, 1);
        for _ in 0..1_000 {
            state.key(0xa2, true, false); // Autorepeat cannot erase protection.
            assert_eq!(
                state.reconcile(false, false, false, |_| panic!("must not query")),
                Reconciled::default()
            );
        }
        assert!(state.any_held());
        state.key(0xa2, false, false);
        assert!(!state.any_held());
        // A new local DOWN is not protected by an earlier suppressed cycle.
        state.key(0xa2, true, false);
        assert_eq!(state.reconcile(false, false, false, |_| false).keys, 1);
        assert!(!state.any_held());
    }

    #[test]
    fn suppressed_buttons_require_physical_up_or_device_removal() {
        let mut state = LocalInputState::default();
        state.button(0, true);
        state.protect_suppressed_button_down(0);
        state.button(0, true); // Repeated DOWN preserves protection.
        assert_eq!(
            state.reconcile(false, false, false, |_| panic!("must not query")),
            Reconciled::default()
        );
        assert!(state.any_held());
        state.button(0, false);
        assert!(!state.any_held());
        state.button(0, true); // Local pass-through can reconcile normally.
        assert_eq!(state.reconcile(false, false, false, |_| false).buttons, 1);
        state.button(4, true);
        state.protect_suppressed_button_down(4);
        state.key(0xa2, true, false);
        state.protect_suppressed_key_down(0xa2);
        state.clear(); // Native device-removal handling.
        assert!(!state.any_held());
        assert_eq!(
            state.reconcile(false, false, false, |_| panic!("empty ledger")),
            Reconciled::default()
        );
    }

    #[test]
    fn panic_keeps_suppressed_modifier_holds_until_cleanup_ups() {
        let mut state = LocalInputState::default();
        for vk in [0xa2, 0xa4] {
            state.key(vk, true, false);
            state.protect_suppressed_key_down(vk);
        }
        assert!(state.key(0x1b, true, false).panic);
        state.protect_suppressed_key_down(0x1b);
        // Resetting routing does not clear physical hold guards.
        state.reconcile(false, false, false, |_| panic!("must not query"));
        assert_eq!(state.key_count(), 3);
        for vk in [0x1b, 0xa4, 0xa2, 0xa2] {
            let cleanup = state.key(vk, false, true);
            assert!(!cleanup.panic);
            assert!(!cleanup.reclaim);
            assert!(cleanup.ignored_key_up_reclaim);
        }
        assert!(!state.any_held());
    }
}

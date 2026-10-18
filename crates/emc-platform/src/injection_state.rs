//! The inserted-key bound must never prevent release or an existing-key repeat.
use std::collections::BTreeSet;
pub(crate) fn key_admissible(
    keys: &BTreeSet<(u16, u16, bool)>,
    key: (u16, u16, bool),
    down: bool,
) -> bool {
    !down || keys.len() < 256 || keys.contains(&key)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn full_inserted_ledger_allows_release_and_repeat_but_not_a_new_down() {
        let mut held: BTreeSet<_> = (0..256).map(|vk| (vk, vk, false)).collect();
        assert!(key_admissible(&held, (42, 42, false), false));
        assert!(key_admissible(&held, (42, 42, false), true));
        assert!(!key_admissible(&held, (42, 42, true), true));
        held.remove(&(42, 42, false));
        assert!(key_admissible(&held, (42, 42, true), true));
    }
}

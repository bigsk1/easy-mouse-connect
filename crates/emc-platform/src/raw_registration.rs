//! Raw registration readback is not a byte-for-byte echo of registration flags.
//! DEVNOTIFY may be omitted; background delivery and the owning target cannot.
use std::time::{Duration, Instant};

pub(crate) const INPUT_SINK: u32 = 0x0000_0100;
pub(crate) const DEVICE_NOTIFY: u32 = 0x0000_2000;
const ALLOWED_READBACK: u32 = INPUT_SINK | DEVICE_NOTIFY;

#[derive(Clone, Copy, Debug)]
pub(crate) struct RegistrationEntry {
    pub page: u16,
    pub usage: u16,
    pub flags: u32,
    pub target: usize,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RegistrationStatus {
    Owned,
    Mismatch,
    Unavailable,
}

pub(crate) fn inspect_registration(
    target: usize,
    entries: Option<&[RegistrationEntry]>,
) -> RegistrationStatus {
    let Some(entries) = entries else {
        return RegistrationStatus::Unavailable;
    };
    if target == 0 {
        return RegistrationStatus::Mismatch;
    }
    for usage in [2, 6] {
        let mut matching = entries
            .iter()
            .filter(|entry| entry.page == 1 && entry.usage == usage);
        let Some(entry) = matching.next() else {
            return RegistrationStatus::Mismatch;
        };
        // Exclude conflicting selectors, legacy suppression/capture, alternative
        // background modes, and unknown flags. DEVNOTIFY is optional readback.
        if matching.next().is_some()
            || entry.target != target
            || entry.flags & INPUT_SINK == 0
            || entry.flags & !ALLOWED_READBACK != 0
        {
            return RegistrationStatus::Mismatch;
        }
    }
    RegistrationStatus::Owned
}

pub(crate) struct RegistrationAudit {
    admitted: bool,
    next_check: Instant,
    retry: Duration,
}
pub(crate) struct AuditAction {
    pub suspend: bool,
    pub repair: bool,
}
impl RegistrationAudit {
    /// Constructed only after RegisterRawInputDevices has succeeded. Audit it
    /// immediately; future admission cannot be inferred from arriving packets.
    pub fn new(now: Instant) -> Self {
        Self {
            admitted: true,
            next_check: now,
            retry: Duration::from_secs(1),
        }
    }
    pub fn due(&self, now: Instant) -> bool {
        now >= self.next_check
    }
    pub fn admitted(&self) -> bool {
        self.admitted
    }
    pub fn request_check(&mut self, now: Instant) {
        self.next_check = now;
    }
    pub fn observe(&mut self, status: RegistrationStatus, now: Instant) -> AuditAction {
        if status == RegistrationStatus::Owned {
            self.verified(now);
            return AuditAction {
                suspend: false,
                repair: false,
            };
        }
        let suspend = std::mem::replace(&mut self.admitted, false);
        self.next_check = now + self.retry;
        self.retry = (self.retry * 2).min(Duration::from_secs(30));
        AuditAction {
            suspend,
            repair: status == RegistrationStatus::Mismatch,
        }
    }
    pub fn after_repair(&mut self, status: RegistrationStatus, now: Instant) -> bool {
        if status == RegistrationStatus::Owned {
            self.verified(now);
            true
        } else {
            self.admitted = false;
            false
        }
    }
    fn verified(&mut self, now: Instant) {
        self.admitted = true;
        self.retry = Duration::from_secs(1);
        self.next_check = now + Duration::from_secs(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn owned(flags: u32) -> [RegistrationEntry; 2] {
        [2, 6].map(|usage| RegistrationEntry {
            page: 1,
            usage,
            flags,
            target: 42,
        })
    }
    #[test]
    fn omitted_notification_flag_preserves_admission_for_both_classes() {
        let now = Instant::now();
        let mut audit = RegistrationAudit::new(now);
        for flags in [INPUT_SINK, INPUT_SINK | DEVICE_NOTIFY] {
            let status = inspect_registration(42, Some(&owned(flags)));
            assert_eq!(status, RegistrationStatus::Owned);
            let action = audit.observe(status, now);
            assert!(!action.suspend && !action.repair && audit.admitted());
        }
    }
    #[test]
    fn wrong_target_missing_class_and_conflicting_flags_are_authoritative_mismatch() {
        let mut entries = owned(INPUT_SINK);
        entries[1].target = 99;
        assert_eq!(
            inspect_registration(42, Some(&entries)),
            RegistrationStatus::Mismatch
        );
        assert_eq!(
            inspect_registration(42, Some(&entries[..1])),
            RegistrationStatus::Mismatch
        );
        assert_eq!(
            inspect_registration(42, Some(&[])),
            RegistrationStatus::Mismatch
        );
        for forbidden in [1, 0x10, 0x20, 0x30, 0x200, 0x400, 0x1000, 0x8000_0000] {
            assert_eq!(
                inspect_registration(42, Some(&owned(INPUT_SINK | forbidden))),
                RegistrationStatus::Mismatch
            );
        }
        assert_eq!(
            inspect_registration(42, Some(&owned(DEVICE_NOTIFY))),
            RegistrationStatus::Mismatch
        );
        let entries = owned(INPUT_SINK);
        assert_eq!(
            inspect_registration(42, Some(&[entries[0], entries[0], entries[1]])),
            RegistrationStatus::Mismatch
        );
    }
    #[test]
    fn unavailable_is_not_an_authoritative_mismatch_or_reason_to_reregister() {
        let now = Instant::now();
        let mut audit = RegistrationAudit::new(now);
        assert_eq!(
            inspect_registration(42, None),
            RegistrationStatus::Unavailable
        );
        let first = audit.observe(RegistrationStatus::Unavailable, now);
        assert!(first.suspend && !first.repair && !audit.admitted());
        let again = audit.observe(
            RegistrationStatus::Unavailable,
            now + Duration::from_secs(1),
        );
        assert!(!again.suspend && !again.repair && !audit.admitted());
        assert!(!audit.after_repair(RegistrationStatus::Unavailable, now));
        assert!(!audit.admitted());
    }
    #[test]
    fn only_verified_ownership_restores_admission_after_proven_loss() {
        let now = Instant::now();
        let mut audit = RegistrationAudit::new(now);
        let loss = audit.observe(RegistrationStatus::Mismatch, now);
        assert!(loss.suspend && loss.repair && !audit.admitted());
        // Register succeeding, raw packets arriving, and a failed query cannot
        // override an authoritative wrong target. Only a valid snapshot admits.
        assert!(!audit.after_repair(RegistrationStatus::Mismatch, now));
        assert!(!audit.after_repair(RegistrationStatus::Unavailable, now));
        assert!(audit.after_repair(RegistrationStatus::Owned, now));
        assert!(audit.admitted());
    }
    #[test]
    fn repeated_failures_back_off_to_bounded_thirty_seconds_and_success_resets() {
        let start = Instant::now();
        let mut now = start;
        let mut audit = RegistrationAudit::new(now);
        for seconds in [1, 2, 4, 8, 16, 30, 30] {
            assert!(audit.due(now));
            audit.observe(RegistrationStatus::Mismatch, now);
            assert!(!audit.due(now + Duration::from_secs(seconds) - Duration::from_millis(1)));
            now += Duration::from_secs(seconds);
            assert!(audit.due(now));
        }
        audit.after_repair(RegistrationStatus::Owned, now);
        now += Duration::from_secs(1);
        assert!(audit.due(now));
        audit.observe(RegistrationStatus::Unavailable, now);
        assert!(audit.due(now + Duration::from_secs(1)));
        audit.request_check(now);
        assert!(audit.due(now));
    }
}

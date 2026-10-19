//! Optional content-free status notices. Native shell work runs only from the
//! desktop's slow status poll, never from an input callback or clipboard parser.

use std::time::{Duration, Instant};

use emc_core::{ClipboardNotice, NotificationSnapshot};

const NETWORK_NOTICE_INTERVAL: Duration = Duration::from_secs(5);
const CLIPBOARD_NOTICE_INTERVAL: Duration = Duration::from_secs(30);
const MAX_NOTICE_AGE: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NotificationKind {
    Connected,
    Disconnected,
    TextReceived,
    ImageReceived,
}

impl NotificationKind {
    fn received(value: ClipboardNotice) -> Option<Self> {
        match value {
            ClipboardNotice::TextReceived => Some(Self::TextReceived),
            ClipboardNotice::ImageReceived => Some(Self::ImageReceived),
            ClipboardNotice::TextSent | ClipboardNotice::ImageSent => None,
        }
    }

    #[cfg(windows)]
    fn body(self) -> &'static str {
        match self {
            Self::Connected => "The paired PC is connected.",
            Self::Disconnected => "Connection to the paired PC ended.",
            Self::TextReceived => "Text clipboard received from the paired PC.",
            Self::ImageReceived => "Image clipboard received from the paired PC.",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Notice {
    kind: NotificationKind,
    deadline: Instant,
}

#[derive(Clone, Copy)]
struct ObservedSnapshot {
    activation: u64,
    generation: u64,
    enabled: bool,
    connected: bool,
    clipboard_serial: u64,
}

#[derive(Default)]
struct NotificationTracker {
    previous: Option<ObservedSnapshot>,
    last_network_notice: Option<Instant>,
    last_clipboard_notice: Option<Instant>,
}

impl NotificationTracker {
    fn observe(&mut self, snapshot: &NotificationSnapshot, now: Instant) -> Option<Notice> {
        let previous = self.previous.replace(ObservedSnapshot {
            activation: snapshot.activation,
            generation: snapshot.generation,
            enabled: snapshot.enabled,
            connected: snapshot.connected,
            clipboard_serial: snapshot.clipboard_serial,
        });
        if !snapshot.enabled || previous.is_none_or(|old| !old.enabled) {
            // Starting or enabling notifications establishes a baseline. Old
            // clipboard activity and an existing connection are not replayed.
            self.last_network_notice = None;
            self.last_clipboard_notice = None;
            return None;
        }
        let previous = previous?;
        if previous.activation != snapshot.activation {
            // A fast off/on can happen between status polls. The activation is
            // changed under the settings lock, so that interval stays private
            // even if no disabled snapshot reached this tracker.
            self.last_network_notice = None;
            self.last_clipboard_notice = None;
            return None;
        }
        if previous.connected != snapshot.connected {
            let kind = if snapshot.connected {
                NotificationKind::Connected
            } else {
                NotificationKind::Disconnected
            };
            if self
                .last_network_notice
                .is_some_and(|last| now.saturating_duration_since(last) < NETWORK_NOTICE_INTERVAL)
            {
                return None;
            }
            self.last_network_notice = Some(now);
            return Some(Notice {
                kind,
                deadline: now.checked_add(MAX_NOTICE_AGE)?,
            });
        }
        if !snapshot.connected || previous.clipboard_serial == snapshot.clipboard_serial {
            return None;
        }
        let kind = NotificationKind::received(snapshot.clipboard?)?;
        let at = snapshot.clipboard_at?;
        let deadline = at.checked_add(MAX_NOTICE_AGE)?;
        if at > now || now >= deadline {
            // The original completion time remains authoritative through a
            // delayed status poll. Discard old/future/unknown activity rather
            // than replacing its timestamp with the current poll time.
            return None;
        }
        if self
            .last_clipboard_notice
            .is_some_and(|last| now.saturating_duration_since(last) < CLIPBOARD_NOTICE_INTERVAL)
        {
            // Every snapshot is consumed even when rate-limited. There is no
            // backlog to replay after a burst of copies or a network flap.
            return None;
        }
        self.last_clipboard_notice = Some(now);
        Some(Notice { kind, deadline })
    }
}

pub struct StatusNotifications {
    tracker: NotificationTracker,
    visible_deadline: Option<Instant>,
    #[cfg(windows)]
    native: native::ShellNotices,
}

impl StatusNotifications {
    /// The caller supplies its owned main-window HWND. No shell/window lookup
    /// is used to discover or modify Tauri's independently owned tray icon.
    pub fn new(hwnd: usize) -> Self {
        #[cfg(not(windows))]
        let _ = hwnd;
        Self {
            tracker: NotificationTracker::default(),
            visible_deadline: None,
            #[cfg(windows)]
            native: native::ShellNotices::new(hwnd),
        }
    }

    /// `true` means the shell accepted a notice request, not that Windows showed
    /// a banner. Focus Assist, notification policy and shell behavior still win.
    pub fn poll(&mut self, snapshot: &NotificationSnapshot) -> bool {
        let now = Instant::now();
        let activation_changed = self
            .tracker
            .previous
            .is_some_and(|old| old.activation != snapshot.activation);
        let generation_changed = self
            .tracker
            .previous
            .is_some_and(|old| old.generation != snapshot.generation);
        if !snapshot.enabled
            || activation_changed
            || generation_changed
            || self
                .visible_deadline
                .is_some_and(|deadline| now >= deadline)
        {
            self.retire(now);
        }
        let notice = self.tracker.observe(snapshot, now);
        if !snapshot.enabled || activation_changed {
            return false;
        }
        #[cfg(windows)]
        return notice.is_some_and(|notice| {
            self.visible_deadline = Some(notice.deadline);
            self.native.show(notice.kind.body(), notice.deadline)
        });
        #[cfg(not(windows))]
        {
            let _ = notice;
            false
        }
    }

    pub fn clear(&mut self) {
        self.tracker = NotificationTracker::default();
        self.retire(Instant::now());
    }

    fn retire(&mut self, now: Instant) {
        #[cfg(windows)]
        let cleared = self.native.clear();
        #[cfg(not(windows))]
        let cleared = true;
        self.visible_deadline = if cleared {
            None
        } else {
            // Retry a refused cleanup on the next poll without renewing its
            // original visibility deadline or resetting notification policy.
            Some(self.visible_deadline.unwrap_or(now).min(now))
        };
    }
}

#[cfg(windows)]
mod native {
    use std::time::Instant;
    use windows_sys::Win32::{
        Foundation::HWND,
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            Shell::{
                Shell_NotifyIconW, NIF_ICON, NIF_INFO, NIF_REALTIME, NIF_STATE, NIF_TIP, NIIF_INFO,
                NIIF_NOSOUND, NIIF_RESPECT_QUIET_TIME, NIM_ADD, NIM_DELETE, NIM_MODIFY,
                NIM_SETVERSION, NIS_HIDDEN, NOTIFYICONDATAW, NOTIFYICON_VERSION_4,
            },
            WindowsAndMessaging::{IsWindow, LoadIconW, IDI_APPLICATION},
        },
    };

    const NOTICE_ID: u32 = 0x454D;

    pub(super) struct ShellNotices {
        hwnd: usize,
        registered: bool,
        may_exist: bool,
    }

    impl ShellNotices {
        pub(super) fn new(hwnd: usize) -> Self {
            Self {
                hwnd,
                registered: false,
                may_exist: false,
            }
        }

        fn data(&self) -> NOTIFYICONDATAW {
            NOTIFYICONDATAW {
                cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
                hWnd: self.hwnd as HWND,
                uID: NOTICE_ID,
                ..Default::default()
            }
        }

        fn window_available(&self) -> bool {
            self.hwnd != 0 && unsafe { IsWindow(self.hwnd as HWND) } != 0
        }

        fn add(&mut self, deadline: Instant) -> bool {
            if Instant::now() >= deadline || !self.window_available() {
                return false;
            }
            let mut data = self.data();
            data.uFlags = NIF_STATE | NIF_TIP;
            data.dwState = NIS_HIDDEN;
            data.dwStateMask = NIS_HIDDEN;
            copy_static_text(&mut data.szTip, "Easy Mouse Connect");
            // LoadIcon returns shared resources; they must not be destroyed by
            // this helper. Resource 1 is the compiled application icon.
            let module = unsafe { GetModuleHandleW(std::ptr::null()) };
            let mut icon = if module.is_null() {
                std::ptr::null_mut()
            } else {
                // Win32 MAKEINTRESOURCEW(1): the small address is a resource
                // ordinal, not a pointer that the API may dereference.
                unsafe { LoadIconW(module, std::ptr::without_provenance::<u16>(1)) }
            };
            if icon.is_null() {
                icon = unsafe { LoadIconW(std::ptr::null_mut(), IDI_APPLICATION) };
            }
            if !icon.is_null() {
                data.hIcon = icon;
                data.uFlags |= NIF_ICON;
            }
            if Instant::now() >= deadline {
                return false;
            }
            self.registered = unsafe { Shell_NotifyIconW(NIM_ADD, &data) } != 0;
            if self.registered {
                self.may_exist = true;
                let mut version = self.data();
                version.Anonymous.uVersion = NOTIFYICON_VERSION_4;
                // Legacy notification support is still usable if version 4 is
                // declined. No callback messages or user action are installed.
                if Instant::now() < deadline {
                    unsafe { Shell_NotifyIconW(NIM_SETVERSION, &version) };
                }
            }
            if Instant::now() >= deadline {
                self.clear();
                return false;
            }
            self.registered
        }

        pub(super) fn show(&mut self, body: &'static str, deadline: Instant) -> bool {
            if !self.window_available() {
                self.registered = false;
                self.may_exist = false;
                return false;
            }
            // Delete our previous entry before each eligible notice. This
            // cancels an old app-owned balloon instead of building a shell
            // backlog. A missing entry after Explorer restarts is harmless:
            // a successful add proves we own the fresh registration.
            for _ in 0..2 {
                self.clear();
                if Instant::now() >= deadline || !self.add(deadline) {
                    return false;
                }
                let mut data = self.data();
                data.uFlags = NIF_INFO | NIF_REALTIME;
                data.dwInfoFlags = NIIF_INFO | NIIF_NOSOUND | NIIF_RESPECT_QUIET_TIME;
                copy_static_text(&mut data.szInfoTitle, "Easy Mouse Connect");
                copy_static_text(&mut data.szInfo, body);
                if Instant::now() >= deadline {
                    self.clear();
                    return false;
                }
                let accepted = unsafe { Shell_NotifyIconW(NIM_MODIFY, &data) } != 0;
                // A late OS completion cannot renew a notification's age.
                if Instant::now() >= deadline {
                    self.clear();
                    return false;
                }
                if accepted {
                    return true;
                }
                self.registered = false;
            }
            self.clear();
            false
        }

        pub(super) fn clear(&mut self) -> bool {
            if self.may_exist {
                self.may_exist = self.window_available()
                    && unsafe { Shell_NotifyIconW(NIM_DELETE, &self.data()) } == 0;
            }
            self.registered = false;
            !self.may_exist
        }
    }

    impl Drop for ShellNotices {
        fn drop(&mut self) {
            self.clear();
        }
    }

    fn copy_static_text<const N: usize>(destination: &mut [u16; N], text: &'static str) {
        for (slot, unit) in destination
            .iter_mut()
            .take(N.saturating_sub(1))
            .zip(text.encode_utf16())
        {
            *slot = unit;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(at: Instant, enabled: bool, connected: bool, serial: u64) -> NotificationSnapshot {
        NotificationSnapshot {
            activation: 0,
            generation: 1,
            enabled,
            connected,
            clipboard_serial: serial,
            clipboard: Some(ClipboardNotice::TextReceived),
            clipboard_at: Some(at),
        }
    }

    fn kind(
        tracker: &mut NotificationTracker,
        snapshot: &NotificationSnapshot,
        now: Instant,
    ) -> Option<NotificationKind> {
        tracker.observe(snapshot, now).map(|notice| notice.kind)
    }

    #[test]
    fn initial_and_reenabled_snapshots_do_not_replay_history() {
        let now = Instant::now();
        let mut tracker = NotificationTracker::default();
        assert_eq!(
            kind(&mut tracker, &snapshot(now, true, true, 20), now),
            None
        );
        assert_eq!(
            kind(&mut tracker, &snapshot(now, false, false, 21), now),
            None
        );
        assert_eq!(
            kind(&mut tracker, &snapshot(now, true, true, 25), now),
            None
        );
        assert_eq!(
            kind(&mut tracker, &snapshot(now, true, true, 25), now),
            None
        );
        assert_eq!(
            kind(&mut tracker, &snapshot(now, true, true, 26), now),
            Some(NotificationKind::TextReceived)
        );
    }

    #[test]
    fn fast_off_on_between_polls_establishes_a_fresh_activation_baseline() {
        let now = Instant::now();
        let mut tracker = NotificationTracker::default();
        assert_eq!(
            kind(&mut tracker, &snapshot(now, true, true, 10), now),
            None
        );
        let mut current = snapshot(now + Duration::from_secs(1), true, true, 11);
        current.activation = 2;
        assert_eq!(
            kind(&mut tracker, &current, now + Duration::from_secs(1)),
            None
        );
        assert_eq!(
            kind(&mut tracker, &current, now + Duration::from_secs(2)),
            None
        );
        current.clipboard_serial = 12;
        current.clipboard_at = Some(now + Duration::from_secs(3));
        assert_eq!(
            kind(&mut tracker, &current, now + Duration::from_secs(3)),
            Some(NotificationKind::TextReceived)
        );
    }

    #[test]
    fn clipboard_bursts_are_consumed_without_replay_after_the_quiet_cooldown() {
        let now = Instant::now();
        let mut tracker = NotificationTracker::default();
        assert_eq!(kind(&mut tracker, &snapshot(now, true, true, 1), now), None);
        assert_eq!(
            kind(&mut tracker, &snapshot(now, true, true, 2), now),
            Some(NotificationKind::TextReceived)
        );
        let burst_at = now + Duration::from_secs(1);
        let burst = snapshot(burst_at, true, true, 3);
        assert_eq!(kind(&mut tracker, &burst, burst_at), None);
        assert_eq!(
            kind(&mut tracker, &burst, now + CLIPBOARD_NOTICE_INTERVAL),
            None
        );
        let fresh_at = now + CLIPBOARD_NOTICE_INTERVAL;
        assert_eq!(
            kind(&mut tracker, &snapshot(fresh_at, true, true, 4), fresh_at),
            Some(NotificationKind::TextReceived)
        );
    }

    #[test]
    fn network_status_has_its_own_short_cooldown_and_no_flap_backlog() {
        let now = Instant::now();
        let mut tracker = NotificationTracker::default();
        assert_eq!(
            kind(&mut tracker, &snapshot(now, true, false, 0), now),
            None
        );
        let connected_at = now + Duration::from_secs(1);
        assert_eq!(
            kind(
                &mut tracker,
                &snapshot(connected_at, true, true, 0),
                connected_at
            ),
            Some(NotificationKind::Connected)
        );
        let copied_at = now + Duration::from_secs(2);
        assert_eq!(
            kind(&mut tracker, &snapshot(copied_at, true, true, 1), copied_at),
            Some(NotificationKind::TextReceived)
        );
        let disconnected_at = now + Duration::from_secs(3);
        let disconnected = snapshot(disconnected_at, true, false, 1);
        assert_eq!(kind(&mut tracker, &disconnected, disconnected_at), None);
        assert_eq!(
            kind(&mut tracker, &disconnected, now + Duration::from_secs(7)),
            None
        );
        let reconnected_at = now + Duration::from_secs(8);
        assert_eq!(
            kind(
                &mut tracker,
                &snapshot(reconnected_at, true, true, 1),
                reconnected_at
            ),
            Some(NotificationKind::Connected)
        );
        let stopped_at = reconnected_at + NETWORK_NOTICE_INTERVAL;
        assert_eq!(
            kind(
                &mut tracker,
                &snapshot(stopped_at, true, false, 1),
                stopped_at
            ),
            Some(NotificationKind::Disconnected)
        );
    }

    #[test]
    fn current_connection_changes_take_priority_over_coincident_clipboard_activity() {
        let now = Instant::now();
        let mut tracker = NotificationTracker::default();
        assert_eq!(
            kind(&mut tracker, &snapshot(now, true, false, 0), now),
            None
        );
        assert_eq!(
            kind(&mut tracker, &snapshot(now, true, true, 1), now),
            Some(NotificationKind::Connected)
        );
        assert_eq!(kind(&mut tracker, &snapshot(now, true, true, 1), now), None);
    }

    #[test]
    fn sent_activity_never_creates_a_notice_or_consumes_receive_cooldown() {
        let now = Instant::now();
        let mut tracker = NotificationTracker::default();
        let mut current = snapshot(now, true, true, 0);
        assert_eq!(kind(&mut tracker, &current, now), None);
        for (serial, notice) in [
            (1, ClipboardNotice::TextSent),
            (2, ClipboardNotice::ImageSent),
        ] {
            current.clipboard_serial = serial;
            current.clipboard = Some(notice);
            assert_eq!(kind(&mut tracker, &current, now), None);
        }
        current.clipboard_serial = 3;
        current.clipboard = Some(ClipboardNotice::TextReceived);
        assert_eq!(
            kind(&mut tracker, &current, now),
            Some(NotificationKind::TextReceived)
        );
        let image_at = now + CLIPBOARD_NOTICE_INTERVAL;
        current.clipboard_serial = 4;
        current.clipboard = Some(ClipboardNotice::ImageReceived);
        current.clipboard_at = Some(image_at);
        assert_eq!(
            kind(&mut tracker, &current, image_at),
            Some(NotificationKind::ImageReceived)
        );
    }

    #[test]
    fn stale_unknown_and_future_events_are_consumed_without_spending_cooldown() {
        let now = Instant::now();
        let mut tracker = NotificationTracker::default();
        let mut current = snapshot(now, true, true, 0);
        assert_eq!(kind(&mut tracker, &current, now), None);
        current.clipboard_serial = 1;
        current.clipboard_at = Some(now - Duration::from_secs(30));
        assert_eq!(kind(&mut tracker, &current, now), None);
        assert_eq!(
            kind(&mut tracker, &current, now + Duration::from_secs(1)),
            None
        );
        current.clipboard_serial = 2;
        current.clipboard_at = None;
        assert_eq!(kind(&mut tracker, &current, now), None);
        current.clipboard_serial = 3;
        current.clipboard_at = Some(now + Duration::from_secs(1));
        assert_eq!(kind(&mut tracker, &current, now), None);
        current.clipboard_serial = 4;
        current.clipboard_at = Some(now);
        assert_eq!(
            kind(&mut tracker, &current, now),
            Some(NotificationKind::TextReceived)
        );
    }

    #[test]
    fn delayed_poll_preserves_original_notice_deadline_and_rejects_expired_receipts() {
        let now = Instant::now();
        let mut tracker = NotificationTracker::default();
        let baseline = snapshot(now, true, true, 0);
        assert_eq!(tracker.observe(&baseline, now), None);
        let current = snapshot(now, true, true, 1);
        let delayed_poll = now + Duration::from_secs(4);
        let notice = tracker.observe(&current, delayed_poll).unwrap();
        assert_eq!(notice.kind, NotificationKind::TextReceived);
        assert_eq!(notice.deadline, now + MAX_NOTICE_AGE);
        assert_ne!(notice.deadline, delayed_poll + MAX_NOTICE_AGE);

        let mut expired_tracker = NotificationTracker::default();
        assert_eq!(expired_tracker.observe(&baseline, now), None);
        assert_eq!(
            expired_tracker.observe(&current, now + MAX_NOTICE_AGE),
            None
        );
        assert_eq!(
            expired_tracker.observe(&current, now + Duration::from_secs(30)),
            None
        );
    }

    #[test]
    fn disconnected_snapshots_cannot_publish_an_old_clipboard_notice() {
        let now = Instant::now();
        let mut tracker = NotificationTracker::default();
        assert_eq!(tracker.observe(&snapshot(now, true, false, 0), now), None);
        assert_eq!(tracker.observe(&snapshot(now, true, false, 1), now), None);
    }
}

//! Opt-in peer display/sleep requests without synthesizing input or changing
//! Windows power, screen-saver, lock-screen, or registry settings.
//!
//! Windows documents that `SetThreadExecutionState` does not prevent a screen
//! saver. This module therefore implements "keep the other PC awake", not a
//! screen-saver or security-lock bypass. The owning thread clears its request
//! when a short authenticated activity lease expires or its scope is revoked.

use std::{
    sync::{Arc, Condvar, Mutex, MutexGuard},
    time::{Duration, Instant},
};

use crate::InputDiagnostics;

/// The original authenticated receipt time, rather than processing time, bounds
/// every request. Missing traffic cannot leave an indefinite execution request.
pub const PEER_ACTIVITY_LEASE: Duration = Duration::from_secs(15);
pub const LOCAL_ACTIVITY_WINDOW: Duration = Duration::from_secs(30);

/// Aggregate request state only. No input times, coordinates or content leave
/// the native activity monitor.
#[derive(Debug, Clone, Copy, Default, serde::Serialize)]
pub struct AwakeStatus {
    pub supported: bool,
    pub requested: bool,
    pub applied: bool,
    pub failed: bool,
}

#[derive(Default)]
struct ActivityLease {
    scope: Option<u64>,
    enabled_since: Option<Instant>,
    until: Option<Instant>,
}

impl ActivityLease {
    fn configure(&mut self, scope: Option<u64>, now: Instant) -> bool {
        if self.scope == scope {
            return false;
        }
        self.scope = scope;
        self.enabled_since = scope.map(|_| now);
        self.until = None;
        true
    }

    fn refresh(&mut self, scope: u64, received_at: Instant, now: Instant) -> bool {
        let Some(enabled_since) = self.enabled_since else {
            return false;
        };
        let Some(until) = received_at.checked_add(PEER_ACTIVITY_LEASE) else {
            return false;
        };
        if self.scope != Some(scope)
            || received_at < enabled_since
            || received_at > now
            || until <= now
        {
            return false;
        }
        self.until = Some(self.until.map_or(until, |old| old.max(until)));
        true
    }

    fn active(&self, now: Instant) -> bool {
        self.scope.is_some() && self.until.is_some_and(|until| now < until)
    }
}

#[derive(Default)]
struct State {
    lease: ActivityLease,
    revision: u64,
    closed: bool,
    applied: bool,
    failed: bool,
}

#[derive(Default)]
struct Shared {
    state: Mutex<State>,
    wake: Condvar,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|error| error.into_inner())
    }

    fn close(&self) {
        let mut state = self.lock();
        state.closed = true;
        state.lease.configure(None, Instant::now());
        state.revision = state.revision.wrapping_add(1);
        self.wake.notify_one();
    }
}

struct WorkerLifetime(Arc<Shared>);

impl Drop for WorkerLifetime {
    fn drop(&mut self) {
        // Never join or call a Windows API from a caller, including shutdown.
        // Windows also retires execution-state requests when their thread exits.
        self.0.close();
    }
}

/// A cloneable latest-state controller. There is no unbounded command queue and
/// no power API or worker wait on the input hook, raw window or control reader.
#[derive(Clone)]
pub struct PeerAwakeController {
    shared: Arc<Shared>,
    _lifetime: Arc<WorkerLifetime>,
}

impl PeerAwakeController {
    pub fn start() -> anyhow::Result<Self> {
        let shared = Arc::new(Shared::default());
        let controller = Self {
            shared: shared.clone(),
            _lifetime: Arc::new(WorkerLifetime(shared.clone())),
        };
        #[cfg(windows)]
        std::thread::Builder::new()
            .name("emc-peer-awake".into())
            .spawn(move || power_worker(shared))?;
        Ok(controller)
    }

    /// `Some` is only for the current authenticated control generation with
    /// local sharing and keep-awake permission enabled. Policy-off, pause,
    /// disconnect and replacement must revoke the scope explicitly.
    pub fn configure_scope(&self, scope: Option<u64>) {
        let mut state = self.shared.lock();
        if state.closed || !state.lease.configure(scope, Instant::now()) {
            return;
        }
        state.revision = state.revision.wrapping_add(1);
        self.shared.wake.notify_one();
    }

    /// Call only for an authenticated activity message after a final generation
    /// and local-policy check. Pass the message's original receipt instant.
    /// A queued old-generation or pre-activation message cannot renew a lease.
    pub fn refresh(&self, scope: u64, received_at: Instant) -> bool {
        let mut state = self.shared.lock();
        if state.closed || !state.lease.refresh(scope, received_at, Instant::now()) {
            return false;
        }
        state.revision = state.revision.wrapping_add(1);
        self.shared.wake.notify_one();
        true
    }

    pub fn status(&self) -> AwakeStatus {
        let state = self.shared.lock();
        AwakeStatus {
            supported: cfg!(windows),
            requested: cfg!(windows) && !state.closed && state.lease.active(Instant::now()),
            applied: state.applied,
            failed: state.failed,
        }
    }

    pub fn shutdown(&self) {
        self.shared.close();
    }
}

#[cfg(windows)]
fn power_worker(shared: Arc<Shared>) {
    use windows_sys::Win32::System::Power::{
        SetThreadExecutionState, ES_CONTINUOUS, ES_DISPLAY_REQUIRED, ES_SYSTEM_REQUIRED,
    };

    let mut applied = false;
    loop {
        let (desired, revision, closed) = {
            let state = shared.lock();
            (
                !state.closed && state.lease.active(Instant::now()),
                state.revision,
                state.closed,
            )
        };
        if desired != applied {
            let flags = if desired {
                ES_CONTINUOUS | ES_DISPLAY_REQUIRED | ES_SYSTEM_REQUIRED
            } else {
                ES_CONTINUOUS
            };
            // This is the only thread that owns this execution-state request.
            // The call creates no input and alters no saved system setting.
            let succeeded = unsafe { SetThreadExecutionState(flags) } != 0;
            if succeeded {
                applied = desired;
            }
            let mut state = shared.lock();
            state.applied = applied;
            state.failed = !succeeded;
        }
        if closed {
            break;
        }
        let state = shared.lock();
        if state.revision != revision || state.closed {
            continue;
        }
        let until_expiry = state
            .lease
            .until
            .filter(|until| *until > Instant::now())
            .map(|until| until.saturating_duration_since(Instant::now()))
            .unwrap_or(Duration::from_secs(1));
        // Failed OS requests retry at most once a second. The original lease
        // deadline and explicit policy changes remain independently effective.
        let _ = shared
            .wake
            .wait_timeout(state, until_expiry.min(Duration::from_secs(1)))
            .unwrap_or_else(|error| error.into_inner());
    }
    // Exiting the owning thread also retires any request whose explicit clear
    // failed. No replacement worker is started in parallel.
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct InputTotals {
    captured: [u64; 4],
    injected: [u64; 4],
}

impl From<&InputDiagnostics> for InputTotals {
    fn from(value: &InputDiagnostics) -> Self {
        Self {
            captured: [
                value.captured_moves,
                value.captured_buttons,
                value.captured_keys,
                value.captured_wheels,
            ],
            injected: [
                value.injected_moves,
                value.injected_buttons,
                value.injected_keys,
                value.injected_wheels,
            ],
        }
    }
}

/// Samples only an opaque OS last-input token and existing aggregate native
/// counters. It never installs a hook or retains a key, location or input event.
/// Our receiving/injected activity cannot perpetually reflect keep-awake beats.
#[derive(Default)]
pub struct LocalActivityMonitor {
    previous: Option<(Option<u32>, InputTotals, bool)>,
    recent_activity: Option<Instant>,
}

impl LocalActivityMonitor {
    pub fn sample(&mut self, diagnostics: &InputDiagnostics) -> bool {
        self.observe(last_input_token(), diagnostics, Instant::now())
    }

    fn observe(
        &mut self,
        token: Option<u32>,
        diagnostics: &InputDiagnostics,
        now: Instant,
    ) -> bool {
        let totals = InputTotals::from(diagnostics);
        let previous = self
            .previous
            .replace((token, totals, diagnostics.receiving));
        let Some((old_token, old_totals, was_receiving)) = previous else {
            return false;
        };
        if diagnostics.receiving || was_receiving || old_totals.injected != totals.injected {
            self.recent_activity = None;
            return false;
        }
        if token.is_none() && old_totals.captured == totals.captured {
            self.recent_activity = None;
            return false;
        }
        let local_input_changed =
            matches!((old_token, token), (Some(old), Some(new)) if old != new);
        if local_input_changed || old_totals.captured != totals.captured {
            self.recent_activity = Some(now);
        }
        self.recent_activity
            .is_some_and(|observed| now.saturating_duration_since(observed) < LOCAL_ACTIVITY_WINDOW)
    }
}

#[cfg(windows)]
fn last_input_token() -> Option<u32> {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};

    let mut info = LASTINPUTINFO {
        cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32,
        dwTime: 0,
    };
    // The token is session-local, can wrap or move backward, and is therefore
    // compared only for change. Freshness uses our own monotonic observation.
    (unsafe { GetLastInputInfo(&mut info) } != 0).then_some(info.dwTime)
}

#[cfg(not(windows))]
fn last_input_token() -> Option<u32> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lease_needs_current_scope_and_fresh_authenticated_receipt() {
        let now = Instant::now();
        let mut lease = ActivityLease::default();
        assert!(!lease.refresh(7, now, now));
        lease.configure(Some(7), now);
        assert!(!lease.refresh(6, now, now));
        assert!(lease.refresh(7, now, now));
        assert!(lease.active(now + PEER_ACTIVITY_LEASE - Duration::from_millis(1)));
        assert!(!lease.active(now + PEER_ACTIVITY_LEASE));
        assert!(!lease.refresh(7, now, now + PEER_ACTIVITY_LEASE));
        assert!(!lease.refresh(7, now + Duration::from_secs(1), now));
    }

    #[test]
    fn old_generation_and_old_activation_cannot_reactivate_a_lease() {
        let now = Instant::now();
        let later = now + Duration::from_secs(1);
        let mut lease = ActivityLease::default();
        lease.configure(Some(1), now);
        assert!(lease.refresh(1, now, now));
        lease.configure(Some(2), later);
        assert!(!lease.active(later));
        assert!(!lease.refresh(1, later, later));
        assert!(!lease.refresh(2, now, later));
        assert!(lease.refresh(2, later, later));
        lease.configure(None, later);
        assert!(!lease.active(later));
        lease.configure(Some(2), later + Duration::from_secs(1));
        assert!(!lease.refresh(2, later, later + Duration::from_secs(1)));
    }

    #[test]
    fn processing_an_old_receipt_cannot_extend_its_original_deadline() {
        let now = Instant::now();
        let mut lease = ActivityLease::default();
        lease.configure(Some(8), now);
        assert!(lease.refresh(8, now, now + Duration::from_secs(14)));
        assert!(!lease.active(now + PEER_ACTIVITY_LEASE));
        assert!(lease.refresh(
            8,
            now + Duration::from_secs(14),
            now + Duration::from_secs(14)
        ));
        assert!(lease.active(now + Duration::from_secs(28)));
        assert!(!lease.active(now + Duration::from_secs(29)));
    }

    #[test]
    fn unchanged_scope_does_not_refresh_or_clear_activity() {
        let now = Instant::now();
        let mut lease = ActivityLease::default();
        assert!(lease.configure(Some(2), now));
        assert!(lease.refresh(2, now, now));
        assert!(!lease.configure(Some(2), now + Duration::from_secs(1)));
        assert!(lease.active(now + Duration::from_secs(1)));
        assert!(!lease.active(now + PEER_ACTIVITY_LEASE));
    }

    #[test]
    fn sampling_has_a_baseline_and_stops_after_local_idle() {
        let now = Instant::now();
        let diagnostics = InputDiagnostics::default();
        let mut monitor = LocalActivityMonitor::default();
        assert!(!monitor.observe(Some(100), &diagnostics, now));
        assert!(!monitor.observe(Some(100), &diagnostics, now + Duration::from_secs(1)));
        let active_at = now + Duration::from_secs(2);
        assert!(monitor.observe(Some(101), &diagnostics, active_at));
        assert!(monitor.observe(Some(101), &diagnostics, active_at + Duration::from_secs(29)));
        assert!(!monitor.observe(Some(101), &diagnostics, active_at + LOCAL_ACTIVITY_WINDOW));
    }

    #[test]
    fn suppressed_source_capture_counts_as_real_activity() {
        let now = Instant::now();
        let mut diagnostics = InputDiagnostics::default();
        let mut monitor = LocalActivityMonitor::default();
        assert!(!monitor.observe(Some(100), &diagnostics, now));
        diagnostics.sending = true;
        diagnostics.captured_keys = 1;
        assert!(monitor.observe(Some(100), &diagnostics, now + Duration::from_secs(1)));
    }

    #[test]
    fn receiving_and_late_injected_progress_cannot_reflect_peer_activity() {
        let now = Instant::now();
        let mut diagnostics = InputDiagnostics::default();
        let mut monitor = LocalActivityMonitor::default();
        assert!(!monitor.observe(Some(100), &diagnostics, now));
        diagnostics.receiving = true;
        diagnostics.injected_moves = 1;
        assert!(!monitor.observe(Some(101), &diagnostics, now + Duration::from_secs(1)));
        diagnostics.receiving = false;
        assert!(!monitor.observe(Some(102), &diagnostics, now + Duration::from_secs(2)));
        diagnostics.injected_wheels = 1;
        assert!(!monitor.observe(Some(103), &diagnostics, now + Duration::from_secs(3)));
        assert!(!monitor.observe(Some(103), &diagnostics, now + Duration::from_secs(4)));
        assert!(monitor.observe(Some(104), &diagnostics, now + Duration::from_secs(5)));
    }

    #[test]
    fn last_input_failure_and_tick_wrap_do_not_create_a_permanent_activity_run() {
        let now = Instant::now();
        let diagnostics = InputDiagnostics::default();
        let mut monitor = LocalActivityMonitor::default();
        assert!(!monitor.observe(Some(u32::MAX), &diagnostics, now));
        assert!(!monitor.observe(None, &diagnostics, now + Duration::from_secs(1)));
        assert!(!monitor.observe(Some(0), &diagnostics, now + Duration::from_secs(2)));
        assert!(monitor.observe(Some(1), &diagnostics, now + Duration::from_secs(3)));
        assert!(!monitor.observe(None, &diagnostics, now + Duration::from_secs(4)));
        assert!(!monitor.observe(Some(1), &diagnostics, now + Duration::from_secs(5)));
        assert!(monitor.observe(Some(2), &diagnostics, now + Duration::from_secs(6)));
        assert!(!monitor.observe(Some(2), &diagnostics, now + Duration::from_secs(36)));
    }
}

//! Optional metadata-only activity and notification state, independent of the
//! native capture/insertion path. No preference work runs in an input callback.
use super::*;
use emc_platform::awake::{LocalActivityMonitor, PeerAwakeController};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardNotice {
    TextSent,
    TextReceived,
    ImageSent,
    ImageReceived,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct NotificationSnapshot {
    pub activation: u64,
    pub generation: u64,
    pub enabled: bool,
    pub connected: bool,
    pub clipboard_serial: u64,
    pub clipboard: Option<ClipboardNotice>,
    pub clipboard_at: Option<Instant>,
}

impl Engine {
    pub fn is_shutting_down(&self) -> bool {
        *self.0.stop.borrow()
    }
    pub fn notification_snapshot(&self) -> NotificationSnapshot {
        let s = self.state();
        let current_clipboard_notice = s.connected
            && s.config.settings.enabled
            && !*self.0.stop.borrow()
            && s.clipboard_notice_generation == Some(s.generation);
        NotificationSnapshot {
            activation: s.notification_revision,
            generation: s.generation,
            enabled: s.config.settings.show_status_notifications,
            connected: s.connected,
            clipboard_serial: s.clipboard_notice_serial,
            clipboard: current_clipboard_notice
                .then_some(s.clipboard_notice)
                .flatten(),
            clipboard_at: current_clipboard_notice
                .then_some(s.clipboard_notice_at)
                .flatten(),
        }
    }

    pub(super) fn clipboard_notice(&self, notice: ClipboardNotice, generation: u64) {
        if !matches!(
            notice,
            ClipboardNotice::TextReceived | ClipboardNotice::ImageReceived
        ) {
            // A successful copy is announced on the receiving PC only. A send
            // acknowledgment must not replace a pending receive notification.
            return;
        }
        let at = Instant::now();
        let mut s = self.state();
        if *self.0.stop.borrow()
            || !s.connected
            || s.generation != generation
            || !s.config.settings.enabled
        {
            return;
        }
        s.clipboard_notice_serial = s.clipboard_notice_serial.wrapping_add(1);
        s.clipboard_notice = Some(notice);
        s.clipboard_notice_at = Some(at);
        s.clipboard_notice_generation = Some(generation);
    }

    pub(super) fn record_peer_activity(&self, generation: u64, received_at: Instant) {
        let mut s = self.state();
        if !*self.0.stop.borrow()
            && s.connected
            && s.generation == generation
            && s.config.settings.enabled
            && s.config.settings.keep_peer_awake
        {
            let revision = s.awake_revision;
            s.peer_activity = Some((generation, revision, received_at));
        }
    }

    pub(super) async fn preferences_loop(&self) {
        let Ok(controller) = PeerAwakeController::start() else {
            self.event(EventCode::OperationFailed);
            return;
        };
        let mut stopped = self.0.stop.subscribe();
        let mut tick = interval(Duration::from_millis(250));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut activity = LocalActivityMonitor::default();
        let mut previous_context = None;
        let mut scope_token = 0u64;
        let mut next_activity_sample = Instant::now();
        loop {
            tokio::select! {
                _ = stopped.changed() => {
                    if *stopped.borrow() { break; }
                }
                _ = tick.tick() => {
                    if *stopped.borrow() { break; }
                    // Permission, connection and activation are rechecked at
                    // the power-request commit boundary. The native controller
                    // only takes a short state lock; OS work stays on its thread.
                    {
                        let s = self.state();
                        let context = (s.connected && s.config.settings.enabled
                            && s.config.settings.keep_peer_awake)
                            .then_some((s.generation, s.awake_revision));
                        if context != previous_context {
                            scope_token = scope_token.wrapping_add(1);
                            controller.configure_scope(context.map(|_| scope_token));
                            previous_context = context;
                        }
                        if let (Some(context), Some((generation, revision, received_at))) =
                            (context, s.peer_activity)
                        {
                            if context == (generation, revision) {
                                controller.refresh(scope_token, received_at);
                            }
                        }
                    }
                    if Instant::now() < next_activity_sample { continue; }
                    next_activity_sample = Instant::now() + Duration::from_secs(3);
                    let diagnostics = self.0.platform.diagnostics();
                    let active = activity.sample(&diagnostics);
                    let s = self.state();
                    if active && s.connected && s.config.settings.enabled
                        && s.config.settings.keep_peer_awake && !*self.0.stop.borrow()
                    {
                        if let Some(sender) = &s.control {
                            // This exact authenticated session owns the sender.
                            // Activity can be omitted under pressure; it must
                            // never displace input or disconnect working control.
                            sender.push_activity(Instant::now());
                        }
                    }
                }
            }
        }
        controller.shutdown();
    }
}

//! Child of the isolated portable engine policy fixture. These tests create no
//! native backend, input hooks, power requests, real clipboard or app profile.
use super::*;
use emc_platform::awake::PeerAwakeController;

fn awake_fixture() -> PolicyFixture {
    PolicyFixture::new(Settings {
        keep_peer_awake: true,
        ..Settings::default()
    })
}

#[test]
fn peer_activity_requires_current_connected_enabled_permission() {
    let f = awake_fixture();
    let received_at = Instant::now();
    f.engine.record_peer_activity(0, received_at);
    assert!(f.engine.state().peer_activity.is_none());
    let _cancelled = f.install_control();
    let generation = f.engine.state().generation;
    f.engine
        .record_peer_activity(generation.wrapping_add(1), received_at);
    assert!(f.engine.state().peer_activity.is_none());
    f.engine.record_peer_activity(generation, received_at);
    let s = f.engine.state();
    assert_eq!(
        s.peer_activity,
        Some((generation, s.awake_revision, received_at))
    );
    drop(s);

    for (enabled, keep_peer_awake) in [(false, true), (true, false)] {
        let denied = PolicyFixture::new(Settings {
            enabled,
            keep_peer_awake,
            ..Settings::default()
        });
        let _cancelled = denied.install_control();
        let generation = denied.engine.state().generation;
        denied
            .engine
            .record_peer_activity(generation, Instant::now());
        assert!(denied.engine.state().peer_activity.is_none());
    }
}

#[test]
fn shutdown_started_cannot_install_new_activity_metadata() {
    let f = awake_fixture();
    let _cancelled = f.install_control();
    let generation = f.engine.state().generation;
    // Model the interval after shutdown announces cancellation and before its
    // state-locked disconnect completes. The stop guard must work on its own.
    f.engine.0.stop.send_replace(true);
    f.engine.record_peer_activity(generation, Instant::now());
    assert!(f.engine.state().peer_activity.is_none());
    f.engine.disconnect(EventCode::EngineStopped);
}

#[test]
fn retired_generation_cannot_overwrite_activity_for_a_replacement_session() {
    let f = awake_fixture();
    let old_cancelled = f.install_control();
    let old_generation = f.engine.state().generation;
    f.engine
        .record_peer_activity(old_generation, Instant::now());
    f.engine.disconnect(EventCode::PeerDisconnected);
    assert!(*old_cancelled.borrow());
    let _replacement = f.install_control();
    let generation = f.engine.state().generation;
    assert_ne!(generation, old_generation);
    let received_at = Instant::now();
    f.engine.record_peer_activity(generation, received_at);
    f.engine
        .record_peer_activity(old_generation, Instant::now());
    let s = f.engine.state();
    assert_eq!(
        s.peer_activity,
        Some((generation, s.awake_revision, received_at))
    );
}

#[test]
fn fast_awake_off_on_rejects_old_activity_without_replacing_input_control() {
    let f = awake_fixture();
    let cancelled = f.install_control();
    let generation = f.engine.state().generation;
    let original_revision = f.engine.state().awake_revision;
    let received_at = Instant::now() - Duration::from_secs(1);
    f.engine.record_peer_activity(generation, received_at);

    let mut settings = f.engine.status().settings;
    settings.keep_peer_awake = false;
    f.engine.update_settings(settings.clone()).unwrap();
    settings.keep_peer_awake = true;
    f.engine.update_settings(settings).unwrap();
    let s = f.engine.state();
    assert!(s.connected);
    assert_eq!(s.generation, generation);
    assert_ne!(s.awake_revision, original_revision);
    assert!(s.peer_activity.is_none());
    let revision = s.awake_revision;
    drop(s);
    assert!(!*cancelled.borrow());

    // Even if a reader's old original receipt reaches the new metadata scope,
    // native admission rejects its pre-activation timestamp. This portable
    // controller has no Windows worker and performs no OS power operation.
    let controller = PeerAwakeController::start().unwrap();
    controller.configure_scope(Some(revision));
    f.engine.record_peer_activity(generation, received_at);
    let (_, _, stale_receipt) = f.engine.state().peer_activity.unwrap();
    assert!(!controller.refresh(revision, stale_receipt));
    let fresh_receipt = Instant::now();
    f.engine.record_peer_activity(generation, fresh_receipt);
    assert!(controller.refresh(revision, fresh_receipt));
    controller.shutdown();
}

#[test]
fn failed_save_awake_off_reduces_authority_without_replacing_control() {
    let f = awake_fixture();
    let cancelled = f.install_control();
    let generation = f.engine.state().generation;
    f.engine.record_peer_activity(generation, Instant::now());
    f.fail_saves();
    let mut settings = f.engine.status().settings;
    settings.keep_peer_awake = false;
    let error = f.engine.update_settings(settings).err().unwrap();
    assert!(error.to_string().contains("active for this run"));
    let s = f.engine.state();
    assert!(s.connected);
    assert_eq!(s.generation, generation);
    assert!(!s.config.settings.keep_peer_awake);
    assert!(s.peer_activity.is_none());
    drop(s);
    assert!(!*cancelled.borrow());
    f.engine.record_peer_activity(generation, Instant::now());
    assert!(f.engine.state().peer_activity.is_none());
    // The persistence warning is truthful: the unchanged file still contains
    // the earlier permission, but it cannot restore authority in this process.
    let saved = Config::load(&f.engine.0.config_dir).unwrap();
    assert!(saved.settings.keep_peer_awake);
}

#[test]
fn failed_save_awake_on_cannot_increase_authority() {
    let f = PolicyFixture::new(Settings::default());
    let cancelled = f.install_control();
    let generation = f.engine.state().generation;
    f.fail_saves();
    let mut settings = f.engine.status().settings;
    settings.keep_peer_awake = true;
    let error = f.engine.update_settings(settings).err().unwrap();
    assert!(error.to_string().contains("no new permissions"));
    assert!(!f.engine.status().settings.keep_peer_awake);
    assert_eq!(f.engine.state().generation, generation);
    assert!(f.engine.state().connected);
    assert!(!*cancelled.borrow());
    f.engine.record_peer_activity(generation, Instant::now());
    assert!(f.engine.state().peer_activity.is_none());
}

#[test]
fn notification_activation_changes_across_off_on_without_an_intermediate_snapshot() {
    let f = PolicyFixture::new(Settings {
        show_status_notifications: true,
        ..Settings::default()
    });
    let _cancelled = f.install_control();
    let generation = f.engine.state().generation;
    let original = f.engine.notification_snapshot();
    let mut settings = f.engine.status().settings;
    settings.show_status_notifications = false;
    f.engine.update_settings(settings.clone()).unwrap();
    f.engine
        .clipboard_notice(ClipboardNotice::TextReceived, generation);
    settings.show_status_notifications = true;
    f.engine.update_settings(settings).unwrap();
    let current = f.engine.notification_snapshot();
    assert!(current.enabled);
    assert_ne!(current.activation, original.activation);
    assert_ne!(current.clipboard_serial, original.clipboard_serial);
}

#[test]
fn failed_save_notification_opt_out_applies_and_retires_the_old_activation() {
    let f = PolicyFixture::new(Settings {
        show_status_notifications: true,
        ..Settings::default()
    });
    let original = f.engine.notification_snapshot();
    f.fail_saves();
    let mut settings = f.engine.status().settings;
    settings.show_status_notifications = false;
    let error = f.engine.update_settings(settings).err().unwrap();
    assert!(error.to_string().contains("active for this run"));
    let current = f.engine.notification_snapshot();
    assert!(!current.enabled);
    assert_ne!(current.activation, original.activation);
    // The old saved preference is retained only on disk, as explained by the
    // persistence error. It must not restore notices in the live process.
    let saved = Config::load(&f.engine.0.config_dir).unwrap();
    assert!(saved.settings.show_status_notifications);
}

#[test]
fn failed_save_notification_opt_in_does_not_enable_notices() {
    let f = PolicyFixture::new(Settings::default());
    let original = f.engine.notification_snapshot();
    f.fail_saves();
    let mut settings = f.engine.status().settings;
    settings.show_status_notifications = true;
    let error = f.engine.update_settings(settings).err().unwrap();
    assert!(error.to_string().contains("no new permissions"));
    let current = f.engine.notification_snapshot();
    assert!(!current.enabled);
    assert_eq!(current.activation, original.activation);
}

#[test]
fn clipboard_notice_slot_preserves_receive_time_and_ignores_send_acknowledgments() {
    let f = PolicyFixture::new(Settings::default());
    let _cancelled = f.install_control();
    let generation = f.engine.state().generation;
    f.engine
        .clipboard_notice(ClipboardNotice::TextSent, generation);
    assert_eq!(f.engine.notification_snapshot().clipboard_serial, 0);
    let before = Instant::now();
    f.engine
        .clipboard_notice(ClipboardNotice::TextReceived, generation);
    let after = Instant::now();
    let received = f.engine.notification_snapshot();
    assert_eq!(received.clipboard, Some(ClipboardNotice::TextReceived));
    assert_eq!(received.generation, generation);
    let at = received.clipboard_at.unwrap();
    assert!(before <= at && at <= after);
    for notice in [ClipboardNotice::TextSent, ClipboardNotice::ImageSent] {
        f.engine.clipboard_notice(notice, generation);
        let current = f.engine.notification_snapshot();
        assert_eq!(current.clipboard_serial, received.clipboard_serial);
        assert_eq!(current.clipboard, received.clipboard);
        assert_eq!(current.clipboard_at, received.clipboard_at);
    }
}

#[test]
fn retired_clipboard_completion_cannot_bind_a_notice_to_a_new_session() {
    let f = PolicyFixture::new(Settings::default());
    let _cancelled = f.install_control();
    let old_generation = f.engine.state().generation;
    f.engine
        .clipboard_notice(ClipboardNotice::TextReceived, old_generation);
    let original_serial = f.engine.notification_snapshot().clipboard_serial;
    f.engine.disconnect(EventCode::PeerDisconnected);
    let disconnected = f.engine.notification_snapshot();
    assert_eq!(disconnected.clipboard, None);
    assert_eq!(disconnected.clipboard_at, None);
    let _replacement = f.install_control();
    let generation = f.engine.state().generation;
    f.engine
        .clipboard_notice(ClipboardNotice::ImageReceived, old_generation);
    let replacement = f.engine.notification_snapshot();
    assert_ne!(replacement.generation, old_generation);
    assert_eq!(replacement.clipboard_serial, original_serial);
    assert_eq!(replacement.clipboard, None);
    assert_eq!(replacement.clipboard_at, None);
    f.engine
        .clipboard_notice(ClipboardNotice::ImageReceived, generation);
    let current = f.engine.notification_snapshot();
    assert_eq!(current.clipboard, Some(ClipboardNotice::ImageReceived));
    assert!(current.clipboard_at.is_some());
}

#[test]
fn clipboard_notices_stop_with_sharing_or_shutdown() {
    let f = PolicyFixture::new(Settings::default());
    let _cancelled = f.install_control();
    let generation = f.engine.state().generation;
    f.engine.state().config.settings.enabled = false;
    f.engine
        .clipboard_notice(ClipboardNotice::TextReceived, generation);
    assert_eq!(f.engine.notification_snapshot().clipboard_serial, 0);
    f.engine.state().config.settings.enabled = true;
    f.engine.0.stop.send_replace(true);
    f.engine
        .clipboard_notice(ClipboardNotice::ImageReceived, generation);
    assert_eq!(f.engine.notification_snapshot().clipboard_serial, 0);
    assert_eq!(f.engine.notification_snapshot().clipboard_at, None);
    f.engine.disconnect(EventCode::EngineStopped);
}

#[test]
fn repeated_snapshots_never_renew_an_old_clipboard_event_timestamp() {
    let f = PolicyFixture::new(Settings::default());
    let _cancelled = f.install_control();
    let generation = f.engine.state().generation;
    f.engine
        .clipboard_notice(ClipboardNotice::TextReceived, generation);
    let old_event = Instant::now() - Duration::from_secs(30);
    f.engine.state().clipboard_notice_at = Some(old_event);
    let first = f.engine.notification_snapshot();
    let second = f.engine.notification_snapshot();
    assert_eq!(first.clipboard_at, Some(old_event));
    assert_eq!(second.clipboard_at, Some(old_event));
    assert_eq!(first.clipboard_serial, second.clipboard_serial);
}

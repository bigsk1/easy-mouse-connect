use super::*;
use tempfile::TempDir;

#[path = "engine_preferences_tests.rs"]
mod preferences_tests;

struct PolicyFixture {
    engine: Engine,
    _root: TempDir,
}

impl PolicyFixture {
    fn new(settings: Settings) -> Self {
        let root = tempfile::tempdir().unwrap();
        let config_dir = root.path().join("profile");
        let mut config = Config::load(&config_dir).unwrap();
        config.settings = settings.clone();
        config.peer = Some(Peer {
            name: "synthetic policy peer".into(),
            public_key: encode_key(&[7; 32]),
            address: "127.0.0.1:1".into(),
        });
        config.save(&config_dir).unwrap();
        let diagnostics = DiagnosticLog::open(config_dir.join("logs")).unwrap();
        let (stop, _) = watch::channel(false);
        let platform = InputController::default();
        platform.configure(
            settings.enabled,
            settings.edge_switching,
            settings.peer_on_left,
        );
        let engine = Engine(Arc::new(Inner {
            state: Mutex::new(Runtime {
                config,
                invitation: None,
                connected: false,
                generation: 0,
                bulk_generation: 0,
                clipboard_revision: 0,
                clipboard_content_revision: 0,
                awake_revision: 0,
                peer_activity: None,
                clipboard_notice_serial: 0,
                notification_revision: 0,
                clipboard_notice: None,
                clipboard_notice_at: None,
                clipboard_notice_generation: None,
                control: None,
                control_stop: None,
                bulk: None,
                last_error: None,
                latency_ms: None,
                latency_updated: None,
                events: vec![],
                pending: vec![],
                decisions: HashMap::new(),
                suppress_clipboard: None,
                suppress_clipboard_image: None,
            }),
            address_hints: Mutex::new(vec![]),
            config_dir,
            receive_dir: root.path().join("downloads"),
            platform,
            diagnostics,
            pipeline: Arc::new(Counters::default()),
            control_ready: Notify::new(),
            bulk_changed: Notify::new(),
            bulk_waiting: Notify::new(),
            bulk_install_waiting: Notify::new(),
            bulk_retirement_waiting: Notify::new(),
            input_sink: Mutex::new(None),
            image_sink: Mutex::new(None),
            text_sink: Mutex::new(None),
            incoming: AsyncMutex::new(None),
            outgoing: AsyncMutex::new(()),
            pairing: AsyncMutex::new(()),
            dialing: AsyncMutex::new(()),
            bulk_dialing: Arc::new(AsyncMutex::new(())),
            stop,
            address: "127.0.0.1:1".parse().unwrap(),
        }));
        Self {
            engine,
            _root: root,
        }
    }

    fn connected() -> Self {
        Self::new(Settings {
            share_clipboard: true,
            share_files: true,
            ..Settings::default()
        })
    }

    fn install_control(&self) -> watch::Receiver<bool> {
        let (stop, cancelled) = watch::channel(false);
        let mut s = self.engine.state();
        assert!(!s.connected);
        s.generation = s.generation.wrapping_add(1);
        s.connected = true;
        s.control = Some(OrderedQueue::new(
            Direction::Outgoing,
            self.engine.0.pipeline.clone(),
        ));
        s.control_stop = Some(stop);
        drop(s);
        self.engine.0.platform.set_connected(true);
        cancelled
    }

    fn install_bulk(&self) -> mpsc::Receiver<Message> {
        let (tx, rx) = mpsc::channel(8);
        let mut s = self.engine.state();
        assert!(s.connected && s.config.settings.enabled);
        assert!(s.bulk.is_none());
        s.bulk_generation = s.bulk_generation.wrapping_add(1);
        s.bulk = Some(tx);
        rx
    }

    fn fail_saves(&self) {
        // A directory at the fixed temporary-file name makes OpenOptions fail
        // deterministically, including when tests run as a privileged user.
        std::fs::create_dir(self.engine.0.config_dir.join("identity.tmp")).unwrap();
    }
}

impl Drop for PolicyFixture {
    fn drop(&mut self) {
        self.engine.shutdown();
    }
}

async fn sample(engine: &Engine, previous: &mut ClipboardBaseline, text: &str) {
    engine
        .poll_clipboard_with(previous, std::future::ready(Some(text.to_string())))
        .await
        .unwrap();
}

async fn receive_bytes(
    engine: &Engine,
    rx: &mut mpsc::Receiver<Message>,
    expected_kind: BulkKind,
) -> Vec<u8> {
    let Message::Offer {
        id,
        name,
        size,
        kind,
    } = rx.recv().await.unwrap()
    else {
        panic!("Expected a synthetic clipboard offer");
    };
    assert!(name.is_empty() && kind == expected_kind);
    engine
        .state()
        .decisions
        .remove(&id)
        .unwrap()
        .send(true)
        .unwrap();
    let mut bytes = vec![];
    loop {
        match rx.recv().await.unwrap() {
            Message::Chunk { id: chunk_id, data } => {
                assert_eq!(chunk_id, id);
                bytes.extend(data);
            }
            Message::Complete {
                id: complete_id,
                sha256,
            } => {
                assert_eq!(complete_id, id);
                assert_eq!(bytes.len() as u64, size);
                let digest: [u8; 32] = Sha256::digest(&bytes).into();
                assert_eq!(sha256, digest);
                engine
                    .state()
                    .decisions
                    .remove(&id)
                    .unwrap()
                    .send(true)
                    .unwrap();
                return bytes;
            }
            message => panic!("Unexpected synthetic transfer message: {message:?}"),
        }
    }
}

async fn receive_text(engine: &Engine, rx: &mut mpsc::Receiver<Message>) -> String {
    String::from_utf8(receive_bytes(engine, rx, BulkKind::Text).await).unwrap()
}

async fn sample_and_receive(
    engine: &Engine,
    previous: &mut ClipboardBaseline,
    rx: &mut mpsc::Receiver<Message>,
    text: &str,
) {
    let (sent, received) = timeout(Duration::from_secs(1), async {
        tokio::join!(
            engine.poll_clipboard_with(previous, std::future::ready(Some(text.to_string()))),
            receive_text(engine, rx),
        )
    })
    .await
    .unwrap();
    sent.unwrap();
    assert_eq!(received, text);
}

#[tokio::test]
async fn failed_save_emergency_stop_revokes_current_authority_and_keeps_pinned_identity() {
    let f = PolicyFixture::connected();
    let cancelled = f.install_control();
    let mut rx = f.install_bulk();
    let old_session = f.engine.bulk_session().unwrap();
    let old_epoch = f.engine.0.platform.connection_epoch();
    let old_key = f.engine.status().public_key;
    let old_peer = f.engine.status().peer.unwrap().public_key;
    let old_control = f.engine.state().control.as_ref().unwrap().clone();
    let (decision, reply) = oneshot::channel();
    f.engine.state().decisions.insert(random_id(), decision);
    f.fail_saves();

    let error = f.engine.emergency_stop().unwrap_err();
    assert!(error.to_string().contains("active for this run"));
    assert!(error.to_string().contains("could not be saved"));
    let status = f.engine.status();
    assert!(!status.enabled && !status.connected);
    assert_eq!(status.public_key, old_key);
    assert_eq!(status.peer.unwrap().public_key, old_peer);
    assert!(*cancelled.borrow());
    assert!(reply.await.is_err());
    assert!(old_control.recv().await.is_none());
    assert_ne!(f.engine.0.platform.connection_epoch(), old_epoch);
    assert!(f.engine.clipboard_session().is_none());
    assert!(f.engine.state().control.is_none());
    assert!(f.engine.state().bulk.is_none());
    assert!(f.engine.state().decisions.is_empty());
    assert!(f.engine.state().pending.is_empty());
    assert!(f.engine.session_current(old_session.generation).is_err());
    assert!(f
        .engine
        .bulk(
            &old_session,
            Message::Offer {
                id: random_id(),
                name: String::new(),
                size: 0,
                kind: BulkKind::Text,
            }
        )
        .await
        .is_err());
    assert!(rx.try_recv().is_err());
    let saved = Config::load(&f.engine.0.config_dir).unwrap();
    assert!(
        saved.settings.enabled,
        "The error must disclose the unchanged disk policy"
    );
    assert_eq!(saved.version, 1);
}

#[tokio::test]
async fn failed_save_file_revocation_blocks_existing_transfer_without_unsaved_layout_changes() {
    let f = PolicyFixture::connected();
    let cancelled = f.install_control();
    let _rx = f.install_bulk();
    let session = f.engine.bulk_session().unwrap();
    let id = random_id();
    f.engine
        .receive_bulk(
            Message::Offer {
                id: id.clone(),
                name: "synthetic.txt".into(),
                size: 1,
                kind: BulkKind::File,
            },
            session.generation,
            session.bulk_generation,
        )
        .await
        .unwrap();
    f.engine.respond_file(id.clone(), true).await.unwrap();
    f.fail_saves();
    let mut settings = f.engine.status().settings;
    settings.share_files = false;
    settings.edge_switching = false;
    settings.peer_on_left = true;
    assert!(f.engine.update_settings(settings).is_err());

    let status = f.engine.status();
    assert!(status.enabled && status.connected && status.settings.share_clipboard);
    assert!(!status.settings.share_files);
    assert!(status.settings.edge_switching);
    assert!(!status.settings.peer_on_left);
    assert!(
        !*cancelled.borrow(),
        "File revocation does not retire the control session"
    );
    assert!(f.engine.state().bulk.is_none());
    assert_ne!(f.engine.state().bulk_generation, session.bulk_generation);
    assert!(status.pending_files.is_empty());
    assert!(f
        .engine
        .receive_bulk(
            Message::Chunk {
                id: id.clone(),
                data: vec![7],
            },
            session.generation,
            session.bulk_generation
        )
        .await
        .is_err());
    assert!(f
        .engine
        .receive_bulk(
            Message::Complete {
                id: id.clone(),
                sha256: Sha256::digest([7]).into(),
            },
            session.generation,
            session.bulk_generation
        )
        .await
        .is_err());
    assert_eq!(
        std::fs::metadata(f.engine.0.receive_dir.join(&id).join("receiving.part"))
            .unwrap()
            .len(),
        0
    );
    f.engine.0.incoming.lock().await.take();
    assert!(!f.engine.0.receive_dir.join(id).exists());
}

#[tokio::test]
async fn failed_save_clipboard_revocation_does_not_enable_files_or_change_layout() {
    let f = PolicyFixture::new(Settings {
        share_clipboard: true,
        ..Settings::default()
    });
    let _cancelled = f.install_control();
    let mut rx = f.install_bulk();
    let (_, session) = f.engine.clipboard_session().unwrap();
    f.fail_saves();
    let mut settings = f.engine.status().settings;
    settings.share_clipboard = false;
    settings.share_files = true;
    settings.peer_on_left = true;
    assert!(f.engine.update_settings(settings).is_err());
    let status = f.engine.status();
    assert!(status.enabled && status.connected);
    assert!(!status.settings.share_clipboard && !status.settings.share_files);
    assert!(!status.settings.peer_on_left);
    assert!(f.engine.clipboard_session().is_none());
    assert!(f.engine.send_text(String::new(), session).await.is_err());
    assert!(rx.try_recv().is_err());
}

#[test]
fn failed_save_cannot_enable_any_permission_or_apply_a_layout_change() {
    let f = PolicyFixture::new(Settings {
        enabled: false,
        ..Settings::default()
    });
    f.fail_saves();
    assert!(f
        .engine
        .update_settings(Settings {
            enabled: true,
            share_clipboard: true,
            share_files: true,
            edge_switching: false,
            peer_on_left: true,
            share_clipboard_images: true,
            keep_peer_awake: true,
            show_status_notifications: true,
        })
        .is_err());
    let settings = f.engine.status().settings;
    assert!(
        !settings.enabled
            && !settings.share_clipboard
            && !settings.share_files
            && !settings.share_clipboard_images
            && !settings.keep_peer_awake
            && !settings.show_status_notifications
    );
    assert!(settings.edge_switching && !settings.peer_on_left);
}

#[tokio::test]
async fn clipboard_fast_off_on_establishes_a_new_baseline_without_an_inactive_poll() {
    let f = PolicyFixture::connected();
    let _cancelled = f.install_control();
    let mut old_rx = f.install_bulk();
    let mut previous = None;
    sample(&f.engine, &mut previous, "synthetic initial baseline").await;
    let mut settings = f.engine.status().settings;
    settings.share_clipboard = false;
    f.engine.update_settings(settings.clone()).unwrap();
    settings.share_clipboard = true;
    f.engine.update_settings(settings).unwrap();
    let mut fresh_rx = f.install_bulk();

    sample(
        &f.engine,
        &mut previous,
        "synthetic text copied while disabled",
    )
    .await;
    assert!(old_rx.try_recv().is_err());
    assert!(fresh_rx.try_recv().is_err());
    sample_and_receive(
        &f.engine,
        &mut previous,
        &mut fresh_rx,
        "synthetic new active copy",
    )
    .await;
}

#[tokio::test]
async fn clipboard_reconnection_establishes_a_baseline_even_without_an_inactive_poll() {
    let f = PolicyFixture::connected();
    let _cancelled = f.install_control();
    let mut old_rx = f.install_bulk();
    let mut previous = None;
    sample(
        &f.engine,
        &mut previous,
        "synthetic pre-disconnect baseline",
    )
    .await;
    f.engine.disconnect(EventCode::PeerDisconnected);
    let _fresh_cancelled = f.install_control();
    let mut fresh_rx = f.install_bulk();

    sample(
        &f.engine,
        &mut previous,
        "synthetic copy made while disconnected",
    )
    .await;
    assert!(old_rx.try_recv().is_err());
    assert!(fresh_rx.try_recv().is_err());
    sample_and_receive(
        &f.engine,
        &mut previous,
        &mut fresh_rx,
        "synthetic connected copy",
    )
    .await;
}

#[tokio::test]
async fn clipboard_paused_read_is_discarded_across_policy_off_on_or_session_replacement() {
    for replace_control in [false, true] {
        let f = PolicyFixture::connected();
        let _cancelled = f.install_control();
        let mut old_rx = f.install_bulk();
        let mut previous = None;
        sample(&f.engine, &mut previous, "synthetic read baseline").await;
        let (began, started) = oneshot::channel();
        let (resume, resumed) = oneshot::channel();
        let read = async {
            began.send(()).unwrap();
            resumed.await.unwrap();
            Some("synthetic sample from revoked read".to_string())
        };
        let change = async {
            started.await.unwrap();
            if replace_control {
                f.engine.disconnect(EventCode::PeerDisconnected);
                let _ = f.install_control();
            } else {
                let mut settings = f.engine.status().settings;
                settings.share_clipboard = false;
                f.engine.update_settings(settings.clone()).unwrap();
                settings.share_clipboard = true;
                f.engine.update_settings(settings).unwrap();
            }
            let rx = f.install_bulk();
            resume.send(()).unwrap();
            rx
        };
        let (result, mut fresh_rx) =
            tokio::join!(f.engine.poll_clipboard_with(&mut previous, read), change,);
        result.unwrap();
        assert!(previous.is_none());
        assert!(old_rx.try_recv().is_err());
        assert!(fresh_rx.try_recv().is_err());
        sample(
            &f.engine,
            &mut previous,
            "synthetic new activation baseline",
        )
        .await;
        assert!(fresh_rx.try_recv().is_err());
        sample_and_receive(
            &f.engine,
            &mut previous,
            &mut fresh_rx,
            "synthetic permitted fresh copy",
        )
        .await;
    }
}

#[tokio::test]
async fn clipboard_captured_sender_never_offers_a_sample_into_a_replacement_channel() {
    let f = PolicyFixture::connected();
    let _cancelled = f.install_control();
    let mut old_rx = f.install_bulk();
    let (_, captured) = f.engine.clipboard_session().unwrap();
    f.engine.disconnect(EventCode::PeerDisconnected);
    let _fresh_cancelled = f.install_control();
    let mut fresh_rx = f.install_bulk();
    assert!(timeout(
        Duration::from_secs(1),
        f.engine
            .send_text("synthetic old-session sample".into(), captured,)
    )
    .await
    .unwrap()
    .is_err());
    assert!(old_rx.try_recv().is_err());
    assert!(fresh_rx.try_recv().is_err());
    assert!(f.engine.state().decisions.is_empty());
}

#[tokio::test]
async fn clipboard_filters_credentials_suppressed_content_and_unchanged_samples() {
    let f = PolicyFixture::connected();
    let _cancelled = f.install_control();
    let mut rx = f.install_bulk();
    let mut previous = None;
    sample(&f.engine, &mut previous, "synthetic baseline").await;
    sample(&f.engine, &mut previous, "synthetic baseline").await;
    sample(&f.engine, &mut previous, "ABCD-EFGH-JKMP").await;
    let suppressed = "synthetic remote clipboard echo";
    f.engine.state().suppress_clipboard = Some(Sha256::digest(suppressed.as_bytes()).into());
    sample(&f.engine, &mut previous, suppressed).await;
    assert!(rx.try_recv().is_err());
    sample_and_receive(
        &f.engine,
        &mut previous,
        &mut rx,
        "synthetic next local copy",
    )
    .await;
}

fn synthetic_image(value: u8) -> ClipboardImage {
    ClipboardImage::new(1, 1, vec![value, 17, 31, 255]).unwrap()
}

fn image_fixture() -> PolicyFixture {
    PolicyFixture::new(Settings {
        share_clipboard_images: true,
        ..Settings::default()
    })
}

async fn image_sample(
    engine: &Engine,
    previous: &mut ImageBaseline,
    image: Option<ClipboardImage>,
) {
    let hash = image.as_ref().map(ClipboardImage::digest);
    let sequence = previous.as_ref().map_or(1, |(_, sequence, old)| {
        if *old == hash {
            *sequence
        } else {
            sequence.wrapping_add(1)
        }
    });
    engine
        .poll_clipboard_image_with(
            previous,
            std::future::ready(Ok(image_read(sequence, image))),
        )
        .await
        .unwrap();
}

async fn image_sample_and_receive(
    engine: &Engine,
    previous: &mut ImageBaseline,
    rx: &mut mpsc::Receiver<Message>,
    image: ClipboardImage,
) {
    let notice = engine.notification_snapshot();
    let expected = image.clone();
    let sequence = previous
        .as_ref()
        .map_or(1, |(_, sequence, _)| sequence.wrapping_add(1));
    let (sent, received) = timeout(Duration::from_secs(2), async {
        tokio::join!(
            engine.poll_clipboard_image_with(
                previous,
                std::future::ready(Ok(image_read(sequence, Some(image))))
            ),
            receive_bytes(engine, rx, BulkKind::Image)
        )
    })
    .await
    .unwrap();
    sent.unwrap();
    assert_eq!(ClipboardImage::from_png(&received).unwrap(), expected);
    let after = engine.notification_snapshot();
    assert_eq!(after.clipboard, notice.clipboard);
    assert_eq!(after.clipboard_serial, notice.clipboard_serial);
}

fn image_read(sequence: u32, image: Option<ClipboardImage>) -> ClipboardImageSample {
    ClipboardImageSample {
        sequence,
        current_sequence: sequence,
        image,
    }
}

#[tokio::test]
async fn failed_save_image_and_awake_revocation_keeps_reductions_and_retires_authority() {
    let f = PolicyFixture::new(Settings {
        share_clipboard_images: true,
        keep_peer_awake: true,
        ..Settings::default()
    });
    let cancelled = f.install_control();
    let mut rx = f.install_bulk();
    let (_, session) = f.engine.clipboard_session_for(BulkKind::Image).unwrap();
    f.engine
        .record_peer_activity(session.generation, Instant::now());
    let revision = f.engine.state().awake_revision;
    assert!(f.engine.state().peer_activity.is_some());
    f.fail_saves();
    let mut settings = f.engine.status().settings;
    settings.share_clipboard_images = false;
    settings.keep_peer_awake = false;
    settings.share_files = true;
    settings.show_status_notifications = true;
    assert!(f
        .engine
        .update_settings(settings)
        .err()
        .unwrap()
        .to_string()
        .contains("active for this run"));
    let settings = f.engine.status().settings;
    assert!(
        !settings.share_clipboard_images
            && !settings.keep_peer_awake
            && !settings.share_files
            && !settings.show_status_notifications
    );
    assert!(f.engine.state().peer_activity.is_none());
    assert_ne!(f.engine.state().awake_revision, revision);
    assert!(!*cancelled.borrow());
    assert!(f
        .engine
        .send_image(synthetic_image(1), session)
        .await
        .is_err());
    assert!(rx.try_recv().is_err());
}

#[tokio::test]
async fn image_consent_is_independent_and_rejects_invalid_offer_bounds() {
    let f = PolicyFixture::connected(); // Text/files enabled; images remain off.
    let _cancelled = f.install_control();
    let _rx = f.install_bulk();
    let session = f.engine.bulk_session().unwrap();
    let mut previous = None;
    image_sample(&f.engine, &mut previous, Some(synthetic_image(1))).await;
    assert!(previous.is_none());
    let control = f.engine.state().control.clone().unwrap();
    for (size, name) in [
        (1, ""),
        (0, ""),
        (MAX_CLIPBOARD_IMAGE_BYTES as u64 + 1, ""),
        (1, "peer.png"),
    ] {
        let id = random_id();
        f.engine
            .receive_bulk(
                Message::Offer {
                    id: id.clone(),
                    name: name.into(),
                    size,
                    kind: BulkKind::Image,
                },
                session.generation,
                session.bulk_generation,
            )
            .await
            .unwrap();
        let response = control.recv().await.unwrap();
        assert!(
            matches!(response.value, Message::Decision { id: refused, accept: false } if refused == id)
        );
        assert!(f.engine.0.incoming.lock().await.is_none());
    }
    assert!(f.engine.clipboard_session_for(BulkKind::Image).is_none());

    let f = image_fixture(); // Image opt-in does not grant text/files.
    let _cancelled = f.install_control();
    let mut rx = f.install_bulk();
    let session = f.engine.bulk_session().unwrap();
    let control = f.engine.state().control.clone().unwrap();
    for (size, name) in [
        (0, ""),
        (MAX_CLIPBOARD_IMAGE_BYTES as u64 + 1, ""),
        (1, "peer.png"),
    ] {
        let id = random_id();
        f.engine
            .receive_bulk(
                Message::Offer {
                    id: id.clone(),
                    name: name.into(),
                    size,
                    kind: BulkKind::Image,
                },
                session.generation,
                session.bulk_generation,
            )
            .await
            .unwrap();
        assert!(matches!(control.recv().await.unwrap().value,
            Message::Decision { id: refused, accept: false } if refused == id));
        assert!(f.engine.0.incoming.lock().await.is_none());
    }
    assert!(f.engine.clipboard_session().is_none());
    let mut previous = None;
    image_sample(&f.engine, &mut previous, None).await;
    image_sample_and_receive(&f.engine, &mut previous, &mut rx, synthetic_image(2)).await;
    assert!(!f.engine.status().settings.share_files && !f.engine.status().settings.share_clipboard);
}

#[tokio::test]
async fn image_activation_baseline_unchanged_echo_and_later_copy_follow_pixels() {
    let f = image_fixture();
    let _cancelled = f.install_control();
    let mut rx = f.install_bulk();
    let mut previous = None;
    let initial = synthetic_image(1);
    image_sample(&f.engine, &mut previous, Some(initial.clone())).await;
    image_sample(&f.engine, &mut previous, Some(initial)).await;
    let remote = synthetic_image(2);
    f.engine.state().suppress_clipboard_image = Some(remote.digest());
    image_sample(&f.engine, &mut previous, Some(remote)).await;
    assert!(rx.try_recv().is_err());
    image_sample_and_receive(&f.engine, &mut previous, &mut rx, synthetic_image(3)).await;
}

#[tokio::test]
async fn image_fast_off_on_and_reconnection_establish_new_baseline_without_inactive_poll() {
    for replace_control in [false, true] {
        let f = image_fixture();
        let _cancelled = f.install_control();
        let mut old_rx = f.install_bulk();
        let mut previous = None;
        image_sample(&f.engine, &mut previous, Some(synthetic_image(1))).await;
        if replace_control {
            f.engine.disconnect(EventCode::PeerDisconnected);
            let _ = f.install_control();
        } else {
            let mut settings = f.engine.status().settings;
            settings.share_clipboard_images = false;
            f.engine.update_settings(settings.clone()).unwrap();
            settings.share_clipboard_images = true;
            f.engine.update_settings(settings).unwrap();
        }
        let mut fresh_rx = f.install_bulk();
        image_sample(&f.engine, &mut previous, Some(synthetic_image(2))).await;
        assert!(old_rx.try_recv().is_err() && fresh_rx.try_recv().is_err());
        image_sample_and_receive(&f.engine, &mut previous, &mut fresh_rx, synthetic_image(3)).await;
    }
}

#[tokio::test]
async fn image_paused_read_cannot_cross_policy_or_session_replacement() {
    for replace_control in [false, true] {
        let f = image_fixture();
        let _cancelled = f.install_control();
        let mut old_rx = f.install_bulk();
        let mut previous = None;
        image_sample(&f.engine, &mut previous, Some(synthetic_image(1))).await;
        let (began, started) = oneshot::channel();
        let (resume, resumed) = oneshot::channel();
        let read = async {
            began.send(()).unwrap();
            resumed.await.unwrap();
            Ok(image_read(2, Some(synthetic_image(2))))
        };
        let change = async {
            started.await.unwrap();
            if replace_control {
                f.engine.disconnect(EventCode::PeerDisconnected);
                let _ = f.install_control();
            } else {
                let mut settings = f.engine.status().settings;
                settings.share_clipboard_images = false;
                f.engine.update_settings(settings.clone()).unwrap();
                settings.share_clipboard_images = true;
                f.engine.update_settings(settings).unwrap();
            }
            let rx = f.install_bulk();
            resume.send(()).unwrap();
            rx
        };
        let (result, mut fresh_rx) = tokio::join!(
            f.engine.poll_clipboard_image_with(&mut previous, read),
            change
        );
        result.unwrap();
        assert!(previous.is_none());
        assert!(old_rx.try_recv().is_err() && fresh_rx.try_recv().is_err());
        image_sample(&f.engine, &mut previous, Some(synthetic_image(3))).await;
        assert!(fresh_rx.try_recv().is_err());
        image_sample_and_receive(&f.engine, &mut previous, &mut fresh_rx, synthetic_image(4)).await;
    }
}

#[tokio::test]
async fn image_captured_sender_never_offers_into_replacement_channel() {
    let f = image_fixture();
    let _cancelled = f.install_control();
    let mut old_rx = f.install_bulk();
    let (_, captured) = f.engine.clipboard_session_for(BulkKind::Image).unwrap();
    f.engine.disconnect(EventCode::PeerDisconnected);
    let _cancelled = f.install_control();
    let mut fresh_rx = f.install_bulk();
    assert!(f
        .engine
        .send_image(synthetic_image(1), captured)
        .await
        .is_err());
    assert!(old_rx.try_recv().is_err() && fresh_rx.try_recv().is_err());
    assert!(f.engine.state().decisions.is_empty());
}

#[tokio::test]
async fn image_receive_checks_hash_exact_length_and_revocation_at_actual_commit() {
    let f = image_fixture();
    let _cancelled = f.install_control();
    let _rx = f.install_bulk();
    let session = f.engine.bulk_session().unwrap();
    let control = f.engine.state().control.clone().unwrap();
    let image = synthetic_image(1);
    let expected = image.digest();
    let writes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed = writes.clone();
    *f.engine.0.image_sink.lock().unwrap() = Some(Arc::new(move |prepared| {
        assert_eq!(prepared.digest(), expected);
        observed.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }));
    let png = image.to_png().unwrap();
    let id = random_id();
    f.engine
        .receive_bulk(
            Message::Offer {
                id: id.clone(),
                name: String::new(),
                size: png.len() as u64,
                kind: BulkKind::Image,
            },
            session.generation,
            session.bulk_generation,
        )
        .await
        .unwrap();
    assert!(matches!(
        control.recv().await.unwrap().value,
        Message::Decision { accept: true, .. }
    ));
    assert!(f
        .engine
        .receive_bulk(
            Message::Complete {
                id: id.clone(),
                sha256: Sha256::digest(&png).into()
            },
            session.generation,
            session.bulk_generation
        )
        .await
        .is_err());
    assert_eq!(writes.load(Ordering::SeqCst), 0);
    f.engine
        .receive_bulk(
            Message::Chunk {
                id: id.clone(),
                data: png.clone(),
            },
            session.generation,
            session.bulk_generation,
        )
        .await
        .unwrap();
    assert!(f
        .engine
        .receive_bulk(
            Message::Complete {
                id: id.clone(),
                sha256: [0; 32]
            },
            session.generation,
            session.bulk_generation
        )
        .await
        .is_err());
    assert_eq!(writes.load(Ordering::SeqCst), 0);
    f.engine
        .receive_bulk(
            Message::Complete {
                id: id.clone(),
                sha256: Sha256::digest(&png).into(),
            },
            session.generation,
            session.bulk_generation,
        )
        .await
        .unwrap();
    assert_eq!(writes.load(Ordering::SeqCst), 1);
    assert!(
        matches!(control.recv().await.unwrap().value, Message::Received { id: received } if received == id)
    );
    assert_eq!(
        f.engine.notification_snapshot().clipboard,
        Some(ClipboardNotice::ImageReceived)
    );
    assert!(
        std::fs::read_dir(f.engine.0.config_dir.join("clipboard-images"))
            .unwrap()
            .next()
            .is_none()
    );

    let prepared = emc_platform::clipboard_image::prepare_image(
        synthetic_image(1),
        &f.engine.0.config_dir.join("clipboard-images"),
    )
    .unwrap();
    let mut settings = f.engine.status().settings;
    settings.share_clipboard_images = false;
    f.engine.update_settings(settings).unwrap();
    assert!(f
        .engine
        .apply_clipboard_image(prepared, session.generation, session.bulk_generation)
        .await
        .is_err());
    assert_eq!(writes.load(Ordering::SeqCst), 1);
    assert!(
        std::fs::read_dir(f.engine.0.config_dir.join("clipboard-images"))
            .unwrap()
            .next()
            .is_none()
    );
}

#[tokio::test]
async fn received_image_invalidates_pending_sample_and_old_unsent_offer() {
    let f = image_fixture();
    let _cancelled = f.install_control();
    let mut rx = f.install_bulk();
    let (_, captured) = f.engine.clipboard_session_for(BulkKind::Image).unwrap();
    let session = f.engine.bulk_session().unwrap();
    *f.engine.0.image_sink.lock().unwrap() = Some(Arc::new(|_| Ok(())));
    let mut previous = None;
    image_sample(&f.engine, &mut previous, Some(synthetic_image(1))).await;
    let (began, started) = oneshot::channel();
    let (resume, resumed) = oneshot::channel();
    let read = async {
        began.send(()).unwrap();
        resumed.await.unwrap();
        Ok(image_read(2, Some(synthetic_image(2))))
    };
    let receive = async {
        started.await.unwrap();
        let prepared = emc_platform::clipboard_image::prepare_image(
            synthetic_image(3),
            &f.engine.0.config_dir.join("clipboard-images"),
        )
        .unwrap();
        f.engine
            .apply_clipboard_image(prepared, session.generation, session.bulk_generation)
            .await
            .unwrap();
        resume.send(()).unwrap();
    };
    let (result, ()) = tokio::join!(
        f.engine.poll_clipboard_image_with(&mut previous, read),
        receive
    );
    result.unwrap();
    assert!(
        previous.is_some(),
        "A content write discards the read without retiring its activation baseline"
    );
    assert!(rx.try_recv().is_err());
    assert!(f
        .engine
        .send_image(synthetic_image(2), captured)
        .await
        .is_err());
    assert!(rx.try_recv().is_err());
    image_sample(&f.engine, &mut previous, Some(synthetic_image(3))).await;
    assert!(rx.try_recv().is_err());
    image_sample_and_receive(&f.engine, &mut previous, &mut rx, synthetic_image(4)).await;
}

#[tokio::test]
async fn queued_image_offer_rechecks_permission_after_waiting_for_capacity() {
    let f = image_fixture();
    let _cancelled = f.install_control();
    let mut rx = f.install_bulk();
    let (_, session) = f.engine.clipboard_session_for(BulkKind::Image).unwrap();
    for _ in 0..8 {
        session.tx.try_send(Message::Ready).unwrap();
    }
    let old_generation = session.bulk_generation;
    let mut send = std::pin::pin!(f.engine.bulk(
        &session,
        Message::Offer {
            id: random_id(),
            name: String::new(),
            size: 1,
            kind: BulkKind::Image
        }
    ));
    // Poll the real queue send until blocked on its full original channel.
    assert!(
        std::future::poll_fn(|cx| std::task::Poll::Ready(
            std::future::Future::poll(send.as_mut(), cx).is_pending()
        ))
        .await
    );
    let mut settings = f.engine.status().settings;
    settings.share_clipboard_images = false;
    f.engine.update_settings(settings).unwrap();
    assert_ne!(f.engine.state().bulk_generation, old_generation);
    assert!(matches!(rx.recv().await, Some(Message::Ready)));
    assert!(send.await.is_err());
    while let Ok(message) = rx.try_recv() {
        assert!(matches!(message, Message::Ready));
    }
}

#[tokio::test]
async fn image_copy_of_same_pixels_resends_with_new_sequence_while_text_is_off() {
    let f = image_fixture();
    let _cancelled = f.install_control();
    let mut rx = f.install_bulk();
    assert!(!f.engine.status().settings.share_clipboard);
    let mut previous = None;
    image_sample(&f.engine, &mut previous, None).await;
    let image = synthetic_image(10);
    image_sample_and_receive(&f.engine, &mut previous, &mut rx, image.clone()).await;
    let serial = f.engine.notification_snapshot().clipboard_serial;
    // A poll/read of the same native sequence is not another copy.
    image_sample(&f.engine, &mut previous, Some(image.clone())).await;
    assert!(rx.try_recv().is_err());
    // The peer may replace its clipboard with local text while text sharing is
    // off. A new local copy of identical pixels must still offer the image again.
    image_sample_and_receive(&f.engine, &mut previous, &mut rx, image).await;
    assert_eq!(f.engine.notification_snapshot().clipboard_serial, serial);
}

#[tokio::test]
async fn image_read_uses_rendered_token_and_retries_clipboard_change_during_decode() {
    let f = image_fixture();
    let _cancelled = f.install_control();
    let mut rx = f.install_bulk();
    let mut previous = None;
    f.engine
        .poll_clipboard_image_with(&mut previous, std::future::ready(Ok(image_read(10, None))))
        .await
        .unwrap();
    let image = synthetic_image(11);
    // Rendering a delayed/synthesized format can advance the token during the
    // locked read. The returned post-read token is the accepted sample's token.
    let (sent, bytes) = timeout(Duration::from_secs(2), async {
        tokio::join!(
            f.engine.poll_clipboard_image_with(
                &mut previous,
                std::future::ready(Ok(image_read(12, Some(image.clone()))))
            ),
            receive_bytes(&f.engine, &mut rx, BulkKind::Image)
        )
    })
    .await
    .unwrap();
    sent.unwrap();
    assert_eq!(ClipboardImage::from_png(&bytes).unwrap(), image);
    assert_eq!(previous.as_ref().unwrap().1, 12);
    f.engine
        .poll_clipboard_image_with(
            &mut previous,
            std::future::ready(Ok(image_read(12, Some(image)))),
        )
        .await
        .unwrap();
    assert!(rx.try_recv().is_err());

    let before = previous;
    let stale = ClipboardImageSample {
        sequence: 13,
        current_sequence: 14,
        image: Some(synthetic_image(13)),
    };
    f.engine
        .poll_clipboard_image_with(&mut previous, std::future::ready(Ok(stale)))
        .await
        .unwrap();
    assert_eq!(
        previous, before,
        "A decode crossing a new clipboard write cannot advance the baseline"
    );
    assert!(rx.try_recv().is_err());
    let expected = synthetic_image(14);
    let (sent, bytes) = timeout(Duration::from_secs(2), async {
        tokio::join!(
            f.engine.poll_clipboard_image_with(
                &mut previous,
                std::future::ready(Ok(image_read(14, Some(expected.clone()))))
            ),
            receive_bytes(&f.engine, &mut rx, BulkKind::Image)
        )
    })
    .await
    .unwrap();
    sent.unwrap();
    assert_eq!(ClipboardImage::from_png(&bytes).unwrap(), expected);
}

#[tokio::test]
async fn failed_image_publication_attempt_invalidates_pending_read_and_unsent_offer() {
    let f = image_fixture();
    let _cancelled = f.install_control();
    let mut rx = f.install_bulk();
    let (_, captured) = f.engine.clipboard_session_for(BulkKind::Image).unwrap();
    let session = f.engine.bulk_session().unwrap();
    let mut previous = None;
    image_sample(&f.engine, &mut previous, Some(synthetic_image(1))).await;
    let revision = f.engine.state().clipboard_content_revision;
    let attempted = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let observed = attempted.clone();
    *f.engine.0.image_sink.lock().unwrap() = Some(Arc::new(move |_| {
        observed.store(true, Ordering::SeqCst);
        bail!("Synthetic failure after an earlier clipboard format changed")
    }));
    let (began, started) = oneshot::channel();
    let (resume, resumed) = oneshot::channel();
    let read = async {
        began.send(()).unwrap();
        resumed.await.unwrap();
        Ok(image_read(2, Some(synthetic_image(2))))
    };
    let receive = async {
        started.await.unwrap();
        let prepared = emc_platform::clipboard_image::prepare_image(
            synthetic_image(3),
            &f.engine.0.config_dir.join("clipboard-images"),
        )
        .unwrap();
        assert!(f
            .engine
            .apply_clipboard_image(prepared, session.generation, session.bulk_generation)
            .await
            .is_err());
        resume.send(()).unwrap();
    };
    let (result, ()) = tokio::join!(
        f.engine.poll_clipboard_image_with(&mut previous, read),
        receive
    );
    result.unwrap();
    assert!(attempted.load(Ordering::SeqCst));
    assert_ne!(f.engine.state().clipboard_content_revision, revision);
    assert!(
        previous.is_some(),
        "A failed content write must not reactivate clipboard sharing"
    );
    assert!(rx.try_recv().is_err());
    assert!(f
        .engine
        .send_image(synthetic_image(2), captured)
        .await
        .is_err());
    assert!(rx.try_recv().is_err());
    assert_eq!(f.engine.notification_snapshot().clipboard_serial, 0);
    assert!(
        std::fs::read_dir(f.engine.0.config_dir.join("clipboard-images"))
            .unwrap()
            .next()
            .is_none()
    );
}

fn dual_clipboard_fixture() -> PolicyFixture {
    let f = PolicyFixture::new(Settings {
        share_clipboard: true,
        share_clipboard_images: true,
        ..Settings::default()
    });
    *f.engine.0.text_sink.lock().unwrap() = Some(Arc::new(|_| Ok(())));
    *f.engine.0.image_sink.lock().unwrap() = Some(Arc::new(|_| Ok(())));
    f
}

async fn receive_clipboard_data(engine: &Engine, kind: BulkKind, bytes: Vec<u8>) {
    assert!(matches!(kind, BulkKind::Text | BulkKind::Image));
    let session = engine.bulk_session().unwrap();
    let control = engine.state().control.clone().unwrap();
    let id = random_id();
    engine
        .receive_bulk(
            Message::Offer {
                id: id.clone(),
                name: String::new(),
                size: bytes.len() as u64,
                kind,
            },
            session.generation,
            session.bulk_generation,
        )
        .await
        .unwrap();
    assert!(matches!(control.recv().await.unwrap().value,
        Message::Decision { id: accepted, accept: true } if accepted == id));
    for chunk in bytes.chunks(CHUNK_BYTES) {
        engine
            .receive_bulk(
                Message::Chunk {
                    id: id.clone(),
                    data: chunk.to_vec(),
                },
                session.generation,
                session.bulk_generation,
            )
            .await
            .unwrap();
    }
    engine
        .receive_bulk(
            Message::Complete {
                id: id.clone(),
                sha256: Sha256::digest(&bytes).into(),
            },
            session.generation,
            session.bulk_generation,
        )
        .await
        .unwrap();
    assert!(matches!(control.recv().await.unwrap().value,
        Message::Received { id: received } if received == id));
}

#[tokio::test]
async fn received_image_does_not_rebaseline_the_next_local_text_copy() {
    let f = dual_clipboard_fixture();
    let _cancelled = f.install_control();
    let mut rx = f.install_bulk();
    let mut previous = None;
    sample(&f.engine, &mut previous, "synthetic existing text baseline").await;
    receive_clipboard_data(
        &f.engine,
        BulkKind::Image,
        synthetic_image(10).to_png().unwrap(),
    )
    .await;
    // The native image write removes CF_UNICODETEXT. A no-text poll must not
    // convert the next local text copy into a new activation baseline.
    f.engine
        .poll_clipboard_with(&mut previous, std::future::ready(None))
        .await
        .unwrap();
    sample_and_receive(
        &f.engine,
        &mut previous,
        &mut rx,
        "synthetic first text after received image",
    )
    .await;
    assert_eq!(
        f.engine.notification_snapshot().clipboard,
        Some(ClipboardNotice::ImageReceived)
    );
}

#[tokio::test]
async fn received_text_does_not_rebaseline_an_image_copied_before_the_next_poll() {
    let f = dual_clipboard_fixture();
    let _cancelled = f.install_control();
    let mut rx = f.install_bulk();
    let mut previous = None;
    image_sample(&f.engine, &mut previous, Some(synthetic_image(1))).await;
    receive_clipboard_data(
        &f.engine,
        BulkKind::Text,
        b"synthetic received text".to_vec(),
    )
    .await;
    // A new local image may be copied before an intervening image-absence poll.
    image_sample_and_receive(&f.engine, &mut previous, &mut rx, synthetic_image(2)).await;
    assert_eq!(
        f.engine.notification_snapshot().clipboard,
        Some(ClipboardNotice::TextReceived)
    );
}

#[tokio::test]
async fn received_image_discards_paused_text_read_and_old_offer_without_losing_activation() {
    let f = dual_clipboard_fixture();
    let _cancelled = f.install_control();
    let mut rx = f.install_bulk();
    let (_, captured) = f.engine.clipboard_session().unwrap();
    let mut previous = None;
    sample(&f.engine, &mut previous, "synthetic initial text baseline").await;
    let baseline = previous;
    let (began, started) = oneshot::channel();
    let (resume, resumed) = oneshot::channel();
    let read = async {
        began.send(()).unwrap();
        resumed.await.unwrap();
        Some("synthetic stale text sampled before image".to_string())
    };
    let receive = async {
        started.await.unwrap();
        receive_clipboard_data(
            &f.engine,
            BulkKind::Image,
            synthetic_image(3).to_png().unwrap(),
        )
        .await;
        resume.send(()).unwrap();
    };
    let (result, ()) = tokio::join!(f.engine.poll_clipboard_with(&mut previous, read), receive);
    result.unwrap();
    assert_eq!(previous, baseline);
    assert!(rx.try_recv().is_err());
    assert!(f
        .engine
        .send_text("synthetic old captured text".into(), captured)
        .await
        .is_err());
    assert!(rx.try_recv().is_err());
    sample_and_receive(
        &f.engine,
        &mut previous,
        &mut rx,
        "synthetic fresh text after image commit",
    )
    .await;
}

#[tokio::test]
async fn received_text_discards_paused_image_read_and_old_offer_without_losing_activation() {
    let f = dual_clipboard_fixture();
    let _cancelled = f.install_control();
    let mut rx = f.install_bulk();
    let (_, captured) = f.engine.clipboard_session_for(BulkKind::Image).unwrap();
    let mut previous = None;
    image_sample(&f.engine, &mut previous, Some(synthetic_image(1))).await;
    let baseline = previous;
    let (began, started) = oneshot::channel();
    let (resume, resumed) = oneshot::channel();
    let read = async {
        began.send(()).unwrap();
        resumed.await.unwrap();
        Ok(image_read(2, Some(synthetic_image(2))))
    };
    let receive = async {
        started.await.unwrap();
        receive_clipboard_data(
            &f.engine,
            BulkKind::Text,
            b"synthetic text during image decode".to_vec(),
        )
        .await;
        resume.send(()).unwrap();
    };
    let (result, ()) = tokio::join!(
        f.engine.poll_clipboard_image_with(&mut previous, read),
        receive
    );
    result.unwrap();
    assert_eq!(previous, baseline);
    assert!(rx.try_recv().is_err());
    assert!(f
        .engine
        .send_image(synthetic_image(2), captured)
        .await
        .is_err());
    assert!(rx.try_recv().is_err());
    image_sample_and_receive(&f.engine, &mut previous, &mut rx, synthetic_image(3)).await;
}

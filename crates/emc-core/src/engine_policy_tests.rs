use super::*;
use tempfile::TempDir;

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

async fn receive_text(engine: &Engine, rx: &mut mpsc::Receiver<Message>) -> String {
    let Message::Offer {
        id,
        name,
        size,
        kind,
    } = rx.recv().await.unwrap()
    else {
        panic!("Expected a synthetic text offer");
    };
    assert!(name.is_empty() && kind == BulkKind::Text);
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
                return String::from_utf8(bytes).unwrap();
            }
            message => panic!("Unexpected synthetic transfer message: {message:?}"),
        }
    }
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
        })
        .is_err());
    let settings = f.engine.status().settings;
    assert!(!settings.enabled && !settings.share_clipboard && !settings.share_files);
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

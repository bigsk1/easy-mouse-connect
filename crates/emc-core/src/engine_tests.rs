use super::*;
use tempfile::TempDir;

struct Fixture {
    a: Engine,
    b: Engine,
    root: TempDir,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.a.shutdown();
        self.b.shutdown();
    }
}

async fn until(mut check: impl FnMut() -> bool) {
    timeout(Duration::from_secs(8), async {
        while !check() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("Condition timed out");
}
async fn unpaired_fixture() -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let a = Engine::start_at(
        root.path().join("a"),
        root.path().join("a-downloads"),
        "127.0.0.1:0".parse().unwrap(),
    )
    .await
    .unwrap();
    let b = Engine::start_at(
        root.path().join("b"),
        root.path().join("b-downloads"),
        "127.0.0.1:0".parse().unwrap(),
    )
    .await
    .unwrap();
    Fixture { a, b, root }
}
async fn fixture() -> Fixture {
    let f = unpaired_fixture().await;
    let invitation = f.b.create_invitation().unwrap();
    f.a.pair_device(f.b.listen_address().to_string(), invitation)
        .await
        .unwrap();
    until(|| f.a.state().bulk.is_some() && f.b.state().bulk.is_some()).await;
    f
}
fn enable_files(e: &Engine) {
    let mut settings = e.status().settings;
    settings.share_files = true;
    settings.share_clipboard = true;
    e.update_settings(settings).unwrap();
}

#[tokio::test]
async fn short_code_wrong_password_budget_and_replacement_fail_closed() {
    let f = unpaired_fixture().await;
    let code = f.b.create_invitation().unwrap();
    assert_eq!(code.len(), 14);
    assert_eq!(f.b.status().pairing_attempts_remaining, 5);
    let wrong = if code == "0000-0000-0000" {
        "1111-1111-1111"
    } else {
        "0000-0000-0000"
    };
    for remaining in (0..5).rev() {
        assert!(f
            .a
            .pair_device(f.b.listen_address().to_string(), wrong.into())
            .await
            .is_err());
        assert!(f.a.status().peer.is_none() && f.b.status().peer.is_none());
        assert_eq!(
            f.b.state().invitation.as_ref().unwrap().attempts,
            5 - remaining
        );
    }
    assert_eq!(f.b.status().pairing_attempts_remaining, 0);
    assert!(f.b.status().invitation.is_none());
    assert!(f
        .a
        .pair_device(f.b.listen_address().to_string(), code)
        .await
        .is_err());
    assert_eq!(f.b.state().invitation.as_ref().unwrap().attempts, 5);
    let new_code = f.b.create_invitation().unwrap();
    f.a.pair_device(f.b.listen_address().to_string(), new_code)
        .await
        .unwrap();
    assert!(f.a.status().peer.is_some() && f.b.status().peer.is_some());
    assert!(f.b.state().invitation.is_none());
}

#[tokio::test]
async fn invalid_expired_replaced_and_replayed_codes_do_not_grant_trust() {
    let f = unpaired_fixture().await;
    let old = f.b.create_invitation().unwrap();
    for invalid in ["", "1234", "AAAA-AAAA-AAAU", "x"].into_iter() {
        assert!(f
            .a
            .pair_device(f.b.listen_address().to_string(), invalid.into())
            .await
            .is_err());
    }
    assert_eq!(f.b.state().invitation.as_ref().unwrap().attempts, 0);
    f.b.state().invitation.as_mut().unwrap().expires = Instant::now() - Duration::from_secs(1);
    assert!(f
        .a
        .pair_device(f.b.listen_address().to_string(), old)
        .await
        .is_err());
    assert!(f.a.status().peer.is_none() && f.b.status().peer.is_none());
    let replaced = f.b.create_invitation().unwrap();
    let fresh = f.b.create_invitation().unwrap();
    assert!(f
        .a
        .pair_device(f.b.listen_address().to_string(), replaced)
        .await
        .is_err());
    f.a.pair_device(f.b.listen_address().to_string(), fresh.clone())
        .await
        .unwrap();
    let pinned = f.b.status().peer.unwrap().public_key;
    // Simulate the initiator losing its local trust: the receiver must still
    // refuse a replay and retain the original pinned key.
    f.a.forget_peer().await.unwrap();
    assert!(f
        .a
        .pair_device(f.b.listen_address().to_string(), fresh)
        .await
        .is_err());
    assert_eq!(f.b.status().peer.unwrap().public_key, pinned);
    assert!(f.a.status().peer.is_none());
}

#[tokio::test]
async fn existing_pins_reconnect_without_any_new_pairing_credential() {
    let f = fixture().await;
    let key_a = f.a.status().public_key;
    let key_b = f.b.status().public_key;
    let disk_a = Config::load(&f.root.path().join("a")).unwrap();
    let disk_b = Config::load(&f.root.path().join("b")).unwrap();
    assert_eq!(disk_a.version, 1);
    assert_eq!(disk_b.version, 1);
    assert_eq!(disk_a.peer.as_ref().unwrap().public_key, key_b);
    assert_eq!(disk_b.peer.as_ref().unwrap().public_key, key_a);
    f.a.disconnect(EventCode::PeerDisconnected);
    until(|| !f.b.status().connected).await;
    f.a.reconnect().await.unwrap();
    until(|| f.a.status().connected && f.b.status().connected).await;
    assert_eq!(f.a.status().public_key, key_a);
    assert_eq!(f.b.status().public_key, key_b);
    assert!(f.a.status().invitation.is_none() && f.b.status().invitation.is_none());
}

#[tokio::test]
async fn engine_diagnostic_export_excludes_credentials_and_raw_errors() {
    use std::io::Read;
    let f = unpaired_fixture().await;
    let code = f.b.create_invitation().unwrap();
    let sensitive =
        format!("synthetic-secret-canary {code} 192.0.2.123 C:\\synthetic-private-file.txt");
    f.b.error(
        EventCode::OperationFailed,
        &anyhow::anyhow!(sensitive.clone()),
    );
    assert!(f.b.status().last_error.unwrap().contains(&sensitive));
    assert!(f
        .b
        .status()
        .recent_events
        .iter()
        .all(|event| !event.contains(&sensitive)));
    let path = f.b.export_diagnostics().await.unwrap();
    let file = std::fs::File::open(path).unwrap();
    let mut archive = zip::ZipArchive::new(file).unwrap();
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).unwrap();
        let mut text = String::new();
        entry.read_to_string(&mut text).unwrap();
        for private in [
            &code,
            "synthetic-secret-canary",
            "192.0.2.123",
            "synthetic-private-file.txt",
        ] {
            assert!(
                !text.contains(private),
                "Private metadata entered a diagnostic archive"
            );
        }
        assert!(!text.contains(&f.b.status().public_key));
    }
}

#[tokio::test]
async fn corrupt_identity_startup_records_safe_failure_without_regenerating() {
    let root = tempfile::tempdir().unwrap();
    let profile = root.path().join("profile");
    Config::load(&profile).unwrap();
    let corrupt = b"synthetic-private-corruption-canary";
    std::fs::write(profile.join("identity.dat"), corrupt).unwrap();
    let result = Engine::start_at(
        profile.clone(),
        root.path().join("downloads"),
        "127.0.0.1:0".parse().unwrap(),
    )
    .await;
    assert!(result.is_err());
    assert_eq!(
        std::fs::read(profile.join("identity.dat")).unwrap(),
        corrupt
    );
    let log = std::fs::read_to_string(profile.join("logs/emc.log")).unwrap();
    assert!(log.contains("operation_failed"));
    assert!(!log.contains("synthetic-private-corruption-canary"));
    assert!(!log.contains(&profile.display().to_string()));
}

#[tokio::test]
async fn shutdown_is_idempotent_and_flushes_its_single_stop_record() {
    let f = unpaired_fixture().await;
    f.a.shutdown();
    f.a.shutdown();
    let log = std::fs::read_to_string(f.root.path().join("a/logs/emc.log")).unwrap();
    let count = log
        .lines()
        .filter(|line| {
            serde_json::from_str::<serde_json::Value>(line).unwrap()["event"] == "engine_stopped"
        })
        .count();
    assert_eq!(count, 1);
}

#[tokio::test]
async fn pairs_once_persists_identity_and_heartbeats() {
    let f = fixture().await;
    assert!(f.a.status().connected && f.b.status().connected);
    assert!(f.b.status().invitation.is_none());
    assert!(f.a.create_invitation().is_err());
    assert_eq!(
        f.a.status().peer.unwrap().public_key,
        f.b.status().public_key
    );
    until(|| f.a.status().latency_ms.is_some()).await;
    assert_eq!(
        Config::load(&f.root.path().join("a")).unwrap().public_key,
        f.a.state().config.public_key
    );
}

#[tokio::test]
async fn file_requires_consent_then_hash_and_remote_save_confirmation() {
    let f = fixture().await;
    enable_files(&f.a);
    enable_files(&f.b);
    let path = f.root.path().join("example.txt");
    let bytes = vec![b'a'; CHUNK_BYTES * 3 + 917];
    tokio::fs::write(&path, &bytes).await.unwrap();
    let a = f.a.clone();
    let task = tokio::spawn(async move { a.send_file(path.display().to_string()).await });
    until(|| !f.b.status().pending_files.is_empty()).await;
    assert!(
        !f.root.path().join("b-downloads").exists(),
        "No file may be written before consent"
    );
    let offer = f.b.status().pending_files.remove(0);
    assert_eq!(offer.size, bytes.len() as u64);
    f.b.respond_file(offer.id.clone(), true).await.unwrap();
    timeout(Duration::from_secs(8), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(
        tokio::fs::read(
            f.root
                .path()
                .join("b-downloads")
                .join(offer.id)
                .join("example.txt")
        )
        .await
        .unwrap(),
        bytes
    );
    assert!(f.b.status().pending_files.is_empty());
}

#[tokio::test]
async fn file_decline_and_strict_100mb_limit() {
    let f = fixture().await;
    enable_files(&f.a);
    enable_files(&f.b);
    let path = f.root.path().join("sample.txt");
    tokio::fs::write(&path, b"example").await.unwrap();
    let a = f.a.clone();
    let task = tokio::spawn(async move { a.send_file(path.display().to_string()).await });
    until(|| !f.b.status().pending_files.is_empty()).await;
    let offer = f.b.status().pending_files.remove(0);
    f.b.respond_file(offer.id, false).await.unwrap();
    assert!(task.await.unwrap().is_err());
    assert!(!f.root.path().join("b-downloads").exists());
    let large = f.root.path().join("large.bin");
    let file = File::create(&large).await.unwrap();
    file.set_len(MAX_FILE_BYTES).await.unwrap();
    drop(file);
    assert!(f.a.send_file(large.display().to_string()).await.is_err());
    assert!(f.b.status().pending_files.is_empty());
}

#[tokio::test]
async fn pause_disconnects_and_forget_revokes_reconnect() {
    let f = fixture().await;
    let mut settings = f.b.status().settings;
    settings.enabled = false;
    f.b.update_settings(settings.clone()).unwrap();
    until(|| !f.a.status().connected).await;
    assert!(!f.b.status().connected);
    settings.enabled = true;
    f.b.update_settings(settings).unwrap();
    f.a.reconnect().await.unwrap();
    until(|| f.a.status().connected && f.b.status().connected).await;
    f.b.forget_peer().await.unwrap();
    until(|| !f.a.status().connected).await;
    assert!(f.b.status().peer.is_none());
    // A fresh transport response cannot grant authority after trust is deleted.
    let _ = f.a.reconnect().await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!f.b.status().connected);
}

#[tokio::test]
async fn old_bulk_sender_cannot_migrate_to_new_session() {
    let f = fixture().await;
    let mut settings = f.a.status().settings;
    settings.share_clipboard = true;
    f.a.update_settings(settings.clone()).unwrap();
    let old = f.a.bulk_session().unwrap();
    settings.share_clipboard = false;
    f.a.update_settings(settings).unwrap();
    assert!(f
        .a
        .bulk(&old, Message::Cancel { id: random_id() })
        .await
        .is_err());
    f.a.connect_bulk().await.unwrap();
    until(|| f.a.state().bulk.is_some()).await;
    assert_ne!(old.bulk_generation, f.a.state().bulk_generation);
    assert!(f
        .a
        .bulk(
            &old,
            Message::Chunk {
                id: random_id(),
                data: b"old private content".to_vec()
            }
        )
        .await
        .is_err());
}

#[tokio::test]
async fn delayed_heartbeat_reply_remains_valid_across_a_tick() {
    let root = tempfile::tempdir().unwrap();
    let e = Engine::start_at(
        root.path().join("profile"),
        root.path().join("downloads"),
        "127.0.0.1:0".parse().unwrap(),
    )
    .await
    .unwrap();
    let client_key = transport::keypair().unwrap();
    let pin = e.state().config.public_key;
    e.state().config.peer = Some(Peer {
        name: "fixture".into(),
        public_key: encode_key(&client_key.public),
        address: "127.0.0.1:1".into(),
    });
    let io = TcpStream::connect(e.listen_address()).await.unwrap();
    let mut channel = transport::connect(io, transport::CONTROL, &client_key.private, &pin, None)
        .await
        .unwrap();
    let Message::Ping(id) = channel.reader.recv().await.unwrap() else {
        panic!("expected ping")
    };
    tokio::time::sleep(Duration::from_millis(1250)).await;
    channel.writer.send(&Message::Pong(id)).await.unwrap();
    until(|| e.status().latency_ms.is_some()).await;
    assert!(e.status().connected);
    assert!(e.status().latency_ms.unwrap() >= 1200);
    e.shutdown();
}

#[tokio::test]
async fn stale_captured_input_is_dropped_after_reconnect() {
    let f = fixture().await;
    let epoch = f.a.0.platform.connection_epoch();
    f.a.disconnect(EventCode::PeerDisconnected);
    until(|| !f.b.status().connected).await;
    f.a.reconnect().await.unwrap();
    until(|| f.a.status().connected && f.b.status().connected).await;
    assert_ne!(epoch, f.a.0.platform.connection_epoch());
    f.a.forward_input(CapturedInput {
        captured_at: Instant::now(),
        epoch,
        event: InputEvent::Move { dx: 100, dy: 50 },
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(f.a.status().connected && f.b.status().connected);
}

#[tokio::test]
async fn invitation_installation_rechecks_expiry_replacement_and_revocation() {
    let root = tempfile::tempdir().unwrap();
    let e = Engine::start_at(
        root.path().join("profile"),
        root.path().join("downloads"),
        "127.0.0.1:0".parse().unwrap(),
    )
    .await
    .unwrap();
    e.create_invitation().unwrap();
    let invitation_id = e.state().invitation.as_ref().unwrap().id;
    let generation = e.state().generation;
    e.create_invitation().unwrap();
    assert!(e
        .remember(
            "peer".into(),
            [9; 32],
            "127.0.0.1:1".parse().unwrap(),
            false,
            generation,
            Some(&invitation_id)
        )
        .is_err());
    e.create_invitation().unwrap();
    let invitation_id = e.state().invitation.as_ref().unwrap().id;
    e.state().invitation.as_mut().unwrap().expires = Instant::now() - Duration::from_secs(1);
    assert!(e
        .remember(
            "peer".into(),
            [9; 32],
            "127.0.0.1:1".parse().unwrap(),
            false,
            generation,
            Some(&invitation_id)
        )
        .is_err());
    e.shutdown();
    assert!(e
        .remember(
            "peer".into(),
            [9; 32],
            "127.0.0.1:1".parse().unwrap(),
            true,
            generation,
            None
        )
        .is_err());
}

#[tokio::test]
async fn immediate_shutdown_releases_listener_without_task_startup_race() {
    let root = tempfile::tempdir().unwrap();
    let e = Engine::start_at(
        root.path().join("profile"),
        root.path().join("downloads"),
        "127.0.0.1:0".parse().unwrap(),
    )
    .await
    .unwrap();
    let addr = e.listen_address();
    e.shutdown();
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert!(TcpStream::connect(addr).await.is_err());
}

#[tokio::test]
async fn receiving_rejects_unsolicited_data_and_bad_hash_cleans_partial_file() {
    let f = fixture().await;
    enable_files(&f.a);
    enable_files(&f.b);
    let (generation, owner) = {
        let s = f.b.state();
        (s.generation, s.bulk_generation)
    };
    assert!(f
        .b
        .receive_bulk(
            Message::Chunk {
                id: random_id(),
                data: vec![1]
            },
            generation,
            owner
        )
        .await
        .is_err());
    let id = random_id();
    f.b.receive_bulk(
        Message::Offer {
            id: id.clone(),
            name: "test.txt".into(),
            size: 1,
            kind: BulkKind::File,
        },
        generation,
        owner,
    )
    .await
    .unwrap();
    f.b.respond_file(id.clone(), true).await.unwrap();
    f.b.receive_bulk(
        Message::Chunk {
            id: id.clone(),
            data: vec![7],
        },
        generation,
        owner,
    )
    .await
    .unwrap();
    assert!(f
        .b
        .receive_bulk(
            Message::Complete {
                id: id.clone(),
                sha256: [0; 32]
            },
            generation,
            owner
        )
        .await
        .is_err());
    f.b.0.incoming.lock().await.take();
    assert!(!f.root.path().join("b-downloads").join(id).exists());
}

struct SyntheticPeer {
    engine: Engine,
    key: snow::Keypair,
    _root: TempDir,
}
impl Drop for SyntheticPeer {
    fn drop(&mut self) {
        self.engine.shutdown();
    }
}
impl SyntheticPeer {
    async fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let engine = Engine::start_at(
            root.path().join("profile"),
            root.path().join("downloads"),
            "127.0.0.1:0".parse().unwrap(),
        )
        .await
        .unwrap();
        let key = transport::keypair().unwrap();
        engine.state().config.peer = Some(Peer {
            name: "synthetic peer".into(),
            public_key: encode_key(&key.public),
            address: "127.0.0.1:1".into(),
        });
        Self {
            engine,
            key,
            _root: root,
        }
    }
    async fn connect(&self, kind: u8) -> Channel {
        let pin = self.engine.state().config.public_key;
        let io = TcpStream::connect(self.engine.listen_address())
            .await
            .unwrap();
        transport::connect(io, kind, &self.key.private, &pin, None)
            .await
            .unwrap()
    }
}

#[tokio::test]
async fn slow_native_sink_does_not_block_heartbeat_or_opposite_input_and_preserves_order() {
    let peer = SyntheticPeer::new().await;
    let gate = Arc::new(Notify::new());
    let (started, mut began) = mpsc::channel(1);
    let (applied, mut events) = mpsc::channel(16);
    *peer.engine.0.input_sink.lock().unwrap() = Some(Arc::new({
        let gate = gate.clone();
        move |event, _epoch| {
            let gate = gate.clone();
            let started = started.clone();
            let applied = applied.clone();
            Box::pin(async move {
                if matches!(event, InputEvent::Enter { .. }) {
                    started.try_send(()).unwrap();
                    gate.notified().await;
                }
                applied.try_send(event).unwrap();
                Ok(())
            })
        }
    }));
    let mut channel = peer.connect(transport::CONTROL).await;
    channel
        .writer
        .send(&Message::Input(InputEvent::Enter {
            from_left: true,
            y: 100,
        }))
        .await
        .unwrap();
    timeout(Duration::from_secs(1), began.recv())
        .await
        .unwrap()
        .unwrap();
    let key_down = InputEvent::Key {
        scan: 30,
        vk: 65,
        extended: false,
        down: true,
    };
    let key_up = InputEvent::Key {
        scan: 30,
        vk: 65,
        extended: false,
        down: false,
    };
    let button = InputEvent::Button {
        button: emc_protocol::MouseButton::Left,
        down: true,
    };
    for event in [
        InputEvent::Move { dx: 1, dy: 2 },
        InputEvent::Move { dx: 3, dy: 4 },
        key_down.clone(),
        InputEvent::Move { dx: 5, dy: 6 },
        InputEvent::Move { dx: 7, dy: 8 },
        button.clone(),
        InputEvent::Wheel {
            delta: 120,
            horizontal: false,
        },
        InputEvent::ReleaseAll,
        InputEvent::Leave,
    ] {
        channel.writer.send(&Message::Input(event)).await.unwrap();
    }
    channel.writer.send(&Message::Ping(987)).await.unwrap();
    peer.engine.control(Message::Input(key_up.clone())).unwrap();
    let (mut pong, mut opposite) = (false, false);
    timeout(Duration::from_secs(1), async {
        while !pong || !opposite {
            match channel.reader.recv().await.unwrap() {
                Message::Ping(id) => channel.writer.send(&Message::Pong(id)).await.unwrap(),
                Message::Pong(987) => pong = true,
                Message::Input(event) => {
                    assert_eq!(event, key_up);
                    opposite = true;
                }
                message => panic!("Unexpected synthetic message: {message:?}"),
            }
        }
    })
    .await
    .unwrap();
    assert!(peer.engine.status().connected);
    assert!(
        events.try_recv().is_err(),
        "The native sink must still be blocked while network/control progress"
    );
    gate.notify_one();
    for expected in [
        InputEvent::Enter {
            from_left: true,
            y: 100,
        },
        InputEvent::Move { dx: 4, dy: 6 },
        key_down,
        InputEvent::Move { dx: 12, dy: 14 },
        button,
        InputEvent::Wheel {
            delta: 120,
            horizontal: false,
        },
        InputEvent::ReleaseAll,
        InputEvent::Leave,
    ] {
        assert_eq!(
            timeout(Duration::from_secs(1), events.recv())
                .await
                .unwrap()
                .unwrap(),
            expected
        );
    }
    let metrics = peer.engine.status().control_diagnostics;
    assert!(metrics.incoming_moves_coalesced >= 2);
    assert_eq!(metrics.stale_input_failures, 0);
    until(|| {
        peer.engine
            .status()
            .control_diagnostics
            .round_trip_sample_age_ms
            .is_some()
    })
    .await;
}

#[tokio::test]
async fn eof_cancels_pending_native_work_discards_backlog_and_old_epoch_cannot_replay() {
    let peer = SyntheticPeer::new().await;
    let gate = Arc::new(Notify::new());
    let (started, mut began) = mpsc::channel(1);
    let applied = Arc::new(AtomicU64::new(0));
    *peer.engine.0.input_sink.lock().unwrap() = Some(Arc::new({
        let gate = gate.clone();
        let applied = applied.clone();
        move |_event, _epoch| {
            let gate = gate.clone();
            let started = started.clone();
            let applied = applied.clone();
            Box::pin(async move {
                let _ = started.try_send(());
                gate.notified().await;
                applied.fetch_add(1, Ordering::Relaxed);
                Ok(())
            })
        }
    }));
    let mut channel = peer.connect(transport::CONTROL).await;
    let old_epoch = peer.engine.0.platform.connection_epoch();
    channel
        .writer
        .send(&Message::Input(InputEvent::Enter {
            from_left: true,
            y: 1,
        }))
        .await
        .unwrap();
    began.recv().await.unwrap();
    channel
        .writer
        .send(&Message::Input(InputEvent::Key {
            scan: 30,
            vk: 65,
            extended: false,
            down: true,
        }))
        .await
        .unwrap();
    // EOF must bypass authenticated messages waiting for the blocked native sink.
    drop(channel);
    until(|| !peer.engine.status().connected).await;
    assert_ne!(peer.engine.0.platform.connection_epoch(), old_epoch);
    assert!(!peer.engine.0.platform.is_remote());
    let mut fresh = peer.connect(transport::CONTROL).await;
    peer.engine.wait_control_ready().await.unwrap();
    peer.engine.forward_input(CapturedInput {
        event: InputEvent::Leave,
        epoch: old_epoch,
        captured_at: Instant::now(),
    });
    gate.notify_waiters();
    // A fresh ping synchronizes through both network tasks after cancellation.
    fresh.writer.send(&Message::Ping(654)).await.unwrap();
    loop {
        match fresh.reader.recv().await.unwrap() {
            Message::Pong(654) => break,
            Message::Ping(id) => fresh.writer.send(&Message::Pong(id)).await.unwrap(),
            message => panic!("Revoked input escaped into the replacement session: {message:?}"),
        }
    }
    assert_eq!(applied.load(Ordering::Relaxed), 0);
    assert!(peer.engine.status().connected);
}

#[tokio::test]
async fn stale_native_work_disconnects_without_replaying_actions() {
    let peer = SyntheticPeer::new().await;
    let (started, mut began) = mpsc::channel(1);
    *peer.engine.0.input_sink.lock().unwrap() = Some(Arc::new(move |_, _| {
        let started = started.clone();
        Box::pin(async move {
            let _ = started.try_send(());
            std::future::pending::<Result<()>>().await
        })
    }));
    let mut channel = peer.connect(transport::CONTROL).await;
    channel
        .writer
        .send(&Message::Input(InputEvent::Enter {
            from_left: true,
            y: 1,
        }))
        .await
        .unwrap();
    began.recv().await.unwrap();
    channel
        .writer
        .send(&Message::Input(InputEvent::Key {
            scan: 30,
            vk: 65,
            extended: false,
            down: true,
        }))
        .await
        .unwrap();
    until(|| !peer.engine.status().connected).await;
    let snapshot = peer.engine.status();
    assert_eq!(snapshot.control_diagnostics.stale_input_failures, 1);
    assert_eq!(
        snapshot
            .control_diagnostics
            .native_completion_deadline_failures,
        1
    );
    assert_eq!(
        snapshot.control_diagnostics.outgoing_queue_stale_failures,
        0
    );
    assert_eq!(
        snapshot
            .control_diagnostics
            .outgoing_write_deadline_failures,
        0
    );
    assert_eq!(
        snapshot.control_diagnostics.incoming_queue_stale_failures,
        0
    );
    assert!(snapshot.control_diagnostics.max_native_apply_ms >= 190);
    assert!(!peer.engine.0.platform.is_remote());
    until(|| {
        peer.engine
            .status()
            .recent_events
            .iter()
            .any(|event| event == EventCode::ControlChannelClosed.description())
    })
    .await;
    assert!(peer
        .engine
        .status()
        .recent_events
        .iter()
        .any(|event| event == EventCode::InputDispatchStale.description()));
    assert!(!peer
        .engine
        .status()
        .recent_events
        .iter()
        .any(|event| event == EventCode::AuthenticationRejected.description()));
}

#[tokio::test]
async fn bulk_started_before_control_waits_for_installation_without_generation_race() {
    let peer = SyntheticPeer::new().await;
    let waiting = peer.engine.0.bulk_waiting.notified();
    let bulk = peer.connect(transport::BULK);
    tokio::pin!(bulk);
    // The test hook signals only after the actual BULK preamble was received
    // and before the readiness wait. No timing-based installation sleep.
    tokio::select! {
        _ = waiting => {},
        _ = &mut bulk => panic!("Bulk must wait for authenticated control installation"),
    }
    assert!(!peer.engine.status().connected);
    let mut control = peer.connect(transport::CONTROL).await;
    let mut bulk = timeout(Duration::from_secs(2), bulk).await.unwrap();
    peer.engine.wait_control_ready().await.unwrap();
    until(|| peer.engine.state().bulk.is_some()).await;
    let generation = peer.engine.state().generation;
    control.writer.send(&Message::Ping(765)).await.unwrap();
    loop {
        match control.reader.recv().await.unwrap() {
            Message::Pong(765) => break,
            Message::Ping(id) => control.writer.send(&Message::Pong(id)).await.unwrap(),
            other => panic!("Unexpected control message: {other:?}"),
        }
    }
    // Exercise the bulk connection, rather than relying only on local flags.
    bulk.writer
        .send(&Message::Offer {
            id: random_id(),
            name: "synthetic.txt".into(),
            size: 1,
            kind: BulkKind::File,
        })
        .await
        .unwrap();
    loop {
        match control.reader.recv().await.unwrap() {
            Message::Decision { accept: false, .. } => break,
            Message::Ping(id) => control.writer.send(&Message::Pong(id)).await.unwrap(),
            other => panic!("Unexpected control message: {other:?}"),
        }
    }
    assert_eq!(peer.engine.state().generation, generation);
    assert!(!peer
        .engine
        .status()
        .recent_events
        .iter()
        .any(|event| event == EventCode::AuthenticationRejected.description()));
}

#[tokio::test]
async fn stale_capture_age_is_not_refreshed_when_outgoing_input_is_enqueued() {
    let peer = SyntheticPeer::new().await;
    let mut channel = peer.connect(transport::CONTROL).await;
    peer.engine.wait_control_ready().await.unwrap();
    peer.engine.forward_input(CapturedInput {
        event: InputEvent::Key {
            scan: 30,
            vk: 65,
            extended: false,
            down: true,
        },
        epoch: peer.engine.0.platform.connection_epoch(),
        captured_at: Instant::now() - MAX_INPUT_AGE - Duration::from_millis(1),
    });
    until(|| !peer.engine.status().connected).await;
    timeout(Duration::from_secs(1), async {
        while let Ok(message) = channel.reader.recv().await {
            assert!(
                !matches!(message, Message::Input(_)),
                "An already stale captured key must never reach the peer"
            );
        }
    })
    .await
    .unwrap();
    let metrics = peer.engine.status().control_diagnostics;
    assert_eq!(metrics.stale_input_failures, 1);
    assert_eq!(metrics.outgoing_queue_stale_failures, 1);
    assert_eq!(metrics.outgoing_write_deadline_failures, 0);
    assert_eq!(metrics.incoming_queue_stale_failures, 0);
    assert_eq!(metrics.native_completion_deadline_failures, 0);
    assert!(metrics.max_outgoing_queue_age_ms >= 200);
}

#[tokio::test]
async fn blocked_address_refresh_cannot_block_status_or_authenticated_input() {
    // Keep the fake OS lookup blocked until both status and an actual encrypted
    // write complete. A timeout-based microbenchmark would not establish this
    // independence and would depend on the machine's scheduler.
    struct ReleaseLookup(Option<std::sync::mpsc::Sender<()>>);
    impl Drop for ReleaseLookup {
        fn drop(&mut self) {
            if let Some(release) = self.0.take() {
                let _ = release.send(());
            }
        }
    }
    let peer = SyntheticPeer::new().await;
    let mut channel = peer.connect(transport::CONTROL).await;
    peer.engine.wait_control_ready().await.unwrap();
    let previous_hints = peer.engine.status().local_addresses;
    let (started, began) = oneshot::channel();
    let (release, released) = std::sync::mpsc::channel();
    let release = ReleaseLookup(Some(release));
    let refresh = tokio::spawn({
        let engine = peer.engine.clone();
        async move {
            engine
                .refresh_address_hints(move |port| {
                    let _ = started.send(());
                    released.recv().unwrap();
                    vec![format!("192.0.2.19:{port}")]
                })
                .await;
        }
    });
    began.await.unwrap();
    // No mutex is held while probing. In particular, status remains a cache
    // read, and the same session lock remains available to captured input.
    assert!(peer.engine.0.state.try_lock().is_ok());
    assert!(peer.engine.0.address_hints.try_lock().is_ok());
    assert_eq!(peer.engine.status().local_addresses, previous_hints);
    let event = InputEvent::Key {
        scan: 30,
        vk: 65,
        extended: false,
        down: true,
    };
    peer.engine.forward_input(CapturedInput {
        event: event.clone(),
        epoch: peer.engine.0.platform.connection_epoch(),
        captured_at: Instant::now(),
    });
    timeout(Duration::from_secs(1), async {
        loop {
            match channel.reader.recv().await.unwrap() {
                Message::Input(actual) => {
                    assert_eq!(actual, event);
                    break;
                }
                Message::Ping(id) => channel.writer.send(&Message::Pong(id)).await.unwrap(),
                unexpected => panic!("Unexpected synthetic message: {unexpected:?}"),
            }
        }
    })
    .await
    .unwrap();
    assert!(
        !refresh.is_finished(),
        "Input must complete while the lookup is still blocked"
    );
    drop(release);
    refresh.await.unwrap();
    assert_eq!(
        peer.engine.status().local_addresses,
        vec![format!(
            "192.0.2.19:{}",
            peer.engine.listen_address().port()
        )]
    );
    assert!(peer.engine.status().connected);
}

struct SyntheticBulkServer {
    accepted: Arc<AtomicU64>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for SyntheticBulkServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn synthetic_bulk_server(peer: &SyntheticPeer) -> SyntheticBulkServer {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    peer.engine.state().config.peer.as_mut().unwrap().address =
        listener.local_addr().unwrap().to_string();
    let accepted = Arc::new(AtomicU64::new(0));
    let count = accepted.clone();
    let private = Zeroizing::new(peer.key.private.clone());
    let pin = peer.engine.state().config.public_key;
    let task = tokio::spawn(async move {
        let mut channels = Vec::new();
        loop {
            let (mut io, _) = listener.accept().await.unwrap();
            count.fetch_add(1, Ordering::Relaxed);
            assert_eq!(transport::preamble(&mut io).await.unwrap(), transport::BULK);
            channels.push(
                transport::accept(io, transport::BULK, &private, Some(&pin), None)
                    .await
                    .unwrap(),
            );
        }
    });
    SyntheticBulkServer { accepted, task }
}

#[tokio::test]
async fn concurrent_bulk_attempts_coalesce_until_the_first_installation_commits() {
    let peer = SyntheticPeer::new().await;
    let _control = peer.connect(transport::CONTROL).await;
    peer.engine.wait_control_ready().await.unwrap();
    let server = synthetic_bulk_server(&peer).await;
    let previous = peer.engine.state().bulk_generation;
    let installation = peer.engine.0.incoming.lock().await;
    let installing = peer.engine.0.bulk_install_waiting.notified();
    let first = tokio::spawn({
        let e = peer.engine.clone();
        async move { e.connect_bulk().await }
    });
    timeout(Duration::from_secs(1), installing).await.unwrap();
    let second = peer.engine.connect_bulk();
    tokio::pin!(second);
    // The first authenticated channel is blocked exactly before installation.
    // A second caller must wait here, not open another socket/Noise handshake.
    assert!(
        std::future::poll_fn(|cx| std::task::Poll::Ready(
            std::future::Future::poll(second.as_mut(), cx).is_pending()
        ))
        .await
    );
    assert_eq!(server.accepted.load(Ordering::Relaxed), 1);
    assert!(peer.engine.state().bulk.is_none());
    drop(installation);
    timeout(Duration::from_secs(1), first)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    timeout(Duration::from_secs(1), second)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(server.accepted.load(Ordering::Relaxed), 1);
    assert_eq!(
        peer.engine.state().bulk_generation,
        previous.wrapping_add(1)
    );
    assert!(peer.engine.state().bulk.is_some());
    assert!(!peer
        .engine
        .status()
        .recent_events
        .iter()
        .any(|event| event == EventCode::BulkChannelClosed.description()));
}

#[tokio::test]
async fn cancelled_bulk_caller_cannot_release_the_installation_dial_guard_early() {
    let peer = SyntheticPeer::new().await;
    let _control = peer.connect(transport::CONTROL).await;
    peer.engine.wait_control_ready().await.unwrap();
    let server = synthetic_bulk_server(&peer).await;
    let installation = peer.engine.0.incoming.lock().await;
    let installing = peer.engine.0.bulk_install_waiting.notified();
    let first = tokio::spawn({
        let e = peer.engine.clone();
        async move { e.connect_bulk().await }
    });
    timeout(Duration::from_secs(1), installing).await.unwrap();
    first.abort();
    assert!(first.await.unwrap_err().is_cancelled());
    let second = peer.engine.connect_bulk();
    tokio::pin!(second);
    assert!(
        std::future::poll_fn(|cx| std::task::Poll::Ready(
            std::future::Future::poll(second.as_mut(), cx).is_pending()
        ))
        .await
    );
    assert_eq!(server.accepted.load(Ordering::Relaxed), 1);
    drop(installation);
    timeout(Duration::from_secs(1), second)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(server.accepted.load(Ordering::Relaxed), 1);
    assert!(peer.engine.state().bulk.is_some());
}

#[tokio::test]
async fn revoked_bulk_waiters_cannot_dial_or_join_a_replacement_control_session() {
    let peer = SyntheticPeer::new().await;
    let _control = peer.connect(transport::CONTROL).await;
    peer.engine.wait_control_ready().await.unwrap();
    let server = synthetic_bulk_server(&peer).await;
    let original_generation = peer.engine.state().generation;
    let pin = peer.engine.status().peer.unwrap().public_key;
    let installation = peer.engine.0.incoming.lock().await;
    let installing = peer.engine.0.bulk_install_waiting.notified();
    let first = tokio::spawn({
        let e = peer.engine.clone();
        async move { e.connect_bulk().await }
    });
    timeout(Duration::from_secs(1), installing).await.unwrap();
    let second = peer.engine.connect_bulk();
    tokio::pin!(second);
    assert!(
        std::future::poll_fn(|cx| std::task::Poll::Ready(
            std::future::Future::poll(second.as_mut(), cx).is_pending()
        ))
        .await
    );
    peer.engine.disconnect(EventCode::PeerDisconnected);
    // Cancellation and dial-guard release must not wait for transfer state.
    assert_eq!(
        FailureReason::from_error(
            &timeout(Duration::from_secs(1), first)
                .await
                .unwrap()
                .unwrap()
                .unwrap_err()
        ),
        FailureReason::SessionRevoked
    );
    assert_eq!(
        FailureReason::from_error(
            &timeout(Duration::from_secs(1), second)
                .await
                .unwrap()
                .unwrap_err()
        ),
        FailureReason::SessionRevoked
    );
    let _fresh = peer.connect(transport::CONTROL).await;
    peer.engine.wait_control_ready().await.unwrap();
    assert_ne!(peer.engine.state().generation, original_generation);
    assert_eq!(
        FailureReason::from_error(
            &peer
                .engine
                .connect_bulk_for(original_generation, pin.clone())
                .await
                .unwrap_err()
        ),
        FailureReason::SessionRevoked
    );
    assert_eq!(
        FailureReason::from_error(
            &peer
                .engine
                .wait_control_installation(original_generation.wrapping_sub(1), &pin)
                .await
                .unwrap_err()
        ),
        FailureReason::SessionRevoked
    );
    assert_eq!(server.accepted.load(Ordering::Relaxed), 1);
    assert!(peer.engine.state().bulk.is_none());
    drop(installation);
    timeout(Duration::from_secs(1), peer.engine.connect_bulk())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(server.accepted.load(Ordering::Relaxed), 2);
    assert!(peer.engine.state().bulk.is_some());
}

#[tokio::test]
async fn local_paused_admission_is_typed_but_remote_eof_does_not_claim_peer_policy() {
    let peer = SyntheticPeer::new().await;
    let mut settings = peer.engine.status().settings;
    settings.enabled = false;
    peer.engine.update_settings(settings).unwrap();
    let pin = peer.engine.state().config.public_key;
    let io = TcpStream::connect(peer.engine.listen_address())
        .await
        .unwrap();
    let remote_error = transport::connect(io, transport::CONTROL, &peer.key.private, &pin, None)
        .await
        .err()
        .expect("Local pause must reject admission");
    assert_ne!(
        FailureReason::from_error(&remote_error),
        FailureReason::PolicyPaused,
        "EOF cannot authenticate a remote policy explanation"
    );
    until(|| {
        peer.engine
            .status()
            .recent_events
            .iter()
            .any(|event| event == EventCode::ConnectionAdmissionRejected.description())
    })
    .await;
    peer.engine
        .0
        .diagnostics
        .flush_timeout(Duration::from_millis(250));
    let logs = std::fs::read_to_string(peer.engine.0.config_dir.join("logs/emc.log")).unwrap();
    let records: Vec<serde_json::Value> = logs
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(records
        .iter()
        .any(|record| record["event"] == "connection_admission_rejected"
            && record["failure"] == "policy_paused"));
    assert!(!records
        .iter()
        .any(|record| record["event"] == "authentication_rejected"));
}

#[tokio::test]
async fn bulk_only_policy_revocation_blocks_a_late_outgoing_installation() {
    let peer = SyntheticPeer::new().await;
    let _control = peer.connect(transport::CONTROL).await;
    peer.engine.wait_control_ready().await.unwrap();
    let server = synthetic_bulk_server(&peer).await;
    let mut settings = peer.engine.status().settings;
    settings.share_files = true;
    peer.engine.update_settings(settings.clone()).unwrap();
    let control_generation = peer.engine.state().generation;
    let previous_bulk = peer.engine.state().bulk_generation;
    let installation = peer.engine.0.incoming.lock().await;
    let installing = peer.engine.0.bulk_install_waiting.notified();
    let first = tokio::spawn({
        let e = peer.engine.clone();
        async move { e.connect_bulk().await }
    });
    timeout(Duration::from_secs(1), installing).await.unwrap();
    settings.share_files = false;
    peer.engine.update_settings(settings).unwrap();
    assert_eq!(peer.engine.state().generation, control_generation);
    assert_eq!(
        peer.engine.state().bulk_generation,
        previous_bulk.wrapping_add(1)
    );
    drop(installation);
    assert_eq!(
        FailureReason::from_error(
            &timeout(Duration::from_secs(1), first)
                .await
                .unwrap()
                .unwrap()
                .unwrap_err()
        ),
        FailureReason::SessionRevoked
    );
    assert!(peer.engine.state().bulk.is_none());
    assert_eq!(
        peer.engine.state().bulk_generation,
        previous_bulk.wrapping_add(1)
    );
    assert_eq!(server.accepted.load(Ordering::Relaxed), 1);
    assert!(peer.engine.status().connected);
}

#[tokio::test]
async fn authenticated_bulk_redial_waits_for_natural_retirement_without_evicting_the_old_slot() {
    let peer = SyntheticPeer::new().await;
    let _control = peer.connect(transport::CONTROL).await;
    peer.engine.wait_control_ready().await.unwrap();
    let old = peer.connect(transport::BULK).await;
    until(|| peer.engine.state().bulk.is_some()).await;
    let previous = peer.engine.state().bulk_generation;
    let waiting = peer.engine.0.bulk_retirement_waiting.notified();
    let _replacement = peer.connect(transport::BULK).await;
    timeout(Duration::from_secs(1), waiting).await.unwrap();
    assert_eq!(
        peer.engine.state().bulk_generation,
        previous,
        "A newly authenticated channel cannot evict an active bulk slot"
    );
    assert!(peer.engine.state().bulk.is_some());
    drop(old);
    until(|| peer.engine.state().bulk_generation == previous.wrapping_add(1)).await;
    assert!(peer.engine.state().bulk.is_some());
    assert!(peer.engine.status().connected);
}

#[tokio::test]
async fn active_bulk_slot_is_not_replaced_when_retirement_wait_expires() {
    let peer = SyntheticPeer::new().await;
    let _control = peer.connect(transport::CONTROL).await;
    peer.engine.wait_control_ready().await.unwrap();
    let _old = peer.connect(transport::BULK).await;
    until(|| peer.engine.state().bulk.is_some()).await;
    let generation = peer.engine.state().generation;
    let previous = peer.engine.state().bulk_generation;
    let pin = peer.engine.status().peer.unwrap().public_key;
    let error = peer
        .engine
        .wait_bulk_vacant(generation, previous, &pin)
        .await
        .unwrap_err();
    assert_eq!(
        FailureReason::from_error(&error),
        FailureReason::DuplicateSession
    );
    assert_eq!(peer.engine.state().bulk_generation, previous);
    assert!(peer.engine.state().bulk.is_some());
}

#[tokio::test]
async fn typed_native_enter_failure_is_operational_and_never_exports_raw_context() {
    let peer = SyntheticPeer::new().await;
    *peer.engine.0.input_sink.lock().unwrap() = Some(Arc::new(|_, _| {
        Box::pin(async {
            Err(anyhow::anyhow!("SYNTHETIC_PRIVATE_NATIVE_CONTEXT")
                .context(emc_platform::NativeInputFailure::CursorUnavailable))
        })
    }));
    let mut channel = peer.connect(transport::CONTROL).await;
    channel
        .writer
        .send(&Message::Input(InputEvent::Enter {
            from_left: true,
            y: 1,
        }))
        .await
        .unwrap();
    until(|| !peer.engine.status().connected).await;
    until(|| {
        peer.engine
            .status()
            .recent_events
            .iter()
            .any(|event| event == EventCode::InputEntryFailed.description())
    })
    .await;
    peer.engine
        .0
        .diagnostics
        .flush_timeout(Duration::from_millis(250));
    let logs = std::fs::read_to_string(peer.engine.0.config_dir.join("logs/emc.log")).unwrap();
    assert!(!logs.contains("SYNTHETIC_PRIVATE_NATIVE_CONTEXT"));
    let records: Vec<serde_json::Value> = logs
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(records
        .iter()
        .any(|record| record["event"] == "input_entry_failed"
            && record["failure"] == "native_cursor_unavailable"));
    assert!(!records
        .iter()
        .any(|record| record["event"] == "input_injection_failed"));
    assert_eq!(
        peer.engine
            .status()
            .control_diagnostics
            .stale_input_failures,
        0
    );
}

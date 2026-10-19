pub use crate::input_pipeline::ControlDiagnostics;
#[cfg(test)]
use crate::input_pipeline::MAX_INPUT_AGE;
pub use crate::store::{default_config_dir, default_receive_dir, Peer, Settings};
use crate::{
    diagnostics::{DiagnosticLog, DiagnosticStatus, DiagnosticSummary, EventCode, FailureReason},
    input_pipeline::{self, Counters, Direction, OrderedQueue, StaleStage, Timed},
    pairing::{self, ManualCode},
    store::{decode_key, encode_key, parse_address, Config},
    transport::{self, Channel},
};
use anyhow::{bail, ensure, Context, Result};
use emc_platform::clipboard_image::{ClipboardImage, PreparedImage};
use emc_platform::{CapturedInput, InputController};
use emc_protocol::{
    validate_filename, validate_id, BulkKind, InputEvent, Message, CHUNK_BYTES,
    MAX_CLIPBOARD_BYTES, MAX_CLIPBOARD_IMAGE_BYTES, MAX_FILE_BYTES, PORT,
};
use rand::{rngs::OsRng, RngCore};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex, MutexGuard,
    },
    time::{Duration, Instant},
};
use tokio::{
    fs::File,
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{mpsc, oneshot, watch, Mutex as AsyncMutex, Notify, OwnedMutexGuard, Semaphore},
    time::{interval, timeout},
};
use zeroize::Zeroizing;

#[path = "engine_preferences.rs"]
mod preferences;
pub use preferences::{ClipboardNotice, NotificationSnapshot};

#[derive(Debug, Clone, Serialize)]
pub struct FileOffer {
    pub id: String,
    pub name: String,
    pub size: u64,
}
#[derive(Serialize)]
pub struct Status {
    pub device_name: String,
    pub public_key: String,
    pub local_addresses: Vec<String>,
    pub enabled: bool,
    pub connected: bool,
    pub remote_name: Option<String>,
    pub peer: Option<Peer>,
    pub settings: Settings,
    pub invitation: Option<String>,
    pub invitation_seconds_left: u64,
    pub pairing_attempts_remaining: u8,
    pub last_error: Option<String>,
    pub latency_ms: Option<u64>,
    pub pending_files: Vec<FileOffer>,
    pub recent_events: Vec<String>,
    pub receiving_directory: String,
    pub native_input: bool,
    pub diagnostics: DiagnosticStatus,
    pub input_diagnostics: emc_platform::InputDiagnostics,
    pub control_diagnostics: ControlDiagnostics,
}
struct Invitation {
    code: ManualCode,
    id: [u8; 16],
    expires: Instant,
    attempts: u8,
}
struct Runtime {
    config: Config,
    invitation: Option<Invitation>,
    connected: bool,
    generation: u64,
    bulk_generation: u64,
    clipboard_revision: u64,
    clipboard_content_revision: u64,
    awake_revision: u64,
    peer_activity: Option<(u64, u64, Instant)>,
    clipboard_notice_serial: u64,
    notification_revision: u64,
    clipboard_notice: Option<ClipboardNotice>,
    clipboard_notice_at: Option<Instant>,
    clipboard_notice_generation: Option<u64>,
    control: Option<Arc<OrderedQueue<Message>>>,
    control_stop: Option<watch::Sender<bool>>,
    bulk: Option<mpsc::Sender<Message>>,
    last_error: Option<String>,
    latency_ms: Option<u64>,
    latency_updated: Option<Instant>,
    events: Vec<String>,
    pending: Vec<FileOffer>,
    decisions: HashMap<String, oneshot::Sender<bool>>,
    suppress_clipboard: Option<[u8; 32]>,
    suppress_clipboard_image: Option<[u8; 32]>,
}
struct Inner {
    state: Mutex<Runtime>,
    // Display-only hints: route lookups must never hold the input/session lock.
    address_hints: Mutex<Vec<String>>,
    config_dir: PathBuf,
    receive_dir: PathBuf,
    platform: InputController,
    diagnostics: DiagnosticLog,
    pipeline: Arc<Counters>,
    control_ready: Notify,
    bulk_changed: Notify,
    #[cfg(test)]
    bulk_waiting: Notify,
    #[cfg(test)]
    bulk_install_waiting: Notify,
    #[cfg(test)]
    bulk_retirement_waiting: Notify,
    #[cfg(test)]
    input_sink: Mutex<Option<TestInputSink>>,
    #[cfg(test)]
    image_sink: Mutex<Option<TestImageSink>>,
    #[cfg(test)]
    text_sink: Mutex<Option<TestTextSink>>,
    incoming: AsyncMutex<Option<Incoming>>,
    outgoing: AsyncMutex<()>,
    pairing: AsyncMutex<()>,
    dialing: AsyncMutex<()>,
    bulk_dialing: Arc<AsyncMutex<()>>,
    stop: watch::Sender<bool>,
    address: SocketAddr,
}
#[derive(Clone)]
pub struct Engine(Arc<Inner>);
#[cfg(test)]
type TestInputSink = Arc<
    dyn Fn(
            InputEvent,
            u64,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send>>
        + Send
        + Sync,
>;
#[cfg(test)]
type TestImageSink = Arc<dyn Fn(&PreparedImage) -> Result<()> + Send + Sync>;
#[cfg(test)]
type TestTextSink = Arc<dyn Fn(&str) -> Result<()> + Send + Sync>;
#[derive(Debug, Clone, Copy)]
enum EstablishedChannel {
    Control,
    Bulk,
}
impl std::fmt::Display for EstablishedChannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Control => "Authenticated control session",
            Self::Bulk => "Authenticated bulk session",
        })
    }
}
impl std::error::Error for EstablishedChannel {}
struct Incoming {
    offer: FileOffer,
    kind: BulkKind,
    accepted: bool,
    received: u64,
    hash: Sha256,
    text: Vec<u8>,
    file: Option<File>,
    directory: Option<PathBuf>,
    finished: bool,
    created: Instant,
    owner: u64,
}
struct BulkSession {
    tx: mpsc::Sender<Message>,
    generation: u64,
    bulk_generation: u64,
    clipboard_revision: Option<(u64, u64, BulkKind)>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ClipboardContext {
    generation: u64,
    bulk_generation: u64,
    revision: u64,
    content_revision: u64,
    kind: BulkKind,
}
impl ClipboardContext {
    fn same_activation(self, other: Self) -> bool {
        self.generation == other.generation
            && self.bulk_generation == other.bulk_generation
            && self.revision == other.revision
            && self.kind == other.kind
    }
}
type ClipboardBaseline = Option<(ClipboardContext, [u8; 32])>;
type ImageBaseline = Option<(ClipboardContext, u32, Option<[u8; 32]>)>;
struct ClipboardImageSample {
    sequence: u32,
    current_sequence: u32,
    image: Option<ClipboardImage>,
}

impl Runtime {
    fn clipboard_context(&self) -> Option<ClipboardContext> {
        self.clipboard_context_for(BulkKind::Text)
    }
    fn clipboard_context_for(&self, kind: BulkKind) -> Option<ClipboardContext> {
        (self.connected
            && self.bulk.is_some()
            && self.config.settings.enabled
            && match kind {
                BulkKind::Text => self.config.settings.share_clipboard,
                BulkKind::Image => self.config.settings.share_clipboard_images,
                BulkKind::File => false,
            })
        .then_some(ClipboardContext {
            generation: self.generation,
            bulk_generation: self.bulk_generation,
            revision: self.clipboard_revision,
            content_revision: self.clipboard_content_revision,
            kind,
        })
    }
}
struct BulkInstallation {
    // Ownership follows the installer if its original caller is cancelled.
    // Another dial cannot race the gap between spawn and state installation.
    dial_guard: OwnedMutexGuard<()>,
    expected_bulk_generation: u64,
    ready: oneshot::Sender<()>,
}
impl Drop for Incoming {
    fn drop(&mut self) {
        self.file.take();
        if !self.finished {
            if let Some(dir) = &self.directory {
                let _ = std::fs::remove_file(dir.join("receiving.part"));
                let _ = std::fs::remove_dir(dir);
            }
        }
    }
}

impl Engine {
    pub async fn start() -> Result<Self> {
        Self::start_at(
            default_config_dir(),
            default_receive_dir(),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), PORT),
        )
        .await
    }
    /// Custom paths and ephemeral ports let integration tests exercise two actual engines.
    pub async fn start_at(
        config_dir: PathBuf,
        receive_dir: PathBuf,
        address: SocketAddr,
    ) -> Result<Self> {
        let listener = TcpListener::bind(address)
            .await
            .context("Cannot listen: another instance may already be running")?;
        let diagnostics = DiagnosticLog::open(config_dir.join("logs"))?;
        let config = match Config::load(&config_dir) {
            Ok(config) => config,
            Err(error) => {
                diagnostics.record_failure(
                    EventCode::OperationFailed,
                    FailureReason::from_error(&error),
                );
                // Startup failure has no input hot path. Give the bounded
                // writer a brief chance to retain a safe failure category.
                diagnostics.flush_timeout(Duration::from_millis(250));
                return Err(error);
            }
        };
        let address = listener.local_addr()?;
        let port = address.port();
        let address_hints = tokio::task::spawn_blocking(move || local_addresses(port))
            .await
            .unwrap_or_default();
        let (events, mut event_rx) = mpsc::channel(2048);
        let platform = match InputController::start(events) {
            Ok(platform) => platform,
            Err(error) => {
                diagnostics.record_failure(
                    EventCode::InputInjectionFailed,
                    FailureReason::from_error(&error),
                );
                diagnostics.flush_timeout(Duration::from_millis(250));
                return Err(error);
            }
        };
        platform.configure(
            config.settings.enabled,
            config.settings.edge_switching,
            config.settings.peer_on_left,
        );
        let (stop, _) = watch::channel(false);
        let engine = Self(Arc::new(Inner {
            address_hints: Mutex::new(address_hints),
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
                events: vec![EventCode::EngineStarted.description().into()],
                pending: vec![],
                decisions: HashMap::new(),
                suppress_clipboard: None,
                suppress_clipboard_image: None,
            }),
            config_dir,
            receive_dir,
            platform,
            diagnostics,
            pipeline: Arc::new(Counters::default()),
            control_ready: Notify::new(),
            bulk_changed: Notify::new(),
            #[cfg(test)]
            bulk_waiting: Notify::new(),
            #[cfg(test)]
            bulk_install_waiting: Notify::new(),
            #[cfg(test)]
            bulk_retirement_waiting: Notify::new(),
            #[cfg(test)]
            input_sink: Mutex::new(None),
            #[cfg(test)]
            image_sink: Mutex::new(None),
            #[cfg(test)]
            text_sink: Mutex::new(None),
            incoming: AsyncMutex::new(None),
            outgoing: AsyncMutex::new(()),
            pairing: AsyncMutex::new(()),
            dialing: AsyncMutex::new(()),
            bulk_dialing: Arc::new(AsyncMutex::new(())),
            stop,
            address,
        }));
        engine.0.diagnostics.record(EventCode::EngineStarted);
        let e = engine.clone();
        tokio::spawn(async move {
            e.listen(listener).await;
        });
        // IPv6 is a separate listener: accept either family without any subnet comparison.
        if address.ip().is_unspecified() && address.is_ipv4() {
            if let Ok(listener) = TcpListener::bind(format!("[::]:{}", address.port())).await {
                let e = engine.clone();
                tokio::spawn(async move {
                    e.listen(listener).await;
                });
            }
        }
        let e = engine.clone();
        let mut stop = e.0.stop.subscribe();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _=stopped(&mut stop)=>break,
                    event=event_rx.recv()=>{
                        let Some(captured) = event else { break };
                        e.forward_input(captured);
                    }
                }
            }
        });
        let e = engine.clone();
        tokio::spawn(async move {
            e.clipboard_loop().await;
        });
        let e = engine.clone();
        tokio::spawn(async move {
            e.preferences_loop().await;
        });
        let e = engine.clone();
        tokio::spawn(async move {
            e.reconnect_loop().await;
        });
        let e = engine.clone();
        tokio::spawn(async move {
            e.address_hint_loop().await;
        });
        Ok(engine)
    }
    fn state(&self) -> MutexGuard<'_, Runtime> {
        self.0.state.lock().unwrap()
    }
    pub fn listen_address(&self) -> SocketAddr {
        self.0.address
    }
    pub fn status(&self) -> Status {
        let local_addresses = self.0.address_hints.lock().unwrap().clone();
        let s = self.state();
        let seconds = s
            .invitation
            .as_ref()
            .filter(|i| i.attempts < pairing::MAX_ATTEMPTS)
            .map(|i| {
                i.expires
                    .saturating_duration_since(Instant::now())
                    .as_secs()
            })
            .unwrap_or(0);
        Status {
            device_name: s.config.device_name.clone(),
            public_key: encode_key(&s.config.public_key),
            local_addresses,
            enabled: s.config.settings.enabled,
            connected: s.connected,
            remote_name: s
                .config
                .peer
                .as_ref()
                .filter(|_| s.connected)
                .map(|p| p.name.clone()),
            peer: s.config.peer.clone(),
            settings: s.config.settings.clone(),
            invitation: if seconds > 0 {
                s.invitation.as_ref().map(|i| i.code.display())
            } else {
                None
            },
            invitation_seconds_left: seconds,
            pairing_attempts_remaining: if seconds > 0 {
                s.invitation
                    .as_ref()
                    .map(|i| pairing::MAX_ATTEMPTS.saturating_sub(i.attempts))
                    .unwrap_or(0)
            } else {
                0
            },
            last_error: s.last_error.clone(),
            latency_ms: s.latency_ms,
            pending_files: s.pending.clone(),
            recent_events: s.events.clone(),
            receiving_directory: self.0.receive_dir.display().to_string(),
            native_input: cfg!(windows),
            diagnostics: self.0.diagnostics.status(),
            input_diagnostics: self.0.platform.diagnostics(),
            control_diagnostics: self.0.pipeline.snapshot(
                s.latency_updated
                    .map(|at| input_pipeline::millis(at.elapsed())),
            ),
        }
    }
    pub fn diagnostic_status(&self) -> DiagnosticStatus {
        self.0.diagnostics.status()
    }
    async fn refresh_address_hints(
        &self,
        lookup: impl FnOnce(u16) -> Vec<String> + Send + 'static,
    ) {
        let port = self.0.address.port();
        // Socket creation and route selection can call into the OS. They run
        // away from both Runtime and the short cache-copy mutex critical section.
        if let Ok(hints) = tokio::task::spawn_blocking(move || lookup(port)).await {
            *self.0.address_hints.lock().unwrap() = hints;
        }
    }
    async fn address_hint_loop(&self) {
        let period = Duration::from_secs(30);
        let mut tick = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut stop = self.0.stop.subscribe();
        loop {
            tokio::select! {
                _ = stopped(&mut stop) => break,
                _ = tick.tick() => {
                    tokio::select! {
                        _ = stopped(&mut stop) => break,
                        _ = self.refresh_address_hints(local_addresses) => {},
                    }
                },
            }
        }
    }
    /// The desktop may inspect/export startup logs even when no engine could
    /// be initialized. The frontend cannot choose this directory.
    pub fn diagnostic_directory() -> Result<PathBuf> {
        let config = default_config_dir();
        ensure!(
            config.is_absolute(),
            "The application data directory is unavailable"
        );
        Ok(config.join("logs"))
    }
    pub async fn export_diagnostics(&self) -> Result<PathBuf> {
        let summary = {
            let s = self.state();
            DiagnosticSummary {
                engine_available: true,
                native_input: cfg!(windows),
                sharing_enabled: s.config.settings.enabled,
                paired: s.config.peer.is_some(),
                connected: s.connected,
                share_clipboard: s.config.settings.share_clipboard,
                share_clipboard_images: s.config.settings.share_clipboard_images,
                keep_peer_awake: s.config.settings.keep_peer_awake,
                show_status_notifications: s.config.settings.show_status_notifications,
                share_files: s.config.settings.share_files,
                edge_switching: s.config.settings.edge_switching,
                peer_on_left: s.config.settings.peer_on_left,
                round_trip_ms: s.latency_ms,
                input: self.0.platform.diagnostics(),
                control_diagnostics: self.0.pipeline.snapshot(
                    s.latency_updated
                        .map(|at| input_pipeline::millis(at.elapsed())),
                ),
            }
        };
        self.0.diagnostics.export(summary).await
    }
    fn push_event(&self, code: EventCode) {
        let mut s = self.state();
        s.events.push(code.description().into());
        if s.events.len() > 32 {
            s.events.remove(0);
        }
    }
    fn event(&self, code: EventCode) {
        self.0.diagnostics.record(code);
        self.push_event(code);
    }
    fn error(&self, code: EventCode, error: &anyhow::Error) {
        // Detailed errors are local UI state only. Managed logs/export receive
        // allowlisted codes and typed categories, never this raw string.
        let message = format!("{error:#}");
        self.state().last_error = Some(message);
        self.0
            .diagnostics
            .record_failure(code, FailureReason::from_error(error));
        self.push_event(code);
    }
    fn control(&self, message: Message) -> Result<()> {
        let mut s = self.state();
        let result = s
            .control
            .as_ref()
            .context("Device is disconnected")?
            .push(message, Instant::now());
        if result.is_err() {
            self.disconnect_locked(&mut s, EventCode::InputQueueOverflow);
        }
        result
    }
    fn forward_input(&self, captured: CapturedInput) {
        let mut s = self.state();
        if !s.connected
            || !s.config.settings.enabled
            || captured.epoch != self.0.platform.connection_epoch()
        {
            return;
        }
        if s.control.as_ref().is_none_or(|tx| {
            tx.push(Message::Input(captured.event), captured.captured_at)
                .is_err()
        }) {
            self.disconnect_locked(&mut s, EventCode::InputQueueOverflow);
        }
    }
    fn bulk_session(&self) -> Result<BulkSession> {
        let s = self.state();
        Ok(BulkSession {
            tx: s
                .bulk
                .clone()
                .context("File/clipboard channel is not ready")?,
            generation: s.generation,
            bulk_generation: s.bulk_generation,
            clipboard_revision: None,
        })
    }
    fn clipboard_session(&self) -> Option<(ClipboardContext, BulkSession)> {
        self.clipboard_session_for(BulkKind::Text)
    }
    fn clipboard_session_for(&self, kind: BulkKind) -> Option<(ClipboardContext, BulkSession)> {
        let s = self.state();
        if *self.0.stop.borrow() {
            return None;
        }
        let context = s.clipboard_context_for(kind)?;
        Some((
            context,
            BulkSession {
                tx: s.bulk.as_ref()?.clone(),
                generation: context.generation,
                bulk_generation: context.bulk_generation,
                clipboard_revision: Some((context.revision, context.content_revision, kind)),
            },
        ))
    }
    async fn bulk(&self, session: &BulkSession, message: Message) -> Result<()> {
        // Waiting for capacity grants no authority. Reserve outside state, then
        // recheck at the nonblocking enqueue commit on this original sender.
        let permit = timeout(Duration::from_secs(6), session.tx.reserve())
            .await
            .context("Bulk queue timed out")??;
        {
            let s = self.state();
            ensure!(
                s.generation == session.generation
                    && s.bulk_generation == session.bulk_generation
                    && s.config.settings.enabled
                    && s.connected,
                "Transfer session changed; canceled"
            );
            if let Some((revision, content_revision, kind)) = session.clipboard_revision {
                ensure!(
                    !*self.0.stop.borrow()
                        && match kind {
                            BulkKind::Text => s.config.settings.share_clipboard,
                            BulkKind::Image => s.config.settings.share_clipboard_images,
                            BulkKind::File => false,
                        }
                        && s.clipboard_revision == revision,
                    "Clipboard activation changed; canceled"
                );
                if matches!(message, Message::Offer { .. }) {
                    ensure!(
                        s.clipboard_content_revision == content_revision,
                        "Clipboard changed before transfer offer; canceled"
                    );
                }
            }
            permit.send(message);
        }
        Ok(())
    }
    pub fn create_invitation(&self) -> Result<String> {
        let mut s = self.state();
        ensure!(s.config.settings.enabled, "Enable the engine first");
        ensure!(
            s.config.peer.is_none(),
            "Forget the existing peer before pairing another device"
        );
        let code = ManualCode::generate();
        let mut id = [0; 16];
        OsRng.fill_bytes(&mut id);
        let result = code.display();
        s.invitation = Some(Invitation {
            code,
            id,
            expires: Instant::now() + pairing::LIFETIME,
            attempts: 0,
        });
        drop(s);
        self.event(EventCode::PairingWindowOpened);
        Ok(result)
    }
    pub fn copy_invitation(&self) -> Result<()> {
        let invitation = self
            .status()
            .invitation
            .context("Create an invitation first")?;
        // This value must never be picked up by clipboard synchronization.
        self.state().suppress_clipboard = Some(Sha256::digest(invitation.as_bytes()).into());
        emc_platform::write_clipboard(&invitation)
    }
    pub fn update_settings(&self, settings: Settings) -> Result<Status> {
        let (settings, save_error, revoked) = {
            let mut s = self.state();
            let old = s.config.settings.clone();
            s.config.settings = settings.clone();
            let save_error = s.config.save(&self.0.config_dir).err();
            if save_error.is_some() {
                // A failed disk write cannot restore authority the user revoked.
                // New permissions and unrelated unsaved layout changes retain
                // their prior values; these reductions apply to this run.
                s.config.settings = Settings {
                    enabled: old.enabled && settings.enabled,
                    share_clipboard: old.share_clipboard && settings.share_clipboard,
                    share_clipboard_images: old.share_clipboard_images
                        && settings.share_clipboard_images,
                    share_files: old.share_files && settings.share_files,
                    edge_switching: old.edge_switching,
                    peer_on_left: old.peer_on_left,
                    keep_peer_awake: old.keep_peer_awake && settings.keep_peer_awake,
                    show_status_notifications: old.show_status_notifications
                        && settings.show_status_notifications,
                };
            }
            let applied = s.config.settings.clone();
            if old.show_status_notifications != applied.show_status_notifications {
                s.notification_revision = s.notification_revision.wrapping_add(1);
            }
            if old.enabled != applied.enabled
                || old.share_clipboard != applied.share_clipboard
                || old.share_clipboard_images != applied.share_clipboard_images
            {
                // This transition and its revision share one lock: even a fast
                // off/on between clipboard polls must establish a new baseline.
                s.clipboard_revision = s.clipboard_revision.wrapping_add(1);
            }
            if old.enabled != applied.enabled || old.keep_peer_awake != applied.keep_peer_awake {
                s.awake_revision = s.awake_revision.wrapping_add(1);
                s.peer_activity = None;
            }
            let revoke_bulk = (old.share_files && !applied.share_files)
                || (old.share_clipboard && !applied.share_clipboard)
                || (old.share_clipboard_images && !applied.share_clipboard_images);
            if revoke_bulk {
                s.bulk = None;
                s.bulk_generation = s.bulk_generation.wrapping_add(1);
                self.0.bulk_changed.notify_waiters();
                s.pending.clear();
                s.decisions.clear();
            }
            let revoked = (old.enabled && !applied.enabled)
                || revoke_bulk
                || (old.keep_peer_awake && !applied.keep_peer_awake)
                || (old.show_status_notifications && !applied.show_status_notifications);
            (applied, save_error, revoked)
        };
        self.0.platform.configure(
            settings.enabled,
            settings.edge_switching,
            settings.peer_on_left,
        );
        if !settings.enabled {
            self.state().invitation = None;
            self.disconnect(EventCode::SharingPaused);
        } else if save_error.is_none() {
            self.event(EventCode::SharingEnabled);
        }
        if let Some(error) = save_error {
            return Err(error.context(if revoked {
                "Sharing revocations are active for this run, but settings could not be saved; the last saved permissions may return after restarting"
            } else {
                "Settings could not be saved; no new permissions or layout changes were applied"
            }));
        }
        Ok(self.status())
    }
    pub fn emergency_stop(&self) -> Result<()> {
        let _ = self.control(Message::Input(InputEvent::ReleaseAll));
        let _ = self.control(Message::Input(InputEvent::Leave));
        self.0.platform.set_remote(false);
        self.0.platform.release_all();
        let mut settings = self.state().config.settings.clone();
        settings.enabled = false;
        let result = self.update_settings(settings);
        self.event(EventCode::EmergencyStop);
        result.map(|_| ())
    }
    pub async fn forget_peer(&self) -> Result<()> {
        let (owner, result) = {
            let mut s = self.state();
            s.config.peer = None;
            s.config.initiator = false;
            s.invitation = None;
            let result = s.config.save(&self.0.config_dir);
            let owner = s.bulk_generation;
            self.disconnect_locked(&mut s, EventCode::PeerForgotten);
            (owner, result)
        };
        let mut incoming = self.0.incoming.lock().await;
        if incoming.as_ref().is_some_and(|i| i.owner == owner) {
            incoming.take();
        }
        result?;
        Ok(())
    }
    fn disconnect(&self, reason: EventCode) {
        self.disconnect_locked(&mut self.state(), reason);
    }
    fn disconnect_locked(&self, s: &mut Runtime, code: EventCode) {
        self.0.platform.set_connected(false);
        self.0.platform.set_remote(false);
        self.0.platform.release_all();
        s.generation = s.generation.wrapping_add(1);
        s.bulk_generation = s.bulk_generation.wrapping_add(1);
        s.connected = false;
        if let Some(queue) = s.control.take() {
            queue.close();
        }
        if let Some(stop) = s.control_stop.take() {
            stop.send_replace(true);
        }
        self.0.control_ready.notify_waiters();
        self.0.bulk_changed.notify_waiters();
        s.bulk = None;
        s.latency_ms = None;
        s.latency_updated = None;
        s.pending.clear();
        s.decisions.clear();
        let description = code.description();
        self.0.diagnostics.record(code);
        if s.events.last().is_none_or(|x| x != description) {
            s.events.push(description.into());
            if s.events.len() > 32 {
                s.events.remove(0);
            }
        }
    }
    pub fn shutdown(&self) {
        if self.0.stop.send_replace(true) {
            return;
        }
        self.disconnect(EventCode::EngineStopped);
        // Tauri exits the process directly, so normal Quit cannot depend on a
        // background thread's Drop to drain records. No engine/native lock is
        // held here, and the maximum shutdown wait is bounded.
        self.0.diagnostics.flush_timeout(Duration::from_millis(250));
    }

    pub async fn pair_device(&self, address: String, invitation: String) -> Result<()> {
        let result = self.pair_device_inner(address, invitation).await;
        if let Err(error) = &result {
            self.error(EventCode::PairingRejected, error);
        }
        result
    }
    async fn pair_device_inner(&self, address: String, invitation: String) -> Result<()> {
        let _guard = self
            .0
            .pairing
            .try_lock()
            .context("Another pairing operation is in progress")?;
        let addr = parse_address(address.trim())?;
        // Legacy long tokens are accepted only when explicitly supplied. A
        // failed short-code exchange never falls back to the legacy protocol.
        let legacy = if invitation.trim().starts_with("emc1.") {
            Some(parse_invitation(invitation.trim())?)
        } else {
            None
        };
        let code = if legacy.is_none() {
            Some(ManualCode::parse(invitation.trim())?)
        } else {
            None
        };
        let (private, generation) = {
            let s = self.state();
            ensure!(s.config.peer.is_none(), "Forget the existing peer first");
            ensure!(
                s.config.settings.enabled && !*self.0.stop.borrow(),
                "Enable the engine first"
            );
            (Zeroizing::new(s.config.private_key.clone()), s.generation)
        };
        let io = timeout(Duration::from_secs(5), TcpStream::connect(addr))
            .await
            .context("Connection timed out; check routing, Tailscale and the peer firewall")??;
        let mut channel = if let Some((pin, secret)) = &legacy {
            transport::connect(io, transport::PAIR, &private, pin, Some(secret)).await?
        } else {
            transport::connect_with_code(io, &private, code.as_ref().expect("Parsed pairing code"))
                .await?
        };
        let device_name = self.state().config.device_name.clone();
        channel
            .writer
            .send(&Message::Hello { name: device_name })
            .await?;
        let name = read_hello(&mut channel).await?;
        self.remember(name, channel.remote_key, addr, true, generation, None)?;
        channel.writer.send(&Message::Ready).await?;
        ensure!(
            matches!(
                timeout(Duration::from_secs(5), channel.reader.recv()).await??,
                Message::Ready
            ),
            "Pairing confirmation missing"
        );
        drop(channel);
        self.event(EventCode::PairingSucceeded);
        self.reconnect().await
    }
    fn remember(
        &self,
        name: String,
        key: [u8; 32],
        address: SocketAddr,
        initiator: bool,
        generation: u64,
        invitation_id: Option<&[u8; 16]>,
    ) -> Result<()> {
        let mut s = self.state();
        ensure!(s.config.peer.is_none(), "A peer is already paired");
        ensure!(
            s.generation == generation && s.config.settings.enabled && !*self.0.stop.borrow(),
            "Pairing was canceled or disabled"
        );
        if let Some(invitation_id) = invitation_id {
            ensure!(
                s.invitation
                    .as_ref()
                    .is_some_and(|i| i.expires > Instant::now() && &i.id == invitation_id),
                "Invitation expired or replaced"
            );
        }
        s.config.peer = Some(Peer {
            name,
            public_key: encode_key(&key),
            address: address.to_string(),
        });
        s.config.initiator = initiator;
        if let Err(e) = s.config.save(&self.0.config_dir) {
            s.config.peer = None;
            s.config.initiator = false;
            return Err(e);
        }
        s.invitation = None;
        Ok(())
    }
    async fn listen(&self, listener: TcpListener) {
        let permits = Arc::new(Semaphore::new(8));
        let mut stop = self.0.stop.subscribe();
        loop {
            tokio::select! {
                _=stopped(&mut stop)=>break,
                accepted=listener.accept()=> {
                    let Ok((io,addr))=accepted else {break};let Ok(permit)=permits.clone().try_acquire_owned() else {continue};
                    let e=self.clone();tokio::spawn(async move {let _permit=permit;if let Err(err)=e.accept(io,addr).await {
                        let code = match err.downcast_ref::<EstablishedChannel>() {
                            Some(EstablishedChannel::Control) => EventCode::ControlChannelClosed,
                            Some(EstablishedChannel::Bulk) => EventCode::BulkChannelClosed,
                            None if FailureReason::from_error(&err) == FailureReason::PolicyPaused => EventCode::ConnectionAdmissionRejected,
                            None => EventCode::AuthenticationRejected,
                        };
                        e.error(code,&err);
                    } });
                }
            }
        }
    }
    async fn accept(&self, mut io: TcpStream, addr: SocketAddr) -> Result<()> {
        let kind = transport::preamble(&mut io).await?;
        ensure!(
            self.state().config.settings.enabled,
            FailureReason::PolicyPaused
        );
        if kind == transport::PAIR {
            bail!("Legacy pairing is unavailable on this device; update both applications and create a new short code");
        }
        if kind == transport::PAIR_PAKE {
            let _guard = self.0.pairing.try_lock().context("Pairing busy")?;
            let (private, code, invitation_id, generation) = {
                let mut s = self.state();
                ensure!(s.config.settings.enabled, FailureReason::PolicyPaused);
                ensure!(s.config.peer.is_none(), "Pairing closed");
                ensure!(!*self.0.stop.borrow(), "Pairing is stopped");
                let private = Zeroizing::new(s.config.private_key.clone());
                let generation = s.generation;
                let inv = s
                    .invitation
                    .as_mut()
                    .filter(|i| i.expires > Instant::now())
                    .context("Invitation expired or unavailable")?;
                ensure!(
                    inv.attempts < pairing::MAX_ATTEMPTS,
                    "Pairing attempt limit reached; create a new code"
                );
                // Reserve a bounded attempt before doing elliptic-curve work or
                // replying. A failed/abandoned exchange also consumes one.
                inv.attempts += 1;
                (private, inv.code.clone(), inv.id, generation)
            };
            let mut channel = transport::accept_with_code(io, &private, &code).await?;
            // Recheck expiration after the bounded handshake, before persisting trust.
            ensure!(
                self.state()
                    .invitation
                    .as_ref()
                    .is_some_and(|i| i.expires > Instant::now() && i.id == invitation_id),
                "Invitation expired or replaced"
            );
            let name = read_hello(&mut channel).await?;
            let device_name = self.state().config.device_name.clone();
            channel
                .writer
                .send(&Message::Hello { name: device_name })
                .await?;
            ensure!(
                matches!(
                    timeout(Duration::from_secs(5), channel.reader.recv()).await??,
                    Message::Ready
                ),
                "Pairing confirmation missing"
            );
            self.remember(
                name,
                channel.remote_key,
                SocketAddr::new(addr.ip(), PORT),
                false,
                generation,
                Some(&invitation_id),
            )?;
            channel.writer.send(&Message::Ready).await?;
            self.event(EventCode::PairingSucceeded);
            Ok(())
        } else {
            // A peer can finish its IK control handshake and immediately open
            // bulk before this task has installed control. Wait on installation
            // before taking the generation snapshot used by bulk authentication.
            if kind == transport::BULK {
                #[cfg(test)]
                self.0.bulk_waiting.notify_one();
                self.wait_control_ready().await?;
            }
            let (private, pin, generation) = {
                let s = self.state();
                ensure!(s.config.settings.enabled, FailureReason::PolicyPaused);
                let peer = s.config.peer.as_ref().context("Untrusted device")?;
                (
                    Zeroizing::new(s.config.private_key.clone()),
                    decode_key(&peer.public_key)?,
                    s.generation,
                )
            };
            let channel = transport::accept(io, kind, &private, Some(&pin), None).await?;
            let phase = if kind == transport::CONTROL {
                EstablishedChannel::Control
            } else {
                EstablishedChannel::Bulk
            };
            {
                let s = self.state();
                if !(s.generation == generation
                    && s.config
                        .peer
                        .as_ref()
                        .is_some_and(|p| p.public_key == encode_key(&pin))
                    && s.config.settings.enabled)
                {
                    return Err(anyhow::anyhow!(FailureReason::SessionRevoked).context(phase));
                }
            }
            if kind == transport::CONTROL {
                self.run_control(channel, generation).await.context(phase)
            } else {
                self.run_bulk(channel, generation).await.context(phase)
            }
        }
    }
    pub async fn reconnect(&self) -> Result<()> {
        let _guard = self
            .0
            .dialing
            .try_lock()
            .context("Connection attempt already in progress")?;
        let (private, pin, addr, generation) = {
            let s = self.state();
            ensure!(!*self.0.stop.borrow(), FailureReason::SessionRevoked);
            ensure!(s.config.settings.enabled, FailureReason::PolicyPaused);
            ensure!(!s.connected, "Already connected");
            let peer = s.config.peer.as_ref().context("Pair a device first")?;
            ensure!(
                s.config.initiator,
                "Reconnect from the device that entered the invitation"
            );
            (
                Zeroizing::new(s.config.private_key.clone()),
                decode_key(&peer.public_key)?,
                parse_address(&peer.address)?,
                s.generation,
            )
        };
        let io = timeout(Duration::from_secs(4), TcpStream::connect(addr))
            .await
            .context("Connection timed out; verify route and firewall")??;
        let channel = transport::connect(io, transport::CONTROL, &private, &pin, None).await?;
        {
            let s = self.state();
            ensure!(
                !*self.0.stop.borrow()
                    && s.generation == generation
                    && s.config
                        .peer
                        .as_ref()
                        .is_some_and(|peer| peer.public_key == encode_key(&pin)),
                FailureReason::SessionRevoked
            );
            ensure!(s.config.settings.enabled, FailureReason::PolicyPaused);
        }
        let e = self.clone();
        tokio::spawn(async move {
            if let Err(err) = e.run_control(channel, generation).await {
                e.error(EventCode::ControlChannelClosed, &err);
            }
        });
        // This attempt may only continue through the first installation that
        // follows its captured generation, never a replacement after revocation.
        let installed_generation = self
            .wait_control_installation(generation, &encode_key(&pin))
            .await?;
        self.connect_bulk_for(installed_generation, encode_key(&pin))
            .await
    }
    async fn connect_bulk(&self) -> Result<()> {
        let (generation, peer_key) = {
            let s = self.state();
            (
                s.generation,
                s.config
                    .peer
                    .as_ref()
                    .context("No peer")?
                    .public_key
                    .clone(),
            )
        };
        self.connect_bulk_for(generation, peer_key).await
    }
    async fn connect_bulk_for(
        &self,
        expected_generation: u64,
        expected_peer: String,
    ) -> Result<()> {
        let (generation, previous_bulk, peer_key, mut cancelled) = {
            let s = self.state();
            ensure!(!*self.0.stop.borrow(), FailureReason::SessionRevoked);
            ensure!(s.config.settings.enabled, FailureReason::PolicyPaused);
            ensure!(
                s.connected
                    && s.generation == expected_generation
                    && s.config
                        .peer
                        .as_ref()
                        .is_some_and(|peer| peer.public_key == expected_peer),
                FailureReason::SessionRevoked
            );
            if s.bulk.is_some() {
                return Ok(());
            }
            let peer = s.config.peer.as_ref().context("No peer")?;
            let cancelled = s
                .control_stop
                .as_ref()
                .context("Input channel is unavailable")?
                .subscribe();
            (
                s.generation,
                s.bulk_generation,
                peer.public_key.clone(),
                cancelled,
            )
        };
        let mut stop = self.0.stop.subscribe();
        let dial_guard = tokio::select! {
            biased;
            _ = stopped(&mut stop) => bail!(FailureReason::SessionRevoked),
            _ = stopped(&mut cancelled) => bail!(FailureReason::SessionRevoked),
            guard = self.0.bulk_dialing.clone().lock_owned() => guard,
        };
        let (private, pin, addr) = {
            let s = self.state();
            ensure!(!*self.0.stop.borrow(), FailureReason::SessionRevoked);
            ensure!(s.config.settings.enabled, FailureReason::PolicyPaused);
            ensure!(
                s.connected
                    && s.generation == generation
                    && s.config
                        .peer
                        .as_ref()
                        .is_some_and(|peer| peer.public_key == peer_key),
                FailureReason::SessionRevoked
            );
            if s.bulk.is_some() {
                // A waiter may join only the installation that followed its
                // original snapshot, never a later replacement bulk session.
                ensure!(
                    s.bulk_generation == previous_bulk.wrapping_add(1),
                    FailureReason::SessionRevoked
                );
                return Ok(());
            }
            ensure!(
                s.bulk_generation == previous_bulk,
                FailureReason::SessionRevoked
            );
            let peer = s.config.peer.as_ref().context("No peer")?;
            (
                Zeroizing::new(s.config.private_key.clone()),
                decode_key(&peer.public_key)?,
                parse_address(&peer.address)?,
            )
        };
        let attempt = async {
            let io = timeout(Duration::from_secs(4), TcpStream::connect(addr)).await??;
            let channel = transport::connect(io, transport::BULK, &private, &pin, None).await?;
            // Authentication completing does not grant a revoked caller the
            // right to start a task in a different control session.
            self.session_current(generation)?;
            let (ready, installed) = oneshot::channel();
            let installation = BulkInstallation {
                dial_guard,
                expected_bulk_generation: previous_bulk,
                ready,
            };
            let e = self.clone();
            tokio::spawn(async move {
                if let Err(err) = e
                    .run_bulk_install(channel, generation, Some(installation))
                    .await
                {
                    e.error(EventCode::BulkChannelClosed, &err);
                }
            });
            installed
                .await
                .map_err(|_| anyhow::anyhow!(FailureReason::SessionRevoked))?;
            let s = self.state();
            ensure!(
                !*self.0.stop.borrow()
                    && s.connected
                    && s.generation == generation
                    && s.bulk.is_some()
                    && s.bulk_generation == previous_bulk.wrapping_add(1)
                    && s.config
                        .peer
                        .as_ref()
                        .is_some_and(|peer| peer.public_key == peer_key),
                FailureReason::SessionRevoked
            );
            ensure!(s.config.settings.enabled, FailureReason::PolicyPaused);
            Ok(())
        };
        tokio::select! {
            biased;
            _ = stopped(&mut stop) => Err(anyhow::anyhow!(FailureReason::SessionRevoked)),
            _ = stopped(&mut cancelled) => Err(anyhow::anyhow!(FailureReason::SessionRevoked)),
            result = attempt => result,
        }
    }
    async fn reconnect_loop(&self) {
        let mut tick = interval(Duration::from_secs(3));
        let mut stop = self.0.stop.subscribe();
        loop {
            tokio::select! {_=stopped(&mut stop)=>break,_=tick.tick()=>{
                let (dial,connected,bulk)={let s=self.state();(s.config.settings.enabled && s.config.initiator && s.config.peer.is_some(),s.connected,s.bulk.is_some())};
                if dial {let result=if !connected {self.reconnect().await} else if !bulk {self.connect_bulk().await} else {Ok(())};if let Err(e)=result {self.error(EventCode::ReconnectFailed,&e);}}
            }}
        }
    }

    async fn apply_native(&self, event: InputEvent, epoch: u64, deadline: Instant) -> Result<()> {
        #[cfg(test)]
        {
            let sink = self.0.input_sink.lock().unwrap().clone();
            if let Some(sink) = sink {
                return sink(event, epoch).await;
            }
        }
        self.0
            .platform
            .apply_remote_for_epoch_before_async(event, epoch, deadline)
            .await
    }

    fn session_current(&self, generation: u64) -> Result<()> {
        let s = self.state();
        ensure!(
            s.generation == generation && s.connected,
            FailureReason::SessionRevoked
        );
        ensure!(s.config.settings.enabled, FailureReason::PolicyPaused);
        Ok(())
    }

    async fn wait_control_ready(&self) -> Result<()> {
        timeout(Duration::from_secs(2), async {
            loop {
                // Register before checking state so installation cannot be missed.
                let ready = self.0.control_ready.notified();
                tokio::pin!(ready);
                ready.as_mut().enable();
                {
                    let s = self.state();
                    ensure!(!*self.0.stop.borrow(), FailureReason::SessionRevoked);
                    ensure!(s.config.settings.enabled, FailureReason::PolicyPaused);
                    ensure!(s.config.peer.is_some(), FailureReason::SessionRevoked);
                    if s.connected {
                        return Ok(());
                    }
                }
                ready.await;
            }
        })
        .await
        .context("Input channel readiness timed out")?
    }
    async fn wait_control_installation(
        &self,
        previous_generation: u64,
        peer_key: &str,
    ) -> Result<u64> {
        timeout(Duration::from_secs(2), async {
            loop {
                let ready = self.0.control_ready.notified();
                tokio::pin!(ready);
                ready.as_mut().enable();
                {
                    let s = self.state();
                    ensure!(!*self.0.stop.borrow(), FailureReason::SessionRevoked);
                    ensure!(s.config.settings.enabled, FailureReason::PolicyPaused);
                    ensure!(
                        s.config
                            .peer
                            .as_ref()
                            .is_some_and(|peer| peer.public_key == peer_key),
                        FailureReason::SessionRevoked
                    );
                    if s.connected {
                        ensure!(
                            s.generation == previous_generation.wrapping_add(1),
                            FailureReason::SessionRevoked
                        );
                        return Ok(s.generation);
                    }
                    ensure!(
                        s.generation == previous_generation,
                        FailureReason::SessionRevoked
                    );
                }
                ready.await;
            }
        })
        .await
        .context("Input channel installation timed out")?
    }

    async fn run_control(&self, channel: Channel, expected_generation: u64) -> Result<()> {
        let outgoing = OrderedQueue::new(Direction::Outgoing, self.0.pipeline.clone());
        let native = OrderedQueue::new(Direction::Incoming, self.0.pipeline.clone());
        let (session_stop, mut session_cancelled) = watch::channel(false);
        let (generation, input_epoch) = {
            let mut s = self.state();
            ensure!(
                s.generation == expected_generation
                    && !*self.0.stop.borrow()
                    && s.config
                        .peer
                        .as_ref()
                        .is_some_and(|p| p.public_key == encode_key(&channel.remote_key)),
                FailureReason::SessionRevoked
            );
            ensure!(s.config.settings.enabled, FailureReason::PolicyPaused);
            ensure!(!s.connected, FailureReason::DuplicateSession);
            s.generation = s.generation.wrapping_add(1);
            s.connected = true;
            s.control = Some(outgoing.clone());
            s.control_stop = Some(session_stop);
            s.last_error = None;
            s.latency_ms = None;
            s.latency_updated = None;
            self.0.platform.set_connected(true);
            (s.generation, self.0.platform.connection_epoch())
        };
        self.0.control_ready.notify_waiters();
        self.event(EventCode::PeerConnected);
        let start = Instant::now();
        let last_received = Arc::new(AtomicU64::new(0));
        let incoming = OrderedQueue::new(Direction::Incoming, self.0.pipeline.clone());
        let messages = incoming.clone();
        let mut reader = channel.reader;
        let seen = last_received.clone();
        let e = self.clone();
        let mut reader_task = tokio::spawn(async move {
            loop {
                let value = reader.recv().await?;
                let queued_at = Instant::now();
                // Only freshly authenticated network records refresh liveness;
                // processing an old queued event cannot extend the watchdog.
                seen.store(input_pipeline::millis(start.elapsed()), Ordering::Release);
                if matches!(&value, Message::PeerActivity) {
                    e.record_peer_activity(generation, queued_at);
                }
                messages.push(value, queued_at)?;
            }
            #[allow(unreachable_code)]
            Ok::<(), anyhow::Error>(())
        });
        let e = self.clone();
        let queue = outgoing.clone();
        let mut writer = channel.writer;
        let mut writer_task = tokio::spawn(async move {
            while let Some(message) = queue.recv().await {
                e.session_current(generation)?;
                if matches!(message.value, Message::PeerActivity)
                    && message.queued_at.elapsed() >= input_pipeline::MAX_INPUT_AGE
                {
                    // Optional power activity may expire before a write starts.
                    // Never disconnect working input merely to renew that lease.
                    continue;
                }
                let deadline =
                    e.0.pipeline
                        .input_deadline(message.queued_at, StaleStage::OutgoingQueue)?;
                e.0.pipeline
                    .complete_before(
                        deadline,
                        StaleStage::OutgoingWrite,
                        writer.send(&message.value),
                    )
                    .await?;
            }
            Ok::<(), anyhow::Error>(())
        });
        let e = self.clone();
        let queue = native.clone();
        let mut injector_task = tokio::spawn(async move {
            while let Some(event) = queue.recv().await {
                e.session_current(generation)?;
                ensure!(
                    e.0.platform.connection_epoch() == input_epoch,
                    FailureReason::SessionRevoked
                );
                let deadline =
                    e.0.pipeline
                        .input_deadline(event.queued_at, StaleStage::IncomingQueue)?;
                let entering = matches!(&event.value, InputEvent::Enter { .. });
                let began = Instant::now();
                let result =
                    e.0.pipeline
                        .complete_before(
                            deadline,
                            StaleStage::NativeCompletion,
                            e.apply_native(event.value, input_epoch, deadline),
                        )
                        .await;
                e.0.pipeline.native_completed(began.elapsed());
                if let Err(error) = result {
                    if FailureReason::from_error(&error) != FailureReason::StaleInput {
                        e.error(
                            if entering {
                                EventCode::InputEntryFailed
                            } else {
                                EventCode::InputInjectionFailed
                            },
                            &error,
                        );
                    }
                    return Err(error);
                }
            }
            Ok::<(), anyhow::Error>(())
        });
        let mut stop = self.0.stop.subscribe();
        let mut tick = interval(Duration::from_secs(1));
        let mut pending_ping = None;
        let result = async {
            loop {
                tokio::select! {
                    biased;
                    _ = stopped(&mut stop) => break,
                    _ = stopped(&mut session_cancelled) => break,
                    result = &mut reader_task => { result.context("Input reader stopped")??; break; },
                    result = &mut writer_task => { result.context("Input writer stopped")??; break; },
                    result = &mut injector_task => { result.context("Native input worker stopped")??; break; },
                    _ = tick.tick() => {
                        self.session_current(generation)?;
                        let last = last_received.load(Ordering::Acquire);
                        if input_pipeline::millis(start.elapsed()).saturating_sub(last) >= 4_000 {
                            self.event(EventCode::HeartbeatTimeout);
                            bail!(FailureReason::TimedOut);
                        }
                        if pending_ping.is_none() {
                            let n = input_pipeline::millis(start.elapsed());
                            pending_ping = Some(n);
                            outgoing.push(Message::Ping(n), Instant::now())?;
                        }
                    },
                    message = incoming.recv() => {
                        let Timed { value, queued_at } = message.context("Input reader closed")?;
                        self.session_current(generation)?;
                        match value {
                            Message::Input(event) => native.push(event, queued_at)?,
                            Message::Ping(n) => outgoing.push(Message::Pong(n), Instant::now())?,
                            Message::Pong(n) => {
                                ensure!(pending_ping == Some(n), "Invalid heartbeat response");
                                pending_ping = None;
                                let mut s = self.state();
                                s.latency_ms = Some(input_pipeline::millis(start.elapsed()).saturating_sub(n));
                                s.latency_updated = Some(Instant::now());
                            },
                            Message::Decision {id,accept} => { if let Some(sender) = self.state().decisions.remove(&id) {let _=sender.send(accept);} },
                            Message::Received {id} => { if let Some(sender) = self.state().decisions.remove(&id) {let _=sender.send(true);} },
                            Message::PeerActivity => {},
                            _ => bail!("Unexpected message on input channel"),
                        }
                    }
                }
            }
            Ok::<_, anyhow::Error>(())
        }.await;
        // Revoke native epoch before aborting workers: already queued platform
        // commands also fail their commit-time epoch check after this point.
        let owner = {
            let mut s = self.state();
            if s.generation == generation {
                let owner = s.bulk_generation;
                self.disconnect_locked(&mut s, EventCode::PeerDisconnected);
                Some(owner)
            } else {
                None
            }
        };
        outgoing.close();
        native.close();
        incoming.close();
        reader_task.abort();
        writer_task.abort();
        injector_task.abort();
        if result
            .as_ref()
            .is_err_and(|error| FailureReason::from_error(error) == FailureReason::StaleInput)
        {
            self.event(EventCode::InputDispatchStale);
        }
        if result
            .as_ref()
            .is_err_and(|error| FailureReason::from_error(error) == FailureReason::Backlog)
        {
            self.event(EventCode::InputQueueOverflow);
        }
        if let Some(owner) = owner {
            let mut incoming = self.0.incoming.lock().await;
            if incoming.as_ref().is_some_and(|i| i.owner == owner) {
                incoming.take();
            }
        }
        result
    }

    async fn run_bulk(&self, channel: Channel, expected_generation: u64) -> Result<()> {
        self.run_bulk_install(channel, expected_generation, None)
            .await
    }
    async fn wait_bulk_vacant(
        &self,
        expected_generation: u64,
        previous_bulk: u64,
        peer_key: &str,
    ) -> Result<()> {
        timeout(Duration::from_secs(2), async {
            loop {
                let changed = self.0.bulk_changed.notified();
                tokio::pin!(changed);
                changed.as_mut().enable();
                {
                    let s = self.state();
                    ensure!(
                        !*self.0.stop.borrow()
                            && s.connected
                            && s.generation == expected_generation
                            && s.bulk_generation == previous_bulk
                            && s.config
                                .peer
                                .as_ref()
                                .is_some_and(|peer| peer.public_key == peer_key),
                        FailureReason::SessionRevoked
                    );
                    ensure!(s.config.settings.enabled, FailureReason::PolicyPaused);
                    if s.bulk.is_none() {
                        return Ok(());
                    }
                }
                #[cfg(test)]
                self.0.bulk_retirement_waiting.notify_one();
                changed.await;
            }
        })
        .await
        .map_err(|_| anyhow::anyhow!(FailureReason::DuplicateSession))?
    }
    async fn run_bulk_install(
        &self,
        channel: Channel,
        expected_generation: u64,
        installation: Option<BulkInstallation>,
    ) -> Result<()> {
        #[cfg(test)]
        if installation.is_some() {
            self.0.bulk_install_waiting.notify_one();
        }
        let (tx, mut rx) = mpsc::channel(8);
        let (mut cancelled, previous_bulk) = {
            let s = self.state();
            ensure!(
                s.generation == expected_generation && s.connected && !*self.0.stop.borrow(),
                FailureReason::SessionRevoked
            );
            ensure!(s.config.settings.enabled, FailureReason::PolicyPaused);
            if let Some(installation) = &installation {
                ensure!(
                    s.bulk_generation == installation.expected_bulk_generation,
                    FailureReason::SessionRevoked
                );
            }
            (
                s.control_stop
                    .as_ref()
                    .context("Input channel is unavailable")?
                    .subscribe(),
                s.bulk_generation,
            )
        };
        let mut stop = self.0.stop.subscribe();
        // An authenticated redial can arrive before the peer's old EOF task
        // retires. Wait boundedly for natural cleanup; never replace an active
        // transfer or carry this attempt through a changed bulk generation.
        let remote_key = encode_key(&channel.remote_key);
        tokio::select! {
            biased;
            _ = stopped(&mut stop) => bail!(FailureReason::SessionRevoked),
            _ = stopped(&mut cancelled) => bail!(FailureReason::SessionRevoked),
            result = self.wait_bulk_vacant(expected_generation, previous_bulk, &remote_key) => result?,
        }
        let mut guard = tokio::select! {
            biased;
            _ = stopped(&mut stop) => bail!(FailureReason::SessionRevoked),
            _ = stopped(&mut cancelled) => bail!(FailureReason::SessionRevoked),
            guard = self.0.incoming.lock() => guard,
        };
        let (generation, bulk_generation) = {
            let mut s = self.state();
            ensure!(
                s.generation == expected_generation
                    && !*self.0.stop.borrow()
                    && s.config
                        .peer
                        .as_ref()
                        .is_some_and(|p| p.public_key == encode_key(&channel.remote_key)),
                FailureReason::SessionRevoked
            );
            ensure!(s.connected, FailureReason::SessionRevoked);
            ensure!(s.config.settings.enabled, FailureReason::PolicyPaused);
            ensure!(
                s.bulk_generation == previous_bulk,
                FailureReason::SessionRevoked
            );
            ensure!(s.bulk.is_none(), FailureReason::DuplicateSession);
            s.bulk_generation = s.bulk_generation.wrapping_add(1);
            s.bulk = Some(tx);
            self.0.bulk_changed.notify_waiters();
            s.pending.clear();
            (s.generation, s.bulk_generation)
        };
        guard.take();
        drop(guard);
        if let Some(BulkInstallation {
            dial_guard, ready, ..
        }) = installation
        {
            let _ = ready.send(());
            drop(dial_guard);
        }
        let (mut incoming, reader_task) = spawn_reader(channel.reader);
        let mut writer = channel.writer;
        let mut tick = interval(Duration::from_secs(1));
        let result=async {loop {tokio::select! {
            _=stopped(&mut stop)=>break,
            outgoing=rx.recv()=>{ensure!(self.state().bulk_generation==bulk_generation,"Bulk session superseded");let Some(message)=outgoing else {break};writer.send(&message).await?;},
            message=incoming.recv()=>{
                ensure!(self.state().generation==generation && self.state().bulk_generation==bulk_generation && self.state().connected && self.state().bulk.is_some(),"Bulk session revoked");
                self.receive_bulk(message.context("Bulk channel disconnected")??,generation,bulk_generation).await?;
            },
            _=tick.tick()=>{
                ensure!(self.state().generation==generation && self.state().bulk_generation==bulk_generation && self.state().connected && self.state().bulk.is_some(),"Bulk session revoked");
                let expired=self.0.incoming.lock().await.as_ref().is_some_and(|x|x.created.elapsed()>Duration::from_secs(120));
                if expired {self.0.incoming.lock().await.take();self.state().pending.clear();}
            }
        }}Ok::<_,anyhow::Error>(())}.await;
        reader_task.abort();
        {
            let mut s = self.state();
            if s.generation == generation && s.bulk_generation == bulk_generation {
                s.bulk = None;
                self.0.bulk_changed.notify_waiters();
                s.pending.clear();
                s.decisions.clear();
            }
        }
        let mut guard = self.0.incoming.lock().await;
        if guard.as_ref().is_some_and(|i| i.owner == bulk_generation) {
            guard.take();
        }
        result
    }
    async fn receive_bulk(
        &self,
        message: Message,
        generation: u64,
        bulk_generation: u64,
    ) -> Result<()> {
        let valid = |s: &Runtime| {
            s.generation == generation
                && s.bulk_generation == bulk_generation
                && s.connected
                && s.config.settings.enabled
        };
        match message {
            Message::Offer {
                id,
                name,
                size,
                kind,
            } => {
                validate_id(&id)?;
                let mut incoming = self.0.incoming.lock().await;
                let allowed = {
                    let s = self.state();
                    valid(&s)
                        && match kind {
                            BulkKind::File => {
                                s.config.settings.share_files
                                    && size < MAX_FILE_BYTES
                                    && validate_filename(&name).is_ok()
                            }
                            BulkKind::Text => {
                                s.config.settings.share_clipboard
                                    && size <= MAX_CLIPBOARD_BYTES as u64
                                    && name.is_empty()
                            }
                            BulkKind::Image => {
                                s.config.settings.share_clipboard_images
                                    && size > 0
                                    && size <= MAX_CLIPBOARD_IMAGE_BYTES as u64
                                    && name.is_empty()
                            }
                        }
                };
                if !allowed || incoming.is_some() {
                    self.control(Message::Decision { id, accept: false })?;
                    return Ok(());
                }
                let offer = FileOffer {
                    id: id.clone(),
                    name,
                    size,
                };
                *incoming = Some(Incoming {
                    offer: offer.clone(),
                    kind,
                    accepted: kind != BulkKind::File,
                    received: 0,
                    hash: Sha256::new(),
                    text: vec![],
                    file: None,
                    directory: None,
                    finished: false,
                    created: Instant::now(),
                    owner: bulk_generation,
                });
                if kind != BulkKind::File {
                    self.control(Message::Decision { id, accept: true })?;
                } else {
                    self.state().pending = vec![offer];
                    self.event(EventCode::FileOffered);
                }
            }
            Message::Chunk { id, data } => {
                ensure!(data.len() <= CHUNK_BYTES, "Oversized transfer chunk");
                let mut guard = self.0.incoming.lock().await;
                let current = guard.as_mut().context("Unexpected transfer data")?;
                ensure!(
                    current.owner == bulk_generation && current.offer.id == id && current.accepted,
                    "Unaccepted transfer data"
                );
                let allowed = {
                    let s = self.state();
                    valid(&s)
                        && match current.kind {
                            BulkKind::File => s.config.settings.share_files,
                            BulkKind::Text => s.config.settings.share_clipboard,
                            BulkKind::Image => s.config.settings.share_clipboard_images,
                        }
                };
                ensure!(allowed, "Transfer sharing was disabled");
                ensure!(
                    current.received + data.len() as u64 <= current.offer.size,
                    "Transfer exceeds declared length"
                );
                if let Some(file) = &mut current.file {
                    file.write_all(&data).await?;
                } else {
                    current.text.extend_from_slice(&data);
                }
                current.hash.update(&data);
                current.received += data.len() as u64;
            }
            Message::Complete { id, sha256 } => {
                let mut guard = self.0.incoming.lock().await;
                let current = guard.as_mut().context("Unexpected transfer completion")?;
                ensure!(
                    valid(&self.state())
                        && current.owner == bulk_generation
                        && current.offer.id == id
                        && current.accepted
                        && current.received == current.offer.size,
                    "Incomplete transfer"
                );
                let digest: [u8; 32] = current.hash.clone().finalize().into();
                ensure!(digest == sha256, "Transfer integrity check failed");
                if current.kind == BulkKind::Image {
                    ensure!(
                        self.state().config.settings.share_clipboard_images,
                        "Image clipboard sharing disabled"
                    );
                    let png = std::mem::take(&mut current.text);
                    let cache = self.0.config_dir.join("clipboard-images");
                    let prepared = tokio::task::spawn_blocking(move || {
                        let image = ClipboardImage::from_png(&png)?;
                        emc_platform::clipboard_image::prepare_image(image, &cache)
                    })
                    .await??;
                    self.apply_clipboard_image(prepared, generation, bulk_generation)
                        .await?;
                    self.event(EventCode::ClipboardImageApplied);
                    self.clipboard_notice(ClipboardNotice::ImageReceived, generation);
                } else if current.kind == BulkKind::Text {
                    ensure!(
                        self.state().config.settings.share_clipboard,
                        "Clipboard sharing disabled"
                    );
                    let text = String::from_utf8(std::mem::take(&mut current.text))?;
                    // Invitation-shaped content is deliberately excluded in both directions.
                    ensure!(
                        !pairing::looks_like_credential(&text),
                        "Pairing invitations cannot be shared through clipboard sync"
                    );
                    let e = self.clone();
                    tokio::task::spawn_blocking(move || {
                        let mut s = e.state();
                        ensure!(
                            s.generation == generation
                                && s.bulk_generation == bulk_generation
                                && s.config.settings.enabled
                                && s.config.settings.share_clipboard,
                            "Clipboard transfer revoked"
                        );
                        s.suppress_clipboard = Some(digest);
                        #[cfg(test)]
                        let sink = e.0.text_sink.lock().unwrap().clone();
                        #[cfg(test)]
                        if let Some(sink) = sink {
                            sink(&text)?;
                        } else {
                            emc_platform::write_clipboard(&text)?;
                        }
                        #[cfg(not(test))]
                        emc_platform::write_clipboard(&text)?;
                        s.clipboard_content_revision = s.clipboard_content_revision.wrapping_add(1);
                        Ok::<_, anyhow::Error>(())
                    })
                    .await??;
                    self.event(EventCode::ClipboardApplied);
                    self.clipboard_notice(ClipboardNotice::TextReceived, generation);
                } else {
                    ensure!(
                        self.state().config.settings.share_files,
                        "File sharing disabled"
                    );
                    if let Some(mut file) = current.file.take() {
                        file.flush().await?;
                        file.sync_all().await?;
                    }
                    let dir = current
                        .directory
                        .as_ref()
                        .context("Missing transfer directory")?;
                    let temporary = dir.join("receiving.part");
                    let destination = dir.join(&current.offer.name);
                    let e = self.clone();
                    tokio::task::spawn_blocking(move || {
                        let s = e.state();
                        ensure!(
                            s.generation == generation
                                && s.bulk_generation == bulk_generation
                                && s.config.settings.enabled
                                && s.config.settings.share_files,
                            "File transfer revoked"
                        );
                        mark_download(&temporary)?;
                        std::fs::rename(temporary, destination)?;
                        Ok::<_, anyhow::Error>(())
                    })
                    .await??;
                    self.event(EventCode::FileReceived);
                }
                current.finished = true;
                guard.take();
                self.state().pending.clear();
                self.control(Message::Received { id })?;
            }
            Message::Cancel { id } => {
                let mut guard = self.0.incoming.lock().await;
                if guard.as_ref().is_some_and(|x| x.offer.id == id) {
                    guard.take();
                    self.state().pending.clear();
                }
            }
            _ => bail!("Unexpected message on bulk channel"),
        }
        Ok(())
    }
    pub async fn respond_file(&self, id: String, accept: bool) -> Result<()> {
        let result = self.respond_file_inner(id, accept).await;
        match &result {
            Ok(()) => self.event(if accept {
                EventCode::FileAccepted
            } else {
                EventCode::FileDeclined
            }),
            Err(error) => self.error(EventCode::FileTransferFailed, error),
        }
        result
    }
    async fn respond_file_inner(&self, id: String, accept: bool) -> Result<()> {
        let mut guard = self.0.incoming.lock().await;
        let current = guard.as_mut().context("No pending transfer")?;
        ensure!(
            current.offer.id == id && current.kind == BulkKind::File && !current.accepted,
            "No matching pending file"
        );
        if !accept {
            guard.take();
            self.state().pending.clear();
            self.control(Message::Decision { id, accept: false })?;
            return Ok(());
        }
        {
            let s = self.state();
            ensure!(
                s.config.settings.enabled
                    && s.config.settings.share_files
                    && s.connected
                    && s.bulk_generation == current.owner,
                "File sharing is disabled or disconnected"
            );
        }
        tokio::fs::create_dir_all(&self.0.receive_dir).await?;
        let dir = self.0.receive_dir.join(&id);
        tokio::fs::create_dir(&dir).await?;
        current.directory = Some(dir.clone());
        current.file = Some(
            tokio::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(dir.join("receiving.part"))
                .await?,
        );
        current.accepted = true;
        self.state().pending.clear();
        self.control(Message::Decision { id, accept: true })?;
        Ok(())
    }
    async fn offer(
        &self,
        name: String,
        size: u64,
        kind: BulkKind,
    ) -> Result<(String, BulkSession)> {
        let session = self.bulk_session()?;
        self.offer_on(session, name, size, kind).await
    }
    async fn offer_on(
        &self,
        session: BulkSession,
        name: String,
        size: u64,
        kind: BulkKind,
    ) -> Result<(String, BulkSession)> {
        let id = random_id();
        let (tx, rx) = oneshot::channel();
        self.state().decisions.insert(id.clone(), tx);
        if let Err(error) = self
            .bulk(
                &session,
                Message::Offer {
                    id: id.clone(),
                    name,
                    size,
                    kind,
                },
            )
            .await
        {
            self.state().decisions.remove(&id);
            return Err(error);
        }
        let decision = timeout(Duration::from_secs(65), rx).await;
        self.state().decisions.remove(&id);
        if !matches!(decision, Ok(Ok(true))) {
            let _ = self.bulk(&session, Message::Cancel { id }).await;
            bail!("Transfer declined or timed out");
        }
        Ok((id, session))
    }
    async fn finish_send(&self, session: &BulkSession, id: String, sha256: [u8; 32]) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.state().decisions.insert(id.clone(), tx);
        let result = async {
            self.bulk(
                session,
                Message::Complete {
                    id: id.clone(),
                    sha256,
                },
            )
            .await?;
            ensure!(
                matches!(timeout(Duration::from_secs(15), rx).await, Ok(Ok(true))),
                "Receiver did not confirm a successful save"
            );
            Ok(())
        }
        .await;
        self.state().decisions.remove(&id);
        result
    }
    pub async fn send_file(&self, path: String) -> Result<()> {
        let result = self.send_file_inner(path).await;
        if let Err(error) = &result {
            self.error(EventCode::FileTransferFailed, error);
        }
        result
    }
    async fn send_file_inner(&self, path: String) -> Result<()> {
        let _guard = self
            .0
            .outgoing
            .try_lock()
            .context("Another transfer is in progress")?;
        ensure!(
            self.state().config.settings.share_files,
            "Enable file sharing first"
        );
        let path = Path::new(&path);
        let name = path
            .file_name()
            .and_then(|x| x.to_str())
            .context("Invalid filename")?
            .to_string();
        validate_filename(&name)?;
        let mut file = File::open(path).await?;
        let metadata = file.metadata().await?;
        ensure!(
            metadata.is_file() && metadata.len() < MAX_FILE_BYTES,
            "Choose a regular file smaller than 100 MB (100,000,000 bytes)"
        );
        let size = metadata.len();
        let (id, session) = self.offer(name, size, BulkKind::File).await?;
        let result = async {
            let mut hash = Sha256::new();
            let mut count = 0;
            let mut buffer = vec![0; CHUNK_BYTES];
            loop {
                ensure!(
                    self.state().config.settings.share_files,
                    "File sharing was disabled"
                );
                let n = file.read(&mut buffer).await?;
                if n == 0 {
                    break;
                }
                count += n as u64;
                ensure!(count <= size, "File changed during transfer");
                hash.update(&buffer[..n]);
                self.bulk(
                    &session,
                    Message::Chunk {
                        id: id.clone(),
                        data: buffer[..n].to_vec(),
                    },
                )
                .await?;
            }
            ensure!(count == size, "File changed during transfer");
            self.finish_send(&session, id.clone(), hash.finalize().into())
                .await?;
            Ok::<_, anyhow::Error>(())
        }
        .await;
        if result.is_err() {
            let _ = self.bulk(&session, Message::Cancel { id }).await;
        } else {
            self.event(EventCode::FileDelivered);
        }
        result
    }
    async fn send_text(&self, text: String, session: BulkSession) -> Result<()> {
        let _guard = self.0.outgoing.try_lock().context("Bulk sender busy")?;
        let (id, session) = self
            .offer_on(session, String::new(), text.len() as u64, BulkKind::Text)
            .await?;
        for data in text.as_bytes().chunks(CHUNK_BYTES) {
            ensure!(
                self.state().config.settings.share_clipboard,
                "Clipboard sharing disabled"
            );
            self.bulk(
                &session,
                Message::Chunk {
                    id: id.clone(),
                    data: data.to_vec(),
                },
            )
            .await?;
        }
        self.finish_send(&session, id, Sha256::digest(text.as_bytes()).into())
            .await?;
        self.clipboard_notice(ClipboardNotice::TextSent, session.generation);
        Ok(())
    }
    async fn apply_clipboard_image(
        &self,
        prepared: PreparedImage,
        generation: u64,
        bulk_generation: u64,
    ) -> Result<()> {
        let e = self.clone();
        tokio::task::spawn_blocking(move || {
            #[cfg(test)]
            {
                let sink = e.0.image_sink.lock().unwrap().clone();
                if let Some(sink) = sink {
                    let mut s = e.state();
                    ensure!(
                        s.generation == generation
                            && s.bulk_generation == bulk_generation
                            && s.connected
                            && s.bulk.is_some()
                            && !*e.0.stop.borrow()
                            && s.config.settings.enabled
                            && s.config.settings.share_clipboard_images,
                        "Image clipboard transfer revoked"
                    );
                    s.suppress_clipboard_image = Some(prepared.digest());
                    s.clipboard_content_revision = s.clipboard_content_revision.wrapping_add(1);
                    sink(&prepared)?;
                    return Ok(());
                }
            }
            let digest = prepared.digest();
            // Native allocation, cache hashing/eviction and Zone.Identifier I/O
            // finish on this worker before acquiring the session commit lock.
            let mut publication = emc_platform::clipboard_image::prepare_publication(prepared)?;
            let result = {
                let mut s = e.state();
                ensure!(
                    s.generation == generation
                        && s.bulk_generation == bulk_generation
                        && s.connected
                        && s.bulk.is_some()
                        && !*e.0.stop.borrow()
                        && s.config.settings.enabled
                        && s.config.settings.share_clipboard_images,
                    "Image clipboard transfer revoked"
                );
                s.suppress_clipboard_image = Some(digest);
                // EmptyClipboard or earlier formats may succeed before a later
                // native call fails. Every permitted attempt invalidates reads
                // sampled before it, including a partially published clipboard.
                s.clipboard_content_revision = s.clipboard_content_revision.wrapping_add(1);
                publication.publish()
            };
            // Publication drops its clipboard/cache guards, allocations and
            // temporary files here, after state, including on publish failure.
            result
        })
        .await?
    }
    async fn send_image(&self, image: ClipboardImage, session: BulkSession) -> Result<()> {
        let _guard = self.0.outgoing.try_lock().context("Bulk sender busy")?;
        let png = tokio::task::spawn_blocking(move || image.to_png()).await??;
        let (id, session) = self
            .offer_on(session, String::new(), png.len() as u64, BulkKind::Image)
            .await?;
        for data in png.chunks(CHUNK_BYTES) {
            self.bulk(
                &session,
                Message::Chunk {
                    id: id.clone(),
                    data: data.to_vec(),
                },
            )
            .await?;
        }
        self.finish_send(&session, id, Sha256::digest(&png).into())
            .await?;
        self.clipboard_notice(ClipboardNotice::ImageSent, session.generation);
        Ok(())
    }
    async fn poll_clipboard_image_with(
        &self,
        previous: &mut ImageBaseline,
        read: impl std::future::Future<Output = Result<ClipboardImageSample>>,
    ) -> Result<()> {
        let Some((context, session)) = self.clipboard_session_for(BulkKind::Image) else {
            *previous = None;
            return Ok(());
        };
        let ClipboardImageSample {
            sequence,
            current_sequence,
            image,
        } = read.await?;
        let hash = image.as_ref().map(ClipboardImage::digest);
        let suppressed = {
            let mut s = self.state();
            let current = s.clipboard_context_for(BulkKind::Image);
            if *self.0.stop.borrow() || !current.is_some_and(|now| context.same_activation(now)) {
                *previous = None;
                return Ok(());
            }
            if current != Some(context) {
                // A received clipboard write invalidates this read, while its
                // activation baseline still belongs to the same trusted session.
                return Ok(());
            }
            if sequence != current_sequence {
                // Native bytes and their token were captured together while
                // OpenClipboard was held. A change during decode needs a fresh
                // read; it cannot become a baseline or a copy of stale pixels.
                return Ok(());
            }
            s.suppress_clipboard_image.take()
        };
        let baseline = previous
            .as_ref()
            .filter(|(old, _, _)| context.same_activation(*old));
        if baseline.is_none()
            || baseline.is_some_and(|(_, old_sequence, _)| *old_sequence == sequence)
            || hash.is_none()
            || hash.is_some_and(|hash| suppressed == Some(hash))
        {
            // Absence is a real activation baseline: the first image copied
            // later is new, while an image already present on activation stays local.
            *previous = Some((context, sequence, hash));
            return Ok(());
        }
        self.send_image(image.context("Missing clipboard image")?, session)
            .await?;
        *previous = Some((context, sequence, hash));
        Ok(())
    }
    async fn poll_clipboard_with(
        &self,
        previous: &mut ClipboardBaseline,
        read: impl std::future::Future<Output = Option<String>>,
    ) -> Result<()> {
        let Some((context, session)) = self.clipboard_session() else {
            *previous = None;
            return Ok(());
        };
        if previous.is_some_and(|(old, _)| !context.same_activation(old)) {
            *previous = None;
        }
        let Some(text) = read.await else {
            return Ok(());
        };
        let suppressed = {
            let s = self.state();
            let current = s.clipboard_context();
            if *self.0.stop.borrow() || !current.is_some_and(|now| context.same_activation(now)) {
                // An asynchronous read cannot be adopted by another activation
                // or session, including off/on transitions this poll missed.
                *previous = None;
                return Ok(());
            }
            if current != Some(context) {
                return Ok(());
            }
            s.suppress_clipboard
        };
        if text.len() > MAX_CLIPBOARD_BYTES {
            return Ok(());
        }
        let hash: [u8; 32] = Sha256::digest(text.as_bytes()).into();
        if previous.is_none()
            || previous.is_some_and(|(_, old_hash)| old_hash == hash)
            || pairing::looks_like_credential(&text)
            || suppressed == Some(hash)
        {
            *previous = Some((context, hash));
            return Ok(());
        }
        // Carry the sender captured before the read through offer and every
        // chunk/commit. No later lookup may select a replacement bulk channel.
        self.send_text(text, session).await?;
        *previous = Some((context, hash));
        Ok(())
    }
    async fn clipboard_loop(&self) {
        let mut tick = interval(Duration::from_millis(350));
        let mut stop = self.0.stop.subscribe();
        let mut previous = None;
        let mut previous_image = None;
        loop {
            tokio::select! {
                _ = stopped(&mut stop) => break,
                _ = tick.tick() => {
                    let read = async {
                        tokio::task::spawn_blocking(emc_platform::read_clipboard)
                            .await.ok().and_then(Result::ok).flatten()
                    };
                    if let Err(error) = self.poll_clipboard_with(&mut previous, read).await {
                        self.error(EventCode::ClipboardTransferFailed, &error);
                    }
                    let context = self.clipboard_session_for(BulkKind::Image).map(|(context, _)| context);
                    let sequence = emc_platform::clipboard_sequence();
                    if previous_image.map(|(context, sequence, _)| (context, sequence))
                        != context.map(|context| (context, sequence)) {
                        let read = async {
                            let (sequence, image) = tokio::task::spawn_blocking(
                                emc_platform::clipboard_image::read_image_sample)
                                .await.context("Image clipboard read worker failed")??;
                            Ok(ClipboardImageSample { sequence,
                                current_sequence: emc_platform::clipboard_sequence(), image })
                        };
                        if let Err(error) = self.poll_clipboard_image_with(&mut previous_image, read).await {
                            self.error(EventCode::ClipboardTransferFailed, &error);
                        }
                    }
                }
            }
        }
    }
    pub async fn diagnostics(&self, address: String) -> Result<String> {
        let addr = parse_address(address.trim())?;
        let start = Instant::now();
        match timeout(Duration::from_secs(4),TcpStream::connect(addr)).await {
            Ok(Ok(_))=>Ok(format!("TCP {} is reachable ({} ms). This checks the route and open port only; pairing still verifies the device identity. If Tailscale is slow, run tailscale ping <peer> to check whether it uses a relay.",addr,start.elapsed().as_millis())),
            Ok(Err(e))=>Ok(format!("Cannot reach {addr}: {e}. Verify the other app is running and its TCP {PORT} firewall rule permits this device. Different private subnets need a router/VPN route; use the peer's Tailscale IP when both are on the same tailnet.")),
            Err(_)=>Ok(format!("TCP connection to {addr} timed out. Check routing, Tailscale access policy, Windows Firewall, and the remote app. Entering a private IP cannot create a route across the internet.")),
        }
    }
}

async fn read_hello(channel: &mut Channel) -> Result<String> {
    let Message::Hello { name } = timeout(Duration::from_secs(5), channel.reader.recv()).await??
    else {
        bail!("Expected peer greeting")
    };
    ensure!(
        !name.is_empty() && name.len() <= 128 && !name.chars().any(char::is_control),
        "Invalid device name"
    );
    Ok(name)
}
fn parse_invitation(invitation: &str) -> Result<([u8; 32], Zeroizing<[u8; 32]>)> {
    ensure!(invitation.len() <= 128, "Invalid invitation");
    let parts: Vec<_> = invitation.split('.').collect();
    ensure!(
        parts.len() == 3 && parts[0] == "emc1",
        "Use a complete emc1 pairing invitation"
    );
    Ok((decode_key(parts[1])?, Zeroizing::new(decode_key(parts[2])?)))
}
fn random_id() -> String {
    let mut bytes = [0; 16];
    OsRng.fill_bytes(&mut bytes);
    bytes.iter().map(|x| format!("{x:02x}")).collect()
}
fn spawn_reader(
    mut reader: transport::Reader,
) -> (mpsc::Receiver<Result<Message>>, tokio::task::JoinHandle<()>) {
    let (tx, rx) = mpsc::channel(32);
    let task = tokio::spawn(async move {
        loop {
            let result = reader.recv().await;
            let failed = result.is_err();
            if tx.send(result).await.is_err() || failed {
                break;
            }
        }
    });
    (rx, task)
}
fn local_addresses(port: u16) -> Vec<String> {
    let mut addresses = vec![];
    // UDP connect selects a local interface; no datagrams are sent to these addresses.
    for route in ["192.0.2.1:9", "100.100.100.100:9"] {
        if let Ok(socket) = std::net::UdpSocket::bind("0.0.0.0:0") {
            if socket.connect(route).is_ok() {
                if let Ok(addr) = socket.local_addr() {
                    let value = SocketAddr::new(addr.ip(), port).to_string();
                    if !addresses.contains(&value) {
                        addresses.push(value);
                    }
                }
            }
        }
    }
    addresses
}
fn mark_download(path: &Path) -> Result<()> {
    #[cfg(windows)]
    {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(format!("{}:Zone.Identifier", path.display()))?;
        file.write_all(b"[ZoneTransfer]\r\nZoneId=3\r\n")?;
    }
    #[cfg(not(windows))]
    let _ = path;
    Ok(())
}

async fn stopped(stop: &mut watch::Receiver<bool>) {
    let _ = stop.wait_for(|value| *value).await;
}

#[cfg(test)]
#[path = "engine_tests.rs"]
mod tests;

// The isolated policy fixture uses the unsupported controller so these tests
// never install Windows hooks or access the real clipboard/input desktop.
#[cfg(all(test, not(windows)))]
#[path = "engine_policy_tests.rs"]
mod policy_tests;

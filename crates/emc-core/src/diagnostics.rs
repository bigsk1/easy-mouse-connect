//! Bounded, metadata-only local diagnostics. The API cannot accept user strings.
//!
//! Input callbacks never touch disk: callers enqueue typed events on a bounded
//! channel. A dedicated thread owns rotation and explicit diagnostic export.
use anyhow::{ensure, Context, Result};
use emc_platform::InputDiagnostics;
use serde::Serialize;
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender},
        Arc, Mutex,
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::oneshot;
use zip::{write::SimpleFileOptions, CompressionMethod, ZipWriter};

pub const MAX_FILES: usize = 3;
pub const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
const NAMES: [&str; MAX_FILES] = ["emc.log", "emc.1.log", "emc.2.log"];
const QUEUE_SIZE: usize = 256;
// Export work occurs only in the writer thread or spawn_blocking. This also
// serializes a startup fallback against an export still running after timeout.
static EXPORT_LOCK: Mutex<()> = Mutex::new(());

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EventCode {
    EngineStarted,
    EngineStopped,
    SharingEnabled,
    SharingPaused,
    PairingWindowOpened,
    PairingSucceeded,
    PairingRejected,
    AuthenticationRejected,
    ConnectionAdmissionRejected,
    PeerConnected,
    PeerDisconnected,
    ReconnectFailed,
    HeartbeatTimeout,
    InputQueueOverflow,
    InputInjectionFailed,
    InputEntryFailed,
    InputDispatchStale,
    EmergencyStop,
    ClipboardTransferFailed,
    ClipboardApplied,
    FileOffered,
    FileAccepted,
    FileDeclined,
    FileReceived,
    FileDelivered,
    FileTransferFailed,
    PeerForgotten,
    BulkChannelClosed,
    ControlChannelClosed,
    OperationFailed,
    DiagnosticsExported,
}
impl EventCode {
    pub fn description(self) -> &'static str {
        match self {
            Self::EngineStarted => "Engine started; waiting for a trusted device.",
            Self::EngineStopped => "Engine stopped.",
            Self::SharingEnabled => "Desk sharing enabled.",
            Self::SharingPaused => "Desk sharing paused; control returned locally.",
            Self::PairingWindowOpened => "A temporary pairing code was created.",
            Self::PairingSucceeded => "Pairing completed; device identities are pinned.",
            Self::PairingRejected => "Pairing attempt rejected.",
            Self::AuthenticationRejected => "Connection authentication rejected.",
            Self::ConnectionAdmissionRejected => "Connection rejected by local sharing policy.",
            Self::PeerConnected => "Connected with authenticated encryption.",
            Self::PeerDisconnected => "Connection lost; control returned locally.",
            Self::ReconnectFailed => "Trusted reconnect attempt failed.",
            Self::HeartbeatTimeout => "Heartbeat timed out; input released.",
            Self::InputQueueOverflow => "Input queue unavailable; control returned locally.",
            Self::InputInjectionFailed => "Windows input injection failed; control released.",
            Self::InputEntryFailed => "Remote input entry failed; control released.",
            Self::InputDispatchStale => {
                "Input processing exceeded its local age limit; control released."
            }
            Self::EmergencyStop => "Emergency stop: sharing paused; control is local.",
            Self::ClipboardTransferFailed => "Clipboard transfer could not complete.",
            Self::ClipboardApplied => "A permitted clipboard update was applied.",
            Self::FileOffered => "A peer requested permission to send a file.",
            Self::FileAccepted => "Incoming file transfer accepted.",
            Self::FileDeclined => "Incoming file transfer declined.",
            Self::FileReceived => "File received and saved after integrity validation.",
            Self::FileDelivered => "File delivered; receiver confirmed integrity and save.",
            Self::FileTransferFailed => "File transfer could not complete.",
            Self::PeerForgotten => "Peer forgotten; connection authority revoked.",
            Self::BulkChannelClosed => "File and clipboard channel closed.",
            Self::ControlChannelClosed => "Authenticated input channel closed.",
            Self::OperationFailed => {
                "An engine operation failed; inspect diagnostic error category."
            }
            Self::DiagnosticsExported => "A local diagnostic archive was exported by the user.",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureReason {
    ConnectionRefused,
    ConnectionReset,
    ConnectionAborted,
    TimedOut,
    PermissionDenied,
    NetworkUnreachable,
    InvalidData,
    NotFound,
    AuthenticationFailed,
    DiskFull,
    Unavailable,
    Internal,
    SessionRevoked,
    DuplicateSession,
    PolicyPaused,
    Backlog,
    StaleInput,
    NativePreconditionUnavailable,
    NativeCursorUnavailable,
    NativeCommandQueueUnavailable,
}
impl std::fmt::Display for FailureReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for FailureReason {}
impl FailureReason {
    /// Categorize typed errors without formatting an error chain or its data.
    pub fn from_error(error: &anyhow::Error) -> Self {
        // Anyhow also exposes typed context values through downcast_ref; an
        // ordinary std::error::Error chain alone cannot recover those values.
        if let Some(reason) = error.downcast_ref::<Self>() {
            return *reason;
        }
        if let Some(native) = error.downcast_ref::<emc_platform::NativeInputFailure>() {
            return match native {
                emc_platform::NativeInputFailure::PreconditionUnavailable => {
                    Self::NativePreconditionUnavailable
                }
                emc_platform::NativeInputFailure::CursorUnavailable => {
                    Self::NativeCursorUnavailable
                }
                emc_platform::NativeInputFailure::CommandQueueUnavailable => {
                    Self::NativeCommandQueueUnavailable
                }
                emc_platform::NativeInputFailure::StaleCommand => Self::StaleInput,
            };
        }
        for source in error.chain() {
            if let Some(reason) = source.downcast_ref::<Self>() {
                return *reason;
            }
            if source.is::<snow::Error>() {
                return Self::AuthenticationFailed;
            }
            if source.is::<tokio::time::error::Elapsed>() {
                return Self::TimedOut;
            }
            if let Some(io) = source.downcast_ref::<std::io::Error>() {
                return match io.kind() {
                    std::io::ErrorKind::ConnectionRefused => Self::ConnectionRefused,
                    std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::BrokenPipe
                    | std::io::ErrorKind::UnexpectedEof => Self::ConnectionReset,
                    std::io::ErrorKind::ConnectionAborted => Self::ConnectionAborted,
                    std::io::ErrorKind::TimedOut => Self::TimedOut,
                    std::io::ErrorKind::PermissionDenied => Self::PermissionDenied,
                    std::io::ErrorKind::NetworkUnreachable
                    | std::io::ErrorKind::HostUnreachable => Self::NetworkUnreachable,
                    std::io::ErrorKind::InvalidData | std::io::ErrorKind::InvalidInput => {
                        Self::InvalidData
                    }
                    std::io::ErrorKind::NotFound => Self::NotFound,
                    std::io::ErrorKind::StorageFull => Self::DiskFull,
                    std::io::ErrorKind::WouldBlock => Self::Unavailable,
                    _ => Self::Internal,
                };
            }
        }
        Self::Internal
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct DiagnosticStatus {
    pub directory: String,
    pub max_files: usize,
    pub max_file_bytes: u64,
    pub retained_bytes: u64,
    pub dropped_events: u64,
    pub write_failures: u64,
}

/// Only explicit booleans, numeric counters and native aggregate diagnostics.
/// There is intentionally no peer/address/path/credential/free-text field.
#[derive(Debug, Default, Serialize)]
pub struct DiagnosticSummary {
    pub engine_available: bool,
    pub native_input: bool,
    pub sharing_enabled: bool,
    pub paired: bool,
    pub connected: bool,
    pub share_clipboard: bool,
    pub share_files: bool,
    pub edge_switching: bool,
    pub peer_on_left: bool,
    pub round_trip_ms: Option<u64>,
    pub input: InputDiagnostics,
    pub control_diagnostics: crate::input_pipeline::ControlDiagnostics,
}

#[derive(Serialize)]
struct Record {
    schema: u8,
    timestamp_unix_ms: u64,
    app_version: &'static str,
    event: EventCode,
    #[serde(skip_serializing_if = "Option::is_none")]
    failure: Option<FailureReason>,
}
impl Record {
    fn new(event: EventCode, failure: Option<FailureReason>) -> Self {
        Self {
            schema: 1,
            timestamp_unix_ms: unix_ms(),
            app_version: env!("CARGO_PKG_VERSION"),
            event,
            failure,
        }
    }
    fn bytes(&self) -> Result<Vec<u8>> {
        let mut bytes = serde_json::to_vec(self)?;
        bytes.push(b'\n');
        ensure!(
            bytes.len() < 512,
            "Diagnostic record exceeds its fixed schema bound"
        );
        Ok(bytes)
    }
}
enum Request {
    Record(Record),
    Flush(mpsc::Sender<()>),
    Export(Box<DiagnosticSummary>, oneshot::Sender<Result<PathBuf>>),
}
#[derive(Default)]
struct Metrics {
    retained: AtomicU64,
    dropped: AtomicU64,
    failures: AtomicU64,
}
struct Handle {
    sender: SyncSender<Request>,
    metrics: Arc<Metrics>,
    directory: PathBuf,
}
#[derive(Clone)]
pub struct DiagnosticLog(Arc<Handle>);

impl DiagnosticLog {
    pub fn open(directory: PathBuf) -> Result<Self> {
        fs::create_dir_all(&directory).context("Cannot create the local diagnostics directory")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))?;
        }
        let metrics = Arc::new(Metrics::default());
        let writer = RotatingWriter::open(directory.clone(), MAX_FILE_BYTES, metrics.clone())?;
        let (sender, receiver) = mpsc::sync_channel(QUEUE_SIZE);
        thread::Builder::new()
            .name("emc-diagnostic-writer".into())
            .spawn(move || worker(receiver, writer))?;
        Ok(Self(Arc::new(Handle {
            sender,
            metrics,
            directory,
        })))
    }
    pub fn record(&self, code: EventCode) {
        self.enqueue(Record::new(code, None));
    }
    pub fn record_failure(&self, code: EventCode, reason: FailureReason) {
        self.enqueue(Record::new(code, Some(reason)));
    }
    fn enqueue(&self, record: Record) {
        if self.0.sender.try_send(Request::Record(record)).is_err() {
            self.0.metrics.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
    /// Atomics only: safe to call while taking an engine snapshot for the UI.
    pub fn status(&self) -> DiagnosticStatus {
        DiagnosticStatus {
            directory: self.0.directory.display().to_string(),
            max_files: MAX_FILES,
            max_file_bytes: MAX_FILE_BYTES,
            retained_bytes: self.0.metrics.retained.load(Ordering::Relaxed),
            dropped_events: self.0.metrics.dropped.load(Ordering::Relaxed),
            write_failures: self.0.metrics.failures.load(Ordering::Relaxed),
        }
    }
    /// Reserved for explicit shutdown; never invoke from an input callback.
    pub fn flush_timeout(&self, duration: Duration) {
        let (tx, rx) = mpsc::channel();
        if self.0.sender.try_send(Request::Flush(tx)).is_ok() {
            let _ = rx.recv_timeout(duration);
        }
    }
    /// No automatic uploads or arbitrary archive inputs. The single latest ZIP
    /// is replaced only on an explicit request, bounding application exports.
    pub async fn export(&self, summary: DiagnosticSummary) -> Result<PathBuf> {
        let (tx, rx) = oneshot::channel();
        self.0
            .sender
            .try_send(Request::Export(Box::new(summary), tx))
            .map_err(|_| anyhow::anyhow!("Diagnostic writer is busy; try exporting again"))?;
        tokio::time::timeout(Duration::from_secs(15), rx)
            .await
            .context("Diagnostic export timed out")??
    }
    /// Metadata only. This never opens a writer or creates/truncates a log.
    pub fn existing_status(directory: &Path) -> Result<DiagnosticStatus> {
        let mut retained_bytes = 0;
        for name in NAMES {
            let path = directory.join(name);
            let metadata = match fs::symlink_metadata(path) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.into()),
            };
            ensure!(
                metadata.is_file() && !metadata.file_type().is_symlink(),
                "Invalid managed diagnostic file"
            );
            ensure!(
                metadata.len() <= MAX_FILE_BYTES,
                "Diagnostic file exceeds retention bounds"
            );
            retained_bytes += metadata.len();
        }
        Ok(DiagnosticStatus {
            directory: directory.display().to_string(),
            max_files: MAX_FILES,
            max_file_bytes: MAX_FILE_BYTES,
            retained_bytes,
            dropped_events: 0,
            write_failures: 0,
        })
    }
    /// An explicit startup-failure export. No second logger is started, and no
    /// record is appended to logs that another process might currently own.
    pub async fn export_existing(directory: PathBuf) -> Result<PathBuf> {
        tokio::time::timeout(
            Duration::from_secs(15),
            tokio::task::spawn_blocking(move || {
                write_archive(
                    &directory,
                    MAX_FILE_BYTES,
                    DiagnosticSummary::default(),
                    0,
                    0,
                    None,
                )
            }),
        )
        .await
        .context("Diagnostic export timed out")?
        .context("Diagnostic export worker failed")?
    }
}

struct RotatingWriter {
    directory: PathBuf,
    file: Option<File>,
    bytes: u64,
    limit: u64,
    metrics: Arc<Metrics>,
    opened_at_unix_ms: u64,
}
impl RotatingWriter {
    fn open(directory: PathBuf, limit: u64, metrics: Arc<Metrics>) -> Result<Self> {
        for name in NAMES {
            let path = directory.join(name);
            if path.exists() && fs::symlink_metadata(&path)?.file_type().is_symlink() {
                anyhow::bail!("Diagnostic files must not be symbolic links");
            }
            if path.exists() && fs::metadata(&path)?.len() > limit {
                File::create(&path)?.sync_all()?;
            }
        }
        let file = private_append(&directory.join(NAMES[0]))?;
        let bytes = file.metadata()?.len();
        let mut writer = Self {
            directory,
            file: Some(file),
            bytes,
            limit,
            metrics,
            opened_at_unix_ms: unix_ms(),
        };
        writer.refresh_size();
        Ok(writer)
    }
    fn refresh_size(&mut self) {
        let bytes: u64 = NAMES
            .iter()
            .map(|name| {
                fs::metadata(self.directory.join(name))
                    .map(|m| m.len())
                    .unwrap_or(0)
            })
            .sum();
        self.metrics.retained.store(bytes, Ordering::Relaxed);
    }
    fn append(&mut self, record: &Record) -> Result<()> {
        let bytes = record.bytes()?;
        ensure!(
            bytes.len() as u64 <= self.limit,
            "Diagnostic file bound is too small"
        );
        if self.bytes + bytes.len() as u64 > self.limit {
            self.rotate()?;
        }
        if self.file.is_none() {
            self.file = Some(private_append(&self.directory.join(NAMES[0]))?);
        }
        let file = self.file.as_mut().context("Diagnostic file unavailable")?;
        let written = file.write_all(&bytes);
        self.bytes = file.metadata()?.len();
        written?;
        self.refresh_size();
        Ok(())
    }
    fn rotate(&mut self) -> Result<()> {
        if let Some(file) = &self.file {
            file.sync_all()?;
        }
        // Close before rename; this also works with Windows sharing rules.
        self.file.take();
        let result = (|| {
            let oldest = self.directory.join(NAMES[2]);
            if oldest.exists() {
                fs::remove_file(oldest)?;
            }
            let previous = self.directory.join(NAMES[1]);
            if previous.exists() {
                fs::rename(previous, self.directory.join(NAMES[2]))?;
            }
            let current = self.directory.join(NAMES[0]);
            if current.exists() {
                fs::rename(current, self.directory.join(NAMES[1]))?;
            }
            Ok::<_, anyhow::Error>(())
        })();
        self.file = Some(private_append(&self.directory.join(NAMES[0]))?);
        self.bytes = self.file.as_ref().unwrap().metadata()?.len();
        self.refresh_size();
        result
    }
    fn export(&mut self, summary: DiagnosticSummary) -> Result<PathBuf> {
        self.append(&Record::new(EventCode::DiagnosticsExported, None))?;
        if let Some(file) = &self.file {
            file.sync_all()?;
        }
        write_archive(
            &self.directory,
            self.limit,
            summary,
            self.metrics.dropped.load(Ordering::Relaxed),
            self.metrics.failures.load(Ordering::Relaxed),
            Some(self.opened_at_unix_ms),
        )
    }
}

/// Shared by active-engine and startup-failure exports. Only known, bounded,
/// allowlisted records can enter an archive; there is no append/open writer.
fn write_archive(
    directory: &Path,
    limit: u64,
    summary: DiagnosticSummary,
    dropped_events: u64,
    write_failures: u64,
    logger_opened_at_unix_ms: Option<u64>,
) -> Result<PathBuf> {
    let _guard = EXPORT_LOCK
        .lock()
        .map_err(|_| anyhow::anyhow!("Diagnostic exporter is unavailable"))?;
    let parent = directory
        .parent()
        .context("Invalid diagnostics directory")?;
    let exports = parent.join("diagnostic-exports");
    fs::create_dir_all(&exports)?;
    ensure!(
        !fs::symlink_metadata(&exports)?.file_type().is_symlink(),
        "Diagnostic export directory must not be a symbolic link"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&exports, fs::Permissions::from_mode(0o700))?;
    }
    let temporary = exports.join(".emc-diagnostics.tmp");
    let destination = exports.join("emc-diagnostics.zip");
    let result = (|| {
        // Remove only a stale regular temporary file, then create_new so a
        // replaced symlink can never turn export into an arbitrary write.
        match fs::symlink_metadata(&temporary) {
            Ok(metadata) => {
                ensure!(
                    metadata.is_file() && !metadata.file_type().is_symlink(),
                    "Invalid diagnostic temporary file"
                );
                fs::remove_file(&temporary)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut archive = ZipWriter::new(options.open(&temporary)?);
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        let mut latest_retained_engine_start = None;
        // Re-parse every bounded record and reserialize its allowlisted
        // schema. Never copy arbitrary bytes even if a log is modified.
        for name in NAMES.iter().rev() {
            let path = directory.join(name);
            if !path.exists() {
                continue;
            }
            let metadata = fs::symlink_metadata(&path)?;
            ensure!(
                metadata.is_file() && !metadata.file_type().is_symlink(),
                "Diagnostic file was replaced"
            );
            ensure!(
                metadata.len() <= limit,
                "Diagnostic file exceeds export bounds"
            );
            let mut data = String::new();
            File::open(path)?
                .take(limit + 1)
                .read_to_string(&mut data)?;
            ensure!(
                data.len() as u64 <= limit,
                "Diagnostic file exceeds export bounds"
            );
            archive.start_file(format!("logs/{name}"), options)?;
            for line in data.lines() {
                if let Some(record) = sanitize_record(line) {
                    // Files are traversed oldest to newest and records retain
                    // append order. Wall clocks can move backward, so the
                    // largest timestamp is not necessarily the latest start.
                    if let Some(start) = record.engine_start {
                        latest_retained_engine_start = Some(start);
                    }
                    archive.write_all(&record.bytes)?;
                }
            }
        }
        let summary = serde_json::json!({
            "schema":1, "app_version":env!("CARGO_PKG_VERSION"), "exported_at_unix_ms":unix_ms(),
            "logger_opened_at_unix_ms":logger_opened_at_unix_ms,
            "latest_retained_engine_start":latest_retained_engine_start,
            "retention":{"max_files":MAX_FILES,"max_file_bytes":MAX_FILE_BYTES},
            "dropped_events":dropped_events,
            "write_failures":write_failures, "engine":summary,
            "privacy":"Typed operational events and aggregate input counters only. No input history, clipboard/file content, credentials, device names, IP addresses or filesystem paths."
        });
        archive.start_file("summary.json", options)?;
        archive.write_all(&serde_json::to_vec_pretty(&summary)?)?;
        let output = archive.finish()?;
        output.sync_all()?;
        drop(output);
        fs::rename(&temporary, &destination)?;
        Ok::<_, anyhow::Error>(destination.clone())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

fn private_append(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    Ok(options.open(path)?)
}
fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}
fn worker(receiver: Receiver<Request>, mut writer: RotatingWriter) {
    while let Ok(request) = receiver.recv() {
        match request {
            Request::Record(record) => {
                if writer.append(&record).is_err() {
                    writer.metrics.failures.fetch_add(1, Ordering::Relaxed);
                }
            }
            Request::Flush(done) => {
                if writer.file.as_ref().is_some_and(|f| f.sync_all().is_err()) {
                    writer.metrics.failures.fetch_add(1, Ordering::Relaxed);
                }
                let _ = done.send(());
            }
            Request::Export(summary, done) => {
                let result = writer.export(*summary);
                if result.is_err() {
                    writer.metrics.failures.fetch_add(1, Ordering::Relaxed);
                }
                let _ = done.send(result);
            }
        }
    }
    if let Some(file) = &writer.file {
        let _ = file.sync_all();
    }
}

// Strict decoding drops unrecognized fields/strings on export rather than
// exposing raw errors or edits. Only the exact types we generate are accepted.
#[derive(Serialize)]
struct RetainedEngineStart {
    timestamp_unix_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    app_version: Option<String>,
}
struct SanitizedRecord {
    bytes: Vec<u8>,
    engine_start: Option<RetainedEngineStart>,
}

/// Historical versions are untrusted metadata, not free-text fields or proof
/// that an event came from that release. Accept canonical numeric releases only.
fn release_version(value: &serde_json::Value) -> Option<&str> {
    let version = value.as_str()?;
    if version.len() > 17 {
        return None;
    }
    let mut components = version.split('.');
    for _ in 0..3 {
        let part = components.next()?;
        if part.is_empty()
            || !part.bytes().all(|byte| byte.is_ascii_digit())
            || (part.len() > 1 && part.starts_with('0'))
        {
            return None;
        }
        part.parse::<u16>().ok()?;
    }
    if components.next().is_some() {
        return None;
    }
    Some(version)
}

fn sanitize_record(line: &str) -> Option<SanitizedRecord> {
    if line.len() >= 512 {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(line).ok()?;
    let event = value.get("event")?.as_str()?;
    let allowed = [
        "engine_started",
        "engine_stopped",
        "sharing_enabled",
        "sharing_paused",
        "pairing_window_opened",
        "pairing_succeeded",
        "pairing_rejected",
        "authentication_rejected",
        "connection_admission_rejected",
        "peer_connected",
        "peer_disconnected",
        "reconnect_failed",
        "heartbeat_timeout",
        "input_queue_overflow",
        "input_injection_failed",
        "input_entry_failed",
        "input_dispatch_stale",
        "emergency_stop",
        "clipboard_transfer_failed",
        "clipboard_applied",
        "file_offered",
        "file_accepted",
        "file_declined",
        "file_received",
        "file_delivered",
        "file_transfer_failed",
        "peer_forgotten",
        "bulk_channel_closed",
        "control_channel_closed",
        "operation_failed",
        "diagnostics_exported",
    ];
    if !allowed.contains(&event) {
        return None;
    }
    let timestamp_unix_ms = value.get("timestamp_unix_ms")?.as_u64()?;
    let app_version = value.get("app_version").and_then(release_version);
    let mut output =
        serde_json::json!({"schema":1,"timestamp_unix_ms":timestamp_unix_ms,"event":event});
    if let Some(version) = app_version {
        output["app_version"] = version.into();
    }
    if let Some(failure) = value.get("failure").and_then(|v| v.as_str()) {
        let allowed = [
            "connection_refused",
            "connection_reset",
            "connection_aborted",
            "timed_out",
            "permission_denied",
            "network_unreachable",
            "invalid_data",
            "not_found",
            "authentication_failed",
            "disk_full",
            "unavailable",
            "internal",
            "session_revoked",
            "duplicate_session",
            "policy_paused",
            "backlog",
            "stale_input",
            "native_precondition_unavailable",
            "native_cursor_unavailable",
            "native_command_queue_unavailable",
        ];
        if allowed.contains(&failure) {
            output["failure"] = failure.into();
        }
    }
    let mut bytes = serde_json::to_vec(&output).ok()?;
    bytes.push(b'\n');
    Some(SanitizedRecord {
        bytes,
        engine_start: (event == "engine_started").then(|| RetainedEngineStart {
            timestamp_unix_ms,
            app_version: app_version.map(str::to_owned),
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn startup_export_reads_only_existing_managed_logs_without_starting_a_writer() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("logs");
        let empty = DiagnosticLog::existing_status(&directory).unwrap();
        assert_eq!(empty.retained_bytes, 0);
        assert!(
            !directory.exists(),
            "Metadata inspection must not create log files"
        );
        fs::create_dir(&directory).unwrap();
        let safe = Record::new(EventCode::OperationFailed, Some(FailureReason::InvalidData))
            .bytes()
            .unwrap();
        fs::write(directory.join(NAMES[0]), &safe).unwrap();
        fs::write(
            directory.join("unmanaged.txt"),
            b"SYNTHETIC_CLIPBOARD_CANARY",
        )
        .unwrap();
        let status = DiagnosticLog::existing_status(&directory).unwrap();
        assert_eq!(status.retained_bytes, safe.len() as u64);
        let (first, second) = tokio::join!(
            DiagnosticLog::export_existing(directory.clone()),
            DiagnosticLog::export_existing(directory.clone())
        );
        let path = first.unwrap();
        assert_eq!(second.unwrap(), path);
        assert_eq!(
            fs::read(directory.join(NAMES[0])).unwrap(),
            safe,
            "Fallback export must not append an export event or mutate existing logs"
        );
        assert_eq!(
            fs::read_dir(&directory).unwrap().count(),
            2,
            "Fallback must not create extra logs"
        );
        let mut archive = zip::ZipArchive::new(File::open(path).unwrap()).unwrap();
        assert_eq!(archive.len(), 2);
        let mut text = String::new();
        archive
            .by_name("summary.json")
            .unwrap()
            .read_to_string(&mut text)
            .unwrap();
        let summary: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(summary["engine"]["engine_available"], false);
        assert!(!text.contains("SYNTHETIC_CLIPBOARD_CANARY"));
    }
    #[tokio::test]
    async fn startup_export_rejects_oversized_managed_logs_without_retaining_temporary_output() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("logs");
        fs::create_dir(&directory).unwrap();
        File::create(directory.join(NAMES[0]))
            .unwrap()
            .set_len(MAX_FILE_BYTES + 1)
            .unwrap();
        assert!(DiagnosticLog::existing_status(&directory).is_err());
        assert!(DiagnosticLog::export_existing(directory).await.is_err());
        assert!(!root
            .path()
            .join("diagnostic-exports/.emc-diagnostics.tmp")
            .exists());
        assert!(!root
            .path()
            .join("diagnostic-exports/emc-diagnostics.zip")
            .exists());
    }
    #[test]
    fn rotation_keeps_exactly_three_bounded_files_and_the_latest_records() {
        let root = tempfile::tempdir().unwrap();
        let metrics = Arc::new(Metrics::default());
        let mut writer = RotatingWriter::open(root.path().into(), 512, metrics.clone()).unwrap();
        for n in 0..100 {
            let mut record = Record::new(EventCode::PeerConnected, None);
            record.timestamp_unix_ms = n;
            writer.append(&record).unwrap();
        }
        writer.file.as_ref().unwrap().sync_all().unwrap();
        let entries: Vec<_> = fs::read_dir(root.path()).unwrap().collect();
        assert_eq!(entries.len(), 3);
        let mut total = 0;
        for name in NAMES {
            let path = root.path().join(name);
            let size = fs::metadata(&path).unwrap().len();
            assert!(size <= 512);
            total += size;
            for line in fs::read_to_string(path).unwrap().lines() {
                assert!(sanitize_record(line).is_some());
            }
        }
        assert_eq!(metrics.retained.load(Ordering::Relaxed), total);
        assert!(total <= 3 * 512);
        assert!(fs::read_to_string(root.path().join(NAMES[0]))
            .unwrap()
            .contains("\"timestamp_unix_ms\":99"));
    }
    #[tokio::test]
    async fn explicit_export_contains_only_safe_records_and_one_latest_archive() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("logs");
        let logs = DiagnosticLog::open(directory.clone()).unwrap();
        logs.record(EventCode::EngineStarted);
        logs.record_failure(EventCode::ReconnectFailed, FailureReason::ConnectionRefused);
        logs.flush_timeout(Duration::from_secs(2));
        // Simulate sensitive extra fields or hostile raw text in an edited log.
        let mut file = OpenOptions::new()
            .append(true)
            .open(directory.join(NAMES[0]))
            .unwrap();
        writeln!(file,"{}",serde_json::json!({"timestamp_unix_ms":1,"event":"peer_connected","raw":"SECRET emc1.key 192.168.1.1 C:\\private\\file.txt","failure":"SECRET"})).unwrap();
        writeln!(file, "RAW_SECRET_CLIPBOARD_TEXT").unwrap();
        drop(file);
        assert!(!root.path().join("diagnostic-exports").exists());
        let path = logs.export(DiagnosticSummary::default()).await.unwrap();
        let mut archive = zip::ZipArchive::new(File::open(&path).unwrap()).unwrap();
        let mut all = String::new();
        for n in 0..archive.len() {
            let mut entry = archive.by_index(n).unwrap();
            assert!(!entry.name().contains("identity"));
            entry.read_to_string(&mut all).unwrap();
        }
        assert!(!all.contains("SECRET"));
        assert!(!all.contains("192.168.1.1"));
        assert!(!all.contains("private"));
        assert!(!all.contains("file.txt"));
        assert!(all.contains("connection_refused"));
        drop(archive);
        assert_eq!(
            logs.export(DiagnosticSummary::default()).await.unwrap(),
            path
        );
        assert_eq!(
            fs::read_dir(root.path().join("diagnostic-exports"))
                .unwrap()
                .count(),
            1
        );
    }
    #[test]
    fn failure_categories_never_include_error_messages() {
        let e = anyhow::Error::new(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "private password, IP and file path",
        ));
        let reason = FailureReason::from_error(&e);
        assert_eq!(reason, FailureReason::PermissionDenied);
        let record = Record::new(EventCode::OperationFailed, Some(reason));
        let json = String::from_utf8(record.bytes().unwrap()).unwrap();
        assert!(!json.contains("password"));
        assert!(!json.contains("file path"));
    }
    #[test]
    fn full_queue_drops_events_instead_of_blocking_input() {
        let root = tempfile::tempdir().unwrap();
        let (tx, _rx) = mpsc::sync_channel(1);
        let metrics = Arc::new(Metrics::default());
        let logs = DiagnosticLog(Arc::new(Handle {
            sender: tx,
            metrics,
            directory: root.path().into(),
        }));
        logs.record(EventCode::EngineStarted);
        logs.record(EventCode::PeerConnected);
        assert_eq!(logs.status().dropped_events, 1);
    }
}

#[cfg(test)]
mod input_diagnostic_tests {
    use super::*;
    use crate::input_pipeline::{Counters, StaleStage};

    #[tokio::test]
    async fn input_stage_export_preserves_existing_fields_and_allowlists_typed_failures() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("logs");
        let logs = DiagnosticLog::open(directory.clone()).unwrap();
        for reason in [
            FailureReason::NativePreconditionUnavailable,
            FailureReason::NativeCursorUnavailable,
            FailureReason::NativeCommandQueueUnavailable,
        ] {
            logs.record_failure(EventCode::InputEntryFailed, reason);
        }
        logs.flush_timeout(Duration::from_secs(1));
        // An edited known event cannot smuggle content into the exported record.
        let mut file = OpenOptions::new()
            .append(true)
            .open(directory.join("emc.log"))
            .unwrap();
        writeln!(file,"{}",serde_json::json!({"timestamp_unix_ms":1,"event":"input_entry_failed","failure":"native_cursor_unavailable","raw":"SYNTHETIC_INPUT_CONTENT_CANARY","key":65,"address":"192.0.2.19"})).unwrap();
        drop(file);
        let counters = Counters::default();
        for stage in [
            StaleStage::OutgoingQueue,
            StaleStage::OutgoingWrite,
            StaleStage::IncomingQueue,
            StaleStage::NativeCompletion,
        ] {
            counters.stale(stage);
        }
        let summary = DiagnosticSummary {
            engine_available: true,
            control_diagnostics: counters.snapshot(Some(7)),
            input: InputDiagnostics {
                source_edge_samples: 11,
                source_edge_candidates: 12,
                source_edge_intent_ready: 13,
                source_enter_committed: 14,
                remote_enter_committed: 15,
                remote_edge_returns: 16,
                captured_wheels: 17,
                injected_wheels: 18,
                native_sendinput_calls: 19,
                native_sendinput_total_us: 20,
                native_sendinput_peak_us: 21,
                native_wheel_peak_us: 22,
                ..InputDiagnostics::default()
            },
            ..DiagnosticSummary::default()
        };
        let path = logs.export(summary).await.unwrap();
        let mut archive = zip::ZipArchive::new(File::open(path).unwrap()).unwrap();
        let mut summary_text = String::new();
        archive
            .by_name("summary.json")
            .unwrap()
            .read_to_string(&mut summary_text)
            .unwrap();
        let summary: serde_json::Value = serde_json::from_str(&summary_text).unwrap();
        let input = &summary["engine"]["control_diagnostics"];
        assert_eq!(input["stale_input_failures"], 4);
        for field in [
            "outgoing_queue_stale_failures",
            "outgoing_write_deadline_failures",
            "incoming_queue_stale_failures",
            "native_completion_deadline_failures",
        ] {
            assert_eq!(input[field], 1);
        }
        for field in [
            "outgoing_queue_peak",
            "incoming_queue_peak",
            "outgoing_moves_coalesced",
            "incoming_moves_coalesced",
            "max_outgoing_queue_age_ms",
            "max_incoming_queue_age_ms",
            "max_native_apply_ms",
            "input_queue_overflows",
        ] {
            assert_eq!(
                input[field], 0,
                "Existing fields must remain present and numeric"
            );
        }
        assert_eq!(input["round_trip_sample_age_ms"], 7);
        for (field, expected) in [
            ("source_edge_samples", 11),
            ("source_edge_candidates", 12),
            ("source_edge_intent_ready", 13),
            ("source_enter_committed", 14),
            ("remote_enter_committed", 15),
            ("remote_edge_returns", 16),
            ("captured_wheels", 17),
            ("injected_wheels", 18),
            ("native_sendinput_calls", 19),
            ("native_sendinput_total_us", 20),
            ("native_sendinput_peak_us", 21),
            ("native_wheel_peak_us", 22),
        ] {
            assert_eq!(summary["engine"]["input"][field], expected);
            assert!(summary["engine"]["input"][field].is_u64());
        }
        let mut records = String::new();
        archive
            .by_name("logs/emc.log")
            .unwrap()
            .read_to_string(&mut records)
            .unwrap();
        for expected in [
            "input_entry_failed",
            "native_precondition_unavailable",
            "native_cursor_unavailable",
            "native_command_queue_unavailable",
        ] {
            assert!(records.contains(expected));
        }
        assert!(!records.contains("SYNTHETIC_INPUT_CONTENT_CANARY"));
        assert!(!records.contains("192.0.2.19"));
        for line in records.lines() {
            let record: serde_json::Value = serde_json::from_str(line).unwrap();
            assert!(record.get("raw").is_none());
            assert!(record.get("key").is_none());
            assert!(record.get("address").is_none());
            assert!(record.get("message").is_none());
        }
    }
}

#[cfg(test)]
mod native_failure_tests {
    use super::*;
    #[test]
    fn native_categories_work_as_context_without_parsing_sensitive_messages() {
        for (native, expected) in [
            (
                emc_platform::NativeInputFailure::PreconditionUnavailable,
                FailureReason::NativePreconditionUnavailable,
            ),
            (
                emc_platform::NativeInputFailure::CursorUnavailable,
                FailureReason::NativeCursorUnavailable,
            ),
            (
                emc_platform::NativeInputFailure::CommandQueueUnavailable,
                FailureReason::NativeCommandQueueUnavailable,
            ),
            (
                emc_platform::NativeInputFailure::StaleCommand,
                FailureReason::StaleInput,
            ),
        ] {
            let error = anyhow::anyhow!("SYNTHETIC_NATIVE_PRIVATE_CANARY").context(native);
            assert_eq!(FailureReason::from_error(&error), expected);
            let record = Record::new(
                EventCode::InputEntryFailed,
                Some(FailureReason::from_error(&error)),
            );
            let record = String::from_utf8(record.bytes().unwrap()).unwrap();
            assert!(!record.contains("SYNTHETIC_NATIVE_PRIVATE_CANARY"));
            assert!(sanitize_record(&record).is_some());
        }
        let send_input_error =
            anyhow::Error::new(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
                .context("Native insertion rejected");
        assert_eq!(
            FailureReason::from_error(&send_input_error),
            FailureReason::PermissionDenied,
            "An actual OS insertion error retains its typed IO category"
        );
    }
}

#[cfg(test)]
mod provenance_tests {
    use super::*;

    fn event(timestamp: u64, code: &str, version: Option<serde_json::Value>) -> String {
        let mut value = serde_json::json!({
            "schema":1,"timestamp_unix_ms":timestamp,"event":code,
            "raw":"SYNTHETIC_PROVENANCE_PRIVATE_CANARY",
            "path":"SYNTHETIC_PRIVATE_PATH_CANARY"
        });
        if let Some(version) = version {
            value["app_version"] = version;
        }
        format!("{value}\n")
    }

    fn read_export(path: &Path) -> (serde_json::Value, Vec<serde_json::Value>, String) {
        let mut archive = zip::ZipArchive::new(File::open(path).unwrap()).unwrap();
        let mut text = String::new();
        archive
            .by_name("summary.json")
            .unwrap()
            .read_to_string(&mut text)
            .unwrap();
        let summary = serde_json::from_str(&text).unwrap();
        let mut records = Vec::new();
        let mut all = text;
        for name in NAMES.iter().rev() {
            if let Ok(mut file) = archive.by_name(&format!("logs/{name}")) {
                let mut text = String::new();
                file.read_to_string(&mut text).unwrap();
                records.extend(
                    text.lines()
                        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap()),
                );
                all.push_str(&text);
            }
        }
        (summary, records, all)
    }

    #[test]
    fn historical_version_accepts_only_bounded_canonical_numeric_release_triplets() {
        for version in ["0.0.0", "0.3.2", "12.34.56", "65535.65535.65535"] {
            let sanitized =
                sanitize_record(&event(1, "engine_started", Some(version.into()))).unwrap();
            let value: serde_json::Value = serde_json::from_slice(&sanitized.bytes).unwrap();
            assert_eq!(value["app_version"], version);
            assert_eq!(
                sanitized.engine_start.unwrap().app_version.unwrap(),
                version
            );
        }
        for version in [
            "",
            "0.3",
            "0.3.2.1",
            "v0.3.2",
            "0.3.2-preview",
            "01.2.3",
            "1.02.3",
            "1.2.03",
            "65536.0.0",
            "0.0.65536",
            "0.+2.3",
            "0.-2.3",
            "0.3.2\nPRIVATE_VERSION_CANARY",
            "１２.3.4",
            "1.2.3\u{200b}",
            "1.2.3\r",
            "0000000000000000000",
        ] {
            let sanitized =
                sanitize_record(&event(1, "engine_started", Some(version.into()))).unwrap();
            let value: serde_json::Value = serde_json::from_slice(&sanitized.bytes).unwrap();
            assert!(value.get("app_version").is_none(), "Accepted {version:?}");
            assert!(sanitized.engine_start.unwrap().app_version.is_none());
        }
        for version in [None, Some(3.into()), Some(serde_json::Value::Null)] {
            let sanitized = sanitize_record(&event(1, "engine_started", version)).unwrap();
            let value: serde_json::Value = serde_json::from_slice(&sanitized.bytes).unwrap();
            assert!(value.get("app_version").is_none());
            assert!(sanitized.engine_start.unwrap().app_version.is_none());
        }
    }

    #[tokio::test]
    async fn mixed_version_export_preserves_provenance_and_uses_append_order_after_clock_rollback()
    {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("logs");
        fs::create_dir(&directory).unwrap();
        fs::write(
            directory.join(NAMES[2]),
            event(9_000, "engine_started", Some("0.2.0".into()))
                + &event(9_001, "peer_disconnected", Some("0.2.0".into())),
        )
        .unwrap();
        fs::write(
            directory.join(NAMES[1]),
            event(1_000, "engine_started", Some("0.3.2".into()))
                + &event(1_001, "operation_failed", None),
        )
        .unwrap();
        fs::write(
            directory.join(NAMES[0]),
            event(200, "engine_started", Some("0.3.3".into()))
                + &event(
                    201,
                    "operation_failed",
                    Some("PRIVATE_VERSION_CANARY".into()),
                ),
        )
        .unwrap();
        let originals: Vec<_> = NAMES
            .iter()
            .map(|name| fs::read(directory.join(name)).unwrap())
            .collect();
        let path = DiagnosticLog::export_existing(directory.clone())
            .await
            .unwrap();
        let (summary, records, all) = read_export(&path);
        assert_eq!(summary["app_version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(summary["logger_opened_at_unix_ms"], serde_json::Value::Null);
        assert_eq!(
            summary["latest_retained_engine_start"],
            serde_json::json!({"timestamp_unix_ms":200,"app_version":"0.3.3"})
        );
        assert_eq!(summary["engine"]["engine_available"], false);
        assert_eq!(records.len(), 6);
        assert_eq!(records[0]["app_version"], "0.2.0");
        assert_eq!(records[1]["app_version"], "0.2.0");
        assert_eq!(records[2]["app_version"], "0.3.2");
        assert!(records[3].get("app_version").is_none());
        assert_eq!(records[4]["app_version"], "0.3.3");
        assert!(records[5].get("app_version").is_none());
        for canary in [
            "SYNTHETIC_PROVENANCE_PRIVATE_CANARY",
            "SYNTHETIC_PRIVATE_PATH_CANARY",
            "PRIVATE_VERSION_CANARY",
        ] {
            assert!(!all.contains(canary));
        }
        for (name, original) in NAMES.iter().zip(originals) {
            assert_eq!(fs::read(directory.join(name)).unwrap(), original);
        }
    }

    #[tokio::test]
    async fn active_logger_metadata_and_export_marker_do_not_relabel_retained_history() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("logs");
        fs::create_dir(&directory).unwrap();
        fs::write(
            directory.join(NAMES[0]),
            event(1, "engine_started", Some("0.3.2".into())),
        )
        .unwrap();
        let logs = DiagnosticLog::open(directory).unwrap();
        logs.record(EventCode::EngineStarted);
        logs.record(EventCode::PeerConnected);
        let path = logs
            .export(DiagnosticSummary {
                engine_available: true,
                ..DiagnosticSummary::default()
            })
            .await
            .unwrap();
        let (summary, records, _) = read_export(&path);
        assert!(summary["logger_opened_at_unix_ms"].is_u64());
        assert_eq!(records[0]["app_version"], "0.3.2");
        assert_eq!(records[1]["event"], "engine_started");
        assert_eq!(records[1]["app_version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(
            summary["latest_retained_engine_start"],
            serde_json::json!({
                "timestamp_unix_ms":records[1]["timestamp_unix_ms"],
                "app_version":env!("CARGO_PKG_VERSION")
            })
        );
        assert_eq!(records.last().unwrap()["event"], "diagnostics_exported");
        assert_eq!(summary["engine"]["engine_available"], true);
    }

    #[tokio::test]
    async fn missing_retained_start_or_version_is_never_repaired_from_the_exporting_build() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("logs");
        fs::create_dir(&directory).unwrap();
        fs::write(
            directory.join(NAMES[0]),
            event(10, "operation_failed", None),
        )
        .unwrap();
        let path = DiagnosticLog::export_existing(directory.clone())
            .await
            .unwrap();
        let (summary, records, _) = read_export(&path);
        assert!(summary["latest_retained_engine_start"].is_null());
        assert!(records[0].get("app_version").is_none());
        fs::write(directory.join(NAMES[0]), event(11, "engine_started", None)).unwrap();
        let path = DiagnosticLog::export_existing(directory).await.unwrap();
        let (summary, records, _) = read_export(&path);
        assert_eq!(
            summary["latest_retained_engine_start"],
            serde_json::json!({"timestamp_unix_ms":11})
        );
        assert!(records[0].get("app_version").is_none());
    }
}

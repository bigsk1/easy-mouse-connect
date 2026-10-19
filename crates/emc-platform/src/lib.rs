//! Native input, clipboard, and local secret protection.
//!
//! Windows is the supported interactive platform. Other platforms can build the
//! protocol/core and run tests, but do not install hooks or inject input.

pub mod clipboard_image;

#[cfg(not(windows))]
use anyhow::Result;

pub const MAX_CLIPBOARD_BYTES: usize = 1024 * 1024;

pub mod awake;

/// Content-free native failure categories. OS injection errors retain their
/// typed std::io::Error sources instead of being replaced with these categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeInputFailure {
    PreconditionUnavailable,
    CursorUnavailable,
    CommandQueueUnavailable,
    StaleCommand,
}
impl std::fmt::Display for NativeInputFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::PreconditionUnavailable => "Native input precondition unavailable",
            Self::CursorUnavailable => "Receiving cursor unavailable",
            Self::CommandQueueUnavailable => "Native input command completion unavailable",
            Self::StaleCommand => "Native input exceeded its local processing deadline",
        })
    }
}
impl std::error::Error for NativeInputFailure {}

/// Lifetime aggregate counters only. No keys, event coordinates, device names,
/// clipboard contents, or input history are retained in this snapshot.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct InputDiagnostics {
    pub captured_wheels: u64,
    pub injected_wheels: u64,
    pub native_sendinput_calls: u64,
    pub native_sendinput_total_us: u64,
    pub native_sendinput_peak_us: u64,
    pub native_wheel_peak_us: u64,
    pub source_edge_samples: u64,
    pub source_edge_candidates: u64,
    pub source_edge_intent_ready: u64,
    pub source_enter_committed: u64,
    pub remote_enter_committed: u64,
    pub remote_edge_returns: u64,

    pub native_queue_wait_us: u64,
    pub native_queue_wait_peak_us: u64,
    pub native_execution_us: u64,
    pub native_execution_peak_us: u64,
    pub native_commands: u64,
    pub native_pump_slices: u64,
    pub native_pump_yields: u64,

    pub mouse_hook_installed: bool,
    pub keyboard_hook_installed: bool,
    pub hook_delivery_stalls: u64,
    pub hook_recoveries: u64,
    pub hook_recovery_failures: u64,
    pub raw_registration_repairs: u64,
    pub raw_registration_failures: u64,
    pub raw_registration_query_failures: u64,
    pub raw_registration_mismatches: u64,
    pub device_removals_ignored: u64,
    pub physical_mouse_reclaims: u64,
    pub mouse_jitter_events_ignored: u64,
    pub protected_physical_keys: u32,
    pub protected_physical_buttons: u32,
    pub held_input_recovery_required: bool,
    pub cursor_overlay_recoveries: u64,

    pub raw_input_registered: bool,
    pub per_monitor_dpi: bool,
    pub sending: bool,
    pub receiving: bool,
    pub cursor_overlay_visible: bool,
    pub monitor_count: u32,
    pub virtual_width: u32,
    pub virtual_height: u32,
    pub raw_motion_packets: u64,
    pub raw_absolute_packets: u64,
    pub captured_moves: u64,
    pub captured_buttons: u64,
    pub captured_keys: u64,
    pub hook_key_events: u64,
    pub handoff_blocked_by_held: u64,
    pub stale_keys_reconciled: u64,
    pub stale_buttons_reconciled: u64,
    pub held_physical_keys: u32,
    pub held_physical_buttons: u32,
    pub device_input_resets: u64,
    pub native_key_reclaims: u64,
    pub key_up_reclaims_ignored: u64,
    pub ignored_remote_inputs: u64,
    pub injected_moves: u64,
    pub injected_buttons: u64,
    pub injected_keys: u64,
    pub injection_failures: u64,
    pub queue_overflows: u64,
    pub capture_resets: u64,
    pub raw_capture_stalls: u64,
    pub cursor_overlay_activations: u64,
    pub cursor_overlay_shape_changes: u64,
    pub cursor_overlay_work_us: u64,
    pub cursor_overlay_failures: u64,
}

#[cfg(any(windows, test))]
mod cursor_model;
#[cfg(any(windows, test))]
mod geometry;
#[cfg(any(windows, test))]
mod handoff;
#[cfg(any(windows, test))]
mod injection_state;
#[cfg(any(windows, test))]
mod input_commit;
#[cfg(any(windows, test))]
mod input_pump;
#[cfg(any(windows, test))]
mod keyboard_state;
#[cfg(any(windows, test))]
mod motion;
#[cfg(any(windows, test))]
mod native_health;
#[cfg(any(windows, test))]
mod raw_health;
#[cfg(any(windows, test))]
mod raw_registration;

/// Native capture is scoped to the connection that was active when captured.
/// The transport must drop this event if its epoch is no longer current.
#[derive(Debug, Clone)]
pub struct CapturedInput {
    pub epoch: u64,
    /// Local monotonic capture time, including time spent waiting in queues.
    pub captured_at: std::time::Instant,
    pub event: emc_protocol::InputEvent,
}

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::{
    clipboard_sequence, protect_secret, read_clipboard, unprotect_secret, write_clipboard,
    InputController,
};

pub const fn native_supported() -> bool {
    cfg!(windows)
}

pub const fn platform_name() -> &'static str {
    if cfg!(windows) {
        "Windows"
    } else if cfg!(target_os = "linux") {
        "Linux (transport and UI preview only)"
    } else {
        "Unsupported platform"
    }
}

#[cfg(not(windows))]
mod unsupported {
    use super::*;
    use emc_protocol::InputEvent;
    use std::sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    };

    #[derive(Clone, Default)]
    pub struct InputController {
        remote: Arc<AtomicBool>,
        epoch: Arc<AtomicU64>,
    }

    impl InputController {
        pub fn start(_: tokio::sync::mpsc::Sender<CapturedInput>) -> Result<Self> {
            Ok(Self::default())
        }

        pub fn configure(&self, enabled: bool, _: bool, _: bool) {
            if !enabled {
                self.epoch.fetch_add(1, Ordering::AcqRel);
                self.release_all();
            }
        }

        pub fn set_connected(&self, connected: bool) {
            self.epoch.fetch_add(1, Ordering::AcqRel);
            if !connected {
                self.release_all();
            }
        }

        pub fn set_remote(&self, _: bool) {
            self.remote.store(false, Ordering::Release);
        }

        pub fn is_remote(&self) -> bool {
            self.remote.load(Ordering::Acquire)
        }

        pub fn connection_epoch(&self) -> u64 {
            self.epoch.load(Ordering::Acquire)
        }

        pub fn diagnostics(&self) -> InputDiagnostics {
            InputDiagnostics::default()
        }

        pub fn release_all(&self) {
            self.remote.store(false, Ordering::Release);
        }

        pub fn apply_remote(&self, _: &InputEvent) -> Result<()> {
            anyhow::bail!("Native input is currently supported only on Windows")
        }

        pub async fn apply_remote_async(&self, event: InputEvent) -> Result<()> {
            self.apply_remote_for_epoch_async(event, self.connection_epoch())
                .await
        }

        pub async fn apply_remote_for_epoch_async(
            &self,
            event: InputEvent,
            epoch: u64,
        ) -> Result<()> {
            self.apply_remote_for_epoch_before_async(
                event,
                epoch,
                std::time::Instant::now() + std::time::Duration::from_millis(200),
            )
            .await
        }

        pub async fn apply_remote_for_epoch_before_async(
            &self,
            event: InputEvent,
            epoch: u64,
            deadline: std::time::Instant,
        ) -> Result<()> {
            if !matches!(event, InputEvent::Leave | InputEvent::ReleaseAll)
                && std::time::Instant::now() >= deadline
            {
                return Err(super::NativeInputFailure::StaleCommand.into());
            }
            anyhow::ensure!(
                epoch == self.connection_epoch(),
                super::NativeInputFailure::PreconditionUnavailable
            );
            self.apply_remote(&event)
        }
    }

    pub fn read_clipboard() -> Result<Option<String>> {
        Ok(None)
    }

    pub fn write_clipboard(_: &str) -> Result<()> {
        anyhow::bail!("Native clipboard sharing is currently supported only on Windows")
    }

    pub fn clipboard_sequence() -> u32 {
        0
    }

    // The caller must enforce private file permissions on other platforms. These
    // helpers deliberately do not pretend that DPAPI exists outside Windows.
    pub fn protect_secret(bytes: &[u8]) -> Result<Vec<u8>> {
        Ok(bytes.to_vec())
    }

    pub fn unprotect_secret(bytes: &[u8]) -> Result<Vec<u8>> {
        Ok(bytes.to_vec())
    }
}

#[cfg(not(windows))]
pub use unsupported::*;

/// Pixel bounds of the entire Windows virtual desktop. Right/bottom are exclusive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg(any(windows, test))]
struct Desktop {
    left: i32,
    top: i32,
    width: i32,
    height: i32,
}

#[cfg(any(windows, test))]
impl Desktop {
    fn right(self) -> i32 {
        self.left.saturating_add(self.width.max(1))
    }

    fn bottom(self) -> i32 {
        self.top.saturating_add(self.height.max(1))
    }

    fn normalize_y(self, y: i32) -> u16 {
        let span = i64::from((self.height - 1).max(1));
        ((i64::from(y).saturating_sub(i64::from(self.top))).clamp(0, span) * 65535 / span) as u16
    }

    fn denormalize_y(self, y: u16) -> i32 {
        self.top + (i64::from(y) * i64::from((self.height - 1).max(0)) / 65535) as i32
    }

    fn edge_x(self, left: bool, inset: i32) -> i32 {
        let inset = inset.clamp(0, (self.width - 1).max(0));
        if left {
            self.left + inset
        } else {
            self.right() - 1 - inset
        }
    }

    #[cfg(test)]
    fn on_edge(self, x: i32, left: bool) -> bool {
        if left {
            x <= self.left
        } else {
            x >= self.right() - 1
        }
    }

    fn clamp(self, x: i32, y: i32) -> (i32, i32) {
        (
            x.clamp(self.left, self.right() - 1),
            y.clamp(self.top, self.bottom() - 1),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn height_mapping_handles_negative_monitor_coordinates() {
        let desktop = Desktop {
            left: -1920,
            top: -1080,
            width: 3840,
            height: 2160,
        };
        assert_eq!(desktop.normalize_y(-1080), 0);
        assert_eq!(desktop.normalize_y(1079), u16::MAX);
        assert_eq!(desktop.denormalize_y(0), -1080);
        assert_eq!(desktop.denormalize_y(u16::MAX), 1079);
        assert_eq!(desktop.normalize_y(i32::MIN), 0);
        assert_eq!(desktop.normalize_y(i32::MAX), u16::MAX);
    }

    #[test]
    fn edge_switches_use_virtual_desktop_outer_edges() {
        let desktop = Desktop {
            left: -1920,
            top: 0,
            width: 3840,
            height: 1080,
        };
        assert!(desktop.on_edge(-1920, true));
        assert!(!desktop.on_edge(0, true));
        assert!(desktop.on_edge(1919, false));
        assert!(!desktop.on_edge(1918, false));
        assert_eq!(desktop.edge_x(true, 8), -1912);
        assert_eq!(desktop.edge_x(false, 8), 1911);
        assert_eq!(desktop.clamp(i32::MIN, i32::MAX), (-1920, 1079));
    }

    #[test]
    fn one_pixel_desktop_mapping_is_defined() {
        let desktop = Desktop {
            left: 0,
            top: 4,
            width: 1,
            height: 1,
        };
        assert_eq!(desktop.normalize_y(4), 0);
        assert_eq!(desktop.denormalize_y(u16::MAX), 4);
        assert_eq!(desktop.edge_x(false, 8), 0);
        assert_eq!(desktop.clamp(40, -9), (0, 4));
    }
}

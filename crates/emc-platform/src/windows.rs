use super::{
    geometry::MonitorLayout,
    handoff::{ReceiverReturn, SourceHandoff},
    injection_state::key_admissible,
    input_commit::check_deadline,
    input_pump::{WakeGate, WorkSlice},
    keyboard_state::{normalize_raw_key, LocalInputState},
    motion::MotionBatch,
    native_health::{DeviceLedger, HookDelivery, ReclaimMotion},
    raw_health::RawCaptureHealth,
    raw_registration::{
        inspect_registration, RegistrationAudit, RegistrationEntry, RegistrationStatus,
    },
    CapturedInput, Desktop, InputDiagnostics, NativeInputFailure, MAX_CLIPBOARD_BYTES,
};
use anyhow::{anyhow, bail, Context, Result};
use emc_protocol::{InputEvent, MouseButton};
use std::{
    cell::{Cell, RefCell},
    collections::BTreeSet,
    mem::{size_of, zeroed},
    ptr::{null, null_mut},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::{
        GetLastError, GlobalFree, LocalFree, SetLastError, HWND, LPARAM, LRESULT, POINT, RECT,
        WPARAM,
    },
    Graphics::Gdi::{EnumDisplayMonitors, HDC, HMONITOR},
    Security::Cryptography::{
        CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    },
    System::{
        DataExchange::{
            CloseClipboard, EmptyClipboard, GetClipboardData, GetClipboardSequenceNumber,
            IsClipboardFormatAvailable, OpenClipboard, SetClipboardData,
        },
        LibraryLoader::GetModuleHandleW,
        Memory::{GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE},
        StationsAndDesktops::*,
        Threading::GetCurrentThreadId,
    },
    UI::{
        HiDpi::*,
        Input::{KeyboardAndMouse::*, *},
        WindowsAndMessaging::*,
    },
};

mod cursor;
mod diagnostics;
mod injection;
use cursor::CursorOverlay;
use diagnostics::Counters;
use injection::{Effect, Executor, Owner};

const WAKE: u32 = WM_APP + 41;
const INJECTED_TAG: usize = 0x454d_4301;
const CF_UNICODE_TEXT: u32 = 13;

fn post_coalesced_wake(gate: &WakeGate, thread_id: u32) -> Result<()> {
    if gate.request() && unsafe { PostThreadMessageW(thread_id, WAKE, 0, 0) } == 0 {
        gate.received();
        return Err(anyhow!(NativeInputFailure::CommandQueueUnavailable));
    }
    Ok(())
}
fn post_input_wake(shared: &Shared, thread_id: u32) -> Result<()> {
    post_coalesced_wake(&shared.wake_gate, thread_id)
}

/// GetMessage prioritizes posted traffic over input/timers. Explicitly service
/// a bounded input slice between command slices, never while HOOK is borrowed.
/// https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-getmessage
unsafe fn pump_window_messages(window: HWND, first: u32, last: u32) -> (bool, usize) {
    let mut message: MSG = zeroed();
    let mut slice = WorkSlice::new(Instant::now());
    let mut count = 0;
    while PeekMessageW(&mut message, window, first, last, PM_REMOVE) != 0 {
        if message.message == WM_QUIT {
            return (false, count);
        }
        dispatch_stamped_message(&message);
        count += 1;
        if slice.complete(Instant::now()) {
            break;
        }
    }
    (true, count)
}

thread_local! { static DISPATCH_TIME:Cell<Option<u32>>=const {Cell::new(None)}; }
unsafe fn dispatch_stamped_message(message: &MSG) {
    DISPATCH_TIME.with(|time| {
        let previous = time.replace(Some(message.time));
        TranslateMessage(message);
        DispatchMessageW(message);
        time.set(previous);
    });
}
unsafe fn dispatch_then_pump(message: &MSG, window: HWND, first: u32, last: u32) -> (bool, usize) {
    dispatch_stamped_message(message);
    pump_window_messages(window, first, last)
}

struct Shared {
    events: tokio::sync::mpsc::Sender<CapturedInput>,
    enabled: AtomicBool,
    edge_switching: AtomicBool,
    peer_on_left: AtomicBool,
    connected: AtomicBool,
    remote: AtomicBool,
    force_release: AtomicBool,
    reset_queued: AtomicU64,
    epoch: AtomicU64,
    input_desktop_available: AtomicBool,
    physical_input_serial: AtomicU64,
    wake_gate: WakeGate,
    stats: Counters,
}

impl Shared {
    fn ready(&self) -> bool {
        self.enabled.load(Ordering::Acquire) && self.connected.load(Ordering::Acquire)
    }

    fn capture(self: &Arc<Self>, event: InputEvent) -> bool {
        if !self.ready() || !self.remote.load(Ordering::Acquire) {
            return false;
        }
        let counter = match &event {
            InputEvent::Move { .. } => Some(&self.stats.captured_moves),
            InputEvent::Button { .. } => Some(&self.stats.captured_buttons),
            InputEvent::Key { .. } => Some(&self.stats.captured_keys),
            InputEvent::Wheel { .. } => Some(&self.stats.captured_wheels),
            _ => None,
        };
        if !self.emit(event) {
            return false;
        }
        if let Some(counter) = counter {
            counter.fetch_add(1, Ordering::Relaxed);
        }
        true
    }

    /// Hook callbacks must never wait for the network or a congested queue.
    fn emit(self: &Arc<Self>, event: InputEvent) -> bool {
        let epoch = self.epoch.load(Ordering::Acquire);
        self.emit_for_epoch(event, epoch)
    }

    /// Keep preparation-bound events tagged with their original authority even
    /// if connection replacement races the final enqueue. Core drops old tags.
    fn emit_for_epoch(self: &Arc<Self>, event: InputEvent, epoch: u64) -> bool {
        if self.epoch.load(Ordering::Acquire) != epoch {
            return false;
        }
        let captured_at = Instant::now();
        if self
            .events
            .try_send(CapturedInput {
                epoch,
                captured_at,
                event,
            })
            .is_ok()
        {
            return true;
        }
        self.remote.store(false, Ordering::Release);
        self.stats.queue_overflows.fetch_add(1, Ordering::Relaxed);
        self.force_release.store(true, Ordering::Release);
        self.queue_reset_for_epoch(epoch);
        false
    }

    fn queue_reset(self: &Arc<Self>) {
        let epoch = self.epoch.load(Ordering::Acquire);
        self.queue_reset_for_epoch(epoch);
    }

    fn queue_reset_for_epoch(self: &Arc<Self>, epoch: u64) {
        if self.epoch.load(Ordering::Acquire) != epoch {
            return;
        }
        let captured_at = Instant::now();
        let marker = epoch.saturating_add(1);
        let mut queued = self.reset_queued.load(Ordering::Acquire);
        loop {
            if queued >= marker {
                return;
            }
            match self.reset_queued.compare_exchange_weak(
                queued,
                marker,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(actual) => queued = actual,
            }
        }
        let shared = self.clone();
        // Failure is rare, and this worker preserves ordered release messages
        // after all already accepted inputs without blocking the hook thread.
        thread::spawn(move || {
            let _ = shared.events.blocking_send(CapturedInput {
                epoch,
                captured_at,
                event: InputEvent::ReleaseAll,
            });
            let _ = shared.events.blocking_send(CapturedInput {
                epoch,
                captured_at,
                event: InputEvent::Leave,
            });
            // An old worker must not clear a newer session's pending reset.
            let _ = shared.reset_queued.compare_exchange(
                marker,
                0,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
        });
    }
}

enum Completion {
    Blocking(SyncSender<Result<()>>),
    Async(tokio::sync::oneshot::Sender<Result<()>>),
}

impl Completion {
    fn abandoned(&self) -> bool {
        matches!(self,Self::Async(sender) if sender.is_closed())
    }
    fn send(self, result: Result<()>) -> bool {
        match self {
            Self::Blocking(sender) => sender.send(result).is_ok(),
            Self::Async(sender) => sender.send(result).is_ok(),
        }
    }
}

enum Command {
    Apply {
        event: InputEvent,
        epoch: u64,
        queued_at: Instant,
        deadline: Instant,
        done: Completion,
    },
    SetRemote(bool),
    Release,
}

struct ControllerInner {
    shared: Arc<Shared>,
    commands: SyncSender<Command>,
    thread_id: u32,
}

impl Drop for ControllerInner {
    fn drop(&mut self) {
        self.shared.remote.store(false, Ordering::Release);
        self.shared.enabled.store(false, Ordering::Release);
        // The message loop releases injected state and unhooks before exiting.
        unsafe { PostThreadMessageW(self.thread_id, WM_QUIT, 0, 0) };
    }
}

#[derive(Clone)]
pub struct InputController {
    inner: Arc<ControllerInner>,
}

impl InputController {
    pub fn start(events: tokio::sync::mpsc::Sender<CapturedInput>) -> Result<Self> {
        let shared = Arc::new(Shared {
            events,
            enabled: AtomicBool::new(false),
            edge_switching: AtomicBool::new(true),
            peer_on_left: AtomicBool::new(false),
            connected: AtomicBool::new(false),
            remote: AtomicBool::new(false),
            force_release: AtomicBool::new(false),
            reset_queued: AtomicU64::new(0),
            epoch: AtomicU64::new(0),
            input_desktop_available: AtomicBool::new(true),
            physical_input_serial: AtomicU64::new(0),
            wake_gate: WakeGate::default(),
            stats: Counters::default(),
        });
        let (commands, receiver) = mpsc::sync_channel(512);
        let (started, ready) = mpsc::sync_channel(1);
        let hook_shared = shared.clone();
        thread::Builder::new()
            .name("emc-input-hooks".into())
            .spawn(move || hook_loop(hook_shared, receiver, started))
            .context("Start Windows input thread")?;
        let thread_id = ready
            .recv_timeout(Duration::from_secs(5))
            .context("Windows input hook startup timed out")??;
        Ok(Self {
            inner: Arc::new(ControllerInner {
                shared: shared.clone(),
                commands,
                thread_id,
            }),
        })
    }

    pub fn configure(&self, enabled: bool, edge_switching: bool, peer_on_left: bool) {
        self.inner.shared.enabled.store(enabled, Ordering::Release);
        self.inner
            .shared
            .edge_switching
            .store(edge_switching, Ordering::Release);
        self.inner
            .shared
            .peer_on_left
            .store(peer_on_left, Ordering::Release);
        if !enabled {
            self.inner.shared.epoch.fetch_add(1, Ordering::AcqRel);
            self.release_all();
        }
    }

    pub fn set_connected(&self, connected: bool) {
        self.inner.shared.epoch.fetch_add(1, Ordering::AcqRel);
        self.inner
            .shared
            .connected
            .store(connected, Ordering::Release);
        if !connected {
            self.release_all();
        }
    }

    pub fn set_remote(&self, remote: bool) {
        let remote = remote && self.inner.shared.ready();
        if !remote {
            self.release_all();
            return;
        }
        if self
            .inner
            .commands
            .try_send(Command::SetRemote(remote))
            .is_err()
        {
            self.release_all();
        }
        let _ = self.wake();
    }

    pub fn is_remote(&self) -> bool {
        self.inner.shared.remote.load(Ordering::Acquire)
    }

    pub fn connection_epoch(&self) -> u64 {
        self.inner.shared.epoch.load(Ordering::Acquire)
    }

    pub fn diagnostics(&self) -> InputDiagnostics {
        self.inner.shared.stats.snapshot(self.is_remote())
    }

    pub fn release_all(&self) {
        self.inner.shared.remote.store(false, Ordering::Release);
        self.inner
            .shared
            .force_release
            .store(true, Ordering::Release);
        let _ = self.inner.commands.try_send(Command::Release);
        if self.inner.shared.connected.load(Ordering::Acquire) {
            self.inner.shared.queue_reset();
        }
        let _ = self.wake();
    }

    pub fn apply_remote(&self, event: &InputEvent) -> Result<()> {
        let (done, response) = mpsc::sync_channel(1);
        self.enqueue_apply(
            event.clone(),
            self.connection_epoch(),
            Instant::now() + Duration::from_millis(200),
            Completion::Blocking(done),
        )?;
        response
            .recv_timeout(Duration::from_secs(1))
            .context(NativeInputFailure::CommandQueueUnavailable)?
    }

    /// Await hook-thread completion without blocking a Tokio runtime worker.
    pub async fn apply_remote_async(&self, event: InputEvent) -> Result<()> {
        self.apply_remote_for_epoch_async(event, self.connection_epoch())
            .await
    }

    /// Bind incoming input to the epoch captured when its control session began.
    pub async fn apply_remote_for_epoch_async(&self, event: InputEvent, epoch: u64) -> Result<()> {
        self.apply_remote_for_epoch_before_async(
            event,
            epoch,
            Instant::now() + Duration::from_millis(200),
        )
        .await
    }
    pub async fn apply_remote_for_epoch_before_async(
        &self,
        event: InputEvent,
        epoch: u64,
        deadline: Instant,
    ) -> Result<()> {
        let (done, response) = tokio::sync::oneshot::channel();
        let cleanup = matches!(event, InputEvent::Leave | InputEvent::ReleaseAll);
        self.enqueue_apply(event, epoch, deadline, Completion::Async(done))?;
        let wait_deadline = if cleanup {
            Instant::now() + Duration::from_secs(1)
        } else {
            deadline
        };
        let result =
            tokio::time::timeout_at(tokio::time::Instant::from_std(wait_deadline), response)
                .await
                .context(NativeInputFailure::StaleCommand)?
                .context(NativeInputFailure::CommandQueueUnavailable)?;
        if check_deadline(Instant::now(), deadline, cleanup).is_err() {
            return match result {
                Ok(()) => Err(NativeInputFailure::StaleCommand.into()),
                Err(error) => Err(error.context(NativeInputFailure::StaleCommand)),
            };
        }
        result
    }

    fn enqueue_apply(
        &self,
        event: InputEvent,
        epoch: u64,
        deadline: Instant,
        done: Completion,
    ) -> Result<()> {
        if epoch != self.connection_epoch() {
            bail!(NativeInputFailure::PreconditionUnavailable);
        }
        self.inner
            .commands
            .try_send(Command::Apply {
                event,
                epoch,
                queued_at: Instant::now(),
                deadline,
                done,
            })
            .map_err(|_| anyhow!(NativeInputFailure::CommandQueueUnavailable))?;
        self.wake()?;
        Ok(())
    }

    fn wake(&self) -> Result<()> {
        post_input_wake(&self.inner.shared, self.inner.thread_id)
    }
}

struct HookState {
    shared: Arc<Shared>,
    receiving: bool,
    held_keys: BTreeSet<(u16, u16, bool)>,
    held_buttons: [bool; 5],
    physical: LocalInputState,
    return_y: u16,
    edge_ready_at: Instant,
    layout: MonitorLayout,
    overlay: Option<CursorOverlay>,
    raw_health: RawCaptureHealth,
    pending_motion: MotionBatch,
    source_handoff: SourceHandoff,
    receiver_return: ReceiverReturn,
    desktop_name: Vec<u16>,
    next_desktop_check: Instant,
    next_cursor_update: Instant,
    next_physical_check: Instant,
    mouse_delivery: HookDelivery,
    keyboard_delivery: HookDelivery,
    devices: DeviceLedger,
    reclaim_motion: ReclaimMotion,
    reinstall_hooks: bool,
    hooks_available: bool,
    next_hook_retry: Instant,
    raw_window: HWND,
    registration_audit: RegistrationAudit,
    input_desktop_available: bool,
    capture_epoch: u64,
    next_raw_maintenance: Instant,
    injector: Option<Executor>,
    owner: Arc<Owner>,
    pending: Option<PendingApply>,
}

type PreparedApply = (Effect, Option<POINT>, Arc<Owner>);
struct PendingApply {
    event: InputEvent,
    point: Option<POINT>,
    owner: Arc<Owner>,
    epoch: u64,
    deadline: Instant,
    receiver: Receiver<Result<()>>,
    done: Completion,
    started: Instant,
}

thread_local! {
    static HOOK: RefCell<Option<HookState>> = const { RefCell::new(None) };
}

fn desktop() -> Desktop {
    unsafe {
        Desktop {
            left: GetSystemMetrics(SM_XVIRTUALSCREEN),
            top: GetSystemMetrics(SM_YVIRTUALSCREEN),
            width: GetSystemMetrics(SM_CXVIRTUALSCREEN).max(1),
            height: GetSystemMetrics(SM_CYVIRTUALSCREEN).max(1),
        }
    }
}

fn monitor_layout() -> MonitorLayout {
    unsafe extern "system" fn collect(_: HMONITOR, _: HDC, rect: *mut RECT, data: LPARAM) -> i32 {
        if !rect.is_null() {
            let rect = unsafe { *rect };
            let monitors = unsafe { &mut *(data as *mut Vec<Desktop>) };
            monitors.push(Desktop {
                left: rect.left,
                top: rect.top,
                width: rect.right - rect.left,
                height: rect.bottom - rect.top,
            });
        }
        1
    }
    let mut monitors = Vec::new();
    unsafe {
        EnumDisplayMonitors(
            null_mut(),
            null(),
            Some(collect),
            (&mut monitors as *mut Vec<Desktop>) as isize,
        );
    }
    MonitorLayout::new(monitors, desktop())
}

fn desktop_name(handle: HDESK) -> Option<Vec<u16>> {
    let mut name = [0u16; 256];
    let mut needed = 0;
    let ok = unsafe {
        GetUserObjectInformationW(
            handle,
            UOI_NAME,
            name.as_mut_ptr().cast(),
            std::mem::size_of_val(&name) as u32,
            &mut needed,
        )
    };
    if ok == 0 {
        return None;
    }
    Some(name[..name.iter().position(|c| *c == 0).unwrap_or(name.len())].to_vec())
}

fn on_input_desktop(expected: &[u16]) -> bool {
    let input = unsafe { OpenInputDesktop(0, 0, DESKTOP_READOBJECTS) };
    if input.is_null() {
        return false;
    }
    let same = desktop_name(input).as_deref() == Some(expected);
    unsafe {
        CloseDesktop(input);
    }
    same
}

fn current_cursor_position() -> Result<POINT> {
    let mut point = unsafe { zeroed() };
    if unsafe { GetCursorPos(&mut point) } == 0 {
        return Err(last_error("Read source cursor position"));
    }
    Ok(point)
}

unsafe fn raw_window() -> Result<HWND> {
    let name: Vec<u16> = "EMC.RawInput.v2".encode_utf16().chain(Some(0)).collect();
    let instance = GetModuleHandleW(null());
    let definition = WNDCLASSW {
        lpfnWndProc: Some(raw_proc),
        hInstance: instance,
        lpszClassName: name.as_ptr(),
        ..zeroed()
    };
    RegisterClassW(&definition);
    // A hidden top-level window receives display/device changes as well as input.
    let window = CreateWindowExW(
        WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW,
        name.as_ptr(),
        null(),
        WS_POPUP,
        0,
        0,
        0,
        0,
        null_mut(),
        null_mut(),
        instance,
        null(),
    );
    if window.is_null() {
        return Err(last_error("Create raw mouse input window"));
    }
    let devices = raw_devices(window, RIDEV_INPUTSINK | RIDEV_DEVNOTIFY);
    if RegisterRawInputDevices(
        devices.as_ptr(),
        devices.len() as u32,
        size_of::<RAWINPUTDEVICE>() as u32,
    ) == 0
    {
        let error = last_error("Register native input devices");
        DestroyWindow(window);
        return Err(error);
    }
    Ok(window)
}

fn raw_devices(window: HWND, flags: u32) -> [RAWINPUTDEVICE; 2] {
    [2, 6].map(|usage| RAWINPUTDEVICE {
        usUsagePage: 1,
        usUsage: usage,
        dwFlags: flags,
        hwndTarget: window,
    })
}

fn registered_raw_devices() -> Option<Vec<RAWINPUTDEVICE>> {
    // Application registrations are a small set of device classes, not the
    // physical device list. Bound inspection even if another library changes it.
    let mut devices: [RAWINPUTDEVICE; 32] = unsafe { zeroed() };
    let mut count = devices.len() as u32;
    let written = unsafe {
        GetRegisteredRawInputDevices(
            devices.as_mut_ptr(),
            &mut count,
            size_of::<RAWINPUTDEVICE>() as u32,
        )
    };
    (written != u32::MAX && written as usize <= devices.len())
        .then(|| devices[..written as usize].to_vec())
}

fn owns_raw_registration(window: HWND) -> RegistrationStatus {
    let entries = registered_raw_devices().map(|devices| {
        devices
            .into_iter()
            .map(|device| RegistrationEntry {
                page: device.usUsagePage,
                usage: device.usUsage,
                flags: device.dwFlags,
                target: device.hwndTarget as usize,
            })
            .collect::<Vec<_>>()
    });
    inspect_registration(window as usize, entries.as_deref())
}

fn unregister_raw_window(window: HWND) {
    if let Some(devices) = registered_raw_devices() {
        for device in devices {
            if device.usUsagePage == 1
                && matches!(device.usUsage, 2 | 6)
                && device.hwndTarget == window
            {
                let remove = RAWINPUTDEVICE {
                    dwFlags: RIDEV_REMOVE,
                    hwndTarget: null_mut(),
                    ..device
                };
                unsafe {
                    RegisterRawInputDevices(&remove, 1, size_of::<RAWINPUTDEVICE>() as u32);
                }
            }
        }
    }
}

fn initial_devices() -> DeviceLedger {
    let mut ledger = DeviceLedger::default();
    let mut devices: [RAWINPUTDEVICELIST; 128] = unsafe { zeroed() };
    let mut count = devices.len() as u32;
    let written = unsafe {
        GetRawInputDeviceList(
            devices.as_mut_ptr(),
            &mut count,
            size_of::<RAWINPUTDEVICELIST>() as u32,
        )
    };
    if written != u32::MAX && written as usize <= devices.len() {
        for device in &devices[..written as usize] {
            if matches!(device.dwType, RIM_TYPEMOUSE | RIM_TYPEKEYBOARD) {
                ledger.arrived(device.hDevice as usize);
            }
        }
    }
    ledger
}

unsafe extern "system" fn raw_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_INPUT {
        // Same wrapping message timestamp domain as MSLLHOOKSTRUCT.time. Sample
        // before any nested message dispatch and do not infer packet order.
        let message_time = DISPATCH_TIME
            .with(|time| time.get())
            .unwrap_or_else(|| GetMessageTime() as u32);
        // RAWINPUT is naturally aligned and sufficient for mouse packets. No
        // per-packet heap allocation or unbounded HID payload is accepted.
        let mut raw: RAWINPUT = zeroed();
        let mut size = size_of::<RAWINPUT>() as u32;
        let read = GetRawInputData(
            lparam as HRAWINPUT,
            RID_INPUT,
            (&mut raw as *mut RAWINPUT).cast(),
            &mut size,
            size_of::<RAWINPUTHEADER>() as u32,
        );
        if read != u32::MAX
            && !raw.header.hDevice.is_null()
            && raw.header.dwType == RIM_TYPEMOUSE
            && read as usize >= size_of::<RAWINPUTHEADER>() + size_of::<RAWMOUSE>()
        {
            let mouse = raw.data.mouse;
            if mouse.ulExtraInformation != INJECTED_TAG as u32 {
                HOOK.with(|cell| {
                    if let Ok(mut guard) = cell.try_borrow_mut() {
                        if let Some(state) = guard.as_mut() {
                            state.raw_mouse(mouse, message_time, raw.header.hDevice as usize);
                            state.maybe_maintain_after_raw();
                        }
                    }
                });
            }
        } else if read != u32::MAX
            && !raw.header.hDevice.is_null()
            && raw.header.dwType == RIM_TYPEKEYBOARD
            && read as usize >= size_of::<RAWINPUTHEADER>() + size_of::<RAWKEYBOARD>()
        {
            HOOK.with(|cell| {
                if let Ok(mut guard) = cell.try_borrow_mut() {
                    if let Some(state) = guard.as_mut() {
                        state.raw_keyboard(
                            raw.data.keyboard,
                            message_time,
                            raw.header.hDevice as usize,
                        );
                        state.maybe_maintain_after_raw();
                    }
                }
            });
        }
        // DefWindowProc performs the foreground raw-input cleanup required by
        // WM_INPUT, even though most packets arrive through INPUTSINK.
        return DefWindowProcW(window, message, wparam, lparam);
    }
    if matches!(
        message,
        WM_DISPLAYCHANGE | WM_DEVICECHANGE | WM_INPUT_DEVICE_CHANGE
    ) {
        HOOK.with(|cell| {
            if let Ok(mut guard) = cell.try_borrow_mut() {
                if let Some(state) = guard.as_mut() {
                    if message == WM_INPUT_DEVICE_CHANGE && wparam as u32 == GIDC_REMOVAL {
                        // Device cycling can omit key/button ups. Keep no stale
                        // physical hold or raw readiness across removal.
                        state.device_removed(lparam as usize);
                    } else if message == WM_INPUT_DEVICE_CHANGE && wparam as u32 == GIDC_ARRIVAL {
                        state.devices.arrived(lparam as usize);
                    } else if message == WM_DISPLAYCHANGE {
                        state.refresh_layout();
                        state.reset();
                        state.shared.queue_reset();
                    }
                }
            }
        });
        if message == WM_INPUT_DEVICE_CHANGE {
            return 0;
        }
    }
    DefWindowProcW(window, message, wparam, lparam)
}

fn hook_loop(shared: Arc<Shared>, commands: Receiver<Command>, started: SyncSender<Result<u32>>) {
    unsafe {
        let dpi = SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        shared
            .stats
            .per_monitor_dpi
            .store(!dpi.is_null(), Ordering::Relaxed);
        let mut msg: MSG = zeroed();
        PeekMessageW(&mut msg, null_mut(), 0, 0, PM_NOREMOVE);
        let window = match raw_window() {
            Ok(window) => window,
            Err(error) => {
                let _ = started.send(Err(error));
                return;
            }
        };
        shared
            .stats
            .raw_input_registered
            .store(true, Ordering::Release);
        // Prewarm the renderer before installing hooks. Its subsequent facade
        // calls never wait for GDI/painting on this input thread.
        let overlay = CursorOverlay::create().ok();
        if overlay.is_none() {
            shared
                .stats
                .cursor_overlay_failures
                .fetch_add(1, Ordering::Relaxed);
        }
        let injector = match Executor::start(shared.clone(), GetCurrentThreadId()) {
            Ok(injector) => injector,
            Err(error) => {
                unregister_raw_window(window);
                DestroyWindow(window);
                let _ = started.send(Err(error));
                return;
            }
        };
        let module = GetModuleHandleW(null());
        let mut mouse = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook), module, 0);
        let mut keyboard = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_hook), module, 0);
        if mouse.is_null() || keyboard.is_null() {
            let error = last_error("Install native input hooks");
            if !mouse.is_null() {
                UnhookWindowsHookEx(mouse);
            }
            if !keyboard.is_null() {
                UnhookWindowsHookEx(keyboard);
            }
            unregister_raw_window(window);
            DestroyWindow(window);
            let _ = started.send(Err(error));
            return;
        }
        let now = Instant::now();
        shared
            .stats
            .mouse_hook_installed
            .store(true, Ordering::Relaxed);
        shared
            .stats
            .keyboard_hook_installed
            .store(true, Ordering::Relaxed);
        HOOK.with(|cell| {
            let mut state = HookState {
                shared: shared.clone(),
                receiving: false,
                held_keys: BTreeSet::new(),
                held_buttons: [false; 5],
                physical: LocalInputState::default(),
                return_y: 32768,
                edge_ready_at: now,
                layout: monitor_layout(),
                overlay,
                raw_health: RawCaptureHealth::new(now),
                pending_motion: MotionBatch::default(),
                source_handoff: SourceHandoff::default(),
                receiver_return: ReceiverReturn::default(),
                desktop_name: desktop_name(GetThreadDesktop(GetCurrentThreadId()))
                    .unwrap_or_default(),
                next_desktop_check: now,
                next_cursor_update: now,
                next_physical_check: now,
                mouse_delivery: HookDelivery::default(),
                keyboard_delivery: HookDelivery::default(),
                devices: initial_devices(),
                reclaim_motion: ReclaimMotion::default(),
                reinstall_hooks: false,
                hooks_available: true,
                next_hook_retry: now,
                raw_window: window,
                registration_audit: RegistrationAudit::new(now),
                input_desktop_available: true,
                capture_epoch: 0,
                next_raw_maintenance: now,
                injector: Some(injector),
                owner: Arc::new(Owner::default()),
                pending: None,
            };
            state.refresh_layout();
            *cell.borrow_mut() = Some(state);
        });
        // Timers provide idle maintenance; continuous command traffic also runs
        // maintenance explicitly so low-priority WM_TIMER cannot be starved.
        let timer = SetTimer(null_mut(), 0, 10, None);
        if timer == 0 {
            let _ = started.send(Err(last_error("Start native input recovery timer")));
        } else if started.send(Ok(GetCurrentThreadId())).is_ok() {
            while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
                if msg.message == WAKE {
                    shared.wake_gate.received();
                }
                if !dispatch_then_pump(&msg, window, WM_INPUT_DEVICE_CHANGE, WM_INPUT).0 {
                    break;
                }
                if msg.message == WAKE || msg.message == WM_TIMER {
                    HOOK.with(|cell| {
                        let mut guard = cell.borrow_mut();
                        let Some(state) = guard.as_mut() else { return };
                        if state.shared.force_release.swap(false, Ordering::AcqRel)
                            || !state.shared.ready()
                        {
                            state.reset();
                        }
                        if !state.receiving
                            && (!state.held_keys.is_empty()
                                || state.held_buttons.iter().any(|held| *held))
                        {
                            state.release_injected();
                        }
                        state
                            .shared
                            .stats
                            .native_pump_slices
                            .fetch_add(1, Ordering::Relaxed);
                        let mut slice = WorkSlice::new(Instant::now());
                        // Both count and elapsed time bound a drain even when a
                        // producer continuously replenishes this bounded queue.
                        state.poll_injection();
                        while state.pending.is_none() {
                            let Ok(command) = commands.try_recv() else {
                                break;
                            };
                            match command {
                                Command::Apply {
                                    event,
                                    epoch,
                                    queued_at,
                                    deadline,
                                    done,
                                } => {
                                    let started = Instant::now();
                                    let wait = started
                                        .saturating_duration_since(queued_at)
                                        .as_micros()
                                        .min(u64::MAX as u128)
                                        as u64;
                                    state
                                        .shared
                                        .stats
                                        .native_queue_wait_us
                                        .fetch_add(wait, Ordering::Relaxed);
                                    state
                                        .shared
                                        .stats
                                        .native_queue_wait_peak_us
                                        .fetch_max(wait, Ordering::Relaxed);
                                    let cleanup =
                                        matches!(event, InputEvent::Leave | InputEvent::ReleaseAll);
                                    let result = if epoch
                                        != state.shared.epoch.load(Ordering::Acquire)
                                    {
                                        Err(anyhow!(NativeInputFailure::PreconditionUnavailable))
                                    } else if check_deadline(started, deadline, cleanup).is_err() {
                                        state.reset();
                                        Err(anyhow!(NativeInputFailure::StaleCommand))
                                    } else if !cleanup && done.abandoned() {
                                        state.reset();
                                        Err(anyhow!(NativeInputFailure::CommandQueueUnavailable))
                                    } else {
                                        state.submit_remote(
                                            event.clone(),
                                            epoch,
                                            deadline,
                                            done,
                                            started,
                                        );
                                        let _ =
                                            post_input_wake(&state.shared, GetCurrentThreadId());
                                        if slice.complete(Instant::now()) {
                                            state
                                                .shared
                                                .stats
                                                .native_pump_yields
                                                .fetch_add(1, Ordering::Relaxed);
                                            let _ = post_input_wake(
                                                &state.shared,
                                                GetCurrentThreadId(),
                                            );
                                        }
                                        break;
                                    };
                                    let work =
                                        started.elapsed().as_micros().min(u64::MAX as u128) as u64;
                                    state
                                        .shared
                                        .stats
                                        .native_execution_us
                                        .fetch_add(work, Ordering::Relaxed);
                                    state
                                        .shared
                                        .stats
                                        .native_execution_peak_us
                                        .fetch_max(work, Ordering::Relaxed);
                                    state
                                        .shared
                                        .stats
                                        .native_commands
                                        .fetch_add(1, Ordering::Relaxed);
                                    done.send(result);
                                }
                                Command::Release => state.reset(),
                                Command::SetRemote(true) => state.begin_remote(),
                                Command::SetRemote(false) => state.stop_remote(),
                            }
                            if slice.complete(Instant::now()) {
                                state
                                    .shared
                                    .stats
                                    .native_pump_yields
                                    .fetch_add(1, Ordering::Relaxed);
                                let _ = post_input_wake(&state.shared, GetCurrentThreadId());
                                break;
                            }
                        }
                        state.maintenance();
                        if state.reinstall_hooks && Instant::now() >= state.next_hook_retry {
                            // Recovery is inferred from raw/callback evidence,
                            // not a Windows liveness query. Release before any
                            // unhook/install gap and never leave partial capture.
                            state.reset();
                            state.hooks_available = false;
                            if !mouse.is_null() {
                                UnhookWindowsHookEx(mouse);
                            }
                            if !keyboard.is_null() {
                                UnhookWindowsHookEx(keyboard);
                            }
                            mouse = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook), module, 0);
                            keyboard =
                                SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_hook), module, 0);
                            let success = !mouse.is_null() && !keyboard.is_null();
                            if !success {
                                if !mouse.is_null() {
                                    UnhookWindowsHookEx(mouse);
                                    mouse = null_mut();
                                }
                                if !keyboard.is_null() {
                                    UnhookWindowsHookEx(keyboard);
                                    keyboard = null_mut();
                                }
                                state
                                    .shared
                                    .stats
                                    .hook_recovery_failures
                                    .fetch_add(1, Ordering::Relaxed);
                            } else {
                                state
                                    .shared
                                    .stats
                                    .hook_recoveries
                                    .fetch_add(1, Ordering::Relaxed);
                            }
                            state.hooks_available = success;
                            state.reinstall_hooks = !success;
                            state.next_hook_retry = Instant::now() + Duration::from_secs(1);
                            state
                                .shared
                                .stats
                                .mouse_hook_installed
                                .store(success, Ordering::Relaxed);
                            state
                                .shared
                                .stats
                                .keyboard_hook_installed
                                .store(success, Ordering::Relaxed);
                            state.raw_health.invalidate();
                            state.mouse_delivery.clear();
                            state.keyboard_delivery.clear();
                        }
                    });
                }
            }
        }
        if timer != 0 {
            KillTimer(null_mut(), timer);
        }
        HOOK.with(|cell| {
            if let Some(mut state) = cell.borrow_mut().take() {
                state.reset();
            }
        });
        unregister_raw_window(window);
        DestroyWindow(window);
        UnhookWindowsHookEx(keyboard);
        UnhookWindowsHookEx(mouse);
    }
}

impl HookState {
    fn sync_capture_epoch(&mut self) {
        let epoch = self.shared.epoch.load(Ordering::Acquire);
        if epoch != self.capture_epoch {
            self.reset();
            self.capture_epoch = epoch;
            self.raw_health.invalidate();
        }
    }
    fn maybe_maintain_after_raw(&mut self) {
        if Instant::now() >= self.next_raw_maintenance {
            self.next_raw_maintenance = Instant::now() + Duration::from_millis(10);
            self.maintenance();
        }
    }
    fn sync_physical_counts(&self) {
        let (key_count, button_count) = self.physical.union_counts(
            |vk| self.devices.has_key(vk),
            |index| self.devices.has_button(index),
        );
        self.shared
            .stats
            .held_physical_keys
            .store(key_count as u64, Ordering::Relaxed);
        self.shared
            .stats
            .held_physical_buttons
            .store(button_count as u64, Ordering::Relaxed);
        let keys = self.physical.protected_key_count() as u64;
        let buttons = self.physical.protected_button_count() as u64;
        self.shared
            .stats
            .protected_physical_keys
            .store(keys, Ordering::Relaxed);
        self.shared
            .stats
            .protected_physical_buttons
            .store(buttons, Ordering::Relaxed);
        self.shared.stats.held_input_recovery_required.store(
            (keys + buttons > 0 || self.devices.any_held())
                && !self.receiving
                && !self.shared.remote.load(Ordering::Acquire),
            Ordering::Relaxed,
        );
    }

    fn reconcile_local_physical_state(&mut self) {
        let sending = self.shared.remote.load(Ordering::Acquire);
        let injected_held =
            !self.held_keys.is_empty() || self.held_buttons.iter().any(|held| *held);
        let pruned = self
            .physical
            .reconcile(sending, self.receiving, injected_held, |vk| {
                // GetAsyncKeyState runs in timer maintenance, never inside a hook.
                // Only its high bit is meaningful here; the low bit is unreliable.
                unsafe { GetAsyncKeyState(i32::from(vk)) < 0 }
            });
        self.shared
            .stats
            .stale_keys_reconciled
            .fetch_add(pruned.keys as u64, Ordering::Relaxed);
        self.shared
            .stats
            .stale_buttons_reconciled
            .fetch_add(pruned.buttons as u64, Ordering::Relaxed);
        self.sync_physical_counts();
    }

    fn sync_overlay_diagnostics(&self) {
        if let Some(overlay) = &self.overlay {
            let snapshot = overlay.diagnostics();
            self.shared
                .stats
                .cursor_overlay_visible
                .store(snapshot.visible, Ordering::Relaxed);
            self.shared
                .stats
                .cursor_overlay_activations
                .store(snapshot.activations, Ordering::Relaxed);
            self.shared
                .stats
                .cursor_overlay_shape_changes
                .store(snapshot.shape_changes, Ordering::Relaxed);
            self.shared
                .stats
                .cursor_overlay_work_us
                .store(snapshot.work_us, Ordering::Relaxed);
            self.shared
                .stats
                .cursor_overlay_failures
                .store(snapshot.failures, Ordering::Relaxed);
            self.shared
                .stats
                .cursor_overlay_recoveries
                .store(snapshot.recoveries, Ordering::Relaxed);
        }
    }

    fn refresh_layout(&mut self) {
        self.layout = monitor_layout();
        self.shared
            .stats
            .monitor_count
            .store(self.layout.monitors.len() as u64, Ordering::Relaxed);
        self.shared
            .stats
            .virtual_width
            .store(self.layout.bounds.width as u64, Ordering::Relaxed);
        self.shared
            .stats
            .virtual_height
            .store(self.layout.bounds.height as u64, Ordering::Relaxed);
    }

    fn reset(&mut self) {
        if self.receiving || self.shared.remote.load(Ordering::Acquire) {
            self.shared
                .stats
                .capture_resets
                .fetch_add(1, Ordering::Relaxed);
        }
        self.release_injected();
        self.owner = Arc::new(Owner::default());
        self.stop_remote();
        self.receiving = false;
        self.reclaim_motion.clear();
        self.receiver_return.clear();
        self.shared.stats.receiving.store(false, Ordering::Release);
        self.hide_overlay();
    }

    fn hide_overlay(&mut self) {
        if let Some(overlay) = self.overlay.as_mut() {
            overlay.hide();
        }
        self.shared
            .stats
            .cursor_overlay_visible
            .store(false, Ordering::Relaxed);
    }

    fn release_injected(&mut self) {
        self.owner.cancel();
        if let Some(pending) = &self.pending {
            pending.owner.cancel();
        }
        self.held_keys.clear();
        self.held_buttons = [false; 5];
        if let Some(injector) = &self.injector {
            injector.cleanup();
        }
    }
    fn publish_desktop(&mut self, available: bool) {
        self.input_desktop_available = available;
        self.shared
            .input_desktop_available
            .store(available, Ordering::Release);
    }
    fn receiving_or_pending(&self) -> bool {
        self.receiving
            || self.pending.as_ref().is_some_and(|pending| {
                matches!(pending.event, InputEvent::Enter { .. }) && pending.owner.current()
            })
    }

    fn begin_remote(&mut self) {
        self.sync_capture_epoch();
        let epoch = self.shared.epoch.load(Ordering::Acquire);
        let left = self.shared.peer_on_left.load(Ordering::Acquire);
        if let Ok(point) = current_cursor_position() {
            self.begin_remote_at(point, epoch, left, false);
        }
    }
    fn source_admission_current(&self, epoch: u64, left: bool, automatic: bool) -> bool {
        self.shared.epoch.load(Ordering::Acquire) == epoch
            && self.shared.peer_on_left.load(Ordering::Acquire) == left
            && self.shared.ready()
            && !self.shared.force_release.load(Ordering::Acquire)
            && !self.receiving
            && self.input_desktop_available
            && self.hooks_available
            && self.pending.is_none()
            && !self
                .injector
                .as_ref()
                .is_some_and(|injector| injector.busy())
            && self
                .shared
                .stats
                .raw_input_registered
                .load(Ordering::Acquire)
            && self.raw_health.can_capture()
            && self.shared.reset_queued.load(Ordering::Acquire) != epoch.saturating_add(1)
            && (!automatic
                || (self.shared.edge_switching.load(Ordering::Acquire)
                    && Instant::now() >= self.edge_ready_at))
    }
    fn source_holds_clear(&self) -> bool {
        !self
            .injector
            .as_ref()
            .is_some_and(|injector| injector.has_held())
            && !self.physical.any_held()
            && !self.devices.any_held()
            && self.held_keys.is_empty()
            && !self.held_buttons.iter().any(|held| *held)
    }
    fn begin_remote_at(&mut self, point: POINT, epoch: u64, left: bool, automatic: bool) {
        // The point is the one authorized by the raw sample. A second async
        // cursor query could observe a different position/height altogether.
        if !self.source_admission_current(epoch, left, automatic) {
            self.source_handoff.clear();
            return;
        }
        if !self.source_holds_clear() {
            self.source_handoff.clear();
            self.shared
                .stats
                .handoff_blocked_by_held
                .fetch_add(1, Ordering::Relaxed);
            return;
        }
        if automatic && !self.layout.handoff_edge(point.x, point.y, left) {
            self.source_handoff.clear();
            return;
        }
        self.owner.cancel();
        self.owner = Arc::new(Owner::default());
        if let Some(injector) = &self.injector {
            injector.cleanup();
        }
        self.return_y = self.layout.bounds.normalize_y(point.y);
        self.pending_motion.clear();
        if !self.raw_health.begin(Instant::now()) {
            return;
        }
        if !self.source_admission_current(epoch, left, automatic) || !self.source_holds_clear() {
            self.source_handoff.clear();
            return;
        }
        // Raw deltas remain valid while the source cursor is stationary at its
        // edge. There is deliberately no center warp, ClipCursor, or hide call.
        if self.shared.emit_for_epoch(
            InputEvent::Enter {
                from_left: !left,
                y: self.return_y,
            },
            epoch,
        ) {
            if !self.source_admission_current(epoch, left, automatic) || !self.source_holds_clear()
            {
                self.abort_source_entry(epoch);
                return;
            }
            self.devices.begin_source();
            self.shared.remote.store(true, Ordering::Release);
            // Atomics cannot make enqueue and OS authority one transaction. A
            // raced revocation must still prevent an old source state winning.
            if !self.source_admission_current(epoch, left, automatic) || !self.source_holds_clear()
            {
                self.abort_source_entry(epoch);
                return;
            }
            self.shared
                .stats
                .source_enter_committed
                .fetch_add(1, Ordering::Relaxed);
        }
    }

    fn abort_source_entry(&mut self, epoch: u64) {
        self.shared.remote.store(false, Ordering::Release);
        self.devices.end_source();
        self.pending_motion.clear();
        self.source_handoff.clear();
        self.raw_health.invalidate();
        // Old-epoch cleanup must not be retagged for a replacement connection.
        // Epoch revocation already retires its session; same-epoch policy/side
        // changes need ordered cleanup behind the accepted Enter.
        self.shared.emit_for_epoch(InputEvent::ReleaseAll, epoch);
        self.shared.emit_for_epoch(InputEvent::Leave, epoch);
    }

    fn stop_remote(&mut self) {
        let was_remote = self.shared.remote.swap(false, Ordering::AcqRel);
        self.pending_motion.clear();
        self.raw_health.stop();
        self.source_handoff.clear();
        self.devices.end_source();
        if was_remote {
            let peer_on_left = self.shared.peer_on_left.load(Ordering::Acquire);
            let (x, y) = self.layout.entry(peer_on_left, self.return_y, 12);
            if let Some(injector) = &self.injector {
                injector.restore(
                    x,
                    y,
                    self.layout.bounds,
                    self.shared.physical_input_serial.load(Ordering::Acquire),
                    self.owner.clone(),
                    self.shared.epoch.load(Ordering::Acquire),
                );
            }
        }
        self.edge_ready_at = Instant::now() + Duration::from_millis(350);
    }

    fn reclaim(&mut self) {
        if self.receiving_or_pending() {
            self.receiving = false;
            self.receiver_return.clear();
            self.shared.stats.receiving.store(false, Ordering::Release);
            self.hide_overlay();
            self.release_injected();
            self.owner = Arc::new(Owner::default());
            self.shared.emit(InputEvent::Leave);
            self.edge_ready_at = Instant::now() + Duration::from_millis(350);
        }
    }

    fn flush_motion(&mut self) -> bool {
        if !self.shared.ready() || !self.shared.remote.load(Ordering::Acquire) {
            self.pending_motion.clear();
            return false;
        }
        self.pending_motion
            .flush(|event| self.shared.capture(event))
    }

    fn emit_action(&mut self, event: InputEvent) -> bool {
        self.pending_motion
            .action(event, |event| self.shared.capture(event))
    }

    fn raw_keyboard(&mut self, keyboard: RAWKEYBOARD, timestamp: u32, device: usize) {
        if keyboard.ExtraInformation == INJECTED_TAG as u32 {
            return;
        }
        self.sync_capture_epoch();
        let Some(vk) = normalize_raw_key(
            keyboard.VKey,
            keyboard.MakeCode,
            keyboard.Flags & RI_KEY_E0 as u16 != 0,
        ) else {
            return;
        };
        let down = keyboard.Flags & RI_KEY_BREAK as u16 == 0;
        self.shared
            .physical_input_serial
            .fetch_add(1, Ordering::AcqRel);
        self.keyboard_delivery.raw(Instant::now(), timestamp);
        self.devices
            .key(device, vk, down, self.shared.remote.load(Ordering::Acquire));
        if down {
            self.physical.raw_key_down(vk);
            if self.receiving_or_pending() {
                self.shared
                    .stats
                    .native_key_reclaims
                    .fetch_add(1, Ordering::Relaxed);
                self.reclaim();
            }
        }
        if !down {
            self.devices.key_release(vk, timestamp, Instant::now());
        }
        // An authentic hardware UP can clean up even if the LL callback was
        // swallowed elsewhere. Another device may still hold the same key.
        if !down && !self.devices.has_key(vk) {
            self.physical.release_key(vk);
            self.sync_physical_counts();
        }
    }

    fn device_removed(&mut self, device: usize) {
        let unattributed = self.physical.has_unattributed_holds(
            |vk| self.devices.has_key(vk),
            |index| self.devices.has_button(index),
        );
        let removed = self.devices.remove(device, unattributed);
        for vk in removed.keys {
            self.physical.release_key(vk);
        }
        for index in removed.buttons {
            self.physical.button(index, false);
        }
        self.sync_physical_counts();
        if removed.reset {
            self.raw_health.invalidate();
            self.shared
                .stats
                .device_input_resets
                .fetch_add(1, Ordering::Relaxed);
            self.reset();
            if self.shared.ready() {
                self.shared.queue_reset();
            }
        } else {
            self.shared
                .stats
                .device_removals_ignored
                .fetch_add(1, Ordering::Relaxed);
        }
    }

    fn raw_mouse(&mut self, mouse: RAWMOUSE, message_time: u32, device: usize) {
        self.raw_mouse_with_cursor(mouse, message_time, device, current_cursor_position);
    }

    /// The cursor provider is an integration seam: tests exercise this complete
    /// production raw/admission path without reading or moving the real cursor.
    fn raw_mouse_with_cursor(
        &mut self,
        mouse: RAWMOUSE,
        message_time: u32,
        device: usize,
        mut cursor: impl FnMut() -> Result<POINT>,
    ) {
        if mouse.ulExtraInformation == INJECTED_TAG as u32 {
            return;
        }
        self.sync_capture_epoch();
        if mouse.usFlags & MOUSE_MOVE_ABSOLUTE != 0 {
            self.raw_health.invalidate();
            self.source_handoff.clear();
            self.shared
                .stats
                .raw_absolute_packets
                .fetch_add(1, Ordering::Relaxed);
            // Absolute tablets/RDP are not silently treated as relative mice.
            // Keep local control instead of swallowing unsupported input.
            if self.shared.remote.load(Ordering::Acquire) {
                self.reset();
                self.shared.queue_reset();
            }
            if self.receiving_or_pending() {
                self.reclaim();
            }
            return;
        }
        self.raw_health.raw_progress(
            Instant::now(),
            message_time,
            mouse.lLastX != 0 || mouse.lLastY != 0,
        );
        let buttons = unsafe { mouse.Anonymous.Anonymous };
        let flags = u32::from(buttons.usButtonFlags);
        let motion = mouse.lLastX != 0 || mouse.lLastY != 0;
        if motion || flags != 0 {
            self.shared
                .physical_input_serial
                .fetch_add(1, Ordering::AcqRel);
        }
        let mut changed = [(0, false); 10];
        let mut changed_count = 0;
        for (index, down_flag, up_flag) in [
            (0, RI_MOUSE_LEFT_BUTTON_DOWN, RI_MOUSE_LEFT_BUTTON_UP),
            (1, RI_MOUSE_RIGHT_BUTTON_DOWN, RI_MOUSE_RIGHT_BUTTON_UP),
            (2, RI_MOUSE_MIDDLE_BUTTON_DOWN, RI_MOUSE_MIDDLE_BUTTON_UP),
            (3, RI_MOUSE_BUTTON_4_DOWN, RI_MOUSE_BUTTON_4_UP),
            (4, RI_MOUSE_BUTTON_5_DOWN, RI_MOUSE_BUTTON_5_UP),
        ] {
            for (flag, down) in [(down_flag, true), (up_flag, false)] {
                if flags & flag != 0 {
                    changed[changed_count] = (index, down);
                    changed_count += 1;
                }
            }
        }
        self.devices.mouse(
            device,
            &changed[..changed_count],
            motion,
            self.shared.remote.load(Ordering::Acquire),
        );
        for &(index, down) in &changed[..changed_count] {
            if down {
                self.physical.button(index, true);
            }
            if !down {
                self.devices
                    .button_release(index, message_time, Instant::now());
            }
            if !down && !self.devices.has_button(index) {
                self.physical.button(index, false);
            }
        }
        if motion || flags != 0 {
            self.mouse_delivery.raw(Instant::now(), message_time);
        }
        let physical_action = flags
            & (RI_MOUSE_LEFT_BUTTON_DOWN
                | RI_MOUSE_RIGHT_BUTTON_DOWN
                | RI_MOUSE_MIDDLE_BUTTON_DOWN
                | RI_MOUSE_BUTTON_4_DOWN
                | RI_MOUSE_BUTTON_5_DOWN
                | RI_MOUSE_WHEEL
                | RI_MOUSE_HWHEEL)
            != 0;
        if physical_action && self.receiving_or_pending() {
            self.shared
                .stats
                .physical_mouse_reclaims
                .fetch_add(1, Ordering::Relaxed);
            self.reclaim();
        }
        if self.receiving_or_pending() && motion {
            if self
                .reclaim_motion
                .moved(Instant::now(), mouse.lLastX, mouse.lLastY)
            {
                self.shared
                    .stats
                    .physical_mouse_reclaims
                    .fetch_add(1, Ordering::Relaxed);
                self.reclaim();
            } else {
                self.shared
                    .stats
                    .mouse_jitter_events_ignored
                    .fetch_add(1, Ordering::Relaxed);
            }
        }
        if mouse.lLastX != 0 || mouse.lLastY != 0 {
            self.shared
                .stats
                .raw_motion_packets
                .fetch_add(1, Ordering::Relaxed);
        }
        let mut transition = false;
        if !self.receiving
            && !self.shared.remote.load(Ordering::Acquire)
            // Any action in this packet may already have passed through locally
            // before its raw delivery. Only motion-only packets may cross.
            && flags == 0
            && self.shared.ready()
            && self.shared.edge_switching.load(Ordering::Acquire)
            && Instant::now() >= self.edge_ready_at
        {
            let epoch = self.shared.epoch.load(Ordering::Acquire);
            let left = self.shared.peer_on_left.load(Ordering::Acquire);
            self.shared
                .stats
                .source_edge_samples
                .fetch_add(1, Ordering::Relaxed);
            // GetCursorPos is current screen state, not a hook candidate or a
            // coordinate reconstructed from unaccelerated relative raw units.
            // A fixed/clamped edge can therefore consume fresh raw intent even
            // without another legacy/hook movement. Read it exactly once.
            // https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-getcursorpos
            if let Ok(point) = cursor() {
                if !self.source_admission_current(epoch, left, true) {
                    self.source_handoff.clear();
                } else if !self.source_holds_clear() {
                    self.source_handoff.clear();
                    self.shared
                        .stats
                        .handoff_blocked_by_held
                        .fetch_add(1, Ordering::Relaxed);
                } else {
                    if self.layout.handoff_edge(point.x, point.y, left) {
                        self.shared
                            .stats
                            .source_edge_candidates
                            .fetch_add(1, Ordering::Relaxed);
                    }
                    if self
                        .source_handoff
                        .raw(
                            (point.x, point.y),
                            mouse.lLastX,
                            mouse.lLastY,
                            left,
                            &self.layout,
                        )
                        .is_some()
                    {
                        self.shared
                            .stats
                            .source_edge_intent_ready
                            .fetch_add(1, Ordering::Relaxed);
                        self.begin_remote_at(point, epoch, left, true);
                        transition = self.shared.remote.load(Ordering::Acquire);
                    }
                }
            } else {
                self.source_handoff.clear();
            }
        } else if !self.shared.remote.load(Ordering::Acquire) {
            // Action packets/policy boundaries cannot preserve a half-gesture
            // through held controls or an interval without source admission.
            self.source_handoff.clear();
        }
        if !self.shared.ready() || !self.shared.remote.load(Ordering::Acquire) {
            return;
        }
        if !transition {
            self.pending_motion.add(mouse.lLastX, mouse.lLastY);
        }
        // Buttons/wheels come from the same ordered raw packet stream. Flush
        // accumulated motion before them so clicks never overtake cursor motion.
        if !self.flush_motion() {
            return;
        }
        for (down_flag, up_flag, button) in [
            (
                RI_MOUSE_LEFT_BUTTON_DOWN,
                RI_MOUSE_LEFT_BUTTON_UP,
                MouseButton::Left,
            ),
            (
                RI_MOUSE_RIGHT_BUTTON_DOWN,
                RI_MOUSE_RIGHT_BUTTON_UP,
                MouseButton::Right,
            ),
            (
                RI_MOUSE_MIDDLE_BUTTON_DOWN,
                RI_MOUSE_MIDDLE_BUTTON_UP,
                MouseButton::Middle,
            ),
            (
                RI_MOUSE_BUTTON_4_DOWN,
                RI_MOUSE_BUTTON_4_UP,
                MouseButton::Back,
            ),
            (
                RI_MOUSE_BUTTON_5_DOWN,
                RI_MOUSE_BUTTON_5_UP,
                MouseButton::Forward,
            ),
        ] {
            for (flag, down) in [(down_flag, true), (up_flag, false)] {
                if flags & flag != 0 && !self.emit_action(InputEvent::Button { button, down }) {
                    return;
                }
            }
        }
        for (flag, horizontal) in [(RI_MOUSE_WHEEL, false), (RI_MOUSE_HWHEEL, true)] {
            if flags & flag != 0
                && !self.emit_action(InputEvent::Wheel {
                    delta: i32::from(buttons.usButtonData as i16),
                    horizontal,
                })
            {
                return;
            }
        }
    }

    fn note_registration_status(&self, status: RegistrationStatus) {
        match status {
            RegistrationStatus::Unavailable => {
                self.shared
                    .stats
                    .raw_registration_query_failures
                    .fetch_add(1, Ordering::Relaxed);
            }
            RegistrationStatus::Mismatch => {
                self.shared
                    .stats
                    .raw_registration_mismatches
                    .fetch_add(1, Ordering::Relaxed);
            }
            RegistrationStatus::Owned => {}
        }
    }

    fn audit_raw_registration(&mut self) {
        let status = owns_raw_registration(self.raw_window);
        self.note_registration_status(status);
        let action = self.registration_audit.observe(status, Instant::now());
        self.shared
            .stats
            .raw_input_registered
            .store(self.registration_audit.admitted(), Ordering::Release);
        if action.suspend
            || (status != RegistrationStatus::Owned
                && (self.receiving || self.shared.remote.load(Ordering::Acquire)))
        {
            let active = self.receiving || self.shared.remote.load(Ordering::Acquire);
            self.reset();
            self.raw_health.invalidate();
            self.mouse_delivery.clear();
            self.keyboard_delivery.clear();
            if active {
                self.shared.queue_reset();
            }
        }
        if !action.repair {
            return;
        }
        // Every actual repair starts a fresh readiness boundary, including a
        // retry after a prior unavailable/mismatched verification snapshot.
        self.raw_health.invalidate();
        // An unavailable query is not evidence that another window owns input.
        // Re-register only after an authoritative incompatible snapshot, and
        // never let arriving old/raw packets override that snapshot.
        let devices = raw_devices(self.raw_window, RIDEV_INPUTSINK | RIDEV_DEVNOTIFY);
        let registered = unsafe {
            RegisterRawInputDevices(
                devices.as_ptr(),
                devices.len() as u32,
                size_of::<RAWINPUTDEVICE>() as u32,
            )
        } != 0;
        let verified = if registered {
            let status = owns_raw_registration(self.raw_window);
            self.note_registration_status(status);
            self.registration_audit.after_repair(status, Instant::now())
        } else {
            false
        };
        self.shared
            .stats
            .raw_input_registered
            .store(verified, Ordering::Release);
        if verified {
            self.shared
                .stats
                .raw_registration_repairs
                .fetch_add(1, Ordering::Relaxed);
        } else {
            self.shared
                .stats
                .raw_registration_failures
                .fetch_add(1, Ordering::Relaxed);
        }
    }

    fn maintenance(&mut self) {
        self.sync_capture_epoch();
        self.poll_injection();
        if self.shared.force_release.swap(false, Ordering::AcqRel) || !self.shared.ready() {
            self.reset();
        }
        if let Some(overlay) = &mut self.overlay {
            overlay.service_recovery();
        }
        self.sync_overlay_diagnostics();
        self.sync_physical_counts();
        if !self.reinstall_hooks
            && (self.mouse_delivery.suspected_stall() || self.keyboard_delivery.suspected_stall())
        {
            self.shared
                .stats
                .hook_delivery_stalls
                .fetch_add(1, Ordering::Relaxed);
            self.reset();
            self.raw_health.invalidate();
            if self.shared.ready() {
                self.shared.queue_reset();
            }
            self.reinstall_hooks = true;
            self.next_hook_retry = Instant::now();
        }
        if self.registration_audit.due(Instant::now()) {
            self.audit_raw_registration();
        }
        if Instant::now() >= self.next_physical_check {
            self.next_physical_check = Instant::now() + Duration::from_millis(100);
            self.reconcile_local_physical_state();
        }
        if self.shared.remote.load(Ordering::Acquire) {
            if self.raw_health.stalled(Instant::now()) {
                self.shared
                    .stats
                    .raw_capture_stalls
                    .fetch_add(1, Ordering::Relaxed);
                self.reset();
                self.raw_health.invalidate();
                self.shared.queue_reset();
            } else if self.shared.remote.load(Ordering::Acquire) {
                self.flush_motion();
            }
        }
        if Instant::now() >= self.next_desktop_check {
            self.next_desktop_check = Instant::now() + Duration::from_millis(100);
            let available = on_input_desktop(&self.desktop_name);
            if available != self.input_desktop_available {
                self.publish_desktop(available);
                let active = self.receiving || self.shared.remote.load(Ordering::Acquire);
                self.reset();
                self.raw_health.invalidate();
                self.mouse_delivery.clear();
                self.keyboard_delivery.clear();
                if active {
                    self.shared.queue_reset();
                }
                if available {
                    self.reinstall_hooks = true;
                    self.next_hook_retry = Instant::now();
                    self.registration_audit.request_check(Instant::now());
                }
            }
        }
        if self.receiving {
            let mut point: POINT = unsafe { zeroed() };
            if unsafe { GetCursorPos(&mut point) } == 0 {
                self.reset();
                self.shared.queue_reset();
                return;
            }
            if self
                .receiver_return
                .sample(Instant::now(), (point.x, point.y), &self.layout)
            {
                self.shared
                    .stats
                    .remote_edge_returns
                    .fetch_add(1, Ordering::Relaxed);
                self.reclaim();
                return;
            }
            if Instant::now() >= self.next_cursor_update && self.update_overlay(point).is_err() {
                // A remote session without a visible cursor must fail back to
                // local control if its native fallback cannot be presented.
                self.reset();
                self.shared.queue_reset();
            }
        } else {
            self.hide_overlay();
        }
    }

    fn update_overlay(&mut self, point: POINT) -> Result<()> {
        // Incoming mouse packets can arrive at 500Hz; cursor painting is bounded
        // separately so GDI work never throttles input delivery.
        self.next_cursor_update = Instant::now() + Duration::from_millis(8);
        if !self.input_desktop_available {
            bail!(NativeInputFailure::PreconditionUnavailable);
        }
        if !cursor::fallback_required().context(NativeInputFailure::CursorUnavailable)? {
            self.hide_overlay();
            return Ok(());
        }
        // The prewarmed worker samples cursor visibility/shape and paints on its
        // own thread, preserving valid hidden hCursor shapes. No HWND/GDI work
        // or borrowed cross-thread handle runs on this native input path.
        self.overlay
            .as_mut()
            .context(NativeInputFailure::CursorUnavailable)?
            .update(point, null_mut())
            .context(NativeInputFailure::CursorUnavailable)?;
        self.sync_overlay_diagnostics();
        Ok(())
    }

    fn epoch_is_current(&self, expected_epoch: u64) -> bool {
        self.shared.ready()
            && self.input_desktop_available
            && self.hooks_available
            && self
                .shared
                .stats
                .raw_input_registered
                .load(Ordering::Acquire)
            && self.shared.epoch.load(Ordering::Acquire) == expected_epoch
    }

    fn check_input_epoch(&mut self, expected_epoch: u64, deadline: Instant) -> Result<()> {
        if let Err(error) = check_deadline(Instant::now(), deadline, false) {
            self.reset();
            return Err(error);
        }
        if !self.epoch_is_current(expected_epoch) {
            self.reset();
            bail!(NativeInputFailure::PreconditionUnavailable);
        }
        Ok(())
    }

    fn submit_remote(
        &mut self,
        event: InputEvent,
        epoch: u64,
        deadline: Instant,
        done: Completion,
        started: Instant,
    ) {
        let prepared = self.prepare_remote(&event, epoch, deadline);
        let (effect, point, owner) = match prepared {
            Ok(Some(prepared)) => prepared,
            Ok(None) => {
                self.finish_apply(done, Ok(()), started, None);
                return;
            }
            Err(error) => {
                self.finish_apply(done, Err(error), started, None);
                return;
            }
        };
        self.submit_prepared(event, effect, point, owner, epoch, deadline, done, started);
    }

    #[allow(clippy::too_many_arguments)]
    fn submit_prepared(
        &mut self,
        event: InputEvent,
        effect: Effect,
        point: Option<POINT>,
        owner: Arc<Owner>,
        epoch: u64,
        deadline: Instant,
        done: Completion,
        started: Instant,
    ) {
        let response = self
            .injector
            .as_ref()
            .context(NativeInputFailure::CommandQueueUnavailable)
            .and_then(|injector| injector.submit(effect, owner.clone(), epoch, deadline));
        match response {
            Ok(receiver) => {
                self.pending = Some(PendingApply {
                    event,
                    point,
                    owner,
                    epoch,
                    deadline,
                    receiver,
                    done,
                    started,
                })
            }
            Err(error) => {
                owner.cancel();
                self.reset();
                self.finish_apply(done, Err(error), started, None);
            }
        }
    }

    fn prepare_enter_owner(&mut self) {
        self.reset();
        // reset may have queued a source return warp. Incoming Enter also
        // supersedes that optional position work before it reaches the OS.
        self.owner.cancel();
        self.owner = Arc::new(Owner::default());
    }

    fn prepare_remote(
        &mut self,
        event: &InputEvent,
        epoch: u64,
        deadline: Instant,
    ) -> Result<Option<PreparedApply>> {
        self.sync_capture_epoch();
        if matches!(event, InputEvent::Leave | InputEvent::ReleaseAll) {
            let owner = self.owner.clone();
            if matches!(event, InputEvent::Leave) {
                self.reset();
            } else {
                self.release_injected();
                self.owner = Arc::new(Owner::default());
            }
            return Ok(Some((Effect::Release, None, owner)));
        }
        self.check_input_epoch(epoch, deadline)?;
        if let InputEvent::Enter { from_left, y } = event {
            self.publish_desktop(on_input_desktop(&self.desktop_name));
            self.check_input_epoch(epoch, deadline)?;
            self.prepare_enter_owner();
            self.refresh_layout();
            let (x, y) = self.layout.entry(*from_left, *y, 12);
            self.check_input_epoch(epoch, deadline)?;
            return Ok(Some((
                Effect::Warp(x, y, self.layout.bounds),
                Some(POINT { x, y }),
                self.owner.clone(),
            )));
        }
        if !self.receiving {
            self.shared
                .stats
                .ignored_remote_inputs
                .fetch_add(1, Ordering::Relaxed);
            return Ok(None);
        }
        if let InputEvent::Key {
            scan,
            vk,
            extended,
            down,
        } = event
        {
            if *scan > 255 || *vk > 255 {
                bail!(NativeInputFailure::PreconditionUnavailable);
            }
            if !key_admissible(&self.held_keys, (*scan, *vk, *extended), *down) {
                bail!(NativeInputFailure::PreconditionUnavailable);
            }
        }
        Ok(Some((
            Effect::Event(event.clone()),
            None,
            self.owner.clone(),
        )))
    }

    fn finish_apply(
        &mut self,
        done: Completion,
        result: Result<()>,
        started: Instant,
        owner: Option<&Arc<Owner>>,
    ) {
        let work = started.elapsed().as_micros().min(u64::MAX as u128) as u64;
        self.shared
            .stats
            .native_execution_us
            .fetch_add(work, Ordering::Relaxed);
        self.shared
            .stats
            .native_execution_peak_us
            .fetch_max(work, Ordering::Relaxed);
        self.shared
            .stats
            .native_commands
            .fetch_add(1, Ordering::Relaxed);
        if !done.send(result) {
            if let Some(owner) = owner {
                owner.cancel();
                if let Some(injector) = &self.injector {
                    injector.cleanup();
                }
                if Arc::ptr_eq(&self.owner, owner) {
                    self.reset();
                }
            }
        }
    }

    fn poll_injection(&mut self) {
        let Some(pending) = self.pending.as_ref() else {
            return;
        };
        let cleanup = matches!(pending.event, InputEvent::Leave | InputEvent::ReleaseAll);
        let invalid = pending.done.abandoned()
            || pending.epoch != self.shared.epoch.load(Ordering::Acquire)
            || (!cleanup
                && (!pending.owner.current()
                    || !self.epoch_is_current(pending.epoch)
                    || Instant::now() >= pending.deadline));
        let response = if invalid {
            Some(Err(if Instant::now() >= pending.deadline {
                anyhow!(NativeInputFailure::StaleCommand)
            } else {
                anyhow!(NativeInputFailure::PreconditionUnavailable)
            }))
        } else {
            match pending.receiver.try_recv() {
                Ok(result) => Some(result),
                Err(mpsc::TryRecvError::Empty) => None,
                Err(mpsc::TryRecvError::Disconnected) => {
                    Some(Err(anyhow!(NativeInputFailure::CommandQueueUnavailable)))
                }
            }
        };
        let Some(result) = response else { return };
        let pending = self.pending.take().unwrap();
        let result = result.and_then(|()| self.commit_remote(&pending));
        if result.is_err() {
            pending.owner.cancel();
            self.reset();
        }
        self.finish_apply(pending.done, result, pending.started, Some(&pending.owner));
        let _ = post_input_wake(&self.shared, unsafe { GetCurrentThreadId() });
    }

    fn commit_remote(&mut self, pending: &PendingApply) -> Result<()> {
        if matches!(pending.event, InputEvent::Leave | InputEvent::ReleaseAll) {
            return Ok(());
        }
        self.check_input_epoch(pending.epoch, pending.deadline)?;
        if !pending.owner.current() || !Arc::ptr_eq(&self.owner, &pending.owner) {
            bail!(NativeInputFailure::PreconditionUnavailable);
        }
        match &pending.event {
            InputEvent::Enter { from_left, .. } => {
                self.publish_desktop(on_input_desktop(&self.desktop_name));
                self.check_input_epoch(pending.epoch, pending.deadline)?;
                if !pending.owner.current() {
                    bail!(NativeInputFailure::PreconditionUnavailable);
                }
                let point = pending
                    .point
                    .context(NativeInputFailure::PreconditionUnavailable)?;
                self.receiving = true;
                self.reclaim_motion.clear();
                self.receiver_return.entered(*from_left, (point.x, point.y));
                self.shared.stats.receiving.store(true, Ordering::Release);
                self.update_overlay(point)?;
                self.check_input_epoch(pending.epoch, pending.deadline)?;
                if !pending.owner.current() {
                    bail!(NativeInputFailure::PreconditionUnavailable);
                }
                self.shared
                    .stats
                    .remote_enter_committed
                    .fetch_add(1, Ordering::Relaxed);
            }
            InputEvent::Move { dx, dy } => {
                if let Some(overlay) = &mut self.overlay {
                    overlay.movement();
                }
                self.receiver_return.motion(Instant::now(), *dx, *dy);
                self.shared
                    .stats
                    .injected_moves
                    .fetch_add(1, Ordering::Relaxed);
            }
            InputEvent::Button { button, down } => {
                self.held_buttons[button_index(*button)] = *down;
                self.shared
                    .stats
                    .injected_buttons
                    .fetch_add(1, Ordering::Relaxed);
            }
            InputEvent::Key {
                scan,
                vk,
                extended,
                down,
            } => {
                if *down {
                    self.held_keys.insert((*scan, *vk, *extended));
                } else {
                    self.held_keys.remove(&(*scan, *vk, *extended));
                }
                self.shared
                    .stats
                    .injected_keys
                    .fetch_add(1, Ordering::Relaxed);
            }
            _ => {}
        }
        self.check_input_epoch(pending.epoch, pending.deadline)?;
        Ok(())
    }
}

unsafe extern "system" fn mouse_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code < 0 {
        return CallNextHookEx(null_mut(), code, wparam, lparam);
    }
    let event = &*(lparam as *const MSLLHOOKSTRUCT);
    if event.flags & LLMHF_INJECTED != 0 || event.dwExtraInfo == INJECTED_TAG {
        return CallNextHookEx(null_mut(), code, wparam, lparam);
    }
    let suppress = HOOK.with(|cell| {
        let Ok(mut guard) = cell.try_borrow_mut() else {
            return false;
        };
        let Some(state) = guard.as_mut() else {
            return false;
        };
        state.sync_capture_epoch();
        state
            .shared
            .physical_input_serial
            .fetch_add(1, Ordering::AcqRel);
        let message = wparam as u32;
        state.mouse_delivery.hook(Instant::now(), event.time);
        // Relative raw motion decides deliberate receiver movement; screen-point
        // jumps include remote injection and cannot measure physical intent.
        if message != WM_MOUSEMOVE {
            let pending_action = matches!(
                message,
                WM_LBUTTONDOWN
                    | WM_RBUTTONDOWN
                    | WM_MBUTTONDOWN
                    | WM_XBUTTONDOWN
                    | WM_MOUSEWHEEL
                    | WM_MOUSEHWHEEL
            );
            if state.receiving || (pending_action && state.receiving_or_pending()) {
                state
                    .shared
                    .stats
                    .physical_mouse_reclaims
                    .fetch_add(1, Ordering::Relaxed);
                state.reclaim();
            }
        }
        let button = match message {
            WM_LBUTTONDOWN | WM_LBUTTONUP => Some((MouseButton::Left, message == WM_LBUTTONDOWN)),
            WM_RBUTTONDOWN | WM_RBUTTONUP => Some((MouseButton::Right, message == WM_RBUTTONDOWN)),
            WM_MBUTTONDOWN | WM_MBUTTONUP => Some((MouseButton::Middle, message == WM_MBUTTONDOWN)),
            WM_XBUTTONDOWN | WM_XBUTTONUP => Some((
                if event.mouseData >> 16 == XBUTTON1 as u32 {
                    MouseButton::Back
                } else {
                    MouseButton::Forward
                },
                message == WM_XBUTTONDOWN,
            )),
            _ => None,
        };
        if let Some((button, down)) = button {
            let index = button_index(button);
            let remaining = !down
                && state
                    .devices
                    .keep_button_hold(index, event.time, Instant::now());
            state
                .physical
                .button_with_remaining_hold(index, down, remaining);
            state.sync_physical_counts();
        }
        if !state.shared.ready() {
            return false;
        }
        if state.shared.remote.load(Ordering::Acquire) {
            if let Some((button, true)) = button {
                state
                    .physical
                    .protect_suppressed_button_down(button_index(button));
            }
            state.raw_health.suppressed(Instant::now(), event.time);
            // No hook-coordinate deltas or button forwarding: the ordered raw
            // stream supplies both. If raw delivery stalls, maintenance releases.
            return true;
        }
        false
    });
    if suppress {
        1
    } else {
        CallNextHookEx(null_mut(), code, wparam, lparam)
    }
}

unsafe extern "system" fn keyboard_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code < 0 {
        return CallNextHookEx(null_mut(), code, wparam, lparam);
    }
    let event = &*(lparam as *const KBDLLHOOKSTRUCT);
    if event.flags & LLKHF_INJECTED != 0 || event.dwExtraInfo == INJECTED_TAG {
        return CallNextHookEx(null_mut(), code, wparam, lparam);
    }
    let suppress = HOOK.with(|cell| {
        let Ok(mut guard) = cell.try_borrow_mut() else {
            return false;
        };
        let Some(state) = guard.as_mut() else {
            return false;
        };
        state.sync_capture_epoch();
        state
            .shared
            .physical_input_serial
            .fetch_add(1, Ordering::AcqRel);
        let down = matches!(wparam as u32, WM_KEYDOWN | WM_SYSKEYDOWN);
        state.keyboard_delivery.hook(Instant::now(), event.time);
        let vk = event.vkCode as u16;
        state
            .shared
            .stats
            .hook_key_events
            .fetch_add(1, Ordering::Relaxed);
        let remaining = !down && state.devices.keep_key_hold(vk, event.time, Instant::now());
        let update = state.physical.key_with_remaining_hold(
            vk,
            down,
            state.receiving_or_pending(),
            remaining,
        );
        state.sync_physical_counts();
        if update.reclaim {
            state
                .shared
                .stats
                .native_key_reclaims
                .fetch_add(1, Ordering::Relaxed);
            state.reclaim();
        } else if update.ignored_key_up_reclaim {
            state
                .shared
                .stats
                .key_up_reclaims_ignored
                .fetch_add(1, Ordering::Relaxed);
        }
        if update.panic {
            // Panic Escape is suppressed too; retain the physical hold guard
            // until UP even though its DOWN is deliberately not forwarded.
            state.physical.protect_suppressed_key_down(vk);
            state.reset();
            state.shared.queue_reset();
            return true;
        }
        if state.shared.ready()
            && state.shared.remote.load(Ordering::Acquire)
            && state.emit_action(InputEvent::Key {
                scan: event.scanCode as u16,
                vk,
                extended: event.flags & LLKHF_EXTENDED != 0,
                down,
            })
        {
            if down {
                state.physical.protect_suppressed_key_down(vk);
            }
            return true;
        }
        false
    });
    if suppress {
        1
    } else {
        CallNextHookEx(null_mut(), code, wparam, lparam)
    }
}

fn button_index(button: MouseButton) -> usize {
    match button {
        MouseButton::Left => 0,
        MouseButton::Right => 1,
        MouseButton::Middle => 2,
        MouseButton::Back => 3,
        MouseButton::Forward => 4,
    }
}

fn button_at(index: usize) -> MouseButton {
    match index {
        0 => MouseButton::Left,
        1 => MouseButton::Right,
        2 => MouseButton::Middle,
        3 => MouseButton::Back,
        _ => MouseButton::Forward,
    }
}

fn last_error(action: &'static str) -> anyhow::Error {
    anyhow::Error::new(std::io::Error::last_os_error()).context(action)
}

pub fn clipboard_sequence() -> u32 {
    unsafe { GetClipboardSequenceNumber() }
}

struct ClipboardGuard;
impl Drop for ClipboardGuard {
    fn drop(&mut self) {
        unsafe {
            CloseClipboard();
        }
    }
}

pub fn read_clipboard() -> Result<Option<String>> {
    unsafe {
        if IsClipboardFormatAvailable(CF_UNICODE_TEXT) == 0 {
            return Ok(None);
        }
        if OpenClipboard(null_mut()) == 0 {
            return Err(last_error("Open clipboard"));
        }
        let _guard = ClipboardGuard;
        let data = GetClipboardData(CF_UNICODE_TEXT);
        if data.is_null() {
            return Err(last_error("Read Unicode clipboard data"));
        }
        let size = GlobalSize(data);
        // This bound is applied before dereferencing or allocating clipboard data.
        if size > (MAX_CLIPBOARD_BYTES + 1) * 2 {
            bail!("Clipboard text exceeds 1 MiB");
        }
        if size < 2 || !size.is_multiple_of(2) {
            bail!("Invalid Unicode clipboard allocation");
        }
        let pointer = GlobalLock(data) as *const u16;
        if pointer.is_null() {
            return Err(last_error("Lock clipboard data"));
        }
        let units = std::slice::from_raw_parts(pointer, size / 2);
        let end = units.iter().position(|unit| *unit == 0);
        let text = end.map(|end| String::from_utf16(&units[..end]));
        GlobalUnlock(data);
        let text = text
            .context("Clipboard Unicode text has no terminator")?
            .context("Clipboard contains invalid Unicode text")?;
        if text.len() > MAX_CLIPBOARD_BYTES {
            bail!("Clipboard text exceeds 1 MiB");
        }
        Ok(Some(text))
    }
}

pub fn write_clipboard(text: &str) -> Result<()> {
    if text.len() > MAX_CLIPBOARD_BYTES {
        bail!("Clipboard text exceeds 1 MiB");
    }
    if text.contains('\0') {
        bail!("Clipboard text contains a NUL character");
    }
    let utf16: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        // EmptyClipboard requires an owning window for subsequent SetClipboardData.
        let class: Vec<u16> = "STATIC\0".encode_utf16().collect();
        let window = CreateWindowExW(
            0,
            class.as_ptr(),
            null(),
            0,
            0,
            0,
            0,
            0,
            HWND_MESSAGE,
            null_mut(),
            GetModuleHandleW(null()),
            null(),
        );
        if window.is_null() {
            return Err(last_error("Create clipboard owner"));
        }
        let result = write_clipboard_owned(window, &utf16);
        DestroyWindow(window);
        result
    }
}

unsafe fn write_clipboard_owned(
    window: windows_sys::Win32::Foundation::HWND,
    utf16: &[u16],
) -> Result<()> {
    let memory = GlobalAlloc(GMEM_MOVEABLE, std::mem::size_of_val(utf16));
    if memory.is_null() {
        return Err(last_error("Allocate clipboard text"));
    }
    let pointer = GlobalLock(memory) as *mut u16;
    if pointer.is_null() {
        GlobalFree(memory);
        return Err(last_error("Lock clipboard allocation"));
    }
    std::ptr::copy_nonoverlapping(utf16.as_ptr(), pointer, utf16.len());
    GlobalUnlock(memory);
    if OpenClipboard(window) == 0 {
        GlobalFree(memory);
        return Err(last_error("Open clipboard for writing"));
    }
    let _guard = ClipboardGuard;
    if EmptyClipboard() == 0 {
        GlobalFree(memory);
        return Err(last_error("Empty clipboard"));
    }
    if SetClipboardData(CF_UNICODE_TEXT, memory).is_null() {
        GlobalFree(memory);
        return Err(last_error("Set Unicode clipboard data"));
    }
    // Windows owns the allocation after SetClipboardData succeeds.
    Ok(())
}

pub fn protect_secret(bytes: &[u8]) -> Result<Vec<u8>> {
    crypt_secret(bytes, true)
}

pub fn unprotect_secret(bytes: &[u8]) -> Result<Vec<u8>> {
    crypt_secret(bytes, false)
}

fn crypt_secret(bytes: &[u8], protect: bool) -> Result<Vec<u8>> {
    let length: u32 = bytes
        .len()
        .try_into()
        .context("Secret too large for DPAPI")?;
    let input = CRYPT_INTEGER_BLOB {
        cbData: length,
        pbData: bytes.as_ptr() as *mut u8,
    };
    let mut output: CRYPT_INTEGER_BLOB = unsafe { zeroed() };
    let success = unsafe {
        if protect {
            // No CRYPTPROTECT_LOCAL_MACHINE: only this Windows user can decrypt.
            CryptProtectData(
                &input,
                null(),
                null(),
                null(),
                null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        } else {
            CryptUnprotectData(
                &input,
                null_mut(),
                null(),
                null(),
                null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        }
    };
    if success == 0 {
        return Err(last_error(if protect {
            "Protect identity using DPAPI"
        } else {
            "Decrypt identity using DPAPI"
        }));
    }
    let result =
        unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize) }.to_vec();
    unsafe {
        if !protect {
            // Clear the temporary native plaintext before returning it to Windows.
            for index in 0..output.cbData as usize {
                std::ptr::write_volatile(output.pbData.add(index), 0);
            }
        }
        LocalFree(output.pbData as *mut _);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    static RAW_REGISTRATION_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// No engine, live hook, native cursor provider, renderer, maintenance, or
    /// controller Drop is constructed. Only raw source evaluation is driven.
    fn synthetic_source(left: bool) -> (HookState, tokio::sync::mpsc::Receiver<CapturedInput>) {
        let (events, receiver) = tokio::sync::mpsc::channel(64);
        let shared = Arc::new(Shared {
            events,
            enabled: AtomicBool::new(true),
            edge_switching: AtomicBool::new(true),
            peer_on_left: AtomicBool::new(left),
            connected: AtomicBool::new(true),
            remote: AtomicBool::new(false),
            force_release: AtomicBool::new(false),
            reset_queued: AtomicU64::new(0),
            epoch: AtomicU64::new(17),
            input_desktop_available: AtomicBool::new(true),
            physical_input_serial: AtomicU64::new(0),
            wake_gate: WakeGate::default(),
            stats: Counters::default(),
        });
        shared
            .stats
            .raw_input_registered
            .store(true, Ordering::Relaxed);
        shared
            .stats
            .mouse_hook_installed
            .store(true, Ordering::Relaxed);
        shared
            .stats
            .keyboard_hook_installed
            .store(true, Ordering::Relaxed);
        let now = Instant::now();
        let monitor = Desktop {
            left: -1600,
            top: -200,
            width: 1600,
            height: 1200,
        };
        (
            HookState {
                shared,
                receiving: false,
                held_keys: BTreeSet::new(),
                held_buttons: [false; 5],
                physical: LocalInputState::default(),
                return_y: 0,
                edge_ready_at: now,
                layout: MonitorLayout::new(vec![monitor], monitor),
                overlay: None,
                raw_health: RawCaptureHealth::new(now),
                pending_motion: MotionBatch::default(),
                source_handoff: SourceHandoff::default(),
                receiver_return: ReceiverReturn::default(),
                desktop_name: Vec::new(),
                next_desktop_check: now,
                next_cursor_update: now,
                next_physical_check: now,
                mouse_delivery: HookDelivery::default(),
                keyboard_delivery: HookDelivery::default(),
                devices: DeviceLedger::default(),
                reclaim_motion: ReclaimMotion::default(),
                reinstall_hooks: false,
                hooks_available: true,
                next_hook_retry: now,
                raw_window: null_mut(),
                registration_audit: RegistrationAudit::new(now),
                input_desktop_available: true,
                capture_epoch: 17,
                next_raw_maintenance: now,
                injector: None,
                owner: Arc::new(Owner::default()),
                pending: None,
            },
            receiver,
        )
    }

    fn synthetic_mouse(dx: i32, dy: i32) -> RAWMOUSE {
        let mut mouse: RAWMOUSE = unsafe { zeroed() };
        mouse.lLastX = dx;
        mouse.lLastY = dy;
        mouse
    }

    const TEST_INJECTOR_UNBLOCK: u32 = WM_APP + 93;
    struct InjectorTestWindowState {
        unblock: SyncSender<()>,
        serviced: usize,
    }
    unsafe extern "system" fn injector_test_proc(
        window: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        if message == TEST_INJECTOR_UNBLOCK {
            let state =
                &mut *(GetWindowLongPtrW(window, GWLP_USERDATA) as *mut InjectorTestWindowState);
            state.serviced += 1;
            state.unblock.try_send(()).unwrap();
            return 0;
        }
        DefWindowProcW(window, message, wparam, lparam)
    }
    struct FakeInjector {
        effects: Arc<std::sync::Mutex<Vec<Effect>>>,
        mode: Arc<AtomicU64>,
        fail_up: Arc<AtomicBool>,
        fail_up_always: Arc<AtomicBool>,
        entered: SyncSender<()>,
        unblock: Receiver<()>,
        window: usize,
    }
    impl injection::Backend for FakeInjector {
        fn insert(&mut self, effect: &Effect) -> Result<()> {
            self.effects.lock().unwrap().push(effect.clone());
            let mode = self.mode.swap(0, Ordering::AcqRel);
            if mode != 0 {
                self.entered.try_send(()).unwrap();
                if mode == 1 {
                    anyhow::ensure!(
                        unsafe { PostMessageW(self.window as HWND, TEST_INJECTOR_UNBLOCK, 0, 0) }
                            != 0
                    );
                }
                self.unblock
                    .recv_timeout(Duration::from_secs(2))
                    .context("Synthetic owner-message dependency was not serviced")?;
            }
            if matches!(effect, Effect::Event(InputEvent::Key { down: false, .. }))
                && (self.fail_up_always.load(Ordering::Acquire)
                    || self.fail_up.swap(false, Ordering::AcqRel))
            {
                return Err(std::io::Error::other("Synthetic release failure").into());
            }
            Ok(())
        }
    }
    fn pump_fake_injector(state: &mut HookState, window: HWND) {
        let limit = Instant::now() + Duration::from_secs(1);
        while state.pending.is_some() && Instant::now() < limit {
            unsafe {
                let mut message: MSG = zeroed();
                while PeekMessageW(
                    &mut message,
                    window,
                    TEST_INJECTOR_UNBLOCK,
                    TEST_INJECTOR_UNBLOCK,
                    PM_REMOVE,
                ) != 0
                {
                    assert!(
                        dispatch_then_pump(
                            &message,
                            window,
                            TEST_INJECTOR_UNBLOCK,
                            TEST_INJECTOR_UNBLOCK
                        )
                        .0
                    );
                }
                if PeekMessageW(&mut message, null_mut(), WAKE, WAKE, PM_REMOVE) != 0 {
                    state.shared.wake_gate.received();
                }
            }
            state.poll_injection();
            thread::sleep(Duration::from_millis(1));
        }
        assert!(
            state.pending.is_none(),
            "Synthetic native completion did not finish"
        );
    }

    #[test]
    #[ignore = "Explicit Windows production executor contract; fake insertion only, no hooks/input/profile"]
    fn native_injection_executor_pumps_owner_messages_and_releases_cancelled_holds() {
        let _lock = RAW_REGISTRATION_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let (mut state, _captured) = synthetic_source(false);
        let effects = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mode = Arc::new(AtomicU64::new(1));
        let fail_up = Arc::new(AtomicBool::new(false));
        let fail_up_always = Arc::new(AtomicBool::new(false));
        let (entered, started) = mpsc::sync_channel(1);
        let (unblock, gate) = mpsc::sync_channel(1);
        let window = RegistrationTestWindow::new();
        let mut window_state = Box::new(InjectorTestWindowState {
            unblock: unblock.clone(),
            serviced: 0,
        });
        unsafe {
            SetWindowLongPtrW(
                window.0,
                GWLP_USERDATA,
                (&mut *window_state as *mut InjectorTestWindowState) as isize,
            );
            SetWindowLongPtrW(
                window.0,
                GWLP_WNDPROC,
                injector_test_proc as *const () as isize,
            );
        }
        state.injector = Some(
            Executor::with_backend(
                state.shared.clone(),
                unsafe { GetCurrentThreadId() },
                FakeInjector {
                    effects: effects.clone(),
                    mode: mode.clone(),
                    fail_up: fail_up.clone(),
                    fail_up_always: fail_up_always.clone(),
                    entered,
                    unblock: gate,
                    window: window.0 as usize,
                },
            )
            .unwrap(),
        );
        state.receiving = true;
        state.shared.stats.receiving.store(true, Ordering::Release);
        let wheel = |delta, horizontal| InputEvent::Wheel { delta, horizontal };
        let (done, reply) = mpsc::sync_channel(1);
        state.submit_remote(
            wheel(-7, false),
            17,
            Instant::now() + Duration::from_millis(200),
            Completion::Blocking(done),
            Instant::now(),
        );
        started.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(
            reply.try_recv().is_err(),
            "Enqueue is not actual completion"
        );
        assert!(state.pending.is_some());
        // Production injector waits for a synthetic message handled by this
        // owner thread; the owner can pump it while the backend is outstanding.
        pump_fake_injector(&mut state, window.0);
        reply.recv_timeout(Duration::from_secs(1)).unwrap().unwrap();
        assert_eq!(window_state.serviced, 1);
        for (delta, horizontal) in [(1, false), (240, false), (-3, true), (9, true)] {
            let (done, reply) = mpsc::sync_channel(1);
            state.submit_remote(
                wheel(delta, horizontal),
                17,
                Instant::now() + Duration::from_millis(200),
                Completion::Blocking(done),
                Instant::now(),
            );
            pump_fake_injector(&mut state, window.0);
            reply.recv_timeout(Duration::from_secs(1)).unwrap().unwrap();
        }
        let recorded: Vec<_> = effects
            .lock()
            .unwrap()
            .iter()
            .filter_map(|effect| match effect {
                Effect::Event(InputEvent::Wheel { delta, horizontal }) => {
                    Some((*delta, *horizontal))
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            recorded,
            vec![(-7, false), (1, false), (240, false), (-3, true), (9, true)]
        );

        // Original receipt budget can expire while insertion stays blocked.
        // The owner loop revokes/returns failure without waiting on the OS call.
        mode.store(2, Ordering::Release);
        let expiring_owner = state.owner.clone();
        let (done, reply) = mpsc::sync_channel(1);
        let original_deadline = Instant::now() + Duration::from_millis(40);
        state.submit_remote(
            wheel(15, false),
            17,
            original_deadline,
            Completion::Blocking(done),
            Instant::now(),
        );
        started.recv_timeout(Duration::from_secs(1)).unwrap();
        pump_fake_injector(&mut state, window.0);
        let error = reply
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .unwrap_err();
        assert_eq!(
            error.downcast_ref::<NativeInputFailure>(),
            Some(&NativeInputFailure::StaleCommand)
        );
        assert!(!state.receiving && !expiring_owner.current());
        let expired_down = state
            .injector
            .as_ref()
            .unwrap()
            .submit(
                Effect::Event(InputEvent::Key {
                    scan: 99,
                    vk: 99,
                    extended: false,
                    down: true,
                }),
                expiring_owner,
                17,
                original_deadline,
            )
            .unwrap();
        unblock.try_send(()).unwrap();
        assert!(expired_down
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .is_err());
        assert!(!effects
            .lock()
            .unwrap()
            .iter()
            .any(|e| matches!(e, Effect::Event(InputEvent::Key { scan: 99, .. }))));
        state.receiving = true;
        state.shared.stats.receiving.store(true, Ordering::Release);

        // A Down accepted after local cancellation is recorded and released by
        // the worker before another token's work can be processed.
        mode.store(2, Ordering::Release);
        let old = state.owner.clone();
        let key = InputEvent::Key {
            scan: 42,
            vk: 42,
            extended: false,
            down: true,
        };
        let (done, reply) = mpsc::sync_channel(1);
        state.submit_remote(
            key.clone(),
            17,
            Instant::now() + Duration::from_millis(200),
            Completion::Blocking(done),
            Instant::now(),
        );
        started.recv_timeout(Duration::from_secs(1)).unwrap();
        let stale = state
            .injector
            .as_ref()
            .unwrap()
            .submit(
                Effect::Event(wheel(999, false)),
                old.clone(),
                17,
                Instant::now() - Duration::from_millis(1),
            )
            .unwrap();
        state.reclaim();
        state.poll_injection();
        assert!(reply.recv_timeout(Duration::from_secs(1)).unwrap().is_err());
        assert!(!state.receiving && !old.current());
        unblock.try_send(()).unwrap();
        assert!(stale.recv_timeout(Duration::from_secs(1)).unwrap().is_err());
        state.receiving = true;
        state.shared.stats.receiving.store(true, Ordering::Release);
        let (done, reply) = mpsc::sync_channel(1);
        state.submit_remote(
            wheel(11, false),
            17,
            Instant::now() + Duration::from_millis(200),
            Completion::Blocking(done),
            Instant::now(),
        );
        pump_fake_injector(&mut state, window.0);
        reply.recv_timeout(Duration::from_secs(1)).unwrap().unwrap();
        let events = effects.lock().unwrap().clone();
        let down = events
            .iter()
            .position(|e| matches!(e, Effect::Event(InputEvent::Key { down: true, .. })))
            .unwrap();
        assert!(matches!(
            events[down + 1],
            Effect::Event(InputEvent::Key { down: false, .. })
        ));
        assert!(matches!(
            events[down + 2],
            Effect::Event(InputEvent::Wheel { delta: 11, .. })
        ));
        assert!(!events
            .iter()
            .any(|e| matches!(e, Effect::Event(InputEvent::Wheel { delta: 999, .. }))));

        // Old A cleanup/expired jobs with abandoned replies cannot release B.
        let owner_b = state.owner.clone();
        let injector = state.injector.as_ref().unwrap();
        injector
            .submit(
                Effect::Event(key.clone()),
                owner_b.clone(),
                17,
                Instant::now() + Duration::from_millis(200),
            )
            .unwrap()
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .unwrap();
        let before = effects.lock().unwrap().len();
        drop(
            injector
                .submit(Effect::Release, old.clone(), 17, Instant::now())
                .unwrap(),
        );
        drop(
            injector
                .submit(
                    Effect::Event(wheel(888, false)),
                    old,
                    17,
                    Instant::now() - Duration::from_millis(1),
                )
                .unwrap(),
        );
        injector
            .submit(
                Effect::Event(wheel(12, false)),
                owner_b.clone(),
                17,
                Instant::now() + Duration::from_millis(200),
            )
            .unwrap()
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .unwrap();
        assert!(injector.has_held());
        assert_eq!(effects.lock().unwrap().len(), before + 1);
        fail_up.store(true, Ordering::Release);
        assert!(injector
            .submit(Effect::Release, owner_b.clone(), 17, Instant::now())
            .unwrap()
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .is_err());
        assert!(injector.has_held(), "Failed UP must stay in actual ledger");
        injector
            .submit(Effect::Release, owner_b, 17, Instant::now())
            .unwrap()
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .unwrap();
        assert!(!injector.has_held());

        // A new hook token's cleanup cannot claim success while a revoked
        // worker ledger still retains keys after persistent UP failures.
        let injector = state.injector.as_ref().unwrap();
        let revoked = state.owner.clone();
        injector
            .submit(
                Effect::Event(key.clone()),
                revoked.clone(),
                17,
                Instant::now() + Duration::from_millis(200),
            )
            .unwrap()
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .unwrap();
        fail_up_always.store(true, Ordering::Release);
        revoked.cancel();
        injector.cleanup();
        let replacement = Arc::new(Owner::default());
        assert!(injector
            .submit(Effect::Release, replacement.clone(), 17, Instant::now())
            .unwrap()
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .is_err());
        assert!(injector.has_held());
        fail_up_always.store(false, Ordering::Release);
        injector
            .submit(Effect::Release, replacement.clone(), 17, Instant::now())
            .unwrap()
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .unwrap();
        assert!(!injector.has_held());
        state.owner = replacement;

        // Pending Enter must be reclaimable before receiving=true. Use the
        // production prepared-submit/ack bridge with only synthetic geometry.
        state.receiving = false;
        state.shared.stats.receiving.store(false, Ordering::Release);
        mode.store(2, Ordering::Release);
        let held_worker = state
            .injector
            .as_ref()
            .unwrap()
            .submit(
                Effect::Event(wheel(211, false)),
                state.owner.clone(),
                17,
                Instant::now() + Duration::from_millis(200),
            )
            .unwrap();
        started.recv_timeout(Duration::from_secs(1)).unwrap();
        state.shared.remote.store(true, Ordering::Release);
        state.prepare_enter_owner(); // Enqueues then supersedes source Restore.
        mode.store(2, Ordering::Release);
        let (done, reply) = mpsc::sync_channel(1);
        state.submit_prepared(
            InputEvent::Enter {
                from_left: true,
                y: 30000,
            },
            Effect::Warp(-1588, 400, state.layout.bounds),
            Some(POINT { x: -1588, y: 400 }),
            state.owner.clone(),
            17,
            Instant::now() + Duration::from_millis(200),
            Completion::Blocking(done),
            Instant::now(),
        );
        unblock.try_send(()).unwrap();
        assert!(held_worker
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .is_err());
        started.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(!effects
            .lock()
            .unwrap()
            .iter()
            .any(|effect| matches!(effect, Effect::Restore { .. })));
        assert!(!state.receiving && state.receiving_or_pending());
        state.reclaim();
        state.poll_injection();
        assert!(reply.recv_timeout(Duration::from_secs(1)).unwrap().is_err());
        unblock.try_send(()).unwrap();
        assert!(
            !state.receiving
                && !state
                    .shared
                    .stats
                    .cursor_overlay_visible
                    .load(Ordering::Relaxed)
        );

        // Accepted RAW actions alone revoke pending Enter when no LL callback
        // arrives. A raw cleanup UP alone does not reclaim; source wheel still
        // forwards unchanged and never invokes receiver reclaim.
        for physical in ["wheel", "key"] {
            state.prepare_enter_owner();
            mode.store(2, Ordering::Release);
            let (done, reply) = mpsc::sync_channel(1);
            state.submit_prepared(
                InputEvent::Enter {
                    from_left: true,
                    y: 30000,
                },
                Effect::Warp(-1588, 400, state.layout.bounds),
                Some(POINT { x: -1588, y: 400 }),
                state.owner.clone(),
                17,
                Instant::now() + Duration::from_millis(200),
                Completion::Blocking(done),
                Instant::now(),
            );
            started.recv_timeout(Duration::from_secs(1)).unwrap();
            let owner = state.owner.clone();
            let mut keyboard: RAWKEYBOARD = unsafe { zeroed() };
            keyboard.VKey = 65;
            keyboard.MakeCode = 30;
            keyboard.Flags = RI_KEY_BREAK as u16;
            state.raw_keyboard(keyboard, 1, 1);
            assert!(owner.current(), "Cleanup UP cannot revoke pending Enter");
            if physical == "wheel" {
                let mut raw = synthetic_mouse(0, 0);
                raw.Anonymous.ulButtons = RI_MOUSE_WHEEL | ((-7i16 as u16 as u32) << 16);
                state.raw_mouse_with_cursor(raw, 2, 1, || panic!("Raw wheel cannot enter source"));
            } else {
                keyboard.Flags = 0;
                state.raw_keyboard(keyboard, 2, 1);
                keyboard.Flags = RI_KEY_BREAK as u16;
                state.raw_keyboard(keyboard, 3, 1);
            }
            state.poll_injection();
            assert!(!owner.current() && !state.receiving);
            assert!(reply.recv_timeout(Duration::from_secs(1)).unwrap().is_err());
            unblock.try_send(()).unwrap();
        }
        let (mut source, mut captured) = synthetic_source(false);
        source.shared.remote.store(true, Ordering::Release);
        let mut raw = synthetic_mouse(0, 0);
        raw.Anonymous.ulButtons = RI_MOUSE_WHEEL | ((-7i16 as u16 as u32) << 16);
        source.raw_mouse_with_cursor(raw, 1, 1, || panic!("Already source: no cursor query"));
        assert!(source.shared.remote.load(Ordering::Acquire));
        assert!(matches!(
            captured.try_recv().unwrap().event,
            InputEvent::Wheel {
                delta: -7,
                horizontal: false
            }
        ));
        assert_eq!(
            source
                .shared
                .stats
                .physical_mouse_reclaims
                .load(Ordering::Relaxed),
            0
        );
        source.shared.remote.store(false, Ordering::Release);

        // Completion receiver loss after a successful commit revokes its owner.
        state.receiving = true;
        let committed_owner = state.owner.clone();
        let (done, reply) = mpsc::sync_channel(1);
        drop(reply);
        state.finish_apply(
            Completion::Blocking(done),
            Ok(()),
            Instant::now(),
            Some(&committed_owner),
        );
        assert!(!committed_owner.current() && !state.receiving);
        // Idle actual held state retires on Shared revocation independently
        // of a delayed hook owner; a legitimate hold has no 200ms expiry.
        let idle_owner = state.owner.clone();
        let injector = state.injector.as_ref().unwrap();
        injector
            .submit(
                Effect::Event(key.clone()),
                idle_owner.clone(),
                17,
                Instant::now() + Duration::from_millis(200),
            )
            .unwrap()
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .unwrap();
        assert!(injector.has_held());
        let before_revoke = effects.lock().unwrap().len();
        state.shared.epoch.fetch_add(1, Ordering::AcqRel);
        let limit = Instant::now() + Duration::from_secs(1);
        while injector.has_held() && Instant::now() < limit {
            thread::sleep(Duration::from_millis(1));
        }
        assert!(
            idle_owner.current(),
            "No owner cancellation/hook pump was used"
        );
        assert!(!injector.has_held());
        assert!(effects.lock().unwrap()[before_revoke..]
            .iter()
            .any(|effect| matches!(effect, Effect::Event(InputEvent::Key { down: false, .. }))));
        state.capture_epoch = 18;
        state.owner = Arc::new(Owner::default());

        // Shutdown while a syscall is outstanding cannot start another queued
        // action, even if epoch/ready/token remain otherwise valid.
        mode.store(2, Ordering::Release);
        let final_owner = state.owner.clone();
        let injector = state.injector.as_ref().unwrap();
        let final_down = injector
            .submit(
                Effect::Event(key),
                final_owner.clone(),
                18,
                Instant::now() + Duration::from_millis(200),
            )
            .unwrap();
        started.recv_timeout(Duration::from_secs(1)).unwrap();
        let never = injector
            .submit(
                Effect::Event(wheel(777, false)),
                final_owner,
                18,
                Instant::now() + Duration::from_millis(200),
            )
            .unwrap();
        state.shared.remote.store(false, Ordering::Release);
        state.injector.take(); // Nonblocking shutdown; no native backend exists.
        unblock.try_send(()).unwrap();
        assert!(final_down
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .is_err());
        assert!(matches!(
            never.recv_timeout(Duration::from_secs(1)),
            Err(mpsc::RecvTimeoutError::Disconnected)
        ));
        assert!(!effects
            .lock()
            .unwrap()
            .iter()
            .any(|effect| matches!(effect, Effect::Event(InputEvent::Wheel { delta: 777, .. }))));
        // Second-job disconnection proves run() dropped its queue after its
        // final wake. Drain only the private wakes posted by this fixture.
        unsafe {
            let mut message: MSG = zeroed();
            while PeekMessageW(&mut message, null_mut(), WAKE, WAKE, PM_REMOVE) != 0 {}
        }
        state.shared.wake_gate.received();
        drop(window);
    }

    #[test]
    #[ignore = "Explicit Windows production raw-source contract; mock cursor, no hooks or injection"]
    fn native_source_edge_raw_first_clamped_cursor_uses_production_admission() {
        let _lock = RAW_REGISTRATION_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        for left in [false, true] {
            let (mut state, mut received) = synthetic_source(left);
            let x = if left { -1600 } else { -1 };
            let dx = if left { -1 } else { 1 };
            let mut queries = 0;
            let mut cursor = || {
                queries += 1;
                Ok(POINT { x, y: 400 })
            };
            // No mouse_hook or SourceHandoff hook candidate is ever invoked.
            state.raw_mouse_with_cursor(synthetic_mouse(dx, dx), 1, 1, &mut cursor);
            assert!(!state.shared.remote.load(Ordering::Acquire));
            assert!(received.try_recv().is_err());
            // Slow/clamped raw-only motion must not require a fresh 50ms hook.
            thread::sleep(Duration::from_millis(60));
            state.raw_mouse_with_cursor(synthetic_mouse(0, 0), 61, 1, &mut cursor);
            state.raw_mouse_with_cursor(synthetic_mouse(0, 30), 62, 1, &mut cursor);
            state.raw_mouse_with_cursor(synthetic_mouse(dx, 30), 63, 1, &mut cursor);
            assert!(!state.shared.remote.load(Ordering::Acquire));
            state.raw_mouse_with_cursor(synthetic_mouse(2 * dx, 2), 64, 1, &mut cursor);
            assert_eq!(
                queries, 5,
                "Exactly one authorized cursor sample per local raw packet"
            );
            assert!(state.shared.remote.load(Ordering::Acquire));
            let entered = received.try_recv().unwrap();
            assert_eq!(entered.epoch, 17);
            assert!(matches!(entered.event, InputEvent::Enter { from_left, y }
                if from_left == !left && y == state.layout.bounds.normalize_y(400)));
            assert!(
                received.try_recv().is_err(),
                "Transition motion stays local"
            );
            state.raw_mouse_with_cursor(synthetic_mouse(3 * dx, 0), 65, 1, || {
                panic!("Already forwarding: no local source cursor query")
            });
            assert!(matches!(received.try_recv().unwrap().event,
                InputEvent::Move { dx: delivered, dy: 0 } if delivered == 3 * dx));
            assert!(received.try_recv().is_err());
            let stats = state.shared.stats.snapshot(true);
            assert_eq!(stats.source_edge_samples, 5);
            assert_eq!(stats.source_edge_candidates, 5);
            assert_eq!(stats.source_edge_intent_ready, 1);
            assert_eq!(stats.source_enter_committed, 1);
        }

        // Current geometry/holds/native gates are exercised through the same
        // production RAWMOUSE path, not just the standalone pressure policy.
        for gate in [
            "interior", "corner", "held", "desktop", "raw", "hooks", "policy", "absolute", "tagged",
        ] {
            let (mut state, mut received) = synthetic_source(true);
            let mut point = POINT { x: -1600, y: 400 };
            let mut mouse = synthetic_mouse(-20, 0);
            match gate {
                "interior" => point.x += 1,
                "corner" => point.y = -200,
                "held" => {
                    state.physical.key(VK_CONTROL, true, false);
                }
                "desktop" => state.input_desktop_available = false,
                "raw" => state
                    .shared
                    .stats
                    .raw_input_registered
                    .store(false, Ordering::Relaxed),
                "hooks" => state.hooks_available = false,
                "policy" => state.shared.edge_switching.store(false, Ordering::Release),
                "absolute" => mouse.usFlags = MOUSE_MOVE_ABSOLUTE,
                "tagged" => mouse.ulExtraInformation = INJECTED_TAG as u32,
                _ => unreachable!(),
            }
            state.raw_mouse_with_cursor(mouse, 1, 1, || Ok(point));
            assert!(!state.shared.remote.load(Ordering::Acquire), "Gate: {gate}");
            assert!(received.try_recv().is_err(), "Gate: {gate}");
        }
        for change in ["epoch", "enabled", "edge_policy", "side", "connected"] {
            let (mut state, mut received) = synthetic_source(true);
            let shared = state.shared.clone();
            state.raw_mouse_with_cursor(synthetic_mouse(-20, 0), 1, 1, || {
                match change {
                    "epoch" => {
                        shared.epoch.fetch_add(1, Ordering::AcqRel);
                    }
                    "enabled" => shared.enabled.store(false, Ordering::Release),
                    "edge_policy" => shared.edge_switching.store(false, Ordering::Release),
                    "side" => shared.peer_on_left.store(false, Ordering::Release),
                    "connected" => shared.connected.store(false, Ordering::Release),
                    _ => unreachable!(),
                }
                Ok(POINT { x: -1600, y: 400 })
            });
            assert!(
                !state.shared.remote.load(Ordering::Acquire),
                "Revoked during query: {change}"
            );
            assert!(
                received.try_recv().is_err(),
                "Revoked during query: {change}"
            );
            assert_eq!(
                state
                    .shared
                    .stats
                    .source_enter_committed
                    .load(Ordering::Relaxed),
                0
            );
        }
        for refusal in ["cursor", "held", "policy", "desktop", "hooks", "absolute"] {
            let (mut state, mut received) = synthetic_source(true);
            let point = POINT { x: -1600, y: 400 };
            state.raw_mouse_with_cursor(synthetic_mouse(-2, 0), 1, 1, || Ok(point));
            assert!(!state.shared.remote.load(Ordering::Acquire));
            match refusal {
                "held" => {
                    state.physical.key(VK_CONTROL, true, false);
                }
                "policy" => state.shared.edge_switching.store(false, Ordering::Release),
                "desktop" => state.input_desktop_available = false,
                "hooks" => state.hooks_available = false,
                "cursor" | "absolute" => {}
                _ => unreachable!(),
            }
            let mut refused_packet = synthetic_mouse(0, 10);
            if refusal == "absolute" {
                refused_packet.usFlags = MOUSE_MOVE_ABSOLUTE;
            }
            state.raw_mouse_with_cursor(refused_packet, 2, 1, || {
                if refusal == "cursor" {
                    Err(anyhow!("Synthetic unavailable cursor"))
                } else {
                    Ok(point)
                }
            });
            state.physical.key(VK_CONTROL, false, false);
            state.shared.edge_switching.store(true, Ordering::Release);
            state.input_desktop_available = true;
            state.hooks_available = true;
            state.raw_mouse_with_cursor(synthetic_mouse(-2, 0), 3, 1, || Ok(point));
            assert!(
                !state.shared.remote.load(Ordering::Acquire),
                "Partial pressure survived {refusal}"
            );
            assert!(received.try_recv().is_err());
            state.raw_mouse_with_cursor(synthetic_mouse(-2, 0), 4, 1, || Ok(point));
            assert!(state.shared.remote.load(Ordering::Acquire));
            assert!(matches!(
                received.try_recv().unwrap().event,
                InputEvent::Enter { .. }
            ));
        }
    }

    const TEST_PUMP_MARKER: u32 = WM_APP + 91;
    unsafe extern "system" fn pump_test_proc(
        window: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        if message == TEST_PUMP_MARKER {
            let observed = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Vec<u32>;
            if !observed.is_null() {
                (*observed).push(wparam as u32);
            }
            return 0;
        }
        DefWindowProcW(window, message, wparam, lparam)
    }

    #[test]
    #[ignore = "Explicit isolated Windows pump contract; synthetic commands/messages only"]
    fn native_input_pump_coalesces_wakes_and_preserves_original_message_order() {
        let _lock = RAW_REGISTRATION_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut observed = Box::new(Vec::<u32>::new());
        let window = RegistrationTestWindow::new();
        unsafe {
            SetWindowLongPtrW(
                window.0,
                GWLP_USERDATA,
                (&mut *observed as *mut Vec<u32>) as isize,
            );
            SetWindowLongPtrW(window.0, GWLP_WNDPROC, pump_test_proc as *const () as isize);
            let thread = GetCurrentThreadId();
            let gate = WakeGate::default();
            let mut message: MSG = zeroed();
            PeekMessageW(&mut message, null_mut(), 0, 0, PM_NOREMOVE);
            for _ in 0..10_000 {
                post_coalesced_wake(&gate, thread).unwrap();
            }
            assert!(PeekMessageW(&mut message, null_mut(), WAKE, WAKE, PM_REMOVE) != 0);
            gate.received();
            assert_eq!(
                PeekMessageW(&mut message, null_mut(), WAKE, WAKE, PM_REMOVE),
                0
            );
            let (producer, consumer) = mpsc::sync_channel(32);
            for value in 0..32 {
                producer.try_send(value).unwrap();
            }
            let mut slice = WorkSlice::new(Instant::now());
            let mut applied = 0;
            while let Ok(value) = consumer.try_recv() {
                applied += 1;
                producer.try_send(value + 1).unwrap();
                post_coalesced_wake(&gate, thread).unwrap();
                if slice.complete(Instant::now()) {
                    break;
                }
            }
            assert!((1..=32).contains(&applied));
            assert!(consumer.try_recv().is_ok()); // Producer refilled throughout.
            for marker in 1..=3 {
                assert_ne!(PostMessageW(window.0, TEST_PUMP_MARKER, marker, 0), 0);
            }
            assert!(
                PeekMessageW(
                    &mut message,
                    window.0,
                    TEST_PUMP_MARKER,
                    TEST_PUMP_MARKER,
                    PM_REMOVE
                ) != 0
            );
            // Actual production ordering helper must dispatch original A before
            // draining later B/C, even with a queued wake/command continuation.
            assert!(dispatch_then_pump(&message, window.0, TEST_PUMP_MARKER, TEST_PUMP_MARKER).0);
            assert_eq!(&*observed, &[1, 2, 3]);
            post_coalesced_wake(&gate, thread).unwrap(); // Coalesced continuation.
            assert!(PeekMessageW(&mut message, null_mut(), WAKE, WAKE, PM_REMOVE) != 0);
            gate.received();
            assert_eq!(
                PeekMessageW(&mut message, null_mut(), WAKE, WAKE, PM_REMOVE),
                0
            );
            post_coalesced_wake(&gate, thread).unwrap(); // New producer after ack.
            assert!(PeekMessageW(&mut message, null_mut(), WAKE, WAKE, PM_REMOVE) != 0);
            gate.received();
        }
        drop(window);
    }

    #[test]
    #[ignore = "Explicit isolated Windows original-deadline contract; no native input"]
    fn native_input_command_preserves_original_deadline_before_synthetic_execution() {
        let _lock = RAW_REGISTRATION_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let window = RegistrationTestWindow::new();
        let original_deadline = Instant::now() - Duration::from_millis(1);
        let native_enqueue = Instant::now();
        let (producer, consumer) = mpsc::sync_channel(1);
        producer.send((original_deadline, native_enqueue)).unwrap();
        let gate = WakeGate::default();
        unsafe {
            let mut message: MSG = zeroed();
            PeekMessageW(&mut message, null_mut(), 0, 0, PM_NOREMOVE);
            post_coalesced_wake(&gate, GetCurrentThreadId()).unwrap();
            assert!(PeekMessageW(&mut message, null_mut(), WAKE, WAKE, PM_REMOVE) != 0);
            gate.received();
            assert!(dispatch_then_pump(&message, window.0, TEST_PUMP_MARKER, TEST_PUMP_MARKER).0);
        }
        let (deadline, queued_at) = consumer.recv().unwrap();
        assert!(queued_at > deadline);
        let mut synthetic_applies = 0;
        let result =
            check_deadline(Instant::now(), deadline, false).map(|()| synthetic_applies += 1);
        assert_eq!(
            result.unwrap_err().downcast_ref::<NativeInputFailure>(),
            Some(&NativeInputFailure::StaleCommand)
        );
        assert_eq!(synthetic_applies, 0);
        check_deadline(Instant::now(), deadline, true).unwrap(); // Cleanup stays permitted.
        drop(window);
    }

    struct RegistrationTestWindow(HWND);
    impl RegistrationTestWindow {
        fn new() -> Self {
            unsafe {
                // This test procedure only discards native window messages.
                // It never parses WM_INPUT, installs hooks, or creates an engine.
                let class: Vec<u16> = "EMC.IsolatedRawRegistration.Test"
                    .encode_utf16()
                    .chain(Some(0))
                    .collect();
                let instance = GetModuleHandleW(null());
                let definition = WNDCLASSW {
                    lpfnWndProc: Some(DefWindowProcW),
                    hInstance: instance,
                    lpszClassName: class.as_ptr(),
                    ..zeroed()
                };
                RegisterClassW(&definition);
                let window = CreateWindowExW(
                    WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW,
                    class.as_ptr(),
                    null(),
                    WS_POPUP,
                    0,
                    0,
                    0,
                    0,
                    null_mut(),
                    null_mut(),
                    instance,
                    null(),
                );
                assert!(
                    !window.is_null(),
                    "Create isolated hidden test window: {}",
                    std::io::Error::last_os_error()
                );
                Self(window)
            }
        }
        fn register(&self) {
            self.register_with_flags(RIDEV_INPUTSINK | RIDEV_DEVNOTIFY);
        }
        fn register_with_flags(&self, flags: u32) {
            let devices = raw_devices(self.0, flags);
            assert_ne!(
                unsafe {
                    RegisterRawInputDevices(
                        devices.as_ptr(),
                        devices.len() as u32,
                        size_of::<RAWINPUTDEVICE>() as u32,
                    )
                },
                0,
                "Register isolated raw window: {}",
                std::io::Error::last_os_error()
            );
        }
        fn pump_discarded_messages(&self) {
            unsafe {
                let mut message: MSG = zeroed();
                while PeekMessageW(&mut message, self.0, 0, 0, PM_REMOVE) != 0 {
                    DispatchMessageW(&message);
                }
            }
        }
    }
    impl Drop for RegistrationTestWindow {
        fn drop(&mut self) {
            unregister_raw_window(self.0);
            unsafe {
                DestroyWindow(self.0);
            }
        }
    }

    fn assert_no_existing_raw_test_targets() {
        let entries =
            registered_raw_devices().expect("Read isolated process registration snapshot");
        assert!(
            !entries
                .iter()
                .any(|entry| entry.usUsagePage == 1 && matches!(entry.usUsage, 2 | 6)),
            "Run only this named test in an isolated test process without a native engine"
        );
    }

    #[test]
    #[ignore = "Explicit isolated Windows registration test; no hooks or input injection"]
    fn native_raw_registration_roundtrip_normalizes_flags_for_eleven_seconds() {
        let _lock = RAW_REGISTRATION_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert_no_existing_raw_test_targets();
        assert_eq!(super::super::raw_registration::INPUT_SINK, RIDEV_INPUTSINK);
        assert_eq!(
            super::super::raw_registration::DEVICE_NOTIFY,
            RIDEV_DEVNOTIFY
        );
        let window = RegistrationTestWindow::new();
        window.register();
        for observation in 0..=10 {
            window.pump_discarded_messages();
            let entries = registered_raw_devices().expect("Read back isolated registration");
            let relevant: Vec<_> = entries
                .iter()
                .filter(|entry| entry.usUsagePage == 1 && matches!(entry.usUsage, 2 | 6))
                .collect();
            assert_eq!(relevant.len(), 2);
            let old_exact_predicate = relevant.iter().all(|entry| {
                entry.hwndTarget == window.0 && entry.dwFlags == RIDEV_INPUTSINK | RIDEV_DEVNOTIFY
            });
            let notification_omitted = relevant
                .iter()
                .any(|entry| entry.dwFlags & RIDEV_DEVNOTIFY == 0);
            if notification_omitted {
                assert!(
                    !old_exact_predicate,
                    "Old exact echo predicate must reject omitted notification flags"
                );
            }
            if observation == 0 {
                for entry in &relevant {
                    println!(
                        "usage={} requested_flags={:#x} reported_flags={:#x}",
                        entry.usUsage,
                        RIDEV_INPUTSINK | RIDEV_DEVNOTIFY,
                        entry.dwFlags
                    );
                }
                println!("old_exact_predicate={old_exact_predicate} notification_omitted={notification_omitted}");
            }
            assert_eq!(owns_raw_registration(window.0), RegistrationStatus::Owned);
            if observation < 10 {
                std::thread::sleep(Duration::from_millis(1100));
            }
        }
        drop(window);
        assert_no_existing_raw_test_targets();
    }

    #[test]
    #[ignore = "Explicit isolated Windows target replacement test; no hooks or input injection"]
    fn native_raw_registration_rejects_replacement_target_and_missing_sink() {
        let _lock = RAW_REGISTRATION_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert_no_existing_raw_test_targets();
        let first = RegistrationTestWindow::new();
        let replacement = RegistrationTestWindow::new();
        first.register();
        assert_eq!(owns_raw_registration(first.0), RegistrationStatus::Owned);
        assert_eq!(
            owns_raw_registration(replacement.0),
            RegistrationStatus::Mismatch
        );
        replacement.register();
        assert_eq!(owns_raw_registration(first.0), RegistrationStatus::Mismatch);
        assert_eq!(
            owns_raw_registration(replacement.0),
            RegistrationStatus::Owned
        );
        replacement.register_with_flags(RIDEV_DEVNOTIFY);
        assert_eq!(
            owns_raw_registration(replacement.0),
            RegistrationStatus::Mismatch
        );
        replacement.register();
        assert_eq!(
            owns_raw_registration(replacement.0),
            RegistrationStatus::Owned
        );
        drop(first); // Must not unregister another window's current ownership.
        assert_eq!(
            owns_raw_registration(replacement.0),
            RegistrationStatus::Owned
        );
        drop(replacement);
        assert_no_existing_raw_test_targets();
    }

    #[test]
    fn dpapi_identity_roundtrip_and_tamper_rejection() {
        let secret = b"test-only identity material; never a real private key";
        let mut protected = protect_secret(secret).expect("Protect using current-user DPAPI");
        assert_ne!(protected, secret);
        assert_eq!(unprotect_secret(&protected).unwrap(), secret);
        let last = protected.len() - 1;
        protected[last] ^= 0x80;
        assert!(unprotect_secret(&protected).is_err());
    }

    #[test]
    fn clipboard_rejects_oversized_text_before_touching_os() {
        assert!(write_clipboard(&"x".repeat(MAX_CLIPBOARD_BYTES + 1)).is_err());
        assert!(write_clipboard("embedded\0terminator").is_err());
    }
}

//! Receiving-only fallback renderer. The input thread only changes atomic
//! lifecycle state; every HWND, borrowed OS cursor, and GDI object stays on the
//! renderer's own native UI thread. No global cursor/Mouse Keys state is changed.
use crate::cursor_model::{
    shape_candidate, CursorGeometry, CursorLifecycle, CursorRequirement, CursorSamplingHealth,
    LatestWake, RetryBackoff,
};
use anyhow::{Context, Result};
use std::{
    mem::{size_of, zeroed},
    ptr::{null, null_mut},
    sync::{
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
        Arc,
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM},
    Graphics::Gdi::*,
    System::{LibraryLoader::GetModuleHandleW, Threading::GetCurrentThreadId},
    UI::{
        HiDpi::{
            GetDpiForWindow, SetThreadDpiAwarenessContext,
            DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        },
        WindowsAndMessaging::*,
    },
};

const CLASS: &str = "EMC.ReceivingCursor.v3";
const KEY_COLOR: u32 = 0x00ff00ff;
const WAKE: u32 = WM_APP + 43;
const TIMER_INTERVAL_MS: u32 = 8;
// Handles can be recycled and application-owned cursor artwork can change in
// place. Refresh the owned copy even when the observed handle stays the same.
const CACHE_LIFETIME: Duration = Duration::from_millis(250);

struct Clock(Instant);
impl Default for Clock {
    fn default() -> Self {
        Self(Instant::now())
    }
}

#[derive(Default)]
struct Shared {
    lifecycle: CursorLifecycle,
    stopping: AtomicBool,
    failed: AtomicBool,
    window_ready: AtomicBool,
    health: CursorSamplingHealth,
    wake_pending: LatestWake,
    thread_id: AtomicU32,
    clock: Clock,
    last_progress_ms: AtomicU64,
    visible: AtomicBool,
    activations: AtomicU64,
    shape_changes: AtomicU64,
    work_us: AtomicU64,
    failures: AtomicU64,
    recovery_pending: AtomicBool,
    recoveries: AtomicU64,
}
impl Shared {
    fn elapsed_ms(&self) -> u64 {
        self.clock.0.elapsed().as_millis().min(u64::MAX as u128) as u64
    }
    fn progressed(&self) {
        self.last_progress_ms
            .store(self.elapsed_ms(), Ordering::Release);
    }
    fn progress_age(&self) -> Duration {
        Duration::from_millis(
            self.elapsed_ms()
                .saturating_sub(self.last_progress_ms.load(Ordering::Acquire)),
        )
    }
}

/// Aggregate work only, with no cursor positions or input history retained.
#[derive(Clone, Copy, Default)]
pub(super) struct RendererDiagnostics {
    pub visible: bool,
    pub activations: u64,
    pub shape_changes: u64,
    pub work_us: u64,
    pub failures: u64,
    pub recoveries: u64,
}

pub(super) struct CursorOverlay {
    shared: Arc<Shared>,
    worker: Option<JoinHandle<()>>,
    restart: RetryBackoff,
}

impl CursorOverlay {
    pub fn create() -> Result<Self> {
        let mut overlay = Self {
            shared: Arc::new(Shared::default()),
            worker: None,
            restart: RetryBackoff::default(),
        };
        // Prewarm without an HWND/GDI acknowledgement wait on the input thread.
        // A required fallback cannot be admitted until the worker reports ready.
        overlay.ensure_worker();
        Ok(overlay)
    }

    fn ensure_worker(&mut self) {
        if self.shared.stopping.load(Ordering::Acquire) {
            return;
        }
        if let Some(worker) = self.worker.as_ref() {
            if !worker.is_finished() {
                if self.shared.window_ready.load(Ordering::Acquire)
                    && !self.shared.failed.load(Ordering::Acquire)
                    && !self.shared.health.unavailable()
                {
                    self.restart.recovered();
                }
                return;
            }
            // Window/timer resources are dropped before the worker finishes.
            // Never run a second renderer alongside a stuck native OS call.
            self.worker.take();
            self.restart.failed(Instant::now());
            return;
        }
        if !self.restart.ready(Instant::now()) {
            return;
        }
        self.shared.lifecycle.hide();
        self.shared.visible.store(false, Ordering::Release);
        self.shared.window_ready.store(false, Ordering::Release);
        self.shared.failed.store(false, Ordering::Release);
        self.shared.thread_id.store(0, Ordering::Release);
        self.shared.wake_pending.consumed();
        let worker_state = self.shared.clone();
        match std::thread::Builder::new()
            .name("emc-cursor".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(|| render_thread(&worker_state));
                if !matches!(result, Ok(Ok(()))) || !worker_state.stopping.load(Ordering::Acquire) {
                    worker_state.failed.store(true, Ordering::Release);
                    worker_state.window_ready.store(false, Ordering::Release);
                    worker_state.recovery_pending.store(true, Ordering::Release);
                    record_unavailable(&worker_state);
                }
                worker_state.visible.store(false, Ordering::Release);
                worker_state.thread_id.store(0, Ordering::Release);
            }) {
            Ok(worker) => self.worker = Some(worker),
            Err(_) => {
                self.shared.failed.store(true, Ordering::Release);
                self.shared.recovery_pending.store(true, Ordering::Release);
                record_unavailable(&self.shared);
                self.restart.failed(Instant::now());
            }
        }
    }

    pub fn hide(&mut self) {
        self.shared.health.acknowledge_fault();
        let changed = self.shared.lifecycle.hide();
        self.shared.visible.store(false, Ordering::Release);
        if changed {
            let _ = wake(&self.shared);
        }
    }

    /// Called only from native maintenance outside low-level hook callbacks.
    /// Idle/native-visible operation can prewarm a dead worker without making
    /// hide/reclaim spawn a thread on the hook path.
    pub fn service_recovery(&mut self) {
        self.ensure_worker();
    }

    pub fn update(&mut self, _point: POINT, _cursor: HCURSOR) -> Result<bool> {
        let requirement = current_requirement();
        if requirement == CursorRequirement::NativeVisible {
            // A successfully sampled normal native cursor does not depend on
            // this optional window or its recovery/startup state.
            self.hide();
            return Ok(false);
        }
        self.ensure_worker();
        if self.shared.health.acknowledge_fault() {
            self.hide();
            anyhow::bail!("Receiving cursor temporarily unavailable; control must return locally");
        }
        let healthy = self.shared.window_ready.load(Ordering::Acquire)
            && !self.shared.failed.load(Ordering::Acquire)
            && !self.shared.health.unavailable()
            && !self.shared.stopping.load(Ordering::Acquire);
        if !requirement.permits(healthy, self.shared.progress_age()) {
            // Includes unavailable sampling and a worker stalled in a native
            // window call: never silently admit a missing required cursor.
            if healthy && requirement == CursorRequirement::FallbackRequired {
                record_unavailable(&self.shared);
            }
            self.hide();
            anyhow::bail!("Receiving cursor unavailable; control must return locally");
        }
        // The worker samples cursor artwork itself. Borrowed HCURSOR hints
        // never cross threads. Every update/movement may request one latest-state
        // wake; the native UI timer is a recovery/visibility fallback.
        let activated = self.shared.lifecycle.activate();
        wake(&self.shared)?;
        Ok(activated)
    }

    /// Called after successful native movement, with no sampling or painting on
    /// the caller. At most one wake can be queued regardless of mouse poll rate.
    pub fn movement(&mut self) {
        if self.shared.lifecycle.current().is_some() {
            let _ = wake(&self.shared);
        }
    }

    pub fn diagnostics(&self) -> RendererDiagnostics {
        RendererDiagnostics {
            visible: self.shared.lifecycle.current().is_some()
                && self.shared.visible.load(Ordering::Acquire),
            activations: self.shared.activations.load(Ordering::Relaxed),
            shape_changes: self.shared.shape_changes.load(Ordering::Relaxed),
            work_us: self.shared.work_us.load(Ordering::Relaxed),
            failures: self.shared.failures.load(Ordering::Relaxed),
            recoveries: self.shared.recoveries.load(Ordering::Relaxed),
        }
    }
}

impl Drop for CursorOverlay {
    fn drop(&mut self) {
        self.shared.lifecycle.hide();
        self.shared.visible.store(false, Ordering::Release);
        self.shared.stopping.store(true, Ordering::Release);
        let _ = wake(&self.shared);
        // Dropping the handle detaches; input shutdown never waits for GDI.
    }
}

fn wake(shared: &Shared) -> Result<()> {
    if !shared.wake_pending.request() {
        return Ok(());
    }
    let id = shared.thread_id.load(Ordering::Acquire);
    if id == 0 || unsafe { PostThreadMessageW(id, WAKE, 0, 0) } == 0 {
        shared.wake_pending.consumed();
        if !shared.stopping.load(Ordering::Acquire) {
            // The worker may still be alive and its timer can recover a full or
            // temporarily unavailable message queue. Do not poison it forever.
            record_unavailable(shared);
            anyhow::bail!("Wake receiving cursor renderer failed");
        }
    }
    Ok(())
}

fn record_unavailable(shared: &Shared) {
    if shared.health.record_failure() {
        shared.failures.fetch_add(1, Ordering::Relaxed);
    }
    shared.lifecycle.hide();
    shared.visible.store(false, Ordering::Release);
}

struct UiTimer(usize);
impl Drop for UiTimer {
    fn drop(&mut self) {
        unsafe {
            KillTimer(null_mut(), self.0);
        }
    }
}

fn render_thread(shared: &Shared) -> Result<()> {
    unsafe {
        SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        // PostThreadMessage requires a queue before publishing the thread id.
        let mut message: MSG = zeroed();
        PeekMessageW(&mut message, null_mut(), 0, 0, PM_NOREMOVE);
        shared
            .thread_id
            .store(GetCurrentThreadId(), Ordering::Release);
        let timer = SetTimer(null_mut(), 0, TIMER_INTERVAL_MS, None);
        anyhow::ensure!(timer != 0, "Start receiving cursor timer failed");
        let _timer = UiTimer(timer);
        let mut renderer = RecoveringRenderer::default();
        renderer.tick(shared);
        shared.progressed();
        loop {
            let received = GetMessageW(&mut message, null_mut(), 0, 0);
            anyhow::ensure!(received != -1, "Receiving cursor message loop failed");
            if received == 0 || shared.stopping.load(Ordering::Acquire) {
                break;
            }
            let start = Instant::now();
            if message.message == WAKE || message.message == WM_TIMER {
                if message.message == WAKE {
                    shared.wake_pending.consumed();
                }
                renderer.tick(shared);
                // Only a sampling/presentation/recovery tick proves renderer
                // progress; unrelated window messages must not refresh its age.
                shared.progressed();
            } else {
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
            shared.work_us.fetch_add(
                start.elapsed().as_micros().min(u64::MAX as u128) as u64,
                Ordering::Relaxed,
            );
        }
        if let Some(window) = renderer.window.as_mut() {
            window.hide(shared);
        }
        Ok(())
    }
}

#[derive(Default)]
struct RecoveringRenderer {
    window: Option<RenderWindow>,
    retry: RetryBackoff,
}
impl RecoveringRenderer {
    fn tick(&mut self, shared: &Shared) {
        if shared.stopping.load(Ordering::Acquire) {
            if let Some(window) = self.window.as_mut() {
                window.hide(shared);
            }
            return;
        }
        if self.window.is_none() && self.retry.ready(Instant::now()) {
            match RenderWindow::create() {
                Ok(window) => {
                    self.window = Some(window);
                    self.retry.recovered();
                    shared.health.readable();
                    shared.window_ready.store(true, Ordering::Release);
                }
                Err(_) => {
                    shared.recovery_pending.store(true, Ordering::Release);
                    record_unavailable(shared);
                    self.retry.failed(Instant::now());
                }
            }
        }
        if let Some(window) = self.window.as_mut() {
            if window.tick(shared).is_err() {
                // The selected cursor, HWND or desktop can temporarily fail.
                // End this receiving generation, destroy all owned paint state,
                // and retry hidden resources at a capped rate on this thread.
                shared.recovery_pending.store(true, Ordering::Release);
                record_unavailable(shared);
                shared.window_ready.store(false, Ordering::Release);
                window.hide(shared);
                self.window.take();
                self.retry.failed(Instant::now());
            } else if window.visible && shared.recovery_pending.swap(false, Ordering::AcqRel) {
                // A recreated window has actually presented valid cached
                // artwork; retrying resources alone never resumes ownership.
                shared.recoveries.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

/// Native cursor sampling must succeed before an optional-renderer bypass.
/// Keep the established absent-mouse policy: hidden intent cannot be inferred.
fn current_requirement() -> CursorRequirement {
    let sample = cursor_info().ok().map(|info| {
        (
            unsafe { GetSystemMetrics(SM_MOUSEPRESENT) } != 0,
            info.flags & CURSOR_SHOWING != 0,
            !info.hCursor.is_null(),
        )
    });
    CursorRequirement::from_sample(sample)
}

pub(super) fn fallback_required() -> Result<bool> {
    match current_requirement() {
        CursorRequirement::NativeVisible => Ok(false),
        CursorRequirement::FallbackRequired => Ok(true),
        CursorRequirement::SamplingUnavailable => anyhow::bail!("Inspect cursor visibility failed"),
    }
}

#[derive(Default)]
struct PaintData {
    dc: HDC,
    width: i32,
    height: i32,
    failed: bool,
}

struct RenderWindow {
    window: HWND,
    paint: Box<PaintData>,
    frame: Option<CursorFrame>,
    visible: bool,
    last_origin: Option<(i32, i32)>,
}

impl RenderWindow {
    fn create() -> Result<Self> {
        unsafe {
            let class: Vec<u16> = CLASS.encode_utf16().chain(Some(0)).collect();
            let instance = GetModuleHandleW(null());
            let definition = WNDCLASSW {
                lpfnWndProc: Some(cursor_proc),
                hInstance: instance,
                lpszClassName: class.as_ptr(),
                ..zeroed()
            };
            RegisterClassW(&definition);
            let window = CreateWindowExW(
                WS_EX_LAYERED
                    | WS_EX_TRANSPARENT
                    | WS_EX_NOACTIVATE
                    | WS_EX_TOOLWINDOW
                    | WS_EX_TOPMOST,
                class.as_ptr(),
                null(),
                WS_POPUP,
                0,
                0,
                1,
                1,
                null_mut(),
                null_mut(),
                instance,
                null(),
            );
            anyhow::ensure!(!window.is_null(), "Create receiving cursor window failed");
            if SetLayeredWindowAttributes(window, KEY_COLOR, 255, LWA_COLORKEY) == 0 {
                DestroyWindow(window);
                anyhow::bail!("Configure receiving cursor transparency failed");
            }
            let paint = Box::<PaintData>::default();
            let data = (&*paint as *const PaintData) as isize;
            SetWindowLongPtrW(window, GWLP_USERDATA, data);
            if GetWindowLongPtrW(window, GWLP_USERDATA) != data {
                DestroyWindow(window);
                anyhow::bail!("Configure receiving cursor paint state failed");
            }
            Ok(Self {
                window,
                paint,
                frame: None,
                visible: false,
                last_origin: None,
            })
        }
    }

    fn hide(&mut self, shared: &Shared) {
        if self.visible {
            unsafe {
                ShowWindow(self.window, SW_HIDE);
            }
            self.visible = false;
            self.last_origin = None;
        }
        shared.visible.store(false, Ordering::Release);
    }

    fn tick(&mut self, shared: &Shared) -> Result<()> {
        anyhow::ensure!(!self.paint.failed, "Paint receiving cursor failed");
        let Some(lifecycle) = shared.lifecycle.current() else {
            self.hide(shared);
            // Cursor inspection can temporarily fail on another input desktop.
            // Retain the prewarmed worker and recover once this desktop becomes
            // readable again, without presenting anything while inactive.
            if shared.health.unavailable() && sample_cursor().is_ok() {
                shared.health.readable();
            }
            return Ok(());
        };
        let (info, point) = match sample_cursor() {
            Ok(sample) => sample,
            Err(_) => {
                if shared.health.record_failure() {
                    shared.failures.fetch_add(1, Ordering::Relaxed);
                }
                shared.lifecycle.hide();
                self.hide(shared);
                return Ok(());
            }
        };
        // Visibility does not identify artwork. A hidden non-null resize/I-beam
        // handle is still meaningful. Preserve the absent-mouse trigger: the
        // logical SHOWING flag alone does not establish pointer visibility on
        // a receiver with no attached mouse. Its aligned shape may overlap a
        // currently rendered native cursor, rather than leave that PC blind.
        let absent_mouse = unsafe { GetSystemMetrics(SM_MOUSEPRESENT) } == 0;
        if !absent_mouse && info.flags & CURSOR_SHOWING != 0 && !info.hCursor.is_null() {
            self.hide(shared);
            return Ok(());
        }
        let source =
            shape_candidate(info.hCursor as usize, info.flags & CURSOR_SHOWING != 0).unwrap_or(0);
        let dpi = unsafe { GetDpiForWindow(self.window) }.max(96);
        let refresh = self.frame.as_ref().is_none_or(|frame| {
            frame.source != source || frame.dpi != dpi || frame.observed.elapsed() >= CACHE_LIFETIME
        });
        if refresh {
            let frame = CursorFrame::copy(source, dpi)?;
            let changed = self.frame.as_ref().is_none_or(|old| {
                old.source != source || old.dpi != dpi || old.geometry != frame.geometry
            });
            self.paint.dc = frame.surface.dc;
            self.paint.width = frame.geometry.width;
            self.paint.height = frame.geometry.height;
            self.frame = Some(frame);
            if changed {
                shared.shape_changes.fetch_add(1, Ordering::Relaxed);
            }
        }
        // A hide/re-enter creates a new generation, so an old renderer sample
        // cannot become current again. Check immediately around presentation.
        if !shared.lifecycle.is_current(lifecycle) || shared.stopping.load(Ordering::Acquire) {
            self.hide(shared);
            return Ok(());
        }
        let frame = self.frame.as_ref().unwrap();
        let origin = frame.geometry.origin(point.x, point.y);
        if refresh || !self.visible || self.last_origin != Some(origin) {
            let mut flags = SWP_NOACTIVATE | SWP_NOSENDCHANGING;
            if !self.visible {
                flags |= SWP_SHOWWINDOW;
            } else {
                // Moving an already-topmost fallback does not need a fresh
                // insertion into the topmost group on every mouse packet.
                flags |= SWP_NOZORDER;
            }
            // Position changes reuse the cached bitmap. Do not invalidate or
            // synchronously paint it; Windows can repaint exposed areas normally.
            if !refresh && self.visible {
                flags |= SWP_NOSIZE;
            }
            anyhow::ensure!(
                unsafe {
                    SetWindowPos(
                        self.window,
                        HWND_TOPMOST,
                        origin.0,
                        origin.1,
                        frame.geometry.width,
                        frame.geometry.height,
                        flags,
                    )
                } != 0,
                "Move receiving cursor window failed"
            );
            if refresh || !self.visible {
                anyhow::ensure!(
                    unsafe { InvalidateRect(self.window, null(), 0) } != 0,
                    "Invalidate receiving cursor failed"
                );
                unsafe {
                    UpdateWindow(self.window);
                }
                anyhow::ensure!(!self.paint.failed, "Paint receiving cursor failed");
            }
            self.last_origin = Some(origin);
        }
        if !shared.lifecycle.is_current(lifecycle) || shared.stopping.load(Ordering::Acquire) {
            // A hide may race an already executing OS call, but the stale window
            // is hidden on this worker before any further queued tick can show it.
            unsafe {
                ShowWindow(self.window, SW_HIDE);
            }
            self.visible = false;
            self.last_origin = None;
            shared.visible.store(false, Ordering::Release);
            return Ok(());
        }
        if !self.visible {
            shared.activations.fetch_add(1, Ordering::Relaxed);
        }
        self.visible = true;
        shared.visible.store(true, Ordering::Release);
        Ok(())
    }
}

impl Drop for RenderWindow {
    fn drop(&mut self) {
        unsafe {
            SetWindowLongPtrW(self.window, GWLP_USERDATA, 0);
            DestroyWindow(self.window);
        }
    }
}

struct CursorFrame {
    source: usize,
    dpi: u32,
    observed: Instant,
    _cursor: OwnedCursor,
    geometry: CursorGeometry,
    surface: DibSurface,
}

impl CursorFrame {
    fn copy(source: usize, dpi: u32) -> Result<Self> {
        let copied = if source == 0 {
            None
        } else {
            Some(Self::from_handle(source as HCURSOR))
        };
        let (cursor, geometry, surface) = match copied {
            Some(Ok(frame)) => frame,
            // A null/invalid/blank selected cursor alone permits the arrow.
            _ => Self::from_handle(unsafe { LoadCursorW(null_mut(), IDC_ARROW) })
                .context("Create visible receiving cursor fallback")?,
        };
        Ok(Self {
            source,
            dpi,
            observed: Instant::now(),
            _cursor: cursor,
            geometry,
            surface,
        })
    }

    fn from_handle(source: HCURSOR) -> Result<(OwnedCursor, CursorGeometry, DibSurface)> {
        anyhow::ensure!(!source.is_null(), "Cursor handle unavailable");
        // Never retain or destroy another application's borrowed cursor.
        let cursor = OwnedCursor(unsafe { CopyImage(source, IMAGE_CURSOR, 0, 0, 0) });
        anyhow::ensure!(!cursor.0.is_null(), "Copy native cursor failed");
        let icon = IconInfo::read(cursor.0)?;
        let bitmap = if icon.0.hbmColor.is_null() {
            icon.0.hbmMask
        } else {
            icon.0.hbmColor
        };
        let mut native: BITMAP = unsafe { zeroed() };
        anyhow::ensure!(
            unsafe {
                GetObjectW(
                    bitmap,
                    size_of::<BITMAP>() as i32,
                    (&mut native as *mut BITMAP).cast(),
                )
            } != 0,
            "Inspect native cursor dimensions failed"
        );
        let height = if icon.0.hbmColor.is_null() {
            native.bmHeight / 2
        } else {
            native.bmHeight
        };
        let geometry =
            CursorGeometry::new(native.bmWidth, height, icon.0.xHotspot, icon.0.yHotspot)
                .context("Unsupported native cursor geometry")?;
        let mut surface = DibSurface::create(geometry.width, geometry.height)?;
        // Transparent cursors leave both black and white backgrounds unchanged;
        // testing both also recognizes black/white and monochrome XOR artwork.
        let mut visible = false;
        for background in [0_u32, 0x00ff_ffff] {
            surface.fill(background);
            surface.draw(cursor.0)?;
            visible |= surface
                .pixels()
                .iter()
                .any(|pixel| pixel & 0x00ff_ffff != background);
        }
        anyhow::ensure!(visible, "Selected native cursor is blank");
        surface.fill(KEY_COLOR);
        surface.draw(cursor.0)?;
        Ok((cursor, geometry, surface))
    }
}

struct OwnedCursor(HCURSOR);
impl Drop for OwnedCursor {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                DestroyCursor(self.0);
            }
        }
    }
}

struct IconInfo(ICONINFO);
impl IconInfo {
    fn read(cursor: HCURSOR) -> Result<Self> {
        let mut info: ICONINFO = unsafe { zeroed() };
        anyhow::ensure!(
            unsafe { GetIconInfo(cursor, &mut info) } != 0,
            "Inspect native cursor hotspot failed"
        );
        Ok(Self(info))
    }
}
impl Drop for IconInfo {
    fn drop(&mut self) {
        unsafe {
            if !self.0.hbmColor.is_null() {
                DeleteObject(self.0.hbmColor);
            }
            if !self.0.hbmMask.is_null() {
                DeleteObject(self.0.hbmMask);
            }
        }
    }
}

struct DibSurface {
    dc: HDC,
    bitmap: HBITMAP,
    previous: HGDIOBJ,
    bits: *mut u32,
    width: i32,
    height: i32,
}
impl DibSurface {
    fn create(width: i32, height: i32) -> Result<Self> {
        unsafe {
            let dc = CreateCompatibleDC(null_mut());
            anyhow::ensure!(!dc.is_null(), "Create receiving cursor memory DC failed");
            let mut definition: BITMAPINFO = zeroed();
            definition.bmiHeader.biSize = size_of::<BITMAPINFOHEADER>() as u32;
            definition.bmiHeader.biWidth = width;
            definition.bmiHeader.biHeight = -height;
            definition.bmiHeader.biPlanes = 1;
            definition.bmiHeader.biBitCount = 32;
            definition.bmiHeader.biCompression = BI_RGB;
            let mut bits = null_mut();
            let bitmap =
                CreateDIBSection(dc, &definition, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
            if bitmap.is_null() || bits.is_null() {
                if !bitmap.is_null() {
                    DeleteObject(bitmap);
                }
                DeleteDC(dc);
                anyhow::bail!("Create receiving cursor bitmap failed");
            }
            let previous = SelectObject(dc, bitmap);
            if previous.is_null() || previous as isize == -1 {
                DeleteObject(bitmap);
                DeleteDC(dc);
                anyhow::bail!("Select receiving cursor bitmap failed");
            }
            Ok(Self {
                dc,
                bitmap,
                previous,
                bits: bits.cast(),
                width,
                height,
            })
        }
    }
    fn fill(&mut self, color: u32) {
        unsafe {
            GdiFlush();
        }
        unsafe { std::slice::from_raw_parts_mut(self.bits, (self.width * self.height) as usize) }
            .fill(color);
    }
    fn pixels(&self) -> &[u32] {
        unsafe {
            GdiFlush();
        }
        unsafe { std::slice::from_raw_parts(self.bits, (self.width * self.height) as usize) }
    }
    fn draw(&self, cursor: HCURSOR) -> Result<()> {
        anyhow::ensure!(
            unsafe {
                DrawIconEx(
                    self.dc,
                    0,
                    0,
                    cursor,
                    self.width,
                    self.height,
                    0,
                    null_mut(),
                    DI_NORMAL,
                )
            } != 0,
            "Render native cursor failed"
        );
        Ok(())
    }
}
impl Drop for DibSurface {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.previous);
            DeleteObject(self.bitmap);
            DeleteDC(self.dc);
        }
    }
}

unsafe extern "system" fn cursor_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_NCHITTEST => HTTRANSPARENT as isize,
        WM_MOUSEACTIVATE => MA_NOACTIVATE as isize,
        WM_ERASEBKGND => 1,
        WM_PAINT => {
            let mut paint: PAINTSTRUCT = zeroed();
            let dc = BeginPaint(window, &mut paint);
            let data = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut PaintData;
            if !data.is_null() && !(*data).dc.is_null() {
                let copied = if dc.is_null() {
                    0
                } else {
                    BitBlt(
                        dc,
                        0,
                        0,
                        (*data).width,
                        (*data).height,
                        (*data).dc,
                        0,
                        0,
                        SRCCOPY,
                    )
                };
                if copied == 0 {
                    (*data).failed = true;
                }
            }
            EndPaint(window, &paint);
            0
        }
        _ => DefWindowProcW(window, message, wparam, lparam),
    }
}

fn sample_cursor() -> Result<(CURSORINFO, POINT)> {
    let info = cursor_info()?;
    let mut point = POINT { x: 0, y: 0 };
    anyhow::ensure!(
        unsafe { GetPhysicalCursorPos(&mut point) } != 0,
        "Inspect physical cursor position failed"
    );
    Ok((info, point))
}

pub(super) fn cursor_info() -> Result<CURSORINFO> {
    let mut info: CURSORINFO = unsafe { zeroed() };
    info.cbSize = size_of::<CURSORINFO>() as u32;
    if unsafe { GetCursorInfo(&mut info) } == 0 {
        return Err(std::io::Error::last_os_error()).context("Inspect cursor visibility");
    }
    Ok(info)
}

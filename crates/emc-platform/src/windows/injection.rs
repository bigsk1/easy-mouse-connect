//! One ordered OS injector, separate from the low-level-hook message owner.
//! MWB explicitly avoids SendInput on its hook thread after observed deadlocks:
//! https://github.com/microsoft/PowerToys/blob/main/src/modules/MouseWithoutBorders/App/Core/Common.cs
//! A blocked OS call is not interruptible: track accepted holds before checking
//! revocation again, and release them before processing a different owner.
use super::*;

#[derive(Default)]
pub(super) struct Owner(AtomicBool);
impl Owner {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }
    pub fn current(&self) -> bool {
        !self.0.load(Ordering::Acquire)
    }
}

#[derive(Clone, Debug)]
pub(super) enum Effect {
    Event(InputEvent),
    Warp(i32, i32, Desktop),
    Restore {
        x: i32,
        y: i32,
        bounds: Desktop,
        motion_serial: u64,
    },
    Release,
}

pub(super) trait Backend: Send + 'static {
    fn insert(&mut self, effect: &Effect) -> Result<()>;
}

struct NativeBackend;
impl Backend for NativeBackend {
    fn insert(&mut self, effect: &Effect) -> Result<()> {
        match effect {
            Effect::Event(InputEvent::Move { dx, dy }) => {
                inject_mouse(*dx, *dy, 0, MOUSEEVENTF_MOVE)
            }
            Effect::Event(InputEvent::Button { button, down }) => inject_button(*button, *down),
            Effect::Event(InputEvent::Wheel { delta, horizontal }) => inject_mouse(
                0,
                0,
                *delta as u32,
                if *horizontal {
                    MOUSEEVENTF_HWHEEL
                } else {
                    MOUSEEVENTF_WHEEL
                },
            ),
            Effect::Event(InputEvent::Key {
                scan,
                vk,
                extended,
                down,
            }) => inject_key(*scan, *vk, *extended, *down),
            Effect::Warp(x, y, bounds) | Effect::Restore { x, y, bounds, .. } => {
                warp(*x, *y, *bounds)
            }
            _ => bail!(NativeInputFailure::PreconditionUnavailable),
        }
    }
}

struct Job {
    effect: Effect,
    owner: Arc<Owner>,
    epoch: u64,
    deadline: Instant,
    done: Option<SyncSender<Result<()>>>,
}
enum Message {
    Apply(Job),
    Cleanup,
}

pub(super) struct Executor {
    sender: SyncSender<Message>,
    cleanup_requested: Arc<AtomicBool>,
    shutdown: Arc<AtomicBool>,
    held: Arc<AtomicBool>,
    busy: Arc<AtomicBool>,
}
impl Executor {
    pub fn start(shared: Arc<Shared>, thread_id: u32) -> Result<Self> {
        Self::with_backend(shared, thread_id, NativeBackend)
    }
    pub fn with_backend(
        shared: Arc<Shared>,
        thread_id: u32,
        backend: impl Backend,
    ) -> Result<Self> {
        let (sender, receiver) = mpsc::sync_channel(8);
        let executor = Self {
            sender,
            cleanup_requested: Arc::new(AtomicBool::new(false)),
            shutdown: Arc::new(AtomicBool::new(false)),
            held: Arc::new(AtomicBool::new(false)),
            busy: Arc::new(AtomicBool::new(false)),
        };
        let mut worker = Worker {
            shared,
            backend,
            owner: None,
            owner_epoch: 0,
            keys: BTreeSet::new(),
            buttons: [false; 5],
            held: executor.held.clone(),
            busy: executor.busy.clone(),
            cleanup: executor.cleanup_requested.clone(),
            shutdown: executor.shutdown.clone(),
            thread_id,
        };
        thread::Builder::new()
            .name("emc-input-injection".into())
            .spawn(move || {
                unsafe {
                    SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
                }
                worker.run(receiver);
            })
            .context("Start ordered native injection executor")?;
        Ok(executor)
    }
    pub fn submit(
        &self,
        effect: Effect,
        owner: Arc<Owner>,
        epoch: u64,
        deadline: Instant,
    ) -> Result<Receiver<Result<()>>> {
        let (done, receiver) = mpsc::sync_channel(1);
        self.sender
            .try_send(Message::Apply(Job {
                effect,
                owner,
                epoch,
                deadline,
                done: Some(done),
            }))
            .map_err(|_| anyhow!(NativeInputFailure::CommandQueueUnavailable))?;
        Ok(receiver)
    }
    pub fn restore(
        &self,
        x: i32,
        y: i32,
        bounds: Desktop,
        motion_serial: u64,
        owner: Arc<Owner>,
        epoch: u64,
    ) {
        // Cursor restoration is optional, bounded position work. Never replay
        // it after later input/ownership or wait for space from an LL callback.
        let _ = self.sender.try_send(Message::Apply(Job {
            effect: Effect::Restore {
                x,
                y,
                bounds,
                motion_serial,
            },
            owner,
            epoch,
            deadline: Instant::now() + Duration::from_millis(200),
            done: None,
        }));
    }
    pub fn cleanup(&self) {
        if !self.cleanup_requested.swap(true, Ordering::AcqRel) {
            let _ = self.sender.try_send(Message::Cleanup);
        }
    }
    pub fn has_held(&self) -> bool {
        self.held.load(Ordering::Acquire)
    }
    pub fn busy(&self) -> bool {
        self.busy.load(Ordering::Acquire)
    }
}
impl Drop for Executor {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        self.cleanup();
    }
}

struct Worker<B> {
    shared: Arc<Shared>,
    backend: B,
    owner: Option<Arc<Owner>>,
    owner_epoch: u64,
    keys: BTreeSet<(u16, u16, bool)>,
    buttons: [bool; 5],
    held: Arc<AtomicBool>,
    busy: Arc<AtomicBool>,
    cleanup: Arc<AtomicBool>,
    shutdown: Arc<AtomicBool>,
    thread_id: u32,
}
impl<B: Backend> Worker<B> {
    fn run(&mut self, receiver: Receiver<Message>) {
        loop {
            self.cleanup.store(false, Ordering::Release);
            if self
                .owner
                .as_ref()
                .is_some_and(|owner| !self.authority_current(owner, self.owner_epoch))
            {
                let _ = self.release();
            }
            if self.shutdown.load(Ordering::Acquire) {
                let _ = self.release();
                break;
            }
            match receiver.recv_timeout(Duration::from_millis(10)) {
                Ok(Message::Apply(job)) => {
                    self.busy.store(true, Ordering::Release);
                    let result = self.apply(&job);
                    if let Some(done) = job.done {
                        if done.try_send(result).is_err() {
                            job.owner.cancel();
                            if self
                                .owner
                                .as_ref()
                                .is_some_and(|owner| Arc::ptr_eq(owner, &job.owner))
                            {
                                let _ = self.release();
                            }
                        }
                    }
                    self.busy.store(false, Ordering::Release);
                    let _ = post_input_wake(&self.shared, self.thread_id);
                }
                Ok(Message::Cleanup) | Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    let _ = self.release();
                    break;
                }
            }
        }
    }
    fn current(&self, job: &Job) -> Result<()> {
        check_deadline(Instant::now(), job.deadline, false)?;
        if !self.authority_current(&job.owner, job.epoch) {
            bail!(NativeInputFailure::PreconditionUnavailable);
        }
        if let Effect::Restore { motion_serial, .. } = job.effect {
            if self.shared.physical_input_serial.load(Ordering::Acquire) != motion_serial
                || self.shared.remote.load(Ordering::Acquire)
                || self.shared.stats.receiving.load(Ordering::Acquire)
            {
                bail!(NativeInputFailure::PreconditionUnavailable);
            }
        }
        Ok(())
    }
    fn authority_current(&self, owner: &Owner, epoch: u64) -> bool {
        owner.current()
            && !self.shutdown.load(Ordering::Acquire)
            && self.shared.epoch.load(Ordering::Acquire) == epoch
            && self.shared.ready()
            && !self.shared.force_release.load(Ordering::Acquire)
            && self.shared.input_desktop_available.load(Ordering::Acquire)
            && self
                .shared
                .stats
                .raw_input_registered
                .load(Ordering::Acquire)
            && self
                .shared
                .stats
                .mouse_hook_installed
                .load(Ordering::Acquire)
            && self
                .shared
                .stats
                .keyboard_hook_installed
                .load(Ordering::Acquire)
    }
    fn apply(&mut self, job: &Job) -> Result<()> {
        if matches!(job.effect, Effect::Release) {
            if self.owner.as_ref().is_some_and(|owner| {
                !self.authority_current(owner, self.owner_epoch) || Arc::ptr_eq(owner, &job.owner)
            }) {
                return self.release();
            }
            return Ok(());
        }
        self.current(job)?;
        if let Effect::Event(InputEvent::Key {
            scan,
            vk,
            extended,
            down,
        }) = &job.effect
        {
            if *scan > 255
                || *vk > 255
                || !key_admissible(&self.keys, (*scan, *vk, *extended), *down)
            {
                bail!(NativeInputFailure::PreconditionUnavailable);
            }
        }
        if self
            .owner
            .as_ref()
            .is_some_and(|owner| !Arc::ptr_eq(owner, &job.owner))
        {
            self.release()?;
            if !self.keys.is_empty() || self.buttons.iter().any(|held| *held) {
                bail!(NativeInputFailure::PreconditionUnavailable);
            }
        }
        self.owner = Some(job.owner.clone());
        self.owner_epoch = job.epoch;
        self.current(job)?;
        self.call(&job.effect)?;
        // Account for an accepted Down before inspecting cancellation. The hook
        // owner can revoke concurrently while this syscall is blocked.
        if let Effect::Event(event) = &job.effect {
            match event {
                InputEvent::Key {
                    scan,
                    vk,
                    extended,
                    down,
                } => {
                    if *down {
                        self.keys.insert((*scan, *vk, *extended));
                    } else {
                        self.keys.remove(&(*scan, *vk, *extended));
                    }
                }
                InputEvent::Button { button, down } => self.buttons[button_index(*button)] = *down,
                _ => {}
            }
        }
        self.publish_held();
        if let Err(error) = self.current(job) {
            let _ = self.release();
            return Err(error);
        }
        Ok(())
    }
    fn call(&mut self, effect: &Effect) -> Result<()> {
        let start = Instant::now();
        let result = self.backend.insert(effect);
        let micros = start.elapsed().as_micros().min(u64::MAX as u128) as u64;
        let stats = &self.shared.stats;
        stats.native_sendinput_calls.fetch_add(1, Ordering::Relaxed);
        stats
            .native_sendinput_total_us
            .fetch_add(micros, Ordering::Relaxed);
        stats
            .native_sendinput_peak_us
            .fetch_max(micros, Ordering::Relaxed);
        if matches!(effect, Effect::Event(InputEvent::Wheel { .. })) {
            stats
                .native_wheel_peak_us
                .fetch_max(micros, Ordering::Relaxed);
            if result.is_ok() {
                stats.injected_wheels.fetch_add(1, Ordering::Relaxed);
            }
        }
        if result.is_err() {
            stats.injection_failures.fetch_add(1, Ordering::Relaxed);
        }
        result
    }
    fn publish_held(&self) {
        self.held.store(
            !self.keys.is_empty() || self.buttons.iter().any(|held| *held),
            Ordering::Release,
        );
    }
    fn release(&mut self) -> Result<()> {
        let mut failure = None;
        for (scan, vk, extended) in std::mem::take(&mut self.keys) {
            if let Err(error) = self.call(&Effect::Event(InputEvent::Key {
                scan,
                vk,
                extended,
                down: false,
            })) {
                self.keys.insert((scan, vk, extended));
                failure.get_or_insert(error);
            }
        }
        for index in 0..5 {
            if self.buttons[index] {
                match self.call(&Effect::Event(InputEvent::Button {
                    button: button_at(index),
                    down: false,
                })) {
                    Ok(()) => self.buttons[index] = false,
                    Err(error) => {
                        failure.get_or_insert(error);
                    }
                }
            }
        }
        self.publish_held();
        match failure {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

fn inject_button(button: MouseButton, down: bool) -> Result<()> {
    let (flags, data) = match (button, down) {
        (MouseButton::Left, true) => (MOUSEEVENTF_LEFTDOWN, 0),
        (MouseButton::Left, false) => (MOUSEEVENTF_LEFTUP, 0),
        (MouseButton::Right, true) => (MOUSEEVENTF_RIGHTDOWN, 0),
        (MouseButton::Right, false) => (MOUSEEVENTF_RIGHTUP, 0),
        (MouseButton::Middle, true) => (MOUSEEVENTF_MIDDLEDOWN, 0),
        (MouseButton::Middle, false) => (MOUSEEVENTF_MIDDLEUP, 0),
        (MouseButton::Back, true) => (MOUSEEVENTF_XDOWN, XBUTTON1 as u32),
        (MouseButton::Back, false) => (MOUSEEVENTF_XUP, XBUTTON1 as u32),
        (MouseButton::Forward, true) => (MOUSEEVENTF_XDOWN, XBUTTON2 as u32),
        (MouseButton::Forward, false) => (MOUSEEVENTF_XUP, XBUTTON2 as u32),
    };
    inject_mouse(0, 0, data, flags)
}

fn inject_key(scan: u16, vk: u16, extended: bool, down: bool) -> Result<()> {
    let mut flags = if scan == 0 { 0 } else { KEYEVENTF_SCANCODE };
    if extended {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    if !down {
        flags |= KEYEVENTF_KEYUP;
    }
    let input = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: if scan == 0 { vk } else { 0 },
                wScan: scan,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: INJECTED_TAG,
            },
        },
    };
    send_input(&input)
}

fn inject_mouse(dx: i32, dy: i32, data: u32, flags: u32) -> Result<()> {
    let input = INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                mouseData: data,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: INJECTED_TAG,
            },
        },
    };
    send_input(&input)
}

fn send_input(input: &INPUT) -> Result<()> {
    // SendInput may fail without setting an error (including UIPI). Do not
    // misclassify a stale last-error value as a permission failure.
    unsafe {
        SetLastError(0);
    }
    if unsafe { SendInput(1, input, size_of::<INPUT>() as i32) } != 1 {
        let code = unsafe { GetLastError() };
        let cause = if code == 0 {
            std::io::Error::other("Windows rejected input without supplying an error code")
        } else {
            std::io::Error::from_raw_os_error(code as i32)
        };
        return Err(anyhow::Error::new(cause).context("Inject native input"));
    }
    Ok(())
}

fn warp(x: i32, y: i32, bounds: Desktop) -> Result<()> {
    let (x, y) = bounds.clamp(x, y);
    let normalized_x = ((i64::from(x) - i64::from(bounds.left)) * 65536 / i64::from(bounds.width)
        + 32768 / i64::from(bounds.width))
    .clamp(0, 65535) as i32;
    let normalized_y = ((i64::from(y) - i64::from(bounds.top)) * 65536 / i64::from(bounds.height)
        + 32768 / i64::from(bounds.height))
    .clamp(0, 65535) as i32;
    inject_mouse(
        normalized_x,
        normalized_y,
        0,
        MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
    )
}

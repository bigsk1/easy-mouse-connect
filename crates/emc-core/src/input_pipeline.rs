//! Bounded ordered queues for the control path. Only adjacent relative motion
//! can merge; every other event/message is an ordering barrier.
use crate::diagnostics::FailureReason;
use anyhow::{ensure, Result};
use emc_protocol::{InputEvent, Message};
use serde::Serialize;
use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use tokio::sync::Notify;

pub(crate) const CAPACITY: usize = 128;
pub(crate) const MAX_INPUT_AGE: Duration = Duration::from_millis(200);

#[derive(Debug, Default, Clone, Serialize)]
pub struct ControlDiagnostics {
    pub outgoing_queue_peak: u64,
    pub incoming_queue_peak: u64,
    pub outgoing_moves_coalesced: u64,
    pub incoming_moves_coalesced: u64,
    pub max_outgoing_queue_age_ms: u64,
    pub max_incoming_queue_age_ms: u64,
    pub max_native_apply_ms: u64,
    pub stale_input_failures: u64,
    pub outgoing_queue_stale_failures: u64,
    pub outgoing_write_deadline_failures: u64,
    pub incoming_queue_stale_failures: u64,
    pub native_completion_deadline_failures: u64,
    pub input_queue_overflows: u64,
    pub round_trip_sample_age_ms: Option<u64>,
}

#[derive(Default)]
pub(crate) struct Counters {
    outgoing_queue_peak: AtomicU64,
    incoming_queue_peak: AtomicU64,
    outgoing_moves_coalesced: AtomicU64,
    incoming_moves_coalesced: AtomicU64,
    max_outgoing_queue_age_ms: AtomicU64,
    max_incoming_queue_age_ms: AtomicU64,
    max_native_apply_ms: AtomicU64,
    stale_input_failures: AtomicU64,
    outgoing_queue_stale_failures: AtomicU64,
    outgoing_write_deadline_failures: AtomicU64,
    incoming_queue_stale_failures: AtomicU64,
    native_completion_deadline_failures: AtomicU64,
    input_queue_overflows: AtomicU64,
}
impl Counters {
    pub(crate) fn snapshot(&self, round_trip_sample_age_ms: Option<u64>) -> ControlDiagnostics {
        ControlDiagnostics {
            outgoing_queue_peak: self.outgoing_queue_peak.load(Ordering::Relaxed),
            incoming_queue_peak: self.incoming_queue_peak.load(Ordering::Relaxed),
            outgoing_moves_coalesced: self.outgoing_moves_coalesced.load(Ordering::Relaxed),
            incoming_moves_coalesced: self.incoming_moves_coalesced.load(Ordering::Relaxed),
            max_outgoing_queue_age_ms: self.max_outgoing_queue_age_ms.load(Ordering::Relaxed),
            max_incoming_queue_age_ms: self.max_incoming_queue_age_ms.load(Ordering::Relaxed),
            max_native_apply_ms: self.max_native_apply_ms.load(Ordering::Relaxed),
            stale_input_failures: self.stale_input_failures.load(Ordering::Relaxed),
            outgoing_queue_stale_failures: self
                .outgoing_queue_stale_failures
                .load(Ordering::Relaxed),
            outgoing_write_deadline_failures: self
                .outgoing_write_deadline_failures
                .load(Ordering::Relaxed),
            incoming_queue_stale_failures: self
                .incoming_queue_stale_failures
                .load(Ordering::Relaxed),
            native_completion_deadline_failures: self
                .native_completion_deadline_failures
                .load(Ordering::Relaxed),
            input_queue_overflows: self.input_queue_overflows.load(Ordering::Relaxed),
            round_trip_sample_age_ms,
        }
    }
    pub(crate) fn native_completed(&self, elapsed: Duration) {
        self.max_native_apply_ms
            .fetch_max(millis(elapsed), Ordering::Relaxed);
    }
    pub(crate) fn stale(&self, stage: StaleStage) {
        match stage {
            StaleStage::OutgoingQueue => &self.outgoing_queue_stale_failures,
            StaleStage::OutgoingWrite => &self.outgoing_write_deadline_failures,
            StaleStage::IncomingQueue => &self.incoming_queue_stale_failures,
            StaleStage::NativeCompletion => &self.native_completion_deadline_failures,
        }
        .fetch_add(1, Ordering::Relaxed);
        self.stale_input_failures.fetch_add(1, Ordering::Relaxed);
    }
    pub(crate) fn input_deadline(&self, queued_at: Instant, stage: StaleStage) -> Result<Instant> {
        let deadline = queued_at + MAX_INPUT_AGE;
        if Instant::now() >= deadline {
            self.stale(stage);
            anyhow::bail!(FailureReason::StaleInput);
        }
        Ok(deadline)
    }
    pub(crate) async fn complete_before(
        &self,
        deadline: Instant,
        stage: StaleStage,
        work: impl std::future::Future<Output = Result<()>>,
    ) -> Result<()> {
        match tokio::time::timeout_at(deadline.into(), work).await {
            Ok(Err(error)) if FailureReason::from_error(&error) == FailureReason::StaleInput => {
                // Native code may reject its queued command as stale just
                // before this outer deadline is polled. Count that once in
                // the same completion stage; it is not an OS insertion error.
                self.stale(stage);
                Err(error)
            }
            Ok(result) => {
                // Tokio polls a ready work future before its timer. A native
                // call or delayed task poll can therefore complete after the
                // original deadline without producing Elapsed. Never treat
                // that late completion as successful or renew its budget.
                if Instant::now() >= deadline {
                    self.stale(stage);
                    return match result {
                        Ok(()) => Err(FailureReason::StaleInput.into()),
                        Err(error) => Err(error.context(FailureReason::StaleInput)),
                    };
                }
                result
            }
            Err(_) => {
                self.stale(stage);
                anyhow::bail!(FailureReason::StaleInput);
            }
        }
    }
}
#[derive(Clone, Copy)]
pub(crate) enum StaleStage {
    OutgoingQueue,
    OutgoingWrite,
    IncomingQueue,
    NativeCompletion,
}
pub(crate) fn millis(duration: Duration) -> u64 {
    duration.as_millis().min(u128::from(u64::MAX)) as u64
}

#[derive(Clone, Copy)]
pub(crate) enum Direction {
    Outgoing,
    Incoming,
}

pub(crate) trait Motion {
    fn motion(&self) -> Option<(i32, i32)>;
    fn set_motion(&mut self, dx: i32, dy: i32);
}
impl Motion for InputEvent {
    fn motion(&self) -> Option<(i32, i32)> {
        match self {
            Self::Move { dx, dy } => Some((*dx, *dy)),
            _ => None,
        }
    }
    fn set_motion(&mut self, dx: i32, dy: i32) {
        *self = Self::Move { dx, dy };
    }
}
impl Motion for Message {
    fn motion(&self) -> Option<(i32, i32)> {
        match self {
            Self::Input(event) => event.motion(),
            _ => None,
        }
    }
    fn set_motion(&mut self, dx: i32, dy: i32) {
        *self = Self::Input(InputEvent::Move { dx, dy });
    }
}

pub(crate) struct Timed<T> {
    pub(crate) value: T,
    pub(crate) queued_at: Instant,
}
struct State<T> {
    items: VecDeque<Timed<T>>,
    closed: bool,
}
pub(crate) struct OrderedQueue<T> {
    state: Mutex<State<T>>,
    changed: Notify,
    direction: Direction,
    counters: Arc<Counters>,
}
impl<T: Motion> OrderedQueue<T> {
    pub(crate) fn new(direction: Direction, counters: Arc<Counters>) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State {
                items: VecDeque::new(),
                closed: false,
            }),
            changed: Notify::new(),
            direction,
            counters,
        })
    }
    pub(crate) fn push(&self, value: T, queued_at: Instant) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        ensure!(!state.closed, FailureReason::SessionRevoked);
        if let (Some(previous), Some((dx, dy))) = (state.items.back_mut(), value.motion()) {
            if let Some((previous_x, previous_y)) = previous.value.motion() {
                let (Some(x), Some(y)) = (previous_x.checked_add(dx), previous_y.checked_add(dy))
                else {
                    self.counters
                        .input_queue_overflows
                        .fetch_add(1, Ordering::Relaxed);
                    anyhow::bail!(FailureReason::Backlog);
                };
                previous.value.set_motion(x, y);
                // Keep the oldest timestamp: merging cannot renew stale input.
                previous.queued_at = previous.queued_at.min(queued_at);
                match self.direction {
                    Direction::Outgoing => &self.counters.outgoing_moves_coalesced,
                    Direction::Incoming => &self.counters.incoming_moves_coalesced,
                }
                .fetch_add(1, Ordering::Relaxed);
                return Ok(());
            }
        }
        if state.items.len() == CAPACITY {
            self.counters
                .input_queue_overflows
                .fetch_add(1, Ordering::Relaxed);
            anyhow::bail!(FailureReason::Backlog);
        }
        state.items.push_back(Timed { value, queued_at });
        match self.direction {
            Direction::Outgoing => &self.counters.outgoing_queue_peak,
            Direction::Incoming => &self.counters.incoming_queue_peak,
        }
        .fetch_max(state.items.len() as u64, Ordering::Relaxed);
        drop(state);
        self.changed.notify_one();
        Ok(())
    }
    pub(crate) async fn recv(&self) -> Option<Timed<T>> {
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            {
                let mut state = self.state.lock().unwrap();
                if let Some(item) = state.items.pop_front() {
                    match self.direction {
                        Direction::Outgoing => &self.counters.max_outgoing_queue_age_ms,
                        Direction::Incoming => &self.counters.max_incoming_queue_age_ms,
                    }
                    .fetch_max(millis(item.queued_at.elapsed()), Ordering::Relaxed);
                    return Some(item);
                }
                if state.closed {
                    return None;
                }
            }
            changed.await;
        }
    }
    pub(crate) fn close(&self) {
        let mut state = self.state.lock().unwrap();
        state.closed = true;
        state.items.clear();
        drop(state);
        self.changed.notify_waiters();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn coalescing_preserves_sums_timestamps_and_all_action_barriers() {
        let counters = Arc::new(Counters::default());
        let queue = OrderedQueue::new(Direction::Incoming, counters.clone());
        let first = Instant::now() - Duration::from_millis(20);
        queue
            .push(InputEvent::Move { dx: 5, dy: -7 }, first)
            .unwrap();
        queue
            .push(InputEvent::Move { dx: -2, dy: 9 }, Instant::now())
            .unwrap();
        let barriers = [
            InputEvent::Enter {
                from_left: true,
                y: 123,
            },
            InputEvent::Key {
                scan: 30,
                vk: 65,
                extended: false,
                down: true,
            },
            InputEvent::Button {
                button: emc_protocol::MouseButton::Left,
                down: true,
            },
            InputEvent::Wheel {
                delta: 120,
                horizontal: false,
            },
            InputEvent::ReleaseAll,
            InputEvent::Leave,
        ];
        let mut expected = vec![InputEvent::Move { dx: 3, dy: 2 }];
        for barrier in barriers {
            queue.push(barrier.clone(), Instant::now()).unwrap();
            expected.push(barrier);
            queue
                .push(InputEvent::Move { dx: 1, dy: 2 }, Instant::now())
                .unwrap();
            queue
                .push(InputEvent::Move { dx: 3, dy: 4 }, Instant::now())
                .unwrap();
            expected.push(InputEvent::Move { dx: 4, dy: 6 });
        }
        for (index, event) in expected.into_iter().enumerate() {
            let item = queue.recv().await.unwrap();
            assert_eq!(item.value, event);
            if index == 0 {
                assert_eq!(item.queued_at, first);
            }
        }
        assert_eq!(counters.snapshot(None).incoming_moves_coalesced, 7);
        queue.close();
        assert!(queue.recv().await.is_none());
    }
    #[tokio::test]
    async fn overflow_fails_without_saturating_or_dropping_actions_and_close_discards_backlog() {
        let queue = OrderedQueue::new(Direction::Outgoing, Arc::new(Counters::default()));
        queue
            .push(
                Message::Input(InputEvent::Move {
                    dx: i32::MAX,
                    dy: 0,
                }),
                Instant::now(),
            )
            .unwrap();
        assert!(queue
            .push(
                Message::Input(InputEvent::Move { dx: 1, dy: 0 }),
                Instant::now()
            )
            .is_err());
        assert!(matches!(
            queue.recv().await.unwrap().value,
            Message::Input(InputEvent::Move { dx: i32::MAX, .. })
        ));
        for _ in 0..CAPACITY {
            queue
                .push(Message::Input(InputEvent::ReleaseAll), Instant::now())
                .unwrap();
        }
        assert!(queue
            .push(Message::Input(InputEvent::Leave), Instant::now())
            .is_err());
        queue.close();
        assert!(queue.recv().await.is_none());
        assert!(queue
            .push(Message::Input(InputEvent::Leave), Instant::now())
            .is_err());
    }
}

#[cfg(test)]
mod deadline_tests {
    use super::*;

    #[tokio::test]
    async fn four_stale_stages_are_accounted_once_without_refreshing_original_deadlines() {
        let counters = Counters::default();
        let old = Instant::now() - MAX_INPUT_AGE - Duration::from_millis(1);
        for stage in [StaleStage::OutgoingQueue, StaleStage::IncomingQueue] {
            assert_eq!(
                FailureReason::from_error(&counters.input_deadline(old, stage).unwrap_err()),
                FailureReason::StaleInput
            );
        }
        for stage in [StaleStage::OutgoingWrite, StaleStage::NativeCompletion] {
            let error = counters
                .complete_before(old + MAX_INPUT_AGE, stage, std::future::pending())
                .await
                .unwrap_err();
            assert_eq!(FailureReason::from_error(&error), FailureReason::StaleInput);
        }
        let stats = counters.snapshot(None);
        assert_eq!(stats.stale_input_failures, 4);
        assert_eq!(stats.outgoing_queue_stale_failures, 1);
        assert_eq!(stats.outgoing_write_deadline_failures, 1);
        assert_eq!(stats.incoming_queue_stale_failures, 1);
        assert_eq!(stats.native_completion_deadline_failures, 1);
        let now = Instant::now();
        assert_eq!(
            counters
                .input_deadline(now, StaleStage::IncomingQueue)
                .unwrap(),
            now + MAX_INPUT_AGE
        );
        let error = counters
            .complete_before(
                now + MAX_INPUT_AGE,
                StaleStage::NativeCompletion,
                std::future::ready(Err(anyhow::Error::new(std::io::Error::from(
                    std::io::ErrorKind::PermissionDenied,
                )))),
            )
            .await
            .unwrap_err();
        assert_eq!(
            FailureReason::from_error(&error),
            FailureReason::PermissionDenied
        );
        assert_eq!(
            counters.snapshot(None).stale_input_failures,
            4,
            "Ordinary native/IO errors must not inflate deadline counts"
        );
    }

    #[tokio::test]
    async fn a_native_stale_rejection_counts_once_as_completion_deadline() {
        let counters = Counters::default();
        let error = counters
            .complete_before(
                Instant::now() + MAX_INPUT_AGE,
                StaleStage::NativeCompletion,
                std::future::ready(Err(anyhow::anyhow!(
                    emc_platform::NativeInputFailure::StaleCommand
                ))),
            )
            .await
            .unwrap_err();
        assert_eq!(FailureReason::from_error(&error), FailureReason::StaleInput);
        let stats = counters.snapshot(None);
        assert_eq!(stats.stale_input_failures, 1);
        assert_eq!(stats.native_completion_deadline_failures, 1);
        assert_eq!(stats.incoming_queue_stale_failures, 0);
    }
}

#[cfg(test)]
mod late_completion_tests {
    use super::*;

    #[tokio::test]
    async fn blocking_then_ready_work_cannot_succeed_past_the_original_deadline() {
        let counters = Counters::default();
        let deadline = Instant::now() + Duration::from_millis(5);
        let completed = std::sync::atomic::AtomicBool::new(false);
        let error = counters
            .complete_before(deadline, StaleStage::NativeCompletion, async {
                // Deliberately model one non-yielding native operation. Tokio's
                // timeout itself cannot preempt it and polls its ready result first.
                std::thread::sleep(Duration::from_millis(15));
                completed.store(true, Ordering::Relaxed);
                Ok(())
            })
            .await
            .unwrap_err();
        assert!(completed.load(Ordering::Relaxed));
        assert_eq!(FailureReason::from_error(&error), FailureReason::StaleInput);
        let stats = counters.snapshot(None);
        assert_eq!(stats.native_completion_deadline_failures, 1);
        assert_eq!(stats.stale_input_failures, 1);
        assert_eq!(stats.incoming_queue_stale_failures, 0);
    }

    #[tokio::test]
    async fn late_error_keeps_its_typed_source_while_counting_the_expired_stage() {
        let counters = Counters::default();
        let error = counters
            .complete_before(
                Instant::now() - Duration::from_millis(1),
                StaleStage::NativeCompletion,
                std::future::ready(Err(std::io::Error::from(
                    std::io::ErrorKind::PermissionDenied,
                )
                .into())),
            )
            .await
            .unwrap_err();
        assert_eq!(FailureReason::from_error(&error), FailureReason::StaleInput);
        assert_eq!(
            error.downcast_ref::<std::io::Error>().unwrap().kind(),
            std::io::ErrorKind::PermissionDenied
        );
        let stats = counters.snapshot(None);
        assert_eq!(stats.native_completion_deadline_failures, 1);
        assert_eq!(stats.stale_input_failures, 1);
    }
}

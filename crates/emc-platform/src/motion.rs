//! Bounded movement batching with explicit ordering around clicks and keys.
use emc_protocol::InputEvent;

#[derive(Default)]
pub(crate) struct MotionBatch {
    dx: i32,
    dy: i32,
}

impl MotionBatch {
    pub fn add(&mut self, dx: i32, dy: i32) {
        self.dx = self.dx.saturating_add(dx);
        self.dy = self.dy.saturating_add(dy);
    }

    pub fn clear(&mut self) {
        self.dx = 0;
        self.dy = 0;
    }

    pub fn flush(&mut self, mut emit: impl FnMut(InputEvent) -> bool) -> bool {
        let (dx, dy) = (self.dx, self.dy);
        self.clear();
        (dx == 0 && dy == 0) || emit(InputEvent::Move { dx, dy })
    }

    pub fn action(&mut self, action: InputEvent, mut emit: impl FnMut(InputEvent) -> bool) -> bool {
        self.flush(&mut emit) && emit(action)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use emc_protocol::MouseButton;

    #[test]
    fn high_polling_motion_accumulates_every_packet_in_both_directions() {
        let mut batch = MotionBatch::default();
        for _ in 0..32 {
            batch.add(7, -11);
        }
        let mut output = vec![];
        assert!(batch.flush(|event| {
            output.push(event);
            true
        }));
        assert_eq!(output, vec![InputEvent::Move { dx: 224, dy: -352 }]);
        assert!(batch.flush(|_| panic!("A flush cannot replay old movement")));
    }

    #[test]
    fn click_and_key_cannot_overtake_accumulated_motion() {
        let mut batch = MotionBatch::default();
        let mut output = vec![];
        batch.add(10, -5);
        let click = InputEvent::Button {
            button: MouseButton::Left,
            down: true,
        };
        assert!(batch.action(click.clone(), |event| {
            output.push(event);
            true
        }));
        batch.add(-3, 8);
        let key = InputEvent::Key {
            scan: 30,
            vk: 65,
            extended: false,
            down: true,
        };
        assert!(batch.action(key.clone(), |event| {
            output.push(event);
            true
        }));
        assert_eq!(
            output,
            vec![
                InputEvent::Move { dx: 10, dy: -5 },
                click,
                InputEvent::Move { dx: -3, dy: 8 },
                key
            ]
        );
    }

    #[test]
    fn congestion_drops_following_click_and_clears_stale_motion() {
        let mut batch = MotionBatch::default();
        batch.add(4, 7);
        let mut attempts = 0;
        assert!(!batch.action(
            InputEvent::Wheel {
                delta: 120,
                horizontal: false
            },
            |_| {
                attempts += 1;
                false
            }
        ));
        assert_eq!(attempts, 1);
        assert!(batch.flush(|_| panic!("Rejected motion must not enter a later session")));
    }
}

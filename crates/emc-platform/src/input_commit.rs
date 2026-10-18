//! Original local deadlines are preserved through native queueing and commit.
//! The asynchronous production bridge/executor contracts live in windows.rs.
use anyhow::Result;
use std::time::Instant;

pub(crate) fn check_deadline(now: Instant, deadline: Instant, cleanup: bool) -> Result<()> {
    if !cleanup && now >= deadline {
        return Err(crate::NativeInputFailure::StaleCommand.into());
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    #[test]
    fn original_deadline_is_not_rebased_by_a_later_native_enqueue() {
        let received = Instant::now();
        let deadline = received + Duration::from_millis(200);
        let native_enqueue = received + Duration::from_millis(180);
        assert!(check_deadline(native_enqueue, deadline, false).is_ok());
        assert!(check_deadline(received + Duration::from_millis(200), deadline, false).is_err());
        assert!(check_deadline(received + Duration::from_millis(250), deadline, false).is_err());
        assert!(check_deadline(received + Duration::from_secs(1), deadline, true).is_ok());
    }
}

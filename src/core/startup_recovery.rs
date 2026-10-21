//! One bounded recovery for an ordinary desktop that never starts sending video.
//! Stop/restart is paired even if a late packet arrives between the two actions.
use std::time::{Duration, Instant};

#[derive(Default)]
pub struct StartupRecovery {
    waiting_since: Option<Instant>,
    resume_at: Option<Instant>,
    finished: bool,
}

impl StartupRecovery {
    pub fn needs_poll(&self) -> bool {
        !self.finished
    }
    pub fn disable(&mut self) {
        self.finished = true;
        self.waiting_since = None;
        self.resume_at = None;
    }

    pub fn in_flight(&self) -> bool {
        self.resume_at.is_some()
    }

    /// `eligible` means desktop requested, business handshake complete and an
    /// ordinary screen selected, with no user mode change in progress.
    pub fn poll(&mut self, now: Instant, eligible: bool, video_received: bool) -> Option<i32> {
        if let Some(at) = self.resume_at {
            if now >= at {
                self.resume_at = None;
                self.finished = true;
                return Some(8);
            }
            return None;
        }
        if video_received {
            self.disable();
        }
        if self.finished {
            return None;
        }
        if !eligible {
            self.waiting_since = None;
            return None;
        }
        let since = *self.waiting_since.get_or_insert(now);
        if now.duration_since(since) >= Duration::from_secs(2) {
            self.resume_at = Some(now + Duration::from_millis(500));
            self.waiting_since = None;
            return Some(7);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn never_resets_a_stream_that_has_received_video() {
        let t = Instant::now();
        let mut recovery = StartupRecovery::default();
        assert_eq!(recovery.poll(t, true, false), None);
        assert_eq!(recovery.poll(t + Duration::from_secs(1), true, true), None);
        assert_eq!(
            recovery.poll(t + Duration::from_secs(10), true, false),
            None
        );
    }

    #[test]
    fn waits_for_eligible_desktop_and_resets_the_wait_when_mode_changes() {
        let t = Instant::now();
        let mut recovery = StartupRecovery::default();
        assert_eq!(recovery.poll(t, false, false), None);
        assert_eq!(
            recovery.poll(t + Duration::from_secs(10), true, false),
            None
        );
        assert_eq!(
            recovery.poll(t + Duration::from_secs(11), false, false),
            None
        );
        assert_eq!(
            recovery.poll(t + Duration::from_secs(12), true, false),
            None
        );
        assert_eq!(
            recovery.poll(t + Duration::from_secs(14), true, false),
            Some(7)
        );
    }

    #[test]
    fn always_pairs_stop_and_restart_even_if_video_arrives_or_desktop_closes() {
        let t = Instant::now();
        let mut recovery = StartupRecovery::default();
        recovery.poll(t, true, false);
        assert_eq!(
            recovery.poll(t + Duration::from_secs(2), true, false),
            Some(7)
        );
        assert!(recovery.in_flight());
        assert_eq!(
            recovery.poll(t + Duration::from_millis(2499), false, true),
            None
        );
        assert_eq!(
            recovery.poll(t + Duration::from_millis(2500), false, true),
            Some(8)
        );
        assert!(!recovery.in_flight());
        assert_eq!(
            recovery.poll(t + Duration::from_secs(10), true, false),
            None
        );
    }

    #[test]
    fn explicit_user_pause_cancels_automatic_restart() {
        let t = Instant::now();
        let mut recovery = StartupRecovery::default();
        recovery.poll(t, true, false);
        recovery.poll(t + Duration::from_secs(2), true, false);
        recovery.disable();
        assert_eq!(recovery.poll(t + Duration::from_secs(3), true, false), None);
    }
}

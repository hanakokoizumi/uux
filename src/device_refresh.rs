//! One device-list refresh schedule for the shared account, regardless of views.
use std::time::{Duration, Instant};

pub struct DeviceRefresh {
    next: Instant,
}

impl Default for DeviceRefresh {
    fn default() -> Self {
        Self {
            next: Instant::now() + Duration::from_secs(15),
        }
    }
}

impl DeviceRefresh {
    pub fn completed(&mut self, now: Instant, success: bool) {
        self.next = now + Duration::from_secs(if success { 15 } else { 30 });
    }

    pub fn due(&mut self, now: Instant, active: bool) -> bool {
        if !active {
            self.completed(now, true);
            return false;
        }
        if now < self.next {
            return false;
        }
        // Never replay missed intervals after suspend or a slow request.
        self.completed(now, true);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refresh_is_bounded_backs_off_and_does_not_catch_up_after_suspend() {
        let now = Instant::now();
        let mut schedule = DeviceRefresh { next: now };
        assert!(schedule.due(now, true));
        assert!(!schedule.due(now + Duration::from_secs(14), true));
        assert!(schedule.due(now + Duration::from_secs(15), true));
        schedule.completed(now + Duration::from_secs(16), false);
        assert!(!schedule.due(now + Duration::from_secs(45), true));
        assert!(schedule.due(now + Duration::from_secs(46), true));
        let resumed = now + Duration::from_secs(3600);
        assert!(schedule.due(resumed, true));
        assert!(!schedule.due(resumed, true));
        assert!(!schedule.due(resumed + Duration::from_secs(60), false));
        assert!(!schedule.due(resumed + Duration::from_secs(60), true));
    }
}

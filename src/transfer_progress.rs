use serde_json::Value;
use std::{
    collections::{HashMap, VecDeque},
    time::{Duration, Instant},
};

const WINDOW: Duration = Duration::from_secs(2);

#[derive(Default)]
pub(super) struct TransferRates(HashMap<i64, Rate>);

impl TransferRates {
    pub fn update(&mut self, event: &Value, now: Instant) {
        if let Some(id) = event["id"].as_i64() {
            let rate = self.0.entry(id).or_default();
            rate.update(
                event["done"]
                    .as_u64()
                    .unwrap_or(0)
                    .saturating_sub(event["initial_offset"].as_u64().unwrap_or(0)),
                Duration::from_millis(event["elapsed_ms"].as_u64().unwrap_or(0)),
                event["status"] == "传输中" && event["directory"] != true,
                now,
            );
        }
    }

    pub fn bytes_per_second(&self, event: &Value, now: Instant) -> Option<f64> {
        if event["status"] != "传输中" || event["directory"] == true {
            return None;
        }
        self.0.get(&event["id"].as_i64()?).map(|r| r.speed(now))
    }

    pub fn retain(&mut self, transfers: &[Value]) {
        let ids = transfers
            .iter()
            .filter_map(|t| t["id"].as_i64())
            .collect::<std::collections::HashSet<_>>();
        self.0.retain(|id, _| ids.contains(id));
    }
}

#[derive(Default)]
struct Rate {
    samples: VecDeque<(Duration, u64)>,
    arrived: Option<Instant>,
    active: bool,
}

impl Rate {
    fn update(&mut self, done: u64, elapsed: Duration, active: bool, now: Instant) {
        // Source timestamps keep a UI repaint delay from turning a batch of
        // progress notifications into a fictitious speed spike.
        if !active
            || !self.active
            || self
                .samples
                .back()
                .is_some_and(|&(t, n)| elapsed < t || done < n)
        {
            self.samples.clear();
        }
        if self.samples.back().is_some_and(|&(t, _)| t == elapsed) {
            self.samples.pop_back();
        }
        self.samples.push_back((elapsed, done));
        while self.samples.len() > 2 && self.samples[1].0 < elapsed.saturating_sub(WINDOW) {
            self.samples.pop_front();
        }
        self.active = active;
        self.arrived = Some(now);
    }

    fn speed(&self, now: Instant) -> f64 {
        let Some(&(last_time, last_bytes)) = self.samples.back() else {
            return 0.0;
        };
        let idle = self
            .arrived
            .map_or(Duration::ZERO, |t| now.saturating_duration_since(t));
        if !self.active || idle >= WINDOW {
            return 0.0;
        }
        let end = last_time + idle;
        let cutoff = end.saturating_sub(WINDOW);
        // Keep the preceding sample to include the first block in the window.
        let base = self
            .samples
            .iter()
            .rfind(|&&(t, _)| t <= cutoff)
            .or_else(|| self.samples.front())
            .unwrap();
        last_bytes.saturating_sub(base.1) as f64
            / end.saturating_sub(base.0).as_secs_f64().max(0.25)
    }
}

pub(super) fn fraction(event: &Value) -> Option<f32> {
    if event["status"] == "已完成" {
        return Some(1.0);
    }
    if event["directory"] == true {
        return None;
    }
    let size = event["size"].as_u64().unwrap_or(0);
    if size == 0 {
        return (event["size_known"] == true).then_some(0.0);
    }
    Some((event["done"].as_u64().unwrap_or(0) as f64 / size as f64).clamp(0.0, 1.0) as f32)
}

pub(super) fn percentage(event: &Value) -> String {
    let Some(_) = fraction(event) else {
        return "—".into();
    };
    if event["status"] == "已完成" {
        return "100%".into();
    }
    let size = event["size"].as_u64().unwrap_or(0);
    let tenths = if size > 0 {
        (u128::from(event["done"].as_u64().unwrap_or(0)) * 1000 / u128::from(size)).min(1000)
    } else {
        0
    };
    if tenths == 1000 {
        "100%".into()
    } else {
        format!("{}.{:01}%", tenths / 10, tenths % 10)
    }
}

pub(super) fn speed_text(speed: Option<f64>) -> String {
    let Some(speed) = speed else {
        return "—".into();
    };
    let speed = if speed.is_finite() {
        speed.max(0.0)
    } else {
        0.0
    };
    if speed >= 1024.0 * 1024.0 {
        format!("{:.2} MiB/s", speed / (1024.0 * 1024.0))
    } else if speed >= 1024.0 {
        format!("{:.1} KiB/s", speed / 1024.0)
    } else {
        format!("{speed:.0} B/s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rate_uses_ack_timestamps_even_when_ui_receives_a_batch() {
        let now = Instant::now();
        let mut rate = Rate::default();
        rate.update(0, Duration::ZERO, true, now);
        rate.update(512 * 1024, Duration::from_secs(1), true, now);
        rate.update(1024 * 1024, Duration::from_secs(2), true, now);
        assert_eq!(rate.speed(now), 512.0 * 1024.0);
        assert_eq!(rate.speed(now + WINDOW), 0.0);
    }

    #[test]
    fn pause_resume_and_resume_offset_do_not_count_old_bytes() {
        let now = Instant::now();
        let mut rate = Rate::default();
        rate.update(100_000, Duration::from_secs(1), true, now);
        assert_eq!(rate.speed(now), 0.0);
        rate.update(120_000, Duration::from_secs(2), true, now);
        assert_eq!(rate.speed(now), 20_000.0);
        rate.update(120_000, Duration::from_secs(3), false, now);
        assert_eq!(rate.speed(now), 0.0);
        rate.update(120_000, Duration::from_secs(50), true, now);
        rate.update(130_000, Duration::from_secs(51), true, now);
        assert_eq!(rate.speed(now), 10_000.0);
        rate.update(0, Duration::ZERO, true, now);
        assert_eq!(rate.speed(now), 0.0);
    }

    #[test]
    fn long_stall_then_new_block_is_not_a_full_window_speed() {
        let now = Instant::now();
        let mut rate = Rate::default();
        rate.update(0, Duration::ZERO, true, now);
        rate.update(1000, Duration::from_secs(20), true, now);
        assert_eq!(rate.speed(now), 50.0);
    }

    #[test]
    fn percentages_handle_empty_unknown_finished_and_unconfirmed_files() {
        assert_eq!(
            percentage(&json!({"size":1000,"done":999,"status":"传输中"})),
            "99.9%"
        );
        assert_eq!(
            percentage(&json!({"size":0,"size_known":true,"status":"传输中"})),
            "0.0%"
        );
        assert_eq!(percentage(&json!({"size":0,"status":"准备中"})), "—");
        assert_eq!(percentage(&json!({"size":0,"status":"已完成"})), "100%");
        assert_eq!(
            percentage(&json!({"size":10,"done":10,"status":"等待远端确认"})),
            "100%"
        );
        assert_eq!(
            fraction(&json!({"directory":true,"status":"文件夹传输中"})),
            None
        );
        assert_eq!(fraction(&json!({"size":10,"done":20})), Some(1.0));
    }

    #[test]
    fn completed_records_have_no_live_speed_and_cleanup_removes_samples() {
        let now = Instant::now();
        let event = json!({"id":1,"done":1024,"elapsed_ms":1000,"status":"传输中"});
        let mut rates = TransferRates::default();
        rates.update(&event, now);
        assert_eq!(
            rates.bytes_per_second(&json!({"id":1,"status":"已完成"}), now),
            None
        );
        rates.retain(&[]);
        assert!(rates.0.is_empty());
        assert_eq!(speed_text(Some(1024.0)), "1.0 KiB/s");
        assert_eq!(speed_text(Some(1048576.0)), "1.00 MiB/s");
    }
}

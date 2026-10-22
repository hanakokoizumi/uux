//! Explicit local-stdin test mode; never enabled during ordinary GUI use.
use crate::backend::VideoFrame;
use eframe::egui;
use serde_json::{Value, json};
use std::{
    io::BufRead,
    path::PathBuf,
    sync::mpsc,
    time::{Duration, Instant},
};

pub struct DiagnosticIo {
    pub commands: mpsc::Receiver<Value>,
    snapshots: mpsc::SyncSender<(PathBuf, usize, usize, Vec<u8>)>,
    last_snapshot: Option<Instant>,
    input_probe: Option<InputProbe>,
}

struct InputProbe {
    id: u64,
    generation: u64,
    point: [usize; 2],
    rgb: [u8; 3],
    started: Instant,
}
impl InputProbe {
    fn from_command(command: &Value) -> Option<Self> {
        if command["op"] != "input" {
            return None;
        }
        let probe = command.get("diagnostic_probe")?;
        let x = usize::try_from(probe["point"][0].as_u64()?).ok()?;
        let y = usize::try_from(probe["point"][1].as_u64()?).ok()?;
        if x >= 8192 || y >= 4320 {
            return None;
        }
        Some(Self {
            id: probe["id"].as_u64()?,
            generation: command["generation"].as_u64()?,
            point: [x, y],
            rgb: [
                u8::try_from(probe["rgb"][0].as_u64()?).ok()?,
                u8::try_from(probe["rgb"][1].as_u64()?).ok()?,
                u8::try_from(probe["rgb"][2].as_u64()?).ok()?,
            ],
            started: Instant::now(),
        })
    }
    fn matches(&self, frame: &VideoFrame) -> bool {
        if frame.generation != self.generation
            || frame.decoded_at < self.started
            || self.point[0] >= frame.width
            || self.point[1] >= frame.height
        {
            return false;
        }
        let offset = (self.point[1] * frame.width + self.point[0]) * 4;
        frame
            .rgba
            .get(offset..offset + 3)
            .is_some_and(|pixel| pixel.iter().zip(self.rgb).all(|(a, b)| a.abs_diff(b) <= 24))
    }
}

impl DiagnosticIo {
    pub fn start(ctx: egui::Context) -> Self {
        let (tx, commands) = mpsc::sync_channel(128);
        std::thread::spawn(move || {
            for line in std::io::stdin().lock().lines().map_while(Result::ok) {
                if let Ok(command) = serde_json::from_str(&line) {
                    if tx.send(command).is_err() {
                        break;
                    }
                    ctx.request_repaint_of(egui::ViewportId::ROOT);
                }
            }
        });
        let (snapshots, rx) = mpsc::sync_channel::<(PathBuf, usize, usize, Vec<u8>)>(1);
        std::thread::spawn(move || {
            for (path, width, height, pixels) in rx {
                let save = || -> anyhow::Result<()> {
                    let parent = path
                        .parent()
                        .filter(|p| !p.as_os_str().is_empty())
                        .unwrap_or_else(|| std::path::Path::new("."));
                    let temporary = tempfile::NamedTempFile::new_in(parent)?;
                    image::save_buffer_with_format(
                        temporary.path(),
                        &pixels,
                        width as u32,
                        height as u32,
                        image::ColorType::Rgba8,
                        image::ImageFormat::Png,
                    )?;
                    temporary.persist(path)?;
                    Ok(())
                };
                if let Err(error) = save() {
                    eprintln!("诊断画面保存失败：{error}");
                }
            }
        });
        Self {
            commands,
            snapshots,
            last_snapshot: None,
            input_probe: None,
        }
    }
    /// Starts immediately before dispatching a diagnostic input through Backend.
    pub fn observe_command(&mut self, command: &Value) {
        if let Some(probe) = InputProbe::from_command(command) {
            self.input_probe = Some(probe);
        }
    }
    pub fn observe_frame(&mut self, frame: Option<&VideoFrame>) {
        let Some(probe) = &self.input_probe else {
            return;
        };
        let matched = frame.filter(|frame| probe.matches(frame));
        if matched.is_none() && probe.started.elapsed() < Duration::from_secs(5) {
            return;
        }
        let result = json!({"id":probe.id,"generation":probe.generation,"matched":matched.is_some(),
            "input_to_decode_ms":matched.map(|f|f.decoded_at.duration_since(probe.started).as_secs_f64()*1000.0),
            "input_to_consume_ms":matched.map(|_|probe.started.elapsed().as_secs_f64()*1000.0)});
        crate::perf::trace("input_latency", result.clone());
        println!("{}", json!({"event":"input_probe","data":result}));
        self.input_probe = None;
    }
    pub fn snapshot(&mut self, frame: &VideoFrame, path: Option<&PathBuf>) {
        if let Some(path) = path
            && self
                .last_snapshot
                .is_none_or(|t| t.elapsed() >= Duration::from_secs(3))
        {
            // PNG encoding must never hold the latest-frame mutex or run on the UI thread.
            if self
                .snapshots
                .try_send((path.clone(), frame.width, frame.height, frame.rgba.clone()))
                .is_ok()
            {
                self.last_snapshot = Some(Instant::now());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn input_probe_ignores_old_frames_wrong_sessions_and_wrong_colors() {
        let probe = InputProbe::from_command(&json!({"op":"input","generation":2,
            "diagnostic_probe":{"id":1,"point":[1,0],"rgb":[255,255,255]}}))
        .unwrap();
        let mut frame = VideoFrame {
            geometry: crate::media_bus::Geometry {
                screen_id: 0,
                version: 1,
                width: 2,
                height: 2,
            },
            generation: 2,
            sequence: 1,
            width: 2,
            height: 1,
            rgba: vec![0, 0, 0, 255, 255, 255, 255, 255],
            assembled_at: Instant::now(),
            decoded_at: Instant::now(),
        };
        assert!(probe.matches(&frame));
        frame.decoded_at = probe.started - Duration::from_millis(1);
        assert!(!probe.matches(&frame));
        frame.decoded_at = Instant::now();
        frame.generation = 1;
        assert!(!probe.matches(&frame));
        frame.generation = 2;
        frame.rgba[4] = 10;
        assert!(!probe.matches(&frame));
        frame.rgba.clear();
        assert!(!probe.matches(&frame));
    }
}

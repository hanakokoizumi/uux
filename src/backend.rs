use crate::core::{cloud::Cloud, runtime, session::Output};
use eframe::egui;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::{Arc, Mutex, mpsc},
    thread,
};
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Device {
    pub id: String,
    pub name: String,
    pub online: bool,
    pub shared: bool,
    pub version: String,
    pub wallpaper: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    Native {
        kind: String,
        data: Value,
    },
    Hello {
        protocol: u32,
    },
    State {
        logged_in: bool,
        nickname: String,
    },
    Devices {
        devices: Vec<Device>,
    },
    Preview {
        device_id: String,
        image: Vec<u8>,
    },
    Qr {
        login_id: u64,
        png: String,
    },
    LoginStatus {
        login_id: u64,
        status: u32,
    },
    LoginComplete {
        login_id: u64,
    },
    Connecting {
        generation: u64,
    },
    Phase {
        generation: u64,
        phase: String,
    },
    Connected {
        generation: u64,
    },
    Disconnected {
        generation: u64,
    },
    Error {
        scope: String,
        message: String,
        generation: Option<u64>,
        login_id: Option<u64>,
    },
}

pub fn remote_viewport_id() -> egui::ViewportId {
    egui::ViewportId::from_hash_of("uu-remote-desktop")
}
pub fn files_viewport_id() -> egui::ViewportId {
    egui::ViewportId::from_hash_of("uu-file-transfer")
}
pub fn terminal_viewport_id() -> egui::ViewportId {
    egui::ViewportId::from_hash_of("uu-terminal")
}
pub fn ports_viewport_id() -> egui::ViewportId {
    egui::ViewportId::from_hash_of("uu-ports")
}

/// Repainting an unknown egui ID creates a viewport entry. Only wake windows
/// the integration has actually registered, so closed views stay closed.
pub fn repaint_ui(ctx: &egui::Context, video_only: bool) {
    ctx.request_repaint_of(egui::ViewportId::ROOT);
    let known = ctx.input(|i| {
        [remote_viewport_id(), files_viewport_id()].map(|id| i.raw.viewports.contains_key(&id))
    });
    if known[0] {
        ctx.request_repaint_of(remote_viewport_id());
    }
    if !video_only && known[1] {
        ctx.request_repaint_of(files_viewport_id());
    }
    if !video_only && ctx.input(|i| i.raw.viewports.contains_key(&terminal_viewport_id())) {
        ctx.request_repaint_of(terminal_viewport_id());
    }
    if !video_only && ctx.input(|i| i.raw.viewports.contains_key(&ports_viewport_id())) {
        ctx.request_repaint_of(ports_viewport_id());
    }
}

pub struct VideoFrame {
    pub generation: u64,
    pub sequence: u64,
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>,
    pub assembled_at: std::time::Instant,
    pub decoded_at: std::time::Instant,
}

pub struct Backend {
    commands: Option<tokio::sync::mpsc::Sender<Value>>,
    pub events: mpsc::Receiver<Event>,
    pub latest: Arc<Mutex<Option<VideoFrame>>>,
    worker: Option<thread::JoinHandle<()>>,
    remote_stop: Option<Arc<std::sync::atomic::AtomicBool>>,
    remote_frames: Option<thread::JoinHandle<()>>,
}
impl Backend {
    pub fn start(state: Option<&Path>, ctx: egui::Context) -> Result<Self, String> {
        let directory = state
            .map(Path::to_path_buf)
            .unwrap_or_else(crate::core::cloud::state_directory);
        let cloud = Cloud::new(Some(directory.clone()), None).map_err(|e| e.to_string())?;
        let cloud = std::fs::read(directory.join("desktop.json"))
            .ok()
            .and_then(|data| serde_json::from_slice::<Value>(&data).ok())
            .and_then(|value| {
                serde_json::from_value::<crate::core::cloud::ProxySettings>(value["proxy"].clone())
                    .ok()
            })
            .and_then(|settings| cloud.with_proxy(&settings).ok())
            .unwrap_or(cloud);
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .enable_all()
            .build()
            .map_err(|e| e.to_string())?;
        let (tx, rx) = tokio::sync::mpsc::channel(1024);
        let (events_tx, events) = mpsc::channel();
        let latest = Arc::new(Mutex::new(None));
        let output = Output {
            generation: None,
            events: events_tx,
            latest: latest.clone(),
            ctx,
        };
        let worker = thread::spawn(move || rt.block_on(runtime::run(cloud, output, rx)));
        Ok(Self {
            commands: Some(tx),
            events,
            latest,
            worker: Some(worker),
            remote_stop: None,
            remote_frames: None,
        })
    }
    pub fn send(&self, command: Value) -> Result<(), String> {
        self.commands
            .as_ref()
            .ok_or("核心已关闭")?
            .try_send(command)
            .map_err(|_| "核心队列繁忙或已停止".into())
    }
    pub fn shutdown(&mut self) {
        if let Some(stop) = self.remote_stop.take() {
            stop.store(true, std::sync::atomic::Ordering::Relaxed);
            self.commands.take();
            if let Some(worker) = self.worker.take() {
                let _ = worker.join();
            }
            if let Some(worker) = self.remote_frames.take() {
                let _ = worker.join();
            }
            return;
        }
        if let Some(tx) = self.commands.take() {
            let _ = tx.blocking_send(json!({"op":"quit"}));
            drop(tx);
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }

    pub fn shared(state: Option<&Path>, ctx: egui::Context) -> Result<Self, String> {
        let client =
            Arc::new(crate::ipc::Client::connect(state, "gui").map_err(|e| e.to_string())?);
        let (tx, mut rx) = tokio::sync::mpsc::channel::<Value>(1024);
        let (events_tx, events) = mpsc::channel();
        let latest = Arc::new(Mutex::new(None));
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let quit = stop.clone();
        let worker_client = client.clone();
        let worker_ctx = ctx.clone();
        let worker = thread::spawn(move || {
            let mut last_poll = std::time::Instant::now() - std::time::Duration::from_secs(1);
            while !quit.load(std::sync::atomic::Ordering::Relaxed) {
                while let Ok(command) = rx.try_recv() {
                    if command["op"] == "service_call" {
                        let result=worker_client.call(json!({"op":"call","name":command["name"],"arguments":command["arguments"]}));
                        let data = match result {
                            Ok(result) => json!({"name":command["name"],"result":result}),
                            Err(e) => json!({"name":command["name"],"error":e.to_string()}),
                        };
                        let _ = events_tx.send(Event::Native {
                            kind: "service_result".into(),
                            data,
                        });
                        repaint_ui(&worker_ctx, false);
                        continue;
                    }
                    if let Err(e) =
                        worker_client.call(json!({"op":"gui_command","command":command}))
                    {
                        let _ = events_tx.send(Event::Error {
                            scope: "operation".into(),
                            message: e.to_string(),
                            generation: None,
                            login_id: None,
                        });
                    }
                }
                if last_poll.elapsed() >= std::time::Duration::from_millis(15) {
                    match worker_client.call(json!({"op":"poll"})) {
                        Ok(value) => {
                            for value in value["events"].as_array().into_iter().flatten() {
                                if let Ok(event) = serde_json::from_value(value.clone()) {
                                    let _ = events_tx.send(event);
                                }
                            }
                            if value["events"].as_array().is_some_and(|v| !v.is_empty()) {
                                repaint_ui(&worker_ctx, false);
                            }
                        }
                        Err(e) => {
                            let _ = events_tx.send(Event::Error {
                                scope: "bridge".into(),
                                message: e.to_string(),
                                generation: None,
                                login_id: None,
                            });
                            repaint_ui(&worker_ctx, false);
                            break;
                        }
                    }
                    last_poll = std::time::Instant::now();
                }
                thread::sleep(std::time::Duration::from_millis(2));
            }
        });
        let frames = latest.clone();
        let quit = stop.clone();
        let remote_frames = thread::spawn(move || {
            let mut last = (0, 0);
            while !quit.load(std::sync::atomic::Ordering::Relaxed) {
                if let Ok(Some(frame)) = client.frame(last) {
                    last = (frame.generation, frame.sequence);
                    *frames.lock().unwrap() = Some(frame);
                    repaint_ui(&ctx, true);
                }
                thread::sleep(std::time::Duration::from_millis(8));
            }
        });
        Ok(Self {
            commands: Some(tx),
            events,
            latest,
            worker: Some(worker),
            remote_stop: Some(stop),
            remote_frames: Some(remote_frames),
        })
    }
}
impl Drop for Backend {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
impl Backend {
    pub(crate) fn fixture() -> (
        Self,
        mpsc::Sender<Event>,
        tokio::sync::mpsc::Receiver<Value>,
    ) {
        let (commands, receiver) = tokio::sync::mpsc::channel(1024);
        let (sender, events) = mpsc::channel();
        (
            Self {
                commands: Some(commands),
                events,
                latest: Arc::new(Mutex::new(None)),
                worker: None,
                remote_stop: None,
                remote_frames: None,
            },
            sender,
            receiver,
        )
    }
}

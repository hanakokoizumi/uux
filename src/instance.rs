//! GUI single-instance guard, independent of the headless core lifetime.
use anyhow::{Context, Result, bail};
use serde_json::json;
use std::{
    fs::File,
    os::unix::net::{UnixListener, UnixStream},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
pub struct Instance {
    _lock: File,
    listener: UnixListener,
    path: PathBuf,
}
impl Instance {
    pub fn acquire(state: Option<&Path>) -> Result<Option<Self>> {
        let dir = crate::ipc::directory(state)?;
        let path = dir.join("gui.sock");
        let Some(lock) = crate::ipc::lock(&dir.join("gui.lock"))? else {
            let start = Instant::now();
            loop {
                if let Ok(mut stream) = UnixStream::connect(&path) {
                    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
                    crate::ipc::write(
                        &mut stream,
                        &json!({"activate":true,"activation_token":std::env::var("XDG_ACTIVATION_TOKEN").or_else(|_|std::env::var("DESKTOP_STARTUP_ID")).ok()}),
                    )?;
                    crate::ipc::read(&mut stream).context("正在运行的窗口未响应唤起请求")?;
                    return Ok(None);
                }
                if start.elapsed() > Duration::from_secs(12) {
                    bail!("程序正在启动，但主窗口暂未就绪");
                }
                std::thread::sleep(Duration::from_millis(40));
            }
        };
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path)?;
        Ok(Some(Self {
            _lock: lock,
            listener,
            path,
        }))
    }
    pub fn activate_with(self, ctx: eframe::egui::Context) {
        std::thread::spawn(move || {
            for stream in self.listener.incoming() {
                let Ok(mut stream) = stream else {
                    break;
                };
                if !crate::ipc::peer_is_owner(&stream) {
                    continue;
                }
                let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                if let Ok(request) = crate::ipc::read(&mut stream) {
                    if let Some(token) = request["activation_token"]
                        .as_str()
                        .filter(|t| !t.is_empty())
                    {
                        ctx.data_mut(|d| {
                            d.insert_temp(
                                eframe::egui::Id::unique("instance_activation"),
                                token.to_owned(),
                            )
                        });
                    }
                    use eframe::egui::{ViewportCommand, ViewportId};
                    ctx.send_viewport_cmd_to(ViewportId::ROOT, ViewportCommand::Visible(true));
                    ctx.send_viewport_cmd_to(ViewportId::ROOT, ViewportCommand::Minimized(false));
                    ctx.send_viewport_cmd_to(ViewportId::ROOT, ViewportCommand::Focus);
                    ctx.request_repaint();
                    let _ = crate::ipc::write(&mut stream, &serde_json::json!({"activated":true}));
                }
            }
            let _ = std::fs::remove_file(&self.path);
            drop(self._lock);
        });
    }
}

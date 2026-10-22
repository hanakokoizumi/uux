use base64::Engine;
use bytes::Bytes;
use serde_json::{Value, json};
use std::{
    borrow::Cow,
    hash::{Hash, Hasher},
};
use std::{sync::mpsc, time::Duration};
pub struct Clipboard {
    commands: mpsc::Sender<Command>,
}
enum Command {
    Remote(String),
    RemoteImage(Vec<u8>),
    RemoteFiles(Vec<std::path::PathBuf>, std::sync::Arc<tempfile::TempDir>),
    Local(Value),
    Enable(bool),
    Paste,
    Stop,
}
impl Clipboard {
    pub fn start(enabled: bool, incoming: tokio::sync::mpsc::Sender<(String, Bytes)>) -> Self {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut clipboard = match arboard::Clipboard::new() {
                Ok(value) => value,
                Err(_) => {
                    let _ = incoming.try_send((
                        "CLIPBOARD_ERROR".into(),
                        Bytes::from_static("无法访问系统剪贴板".as_bytes()),
                    ));
                    return;
                }
            };
            let mut enabled = enabled;
            let mut last = clipboard.get_text().unwrap_or_default();
            let mut last_image = None;
            let mut last_files = clipboard.get().file_list().unwrap_or_default();
            // Keep received paths valid while this clipboard owner is alive. A replacement
            // may race a file manager already copying them, so do not delete on replacement.
            let mut received_files = Vec::new();
            loop {
                match rx.recv_timeout(Duration::from_millis(250)) {
                    Ok(Command::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    Ok(Command::Enable(value)) => {
                        enabled = value;
                        last = clipboard.get_text().unwrap_or_default();
                        last_image = clipboard.get_image().ok().map(|i| image_hash(&i));
                        last_files = clipboard.get().file_list().unwrap_or_default();
                    }
                    Ok(Command::RemoteFiles(paths, storage)) => {
                        if enabled {
                            match clipboard.set().file_list(&paths) {
                                Ok(()) => {
                                    last_files = paths;
                                    last.clear();
                                    last_image = None;
                                    received_files.push(storage);
                                    let _ = incoming.try_send((
                                        "CLIPBOARD_APPLIED".into(),
                                        Bytes::from_static(b"0"),
                                    ));
                                }
                                Err(e) => {
                                    let _ = incoming.try_send((
                                        "CLIPBOARD_ERROR".into(),
                                        Bytes::from(e.to_string()),
                                    ));
                                }
                            }
                        }
                    }
                    Ok(Command::Local(command)) => {
                        let result = local(&mut clipboard, &command);
                        last = clipboard.get_text().unwrap_or_default();
                        last_image = clipboard.get_image().ok().map(|i| image_hash(&i));
                        last_files = clipboard.get().file_list().unwrap_or_default();
                        let result = match result {
                            Ok(value) => {
                                json!({"client_request":command["client_request"],"value":value})
                            }
                            Err(e) => {
                                json!({"client_request":command["client_request"],"error":e.to_string()})
                            }
                        };
                        let _ = incoming.try_send((
                            "LOCAL_CLIPBOARD_RESULT".into(),
                            Bytes::from(result.to_string()),
                        ));
                    }
                    Ok(Command::RemoteImage(png)) => {
                        if enabled {
                            let result = set_image(&mut clipboard, &png)
                                .map(|digest| last_image = Some(digest));
                            if let Err(e) = result {
                                let _ = incoming.try_send((
                                    "CLIPBOARD_ERROR".into(),
                                    Bytes::from(e.to_string()),
                                ));
                            } else {
                                last.clear();
                                let _ = incoming.try_send((
                                    "CLIPBOARD_APPLIED".into(),
                                    Bytes::from(png.len().to_string()),
                                ));
                            }
                        }
                    }
                    Ok(Command::Remote(text)) => {
                        if enabled && text.len() <= 1024 * 1024 && text != last {
                            if clipboard.set_text(&text).is_ok() {
                                let _ = incoming.try_send((
                                    "CLIPBOARD_APPLIED".into(),
                                    Bytes::from(text.len().to_string()),
                                ));
                                last = text;
                                last_image = None;
                            } else {
                                let _ = incoming.try_send((
                                    "CLIPBOARD_ERROR".into(),
                                    Bytes::from_static("无法写入系统剪贴板".as_bytes()),
                                ));
                            }
                        }
                    }
                    Ok(Command::Paste) => {
                        if let Ok(paths) = clipboard.get().file_list()
                            && !paths.is_empty()
                        {
                            if paths.len() > 1000 {
                                let _ = incoming.try_send((
                                    "CLIPBOARD_ERROR".into(),
                                    Bytes::from_static("一次最多粘贴 1000 项".as_bytes()),
                                ));
                            } else {
                                let _ = incoming.try_send((
                                    "LOCAL_CLIPBOARD_FILES_PASTE".into(),
                                    Bytes::from(json!(paths).to_string()),
                                ));
                            }
                            continue;
                        }
                        if let Ok(image) = clipboard.get_image() {
                            match image_png(&image) {
                                Ok(png) => {
                                    let _ = incoming.try_send((
                                        "LOCAL_CLIPBOARD_IMAGE_PASTE".into(),
                                        Bytes::from(png),
                                    ));
                                }
                                Err(e) => {
                                    let _ = incoming.try_send((
                                        "CLIPBOARD_ERROR".into(),
                                        Bytes::from(e.to_string()),
                                    ));
                                }
                            }
                            continue;
                        }
                        if let Ok(text) = clipboard.get_text() {
                            if text.len() > 65536 {
                                let _ = incoming.try_send((
                                    "CLIPBOARD_ERROR".into(),
                                    Bytes::from_static("单次粘贴文字不能超过 64 KiB".as_bytes()),
                                ));
                                continue;
                            }
                            last = text.clone();
                            let _ = incoming
                                .try_send(("LOCAL_CLIPBOARD_PASTE".into(), Bytes::from(text)));
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                }
                if enabled {
                    let paths = clipboard.get().file_list().unwrap_or_default();
                    if !paths.is_empty() {
                        if paths != last_files
                            && paths.len() <= 1000
                            && incoming
                                .try_send((
                                    "LOCAL_CLIPBOARD_FILES".into(),
                                    Bytes::from(json!(paths).to_string()),
                                ))
                                .is_ok()
                        {
                            last_files = paths;
                            last.clear();
                            last_image = None;
                        }
                        continue;
                    }
                    last_files.clear();
                }
                if enabled
                    && let Ok(text) = clipboard.get_text()
                    && text != last
                {
                    if text.len() > 65536 {
                        let _ = incoming.try_send((
                            "CLIPBOARD_ERROR".into(),
                            Bytes::from_static(
                                "剪贴板文字超过 64 KiB，可通过文件传输发送".as_bytes(),
                            ),
                        ));
                        last = text;
                    } else if incoming
                        .try_send(("LOCAL_CLIPBOARD".into(), Bytes::from(text.clone())))
                        .is_ok()
                    {
                        last = text;
                        last_image = None;
                    }
                }
                if enabled
                    && clipboard.get_text().is_err()
                    && let Ok(image) = clipboard.get_image()
                {
                    let digest = image_hash(&image);
                    if last_image != Some(digest) {
                        match image_png(&image) {
                            Ok(png) => {
                                if incoming
                                    .try_send(("LOCAL_CLIPBOARD_IMAGE".into(), Bytes::from(png)))
                                    .is_ok()
                                {
                                    last_image = Some(digest);
                                    last.clear();
                                }
                            }
                            Err(e) => {
                                last_image = Some(digest);
                                let _ = incoming.try_send((
                                    "CLIPBOARD_ERROR".into(),
                                    Bytes::from(e.to_string()),
                                ));
                            }
                        }
                    }
                }
            }
        });
        Self { commands: tx }
    }
    pub fn remote(&self, text: String) {
        let _ = self.commands.send(Command::Remote(text));
    }
    pub fn remote_image(&self, png: Vec<u8>) {
        let _ = self.commands.send(Command::RemoteImage(png));
    }
    pub fn remote_files(
        &self,
        paths: Vec<std::path::PathBuf>,
        storage: std::sync::Arc<tempfile::TempDir>,
    ) {
        let _ = self.commands.send(Command::RemoteFiles(paths, storage));
    }
    pub fn local(&self, command: Value) {
        let _ = self.commands.send(Command::Local(command));
    }
    pub fn enable(&self, value: bool) {
        let _ = self.commands.send(Command::Enable(value));
    }
    pub fn paste(&self) {
        let _ = self.commands.send(Command::Paste);
    }
}
fn image_hash(image: &arboard::ImageData<'_>) -> u64 {
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    image.width.hash(&mut hash);
    image.height.hash(&mut hash);
    image.bytes.hash(&mut hash);
    hash.finish()
}
fn image_png(image: &arboard::ImageData<'_>) -> anyhow::Result<Vec<u8>> {
    anyhow::ensure!(
        image.width > 0 && image.height > 0 && image.width <= 3840 && image.height <= 2160,
        "本地剪贴板图片超过 3840×2160"
    );
    let rgba = image::RgbaImage::from_raw(
        image.width as u32,
        image.height as u32,
        image.bytes.to_vec(),
    )
    .ok_or_else(|| anyhow::anyhow!("本地剪贴板像素无效"))?;
    super::clipboard_image::png(&rgba)
}
fn set_image(clipboard: &mut arboard::Clipboard, png: &[u8]) -> anyhow::Result<u64> {
    let rgba = super::clipboard_image::from_png(png)?;
    let data = arboard::ImageData {
        width: rgba.width() as usize,
        height: rgba.height() as usize,
        bytes: Cow::Borrowed(rgba.as_raw()),
    };
    let digest = image_hash(&data);
    clipboard.set_image(data)?;
    Ok(digest)
}
fn local(clipboard: &mut arboard::Clipboard, command: &Value) -> anyhow::Result<Value> {
    let format = command["format"].as_str().unwrap_or("text");
    let get = command["op"] == "clipboard_get";
    let mut result =
        json!({"source":"local","format":format,"observed_at_ms":crate::service::now_ms()});
    match (get, format) {
        (true, "text") => {
            result["text"] = json!(clipboard.get_text()?);
            anyhow::ensure!(
                result["text"].as_str().unwrap().len() <= 65536,
                "本地文本超过 64 KiB"
            );
        }
        (true, "files") => {
            let paths = clipboard.get().file_list()?;
            anyhow::ensure!(paths.len() <= 1000, "本地剪贴板文件超过 1000 项");
            result["paths"] = json!(paths);
        }
        (false, "files") => {
            let paths = command["paths"]
                .as_array()
                .ok_or_else(|| anyhow::anyhow!("缺少 paths"))?
                .iter()
                .map(|v| {
                    v.as_str()
                        .map(std::path::PathBuf::from)
                        .ok_or_else(|| anyhow::anyhow!("文件路径无效"))
                })
                .collect::<anyhow::Result<Vec<_>>>()?;
            anyhow::ensure!(
                !paths.is_empty()
                    && paths.len() <= 1000
                    && paths.iter().all(|p| p.is_absolute() && p.exists()),
                "paths 须为有效本地绝对路径"
            );
            clipboard.set().file_list(&paths)?;
            result["written"] = json!(true);
            result["paths"] = json!(paths);
        }
        (true, "image") => {
            let image = clipboard.get_image()?;
            let png = image_png(&image)?;
            result["mime_type"] = json!("image/png");
            result["data_base64"] = json!(base64::engine::general_purpose::STANDARD.encode(png));
        }
        (false, "text") => {
            let text = command["text"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("缺少 text"))?;
            anyhow::ensure!(text.len() <= 65536, "本地文本超过 64 KiB");
            clipboard.set_text(text)?;
            result["written"] = json!(true);
        }
        (false, "image") => {
            let data = command["data_base64"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("缺少 PNG data_base64"))?;
            anyhow::ensure!(data.len() <= 2 * 1024 * 1024 * 4 / 3 + 4, "PNG 超过 2 MiB");
            set_image(
                clipboard,
                &base64::engine::general_purpose::STANDARD.decode(data)?,
            )?;
            result["written"] = json!(true);
        }
        _ => anyhow::bail!("尚未实现此剪贴板格式：{format}"),
    }
    Ok(result)
}
impl Drop for Clipboard {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Stop);
    }
}

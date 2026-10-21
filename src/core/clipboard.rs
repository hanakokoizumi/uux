use bytes::Bytes;
use std::{sync::mpsc, time::Duration};
pub struct Clipboard {
    commands: mpsc::Sender<Command>,
}
enum Command {
    Remote(String),
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
            loop {
                match rx.recv_timeout(Duration::from_millis(250)) {
                    Ok(Command::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    Ok(Command::Enable(value)) => {
                        enabled = value;
                        last = clipboard.get_text().unwrap_or_default();
                    }
                    Ok(Command::Remote(text)) => {
                        if enabled && text.len() <= 1024 * 1024 && text != last {
                            if clipboard.set_text(&text).is_ok() {
                                let _ = incoming.try_send((
                                    "CLIPBOARD_APPLIED".into(),
                                    Bytes::from(text.len().to_string()),
                                ));
                                last = text;
                            } else {
                                let _ = incoming.try_send((
                                    "CLIPBOARD_ERROR".into(),
                                    Bytes::from_static("无法写入系统剪贴板".as_bytes()),
                                ));
                            }
                        }
                    }
                    Ok(Command::Paste) => {
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
                    }
                }
            }
        });
        Self { commands: tx }
    }
    pub fn remote(&self, text: String) {
        let _ = self.commands.send(Command::Remote(text));
    }
    pub fn enable(&self, value: bool) {
        let _ = self.commands.send(Command::Enable(value));
    }
    pub fn paste(&self) {
        let _ = self.commands.send(Command::Paste);
    }
}
impl Drop for Clipboard {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Stop);
    }
}

//! Lazy file data objects shared by clipboard copy and desktop OleDrop.
//! Filesystem work stays on a bounded worker, outside the media receive loop.
use super::{pb, session::Output, wire};
use anyhow::{Context, Result, ensure};
use bytes::Bytes;
use pb::{clipboard_request::Which as Req, clipboard_response::Which as Rsp};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::OpenOptions,
    os::unix::fs::{FileExt, MetadataExt, OpenOptionsExt},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

const MAX_ENTRIES: usize = 10000;
const BLOCK: usize = 65536;
struct Entry {
    path: PathBuf,
    description: pb::ClipboardFileDescriptor,
    identity: (u64, u64, i64, i64),
    ranges: BTreeMap<u64, u64>,
}
impl Entry {
    fn record(&mut self, start: u64, end: u64) {
        if start == end {
            return;
        }
        let mut start = start;
        let mut end = end;
        let overlaps: Vec<_> = self
            .ranges
            .range(..=end)
            .filter(|(_, b)| **b >= start)
            .map(|(a, b)| (*a, *b))
            .collect();
        for (a, b) in overlaps {
            self.ranges.remove(&a);
            start = start.min(a);
            end = end.max(b);
        }
        self.ranges.insert(start, end);
    }
    fn bytes(&self) -> u64 {
        self.ranges.iter().map(|(a, b)| b - a).sum()
    }
    fn read(&self, offset: u64, len: usize) -> Result<Vec<u8>> {
        ensure!(
            self.description.file_attributes & 16 == 0,
            "不能读取目录内容"
        );
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&self.path)?;
        let meta = file.metadata()?;
        ensure!(
            meta.is_file()
                && (meta.dev(), meta.ino(), meta.mtime(), meta.mtime_nsec()) == self.identity
                && meta.len() == self.description.file_size,
            "文件已改变，请重新复制或拖放"
        );
        ensure!(offset <= meta.len(), "读取偏移超出文件范围");
        let mut bytes = vec![0; len.min(BLOCK).min((meta.len() - offset) as usize)];
        file.read_exact_at(&mut bytes, offset)?;
        Ok(bytes)
    }
}
fn manifest(paths: &[PathBuf], cancelled: &AtomicBool) -> Result<Vec<Entry>> {
    ensure!(
        !paths.is_empty() && paths.len() <= 1000,
        "请选择 1–1000 个文件或目录"
    );
    let mut entries = Vec::new();
    let mut names = BTreeSet::new();
    let mut queue = Vec::new();
    for path in paths {
        ensure!(path.is_absolute(), "文件路径必须为绝对路径");
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .context("文件名须为有效 Unicode")?;
        super::files::safe_name(name)?;
        ensure!(names.insert(name.to_lowercase()), "所选文件存在同名项");
        queue.push((path.clone(), name.to_owned(), 0));
    }
    while let Some((path, name, depth)) = queue.pop() {
        ensure!(!cancelled.load(Ordering::Relaxed), "已取消");
        ensure!(
            entries.len() < MAX_ENTRIES && depth < 64 && name.len() < 4096,
            "文件清单过大或目录层级过深"
        );
        let meta = std::fs::symlink_metadata(&path)?;
        ensure!(meta.is_file() || meta.is_dir(), "不发送符号链接或特殊文件");
        if meta.is_dir() {
            for child in std::fs::read_dir(&path)? {
                let child = child?;
                let child_name = child
                    .file_name()
                    .into_string()
                    .map_err(|_| anyhow::anyhow!("文件名须为有效 Unicode"))?;
                super::files::safe_name(&child_name)?;
                ensure!(
                    entries.len() + queue.len() < MAX_ENTRIES,
                    "文件清单超过 10000 项"
                );
                queue.push((child.path(), format!("{name}\\{child_name}"), depth + 1));
            }
        }
        entries.push(Entry {
            path,
            description: pb::ClipboardFileDescriptor {
                file_name: name,
                file_attributes: if meta.is_dir() { 16 } else { 128 },
                file_size: if meta.is_file() { meta.len() } else { 0 },
                last_write_time: (meta.mtime().saturating_add(11644473600).max(0) as u64)
                    .saturating_mul(10_000_000)
                    .saturating_add(meta.mtime_nsec().max(0) as u64 / 100),
            },
            identity: (meta.dev(), meta.ino(), meta.mtime(), meta.mtime_nsec()),
            ranges: BTreeMap::new(),
        });
    }
    entries.sort_by(|a, b| a.description.file_name.cmp(&b.description.file_name));
    Ok(entries)
}
enum Command {
    Request(i64, pb::ClipboardRequest),
    Response(i64, pb::ClipboardResponse),
    Pause(bool),
}
pub struct FileOffer {
    tx: mpsc::SyncSender<Command>,
    cancelled: Arc<AtomicBool>,
    pub id: i32,
    released: Arc<AtomicBool>,
    failure: Arc<std::sync::Mutex<Option<String>>>,
    pub geometry: Option<crate::media_bus::Geometry>,
    pub input_pending: bool,
    pub paste: bool,
    finished: Arc<AtomicBool>,
}
impl Drop for FileOffer {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }
}
impl FileOffer {
    pub fn start(
        command: &Value,
        output: Output,
        incoming: tokio::sync::mpsc::Sender<(String, Bytes)>,
    ) -> Result<Self> {
        let paths = command["paths"]
            .as_array()
            .context("缺少 paths")?
            .iter()
            .map(|v| v.as_str().map(PathBuf::from).context("文件路径无效"))
            .collect::<Result<Vec<_>>>()?;
        ensure!(
            !paths.is_empty() && paths.len() <= 1000,
            "请选择 1–1000 个文件或目录"
        );
        let id = command["queued_id"].as_i64().context("缺少文件任务编号")?;
        ensure!(id < 0 && id > -(0x4000_0000i64), "文件任务编号无效");
        let id = id as i32;
        let action = if command["drop"].is_object() {
            let d = &command["drop"];
            let x = d["x"].as_f64().context("拖放横坐标无效")?;
            let y = d["y"].as_f64().context("拖放纵坐标无效")?;
            ensure!(
                x.is_finite()
                    && y.is_finite()
                    && (0.0..=1.0).contains(&x)
                    && (0.0..=1.0).contains(&y),
                "拖放坐标无效"
            );
            Some(pb::clipboard_format_list_request::DragDropAction::OleDrop(
                pb::DragDropOleDrop {
                    target_x: x,
                    target_y: y,
                    screen_id: i32::try_from(d["screen_id"].as_i64().context("屏幕编号无效")?)?,
                },
            ))
        } else {
            None
        };
        let cancelled = Arc::new(AtomicBool::new(false));
        let released = Arc::new(AtomicBool::new(false));
        let worker_released = released.clone();
        let finished = Arc::new(AtomicBool::new(false));
        let worker_finished = finished.clone();
        let failure = Arc::new(std::sync::Mutex::new(None));
        let worker_failure = failure.clone();
        let geometry = command
            .get("geometry")
            .cloned()
            .map(serde_json::from_value)
            .transpose()?;
        let (tx, rx) = mpsc::sync_channel(32);
        let stop = cancelled.clone();
        let client_request = command["client_request"].clone();
        let paste = command["paste"] == true;
        let action_is_input = action.is_some() || paste;
        std::thread::Builder::new()
            .name("uux-file-offer".into())
            .spawn(move || {
                let mut worker = Worker {
                    id,
                    output,
                    incoming,
                    cancelled: stop,
                    released: worker_released,
                    failure: worker_failure,
                    task: None,
                    entries: vec![],
                    started: Instant::now(),
                    last: Instant::now(),
                    paused: false,
                    held: vec![],
                    client_request,
                    paste,
                };
                if let Err(error) = worker.run(paths, action, rx) {
                    worker.progress("失败", Some(&error.to_string()));
                }
                worker_finished.store(true, Ordering::Release);
            })?;
        Ok(Self {
            tx,
            cancelled,
            id,
            released,
            failure,
            geometry,
            input_pending: action_is_input,
            finished,
            paste,
        })
    }
    pub fn active(&self) -> bool {
        !self.cancelled.load(Ordering::Relaxed) && !self.finished.load(Ordering::Acquire)
    }
    pub fn finished(&self) -> bool {
        self.finished.load(Ordering::Acquire)
    }
    pub fn reject(&self, message: &str) {
        *self.failure.lock().unwrap() = Some(message.to_owned());
        self.cancelled.store(true, Ordering::Relaxed);
    }
    pub fn remote_replaced(&self) {
        self.released.store(true, Ordering::Relaxed);
    }
    pub fn request(&self, id: i64, request: pb::ClipboardRequest) -> Result<bool> {
        if !matches!(
            request.which,
            Some(
                Req::FileDescListRequest(_)
                    | Req::FileContentsRequest(_)
                    | Req::CancelRequest(_)
                    | Req::AutoSaveComplete(_)
            )
        ) {
            return Ok(false);
        }
        self.tx
            .try_send(Command::Request(id, request))
            .context("文件发送队列已满或任务已结束")?;
        Ok(true)
    }
    pub fn response(&self, id: i64, response: pb::ClipboardResponse) {
        if matches!(
            response.which,
            Some(Rsp::FormatListResponse(_) | Rsp::DescSegmentConfirm(_))
        ) {
            let _ = self.tx.try_send(Command::Response(id, response));
        }
    }
    pub fn command(&self, command: &Value) -> Result<()> {
        if command["id"]
            .as_i64()
            .is_some_and(|id| id != i64::from(self.id))
        {
            return Ok(());
        }
        match command["op"].as_str() {
            Some("transfer_cancel" | "transfers_cancel") => {
                self.cancelled.store(true, Ordering::Relaxed)
            }
            Some("transfer_pause" | "transfers_pause") => self
                .tx
                .try_send(Command::Pause(command["paused"].as_bool().unwrap_or(true)))
                .context("文件发送队列已满")?,
            _ => {}
        }
        Ok(())
    }
}
struct Worker {
    id: i32,
    output: Output,
    incoming: tokio::sync::mpsc::Sender<(String, Bytes)>,
    cancelled: Arc<AtomicBool>,
    released: Arc<AtomicBool>,
    failure: Arc<std::sync::Mutex<Option<String>>>,
    task: Option<u32>,
    entries: Vec<Entry>,
    started: Instant,
    last: Instant,
    paused: bool,
    held: Vec<(i64, pb::ClipboardRequest)>,
    client_request: Value,
    paste: bool,
}
impl Worker {
    fn send(&self, bytes: Vec<u8>) -> Result<()> {
        self.incoming
            .blocking_send((
                format!("CLIPBOARD_FILES_PACKET:{}", self.id),
                Bytes::from(bytes),
            ))
            .context("会话已结束")
    }
    fn reply(&self, id: i64, which: Rsp) -> Result<()> {
        self.send(wire::response(
            id,
            pb::rpc_response::Which::ClipResponse(pb::ClipboardResponse { which: Some(which) }),
        ))
    }
    fn progress(&self, status: &str, error: Option<&str>) {
        let done: u64 = self.entries.iter().map(Entry::bytes).sum();
        let total: u64 = self.entries.iter().map(|e| e.description.file_size).sum();
        self.output.native("transfer", json!({"id":self.id,"name":self.entries.first().map_or("文件剪贴板",|e| e.description.file_name.as_str()),
            "upload":true,"source":"clipboard","local":self.entries.first().map(|e|&e.path),"remote":"远端粘贴或拖放目标", "done":done,"size":total,"size_known":!self.entries.is_empty(),
            "status":status,"error":error,"elapsed_ms":self.started.elapsed().as_millis() as u64,"items":self.entries.len(),"items_finished":self.entries.iter().filter(|e|e.bytes()==e.description.file_size).count(),
            "completion_basis":"content_read_and_remote_released","destination_confirmed":false}));
    }
    fn run(
        &mut self,
        paths: Vec<PathBuf>,
        action: Option<pb::clipboard_format_list_request::DragDropAction>,
        rx: mpsc::Receiver<Command>,
    ) -> Result<()> {
        self.progress("准备中", None);
        self.entries = manifest(&paths, &self.cancelled)?;
        let (offer_id, bytes) =
            wire::rpc(pb::rpc_request::Which::ClipRequest(pb::ClipboardRequest {
                which: Some(Req::FormatList(pb::ClipboardFormatListRequest {
                    formats: vec![
                        pb::ClipboardFormat {
                            id: 0xc001,
                            name: "FileGroupDescriptorW".into(),
                        },
                        pb::ClipboardFormat {
                            id: 0xc002,
                            name: "FileContents".into(),
                        },
                    ],
                    has_action: i32::from(action.is_some()),
                    drag_drop_action: action,
                })),
            }));
        self.incoming
            .blocking_send((
                format!("CLIPBOARD_FILES_OFFER:{}", self.id),
                Bytes::from(bytes),
            ))
            .context("会话已结束")?;
        loop {
            if let Some(message) = self.failure.lock().unwrap().take() {
                anyhow::bail!("{message}");
            }
            if self.cancelled.load(Ordering::Relaxed) {
                if let Some(task_id) = self.task {
                    let _ = self.send(
                        wire::rpc(pb::rpc_request::Which::ClipRequest(pb::ClipboardRequest {
                            which: Some(Req::CancelRequest(pb::ClipboardFileCancelRequest {
                                task_id,
                            })),
                        }))
                        .1,
                    );
                }
                self.progress(
                    if self.released.load(Ordering::Relaxed)
                        && self.task.is_some()
                        && self
                            .entries
                            .iter()
                            .all(|e| e.bytes() == e.description.file_size)
                    {
                        "已完成"
                    } else {
                        "已取消"
                    },
                    None,
                );
                return Ok(());
            }
            ensure!(
                self.paused
                    || self.last.elapsed()
                        < Duration::from_secs(if self.task.is_some() { 60 } else { 300 }),
                "等待远端读取文件超时"
            );
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(Command::Response(id, r)) => {
                    if id == offer_id && matches!(r.which, Some(Rsp::FormatListResponse(_))) {
                        self.output.native("clipboard_files_offered",json!({"client_request":self.client_request,"id":self.id,"state":"offered","items":self.entries.len()}));
                        self.progress("等待粘贴", None);
                        if self.paste {
                            self.paste = false;
                            self.incoming
                                .blocking_send((
                                    format!("CLIPBOARD_FILES_PASTE:{}", self.id),
                                    Bytes::new(),
                                ))
                                .context("会话已结束")?;
                        }
                    }
                    if let Some(Rsp::DescSegmentConfirm(r)) = r.which {
                        ensure!(r.err == 1, "远端拒绝文件清单");
                    }
                }
                Ok(Command::Request(id, r)) => {
                    self.last = Instant::now();
                    if self.paused && matches!(r.which, Some(Req::FileContentsRequest(_))) {
                        ensure!(self.held.len() < 16, "暂停期间远端读取队列超过限制");
                        self.held.push((id, r));
                    } else if self.request(id, r)? {
                        return Ok(());
                    }
                }
                Ok(Command::Pause(paused)) => {
                    self.paused = paused;
                    self.last = Instant::now();
                    self.progress(if paused { "已暂停" } else { "传输中" }, None);
                    if !paused {
                        for (id, r) in std::mem::take(&mut self.held) {
                            if self.request(id, r)? {
                                return Ok(());
                            }
                        }
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    self.progress(
                        if self.released.load(Ordering::Relaxed)
                            && self.task.is_some()
                            && self
                                .entries
                                .iter()
                                .all(|e| e.bytes() == e.description.file_size)
                        {
                            "已完成"
                        } else {
                            "已取消"
                        },
                        None,
                    );
                    return Ok(());
                }
            }
        }
    }
    fn request(&mut self, id: i64, request: pb::ClipboardRequest) -> Result<bool> {
        match request.which {
            Some(Req::FileDescListRequest(r)) => {
                ensure!(
                    self.task.is_none_or(|t| t == r.task_id),
                    "文件清单正在被另一个任务读取"
                );
                self.task = Some(r.task_id);
                self.reply(
                    id,
                    Rsp::FileDescListResponse(pb::ClipboardFileDescriptorListResponse {
                        task_id: r.task_id,
                        segment_count: self.entries.len().div_ceil(128) as u32,
                        err: 1,
                    }),
                )?;
                for (i, chunk) in self.entries.chunks(128).enumerate() {
                    self.send(
                        wire::rpc(pb::rpc_request::Which::ClipRequest(pb::ClipboardRequest {
                            which: Some(Req::DescSegment(pb::ClipboardFileDescriptorSegment {
                                task_id: r.task_id,
                                segment_id: i as u32 + 1,
                                file_descs: chunk.iter().map(|e| e.description.clone()).collect(),
                            })),
                        }))
                        .1,
                    )?;
                }
                self.progress("传输中", None);
            }
            Some(Req::FileContentsRequest(r)) => {
                let data = (|| -> Result<Vec<u8>> {
                    ensure!(self.task == Some(r.task_id), "文件任务编号不匹配");
                    let entry = self
                        .entries
                        .get_mut(r.list_index as usize)
                        .context("文件索引无效")?;
                    if r.flags == 1 {
                        return Ok(entry.description.file_size.to_le_bytes().to_vec());
                    }
                    ensure!(r.flags == 2 && r.requested_len > 0, "文件读取标记无效");
                    let data = entry.read(r.pos_offset, r.requested_len as usize)?;
                    ensure!(entry.ranges.len() < 4096, "文件随机读取范围过多");
                    entry.record(r.pos_offset, r.pos_offset + data.len() as u64);
                    Ok(data)
                })();
                self.reply(
                    id,
                    Rsp::FileContentsResponse(pb::ClipboardFileContentsResponse {
                        task_id: r.task_id,
                        data: data.as_ref().cloned().unwrap_or_default(),
                        err: if data.is_ok() { 1 } else { 2 },
                        pos_offset: r.pos_offset,
                        list_index: r.list_index,
                    }),
                )?;
                data?;
                self.progress("传输中", None);
            }
            Some(Req::CancelRequest(r)) if self.task == Some(r.task_id) => {
                self.reply(
                    id,
                    Rsp::CancelResponse(pb::ClipboardFileCancelResponse {
                        task_id: r.task_id,
                        err: 1,
                    }),
                )?;
                self.progress(
                    if self
                        .entries
                        .iter()
                        .all(|e| e.bytes() == e.description.file_size)
                    {
                        "已完成"
                    } else {
                        "已取消"
                    },
                    None,
                );
                return Ok(true);
            }
            Some(Req::AutoSaveComplete(r)) => {
                self.reply(
                    id,
                    Rsp::AutoSaveCompleteResponse(pb::DragDropAutoSaveCompleteResponse { err: 1 }),
                )?;
                ensure!(
                    r.total_count as usize == self.entries.len()
                        && r.success_count == r.total_count,
                    "远端未保存全部文件"
                );
                self.progress("已完成", None);
                return Ok(true);
            }
            _ => {}
        }
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn selected_manifest_bounds_reads_and_rejects_replacements() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("测试.txt");
        std::fs::write(&p, b"abcdef").unwrap();
        let mut rows = manifest(std::slice::from_ref(&p), &AtomicBool::new(false)).unwrap();
        let row = &mut rows[0];
        assert_eq!(row.read(2, 4).unwrap(), b"cdef");
        assert!(row.read(7, 1).is_err());
        row.record(2, 4);
        row.record(0, 3);
        row.record(1, 5);
        assert_eq!(row.bytes(), 5);
        std::fs::rename(&p, d.path().join("old")).unwrap();
        std::fs::write(&p, b"secret").unwrap();
        assert!(row.read(0, 6).is_err());
        let link = d.path().join("link");
        std::os::unix::fs::symlink(&p, &link).unwrap();
        assert!(manifest(&[link], &AtomicBool::new(false)).is_err());
    }
}

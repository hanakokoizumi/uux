//! UU's remote drop overlay advertises a clipboard file list with AutoSave.
//! Pull file contents serially; all paths stay inside a private staging directory.
use super::{files::safe_name, pb, session::Output, wire};
use anyhow::{Context, Result, ensure};
use pb::{clipboard_request::Which as Req, clipboard_response::Which as Rsp};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU32, Ordering},
    time::{Duration, Instant},
};

const BLOCK: u32 = 65536;
const MAX_ENTRIES: usize = 10000;
static TASK: AtomicU32 = AtomicU32::new(0x4000_0000);

fn request(value: Req) -> (i64, Vec<u8>) {
    wire::rpc(pb::rpc_request::Which::ClipRequest(pb::ClipboardRequest {
        which: Some(value),
    }))
}
fn response(id: i64, value: Rsp) -> Vec<u8> {
    wire::response(
        id,
        pb::rpc_response::Which::ClipResponse(pb::ClipboardResponse { which: Some(value) }),
    )
}
pub fn is_drag_request(r: &pb::ClipboardRequest) -> bool {
    matches!(&r.which, Some(Req::FormatList(f)) if f.has_action != 0 && matches!(f.drag_drop_action, Some(pb::clipboard_format_list_request::DragDropAction::AutoSave(_))))
        || matches!(r.which, Some(Req::DescSegment(_) | Req::CancelRequest(_)))
}
fn relative_path(name: &str) -> Result<PathBuf> {
    ensure!(name.len() <= 4096, "文件路径过长");
    let mut path = PathBuf::new();
    let parts: Vec<_> = name.split(['/', '\\']).collect();
    ensure!(parts.len() <= 64, "文件目录层级过深");
    for part in parts {
        path.push(safe_name(part)?);
    }
    Ok(path)
}

struct Batch {
    task: u32,
    list_rpc: i64,
    pending_rpc: Option<i64>,
    count: Option<u32>,
    segments: BTreeMap<u32, Vec<pb::ClipboardFileDescriptor>>,
    entries: Vec<pb::ClipboardFileDescriptor>,
    ready: bool,
    root: PathBuf,
    staging: tempfile::TempDir,
    published: Vec<PathBuf>,
    saved_count: u32,
    index: usize,
    file: Option<File>,
    offset: u64,
    total: u64,
    done: u64,
    requested: u32,
    paused: bool,
    started: Instant,
    last: Instant,
}
impl Batch {
    fn ui_id(&self) -> i64 {
        -(self.task as i64)
    }
    fn name(&self) -> &str {
        self.entries
            .first()
            .map_or("拖拽接收", |f| f.file_name.as_str())
    }
    fn progress(&self, out: &Output, status: &str, error: Option<&str>) {
        out.native("transfer", json!({"id":self.ui_id(),"name":self.name(),"upload":false,
            "local":self.root,"remote":"被控端拖拽发送","size":self.total,"done":self.done,
            "size_known":self.ready,"initial_offset":0,"elapsed_ms":self.started.elapsed().as_millis() as u64,
            "status":status,"error":error,"source":"drag","items":self.entries.len()}));
    }
    fn manifest(&mut self) -> Result<()> {
        let Some(count) = self.count else {
            return Ok(());
        };
        if self.ready || self.segments.len() != count as usize {
            return Ok(());
        }
        ensure!(
            (1..=count).all(|n| self.segments.contains_key(&n)),
            "文件清单分段不连续"
        );
        let entries: Vec<_> = std::mem::take(&mut self.segments)
            .into_values()
            .flatten()
            .collect();
        ensure!(
            !entries.is_empty() && entries.len() <= MAX_ENTRIES,
            "文件清单数量无效"
        );
        let mut paths = BTreeSet::new();
        let mut files = BTreeSet::new();
        let mut total = 0u64;
        for entry in &entries {
            let path = relative_path(&entry.file_name)?;
            ensure!(paths.insert(path.clone()), "文件清单含重复路径");
            ensure!(
                entry.file_attributes & 0x400 == 0,
                "不接收符号链接或重解析点"
            );
            if entry.file_attributes & 0x10 == 0 {
                files.insert(path);
                total = total.checked_add(entry.file_size).context("文件大小溢出")?;
            }
        }
        for path in &paths {
            ensure!(
                !path
                    .ancestors()
                    .skip(1)
                    .any(|parent| files.contains(parent)),
                "文件与目录路径冲突"
            );
        }
        self.entries = entries;
        self.total = total;
        self.ready = true;
        Ok(())
    }
    fn next(&mut self) -> Result<Option<Vec<u8>>> {
        if !self.ready || self.paused || self.pending_rpc.is_some() {
            return Ok(None);
        }
        while let Some(entry) = self.entries.get(self.index) {
            let path = self.staging.path().join(relative_path(&entry.file_name)?);
            if entry.file_attributes & 0x10 != 0 {
                std::fs::create_dir_all(&path)?;
                self.index += 1;
                continue;
            }
            if self.file.is_none() {
                std::fs::create_dir_all(path.parent().context("文件目录无效")?)?;
                self.file = Some(
                    OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(&path)?,
                );
            }
            if self.offset == entry.file_size {
                let file = self.file.take().unwrap();
                file.sync_all()?;
                self.index += 1;
                self.offset = 0;
                continue;
            }
            self.requested = (entry.file_size - self.offset).min(BLOCK as u64) as u32;
            let (id, message) =
                request(Req::FileContentsRequest(pb::ClipboardFileContentsRequest {
                    task_id: self.task,
                    list_index: self.index as u32,
                    flags: 2,
                    pos_offset: self.offset,
                    requested_len: self.requested,
                }));
            self.pending_rpc = Some(id);
            self.last = Instant::now();
            return Ok(Some(message));
        }
        Ok(None)
    }
    fn commit(&mut self) -> Result<Vec<PathBuf>> {
        // Each top-level item is published without replacing any existing item.
        // All nested files were created in our private, same-filesystem directory.
        let roots: BTreeSet<_> = self
            .entries
            .iter()
            .map(|e| {
                relative_path(&e.file_name)
                    .unwrap()
                    .components()
                    .next()
                    .unwrap()
                    .as_os_str()
                    .to_owned()
            })
            .collect();
        let mut saved = vec![];
        for name in roots {
            let source = self.staging.path().join(&name);
            let stem = Path::new(&name).file_stem().unwrap().to_string_lossy();
            let ext = Path::new(&name)
                .extension()
                .map(|s| format!(".{}", s.to_string_lossy()))
                .unwrap_or_default();
            let mut moved = false;
            for n in 0..10000 {
                let target = self.root.join(if n == 0 {
                    name.clone()
                } else {
                    format!("{stem} ({n}){ext}").into()
                });
                match rename_no_replace(&source, &target) {
                    Ok(()) => {
                        self.saved_count += self
                            .entries
                            .iter()
                            .filter(|e| {
                                relative_path(&e.file_name)
                                    .is_ok_and(|p| p.starts_with(Path::new(&name)))
                            })
                            .count() as u32;
                        self.published.push(target.clone());
                        saved.push(target);
                        moved = true;
                        break;
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(e) => return Err(e.into()),
                }
            }
            ensure!(moved, "同名文件过多，无法保存");
        }
        Ok(saved)
    }
}
fn rename_no_replace(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let from = std::ffi::CString::new(from.as_os_str().as_bytes())?;
    let to = std::ffi::CString::new(to.as_os_str().as_bytes())?;
    // Linux renameat2 atomically rejects files, directories and symlinks already
    // at the destination; a prior exists() check would leave a replacement race.
    let result = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            from.as_ptr(),
            libc::AT_FDCWD,
            to.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[derive(Default)]
pub struct DragFiles {
    batch: Option<Batch>,
}

impl DragFiles {
    fn abort(&mut self, out: &Output, status: &str, error: Option<&str>) -> Vec<Vec<u8>> {
        let Some(batch) = self.batch.take() else {
            return vec![];
        };
        batch.progress(out, status, error);
        if !batch.published.is_empty() {
            out.native(
                "drag_files_saved",
                json!({"count":batch.saved_count,"directory":batch.root,"paths":batch.published}),
            );
        }
        vec![
            request(Req::AutoSaveComplete(pb::DragDropAutoSaveComplete {
                total_count: batch.entries.len() as u32,
                success_count: batch.saved_count,
                first_file_name: String::new(),
            }))
            .1,
            request(Req::CancelRequest(pb::ClipboardFileCancelRequest {
                task_id: batch.task,
            }))
            .1,
        ]
    }
    fn advance(&mut self, out: &Output) -> Result<Vec<Vec<u8>>> {
        let Some(batch) = self.batch.as_mut() else {
            return Ok(vec![]);
        };
        batch.manifest()?;
        let next = batch.next()?;
        batch.progress(
            out,
            if batch.paused {
                "已暂停"
            } else if batch.ready {
                "传输中"
            } else {
                "准备中"
            },
            None,
        );
        if batch.ready && batch.index == batch.entries.len() {
            let saved = batch.commit()?;
            batch.progress(out, "已完成", None);
            out.native(
                "drag_files_saved",
                json!({"count":batch.entries.len(),"directory":batch.root,"paths":saved}),
            );
            let messages = vec![
                request(Req::AutoSaveComplete(pb::DragDropAutoSaveComplete {
                    total_count: batch.entries.len() as u32,
                    success_count: batch.entries.len() as u32,
                    first_file_name: batch.name().into(),
                }))
                .1,
                request(Req::CancelRequest(pb::ClipboardFileCancelRequest {
                    task_id: batch.task,
                }))
                .1,
            ];
            self.batch = None;
            return Ok(messages);
        }
        Ok(next.into_iter().collect())
    }
    pub fn request(
        &mut self,
        id: i64,
        r: pb::ClipboardRequest,
        allowed: bool,
        destination: &Path,
        out: &Output,
    ) -> Vec<Vec<u8>> {
        let mut messages = vec![];
        let result = (|| -> Result<()> {
            match r.which {
                Some(Req::FormatList(_)) => {
                    messages.push(response(
                        id,
                        Rsp::FormatListResponse(pb::ClipboardFormatListResponse {}),
                    ));
                    messages.extend(self.abort(
                        out,
                        "已取消",
                        Some("新的拖拽发送替换了上一份文件清单"),
                    ));
                    ensure!(allowed, "当前设置未允许接收拖拽文件");
                    ensure!(
                        destination.is_absolute() && destination.is_dir(),
                        "请在设置中选择有效的接收文件目录"
                    );
                    let task = TASK.fetch_add(1, Ordering::Relaxed);
                    let (rpc, message) = request(Req::FileDescListRequest(
                        pb::ClipboardFileDescriptorListRequest { task_id: task },
                    ));
                    self.batch = Some(Batch {
                        task,
                        list_rpc: rpc,
                        pending_rpc: None,
                        count: None,
                        segments: Default::default(),
                        entries: vec![],
                        ready: false,
                        root: destination.canonicalize()?,
                        staging: tempfile::Builder::new()
                            .prefix(".uu-drag-")
                            .tempdir_in(destination)?,
                        published: vec![],
                        saved_count: 0,
                        index: 0,
                        file: None,
                        offset: 0,
                        total: 0,
                        done: 0,
                        requested: 0,
                        paused: false,
                        started: Instant::now(),
                        last: Instant::now(),
                    });
                    messages.push(message);
                    out.native("drag_files_started", json!({"directory":destination}));
                }
                Some(Req::DescSegment(segment)) => {
                    let valid = self
                        .batch
                        .as_ref()
                        .is_some_and(|b| b.task == segment.task_id);
                    messages.push(response(
                        id,
                        Rsp::DescSegmentConfirm(pb::ClipboardFileDescriptorSegmentConfirm {
                            task_id: segment.task_id,
                            segment_id: segment.segment_id,
                            err: if valid { 1 } else { 2 },
                        }),
                    ));
                    if !valid {
                        return Ok(());
                    }
                    let b = self.batch.as_mut().unwrap();
                    ensure!(
                        !b.ready && (1..=128).contains(&segment.segment_id),
                        "文件清单分段编号无效"
                    );
                    if let Some(old) = b.segments.get(&segment.segment_id) {
                        ensure!(*old == segment.file_descs, "重复的文件清单不一致");
                    } else {
                        ensure!(
                            b.segments.values().map(Vec::len).sum::<usize>()
                                + segment.file_descs.len()
                                <= MAX_ENTRIES,
                            "文件数量超过限制"
                        );
                        b.segments.insert(segment.segment_id, segment.file_descs);
                    }
                    b.last = Instant::now();
                }
                Some(Req::CancelRequest(c)) => {
                    if self.batch.as_ref().is_some_and(|b| b.task == c.task_id) {
                        messages.extend(self.abort(out, "已取消", None));
                    }
                    messages.push(response(
                        id,
                        Rsp::CancelResponse(pb::ClipboardFileCancelResponse {
                            task_id: c.task_id,
                            err: 1,
                        }),
                    ));
                }
                _ => {}
            }
            messages.extend(self.advance(out)?);
            Ok(())
        })();
        if let Err(error) = result {
            out.native("drag_files_error", json!({"message":error.to_string()}));
            if self.batch.is_none() {
                messages.push(
                    request(Req::AutoSaveComplete(
                        pb::DragDropAutoSaveComplete::default(),
                    ))
                    .1,
                );
            } else {
                messages.extend(self.abort(out, "失败", Some(&error.to_string())));
            }
        }
        messages
    }
    pub fn response(&mut self, id: i64, r: pb::ClipboardResponse, out: &Output) -> Vec<Vec<u8>> {
        let result = (|| -> Result<Vec<Vec<u8>>> {
            let Some(b) = self.batch.as_mut() else {
                return Ok(vec![]);
            };
            match r.which {
                Some(Rsp::FileDescListResponse(r)) if id == b.list_rpc && r.task_id == b.task => {
                    ensure!(
                        r.err == 1 && (1..=128).contains(&r.segment_count),
                        "远端文件清单不可用"
                    );
                    b.count = Some(r.segment_count);
                    b.last = Instant::now();
                }
                Some(Rsp::FileContentsResponse(r))
                    if Some(id) == b.pending_rpc && r.task_id == b.task =>
                {
                    ensure!(r.err == 1, "远端读取文件失败");
                    // Official 4.33 leaves these optional echo fields at zero.
                    // RPC ID + task ID + a single outstanding bounded request
                    // still correlate the exact file and offset without guessing.
                    ensure!(
                        (r.pos_offset == 0 && r.list_index == 0)
                            || (r.pos_offset == b.offset && r.list_index == b.index as u32),
                        "文件分块位置不匹配"
                    );
                    ensure!(
                        !r.data.is_empty() && r.data.len() <= b.requested as usize,
                        "文件分块长度无效"
                    );
                    b.file
                        .as_mut()
                        .context("接收文件未打开")?
                        .write_all(&r.data)?;
                    b.offset += r.data.len() as u64;
                    b.done += r.data.len() as u64;
                    b.pending_rpc = None;
                    b.last = Instant::now();
                }
                _ => return Ok(vec![]),
            }
            self.advance(out)
        })();
        match result {
            Ok(messages) => messages,
            Err(e) => {
                out.native("drag_files_error", json!({"message":e.to_string()}));
                self.abort(out, "失败", Some(&e.to_string()))
            }
        }
    }
    pub fn command(&mut self, command: &Value, out: &Output) -> Result<Vec<Vec<u8>>> {
        let Some(b) = self.batch.as_mut() else {
            return Ok(vec![]);
        };
        let op = command["op"].as_str().unwrap_or("");
        if command["id"].as_i64().is_some_and(|id| id != b.ui_id()) {
            return Ok(vec![]);
        }
        if matches!(op, "transfer_cancel" | "transfers_cancel") {
            return Ok(self.abort(out, "已取消", None));
        }
        if matches!(op, "transfer_pause" | "transfers_pause") {
            b.paused = command["paused"].as_bool().unwrap_or(true);
            b.last = Instant::now();
            return Ok(match self.advance(out) {
                Ok(messages) => messages,
                Err(e) => {
                    out.native("drag_files_error", json!({"message":e.to_string()}));
                    self.abort(out, "失败", Some(&e.to_string()))
                }
            });
        }
        Ok(vec![])
    }
    pub fn tick(&mut self, out: &Output) -> Vec<Vec<u8>> {
        if self
            .batch
            .as_ref()
            .is_some_and(|b| !b.paused && b.last.elapsed() > Duration::from_secs(30))
        {
            self.abort(out, "失败", Some("拖拽文件接收超时"))
        } else {
            vec![]
        }
    }
    pub fn cancel(&mut self, out: &Output) -> Vec<Vec<u8>> {
        self.abort(out, "已取消", None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex, mpsc};
    fn output() -> Output {
        Output {
            generation: Some(1),
            events: mpsc::channel().0,
            latest: Arc::new(Mutex::new(None)),
            ctx: Default::default(),
        }
    }
    fn offer() -> pb::ClipboardRequest {
        pb::ClipboardRequest {
            which: Some(Req::FormatList(pb::ClipboardFormatListRequest {
                formats: vec![pb::ClipboardFormat {
                    id: 49161,
                    name: "FileGroupDescriptorW".into(),
                }],
                has_action: 1,
                drag_drop_action: Some(
                    pb::clipboard_format_list_request::DragDropAction::AutoSave(
                        pb::DragDropAutoSave {
                            dest_path: "/remote/must-not-select-destination".into(),
                        },
                    ),
                ),
            })),
        }
    }
    fn descriptor(name: &str, size: u64, directory: bool) -> pb::ClipboardFileDescriptor {
        pb::ClipboardFileDescriptor {
            file_name: name.into(),
            file_size: size,
            file_attributes: if directory { 16 } else { 0 },
            ..Default::default()
        }
    }
    fn manifest(
        receiver: &mut DragFiles,
        out: &Output,
        entries: Vec<pb::ClipboardFileDescriptor>,
    ) -> Vec<Vec<u8>> {
        let b = receiver.batch.as_ref().unwrap();
        let (task, id) = (b.task, b.list_rpc);
        receiver.response(
            id,
            pb::ClipboardResponse {
                which: Some(Rsp::FileDescListResponse(
                    pb::ClipboardFileDescriptorListResponse {
                        task_id: task,
                        segment_count: 1,
                        err: 1,
                    },
                )),
            },
            out,
        );
        receiver.request(
            10,
            pb::ClipboardRequest {
                which: Some(Req::DescSegment(pb::ClipboardFileDescriptorSegment {
                    task_id: task,
                    segment_id: 1,
                    file_descs: entries,
                })),
            },
            true,
            Path::new("/ignored"),
            out,
        )
    }
    fn block(
        receiver: &mut DragFiles,
        out: &Output,
        data: Vec<u8>,
        rpc: Option<i64>,
    ) -> Vec<Vec<u8>> {
        let b = receiver.batch.as_ref().unwrap();
        let (task, id) = (b.task, rpc.or(b.pending_rpc).unwrap());
        receiver.response(
            id,
            pb::ClipboardResponse {
                which: Some(Rsp::FileContentsResponse(
                    pb::ClipboardFileContentsResponse {
                        task_id: task,
                        data,
                        err: 1,
                        ..Default::default()
                    },
                )),
            },
            out,
        )
    }
    #[test]
    fn folder_multi_file_legacy_blocks_and_collision_preserve_existing_files() {
        let dir = tempfile::tempdir().unwrap();
        let out = output();
        let mut r = DragFiles::default();
        std::fs::write(dir.path().join("你好.txt"), b"keep").unwrap();
        r.request(1, offer(), true, dir.path(), &out);
        manifest(
            &mut r,
            &out,
            vec![
                descriptor("目录", 0, true),
                descriptor("目录\\子.txt", 3, false),
                descriptor("你好.txt", 2, false),
                descriptor("空文件", 0, false),
            ],
        );
        let id = r.batch.as_ref().unwrap().pending_rpc.unwrap();
        block(&mut r, &out, b"bad".to_vec(), Some(id + 1000));
        assert_eq!(r.batch.as_ref().unwrap().done, 0);
        block(&mut r, &out, b"abc".to_vec(), None);
        let result = block(&mut r, &out, b"ok".to_vec(), None);
        assert!(r.batch.is_none());
        assert_eq!(result.len(), 2);
        assert_eq!(
            std::fs::read(dir.path().join("目录/子.txt")).unwrap(),
            b"abc"
        );
        assert_eq!(std::fs::read(dir.path().join("你好.txt")).unwrap(), b"keep");
        assert_eq!(
            std::fs::read(dir.path().join("你好 (1).txt")).unwrap(),
            b"ok"
        );
        assert_eq!(
            std::fs::metadata(dir.path().join("空文件")).unwrap().len(),
            0
        );
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 4);
    }
    #[test]
    fn pause_keeps_inflight_data_but_delays_next_request_and_cancel_cleans_staging() {
        let dir = tempfile::tempdir().unwrap();
        let out = output();
        let mut r = DragFiles::default();
        r.request(1, offer(), true, dir.path(), &out);
        manifest(
            &mut r,
            &out,
            vec![descriptor("data", BLOCK as u64 + 1, false)],
        );
        r.command(&json!({"op":"transfers_pause","paused":true}), &out)
            .unwrap();
        assert!(block(&mut r, &out, vec![9; BLOCK as usize], None).is_empty());
        assert!(r.batch.as_ref().unwrap().pending_rpc.is_none());
        assert_eq!(
            r.command(&json!({"op":"transfers_pause","paused":false}), &out)
                .unwrap()
                .len(),
            1
        );
        r.cancel(&out);
        assert!(r.batch.is_none());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }
    #[test]
    fn unsafe_manifest_and_unsolicited_or_oversized_data_are_rejected() {
        for name in ["../escape", "C:\\escape", "/abs", "a/../b", "a//b"] {
            let dir = tempfile::tempdir().unwrap();
            let out = output();
            let mut r = DragFiles::default();
            r.request(1, offer(), true, dir.path(), &out);
            manifest(&mut r, &out, vec![descriptor(name, 4, false)]);
            assert!(r.batch.is_none());
            assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
        }
        let dir = tempfile::tempdir().unwrap();
        let out = output();
        let mut r = DragFiles::default();
        r.request(1, offer(), false, dir.path(), &out);
        assert!(r.batch.is_none());
        r.request(2, offer(), true, dir.path(), &out);
        manifest(&mut r, &out, vec![descriptor("small", 1, false)]);
        block(&mut r, &out, vec![1, 2], None);
        assert!(r.batch.is_none());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }
    #[test]
    fn reordered_manifest_segments_keep_original_indices_and_ignore_duplicate_data() {
        let dir = tempfile::tempdir().unwrap();
        let out = output();
        let mut r = DragFiles::default();
        r.request(1, offer(), true, dir.path(), &out);
        let b = r.batch.as_ref().unwrap();
        let (task, id) = (b.task, b.list_rpc);
        for segment in [2, 1] {
            r.request(
                12 + segment as i64,
                pb::ClipboardRequest {
                    which: Some(Req::DescSegment(pb::ClipboardFileDescriptorSegment {
                        task_id: task,
                        segment_id: segment,
                        file_descs: vec![descriptor(&format!("{segment}"), 1, false)],
                    })),
                },
                true,
                dir.path(),
                &out,
            );
        }
        r.response(
            id,
            pb::ClipboardResponse {
                which: Some(Rsp::FileDescListResponse(
                    pb::ClipboardFileDescriptorListResponse {
                        task_id: task,
                        segment_count: 2,
                        err: 1,
                    },
                )),
            },
            &out,
        );
        let first = r.batch.as_ref().unwrap().pending_rpc;
        block(&mut r, &out, vec![1], None);
        block(&mut r, &out, vec![1], first);
        assert_eq!(r.batch.as_ref().unwrap().done, 1);
        block(&mut r, &out, vec![2], None);
        assert_eq!(std::fs::read(dir.path().join("2")).unwrap(), vec![2]);
    }
}

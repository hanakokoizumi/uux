//! File operations use the official protobuf FTP messages. Local destinations
//! are fixed by the user's request; peer paths cannot escape that directory.
#[path = "file_queue.rs"]
mod queue;
use super::session::Output;
use super::{pb, wire};
use anyhow::{Context, Result, ensure};
use pb::{file_transfer_ftp_request::Which as Req, file_transfer_ftp_response::Which as Rsp};
use prost::Message;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    path::PathBuf,
    time::Instant,
};

pub fn compress<M: Message>(message: &M) -> Result<Vec<u8>> {
    let mut encoder = flate2::write::ZlibEncoder::new(vec![], flate2::Compression::fast());
    encoder.write_all(&message.encode_to_vec())?;
    Ok(encoder.finish()?)
}
pub fn decompress<M: Message + Default>(bytes: &[u8]) -> Result<M> {
    let mut output = vec![];
    flate2::read::ZlibDecoder::new(bytes)
        .take(16 * 1024 * 1024 + 1)
        .read_to_end(&mut output)?;
    ensure!(output.len() <= 16 * 1024 * 1024, "目录清单过大");
    Ok(M::decode(output.as_slice())?)
}
fn request(value: Req) -> Vec<u8> {
    let id = match &value {
        Req::ReceiveRequest(r) => r.id.as_ref(),
        Req::SendRequest(r) => r.id.as_ref(),
        Req::FileAsk(r) => r.id.as_ref(),
        Req::FileBlock(r) => r.id.as_ref(),
        Req::Complete(r) => r.id.as_ref(),
        Req::ReadDir(r) => r.id.as_ref(),
        Req::RemoveDir(r) => r.id.as_ref(),
        _ => None,
    }
    .map(|id| id.task_id as i64);
    let value = pb::rpc_request::Which::FileTransferFtpRequest(pb::FileTransferFtpRequest {
        which: Some(value),
    });
    if let Some(id) = id {
        wire::wrap(pb::message::Which::RpcRequest(pb::RpcRequest {
            request_header: Some(pb::RequestHeader { request_id: id }),
            which: Some(value),
        }))
    } else {
        wire::rpc(value).1
    }
}
fn response(id: i64, value: Rsp) -> Vec<u8> {
    wire::response(
        id,
        pb::rpc_response::Which::FileTransferFtpResponse(pb::FileTransferFtpResponse {
            which: Some(value),
        }),
    )
}
fn task(id: i32) -> Option<pb::TaskId> {
    Some(pb::TaskId {
        task_id: id,
        file_index: 1,
    })
}
pub fn safe_name(value: &str) -> Result<&str> {
    ensure!(
        !value.is_empty()
            && value != "."
            && value != ".."
            && !value.contains(['/', '\\', ':', '\0']),
        "远端文件名不安全"
    );
    Ok(value)
}
struct Transfer {
    id: i32,
    name: String,
    remote: String,
    local: PathBuf,
    upload: bool,
    source: Option<File>,
    temporary: Option<tempfile::NamedTempFile>,
    size: u64,
    max_size: Option<u64>,
    offset: u64,
    initial_offset: u64,
    started: Instant,
    block: i32,
    pending: usize,
    block_bytes: usize,
    paused: bool,
    held_ack: Vec<Vec<u8>>,
    ready: bool,
    last: Instant,
}
impl Transfer {
    fn progress(&self, out: &Output, status: &str) {
        self.progress_error(out, status, None);
    }
    fn progress_error(&self, out: &Output, status: &str, error: Option<&str>) {
        out.native(
            "transfer",
            json!({"id":self.id,"name":self.name,"local":self.local,"remote":self.remote,
            "upload":self.upload,"size":self.size,"done":self.offset,"status":status,"error":error,
            "size_known":self.upload || self.ready || self.size > 0,
            "initial_offset":self.initial_offset,"elapsed_ms":self.started.elapsed().as_millis() as u64}),
        );
    }
    fn next_block(&mut self, out: &Output) -> Result<Vec<Vec<u8>>> {
        if self.paused || !self.ready || self.pending > 0 {
            return Ok(vec![]);
        }
        if self.offset == self.size {
            self.ready = false;
            self.progress(out, "等待远端确认");
            return Ok(vec![
                request(Req::Complete(pb::FileTransferComplete {
                    id: task(self.id),
                    error: 1,
                })),
                request(Req::Complete(pb::FileTransferComplete {
                    id: Some(pb::TaskId {
                        task_id: self.id,
                        file_index: -1,
                    }),
                    error: 1,
                })),
            ]);
        }
        // Keep blocks modest to bound shared SCTP congestion and queueing time
        // for interactive input. The vendored transport also handles messages
        // larger than its 128 KiB pending queue, without increasing that limit.
        let mut data = vec![0; ((self.size - self.offset).min(self.block_bytes as u64)) as usize];
        self.source
            .as_mut()
            .context("文件未打开")?
            .read_exact(&mut data)?;
        self.pending = data.len();
        self.last = Instant::now();
        Ok(vec![request(Req::FileBlock(pb::FileTransferBlock {
            id: task(self.id),
            block_id: self.block,
            data,
        }))])
    }
}
enum Mkdir {
    Create { target: String },
    Rename { target: String },
}
enum TreeWait {
    Operation(String),
    Directory {
        id: i32,
        local: PathBuf,
        remote: String,
        depth: u64,
    },
    Transfer(i32),
}
struct Tree {
    queue: std::collections::VecDeque<Value>,
    waiting: Option<TreeWait>,
    count: usize,
    last: Instant,
}
#[derive(Default)]
pub struct Files {
    tasks: HashMap<i32, Transfer>,
    tree: Option<Tree>,
    operations: HashMap<i32, String>,
    operation_rpcs: HashMap<i64, (String, Option<String>)>,
    mkdir: HashMap<i64, Mkdir>,
    queue: queue::BatchQueue,
    upload_block_bytes: Option<usize>,
}
impl Files {
    /// Bounded per-session setting for transport comparisons. Ordinary sessions
    /// keep 64 KiB until a smaller size demonstrates an acceptable tradeoff.
    pub fn set_upload_block_bytes(&mut self, bytes: u64) -> Result<()> {
        ensure!([4096, 16384, 65536].contains(&bytes), "上传分块大小无效");
        self.upload_block_bytes = Some(bytes as usize);
        Ok(())
    }
    fn start_tree(&mut self, c: &Value, out: &Output) -> Result<Vec<Vec<u8>>> {
        ensure!(self.tree.is_none(), "请等待当前文件夹传输结束");
        let local = PathBuf::from(c["local"].as_str().context("本地目录为空")?);
        let remote = c["remote"].as_str().context("远端目录为空")?;
        let mut queue = std::collections::VecDeque::new();
        if c["op"] == "upload" {
            ensure!(
                !local.symlink_metadata()?.file_type().is_symlink(),
                "不能递归发送符号链接"
            );
            let name = safe_name(
                local
                    .file_name()
                    .and_then(|n| n.to_str())
                    .context("目录名无效")?,
            )?;
            let target = format!("{}\\{name}", remote.trim_end_matches('\\'));
            let mut stack = vec![(local, target, 0)];
            while let Some((local, remote, depth)) = stack.pop() {
                ensure!(
                    depth < 64 && queue.len() < 10000,
                    "文件夹层级或文件数量超过限制"
                );
                queue.push_back(json!({"op":"mkdir","path":remote}));
                let mut entries =
                    std::fs::read_dir(&local)?.collect::<std::io::Result<Vec<_>>>()?;
                entries.sort_by_key(|e| e.file_name());
                for entry in entries {
                    let kind = entry.file_type()?;
                    ensure!(
                        !kind.is_symlink(),
                        "文件夹中包含符号链接，请单独选择需要发送的文件"
                    );
                    let name = entry
                        .file_name()
                        .to_str()
                        .context("文件名不是 UTF-8")?
                        .to_owned();
                    safe_name(&name)?;
                    if kind.is_dir() {
                        stack.push((entry.path(), format!("{remote}\\{name}"), depth + 1));
                    } else if kind.is_file() {
                        queue
                            .push_back(json!({"op":"upload","local":entry.path(),"remote":remote}));
                    }
                    ensure!(queue.len() + stack.len() <= 10000, "文件数量超过限制");
                }
            }
        } else {
            ensure!(local.is_dir(), "下载目标目录不存在");
            let name = safe_name(
                remote
                    .trim_end_matches('\\')
                    .rsplit(['\\', '/'])
                    .next()
                    .context("目录名为空")?,
            )?;
            queue.push_back(
                json!({"op":"tree_read","local":local.join(name),"remote":remote,"depth":0}),
            );
        }
        self.tree = Some(Tree {
            queue,
            waiting: None,
            count: 0,
            last: Instant::now(),
        });
        self.queue.tree_result = None;
        out.native("folder_status", json!({"message":"正在传输文件夹"}));
        self.advance_tree(out)
    }
    fn advance_tree(&mut self, out: &Output) -> Result<Vec<Vec<u8>>> {
        if self.queue.tree_paused() {
            return Ok(vec![]);
        }
        let Some(tree) = &mut self.tree else {
            return Ok(vec![]);
        };
        if tree.waiting.is_some() {
            return Ok(vec![]);
        }
        let Some(command) = tree.queue.pop_front() else {
            self.tree = None;
            self.queue.tree_result = Some(true);
            out.native("folder_status", json!({"message":"文件夹传输已完成"}));
            return Ok(vec![]);
        };
        tree.count += 1;
        ensure!(tree.count <= 10000, "文件数量超过限制");
        tree.last = Instant::now();
        let messages = if command["op"] == "tree_read" {
            let local = PathBuf::from(command["local"].as_str().context("下载路径无效")?);
            std::fs::create_dir(&local).context("不能创建下载文件夹，目标可能已存在")?;
            let id = (uuid::Uuid::new_v4().as_u128() as i32 & 0x3fffffff) + 1000;
            tree.waiting = Some(TreeWait::Directory {
                id,
                local,
                remote: command["remote"].as_str().unwrap_or("").into(),
                depth: command["depth"].as_u64().unwrap_or(0),
            });
            vec![request(Req::ReadDir(pb::ReadDir {
                id: task(id),
                path: command["remote"].as_str().unwrap_or("").into(),
            }))]
        } else if command["op"] == "mkdir" {
            tree.waiting = Some(TreeWait::Operation(
                command["path"].as_str().unwrap_or("").into(),
            ));
            self.command(&command, out)?
        } else {
            let messages = self.command(&command, out)?;
            let m = pb::Message::decode(messages[0].as_slice())?;
            let Some(pb::message::Which::RpcRequest(r)) = m.which else {
                unreachable!()
            };
            let id = r.request_header.context("文件请求编号缺失")?.request_id as i32;
            self.tree.as_mut().unwrap().waiting = Some(TreeWait::Transfer(id));
            messages
        };
        Ok(messages)
    }
    fn finish_tree_transfer(
        &mut self,
        id: i32,
        success: bool,
        out: &Output,
    ) -> Result<Vec<Vec<u8>>> {
        if self
            .tree
            .as_ref()
            .is_some_and(|t| matches!(t.waiting,Some(TreeWait::Transfer(i)) if i==id))
        {
            if !success {
                self.abort_tree(out);
                return Ok(vec![]);
            }
            self.tree.as_mut().unwrap().waiting = None;
            return self.advance_tree(out);
        }
        Ok(vec![])
    }
    fn abort_tree(&mut self, out: &Output) {
        if self.tree.take().is_some() {
            self.queue.tree_result = Some(false);
            out.native(
                "folder_status",
                json!({"message":"文件夹传输已停止，已完成的文件保留"}),
            );
        }
    }
    pub fn command(&mut self, c: &Value, out: &Output) -> Result<Vec<Vec<u8>>> {
        if let Some(result) = self.queue_command(c, out) {
            return result;
        }
        if (c["op"] == "upload"
            && c["local"]
                .as_str()
                .is_some_and(|p| std::path::Path::new(p).is_dir()))
            || (c["op"] == "download" && c["directory"] == true)
        {
            return self.start_tree(c, out);
        }
        match c["op"].as_str().unwrap_or("") {
            "folder_cancel" => {
                let id = self.tree.as_ref().and_then(|t| match t.waiting {
                    Some(TreeWait::Transfer(id)) => Some(id),
                    _ => None,
                });
                self.abort_tree(out);
                if let Some(id) = id {
                    return self.command(&json!({"op":"transfer_cancel","id":id}), out);
                }
                Ok(vec![])
            }
            "read_dir" => Ok(vec![request(Req::ReadDir(pb::ReadDir {
                id: task(c["task_id"].as_i64().unwrap_or(1) as i32),
                path: c["path"].as_str().unwrap_or("").into(),
            }))]),
            "mkdir" => {
                let target = c["path"]
                    .as_str()
                    .context("目录为空")?
                    .trim_end_matches('\\')
                    .to_owned();
                let (parent, name) = target
                    .rsplit_once('\\')
                    .context("请输入完整的远端目录路径")?;
                safe_name(name)?;
                let parent = if parent.ends_with(':') {
                    format!("{parent}\\")
                } else {
                    parent.to_owned()
                };
                let (id, bytes) = wire::rpc(pb::rpc_request::Which::FileTransferFtpRequest(
                    pb::FileTransferFtpRequest {
                        which: Some(Req::DirCreate(pb::FileDirCreate { path: parent })),
                    },
                ));
                self.mkdir.insert(id, Mkdir::Create { target });
                Ok(vec![bytes])
            }
            "rename" => {
                let name = c["name"].as_str().context("新名称为空")?;
                safe_name(name)?;
                let path = c["path"].as_str().context("路径为空")?;
                let parent = path
                    .rsplit_once(['\\', '/'])
                    .context("请输入完整的远端路径")?
                    .0;
                self.tracked_operation(
                    Req::Rename(pb::FileRename {
                        path: path.into(),
                        new_name: format!("{parent}\\{name}"),
                    }),
                    path,
                    c,
                )
            }
            "remove_file" => {
                let path = c["path"].as_str().context("路径为空")?;
                self.tracked_operation(
                    Req::RemoveFile(pb::FileRemoveFile { path: path.into() }),
                    path,
                    c,
                )
            }
            "remove_dir" => {
                let path = c["path"].as_str().context("目录为空")?.to_owned();
                let id = (uuid::Uuid::new_v4().as_u128() as i32 & 0x3fffffff) + 1000;
                self.operations.insert(id, path.clone());
                Ok(vec![request(Req::RemoveDir(pb::FileRemoveDir {
                    id: task(id),
                    path,
                    recursive: c["recursive"].as_bool().unwrap_or(false),
                }))])
            }
            "upload" | "download" => {
                ensure!(
                    self.tasks.len() < 32,
                    "同时传输的文件过多，请等待现有任务完成"
                );
                let id = c["queued_id"]
                    .as_i64()
                    .map(|v| v as i32)
                    .unwrap_or_else(|| (uuid::Uuid::new_v4().as_u128() as i32 & 0x3fffffff) + 1000);
                let local = PathBuf::from(c["local"].as_str().context("缺少本地路径")?);
                let remote = c["remote"].as_str().context("缺少远端路径")?.to_owned();
                let upload = c["op"] == "upload";
                let name = if upload {
                    local
                        .file_name()
                        .and_then(|v| v.to_str())
                        .context("文件名无效")?
                        .to_owned()
                } else {
                    remote
                        .rsplit(['\\', '/'])
                        .next()
                        .context("远端文件名为空")?
                        .to_owned()
                };
                safe_name(&name)?;
                let mut t = Transfer {
                    id,
                    name,
                    remote,
                    local,
                    upload,
                    source: None,
                    temporary: None,
                    size: 0,
                    max_size: c["max_bytes"].as_u64(),
                    offset: 0,
                    initial_offset: 0,
                    started: Instant::now(),
                    block: if upload { 0 } else { 1 },
                    pending: 0,
                    block_bytes: self.upload_block_bytes.unwrap_or(65536),
                    paused: false,
                    held_ack: vec![],
                    ready: false,
                    last: Instant::now(),
                };
                let message = if upload {
                    let file = File::open(&t.local)?;
                    let meta = file.metadata()?;
                    ensure!(meta.is_file(), "请选择文件，目录请逐项发送");
                    t.size = meta.len();
                    t.source = Some(file);
                    let files = vec![pb::FileInfo {
                        rel_path: t.name.clone(),
                        size: t.size,
                        modified_time: meta
                            .modified()?
                            .duration_since(std::time::UNIX_EPOCH)?
                            .as_secs(),
                    }];
                    let list_data = compress(&pb::FileList {
                        files: files.clone(),
                    })?;
                    Req::ReceiveRequest(pb::FileTransferReceiveRequest {
                        id: Some(pb::TaskId {
                            task_id: id,
                            file_index: 0,
                        }),
                        path: t.remote.clone(),
                        files,
                        file_op_strategy: 3,
                        folder_name: String::new(),
                        list_data,
                        task_unique_id: uuid::Uuid::new_v4().to_string(),
                    })
                } else {
                    ensure!(t.local.is_dir(), "下载目标必须是已有目录");
                    ensure!(!t.local.join(&t.name).exists(), "本地已存在同名文件");
                    t.temporary = Some(
                        tempfile::Builder::new()
                            .prefix(".uu-transfer-")
                            .tempfile_in(&t.local)?,
                    );
                    let (parent, name) = t
                        .remote
                        .rsplit_once(['\\', '/'])
                        .context("请输入远端文件完整路径")?;
                    let files = vec![pb::FileInfo {
                        rel_path: name.into(),
                        ..Default::default()
                    }];
                    Req::SendRequest(pb::FileTransferSendRequest {
                        id: Some(pb::TaskId {
                            task_id: id,
                            file_index: 0,
                        }),
                        path: format!("{parent}\\"),
                        list_data: compress(&pb::FileList { files })?,
                    })
                };
                t.progress(out, "准备中");
                self.tasks.insert(id, t);
                Ok(vec![request(message)])
            }
            "transfer_pause" => {
                let id = c["id"].as_i64().context("任务无效")? as i32;
                let t = self.tasks.get_mut(&id).context("任务已结束")?;
                t.paused = c["paused"].as_bool().unwrap_or(true);
                t.last = Instant::now();
                t.progress(out, if t.paused { "已暂停" } else { "传输中" });
                if t.upload {
                    t.next_block(out)
                } else if !t.paused {
                    Ok(std::mem::take(&mut t.held_ack))
                } else {
                    Ok(vec![])
                }
            }
            "transfer_cancel" => {
                let id = c["id"].as_i64().context("任务无效")? as i32;
                self.finish_tree_transfer(id, false, out)?;
                if let Some(t) = self.tasks.remove(&id) {
                    t.progress(out, "已取消");
                }
                Ok(vec![request(Req::Complete(pb::FileTransferComplete {
                    id: Some(pb::TaskId {
                        task_id: id,
                        file_index: -1,
                    }),
                    error: 7,
                }))])
            }
            _ => Ok(vec![]),
        }
    }
    fn tracked_operation(
        &mut self,
        request: Req,
        path: &str,
        command: &Value,
    ) -> Result<Vec<Vec<u8>>> {
        ensure!(self.operation_rpcs.len() < 128, "等待确认的文件操作过多");
        let (id, bytes) = wire::rpc(pb::rpc_request::Which::FileTransferFtpRequest(
            pb::FileTransferFtpRequest {
                which: Some(request),
            },
        ));
        self.operation_rpcs.insert(
            id,
            (
                path.into(),
                command["client_request"].as_str().map(str::to_owned),
            ),
        );
        Ok(vec![bytes])
    }
    pub fn response(
        &mut self,
        rpc_id: i64,
        r: pb::FileTransferFtpResponse,
        out: &Output,
    ) -> Result<Vec<Vec<u8>>> {
        if let Some((path, client_request)) = self.operation_rpcs.remove(&rpc_id) {
            let Some(Rsp::OperationResult(result)) = r.which else {
                anyhow::bail!("文件操作响应类型无效")
            };
            out.native(
                "file_operation",
                json!({"path":path,"error":result.file_error,"client_request":client_request}),
            );
            return Ok(vec![]);
        }
        if let Some(pending) = self.mkdir.remove(&rpc_id) {
            let Some(Rsp::OperationResult(r)) = r.which else {
                anyhow::bail!("新建目录响应类型无效")
            };
            match pending {
                Mkdir::Create { target, .. } if r.file_error == 1 => {
                    let (id, bytes) = wire::rpc(pb::rpc_request::Which::FileTransferFtpRequest(
                        pb::FileTransferFtpRequest {
                            which: Some(Req::Rename(pb::FileRename {
                                path: r.path,
                                new_name: target.clone(),
                            })),
                        },
                    ));
                    self.mkdir.insert(id, Mkdir::Rename { target });
                    return Ok(vec![bytes]);
                }
                Mkdir::Create { target, .. } | Mkdir::Rename { target } => {
                    return self.operation_complete(target, r.file_error, out);
                }
            }
        }
        match r.which {
            Some(Rsp::FileDirectory(mut dir)) => {
                if !dir.dir_data.is_empty() {
                    dir.entries = decompress::<pb::DirectoryData>(&dir.dir_data)?.entries;
                }
                if self.tree.as_ref().is_some_and(|t|matches!(&t.waiting,Some(TreeWait::Directory{id,..}) if Some(*id)==dir.id.as_ref().map(|i|i.task_id))) {
                    let tree=self.tree.as_mut().unwrap();
                    let Some(TreeWait::Directory{local,remote,depth,..})=tree.waiting.take() else{unreachable!()};
                    ensure!(dir.file_error==1,"文件夹读取失败");ensure!(depth<64,"文件夹层级超过限制");
                    for entry in &dir.entries {
                        safe_name(&entry.name)?;
                        let remote=format!("{}\\{}",remote.trim_end_matches('\\'),entry.name);
                        match entry.entry_type {
                            0=>tree.queue.push_back(json!({"op":"tree_read","local":local.join(&entry.name),"remote":remote,"depth":depth+1})),
                            4=>tree.queue.push_back(json!({"op":"download","local":local,"remote":remote})),
                            _=>{},
                        }
                        ensure!(tree.count+tree.queue.len()<=10000,"文件数量超过限制");
                    }
                    return self.advance_tree(out);
                }
                let entries:Vec<_>=dir.entries.iter().map(|e|json!({"name":e.name,"path":e.full_path,"size":e.size,"kind":e.entry_type,"modified":e.modified_time})).collect();
                out.native(
                    "directory",
                    json!({"path":dir.path,"error":dir.file_error,"entries":entries,"request_id":dir.id.as_ref().map(|id|id.task_id)}),
                );
            }
            Some(Rsp::ReceiveResponse(r)) => {
                let id = r.id.context("任务编号缺失")?.task_id;
                let Some(t) = self.tasks.get_mut(&id) else {
                    return Ok(vec![]);
                };
                if r.err != 1 {
                    return Ok(self.reject_transfer(
                        id,
                        &format!("远端拒绝接收文件（{}）", r.err),
                        out,
                    ));
                }
                t.progress(out, "传输中");
                return Ok(vec![request(Req::FileAsk(pb::FileTransferAsk {
                    id: task(id),
                    last_modified: 0,
                    file_size: t.size,
                }))]);
            }
            Some(Rsp::FileConfirm(r)) => {
                let id = r.id.context("任务编号缺失")?.task_id;
                let Some(t) = self.tasks.get_mut(&id) else {
                    return Ok(vec![]);
                };
                if r.err != 1 {
                    return Ok(self.reject_transfer(
                        id,
                        &format!("远端文件确认失败（{}）", r.err),
                        out,
                    ));
                }
                if r.skip {
                    t.progress(out, "已跳过同名文件");
                    self.tasks.remove(&id);
                    return self.finish_tree_transfer(id, true, out);
                }
                ensure!(r.resume_point <= t.size, "续传偏移无效");
                t.offset = r.resume_point;
                t.initial_offset = r.resume_point;
                t.source
                    .as_mut()
                    .context("源文件未打开")?
                    .seek(SeekFrom::Start(t.offset))?;
                t.ready = true;
                t.progress(out, "传输中");
                return t.next_block(out);
            }
            Some(Rsp::BlockConfirm(r)) => {
                let id = r.id.context("任务编号缺失")?.task_id;
                let Some(t) = self.tasks.get_mut(&id) else {
                    return Ok(vec![]);
                };
                if r.err != 1 || r.block_id != t.block {
                    return Ok(self.reject_transfer(id, "远端文件块确认失败", out));
                }
                t.offset += t.pending as u64;
                t.pending = 0;
                t.block += 1;
                t.progress(out, "传输中");
                return t.next_block(out);
            }
            Some(Rsp::SendResponse(mut r)) => {
                if !r.list_data.is_empty() {
                    r.files = decompress::<pb::FileList>(&r.list_data)?.files;
                }
                let id = r.id.context("任务编号缺失")?.task_id;
                let Some(t) = self.tasks.get_mut(&id) else {
                    return Ok(vec![]);
                };
                if r.err != 1 || r.files.len() != 1 {
                    return Ok(self.reject_transfer(
                        id,
                        &format!("远端文件请求失败（{}），返回 {} 项", r.err, r.files.len()),
                        out,
                    ));
                }
                ensure!(r.files[0].rel_path == t.name, "远端返回了非请求文件");
                if t.max_size.is_some_and(|limit| r.files[0].size > limit) {
                    t.progress(out, "失败");
                    self.tasks.remove(&id);
                    self.finish_tree_transfer(id, false, out)?;
                    return Ok(vec![request(Req::Complete(pb::FileTransferComplete {
                        id: task(id),
                        error: 7,
                    }))]);
                }
                t.size = r.files[0].size;
                t.progress(out, "等待数据");
            }
            Some(Rsp::Result(r)) => {
                let id = r.id.context("任务编号缺失")?.task_id;
                if let Some(path) = self.operations.remove(&id) {
                    out.native("file_operation", json!({"path":path,"error":r.file_error}));
                }
                if self
                    .tasks
                    .get(&id)
                    .is_some_and(|t| t.upload || r.file_error != 1)
                    && let Some(t) = self.tasks.remove(&id)
                {
                    t.progress(
                        out,
                        if r.file_error == 1 && t.offset == t.size {
                            "已完成"
                        } else {
                            "失败"
                        },
                    );
                    return self.finish_tree_transfer(
                        id,
                        r.file_error == 1 && t.offset == t.size,
                        out,
                    );
                }
            }
            Some(Rsp::OperationResult(r)) => {
                return self.operation_complete(r.path, r.file_error, out);
            }
            _ => {}
        }
        Ok(vec![])
    }
    fn operation_complete(
        &mut self,
        path: String,
        error: i32,
        out: &Output,
    ) -> Result<Vec<Vec<u8>>> {
        if self
            .tree
            .as_ref()
            .is_some_and(|t| matches!(&t.waiting,Some(TreeWait::Operation(p)) if *p==path))
        {
            if error != 1 {
                self.abort_tree(out);
                anyhow::bail!("创建远端文件夹失败（{error}）：{path}");
            }
            self.tree.as_mut().unwrap().waiting = None;
            return self.advance_tree(out);
        }
        out.native("file_operation", json!({"error":error,"path":path}));
        Ok(vec![])
    }
    pub fn request(
        &mut self,
        id: i64,
        r: pb::FileTransferFtpRequest,
        out: &Output,
    ) -> Result<Vec<Vec<u8>>> {
        match r.which {
            Some(Req::FileAsk(r)) => {
                if std::env::var_os("UU_PROTOCOL_TRACE").is_some() {
                    eprintln!("file ask rpc={id} task={:?} size={}", r.id, r.file_size);
                }
                let task_id = r.id.as_ref().context("任务编号缺失")?.task_id;
                let Some(t) = self.tasks.get_mut(&task_id) else {
                    return Ok(vec![response(
                        id,
                        Rsp::FileConfirm(pb::FileTransferConfirm {
                            id: r.id,
                            err: 7,
                            ..Default::default()
                        }),
                    )]);
                };
                ensure!(
                    !t.upload && t.temporary.is_some() && r.file_size == t.size,
                    "文件接收元数据不一致"
                );
                t.ready = true;
                t.last = Instant::now();
                t.progress(out, "传输中");
                Ok(vec![response(
                    id,
                    Rsp::FileConfirm(pb::FileTransferConfirm {
                        id: r.id,
                        err: 1,
                        ..Default::default()
                    }),
                )])
            }
            Some(Req::FileBlock(r)) => {
                let task_id = r.id.as_ref().context("任务编号缺失")?.task_id;
                let Some(t) = self.tasks.get_mut(&task_id) else {
                    return Ok(vec![response(
                        id,
                        Rsp::BlockConfirm(pb::FileTransferBlockConfirm {
                            id: r.id,
                            block_id: r.block_id,
                            err: 7,
                            block_len: 0,
                        }),
                    )]);
                };
                ensure!(
                    !t.upload
                        && t.ready
                        && r.block_id == t.block
                        && r.data.len() <= 1024 * 1024
                        && t.offset + r.data.len() as u64 <= t.size,
                    "文件块顺序或大小无效"
                );
                t.temporary
                    .as_mut()
                    .context("目标未打开")?
                    .write_all(&r.data)?;
                t.offset += r.data.len() as u64;
                t.block += 1;
                t.last = Instant::now();
                t.progress(out, "传输中");
                let ack = response(
                    id,
                    Rsp::BlockConfirm(pb::FileTransferBlockConfirm {
                        id: r.id,
                        block_id: r.block_id,
                        err: 1,
                        block_len: r.data.len() as i32,
                    }),
                );
                if t.paused {
                    ensure!(t.held_ack.len() < 1024, "远端未遵守传输流量限制");
                    t.held_ack.push(ack);
                    t.progress(out, "已暂停");
                    Ok(vec![])
                } else {
                    Ok(vec![ack])
                }
            }
            Some(Req::Complete(r)) => {
                let task_id = r.id.as_ref().context("任务编号缺失")?.task_id;
                let mut error = r.error;
                let mut message = String::new();
                if let Some(mut t) = self.tasks.remove(&task_id) {
                    if r.error == 1 && !t.upload {
                        let commit = (|| -> Result<()> {
                            ensure!(t.offset == t.size, "下载长度不完整");
                            let file = t.temporary.take().context("目标未打开")?;
                            file.as_file().sync_all()?;
                            file.persist_noclobber(t.local.join(&t.name))?;
                            Ok(())
                        })();
                        if let Err(e) = commit {
                            // The task has already been removed. Complete its tree
                            // with failure as well, or all later batch roots hang.
                            error = 7;
                            message = e.to_string();
                            out.native("folder_status", json!({"message":message}));
                        }
                    }
                    t.progress(
                        out,
                        if error == 1 {
                            "已完成"
                        } else if r.error == 1 {
                            "失败"
                        } else {
                            "远端已取消"
                        },
                    );
                }
                let mut messages = vec![response(
                    id,
                    Rsp::Result(pb::FileTransferResult {
                        id: r.id,
                        file_error: error,
                        err_msg: message,
                    }),
                )];
                messages.extend(self.finish_tree_transfer(task_id, error == 1, out)?);
                Ok(messages)
            }
            _ => Ok(vec![]),
        }
    }
    fn reject_transfer(&mut self, id: i32, reason: &str, out: &Output) -> Vec<Vec<u8>> {
        if let Some(t) = self.tasks.remove(&id) {
            t.progress_error(out, "失败", Some(reason));
        }
        let mut messages = self
            .finish_tree_transfer(id, false, out)
            .unwrap_or_default();
        messages.push(request(Req::Complete(pb::FileTransferComplete {
            id: task(id),
            error: 7,
        })));
        messages
    }
    pub fn close(&mut self, out: &Output) {
        self.close_queue(out);
        self.abort_tree(out);
        self.operations.clear();
        self.mkdir.clear();
        for (_, t) in self.tasks.drain() {
            t.progress(out, "连接已断开");
        }
    }
    pub fn tick(&mut self, out: &Output) -> Vec<Vec<u8>> {
        if self.tree.as_ref().is_some_and(|t| {
            !self.queue.tree_paused()
                && !matches!(t.waiting, Some(TreeWait::Transfer(_)))
                && t.last.elapsed().as_secs() > 30
        }) {
            self.abort_tree(out);
        }
        let ids = self
            .tasks
            .iter()
            .filter(|(_, t)| !t.paused && t.last.elapsed().as_secs() > 30)
            .map(|(id, _)| *id)
            .collect::<Vec<_>>();
        let mut messages = vec![];
        for id in ids {
            if let Some(t) = self.tasks.remove(&id) {
                let _ = self.finish_tree_transfer(id, false, out);
                t.progress(out, "传输超时");
                messages.push(request(Req::Complete(pb::FileTransferComplete {
                    id: Some(pb::TaskId {
                        task_id: t.id,
                        file_index: -1,
                    }),
                    error: 7,
                })));
            }
        }
        match self.advance_tree(out) {
            Ok(next) => messages.extend(next),
            Err(e) => {
                self.abort_tree(out);
                out.native("folder_status", json!({"message":e.to_string()}));
            }
        }
        messages.extend(self.advance_queue(out));
        messages
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn output() -> Output {
        let (tx, _) = std::sync::mpsc::channel();
        Output {
            media: Default::default(),
            generation: Some(1),
            events: tx,
            latest: Default::default(),
            ctx: Default::default(),
        }
    }
    fn request_value(data: &[u8]) -> Req {
        let m = pb::Message::decode(data).unwrap();
        let Some(pb::message::Which::RpcRequest(r)) = m.which else {
            panic!()
        };
        let Some(pb::rpc_request::Which::FileTransferFtpRequest(f)) = r.which else {
            panic!()
        };
        f.which.unwrap()
    }
    #[test]
    fn fragmented_upload_preserves_bytes_and_waits_for_each_ack() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("blocks.bin");
        let content: Vec<u8> = (0..150_017).map(|n| (n % 251) as u8).collect();
        std::fs::write(&source, &content).unwrap();
        let out = output();
        for (block_bytes, resume) in [4096, 16384, 65536]
            .into_iter()
            .flat_map(|bytes| [0, 123].map(|resume| (bytes, resume)))
        {
            let mut files = Files::default();
            files.set_upload_block_bytes(block_bytes).unwrap();
            let request = files
                .command(
                    &json!({"op":"upload","local":source,"remote":"C:\\Downloads"}),
                    &out,
                )
                .unwrap();
            let Req::ReceiveRequest(request) = request_value(&request[0]) else {
                panic!()
            };
            let id = request.id.unwrap().task_id;
            let mut messages = files
                .response(
                    1,
                    pb::FileTransferFtpResponse {
                        which: Some(Rsp::FileConfirm(pb::FileTransferConfirm {
                            id: task(id),
                            err: 1,
                            resume_point: resume as u64,
                            ..Default::default()
                        })),
                    },
                    &out,
                )
                .unwrap();
            let mut received = content[..resume].to_vec();
            let mut block_id = 0;
            while let Req::FileBlock(block) = request_value(&messages[0]) {
                assert_eq!(block.block_id, block_id);
                assert!(block.data.len() <= block_bytes as usize);
                // The acknowledged offset must not advance merely by enqueueing data.
                assert_eq!(files.tasks[&id].offset as usize, received.len());
                assert!(files.tick(&out).is_empty());
                received.extend_from_slice(&block.data);
                messages = files
                    .response(
                        1,
                        pb::FileTransferFtpResponse {
                            which: Some(Rsp::BlockConfirm(pb::FileTransferBlockConfirm {
                                id: task(id),
                                err: 1,
                                block_id,
                                ..Default::default()
                            })),
                        },
                        &out,
                    )
                    .unwrap();
                block_id += 1;
            }
            assert!(block_id > 2);
            assert_eq!(received, content);
            assert_eq!(messages.len(), 2);
            assert!(
                messages
                    .iter()
                    .all(|m| matches!(request_value(m), Req::Complete(c) if c.error == 1))
            );
            // Completion is still pending until the receiver confirms it.
            assert!(files.tasks.contains_key(&id));
            files
                .response(
                    1,
                    pb::FileTransferFtpResponse {
                        which: Some(Rsp::Result(pb::FileTransferResult {
                            id: task(id),
                            file_error: 1,
                            ..Default::default()
                        })),
                    },
                    &out,
                )
                .unwrap();
            assert!(!files.tasks.contains_key(&id));
        }
    }
    #[test]
    fn file_operation_reply_without_path_uses_request_correlation() {
        let mut f = Files::default();
        let (tx, rx) = std::sync::mpsc::channel();
        let out = Output {
            media: Default::default(),
            generation: Some(1),
            events: tx,
            latest: Default::default(),
            ctx: Default::default(),
        };
        f.command(&json!({"op":"rename","path":r"C:\test.txt","name":"new.txt","client_request":"request-one"}),&out).unwrap();
        let id = *f.operation_rpcs.keys().next().unwrap();
        f.response(
            id,
            pb::FileTransferFtpResponse {
                which: Some(Rsp::OperationResult(pb::FileOperationResult {
                    path: String::new(),
                    file_error: 1,
                    ..Default::default()
                })),
            },
            &out,
        )
        .unwrap();
        let event = rx.try_recv().unwrap();
        let value = serde_json::to_value(event).unwrap();
        assert_eq!(value["data"]["path"], r"C:\test.txt");
        assert_eq!(value["data"]["client_request"], "request-one");
        assert!(f.operation_rpcs.is_empty());
    }
    #[test]
    fn text_download_size_limit_is_enforced_before_accepting_bytes() {
        let mut f = Files::default();
        let out = output();
        let dir = tempfile::tempdir().unwrap();
        let frames = f
            .command(
                &json!({"op":"download","local":dir.path(),"remote":r"C:\note.txt","max_bytes":4}),
                &out,
            )
            .unwrap();
        let Req::SendRequest(req) = request_value(&frames[0]) else {
            panic!()
        };
        let id = req.id.unwrap().task_id;
        let messages = f
            .response(
                1,
                pb::FileTransferFtpResponse {
                    which: Some(Rsp::SendResponse(pb::FileTransferSendResponse {
                        id: task(id),
                        err: 1,
                        files: vec![pb::FileInfo {
                            rel_path: "note.txt".into(),
                            size: 5,
                            ..Default::default()
                        }],
                        ..Default::default()
                    })),
                },
                &out,
            )
            .unwrap();
        assert!(!messages.is_empty());
        assert!(f.tasks.is_empty());
        assert!(!dir.path().join("note.txt").exists());
    }
    #[test]
    fn requested_download_is_atomic_and_byte_exact() {
        let dir = tempfile::tempdir().unwrap();
        let out = output();
        let mut f = Files::default();
        let data = f
            .command(
                &json!({"op":"download","local":dir.path(),"remote":"C:\\Downloads\\数据.bin"}),
                &out,
            )
            .unwrap();
        let Req::SendRequest(req) = request_value(&data[0]) else {
            panic!()
        };
        let id = req.id.unwrap().task_id;
        f.response(
            1,
            pb::FileTransferFtpResponse {
                which: Some(Rsp::SendResponse(pb::FileTransferSendResponse {
                    id: task(id),
                    err: 1,
                    files: vec![pb::FileInfo {
                        rel_path: "数据.bin".into(),
                        size: 5,
                        ..Default::default()
                    }],
                    ..Default::default()
                })),
            },
            &out,
        )
        .unwrap();
        f.request(
            id as i64,
            pb::FileTransferFtpRequest {
                which: Some(Req::FileAsk(pb::FileTransferAsk {
                    id: task(id),
                    file_size: 5,
                    ..Default::default()
                })),
            },
            &out,
        )
        .unwrap();
        assert!(!dir.path().join("数据.bin").exists());
        let bytes = vec![0, 255, 127, 13, 10];
        f.request(
            id as i64,
            pb::FileTransferFtpRequest {
                which: Some(Req::FileBlock(pb::FileTransferBlock {
                    id: task(id),
                    block_id: 1,
                    data: bytes.clone(),
                })),
            },
            &out,
        )
        .unwrap();
        f.request(
            id as i64,
            pb::FileTransferFtpRequest {
                which: Some(Req::Complete(pb::FileTransferComplete {
                    id: task(id),
                    error: 1,
                })),
            },
            &out,
        )
        .unwrap();
        assert_eq!(std::fs::read(dir.path().join("数据.bin")).unwrap(), bytes);
        assert!(f.tasks.is_empty());
        assert!(
            f.command(
                &json!({"op":"download","local":dir.path(),"remote":"C:\\Downloads\\数据.bin"}),
                &out
            )
            .is_err()
        );
        assert_eq!(std::fs::read(dir.path().join("数据.bin")).unwrap(), bytes);
    }
    #[test]
    fn failed_download_commit_releases_tree_and_preserves_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let out = output();
        let mut f = Files::default();
        let messages = f
            .command(
                &json!({"op":"download","local":dir.path(),"remote":"C:\\collision.txt"}),
                &out,
            )
            .unwrap();
        let Req::SendRequest(req) = request_value(&messages[0]) else {
            panic!()
        };
        let id = req.id.unwrap().task_id;
        f.tree = Some(Tree {
            queue: Default::default(),
            waiting: Some(TreeWait::Transfer(id)),
            count: 1,
            last: Instant::now(),
        });
        // Another program creates the destination while the download is active.
        std::fs::write(dir.path().join("collision.txt"), b"keep").unwrap();
        f.request(
            1,
            pb::FileTransferFtpRequest {
                which: Some(Req::Complete(pb::FileTransferComplete {
                    id: task(id),
                    error: 1,
                })),
            },
            &out,
        )
        .unwrap();
        assert!(f.tasks.is_empty());
        assert!(f.tree.is_none());
        assert_eq!(f.queue.tree_result, Some(false));
        assert_eq!(
            std::fs::read(dir.path().join("collision.txt")).unwrap(),
            b"keep"
        );
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }
    #[test]
    fn cancellation_removes_partial_download_and_unsafe_names_are_rejected() {
        for name in ["..", ".", "../x", "C:\\x", "x/y", "a:b", "\0"] {
            assert!(safe_name(name).is_err());
        }
        let dir = tempfile::tempdir().unwrap();
        let out = output();
        let mut f = Files::default();
        let messages = f
            .command(
                &json!({"op":"download","local":dir.path(),"remote":"C:\\partial.bin"}),
                &out,
            )
            .unwrap();
        let Req::SendRequest(req) = request_value(&messages[0]) else {
            panic!()
        };
        let id = req.id.unwrap().task_id;
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
        let messages = f
            .command(&json!({"op":"transfer_cancel","id":id}), &out)
            .unwrap();
        let Req::Complete(r) = request_value(&messages[0]) else {
            panic!()
        };
        assert_eq!(r.id.unwrap().file_index, -1);
        assert!(f.tasks.is_empty());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }
    #[test]
    fn rejected_request_finishes_immediately_and_removes_partial_destination() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let mut out = output();
        out.events = tx;
        let mut files = Files::default();
        let messages = files
            .command(
                &json!({"op":"download","local":dir.path(),"remote":"C:\\absent.bin"}),
                &out,
            )
            .unwrap();
        let Req::SendRequest(req) = request_value(&messages[0]) else {
            panic!()
        };
        let id = req.id.unwrap().task_id;
        let replies = files
            .response(
                1,
                pb::FileTransferFtpResponse {
                    which: Some(Rsp::SendResponse(pb::FileTransferSendResponse {
                        id: task(id),
                        err: 7,
                        ..Default::default()
                    })),
                },
                &out,
            )
            .unwrap();
        assert!(files.tasks.is_empty());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
        assert!(matches!(request_value(&replies[0]),Req::Complete(c) if c.error == 7));
        assert!(rx.try_iter().any(|event| matches!(event, crate::backend::Event::Native {kind, data, ..} if kind == "transfer" && data["status"] == "失败" && data["error"].as_str().is_some_and(|e|e.contains("远端文件请求失败")))));
    }
    #[test]
    fn truncated_zlib_list_is_rejected() {
        let list = pb::FileList {
            files: vec![pb::FileInfo {
                rel_path: "hello".into(),
                size: 42,
                ..Default::default()
            }],
        };
        let data = compress(&list).unwrap();
        assert_eq!(decompress::<pb::FileList>(&data).unwrap(), list);
        assert!(decompress::<pb::FileList>(&[1, 2, 3]).is_err());
    }
    #[test]
    fn mkdir_uses_parent_then_renames_with_full_destination_path() {
        let out = output();
        let mut f = Files::default();
        let messages = f
            .command(&json!({"op":"mkdir","path":r"C:\Downloads\empty"}), &out)
            .unwrap();
        let Req::DirCreate(r) = request_value(&messages[0]) else {
            panic!()
        };
        assert_eq!(r.path, r"C:\Downloads");
        let m = pb::Message::decode(messages[0].as_slice()).unwrap();
        let Some(pb::message::Which::RpcRequest(r)) = m.which else {
            panic!()
        };
        let id = r.request_header.unwrap().request_id;
        let messages = f
            .response(
                id,
                pb::FileTransferFtpResponse {
                    which: Some(Rsp::OperationResult(pb::FileOperationResult {
                        file_error: 1,
                        path: r"C:\Downloads\新建文件夹".into(),
                        err_msg: String::new(),
                    })),
                },
                &out,
            )
            .unwrap();
        let Req::Rename(r) = request_value(&messages[0]) else {
            panic!()
        };
        assert_eq!(r.path, r"C:\Downloads\新建文件夹");
        assert_eq!(r.new_name, r"C:\Downloads\empty");
    }
}

//! A bounded FIFO of user-selected files and directories. A directory retains
//! the existing tree walker; the next selected root starts only after it ends.
use super::*;
use std::collections::VecDeque;

struct Queued {
    id: i32,
    command: Value,
    name: String,
    directory: bool,
    paused: bool,
}
impl Queued {
    fn progress(&self, out: &Output, status: &str, error: Option<String>) {
        out.native(
            "transfer",
            json!({
                "id":self.id,"name":self.name,"local":self.command["local"],
                "remote":self.command["remote"],"upload":self.command["op"]=="upload",
                "size":self.command["size"].as_u64().unwrap_or(0),"done":0,
                "directory":self.directory,"status":status,"error":error
            }),
        );
    }
}
#[derive(Default)]
pub(super) struct BatchQueue {
    waiting: VecDeque<Queued>,
    active: Option<Queued>,
    pub tree_result: Option<bool>,
}
impl BatchQueue {
    pub fn tree_paused(&self) -> bool {
        self.active
            .as_ref()
            .is_some_and(|q| q.directory && q.paused)
    }
}
impl Files {
    pub(super) fn queue_command(
        &mut self,
        c: &Value,
        out: &Output,
    ) -> Option<Result<Vec<Vec<u8>>>> {
        match c["op"].as_str()? {
            "transfer_batch" => Some(self.enqueue_batch(c, out)),
            "transfers_cancel" => Some(self.cancel_all(out)),
            "transfers_pause" => Some(self.pause_all(c["paused"] != false, out)),
            "transfer_pause" | "transfer_cancel" => {
                let id = c["id"].as_i64()? as i32;
                let cancel = c["op"] == "transfer_cancel";
                if let Some(index) = self.queue.waiting.iter().position(|q| q.id == id) {
                    if cancel {
                        self.queue
                            .waiting
                            .remove(index)
                            .unwrap()
                            .progress(out, "已取消", None);
                    } else {
                        let item = &mut self.queue.waiting[index];
                        item.paused = c["paused"] != false;
                        item.progress(
                            out,
                            if item.paused {
                                "已暂停"
                            } else {
                                "等待中"
                            },
                            None,
                        );
                    }
                    return Some(Ok(vec![]));
                }
                if self
                    .queue
                    .active
                    .as_ref()
                    .is_some_and(|q| q.id == id && q.directory)
                {
                    return Some(if cancel {
                        self.queue
                            .active
                            .take()
                            .unwrap()
                            .progress(out, "已取消", None);
                        self.command(&json!({"op":"folder_cancel"}), out)
                    } else {
                        let paused = c["paused"] != false;
                        let item = self.queue.active.as_mut().unwrap();
                        item.paused = paused;
                        item.progress(
                            out,
                            if paused {
                                "已暂停"
                            } else {
                                "文件夹传输中"
                            },
                            None,
                        );
                        if let Some(tree) = self.tree.as_mut() {
                            tree.last = Instant::now();
                        }
                        if let Some(TreeWait::Transfer(child)) =
                            self.tree.as_ref().and_then(|t| t.waiting.as_ref())
                        {
                            self.command(
                                &json!({"op":"transfer_pause","id":child,"paused":paused}),
                                out,
                            )
                        } else {
                            self.advance_tree(out)
                        }
                    });
                }
                None
            }
            _ => None,
        }
    }
    fn enqueue_batch(&mut self, c: &Value, out: &Output) -> Result<Vec<Vec<u8>>> {
        let items = c["items"].as_array().context("缺少批量传输项目")?;
        ensure!(
            !items.is_empty() && self.queue.waiting.len() + items.len() <= 10000,
            "批量传输最多 10,000 项"
        );
        let mut pending = Vec::with_capacity(items.len());
        let mut ids = self
            .tasks
            .keys()
            .copied()
            .chain(self.queue.waiting.iter().map(|q| q.id))
            .chain(self.queue.active.iter().map(|q| q.id))
            .collect::<std::collections::HashSet<_>>();
        // Validate the whole request before admitting it, then isolate runtime
        // failures per item so one missing file cannot discard later selections.
        for command in items {
            ensure!(
                command["op"] == "upload" || command["op"] == "download",
                "批量任务类型无效"
            );
            let local = command["local"].as_str().context("缺少本地路径")?;
            let remote = command["remote"].as_str().context("缺少远端路径")?;
            let upload = command["op"] == "upload";
            let name = if upload {
                std::path::Path::new(local)
                    .file_name()
                    .and_then(|s| s.to_str())
                    .context("文件名无效")?
            } else {
                remote
                    .trim_end_matches(['\\', '/'])
                    .rsplit(['\\', '/'])
                    .next()
                    .context("远端文件名无效")?
            };
            safe_name(name)?;
            let id = if let Some(requested) = command["queued_id"].as_i64() {
                ensure!(
                    (1000..=i64::from(i32::MAX)).contains(&requested),
                    "任务编号无效"
                );
                let id = requested as i32;
                ensure!(ids.insert(id), "任务编号已存在");
                id
            } else {
                loop {
                    let id = (uuid::Uuid::new_v4().as_u128() as i32 & 0x3fffffff) + 1000;
                    if ids.insert(id) {
                        break id;
                    }
                }
            };
            let directory = if upload {
                std::path::Path::new(local).is_dir()
            } else {
                command["directory"] == true
            };
            let mut command = command.clone();
            command["queued_id"] = json!(id);
            pending.push(Queued {
                id,
                name: name.into(),
                command,
                directory,
                paused: false,
            });
        }
        for item in pending {
            item.progress(out, "等待中", None);
            self.queue.waiting.push_back(item);
        }
        Ok(self.advance_queue(out))
    }
    pub(super) fn advance_queue(&mut self, out: &Output) -> Vec<Vec<u8>> {
        if let Some(item) = &self.queue.active {
            if (item.directory && self.tree.is_some())
                || (!item.directory && self.tasks.contains_key(&item.id))
            {
                return vec![];
            }
            if item.directory {
                item.progress(
                    out,
                    if self.queue.tree_result == Some(true) {
                        "已完成"
                    } else {
                        "失败"
                    },
                    None,
                );
            }
            self.queue.active = None;
        }
        if self.tree.is_some() || !self.tasks.is_empty() {
            return vec![];
        }
        while let Some(index) = self.queue.waiting.iter().position(|q| !q.paused) {
            let item = self.queue.waiting.remove(index).unwrap();
            let command = item.command.clone();
            if item.directory {
                item.progress(out, "文件夹传输中", None);
            }
            self.queue.active = Some(item);
            match self.command(&command, out) {
                Ok(messages) => return messages,
                Err(error) => {
                    self.abort_tree(out);
                    if let Some(item) = self.queue.active.take() {
                        item.progress(out, "失败", Some(error.to_string()));
                    }
                }
            }
        }
        vec![]
    }
    fn pause_all(&mut self, paused: bool, out: &Output) -> Result<Vec<Vec<u8>>> {
        for item in &mut self.queue.waiting {
            item.paused = paused;
            item.progress(out, if paused { "已暂停" } else { "等待中" }, None);
        }
        if let Some(item) = self.queue.active.as_mut().filter(|q| q.directory) {
            item.paused = paused;
            item.progress(
                out,
                if paused {
                    "已暂停"
                } else {
                    "文件夹传输中"
                },
                None,
            );
        }
        if let Some(tree) = &mut self.tree {
            tree.last = Instant::now();
        }
        let ids = self.tasks.keys().copied().collect::<Vec<_>>();
        let mut messages = vec![];
        for id in ids {
            messages.extend(
                self.command(&json!({"op":"transfer_pause","id":id,"paused":paused}), out)?,
            );
        }
        if !paused {
            messages.extend(self.advance_tree(out)?);
            messages.extend(self.advance_queue(out));
        }
        Ok(messages)
    }
    fn cancel_all(&mut self, out: &Output) -> Result<Vec<Vec<u8>>> {
        self.close_queue_with_status(out, "已取消");
        let mut messages = self.command(&json!({"op":"folder_cancel"}), out)?;
        for id in self.tasks.keys().copied().collect::<Vec<_>>() {
            messages.extend(self.command(&json!({"op":"transfer_cancel","id":id}), out)?);
        }
        Ok(messages)
    }
    fn close_queue_with_status(&mut self, out: &Output, status: &str) {
        for item in self.queue.waiting.drain(..) {
            item.progress(out, status, None);
        }
        if let Some(item) = self.queue.active.take().filter(|q| q.directory) {
            item.progress(out, status, None);
        }
    }
    pub(super) fn close_queue(&mut self, out: &Output) {
        self.close_queue_with_status(out, "连接已断开");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn output() -> (Output, std::sync::mpsc::Receiver<crate::backend::Event>) {
        let (tx, rx) = std::sync::mpsc::channel();
        (
            Output {
                media: Default::default(),
                generation: Some(1),
                events: tx,
                latest: Default::default(),
                ctx: Default::default(),
            },
            rx,
        )
    }
    fn skip_upload(f: &mut Files, id: i32, out: &Output) {
        f.response(
            id as i64,
            pb::FileTransferFtpResponse {
                which: Some(Rsp::FileConfirm(pb::FileTransferConfirm {
                    id: task(id),
                    err: 1,
                    skip: true,
                    ..Default::default()
                })),
            },
            out,
        )
        .unwrap();
    }
    #[test]
    fn batch_over_concurrency_limit_keeps_order_and_skips_only_bad_item() {
        let dir = tempfile::tempdir().unwrap();
        let (out, rx) = output();
        let mut f = Files::default();
        let mut items = vec![];
        for n in 0..40 {
            let path = dir.path().join(format!("{n:02}.txt"));
            if n != 12 {
                std::fs::write(&path, b"batch").unwrap();
            }
            items.push(json!({"op":"upload","local":path,"remote":"C:\\Downloads"}));
        }
        f.command(&json!({"op":"transfer_batch","items":items}), &out)
            .unwrap();
        assert_eq!(f.tasks.len(), 1);
        assert_eq!(f.queue.waiting.len(), 39);
        let mut names = vec![];
        while let Some(q) = &f.queue.active {
            let id = q.id;
            names.push(q.name.clone());
            skip_upload(&mut f, id, &out);
            f.tick(&out);
        }
        assert_eq!(names.len(), 39);
        assert_eq!(names.first().unwrap(), "00.txt");
        assert_eq!(names.last().unwrap(), "39.txt");
        assert!(f.queue.waiting.is_empty());
        assert!(rx.try_iter().any(|e|matches!(e,crate::backend::Event::Native{kind,data} if kind=="transfer"&&data["name"]=="12.txt"&&data["status"]=="失败")));
    }
    #[test]
    fn pending_pause_resume_cancel_and_disconnect_do_not_start_later_items() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.txt");
        std::fs::write(&path, b"one").unwrap();
        let (out, _) = output();
        let mut f = Files::default();
        f.command(
            &json!({"op":"transfer_batch","items":[
            {"op":"upload","local":path,"remote":"C:\\A"},
            {"op":"upload","local":path,"remote":"C:\\B"},
            {"op":"upload","local":path,"remote":"C:\\C"}]}),
            &out,
        )
        .unwrap();
        let first = f.queue.active.as_ref().unwrap().id;
        let second = f.queue.waiting[0].id;
        f.command(&json!({"op":"transfers_pause","paused":true}), &out)
            .unwrap();
        assert!(f.tasks[&first].paused);
        assert!(f.queue.waiting.iter().all(|q| q.paused));
        f.command(&json!({"op":"transfer_cancel","id":second}), &out)
            .unwrap();
        assert_eq!(f.queue.waiting.len(), 1);
        f.command(&json!({"op":"transfer_cancel","id":first}), &out)
            .unwrap();
        f.tick(&out);
        assert!(f.tasks.is_empty());
        f.command(&json!({"op":"transfers_pause","paused":false}), &out)
            .unwrap();
        assert_eq!(f.tasks.len(), 1);
        f.command(&json!({"op":"transfers_cancel"}), &out).unwrap();
        f.tick(&out);
        assert!(f.tasks.is_empty() && f.queue.waiting.is_empty());
        f.close(&out);
        assert!(f.tick(&out).is_empty());
    }
    #[test]
    fn two_selected_directories_run_sequentially_and_directory_pause_holds_next_step() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["one", "two"] {
            std::fs::create_dir(dir.path().join(name)).unwrap();
            std::fs::write(dir.path().join(name).join("file.txt"), b"nested").unwrap();
        }
        let (out, rx) = output();
        let mut f = Files::default();
        let mut messages = f
            .command(
                &json!({"op":"transfer_batch","items":[
            {"op":"upload","local":dir.path().join("one"),"remote":"C:\\Downloads"},
            {"op":"upload","local":dir.path().join("two"),"remote":"C:\\Downloads"}]}),
                &out,
            )
            .unwrap();
        assert_eq!(f.queue.waiting.len(), 1);
        let root = f.queue.active.as_ref().unwrap().id;
        f.command(
            &json!({"op":"transfer_pause","id":root,"paused":true}),
            &out,
        )
        .unwrap();
        // Simulate the official create-then-rename handshake, including its reply
        // arriving while the directory is paused.
        let mut visited = vec![];
        for _ in 0..20 {
            if let Some(bytes) = messages.pop() {
                let m = pb::Message::decode(bytes.as_slice()).unwrap();
                let Some(pb::message::Which::RpcRequest(r)) = m.which else {
                    panic!()
                };
                let id = r.request_header.unwrap().request_id;
                let Some(pb::rpc_request::Which::FileTransferFtpRequest(ftp)) = r.which else {
                    panic!()
                };
                match ftp.which.unwrap() {
                    Req::DirCreate(c) => {
                        messages.extend(
                            f.response(
                                id,
                                pb::FileTransferFtpResponse {
                                    which: Some(Rsp::OperationResult(pb::FileOperationResult {
                                        path: format!(
                                            "{}\\New folder",
                                            c.path.trim_end_matches('\\')
                                        ),
                                        file_error: 1,
                                        ..Default::default()
                                    })),
                                },
                                &out,
                            )
                            .unwrap(),
                        );
                    }
                    Req::Rename(r) => {
                        visited.push(r.new_name.clone());
                        messages.extend(
                            f.response(
                                id,
                                pb::FileTransferFtpResponse {
                                    which: Some(Rsp::OperationResult(pb::FileOperationResult {
                                        path: r.new_name,
                                        file_error: 1,
                                        ..Default::default()
                                    })),
                                },
                                &out,
                            )
                            .unwrap(),
                        );
                        if visited.len() == 1 {
                            assert!(messages.is_empty());
                            assert!(f.tasks.is_empty());
                            messages.extend(
                                f.command(
                                    &json!({"op":"transfer_pause","id":root,"paused":false}),
                                    &out,
                                )
                                .unwrap(),
                            );
                        }
                    }
                    Req::ReceiveRequest(r) => {
                        skip_upload(&mut f, r.id.unwrap().task_id, &out);
                    }
                    _ => panic!("unexpected request"),
                }
            }
            messages.extend(f.tick(&out));
            if f.queue.active.is_none() && f.queue.waiting.is_empty() {
                break;
            }
        }
        assert_eq!(visited, vec!["C:\\Downloads\\one", "C:\\Downloads\\two"]);
        assert!(f.queue.active.is_none() && f.tree.is_none());
        assert_eq!(rx.try_iter().filter(|e|matches!(e,crate::backend::Event::Native{kind,data} if kind=="transfer"&&data["directory"]==true&&data["status"]=="已完成")).count(),2);
    }
}

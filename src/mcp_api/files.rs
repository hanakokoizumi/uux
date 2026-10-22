use super::*;
use std::{collections::HashMap, path::Path};
fn transfer_id() -> i32 {
    ((uuid::Uuid::new_v4().as_u128() as u32) & 0x3fff_ffff) as i32 + 1000
}
pub fn directory(hub: &Shared, client: &str, id: &str, path: &str) -> Result<Value> {
    let request = transfer_id();
    let cursor = hub.lock().unwrap().ready(client, id)?.events.seq;
    submit(
        hub,
        client,
        id,
        json!({"op":"read_dir","path":path,"task_id":request}),
    )?;
    let event = wait_event(hub, client, id, cursor, Duration::from_secs(20), |v| {
        v["kind"] == "directory" && v["data"]["request_id"] == request
    })?;
    ensure!(
        event["data"]["error"] == 1,
        "读取目录失败：{}",
        event["data"]["error"]
    );
    Ok(event["data"].clone())
}
fn mutate(hub: &Shared, client: &str, id: &str, op: &str, args: &Value) -> Result<Value> {
    let _operation = Operation::acquire(hub, id, "file_mutation")?;
    let path = str_arg(args, "path")?;
    let cursor = hub.lock().unwrap().ready(client, id)?.events.seq;
    let mut command = args.clone();
    command["op"] = json!(op);
    submit(hub, client, id, command)?;
    let event = wait_event(hub, client, id, cursor, Duration::from_secs(30), |v| {
        v["kind"] == "file_operation"
            && (v["data"]["path"] == path
                || v["data"]["path"]
                    .as_str()
                    .is_some_and(|p| args["name"].as_str().is_some_and(|n| p.ends_with(n))))
    })?;
    ensure!(
        event["data"]["error"] == 1,
        "文件操作失败：{}",
        event["data"]["error"]
    );
    Ok(event["data"].clone())
}
pub fn transfers(
    hub: &Shared,
    client: &str,
    id: &str,
    job: &str,
    mut items: Vec<Value>,
) -> Result<Value> {
    ensure!(
        !items.is_empty() && items.len() <= 1000,
        "一次传输需为 1–1000 项"
    );
    let mut ids = vec![];
    for item in &mut items {
        let n = transfer_id();
        item["queued_id"] = json!(n);
        ids.push(n);
    }
    {
        let mut h = hub.lock().unwrap();
        h.jobs.get_mut(job).context("任务不存在")?.transfer_ids = ids.clone();
    }
    let cursor = hub.lock().unwrap().ready(client, id)?.events.seq;
    submit(
        hub,
        client,
        id,
        json!({"op":"transfer_batch","items":items}),
    )?;
    watch_transfers(hub, client, id, job, ids, cursor)
}
pub fn clipboard_transfer(
    hub: &Shared,
    client: &str,
    id: &str,
    job: &str,
    mut command: Value,
) -> Result<Value> {
    let n = -((transfer_id() as u32 & 0x0fff_ffff).max(1000) as i32
        + if command["op"] == "clipboard_get" {
            0x2000_0000
        } else {
            0
        });
    hub.lock()
        .unwrap()
        .jobs
        .get_mut(job)
        .context("任务不存在")?
        .transfer_ids = vec![n];
    let cursor = hub.lock().unwrap().ready(client, id)?.events.seq;
    command["queued_id"] = json!(n);
    submit(hub, client, id, command)?;
    watch_transfers(hub, client, id, job, vec![n], cursor)
}
fn watch_transfers(
    hub: &Shared,
    client: &str,
    id: &str,
    job: &str,
    ids: Vec<i32>,
    mut cursor: u64,
) -> Result<Value> {
    let initially_paused = hub.lock().unwrap().jobs[job].paused;
    if initially_paused {
        for n in &ids {
            submit(
                hub,
                client,
                id,
                json!({"op":"transfer_pause","id":n,"paused":true}),
            )?;
        }
    }
    let start = Instant::now();
    let mut last_change = Instant::now();
    let mut rows = HashMap::<i32, Value>::new();
    let mut rates = HashMap::<i32, (u64, Instant)>::new();
    loop {
        if check_cancel(hub, job).is_err() {
            for n in &ids {
                let _ = session_send(hub, client, id, json!({"op":"transfer_cancel","id":n}));
            }
            bail!("传输已取消");
        }
        let events = {
            let h = hub.lock().unwrap();
            let s = h.ready(client, id)?;
            s.events.since(cursor)
        };
        for row in events {
            cursor = row["cursor"].as_u64().unwrap_or(cursor);
            let v = &row["event"];
            if v["kind"] == "transfer"
                && let Some(n) = v["data"]["id"].as_i64()
                && ids.contains(&(n as i32))
            {
                let n = n as i32;
                let mut data = v["data"].clone();
                let done = data["done"].as_u64().unwrap_or(0);
                let total = data["size"].as_u64().unwrap_or(0);
                let finished = data["status"] == "已完成";
                data["percent"] = if data["directory"] == true || data["size_known"] == false {
                    if finished { json!(100.0) } else { Value::Null }
                } else if total > 0 {
                    json!((done as f64 * 100.0 / total as f64).min(100.0))
                } else {
                    json!(if finished { 100.0 } else { 0.0 })
                };
                if let Some((previous, time)) = rates.get(&n) {
                    let elapsed = time.elapsed().as_secs_f64();
                    if elapsed >= 0.1 {
                        data["bytes_per_second"] =
                            json!(done.saturating_sub(*previous) as f64 / elapsed);
                        rates.insert(n, (done, Instant::now()));
                    } else if let Some(previous) = rows.get(&n) {
                        data["bytes_per_second"] = previous["bytes_per_second"].clone();
                    }
                } else {
                    rates.insert(n, (done, Instant::now()));
                    data["bytes_per_second"] = json!(0);
                }
                if data["status"] != "传输中" {
                    data["bytes_per_second"] = Value::Null;
                }
                rows.insert(n, data);
                last_change = Instant::now();
            }
        }
        let values = ids
            .iter()
            .filter_map(|n| rows.get(n))
            .cloned()
            .collect::<Vec<_>>();
        let total_known = values.len() == ids.len()
            && values
                .iter()
                .all(|v| v["directory"] != true && v["size_known"] != false && v["size"].is_u64());
        let bytes: u64 = values.iter().filter_map(|v| v["done"].as_u64()).sum();
        let size: u64 = values.iter().filter_map(|v| v["size"].as_u64()).sum();
        let speed: f64 = values
            .iter()
            .filter_map(|v| v["bytes_per_second"].as_f64())
            .sum();
        let result = json!({"items":values,"elapsed_ms":start.elapsed().as_millis() as u64,"progress":{"bytes":bytes,"total_bytes":if total_known {Some(size)} else {None},"total_known":total_known,"percent":if total_known && size>0 {Some((bytes as f64*100.0/size as f64).min(100.0))} else {None},"bytes_per_second":speed,"items_total":ids.len(),"items_finished":rows.values().filter(|v|terminal_status(&v["status"])).count()}});
        if let Some(j) = hub.lock().unwrap().jobs.get_mut(job) {
            if j.paused {
                last_change = Instant::now();
            }
            let active: Vec<_> = values
                .iter()
                .filter(|v| !terminal_status(&v["status"]))
                .collect();
            if j.paused && !active.is_empty() && active.iter().all(|v| v["status"] == "已暂停") {
                j.state = "paused".into();
            } else if !j.paused && !j.cancelled && active.iter().any(|v| v["status"] == "传输中")
            {
                j.state = "running".into();
            }
        }
        job_update(hub, job, result.clone());
        if ids
            .iter()
            .all(|n| rows.get(n).is_some_and(|v| terminal_status(&v["status"])))
        {
            ensure!(
                rows.values()
                    .all(|v| matches!(v["status"].as_str(), Some("已完成" | "已跳过同名文件"))),
                "一个或多个传输失败，请查看 items 状态"
            );
            return Ok(result);
        }
        ensure!(
            last_change.elapsed() < Duration::from_secs(1800),
            "传输长时间没有进度"
        );
        std::thread::sleep(Duration::from_millis(60));
    }
}
fn terminal_status(v: &Value) -> bool {
    matches!(
        v.as_str(),
        Some("已完成" | "已跳过同名文件" | "失败" | "已取消" | "连接已断开" | "传输超时")
    )
}
pub fn call(hub: &Shared, client: &str, name: &str, args: &Value) -> Result<Value> {
    let id = sid(args)?;
    match name {
        "uu_files_list" => {
            let mut result = directory(
                hub,
                client,
                id,
                args["path"]
                    .as_str()
                    .filter(|p| !p.is_empty())
                    .unwrap_or("C:\\"),
            )?;
            let entries = result["entries"].as_array().cloned().unwrap_or_default();
            let offset = args["offset"].as_u64().unwrap_or(0) as usize;
            let limit = args["limit"].as_u64().unwrap_or(200).clamp(1, 1000) as usize;
            let page = entries
                .iter()
                .skip(offset)
                .take(limit)
                .cloned()
                .collect::<Vec<_>>();
            result["entries"] = json!(page);
            result["total"] = json!(entries.len());
            result["next_offset"] = if offset + page.len() < entries.len() {
                json!(offset + page.len())
            } else {
                Value::Null
            };
            Ok(result)
        }
        "uu_files_mkdir" => mutate(hub, client, id, "mkdir", args),
        "uu_files_rename" => mutate(hub, client, id, "rename", args),
        "uu_files_delete" => mutate(
            hub,
            client,
            id,
            if args["directory"] == true {
                "remove_dir"
            } else {
                "remove_file"
            },
            args,
        ),
        "uu_files_upload" | "uu_files_download" => {
            let upload = name == "uu_files_upload";
            let mut items = if let Some(items) = args["items"].as_array() {
                items.clone()
            } else {
                vec![args.clone()]
            };
            for item in &mut items {
                let local = str_arg(item, "local_path")?.to_owned();
                ensure!(
                    Path::new(&local).is_absolute(),
                    "local_path 必须是 Linux 服务器上的绝对路径"
                );
                let remote = str_arg(item, "remote_path")?.to_owned();
                item["op"] = json!(if upload { "upload" } else { "download" });
                item["local"] = json!(local);
                item["remote"] = json!(remote);
            }
            start_job(hub, client, id, "transfer", move |h, c, s, j| {
                transfers(&h, &c, &s, &j, items)
            })
        }
        "uu_files_read_text" | "uu_files_write_text" => {
            let remote = str_arg(args, "path")?.to_owned();
            let write = name == "uu_files_write_text";
            let text = if write {
                args["text"].as_str().context("缺少 text")?.to_owned()
            } else {
                String::new()
            };
            ensure!(text.len() <= 1024 * 1024, "文本文件最多 1 MiB");
            start_job(
                hub,
                client,
                id,
                if write { "write_text" } else { "read_text" },
                move |h, c, s, j| {
                    let (parent, name) = remote
                        .rsplit_once(['\\', '/'])
                        .context("需要远端文件完整路径")?;
                    crate::core::files::safe_name(name)?;
                    let listing =
                        directory(&h, &c, &s, &format!("{}\\", parent.trim_end_matches('\\')))?;
                    let entry = listing["entries"]
                        .as_array()
                        .and_then(|entries| entries.iter().find(|e| e["name"] == name));
                    let temp = tempfile::tempdir()?;
                    if write {
                        ensure!(
                            entry.is_none(),
                            "远端同名文件已存在；请显式删除或选择新文件名"
                        );
                        std::fs::write(temp.path().join(name), &text)?;
                        let result = transfers(
                            &h,
                            &c,
                            &s,
                            &j,
                            vec![
                                json!({"op":"upload","local":temp.path().join(name),"remote":format!("{parent}\\")}),
                            ],
                        )?;
                        ensure!(
                            result["items"]
                                .as_array()
                                .is_some_and(|v| v.iter().all(|v| v["status"] == "已完成")),
                            "远端同名文件被跳过，未写入文本"
                        );
                        Ok(
                            json!({"path":remote,"bytes":text.len(),"written":true,"encoding":"utf-8"}),
                        )
                    } else {
                        let entry = entry.context("文件不存在")?;
                        ensure!(
                            entry["kind"] == 4
                                && entry["size"].as_u64().is_some_and(|n| n <= 1024 * 1024),
                            "只支持不超过 1 MiB 的普通文本文件"
                        );
                        transfers(
                            &h,
                            &c,
                            &s,
                            &j,
                            vec![
                                json!({"op":"download","local":temp.path(),"remote":remote,"max_bytes":1048576}),
                            ],
                        )?;
                        let bytes = std::fs::read(temp.path().join(name))?;
                        ensure!(bytes.len() <= 1024 * 1024, "下载后的文本文件过大");
                        let (text, encoding) = decode_text(&bytes)?;
                        Ok(
                            json!({"path":remote,"text":text,"encoding":encoding,"bytes":bytes.len()}),
                        )
                    }
                },
            )
        }
        "uu_files_search" => {
            let path = str_arg(args, "path")?.to_owned();
            let query = str_arg(args, "query")?.to_lowercase();
            let limit = args["limit"].as_u64().unwrap_or(200).clamp(1, 1000) as usize;
            start_job(hub, client, id, "search", move |h, c, s, j| {
                let mut queue = std::collections::VecDeque::from([(path, 0)]);
                let mut matches = vec![];
                let mut visited = 0;
                let mut skipped = vec![];
                while let Some((path, depth)) = queue.pop_front() {
                    check_cancel(&h, &j)?;
                    visited += 1;
                    ensure!(visited <= 2000, "搜索超过 2000 个目录，请缩小范围");
                    let listing = match directory(&h, &c, &s, &path) {
                        Ok(v) => v,
                        Err(e) => {
                            skipped.push(json!({"path":path,"error":e.to_string()}));
                            continue;
                        }
                    };
                    for entry in listing["entries"].as_array().into_iter().flatten() {
                        let Some(name) = entry["name"].as_str() else {
                            continue;
                        };
                        crate::core::files::safe_name(name)?;
                        let full = format!("{}\\{name}", path.trim_end_matches(['\\', '/']));
                        if name.to_lowercase().contains(&query) {
                            let mut item = entry.clone();
                            item["path"] = json!(full);
                            matches.push(item);
                            if matches.len() >= limit {
                                return Ok(
                                    json!({"matches":matches,"truncated":true,"visited_directories":visited,"skipped":skipped}),
                                );
                            }
                        }
                        if entry["kind"] == 0 {
                            if depth < 32 {
                                queue.push_back((full, depth + 1));
                            } else {
                                skipped.push(json!({"path":full,"error":"达到 32 层深度限制"}));
                            }
                        }
                    }
                    job_update(
                        &h,
                        &j,
                        json!({"matches":matches,"visited_directories":visited,"pending_directories":queue.len(),"skipped":skipped}),
                    );
                }
                Ok(
                    json!({"matches":matches,"truncated":!skipped.is_empty(),"visited_directories":visited,"skipped":skipped}),
                )
            })
        }
        _ => bail!("未知文件工具"),
    }
}
fn decode_text(bytes: &[u8]) -> Result<(String, &'static str)> {
    if bytes.starts_with(&[0xff, 0xfe]) || bytes.starts_with(&[0xfe, 0xff]) {
        ensure!(bytes.len().is_multiple_of(2), "UTF-16 文件长度无效");
        let little = bytes[0] == 0xff;
        let units = bytes[2..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|b| {
                if little {
                    u16::from_le_bytes([b[0], b[1]])
                } else {
                    u16::from_be_bytes([b[0], b[1]])
                }
            })
            .collect::<Vec<_>>();
        return Ok((
            String::from_utf16(&units)?,
            if little { "utf-16le" } else { "utf-16be" },
        ));
    }
    Ok((
        std::str::from_utf8(bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes))?.to_owned(),
        "utf-8",
    ))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sent_bytes_do_not_mean_transfer_completed() {
        assert!(!terminal_status(&json!("等待远端确认")));
        assert!(terminal_status(&json!("已完成")));
    }
}

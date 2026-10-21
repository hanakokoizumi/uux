//! Typed public operations and bounded asynchronous jobs over the native UU core.
mod catalog;
pub(crate) mod desktop;
mod files;
mod ports;
mod preferences;
mod terminal;
use crate::service::{Job, Shared, now_ms, uid};
use anyhow::{Context, Result, bail, ensure};
pub use catalog::catalog;
use serde_json::{Value, json};
use std::time::{Duration, Instant};
pub struct Operation {
    hub: Shared,
    key: (String, String),
}
impl Operation {
    pub fn acquire(hub: &Shared, session: &str, kind: &str) -> Result<Self> {
        let key = (session.to_owned(), kind.to_owned());
        ensure!(
            hub.lock().unwrap().operations.insert(key.clone()),
            "同类操作正在处理中，请等待完成"
        );
        Ok(Self {
            hub: hub.clone(),
            key,
        })
    }
}
impl Drop for Operation {
    fn drop(&mut self) {
        self.hub.lock().unwrap().operations.remove(&self.key);
    }
}
pub fn str_arg<'a>(a: &'a Value, k: &str) -> Result<&'a str> {
    a[k].as_str()
        .filter(|s| !s.is_empty())
        .with_context(|| format!("缺少或无效的参数 {k}"))
}
pub fn sid(a: &Value) -> Result<&str> {
    str_arg(a, "session_id")
}
pub fn session_send(hub: &Shared, client: &str, id: &str, command: Value) -> Result<()> {
    hub.lock().unwrap().ready(client, id)?.send(command)
}
pub fn submit(hub: &Shared, client: &str, id: &str, mut command: Value) -> Result<Value> {
    let request = uid();
    command["client_request"] = json!(request);
    let cursor = hub.lock().unwrap().ready(client, id)?.events.seq;
    session_send(hub, client, id, command)?;
    let event = wait_event(hub, client, id, cursor, Duration::from_secs(10), |v| {
        v["kind"] == "command_result" && v["data"]["client_request"] == request
    })?;
    ensure!(
        event["data"]["ok"] == true,
        "{}",
        event["data"]["error"].as_str().unwrap_or("操作失败")
    );
    Ok(json!({"submitted":true,"request_id":request}))
}
pub fn wait_event<F: Fn(&Value) -> bool>(
    hub: &Shared,
    client: &str,
    id: &str,
    mut cursor: u64,
    timeout: Duration,
    predicate: F,
) -> Result<Value> {
    let start = Instant::now();
    loop {
        {
            let h = hub.lock().unwrap();
            let s = h.owner(client, id)?;
            for row in s.events.since(cursor) {
                cursor = row["cursor"].as_u64().unwrap_or(cursor);
                if predicate(&row["event"]) {
                    return Ok(row["event"].clone());
                }
            }
            ensure!(!s.ended, "远端连接已结束");
        }
        ensure!(start.elapsed() < timeout, "等待远端结果超时");
        std::thread::sleep(Duration::from_millis(15));
    }
}
pub fn start_job<F>(hub: &Shared, client: &str, id: &str, kind: &str, work: F) -> Result<Value>
where
    F: FnOnce(Shared, String, String, String) -> Result<Value> + Send + 'static,
{
    let job = uid();
    {
        let mut h = hub.lock().unwrap();
        h.ready(client, id)?;
        ensure!(
            h.jobs
                .values()
                .filter(|j| !matches!(j.state.as_str(), "completed" | "failed" | "cancelled"))
                .count()
                < 64,
            "运行中的任务过多"
        );
        h.jobs.insert(
            job.clone(),
            Job {
                owner: client.into(),
                session: id.into(),
                kind: kind.into(),
                state: "running".into(),
                result: json!({}),
                error: None,
                cancelled: false,
                created: now_ms(),
                updated: now_ms(),
                transfer_ids: vec![],
            },
        );
    }
    let (h, c, s, j) = (hub.clone(), client.to_owned(), id.to_owned(), job.clone());
    std::thread::spawn(move || {
        let result = work(h.clone(), c, s, j.clone());
        if let Some(job) = h.lock().unwrap().jobs.get_mut(&j) {
            job.updated = now_ms();
            job.state = if job.cancelled {
                "cancelled"
            } else if result.is_ok() {
                "completed"
            } else {
                "failed"
            }
            .into();
            match result {
                Ok(v) => job.result = v,
                Err(e) => job.error = Some(e.to_string()),
            }
        }
    });
    Ok(json!({"task_id":job,"state":"running"}))
}
pub fn check_cancel(hub: &Shared, job: &str) -> Result<()> {
    let h = hub.lock().unwrap();
    let j = h.jobs.get(job).context("任务不存在")?;
    ensure!(!j.cancelled, "任务已取消");
    Ok(())
}
pub fn job_update(hub: &Shared, job: &str, value: Value) {
    if let Some(j) = hub.lock().unwrap().jobs.get_mut(job) {
        j.result = value;
        j.updated = now_ms();
    }
}
pub fn call(hub: &Shared, client: &str, name: &str, args: &Value) -> Result<Value> {
    {
        let h = hub.lock().unwrap();
        let c = h.clients.get(client).context("客户端已退出")?;
        ensure!(c.kind == "gui" || h.enabled, "MCP 已关闭");
    }
    match name {
        "uu_account_status" => Ok(hub.lock().unwrap().account_state.clone()),
        "uu_login_start" => {
            let mut h = hub.lock().unwrap();
            ensure!(
                h.sessions.values().all(|s| s.ended),
                "请先关闭现有会话再登录"
            );
            let login_id = now_ms();
            h.login = json!({"login_id":login_id,"status":0});
            h.account
                .send(json!({"op":"login","login_id":login_id}))
                .map_err(anyhow::Error::msg)?;
            Ok(h.login.clone())
        }
        "uu_login_status" => {
            let h = hub.lock().unwrap();
            let mut login = h.login.clone();
            if let Some(png) = login.as_object_mut().and_then(|v| v.remove("png")) {
                login["image_base64"] = png;
                login["mime_type"] = json!("image/png");
            }
            Ok(login)
        }
        "uu_login_cancel" => {
            let mut h = hub.lock().unwrap();
            h.account
                .send(json!({"op":"cancel_login"}))
                .map_err(anyhow::Error::msg)?;
            h.login = json!({"status":"cancelled"});
            Ok(h.login.clone())
        }
        "uu_devices_list" => {
            let cursor = {
                let h = hub.lock().unwrap();
                h.account
                    .send(json!({"op":"devices"}))
                    .map_err(anyhow::Error::msg)?;
                h.account_events.seq
            };
            let start = Instant::now();
            loop {
                {
                    let h = hub.lock().unwrap();
                    for row in h.account_events.since(cursor) {
                        if row["event"]["event"] == "devices" {
                            return Ok(
                                json!({"devices":h.devices.as_array().into_iter().flatten().map(|d|{let mut d=d.clone();d["device_id"]=d["id"].clone();d.as_object_mut().unwrap().remove("wallpaper");d}).collect::<Vec<_>>()}),
                            );
                        }
                        if row["event"]["scope"] == "devices" {
                            bail!("{}", row["event"]["message"]);
                        }
                    }
                }
                ensure!(
                    start.elapsed() < Duration::from_secs(15),
                    "获取设备列表超时"
                );
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        "uu_session_open" => {
            let mut options = args.clone();
            let desktop = args["mode"].as_str() == Some("desktop");
            let mut settings = json!({"wait_for_video":false,"audio":false,"clipboard":false,"drag_receive":false});
            if let Some(value) = args["super_screen"].as_bool() {
                settings["super_screen"] = json!(value);
            }
            options["settings"] = settings;
            let id = {
                let mut h = hub.lock().unwrap();
                if let Some(session) = args["session_id"].as_str() {
                    let target = h
                        .sessions
                        .get(session)
                        .context("会话不存在")?
                        .target
                        .clone();
                    if target.starts_with("assist:") {
                        bail!("协助会话复用仍需提供 assist_id 与验证码");
                    }
                    options["device_id"] = json!(target);
                }
                h.open(client, &options)?
            };
            if desktop {
                hub.lock().unwrap().video(client, &id, true)?;
            }
            let wait = args["wait_ms"].as_u64().unwrap_or(0).min(30000);
            let start = Instant::now();
            loop {
                let status = { hub.lock().unwrap().owner(client, &id)?.status() };
                if wait == 0
                    || status["state"] == "disconnected"
                    || status[if desktop {
                        "video_ready"
                    } else {
                        "tools_ready"
                    }] == true
                    || start.elapsed() >= Duration::from_millis(wait)
                {
                    return Ok(status);
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        "uu_session_list" => {
            let h = hub.lock().unwrap();
            Ok(
                json!({"sessions":h.sessions.values().map(|s|{let mut v=s.status();v["attached"]=json!(s.owners.contains(client));v}).collect::<Vec<_>>()}),
            )
        }
        "uu_session_status" => Ok(hub.lock().unwrap().owner(client, sid(args)?)?.status()),
        "uu_session_close" => {
            let id = sid(args)?;
            let retained = {
                let mut h = hub.lock().unwrap();
                h.owner(client, id)?;
                h.release(client, id);
                h.sessions.get(id).is_some_and(|s| !s.owners.is_empty())
            };
            if retained {
                return Ok(json!({"detached":true,"session_retained":true}));
            }
            let started = Instant::now();
            loop {
                let released = hub.lock().unwrap().sessions.get(id).is_none_or(|s| s.ended);
                if released || started.elapsed() > Duration::from_secs(8) {
                    return Ok(
                        json!({"detached":true,"session_retained":false,"release_confirmed":released}),
                    );
                }
                std::thread::sleep(Duration::from_millis(25));
            }
        }
        "uu_screenshot"
        | "uu_desktop_action"
        | "uu_type_text"
        | "uu_screens_list"
        | "uu_display_configure"
        | "uu_clipboard_get"
        | "uu_clipboard_set" => desktop::call(hub, client, name, args),
        "uu_command_exec" | "uu_terminal_open" | "uu_terminal_read" | "uu_terminal_write"
        | "uu_terminal_resize" | "uu_terminal_close" => terminal::call(hub, client, name, args),
        "uu_files_list"
        | "uu_files_mkdir"
        | "uu_files_rename"
        | "uu_files_delete"
        | "uu_files_upload"
        | "uu_files_download"
        | "uu_transfer_control"
        | "uu_files_read_text"
        | "uu_files_write_text"
        | "uu_files_search" => files::call(hub, client, name, args),
        "uu_port_create" | "uu_port_list" | "uu_port_update" | "uu_port_start" | "uu_port_stop"
        | "uu_port_delete" => ports::call(hub, client, name, args),
        "uu_favorites_list"
        | "uu_favorites_save"
        | "uu_favorites_delete"
        | "uu_device_rename"
        | "uu_mcp_status"
        | "uu_mcp_configure" => preferences::call(hub, client, name, args),
        "uu_task_get" => {
            let id = str_arg(args, "task_id")?;
            let h = hub.lock().unwrap();
            let j = h.jobs.get(id).context("任务不存在或已过期")?;
            ensure!(
                j.owner == client || h.clients[client].kind == "gui",
                "任务属于另一个客户端"
            );
            Ok(j.value(id))
        }
        "uu_task_cancel" => {
            let id = str_arg(args, "task_id")?;
            let mut h = hub.lock().unwrap();
            let gui = h.clients[client].kind == "gui";
            let j = h.jobs.get_mut(id).context("任务不存在")?;
            ensure!(j.owner == client || gui, "任务属于另一个客户端");
            if !matches!(j.state.as_str(), "completed" | "failed" | "cancelled") {
                j.cancelled = true;
                j.state = "cancelling".into();
            }
            Ok(j.value(id))
        }
        "uu_events_poll" => {
            let id = sid(args)?;
            let cursor = args["cursor"].as_u64().unwrap_or(0);
            let wait = args["wait_ms"].as_u64().unwrap_or(0).min(10000);
            let start = Instant::now();
            loop {
                {
                    let h = hub.lock().unwrap();
                    let s = h.owner(client, id)?;
                    let rows = s
                        .events
                        .since(cursor)
                        .into_iter()
                        .filter(|r| {
                            r["event"]["kind"] != "terminal_output"
                                && r["event"]["kind"] != "clipboard_value"
                        })
                        .collect::<Vec<_>>();
                    if !rows.is_empty() || start.elapsed() >= Duration::from_millis(wait) {
                        return Ok(
                            json!({"events":rows,"cursor":s.events.seq,"gap":cursor>0&&cursor<s.events.oldest().saturating_sub(1)}),
                        );
                    }
                }
                std::thread::sleep(Duration::from_millis(25));
            }
        }
        _ => bail!("未知工具：{name}"),
    }
}

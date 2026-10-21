//! One local owner of UU connections, shared by GUI and MCP clients.
use crate::{
    backend::{Backend, Event, VideoFrame},
    ipc,
};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    io::{Read, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
pub type Shared = Arc<Mutex<Hub>>;
pub fn uid() -> String {
    uuid::Uuid::new_v4().to_string()
}
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
#[derive(Default)]
pub struct Journal {
    pub seq: u64,
    rows: VecDeque<(u64, Value)>,
    bytes: usize,
}
impl Journal {
    pub fn push(&mut self, value: Value) {
        self.seq += 1;
        self.bytes += value.to_string().len();
        self.rows.push_back((self.seq, value));
        while self.rows.len() > 1024 || self.bytes > 4 * 1024 * 1024 {
            if let Some((_, v)) = self.rows.pop_front() {
                self.bytes = self.bytes.saturating_sub(v.to_string().len());
            }
        }
    }
    pub fn since(&self, cursor: u64) -> Vec<Value> {
        self.rows
            .iter()
            .filter(|(n, _)| *n > cursor)
            .map(|(n, v)| json!({"cursor":n,"event":v}))
            .collect()
    }
    pub fn oldest(&self) -> u64 {
        self.rows.front().map_or(self.seq, |(n, _)| *n)
    }
}
pub struct Subscriber {
    pub kind: String,
    pub sessions: HashSet<String>,
    pub gui: Option<(String, u64)>,
    pub account_cursor: u64,
    pub session_cursor: u64,
    pub pending: Vec<Value>,
    pub last_seen: Instant,
}
pub struct Shell {
    pub owner: String,
    pub id: String,
    pub ready: bool,
    pub closed: bool,
    pub exit_code: Option<i64>,
    pub bytes: Vec<u8>,
    pub offset: u64,
    pub error: Option<String>,
}
impl Shell {
    pub fn new(owner: &str) -> Self {
        Self {
            owner: owner.into(),
            id: uid(),
            ready: false,
            closed: false,
            exit_code: None,
            bytes: vec![],
            offset: 0,
            error: None,
        }
    }
}
pub struct Managed {
    pub backend: Backend,
    pub id: String,
    pub target: String,
    pub device_id: Option<String>,
    pub owners: HashSet<String>,
    pub ready: bool,
    pub ended: bool,
    pub closing: bool,
    pub video_owners: HashSet<String>,
    pub phase: String,
    pub events: Journal,
    pub snapshot: HashMap<String, Value>,
    pub frame: Option<Arc<VideoFrame>>,
    pub shell: Option<Shell>,
    pub input_owner: Option<String>,
    pub input_until: Instant,
    pub ports: HashMap<u64, (String, Value)>,
    pub created: u64,
}
impl Managed {
    pub fn send(&self, command: Value) -> Result<()> {
        self.backend
            .send(json!({"op":"input","generation":1,"command":command}))
            .map_err(anyhow::Error::msg)
    }
    pub fn status(&self) -> Value {
        json!({"session_id":self.id,"device_id":self.device_id,"target":self.target,"phase":self.phase,"tools_ready":self.ready&&!self.ended,"video_ready":self.frame.is_some()&&!self.ended,"state":if self.ended{"disconnected"}else if self.closing{"disconnecting"}else if self.ready{"connected"}else{"connecting"},"consumers":self.owners.len(),"created_at_ms":self.created})
    }
}
pub struct Job {
    pub owner: String,
    pub session: String,
    pub kind: String,
    pub state: String,
    pub result: Value,
    pub error: Option<String>,
    pub cancelled: bool,
    pub created: u64,
    pub updated: u64,
    pub transfer_ids: Vec<i32>,
}
impl Job {
    pub fn value(&self, id: &str) -> Value {
        json!({"task_id":id,"session_id":self.session,"kind":self.kind,"state":self.state,"result":self.result,"error":self.error,"created_at_ms":self.created,"updated_at_ms":self.updated})
    }
}
pub struct Hub {
    pub directory: PathBuf,
    pub account: Backend,
    pub account_events: Journal,
    pub account_state: Value,
    pub devices: Value,
    pub login: Value,
    pub sessions: HashMap<String, Managed>,
    pub clients: HashMap<String, Subscriber>,
    pub jobs: HashMap<String, Job>,
    pub shots: HashMap<String, crate::mcp_api::desktop::Shot>,
    pub operations: HashSet<(String, String)>,
    pub enabled: bool,
    pub stopping: bool,
}
impl Hub {
    pub fn new(directory: PathBuf) -> Result<Self> {
        let account =
            Backend::start(Some(&directory), Default::default()).map_err(anyhow::Error::msg)?;
        let settings: Value = std::fs::read(directory.join("mcp.json"))
            .ok()
            .and_then(|s| serde_json::from_slice(&s).ok())
            .unwrap_or(json!({}));
        Ok(Self {
            directory,
            account,
            account_events: Default::default(),
            account_state: json!({"logged_in":false}),
            devices: json!([]),
            login: Value::Null,
            sessions: HashMap::new(),
            clients: HashMap::new(),
            jobs: HashMap::new(),
            shots: HashMap::new(),
            operations: HashSet::new(),
            enabled: settings["enabled"].as_bool().unwrap_or(true),
            stopping: false,
        })
    }
    pub fn register(&mut self, kind: &str) -> Result<String> {
        ensure!(["gui", "mcp"].contains(&kind), "未知客户端类型");
        ensure!(kind != "mcp" || self.enabled, "MCP 已在设置中关闭");
        ensure!(self.clients.len() < 32, "客户端数量已达到上限");
        let id = uid();
        let state = json!({"event":"state","logged_in":self.account_state["logged_in"],"nickname":self.account_state["nickname"].as_str().unwrap_or("")});
        self.clients.insert(
            id.clone(),
            Subscriber {
                kind: kind.into(),
                sessions: HashSet::new(),
                gui: None,
                account_cursor: self.account_events.seq,
                session_cursor: 0,
                pending: vec![json!({"event":"hello","protocol":1}), state],
                last_seen: Instant::now(),
            },
        );
        Ok(id)
    }
    pub fn owner(&self, client: &str, sid: &str) -> Result<&Managed> {
        ensure!(self.clients.contains_key(client), "客户端已断开");
        let s = self.sessions.get(sid).context("会话不存在或已释放")?;
        ensure!(s.owners.contains(client), "本客户端尚未加入此会话");
        Ok(s)
    }
    pub fn ready(&self, client: &str, sid: &str) -> Result<&Managed> {
        let s = self.owner(client, sid)?;
        ensure!(s.ready && !s.ended, "会话尚未就绪或已结束");
        Ok(s)
    }
    pub fn open(&mut self, client: &str, args: &Value) -> Result<String> {
        ensure!(self.account_state["logged_in"] == true, "请先扫码登录");
        let (target, device_id) = if let Some(id) = args["assist_id"].as_str() {
            (
                format!("assist:{}", crate::core::cloud::assistance_id(id)?),
                None,
            )
        } else {
            let id = args["device_id"]
                .as_str()
                .filter(|s| !s.is_empty())
                .context("缺少 device_id 或 assist_id")?;
            (id.to_owned(), Some(id.to_owned()))
        };
        ensure!(self.clients.contains_key(client), "客户端已退出");
        let existing = self
            .sessions
            .values()
            .find(|s| s.target == target && !s.ended && !s.closing)
            .map(|s| s.id.clone());
        let id = if let Some(id) = existing {
            id
        } else {
            ensure!(
                self.sessions.values().filter(|s| !s.ended).count() < 8,
                "最多同时连接 8 台设备"
            );
            let backend = Backend::start(Some(&self.directory), Default::default())
                .map_err(anyhow::Error::msg)?;
            let id = uid();
            let mut command = args.clone();
            command["op"] = json!("connect");
            command["generation"] = json!(1);
            let mut settings = args["settings"].as_object().cloned().unwrap_or_default();
            settings.entry("wait_for_video").or_insert(json!(false));
            settings.entry("audio").or_insert(json!(false));
            settings.entry("clipboard").or_insert(json!(false));
            settings.entry("drag_receive").or_insert(json!(false));
            if !settings.contains_key("super_screen") {
                let prefs = preferences(&self.directory);
                if let Some(value) = prefs["screen_preferences"][&target].as_bool() {
                    settings.insert("super_screen".into(), json!(value));
                }
            }
            command["settings"] = json!(settings);
            backend.send(command).map_err(anyhow::Error::msg)?;
            self.sessions.insert(
                id.clone(),
                Managed {
                    backend,
                    id: id.clone(),
                    target: target.clone(),
                    device_id,
                    owners: HashSet::new(),
                    ready: false,
                    ended: false,
                    closing: false,
                    video_owners: HashSet::new(),
                    phase: "connecting".into(),
                    events: Default::default(),
                    snapshot: HashMap::new(),
                    frame: None,
                    shell: None,
                    input_owner: None,
                    input_until: Instant::now(),
                    ports: HashMap::new(),
                    created: now_ms(),
                },
            );
            id
        };
        self.sessions
            .get_mut(&id)
            .unwrap()
            .owners
            .insert(client.into());
        self.clients
            .get_mut(client)
            .unwrap()
            .sessions
            .insert(id.clone());
        Ok(id)
    }
    pub fn video(&mut self, client: &str, id: &str, enabled: bool) -> Result<()> {
        self.owner(client, id)?;
        let s = self.sessions.get_mut(id).unwrap();
        if enabled {
            s.video_owners.insert(client.into());
        } else {
            s.video_owners.remove(client);
        }
        s.send(json!({"op":"expect_video","enabled":!s.video_owners.is_empty()}))
    }
    pub fn release(&mut self, client: &str, sid: &str) {
        let mut transfers = Vec::new();
        if let Some(c) = self.clients.get_mut(client) {
            c.sessions.remove(sid);
            if c.gui.as_ref().is_some_and(|(id, _)| id == sid) {
                c.gui = None;
            }
        }
        for job in self.jobs.values_mut().filter(|j| {
            j.owner == client
                && j.session == sid
                && !matches!(j.state.as_str(), "completed" | "failed" | "cancelled")
        }) {
            job.cancelled = true;
            transfers.extend(job.transfer_ids.iter().copied());
        }
        if let Some(s) = self.sessions.get_mut(sid) {
            for id in transfers {
                let _ = s.send(json!({"op":"transfer_cancel","id":id}));
            }
            if s.shell
                .as_ref()
                .is_some_and(|t| t.owner == client && !t.closed)
            {
                let _ = s.send(json!({"op":"terminal","action":"close"}));
            }
            if s.input_owner.as_deref() == Some(client) {
                let _ = s.send(json!({"op":"release"}));
                s.input_owner = None;
            }
            let ports: Vec<_> = s
                .ports
                .iter()
                .filter(|(_, (owner, _))| owner == client)
                .map(|(id, _)| *id)
                .collect();
            for id in ports {
                let _ = s.send(json!({"op":"port_mapping","id":id,"enabled":false}));
                s.ports.remove(&id);
            }
            s.owners.remove(client);
            if s.video_owners.remove(client) && !s.owners.is_empty() {
                let _ = s.send(json!({"op":"expect_video","enabled":!s.video_owners.is_empty()}));
            }
            if self.clients.get(client).is_some_and(|c| c.kind == "gui") && !s.owners.is_empty() {
                for op in ["audio", "clipboard_enabled", "drag_receive"] {
                    let _ = s.send(json!({"op":op,"enabled":false}));
                }
            }
            if s.owners.is_empty() {
                s.closing = true;
                let _ = s.backend.send(json!({"op":"disconnect"}));
            }
        }
    }
    pub fn remove_client(&mut self, client: &str) {
        let sessions = self
            .clients
            .get(client)
            .map(|c| c.sessions.iter().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        for sid in sessions {
            self.release(client, &sid);
        }
        self.clients.remove(client);
        self.shots.retain(|_, shot| shot.owner != client);
    }
    pub fn pump(&mut self) {
        for event in self.account.events.try_iter() {
            let value = serde_json::to_value(&event).unwrap_or(Value::Null);
            match &event {
                Event::State { .. } => self.account_state = value.clone(),
                Event::Devices { devices } => self.devices = json!(devices),
                Event::Qr { .. } | Event::LoginStatus { .. } | Event::LoginComplete { .. } => {
                    if self.login.is_null() {
                        self.login = json!({});
                    }
                    if let Some(fields) = value.as_object() {
                        self.login.as_object_mut().unwrap().extend(fields.clone());
                    }
                }
                Event::Error { scope, .. } if scope == "login" => self.login = value.clone(),
                _ => {}
            }
            self.account_events.push(value);
        }
        for s in self.sessions.values_mut() {
            for event in s.backend.events.try_iter() {
                match &event {
                    Event::Connected { .. } => s.ready = true,
                    Event::Disconnected { .. } => {
                        s.ended = true;
                        s.ready = false;
                        s.frame = None;
                    }
                    Event::Phase { phase, .. } => s.phase = phase.clone(),
                    Event::Native { kind, data } => {
                        if kind == "terminal_output" {
                            use base64::Engine;
                            if let Some(t) = &mut s.shell
                                && let Ok(bytes) = base64::engine::general_purpose::STANDARD
                                    .decode(data["data"].as_str().unwrap_or(""))
                            {
                                t.bytes.extend(bytes);
                                if t.bytes.len() > 2 * 1024 * 1024 {
                                    let remove = t.bytes.len() - 2 * 1024 * 1024;
                                    t.bytes.drain(..remove);
                                    t.offset += remove as u64;
                                }
                            }
                        } else if kind == "terminal_status" {
                            if let Some(t) = &mut s.shell {
                                match data["kind"].as_i64() {
                                    Some(6) => {
                                        t.closed = true;
                                        t.exit_code = data["result"]["exit_code"].as_i64();
                                    }
                                    Some(8) => t.error = Some(data["result"].to_string()),
                                    Some(13) if data["result"]["code"] == 0 => t.ready = true,
                                    Some(13) => t.error = Some(data["result"].to_string()),
                                    Some(15) => t.ready = true,
                                    _ => {}
                                }
                            }
                        } else if kind == "terminal_closed"
                            && let Some(t) = &mut s.shell
                        {
                            t.closed = true;
                            t.exit_code = data["exit_code"].as_i64();
                        }
                        if kind == "port_status"
                            && let Some((_, rule)) =
                                data["id"].as_u64().and_then(|n| s.ports.get_mut(&n))
                        {
                            rule["status"] = data["status"].clone();
                            rule["message"] = data["message"].clone();
                        }
                        if [
                            "screens",
                            "super_screen",
                            "remote_capture",
                            "stats",
                            "capabilities",
                            "clipboard_value",
                        ]
                        .contains(&kind.as_str())
                        {
                            s.snapshot.insert(kind.clone(), data.clone());
                        }
                    }
                    _ => {}
                }
                s.events
                    .push(serde_json::to_value(&event).unwrap_or(Value::Null));
            }
            if let Some(frame) = s.backend.latest.lock().unwrap().take() {
                s.frame = Some(Arc::new(frame));
            }
        }
        // Remove fully released backends outside live use. shutdown is short after Disconnected.
        self.sessions
            .retain(|_, s| !(s.ended && s.owners.is_empty()));
        if self.jobs.len() > 256 {
            let mut old: Vec<_> = self
                .jobs
                .iter()
                .filter(|(_, j)| matches!(j.state.as_str(), "completed" | "failed" | "cancelled"))
                .map(|(id, j)| (j.updated, id.clone()))
                .collect();
            old.sort();
            for (_, id) in old.into_iter().take(self.jobs.len() - 256) {
                self.jobs.remove(&id);
            }
        }
    }
    pub fn poll(&mut self, client: &str) -> Result<Value> {
        let c = self.clients.get_mut(client).context("客户端已退出")?;
        let mut events = std::mem::take(&mut c.pending);
        for row in self.account_events.since(c.account_cursor) {
            events.push(row["event"].clone());
        }
        c.account_cursor = self.account_events.seq;
        if let Some((sid, generation)) = &c.gui
            && let Some(s) = self.sessions.get(sid)
        {
            for row in s.events.since(c.session_cursor) {
                let mut v = row["event"].clone();
                remap(&mut v, *generation);
                events.push(v);
            }
            c.session_cursor = s.events.seq;
        }
        Ok(json!({"events":events}))
    }
    pub fn gui_command(&mut self, client: &str, command: &Value) -> Result<Value> {
        ensure!(
            self.clients.get(client).is_some_and(|c| c.kind == "gui"),
            "需要 GUI 客户端"
        );
        match command["op"].as_str().unwrap_or("") {
            "connect" => {
                if let Some((id, _)) = self.clients[client].gui.clone() {
                    self.release(client, &id);
                }
                let id = self.open(client, command)?;
                let generation = command["generation"].as_u64().unwrap_or(1);
                let c = self.clients.get_mut(client).unwrap();
                c.gui = Some((id.clone(), generation));
                let s = &self.sessions[&id];
                c.session_cursor = s.events.seq;
                for (kind, data) in &s.snapshot {
                    let mut v = json!({"event":"native","kind":kind,"data":data});
                    remap(&mut v, generation);
                    c.pending.push(v);
                }
                if s.ready {
                    c.pending
                        .push(json!({"event":"connected","generation":generation}));
                }
                for (setting, op) in [
                    ("audio", "audio"),
                    ("clipboard", "clipboard_enabled"),
                    ("drag_receive", "drag_receive"),
                ] {
                    s.send(json!({"op":op,"enabled":command["settings"][setting],"directory":command["settings"]["download_dir"]}))?;
                }
                self.video(client, &id, command["settings"]["wait_for_video"] == true)?;
            }
            "disconnect" | "quit" => {
                if let Some((id, generation)) = self.clients[client].gui.clone() {
                    self.release(client, &id);
                    self.clients
                        .get_mut(client)
                        .unwrap()
                        .pending
                        .push(json!({"event":"disconnected","generation":generation}));
                }
            }
            "input" => {
                let Some((id, generation)) = self.clients[client].gui.clone() else {
                    return Ok(json!({}));
                };
                ensure!(command["generation"] == generation, "旧会话输入已拒绝");
                let input = &command["command"];
                let op = input["op"].as_str().unwrap_or("");
                if op == "expect_video" {
                    self.video(client, &id, input["enabled"] == true)?;
                    return Ok(json!({"accepted":true}));
                }
                let s = self.sessions.get_mut(&id).context("会话已结束")?;
                if op == "release"
                    && s.input_owner
                        .as_deref()
                        .is_some_and(|owner| owner != client)
                {
                    return Ok(json!({"accepted":true}));
                }
                if op == "terminal" {
                    if input["action"] == "open" {
                        ensure!(
                            s.shell.as_ref().is_none_or(|t| t.closed),
                            "终端已被 MCP 使用"
                        );
                        s.shell = Some(Shell::new(client));
                    } else {
                        ensure!(
                            s.shell.as_ref().is_some_and(|t| t.owner == client),
                            "此终端属于另一个客户端"
                        );
                    }
                }
                if input_action(op) {
                    if s.input_owner
                        .as_deref()
                        .is_some_and(|owner| owner != client)
                    {
                        s.send(json!({"op":"release"}))?;
                    }
                    s.input_owner = Some(client.into());
                    s.input_until = Instant::now() + Duration::from_secs(2);
                }
                if op == "port_mapping"
                    && let Some(port) = input["id"].as_u64()
                {
                    if input["enabled"] == true {
                        s.ports.insert(port, (client.into(), input.clone()));
                    } else {
                        s.ports.remove(&port);
                    }
                }
                s.send(input.clone())?;
            }
            "login" | "logout" => {
                ensure!(
                    self.sessions.values().all(|s| s.ended),
                    "请先结束所有 GUI 和 MCP 会话再切换账号"
                );
                self.account
                    .send(command.clone())
                    .map_err(anyhow::Error::msg)?;
            }
            _ => self
                .account
                .send(command.clone())
                .map_err(anyhow::Error::msg)?,
        }
        Ok(json!({"accepted":true}))
    }
}
pub fn input_action(op: &str) -> bool {
    [
        "down",
        "up",
        "move",
        "relative",
        "button_down",
        "button_up",
        "click",
        "scroll",
        "text",
        "release",
        "paste_local",
    ]
    .contains(&op)
}
fn remap(v: &mut Value, generation: u64) {
    if v.get("generation").is_some() {
        v["generation"] = json!(generation);
    }
    if v["data"].get("generation").is_some() {
        v["data"]["generation"] = json!(generation);
    }
}
pub fn preferences(path: &Path) -> Value {
    std::fs::read(path.join("desktop.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or(json!({}))
}

pub fn run(state: Option<&Path>) -> Result<()> {
    let directory = ipc::directory(state)?;
    let Some(_lock) = ipc::lock(&directory.join("core.lock"))? else {
        return Ok(());
    };
    let socket = directory.join("core.sock");
    let _ = std::fs::remove_file(&socket);
    let listener = UnixListener::bind(&socket)?;
    listener.set_nonblocking(true)?;
    let hub = Arc::new(Mutex::new(Hub::new(
        directory.parent().unwrap().to_owned(),
    )?));
    let mut idle = Instant::now();
    loop {
        loop {
            match listener.accept() {
                Ok((stream, _)) => {
                    if !ipc::peer_is_owner(&stream) {
                        continue;
                    }
                    let hub = hub.clone();
                    std::thread::spawn(move || {
                        let _ = serve(stream, hub);
                    });
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) => return Err(e.into()),
            }
        }
        {
            let mut h = hub.lock().unwrap();
            h.pump();
            if !h.clients.is_empty() || !h.sessions.is_empty() {
                idle = Instant::now();
            }
            if h.stopping {
                break;
            }
        }
        if idle.elapsed() > Duration::from_secs(60) {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let _ = std::fs::remove_file(socket);
    Ok(())
}
fn serve(mut stream: UnixStream, hub: Shared) -> Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    let request = ipc::read(&mut stream)?;
    if request["op"] == "register" {
        let result = hub
            .lock()
            .unwrap()
            .register(request["kind"].as_str().unwrap_or(""));
        let id = match result {
            Ok(id) => id,
            Err(e) => {
                ipc::write(&mut stream, &json!({"_ipc_error":e.to_string()}))?;
                return Ok(());
            }
        };
        if ipc::write(&mut stream, &json!({"client_id":id})).is_ok() {
            stream.set_read_timeout(None)?;
            let mut byte = [0];
            let _ = stream.read(&mut byte);
        }
        hub.lock().unwrap().remove_client(&id);
        return Ok(());
    }
    let client = request["client_id"].as_str().context("缺少客户端编号")?;
    {
        let mut h = hub.lock().unwrap();
        let c = h.clients.get_mut(client).context("客户端不存在")?;
        c.last_seen = Instant::now();
    }
    if request["op"] == "frame" {
        let client = client.to_owned();
        let mut request = request.clone();
        loop {
            let frame = {
                let h = hub.lock().unwrap();
                let c = h.clients.get(&client).context("客户端已退出")?;
                c.gui.as_ref().and_then(|(id, g)| {
                    h.sessions
                        .get(id)
                        .and_then(|s| s.frame.as_ref().map(|f| (f.clone(), *g)))
                })
            };
            if let Some((frame, generation)) = frame
                && (request["generation"] != generation || request["sequence"] != frame.sequence)
            {
                ipc::write(
                    &mut stream,
                    &json!({"frame":true,"width":frame.width,"height":frame.height,"sequence":frame.sequence,"generation":generation,"assembled_age_us":frame.assembled_at.elapsed().as_micros() as u64,"decoded_age_us":frame.decoded_at.elapsed().as_micros() as u64}),
                )?;
                stream.write_all(&frame.rgba)?;
            } else {
                ipc::write(&mut stream, &json!({"frame":false}))?;
            }
            request = ipc::read(&mut stream)?;
            ensure!(
                request["op"] == "frame" && request["client_id"] == client,
                "帧连接请求不匹配"
            );
        }
    }
    let result = match request["op"].as_str().unwrap_or("") {
        "poll" => hub.lock().unwrap().poll(client),
        "gui_command" => hub.lock().unwrap().gui_command(client, &request["command"]),
        "call" => crate::mcp_api::call(
            &hub,
            client,
            request["name"].as_str().unwrap_or(""),
            &request["arguments"],
        ),
        _ => bail!("未知共享核心请求"),
    };
    ipc::write(
        &mut stream,
        &result.unwrap_or_else(|e| json!({"_ipc_error":e.to_string()})),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (tempfile::TempDir, Hub, tokio::sync::mpsc::Receiver<Value>) {
        let dir = tempfile::tempdir().unwrap();
        let mut h = Hub::new(dir.path().to_owned()).unwrap();
        h.account_state = json!({"logged_in":true});
        let (backend, _, rx) = Backend::fixture();
        h.sessions.insert(
            "session".into(),
            Managed {
                backend,
                id: "session".into(),
                target: "device".into(),
                device_id: Some("device".into()),
                owners: HashSet::new(),
                ready: true,
                ended: false,
                closing: false,
                video_owners: HashSet::new(),
                phase: "ready".into(),
                events: Default::default(),
                snapshot: HashMap::new(),
                frame: None,
                shell: None,
                input_owner: None,
                input_until: Instant::now(),
                ports: HashMap::new(),
                created: now_ms(),
            },
        );
        (dir, h, rx)
    }
    #[test]
    fn gui_and_two_mcp_clients_reuse_connection_until_last_lease_leaves() {
        let (_dir, mut h, mut commands) = fixture();
        let gui = h.register("gui").unwrap();
        let a = h.register("mcp").unwrap();
        let b = h.register("mcp").unwrap();
        assert_eq!(
            h.open(&a, &json!({"device_id":"device"})).unwrap(),
            "session"
        );
        assert_eq!(
            h.open(&b, &json!({"device_id":"device"})).unwrap(),
            "session"
        );
        h.gui_command(&gui,&json!({"op":"connect","device_id":"device","generation":42,"settings":{"wait_for_video":true}})).unwrap();
        assert_eq!(h.sessions.len(), 1);
        assert_eq!(h.sessions["session"].owners.len(), 3);
        assert!(
            h.poll(&gui).unwrap()["events"]
                .as_array()
                .unwrap()
                .iter()
                .any(|e| e["event"] == "connected" && e["generation"] == 42)
        );
        while commands.try_recv().is_ok() {}
        h.release(&gui, "session");
        h.remove_client(&a);
        while let Ok(c) = commands.try_recv() {
            assert_ne!(c["op"], "disconnect");
        }
        assert_eq!(h.sessions["session"].owners.len(), 1);
        h.remove_client(&b);
        assert_eq!(commands.try_recv().unwrap()["op"], "disconnect");
    }
    #[test]
    fn client_exit_cancels_only_its_transfers_terminal_and_ports() {
        let (_dir, mut h, mut commands) = fixture();
        let a = h.register("mcp").unwrap();
        let b = h.register("mcp").unwrap();
        h.open(&a, &json!({"device_id":"device"})).unwrap();
        h.open(&b, &json!({"device_id":"device"})).unwrap();
        h.sessions.get_mut("session").unwrap().shell = Some(Shell::new(&a));
        h.sessions
            .get_mut("session")
            .unwrap()
            .ports
            .insert(22, (b.clone(), json!({})));
        h.jobs.insert(
            "job".into(),
            Job {
                owner: a.clone(),
                session: "session".into(),
                kind: "transfer".into(),
                state: "running".into(),
                result: Value::Null,
                error: None,
                cancelled: false,
                created: 0,
                updated: 0,
                transfer_ids: vec![1234],
            },
        );
        h.remove_client(&a);
        let commands: Vec<_> = std::iter::from_fn(|| commands.try_recv().ok()).collect();
        assert!(
            commands
                .iter()
                .any(|c| c["command"]["op"] == "transfer_cancel" && c["command"]["id"] == 1234)
        );
        assert!(
            commands
                .iter()
                .any(|c| c["command"]["op"] == "terminal" && c["command"]["action"] == "close")
        );
        assert!(
            !commands
                .iter()
                .any(|c| c["op"] == "disconnect" || c["command"]["op"] == "port_mapping")
        );
        assert!(h.sessions["session"].ports.contains_key(&22));
        assert!(h.jobs["job"].cancelled);
    }
    #[test]
    fn gui_release_keeps_mcp_video_requested() {
        let (_dir, mut h, mut commands) = fixture();
        let gui = h.register("gui").unwrap();
        let mcp = h.register("mcp").unwrap();
        h.open(&mcp, &json!({"device_id":"device"})).unwrap();
        h.video(&mcp, "session", true).unwrap();
        h.gui_command(&gui,&json!({"op":"connect","generation":1,"device_id":"device","settings":{"wait_for_video":true}})).unwrap();
        while commands.try_recv().is_ok() {}
        h.release(&gui, "session");
        let sent: Vec<_> = std::iter::from_fn(|| commands.try_recv().ok()).collect();
        assert!(!sent.iter().any(|c| c["op"] == "disconnect"
            || (c["command"]["op"] == "expect_video" && c["command"]["enabled"] == false)));
        assert!(h.sessions["session"].video_owners.contains(&mcp));
    }
    #[test]
    fn mcp_switch_removes_only_mcp_leases_and_persists() {
        let (dir, mut h, _commands) = fixture();
        let gui = h.register("gui").unwrap();
        let mcp = h.register("mcp").unwrap();
        h.open(&gui, &json!({"device_id":"device"})).unwrap();
        h.open(&mcp, &json!({"device_id":"device"})).unwrap();
        let shared = Arc::new(Mutex::new(h));
        crate::mcp_api::call(&shared, &gui, "uu_mcp_configure", &json!({"enabled":false})).unwrap();
        let mut h = shared.lock().unwrap();
        assert!(h.clients.contains_key(&gui));
        assert!(!h.clients.contains_key(&mcp));
        assert!(h.sessions["session"].owners.contains(&gui));
        assert!(h.register("mcp").is_err());
        assert_eq!(
            crate::preferences_store::read(&dir.path().join("mcp.json"))["enabled"],
            false
        );
    }
    #[test]
    fn different_targets_keep_independent_sessions() {
        let (_dir, mut h, _commands) = fixture();
        let (_other_dir, mut other, _other_commands) = fixture();
        let mut second = other.sessions.remove("session").unwrap();
        second.id = "session-two".into();
        second.target = "device-two".into();
        second.device_id = Some("device-two".into());
        h.sessions.insert(second.id.clone(), second);
        let client = h.register("mcp").unwrap();
        assert_eq!(
            h.open(&client, &json!({"device_id":"device"})).unwrap(),
            "session"
        );
        assert_eq!(
            h.open(&client, &json!({"device_id":"device-two"})).unwrap(),
            "session-two"
        );
        h.release(&client, "session");
        assert!(h.sessions["session"].closing);
        assert!(h.ready(&client, "session-two").is_ok());
    }
    #[test]
    fn journal_is_bounded_and_reports_cursor_expiration() {
        let mut events = Journal::default();
        for i in 0..1500 {
            events.push(json!({"n":i}));
        }
        assert!(events.rows.len() <= 1024);
        assert!(events.oldest() > 1);
        assert!(events.since(1499).len() == 1);
    }
}

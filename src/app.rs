#[path = "appearance.rs"]
mod appearance;
#[path = "fonts.rs"]
mod fonts;
#[path = "views.rs"]
mod views;
use crate::{
    Options,
    backend::{Backend, Device, Event},
    input::RemoteInput,
};
use base64::Engine;
use eframe::egui::{self, Color32, RichText, Vec2};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

const BLUE: Color32 = Color32::from_rgb(50, 128, 255);
const MUTED: Color32 = Color32::from_rgb(136, 138, 139);
const INK: Color32 = Color32::from_rgb(24, 25, 25);
const BG: Color32 = Color32::from_rgb(248, 251, 253);

pub struct Desktop {
    context: egui::Context,
    window_namespace: u64,
    other_sessions: Vec<Desktop>,
    ui_state: views::UiState,
    backend: Option<Backend>,
    options: Options,
    started: Instant,
    logged_in: bool,
    state_received: bool,
    nickname: String,
    devices: Vec<Device>,
    refreshing: bool,
    generation: u64,
    selected: Option<Device>,
    active_device: Option<Device>,
    connecting: bool,
    disconnecting: bool,
    connected: bool,
    video_wait_requested: bool,
    phase: String,
    status: String,
    error: Option<String>,
    login_open: bool,
    login_id: u64,
    qr: Option<egui::TextureHandle>,
    login_status: String,
    texture: Option<egui::TextureHandle>,
    dimensions: [usize; 2],
    frame_geometry: Option<crate::media_bus::Geometry>,
    frames: u64,
    last_frame: Option<Instant>,
    remote_input: RemoteInput,
    pending_input: Vec<egui::Event>,
    auto_connect_sent: bool,
    auto_login_sent: bool,
    fullscreen: bool,
    screenshot_requested: bool,
    diagnostic_capture: Option<(
        egui::ViewportId,
        PathBuf,
        std::sync::Arc<std::sync::atomic::AtomicBool>,
    )>,
    previews: std::collections::HashMap<String, egui::TextureHandle>,
    window_probe: views::session_windows::WindowProbe,
    display_metrics: crate::perf::DisplayMetrics,
    diagnostic_io: Option<crate::diagnostic_io::DiagnosticIo>,
}

fn setup(ctx: &egui::Context) {
    ctx.set_visuals(egui::Visuals::light());
    let mut style = (*ctx.global_style()).clone();
    style.spacing.item_spacing = Vec2::new(8.0, 4.0);
    style.spacing.button_padding = Vec2::new(12.0, 3.0);
    style.spacing.interact_size = Vec2::new(40.0, 24.0);
    style.visuals.override_text_color = Some(INK);
    style.visuals.interact_cursor = Some(egui::CursorIcon::PointingHand);
    style.visuals.selection.bg_fill = appearance::SELECTED;
    style.visuals.selection.stroke.color = BLUE;
    style.visuals.panel_fill = BG;
    style.visuals.window_fill = BG;
    style.visuals.window_corner_radius = egui::CornerRadius::same(5);
    style.visuals.window_stroke = egui::Stroke::new(1.0, appearance::BORDER);
    style.visuals.extreme_bg_color = Color32::WHITE;
    for widget in [
        &mut style.visuals.widgets.inactive,
        &mut style.visuals.widgets.hovered,
        &mut style.visuals.widgets.active,
        &mut style.visuals.widgets.open,
    ] {
        widget.corner_radius = egui::CornerRadius::same(3);
        widget.bg_stroke = egui::Stroke::new(1.0, appearance::BORDER);
        widget.fg_stroke.color = INK;
    }
    style.visuals.widgets.inactive.bg_fill = Color32::WHITE;
    style.visuals.widgets.inactive.weak_bg_fill = Color32::WHITE;
    style.visuals.widgets.hovered.bg_fill = Color32::from_rgb(240, 244, 247);
    style.visuals.widgets.hovered.weak_bg_fill = Color32::from_rgb(240, 244, 247);
    style.visuals.widgets.hovered.bg_stroke =
        egui::Stroke::new(1.0, Color32::from_rgb(172, 195, 218));
    style.visuals.widgets.active.bg_fill = appearance::SELECTED;
    style.visuals.widgets.active.weak_bg_fill = appearance::SELECTED;
    style.visuals.widgets.open.weak_bg_fill = appearance::SELECTED;
    style.visuals.widgets.noninteractive.bg_stroke.color = appearance::BORDER;
    style
        .text_styles
        .insert(egui::TextStyle::Body, egui::FontId::proportional(13.0));
    style
        .text_styles
        .insert(egui::TextStyle::Button, egui::FontId::proportional(13.0));
    style
        .text_styles
        .insert(egui::TextStyle::Heading, egui::FontId::proportional(24.0));
    ctx.set_global_style(style);
    fonts::setup(ctx);
}

impl Desktop {
    pub fn new(cc: &eframe::CreationContext<'_>, options: Options) -> Self {
        setup(&cc.egui_ctx);
        Self::from_context(&cc.egui_ctx, options)
    }
    fn from_context(ctx: &egui::Context, options: Options) -> Self {
        static NEXT_WINDOW_NAMESPACE: std::sync::atomic::AtomicU64 =
            std::sync::atomic::AtomicU64::new(0);
        let result = Backend::shared(options.state_dir.as_deref(), ctx.clone());
        let (backend, error) = match result {
            Ok(b) => (Some(b), None),
            Err(e) => (None, Some(e)),
        };
        let mut ui_state = views::UiState::load(options.state_dir.clone());
        ui_state.initial_page(options.view.as_deref());
        let diagnostic_io = options
            .diagnostic_stdin
            .then(|| crate::diagnostic_io::DiagnosticIo::start(ctx.clone()));
        Self {
            context: ctx.clone(),
            window_namespace: NEXT_WINDOW_NAMESPACE
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            other_sessions: Vec::new(),
            diagnostic_io,
            ui_state,
            backend,
            options,
            started: Instant::now(),
            logged_in: false,
            state_received: false,
            nickname: String::new(),
            devices: vec![],
            refreshing: false,
            generation: 0,
            selected: None,
            active_device: None,
            connecting: false,
            disconnecting: false,
            connected: false,
            video_wait_requested: false,
            phase: String::new(),
            status: "正在启动原生核心".into(),
            error,
            login_open: false,
            login_id: 0,
            qr: None,
            login_status: "用 UU 远程手机端扫码，在手机上确认登录".into(),
            texture: None,
            dimensions: [0, 0],
            frame_geometry: None,
            frames: 0,
            last_frame: None,
            remote_input: RemoteInput::default(),
            pending_input: vec![],
            auto_connect_sent: false,
            auto_login_sent: false,
            fullscreen: false,
            screenshot_requested: false,
            diagnostic_capture: None,
            previews: Default::default(),
            display_metrics: Default::default(),
            window_probe: Default::default(),
        }
    }

    fn viewport(&self, kind: &str) -> egui::ViewportId {
        if self.window_namespace == 0 {
            match kind {
                "remote" => crate::backend::remote_viewport_id(),
                "files" => crate::backend::files_viewport_id(),
                "terminal" => crate::backend::terminal_viewport_id(),
                _ => crate::backend::ports_viewport_id(),
            }
        } else {
            egui::ViewportId::from_hash_of(("uux-device", self.window_namespace, kind))
        }
    }
    fn activate_device_context(&mut self, device: &Device) -> bool {
        if self
            .active_device
            .as_ref()
            .is_some_and(|d| d.id == device.id)
            && (self.connected || self.connecting)
        {
            return true;
        }
        self.release();
        let mut others = std::mem::take(&mut self.other_sessions);
        let existing = others.iter().position(|d| {
            d.active_device.as_ref().is_some_and(|v| v.id == device.id)
                && (d.connected || d.connecting)
        });
        if existing.is_none() && !self.connected && !self.connecting {
            self.other_sessions = others;
            return false;
        }
        let mut next = if let Some(index) = existing {
            others.swap_remove(index)
        } else {
            let mut next = Self::from_context(
                &self.context,
                Options {
                    state_dir: self.options.state_dir.clone(),
                    ..Default::default()
                },
            );
            next.logged_in = self.logged_in;
            next.state_received = self.state_received;
            next.nickname = self.nickname.clone();
            next.devices = self.devices.clone();
            next
        };
        // Main-window/CLI ownership stays with the root. Device state, textures,
        // native viewports and its IPC client move together without reconnecting.
        std::mem::swap(&mut self.options, &mut next.options);
        std::mem::swap(&mut self.diagnostic_io, &mut next.diagnostic_io);
        std::mem::swap(&mut self.diagnostic_capture, &mut next.diagnostic_capture);
        std::mem::swap(&mut self.auto_connect_sent, &mut next.auto_connect_sent);
        std::mem::swap(&mut self.auto_login_sent, &mut next.auto_login_sent);
        std::mem::swap(
            &mut self.screenshot_requested,
            &mut next.screenshot_requested,
        );
        std::mem::swap(self, &mut next);
        others.push(next);
        self.other_sessions = others;
        self.selected = Some(device.clone());
        self.ui_state.select_device_page();
        existing.is_some()
    }
    fn send(&mut self, value: Value) -> bool {
        let Some(backend) = &self.backend else {
            return false;
        };
        match backend.send(value) {
            Ok(()) => true,
            Err(error) => {
                self.error = Some(error);
                // Dropping the backend closes the session and releases tracked
                // input. A dropped key-up must not leave control active.
                self.remote_input.release();
                self.backend = None;
                self.connected = false;
                self.connecting = false;
                self.refreshing = false;
                self.texture = None;
                self.status = "原生核心通信已停止，请重新打开应用".into();
                false
            }
        }
    }

    fn inputs(&mut self, values: Vec<Value>) {
        for mut command in values {
            if matches!(
                command["op"].as_str(),
                Some(
                    "move"
                        | "relative"
                        | "click"
                        | "button_down"
                        | "button_up"
                        | "scroll"
                        | "touch"
                )
            ) && let Some(geometry) = self.frame_geometry
            {
                command["geometry"] = json!(geometry);
            }
            if !self.send(json!({"op":"input","generation":self.generation,"command":command})) {
                break;
            }
        }
    }

    fn release(&mut self) {
        let commands = self.remote_input.release();
        self.inputs(commands);
    }

    fn refresh(&mut self) {
        self.refreshing = true;
        self.send(json!({"op":"devices"}));
    }

    fn preview(&mut self, device: &Device) {
        if !self.previews.contains_key(&device.id)
            && let Some(url) = &device.wallpaper
        {
            self.send(json!({"op":"preview","device_id":device.id,"url":url}));
        }
    }

    fn login(&mut self) {
        if self.backend.is_none() {
            return;
        }
        self.release();
        self.login_id += 1;
        self.login_open = true;
        self.qr = None;
        self.login_status = "正在生成二维码".into();
        self.error = None;
        self.send(json!({"op":"login","login_id":self.login_id}));
    }

    fn connect(&mut self, device: Device) {
        self.connect_with_video(device, true);
    }
    fn connect_for_tools(&mut self, device: Device) {
        self.connect_with_video(device, false);
    }
    fn connect_with_video(&mut self, device: Device, wait_for_video: bool) {
        if self.activate_device_context(&device) {
            if wait_for_video {
                self.show_remote_window();
            }
            return;
        }
        if self.backend.is_none() {
            return;
        }
        self.release();
        self.ui_state.normal_connection();
        if self.login_open {
            self.send(json!({"op":"cancel_login"}));
            self.login_open = false;
            self.qr = None;
        }
        self.error = None;
        self.generation += 1;
        let mut settings = self.ui_state.settings_for_device(&device.id);
        settings["wait_for_video"] = json!(wait_for_video);
        if !self.send(json!({"op":"connect","device_id":device.id,"generation":self.generation,"settings":settings})) {
            return;
        }
        self.ui_state.windows.remote = wait_for_video;
        self.video_wait_requested = false;
        self.active_device = Some(device.clone());
        self.selected = Some(device);
        self.texture = None;
        self.frames = 0;
        self.last_frame = None;
        self.connected = false;
        self.connecting = true;
        self.phase = "正在申请连接".into();
    }

    fn disconnect(&mut self) {
        if self.disconnecting {
            return;
        }
        self.disconnecting = true;
        self.ui_state.windows = Default::default();
        self.release();
        self.send(json!({"op":"disconnect"}));
        self.connecting = true;
        self.phase = "正在结束远控".into();
        self.connected = false;
        self.status = "正在释放远端会话".into();
    }

    fn receive(&mut self, ctx: &egui::Context) {
        let _scope = crate::perf::UiScope::new("receive");
        let events: Vec<Event> = self
            .backend
            .as_ref()
            .map(|b| b.events.try_iter().collect())
            .unwrap_or_default();
        for event in events {
            if self.options.diagnostic_stdin {
                println!("{}", serde_json::to_string(&event).unwrap());
            }
            match event {
                Event::Native { kind, data }
                    if data["generation"]
                        .as_u64()
                        .is_none_or(|g| g == self.generation) =>
                {
                    self.native_event(&kind, data)
                }
                Event::Hello { protocol } => {
                    if protocol != 1 {
                        self.error = Some("原生核心接口版本不匹配".into());
                    }
                    self.status = "原生核心已就绪".into();
                }
                Event::State {
                    logged_in,
                    nickname,
                } => {
                    let refresh = logged_in && (!self.state_received || !self.logged_in);
                    self.state_received = true;
                    self.logged_in = logged_in;
                    self.nickname = nickname;
                    if refresh {
                        self.refresh();
                    }
                    if !logged_in {
                        self.devices.clear();
                        self.previews.clear();
                        self.status = "请扫码登录".into();
                    }
                }
                Event::Devices { devices } if self.logged_in => {
                    if self.options.window_test.is_some()
                        && let Some(id) = &self.options.connect
                    {
                        eprintln!(
                            "窗口测试：设备列表已返回，目标设备{}",
                            match devices.iter().find(|d| &d.id == id) {
                                Some(d) if d.online => "可连接",
                                Some(_) => "暂不可连接，等待服务器确认连接请求",
                                None => "未找到",
                            }
                        );
                    }
                    if self.selected.is_none()
                        && matches!(
                            self.options.view.as_deref(),
                            Some("device" | "rename" | "ports")
                        )
                    {
                        self.selected = devices
                            .iter()
                            .find(|d| d.online)
                            .or(devices.first())
                            .cloned();
                        self.prepare_tool_preview();
                    }
                    self.devices = devices;
                    if let Some(device) = self.selected.clone() {
                        self.preview(&device);
                    }
                    self.refreshing = false;
                    self.status = "设备列表已更新".into();
                }
                Event::Preview { device_id, image }
                    if self.logged_in && self.devices.iter().any(|d| d.id == device_id) =>
                {
                    if let Ok(image) = image::load_from_memory(&image) {
                        let image = image.to_rgba8();
                        self.previews.insert(
                            device_id,
                            ctx.load_texture(
                                "device-preview",
                                egui::ColorImage::from_rgba_unmultiplied(
                                    [image.width() as usize, image.height() as usize],
                                    image.as_raw(),
                                ),
                                egui::TextureOptions::LINEAR,
                            ),
                        );
                    }
                }
                Event::Qr { login_id, png } if login_id == self.login_id && self.login_open => {
                    let decoded = base64::engine::general_purpose::STANDARD
                        .decode(png)
                        .ok()
                        .and_then(|bytes| image::load_from_memory(&bytes).ok());
                    if let Some(image) = decoded {
                        let image = image.into_rgba8();
                        self.qr = Some(ctx.load_texture(
                            "login-qr",
                            egui::ColorImage::from_rgba_unmultiplied(
                                [image.width() as usize, image.height() as usize],
                                image.as_raw(),
                            ),
                            egui::TextureOptions::NEAREST,
                        ));
                        self.login_status = "打开 UU 远程手机端扫码".into();
                    } else {
                        self.error = Some("二维码图片无法解码，请刷新".into());
                    }
                }
                Event::LoginStatus { login_id, status } if login_id == self.login_id => {
                    self.login_status = match status {
                        1 => "等待扫码",
                        2 => "已扫码，请在手机上确认",
                        3 => "登录已取消",
                        4 => "已确认，正在登录",
                        _ => "等待登录",
                    }
                    .into();
                }
                Event::LoginComplete { login_id } if login_id == self.login_id => {
                    self.login_open = false;
                    self.qr = None;
                    self.status = "登录成功".into();
                }
                Event::Connecting { generation } if generation == self.generation => {
                    self.connecting = true
                }
                Event::Phase { generation, phase } if generation == self.generation => {
                    self.phase = match phase.as_str() {
                        "room_join_started" | "room_joined" => "正在加入房间",
                        "signaling_connected" => "正在请求远端连接",
                        "room_info" => "正在加入房间",
                        "control_accepted" => "远端已接受连接",
                        "offer_sent" => "正在协商画面",
                        "answer_received" => "正在建立安全连接",
                        "webrtc_connected" => "已连接，等待画面",
                        "control_channel_open" => "键鼠通道已就绪",
                        "protobuf_ready" => "已连接，等待远端画面",
                        "startup_capture_stopped" | "startup_capture_restarted" => {
                            "正在恢复远端画面"
                        }
                        "deferred_capture_requested" => "已连接，等待远端画面",
                        "session_ready" => "连接已建立",
                        "first_frame" => "画面已就绪",
                        "releasing" => "正在结束远控",
                        _ => "正在连接",
                    }
                    .into();
                }
                Event::Connected { generation } if generation == self.generation => {
                    self.connected = true;
                    self.ui_state.connected();
                    self.start_terminal();
                    self.send_pending_power();
                    if self.options.files || self.ui_state.windows.files {
                        self.open_files();
                    }
                    self.connecting = false;
                    self.status = "已连接".into();
                }
                Event::Disconnected { generation } if generation == self.generation => {
                    self.release();
                    self.connected = false;
                    self.connecting = false;
                    self.texture = None;
                    self.status = "已断开连接".into();
                    self.ui_state.end_session();
                    self.disconnecting = false;
                    self.active_device = None;
                }
                Event::Error {
                    scope,
                    message,
                    generation,
                    login_id,
                } => {
                    if generation.is_some_and(|g| g != self.generation)
                        || login_id.is_some_and(|id| id != self.login_id)
                    {
                        continue;
                    }
                    if self.options.window_test.is_some() {
                        eprintln!("窗口测试错误（{scope}）：{message}");
                    }
                    if scope == "devices" {
                        self.refreshing = false;
                    }
                    if scope == "connection" || scope == "bridge" {
                        self.release();
                        self.connected = false;
                        self.connecting = false;
                        self.texture = None;
                    }
                    if scope == "bridge" {
                        self.backend = None;
                        self.refreshing = false;
                        self.status = "原生核心已停止".into();
                    }
                    if scope == "login" {
                        self.qr = None;
                        self.login_status = "请刷新二维码重试".into();
                    }
                    self.error = Some(message);
                }
                _ => {}
            }
        }
        let frame = self
            .backend
            .as_ref()
            .and_then(|b| b.latest.lock().unwrap().take());
        if let Some(diagnostics) = &mut self.diagnostic_io {
            diagnostics.observe_frame(frame.as_ref());
        }
        if let Some(frame) = frame
            && frame.generation == self.generation
            && self.connected
        {
            if let Some(diagnostics) = &mut self.diagnostic_io {
                diagnostics.snapshot(&frame, self.options.screenshot.as_ref());
            }
            self.display_metrics
                .decoded_to_ui
                .record(frame.decoded_at.elapsed());
            self.display_metrics
                .assembled_to_ui
                .record(frame.assembled_at.elapsed());
            let upload_started = Instant::now();
            let image =
                egui::ColorImage::from_rgba_unmultiplied([frame.width, frame.height], &frame.rgba);
            if let Some(texture) = &mut self.texture {
                texture.set(image, egui::TextureOptions::LINEAR);
            } else {
                self.texture =
                    Some(ctx.load_texture("remote", image, egui::TextureOptions::LINEAR));
            }
            self.display_metrics.upload.record(upload_started.elapsed());
            if self.dimensions != [frame.width, frame.height] {
                crate::perf::trace(
                    "video_ui_size",
                    json!({
                        "generation":frame.generation,"width":frame.width,"height":frame.height,
                        "assembled_to_ui_ms":frame.assembled_at.elapsed().as_secs_f64()*1000.0,
                        "decoded_to_ui_ms":frame.decoded_at.elapsed().as_secs_f64()*1000.0
                    }),
                );
            }
            let remote_id = self.viewport("remote");
            if self.ui_state.windows.remote
                && ctx.input(|input| input.raw.viewports.contains_key(&remote_id))
            {
                ctx.request_repaint_of(remote_id);
            }
            self.frames = frame.sequence;
            self.dimensions = [frame.width, frame.height];
            self.frame_geometry = Some(frame.geometry);
            self.capture_frame_observed();
            self.last_frame = Some(Instant::now());
        }
        self.display_metrics.report(
            self.generation,
            self.ui_state.windows.remote,
            self.ui_state.windows.files,
        );
        if self.options.login && self.state_received && !self.auto_login_sent {
            self.auto_login_sent = true;
            self.login();
        }
        if !self.auto_connect_sent
            && !self.options.login
            && let Some(id) = &self.options.connect
            && let Some(device) = self
                .devices
                .iter()
                // An explicit target should reach the authoritative join API.
                // The list's controllable flag can lag behind a recent release.
                .find(|d| &d.id == id)
                .cloned()
        {
            self.auto_connect_sent = true;
            if self.options.files && self.options.window_test.is_none() {
                self.connect_for_tools(device);
                self.ui_state.windows.files = true;
            } else {
                self.connect(device);
            }
        }
    }

    fn login_view(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space((ui.available_height() * 0.12).max(30.0));
                    ui.heading("扫码登录");
                    ui.label(RichText::new("连接你的设备，从这里开始").color(MUTED));
                    ui.add_space(18.0);
                    egui::Frame::new()
                        .fill(Color32::WHITE)
                        .inner_margin(16)
                        .corner_radius(5)
                        .stroke(egui::Stroke::new(1.0, Color32::from_rgb(224, 230, 239)))
                        .show(ui, |ui| {
                            if let Some(qr) = &self.qr {
                                ui.allocate_ui_with_layout(
                                    Vec2::splat(232.0),
                                    egui::Layout::centered_and_justified(egui::Direction::TopDown),
                                    |ui| {
                                        ui.add(
                                            egui::Image::new(qr)
                                                .fit_to_exact_size(Vec2::splat(232.0)),
                                        );
                                    },
                                );
                            } else {
                                ui.allocate_ui_with_layout(
                                    Vec2::splat(232.0),
                                    egui::Layout::centered_and_justified(egui::Direction::TopDown),
                                    |ui| {
                                        if self.login_open && self.error.is_none() {
                                            ui.spinner();
                                        } else {
                                            ui.label(RichText::new("UUX").size(28.0).color(BLUE));
                                        }
                                    },
                                );
                            }
                        });
                    ui.add_space(12.0);
                    ui.label(&self.login_status);
                    ui.add_space(8.0);
                    if ui
                        .add_enabled_ui(self.backend.is_some(), |ui| {
                            appearance::primary(
                                ui,
                                if self.login_open {
                                    "刷新二维码"
                                } else {
                                    "生成二维码"
                                },
                                Vec2::new(112.0, 30.0),
                            )
                        })
                        .inner
                        .clicked()
                    {
                        self.login();
                    }
                    if self.logged_in && ui.button("返回设备列表").clicked() {
                        self.send(json!({"op":"cancel_login"}));
                        self.login_open = false;
                        self.qr = None;
                    }
                });
            });
    }

    fn capture_window(&mut self, ctx: &egui::Context) {
        let screenshot_viewport = if self.options.view.as_deref() == Some("ports") {
            self.viewport("ports")
        } else if self.options.connect.is_some() {
            if self.options.files {
                self.viewport("files")
            } else {
                self.viewport("remote")
            }
        } else {
            egui::ViewportId::ROOT
        };
        let ready = if self.error.is_some() {
            true
        } else if self.options.login {
            self.qr.is_some()
        } else if self.options.connect.is_some() {
            self.texture.is_some() && self.frames > 2
        } else {
            self.state_received && !self.refreshing
        };
        let automatic_capture = ready
            && !self.options.diagnostic_stdin
            && ctx.viewport_id() == screenshot_viewport
            && self.started.elapsed() > Duration::from_secs(3)
            && !self.screenshot_requested;
        if self
            .diagnostic_capture
            .as_ref()
            .is_some_and(|(_, _, done)| done.load(std::sync::atomic::Ordering::Relaxed))
        {
            self.diagnostic_capture = None;
        }
        let manual_capture = self
            .diagnostic_capture
            .as_ref()
            .is_some_and(|(target, _, _)| *target == ctx.viewport_id());
        let capture_path = if manual_capture {
            self.diagnostic_capture
                .as_ref()
                .map(|(_, path, done)| (path.clone(), done.clone()))
        } else if automatic_capture {
            self.options.screenshot.clone().map(|path| {
                (
                    path,
                    std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
                )
            })
        } else {
            None
        };
        if let Some((path, captured)) = capture_path {
            self.screenshot_requested = true;
            // eframe 0.37 does not deliver Screenshot callbacks for immediate
            // viewports. Capture our framebuffer after this viewport is painted.
            ctx.layer_painter(egui::LayerId::new(
                egui::Order::Debug,
                egui::Id::unique("diagnostic-capture"),
            ))
            .add(egui::PaintCallback {
                rect: ctx.viewport_rect(),
                callback: std::sync::Arc::new(eframe::egui_glow::CallbackFn::new(
                    move |info, painter| {
                        if captured.swap(true, std::sync::atomic::Ordering::Relaxed) {
                            return;
                        }
                        let pixels = painter.read_screen_rgba(info.screen_size_px);
                        let rgba = pixels
                            .pixels
                            .iter()
                            .flat_map(|p| p.to_array())
                            .collect::<Vec<_>>();
                        if let Some(parent) = path.parent() {
                            let _ = std::fs::create_dir_all(parent);
                        }
                        match image::save_buffer_with_format(
                            &path,
                            &rgba,
                            pixels.size[0] as u32,
                            pixels.size[1] as u32,
                            image::ColorType::Rgba8,
                            image::ImageFormat::Png,
                        ) {
                            Ok(_) => eprintln!("窗口截图已保存：{}", path.display()),
                            Err(e) => eprintln!("窗口截图保存失败：{e}"),
                        }
                    },
                )),
            });
        }
    }

    fn diagnostics(&mut self, ctx: &egui::Context) {
        if self
            .options
            .quit_after
            .is_some_and(|time| self.started.elapsed() >= time)
        {
            self.window_probe_timeout();
            ctx.send_viewport_cmd_to(egui::ViewportId::ROOT, egui::ViewportCommand::Close);
        }
    }
}

impl eframe::App for Desktop {
    fn raw_input_hook(&mut self, ctx: &egui::Context, input: &mut egui::RawInput) {
        let _scope = crate::perf::UiScope::new("raw_input");
        if input.focused {
            for session in &mut self.other_sessions {
                session.release();
            }
            let values = self.remote_input.suspend();
            self.inputs(values);
        }
        Self::local_clipboard_events(&mut input.events);
        if self.options.diagnostic_stdin && !input.dropped_files.is_empty() {
            println!(
                "{}",
                json!({"event":"ui_dropped_files","paths":input.dropped_files.iter().map(|f|f.path()).collect::<Vec<_>>()})
            );
        }
        let _ = ctx;
    }

    fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        if let Some(token) =
            ctx.data_mut(|d| d.remove_temp::<String>(egui::Id::unique("instance_activation")))
        {
            frame.activate_with_token(token);
        }
        if self.options.diagnostic_stdin
            && ctx.data(|d| {
                d.get_temp::<bool>(egui::Id::unique("activation_probe"))
                    .unwrap_or(false)
            })
            && let Some(token) = frame.take_activation_token()
        {
            ctx.data_mut(|d| d.remove::<bool>(egui::Id::unique("activation_probe")));
            println!("{}", json!({"event":"activation_token","token":token}));
        }
        let _scope = crate::perf::UiScope::new("logic");
        crate::perf::trace_focus(ctx, "root");
        let diagnostic_commands: Vec<_> = self
            .diagnostic_io
            .as_ref()
            .map(|io| io.commands.try_iter().collect())
            .unwrap_or_default();
        for command in diagnostic_commands {
            match command["op"].as_str() {
                Some("input") => {
                    if let Some(diagnostics) = &mut self.diagnostic_io {
                        diagnostics.observe_command(&command);
                    }
                    self.send(command);
                }
                Some("quit") => self.options.quit_after = Some(Duration::ZERO),
                Some("disconnect") => self.disconnect(),
                Some("activation_probe") => {
                    ctx.data_mut(|d| d.insert_temp(egui::Id::unique("activation_probe"), true));
                    frame.request_activation_token();
                }
                Some("ui_state") => {
                    let target = match command["target"].as_str() {
                        Some("remote") => self.viewport("remote"),
                        Some("files") => self.viewport("files"),
                        _ => egui::ViewportId::ROOT,
                    };
                    let (focused, minimized) = ctx.input(|i| {
                        i.raw
                            .viewports
                            .get(&target)
                            .map(|v| (v.focused, v.minimized))
                            .unwrap_or_default()
                    });
                    println!(
                        "{}",
                        json!({"event":"ui_state","target":command["target"],"id":command["id"],"focused":focused,"minimized":minimized})
                    );
                }
                Some("ui_ping") => {
                    println!("{}", json!({"event":"ui_pong","id":command["id"]}));
                }
                Some("ui_capture") => {
                    // Explicit stdin diagnostics only; this captures our own framebuffer.
                    let target = match command["target"].as_str() {
                        Some("remote") => self.viewport("remote"),
                        Some("files") => self.viewport("files"),
                        Some("ports") => self.viewport("ports"),
                        Some("terminal") => self.viewport("terminal"),
                        _ => egui::ViewportId::ROOT,
                    };
                    if let Some(path) = command["path"].as_str() {
                        self.diagnostic_capture = Some((
                            target,
                            PathBuf::from(path),
                            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
                        ));
                        ctx.request_repaint_of(target);
                    }
                }
                Some("ui_dialog") => {
                    if let Some(target) =
                        self.diagnostic_dialog(command["name"].as_str().unwrap_or(""))
                    {
                        ctx.request_repaint_of(target);
                    }
                }
                Some("open_remote") if self.connected => {
                    self.show_remote_window();
                }
                Some("viewport") => {
                    let target = match command["target"].as_str() {
                        Some("root") => egui::ViewportId::ROOT,
                        Some("remote") => self.viewport("remote"),
                        Some("files") => self.viewport("files"),
                        _ => continue,
                    };
                    if !ctx.input(|i| i.raw.viewports.contains_key(&target)) {
                        continue;
                    }
                    let action = match command["action"].as_str() {
                        Some("focus") => egui::ViewportCommand::Focus,
                        Some("minimize") => egui::ViewportCommand::Minimized(true),
                        Some("restore") => egui::ViewportCommand::Minimized(false),
                        Some("resize") => {
                            let width = command["width"]
                                .as_f64()
                                .unwrap_or(920.0)
                                .clamp(640.0, 2560.0)
                                as f32;
                            let height = command["height"]
                                .as_f64()
                                .unwrap_or(680.0)
                                .clamp(400.0, 1440.0)
                                as f32;
                            egui::ViewportCommand::InnerSize(Vec2::new(width, height))
                        }
                        _ => continue,
                    };
                    ctx.send_viewport_cmd_to(target, action);
                    crate::perf::trace(
                        "ui_viewport_command",
                        json!({"target":command["target"],"action":command["action"]}),
                    );
                }
                _ => {}
            }
        }
        self.main_window_close(ctx);
        self.receive(ctx);
        for session in &mut self.other_sessions {
            session.receive(ctx);
        }
        let mut retained = Vec::new();
        for session in std::mem::take(&mut self.other_sessions) {
            if !session.connected
                && !session.connecting
                && !session.disconnecting
                && session.ui_state.can_retire()
            {
                std::thread::spawn(move || drop(session));
            } else {
                retained.push(session);
            }
        }
        self.other_sessions = retained;
        self.diagnostics(ctx);
        ctx.request_repaint_after_for(Duration::from_millis(100), egui::ViewportId::ROOT);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _: &mut eframe::Frame) {
        let _scope = crate::perf::UiScope::new("root_ui");
        self.render(ui);
        self.capture_window(ui.ctx());
    }

    fn on_exit(&mut self, _: Option<&eframe::glow::Context>) {
        for session in &mut self.other_sessions {
            session.release();
            if let Some(backend) = &mut session.backend {
                backend.shutdown();
            }
        }
        self.release();
        if let Some(backend) = &mut self.backend {
            backend.shutdown();
            if self.options.diagnostic_stdin {
                for event in backend.events.try_iter() {
                    if matches!(&event, Event::Disconnected { .. })
                        || matches!(&event, Event::Native { kind, .. } if kind == "released")
                    {
                        println!("{}", serde_json::to_string(&event).unwrap());
                    }
                }
            }
        }
    }
}

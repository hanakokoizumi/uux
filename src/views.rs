#[path = "device_tools.rs"]
mod device_tools;
#[path = "favorites.rs"]
mod favorites;
#[path = "file_selection.rs"]
mod file_selection;
#[path = "file_view.rs"]
mod file_view;
#[path = "ports_view.rs"]
mod ports_view;
#[path = "session_windows.rs"]
pub(super) mod session_windows;
#[path = "settings_view.rs"]
mod settings_view;
#[path = "terminal_view.rs"]
mod terminal_view;
#[path = "transfer_progress.rs"]
mod transfer_progress;
#[path = "window_resize.rs"]
mod window_resize;
use super::*;
use serde::{Deserialize, Serialize};
use session_windows::SessionWindows;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};

use super::appearance::{self as art, BORDER, Icon, SIDE};

#[derive(Default, PartialEq)]
enum Page {
    #[default]
    All,
    Device,
    Favorites,
    Assistance,
    Settings,
}

#[derive(Serialize, Deserialize)]
#[serde(default)]
struct Preferences {
    favorites: BTreeSet<String>,
    favorite_devices: Vec<favorites::FavoriteDevice>,
    port_rules: Vec<crate::core::ports::RuleConfig>,
    screen_preferences: BTreeMap<String, bool>,
    fps: i32,
    quality: i32,
    bitrate: i32,
    audio: bool,
    clipboard: bool,
    ime: bool,
    download: String,
    assist_history: Vec<String>,
    keep_awake: bool,
    drag_send: bool,
    drag_receive: bool,
    close_minimizes: bool,
    allow_files: bool,
    read_only: bool,
    lock_on_disconnect: bool,
    scale_up: bool,
    release_shortcut: String,
    fullscreen_shortcut: String,
    local_shortcuts: Vec<String>,
    relay_only: bool,
    proxy: crate::core::cloud::ProxySettings,
}
impl Default for Preferences {
    fn default() -> Self {
        Self {
            favorites: Default::default(),
            favorite_devices: Vec::new(),
            port_rules: Vec::new(),
            screen_preferences: BTreeMap::new(),
            fps: 60,
            quality: 2,
            bitrate: 8_000_000,
            audio: true,
            clipboard: true,
            ime: true,
            download: std::env::var("HOME")
                .map(|h| format!("{h}/Downloads"))
                .unwrap_or_else(|_| ".".into()),
            assist_history: vec![],
            keep_awake: false,
            drag_send: true,
            drag_receive: true,
            close_minimizes: false,
            allow_files: true,
            read_only: false,
            lock_on_disconnect: false,
            scale_up: true,
            release_shortcut: "Ctrl+Alt+G".into(),
            fullscreen_shortcut: "Ctrl+Alt+F".into(),
            local_shortcuts: vec![],
            relay_only: false,
            proxy: Default::default(),
        }
    }
}

pub(super) struct UiState {
    prefs: Preferences,
    saved_prefs: std::cell::RefCell<Value>,
    mcp_status: Value,
    mcp_poll: Instant,
    mcp_notice: String,
    path: PathBuf,
    page: Page,
    pub windows: SessionWindows,
    remote_resize: window_resize::ResizeGrip,
    resize_captured: bool,
    device_tools: device_tools::DeviceTools,
    terminal: terminal_view::TerminalView,
    ports: ports_view::PortsView,
    files_allowed: bool,
    text_window: bool,
    text: String,
    stats: Value,
    screens: Vec<Value>,
    screen: i32,
    resolution: [i32; 2],
    capture: crate::capture_state::CaptureState,
    remote_action: Option<(i32, String)>,
    super_active: bool,
    super_pending: bool,
    super_confirm: Option<bool>,
    screen_choice_pending: Option<bool>,
    remote_path: String,
    local_path: String,
    entries: Vec<Value>,
    remote_selected: file_selection::Selection,
    local_selected: file_selection::Selection,
    local_address: String,
    remote_address: String,
    local_history: Vec<String>,
    remote_history: Vec<String>,
    remote_back: bool,
    remote_loading: bool,
    file_focus_remote: bool,
    remote_hovered: bool,
    devices_collapsed: bool,
    assistance_collapsed: bool,
    assist_prompt: bool,
    favorite_draft: Option<favorites::FavoriteDraft>,
    transfers: Vec<Value>,
    transfer_rates: transfer_progress::TransferRates,
    setting_tab: usize,
    last_notice: String,
    file_dialog: Option<(String, String)>,
    file_name: String,
    directory_request: i64,
    add_help: bool,
    assist_id: String,
    assist_code: String,
    active_assist: Option<String>,
    autostart: bool,
    shortcut_capture: Option<usize>,
    network_notice: String,
    proxy_draft: crate::core::cloud::ProxySettings,
    proxy_port: String,
    local_cache: file_view::LocalCache,
}
impl UiState {
    pub(super) fn normal_connection(&mut self) {
        self.active_assist = None;
    }
    pub(super) fn initial_page(&mut self, page: Option<&str>) {
        match page {
            Some("device") => self.page = Page::Device,
            Some("favorites") => self.page = Page::Favorites,
            Some("favorite-add") => {
                self.page = Page::Favorites;
                self.favorite_draft = Some(Default::default());
            }
            Some("assistance") => self.page = Page::Assistance,
            Some("settings" | "security" | "keyboard" | "network" | "picture" | "mcp") => {
                self.page = Page::Settings;
                self.setting_tab = match page {
                    Some("keyboard") => 2,
                    Some("security") => 1,
                    Some("network") => 3,
                    Some("picture") => 4,
                    Some("mcp") => 5,
                    _ => 0,
                };
            }
            _ => {}
        }
    }
    pub(super) fn load(state: Option<PathBuf>) -> Self {
        let path = state
            .unwrap_or_else(crate::core::cloud::state_directory)
            .join("desktop.json");
        let prefs: Preferences = std::fs::read(&path)
            .ok()
            .and_then(|v| serde_json::from_slice(&v).ok())
            .unwrap_or_default();
        Self {
            proxy_port: prefs.proxy.port.to_string(),
            proxy_draft: prefs.proxy.clone(),
            local_cache: Default::default(),
            local_path: prefs.download.clone(),
            local_address: prefs.download.clone(),
            saved_prefs: std::cell::RefCell::new(json!(prefs)),
            mcp_status: Value::Null,
            mcp_poll: Instant::now() - Duration::from_secs(10),
            mcp_notice: String::new(),
            prefs,
            path,
            page: Page::All,
            windows: SessionWindows::default(),
            remote_resize: Default::default(),
            resize_captured: false,
            device_tools: Default::default(),
            terminal: Default::default(),
            ports: Default::default(),
            files_allowed: true,
            text_window: false,
            text: String::new(),
            stats: Value::Null,
            screens: vec![],
            screen: 0,
            resolution: [0, 0],
            capture: Default::default(),
            remote_action: None,
            super_active: false,
            super_pending: false,
            super_confirm: None,
            screen_choice_pending: None,
            remote_path: "C:\\".into(),
            entries: vec![],
            remote_selected: Default::default(),
            local_selected: Default::default(),
            remote_address: "C:\\".into(),
            local_history: vec![],
            remote_history: vec![],
            remote_back: false,
            remote_loading: false,
            file_focus_remote: false,
            remote_hovered: false,
            devices_collapsed: false,
            assistance_collapsed: false,
            assist_prompt: false,
            favorite_draft: None,
            transfers: vec![],
            transfer_rates: Default::default(),
            setting_tab: 0,
            last_notice: String::new(),
            file_dialog: None,
            file_name: String::new(),
            directory_request: 100,
            add_help: false,
            assist_id: String::new(),
            assist_code: String::new(),
            active_assist: None,
            autostart: crate::platform::autostart_enabled(),
            shortcut_capture: None,
            network_notice: String::new(),
        }
    }
    fn save(&self) {
        let current = json!(self.prefs);
        if crate::preferences_store::merge(&self.path, &self.saved_prefs.borrow(), &current).is_ok()
        {
            *self.saved_prefs.borrow_mut() = current;
        }
    }
    pub(super) fn end_session(&mut self) {
        self.windows.ports = false;
        self.ports = Default::default();
        self.windows.terminal = false;
        self.terminal = Default::default();
        if self
            .device_tools
            .pending
            .as_ref()
            .is_some_and(|p| p.temporary_session)
        {
            self.device_tools.pending = None;
            self.device_tools.error = Some("远端连接已结束，请确认设备状态".into());
        }
        self.capture.clear();
        if self.active_assist.take().is_some() {
            self.page = Page::Assistance;
        }
        self.windows.remote = false;
        self.windows.files = false;
        self.text_window = false;
        self.file_dialog = None;
        self.remote_action = None;
        self.super_active = false;
        self.super_pending = false;
        self.super_confirm = None;
        self.screen_choice_pending = None;
        self.screens.clear();
        self.resolution = [0, 0];
        self.entries.clear();
        self.remote_selected.clear();
        self.remote_loading = false;
        self.remote_history.clear();
        self.remote_hovered = false;
    }
    pub(super) fn settings(&self) -> Value {
        json!({"fps":self.prefs.fps,"quality":self.prefs.quality,
            "bitrate":if self.prefs.quality==6 {self.prefs.bitrate} else {0},
            "audio":self.prefs.audio,"clipboard":self.prefs.clipboard,
            "drag_receive":self.prefs.drag_receive,"download_dir":self.prefs.download,
            "relay_only":self.prefs.relay_only,
            "keep_awake":self.prefs.keep_awake,"allow_files":self.prefs.allow_files,
            "read_only":self.prefs.read_only,"lock_on_disconnect":self.prefs.lock_on_disconnect})
    }
    pub(super) fn settings_for_device(&self, device: &str) -> Value {
        let mut settings = self.settings();
        if let Some(enabled) = self.prefs.screen_preferences.get(device) {
            settings["super_screen"] = json!(enabled);
        }
        settings
    }
    fn observe_screen_choice(&mut self, device: Option<&str>, phase: &str, active: bool) {
        if phase == "ready"
            && self.screen_choice_pending == Some(active)
            && let Some(device) = device
        {
            self.prefs.screen_preferences.insert(device.into(), active);
            self.screen_choice_pending = None;
            self.save();
        } else if matches!(phase, "failed" | "timeout") {
            self.screen_choice_pending = None;
        }
    }
    pub(super) fn connected(&mut self) {
        if let Some(id) = &self.active_assist {
            self.prefs.assist_history.retain(|old| old != id);
            self.prefs.assist_history.insert(0, id.clone());
            self.prefs.assist_history.truncate(12);
            self.save();
        }
    }
}
fn card() -> egui::Frame {
    egui::Frame::new()
        .fill(Color32::WHITE)
        .corner_radius(3)
        .stroke(egui::Stroke::new(1.0, BORDER))
        .inner_margin(16)
}
fn singleline(text: &mut String) -> egui::TextEdit<'_> {
    egui::TextEdit::singleline(text).align(egui::Align2::LEFT_CENTER)
}
fn dialog(title: &str) -> egui::Window<'static> {
    egui::Window::new(RichText::new(title.to_owned()).size(14.0))
        .collapsible(false)
        .resizable(false)
        .default_width(360.0)
        .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
        .frame(
            egui::Frame::new()
                .fill(Color32::WHITE)
                .corner_radius(5)
                .stroke(egui::Stroke::new(1.0, BORDER))
                .inner_margin(16),
        )
        .title_frame(egui::Frame::new().fill(SIDE).inner_margin(8))
}
fn primary(ui: &mut egui::Ui, label: &str) -> egui::Response {
    art::primary(ui, label, Vec2::new(112.0, 30.0))
}
fn windows_icon(p: &egui::Painter, r: egui::Rect, online: bool) {
    p.rect_filled(
        r,
        4,
        if online {
            BLUE
        } else {
            Color32::from_rgb(174, 176, 182)
        },
    );
    let unit = r.width() / 10.0;
    for y in 0..2 {
        for x in 0..2 {
            p.rect_filled(
                egui::Rect::from_min_size(
                    r.min + Vec2::new(2.0 + x as f32 * 3.3, 2.0 + y as f32 * 3.3) * unit,
                    Vec2::splat(2.7 * unit),
                ),
                0,
                Color32::WHITE,
            );
        }
    }
}
fn nav_background(ui: &egui::Ui, r: egui::Rect, response: &egui::Response, selected: bool) {
    if selected || response.hovered() {
        ui.painter()
            .rect_filled(r.shrink2(Vec2::new(4.0, 2.0)), 4, art::SELECTED);
    }
    art::hover_background(ui, response, r.shrink2(Vec2::new(4.0, 2.0)));
    if selected {
        ui.painter().rect_filled(
            egui::Rect::from_center_size(
                egui::pos2(r.left() + 3.0, r.center().y),
                Vec2::new(3.0, 16.0),
            ),
            2,
            BLUE,
        );
    }
}
fn nav_row(
    ui: &mut egui::Ui,
    icon: Icon,
    label: &str,
    selected: bool,
    child: bool,
    _action: bool,
) -> egui::Response {
    let (r, response) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 40.0), egui::Sense::click());
    nav_background(ui, r, &response, selected);
    let x = r.left() + if child { 49.0 } else { 24.0 };
    art::draw(
        ui.painter(),
        egui::Rect::from_center_size(
            egui::pos2(x, r.center().y),
            Vec2::splat(if child { 20.0 } else { 18.0 }),
        ),
        icon,
        if !ui.is_enabled() {
            Color32::from_rgb(156, 173, 192)
        } else if child {
            BLUE
        } else {
            INK
        },
    );
    ui.painter().text(
        egui::pos2(r.left() + if child { 70.0 } else { 40.0 }, r.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::proportional(13.0),
        if ui.is_enabled() { INK } else { MUTED },
    );
    response
}
fn nav_group(ui: &mut egui::Ui, icon: Icon, label: &str, collapsed: &mut bool) {
    let response = nav_row(ui, icon, label, false, false, false);
    let r = egui::Rect::from_center_size(
        egui::pos2(response.rect.right() - 25.0, response.rect.center().y),
        Vec2::splat(11.0),
    );
    art::draw(
        ui.painter(),
        r,
        if *collapsed {
            Icon::Down
        } else {
            Icon::Collapse
        },
        INK,
    );
    if response.clicked() {
        *collapsed = !*collapsed;
    }
}
fn menu_item(ui: &mut egui::Ui, icon: Icon, label: &str) -> egui::Response {
    let (r, response) = ui.allocate_exact_size(
        Vec2::new(ui.available_width().max(180.0), 30.0),
        egui::Sense::click(),
    );
    art::hover_background(ui, &response, r);
    art::draw(
        ui.painter(),
        egui::Rect::from_center_size(egui::pos2(r.left() + 15.0, r.center().y), Vec2::splat(17.0)),
        icon,
        INK,
    );
    ui.painter().text(
        egui::pos2(r.left() + 36.0, r.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::proportional(13.0),
        INK,
    );
    response
}
fn submenu(ui: &mut egui::Ui, icon: Icon, label: &str, content: impl FnOnce(&mut egui::Ui)) {
    let padding = ui.spacing().button_padding.x;
    ui.spacing_mut().button_padding.x = 30.0;
    let response = ui
        .menu_button(label, |ui| {
            ui.spacing_mut().button_padding.x = padding;
            content(ui);
        })
        .response;
    ui.spacing_mut().button_padding.x = padding;
    art::draw(
        ui.painter(),
        egui::Rect::from_center_size(
            egui::pos2(response.rect.left() + 15.0, response.rect.center().y),
            Vec2::splat(17.0),
        ),
        icon,
        INK,
    );
}
fn setting_toggle(
    ui: &mut egui::Ui,
    icon: Icon,
    label: &str,
    description: &str,
    value: &mut bool,
) -> bool {
    let mut changed = false;
    card().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.allocate_ui_with_layout(
            Vec2::new(ui.available_width(), 38.0),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                let (r, _) = ui.allocate_exact_size(Vec2::splat(24.0), egui::Sense::hover());
                art::draw(ui.painter(), r, icon, INK);
                ui.add_space(10.0);
                ui.vertical(|ui| {
                    ui.label(label);
                    ui.label(RichText::new(description).size(12.0).color(MUTED));
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    changed = art::toggle(ui, value).changed();
                    ui.label(if *value { "开" } else { "关" });
                });
            },
        );
    });
    ui.add_space(2.0);
    changed
}

impl Desktop {
    pub(super) fn native_event(&mut self, kind: &str, data: Value) {
        match kind {
            "preferences_changed" => {
                if let Ok(prefs) = serde_json::from_value::<Preferences>(
                    crate::preferences_store::read(&self.ui_state.path),
                ) {
                    *self.ui_state.saved_prefs.borrow_mut() = json!(prefs);
                    self.ui_state.prefs = prefs;
                }
            }
            "service_result" => {
                if let Some(error) = data["error"].as_str() {
                    self.ui_state.mcp_notice = error.into();
                } else if data["name"] == "uu_mcp_status" {
                    self.ui_state.mcp_status = data["result"].clone();
                } else {
                    self.ui_state.mcp_poll = Instant::now() - Duration::from_secs(10);
                    self.ui_state.mcp_notice = "已更新".into();
                }
            }

            "port_status" => self.port_status(data),
            "device_action_result" | "action_sent" => self.device_action_result(&data),
            "terminal_output" | "terminal_status" => self.terminal_event(kind, &data),
            "settings_notice" => {
                self.ui_state.last_notice = data["message"].as_str().unwrap_or("").into()
            }
            "network_result" => {
                self.ui_state.network_notice = data["message"].as_str().unwrap_or("").into()
            }
            "file_permission" => {
                self.ui_state.files_allowed = data["enabled"].as_bool().unwrap_or(false);
                if !self.ui_state.files_allowed {
                    self.ui_state.windows.files = false;
                    for transfer in &mut self.ui_state.transfers {
                        if transfer_active(transfer) {
                            transfer["status"] = json!("远端已禁止传输");
                        }
                    }
                }
            }
            "stats" => {
                if data["audio_errors"].as_u64().unwrap_or(0)
                    > self.ui_state.stats["audio_errors"].as_u64().unwrap_or(0)
                {
                    self.ui_state.last_notice = "声音播放异常，请检查系统输出设备".into();
                }
                self.ui_state.stats = data;
            }
            "super_screen" => {
                self.ui_state.super_active = data["active"] == true;
                self.ui_state.super_pending = data["pending"] == true;
                let phase = data["phase"].as_str().unwrap_or("");
                self.ui_state.observe_screen_choice(
                    self.active_device.as_ref().map(|d| d.id.as_str()),
                    phase,
                    data["active"] == true,
                );
                self.ui_state.last_notice = match phase {
                    "switching" => "正在切换屏幕模式…".into(),
                    "waiting" => "远端已接受，正在等待屏幕切换".into(),
                    "failed" => format!("远端未能切换屏幕模式（错误 {}）", data["error"]),
                    "timeout" => "屏幕切换等待超时，请检查远端状态后重试".into(),
                    _ if self.ui_state.super_active => "已进入超级屏".into(),
                    _ => "当前使用远端显示器".into(),
                };
                if matches!(phase, "failed" | "timeout") {
                    self.error = Some(self.ui_state.last_notice.clone());
                }
                if phase == "ready" {
                    self.ui_state.resolution = [0, 0];
                    self.ui_state.capture.clear();
                }
            }
            "screens" => {
                self.ui_state.screens = data["screens"].as_array().cloned().unwrap_or_default();
                self.ui_state.screen = data["current"].as_i64().unwrap_or(0) as i32;
                if let Some(screen) = self
                    .ui_state
                    .screens
                    .iter()
                    .find(|s| s["id"] == self.ui_state.screen)
                    && let Some(notice) = self.ui_state.capture.observe_screen(
                        self.ui_state.screen,
                        [
                            screen["width"].as_u64().unwrap_or(0) as usize,
                            screen["height"].as_u64().unwrap_or(0) as usize,
                        ],
                        self.dimensions,
                    )
                {
                    self.ui_state.last_notice = notice;
                }
            }
            "directory" => {
                if data["request_id"].as_i64() != Some(self.ui_state.directory_request) {
                    return;
                }
                self.ui_state.remote_loading = false;
                if data["error"] == 1 {
                    self.probe_directory_received();
                    let path = data["path"].as_str().unwrap_or("").to_owned();
                    if path != self.ui_state.remote_path {
                        if self.ui_state.remote_back {
                            self.ui_state.remote_history.pop();
                        } else {
                            self.ui_state
                                .remote_history
                                .push(self.ui_state.remote_path.clone());
                        }
                    }
                    self.ui_state.remote_back = false;
                    self.ui_state.entries = data["entries"].as_array().cloned().unwrap_or_default();
                    self.ui_state.remote_path = path.clone();
                    self.ui_state.remote_address = path;
                } else {
                    self.ui_state.remote_back = false;
                    self.ui_state.remote_address = self.ui_state.remote_path.clone();
                    self.error = Some(format!("远端目录读取失败（{}）", data["error"]));
                }
            }
            "transfer" => {
                self.ui_state.transfer_rates.update(&data, Instant::now());
                if let Some(old) = self
                    .ui_state
                    .transfers
                    .iter_mut()
                    .find(|v| v["id"] == data["id"])
                {
                    *old = data;
                } else {
                    self.ui_state.transfers.push(data);
                }
            }
            "drag_files_started" => {
                self.ui_state.windows.files = true;
                self.ui_state.last_notice = "正在接收被控端拖拽发送的文件".into();
            }
            "drag_files_saved" => {
                self.ui_state.local_cache.invalidate();
                self.ui_state.local_cache.refresh(&self.ui_state.local_path);
                self.ui_state.last_notice = format!(
                    "已接收 {} 项到 {}",
                    data["count"],
                    data["directory"].as_str().unwrap_or("接收目录")
                );
            }
            "drag_files_error" => {
                self.error = Some(
                    data["message"]
                        .as_str()
                        .unwrap_or("拖拽文件接收失败")
                        .into(),
                )
            }
            "capture_submitted" => {
                if let (Some(token), Some(id)) =
                    (data["ui_request"].as_u64(), data["request_id"].as_i64())
                {
                    self.ui_state.capture.submitted(token, id);
                }
            }
            "capture_result" => {
                let accepted = data["errors"]
                    .as_array()
                    .is_some_and(|v| v.iter().all(|e| e["code"] == 0));
                if let Some(id) = data["request_id"].as_i64()
                    && let Some(notice) =
                        self.ui_state
                            .capture
                            .acknowledge(id, accepted, self.dimensions)
                {
                    self.ui_state.last_notice = notice;
                }
            }
            "clipboard_sent" if data["error"] != 1 => {
                self.error = Some("远端未接受剪贴板文字".into())
            }
            "clipboard_error" => {
                self.ui_state.last_notice =
                    data["message"].as_str().unwrap_or("剪贴板同步失败").into()
            }
            "file_operation" => {
                if data["error"] == 1 {
                    self.ui_state.last_notice = "文件操作已完成".into();
                    self.control(json!({"op":"read_dir","path":self.ui_state.remote_path}));
                } else {
                    self.error = Some(format!(
                        "文件操作失败（{}）：{}",
                        data["error"],
                        data["path"].as_str().unwrap_or("")
                    ));
                }
            }
            "folder_status" => {
                self.ui_state.last_notice = data["message"].as_str().unwrap_or("").into()
            }
            "released" => self.ui_state.last_notice = "远控会话已关闭".into(),
            _ => {}
        }
    }
    fn control(&mut self, mut command: Value) {
        if command["op"] == "read_dir" {
            self.ui_state.directory_request += 1;
            command["task_id"] = json!(self.ui_state.directory_request);
            self.ui_state.remote_loading = true;
        }
        self.inputs(vec![command]);
    }
    pub(super) fn open_files(&mut self) {
        if !self.ui_state.prefs.allow_files {
            self.error = Some("请先在设置 → 安全中允许文件传输".into());
            return;
        }
        if !self.ui_state.files_allowed {
            self.error = Some("被控设备尚未允许远程协助传输文件".into());
            return;
        }
        self.release();
        self.ui_state.windows.files = true;
        self.control(json!({"op":"read_dir","path":self.ui_state.remote_path}));
    }
    fn apply_capture(&mut self) {
        let token = self
            .ui_state
            .capture
            .begin(self.ui_state.resolution, self.ui_state.screen);
        let p = &self.ui_state.prefs;
        crate::perf::trace(
            "capture_ui_request",
            json!({
                "generation":self.generation,"fps":p.fps,"quality":p.quality,
                "width":self.ui_state.resolution[0],"height":self.ui_state.resolution[1]
            }),
        );
        self.control(
            json!({"op":"capture","ui_request":token,"fps":p.fps,"quality":p.quality,
            "bitrate":if p.quality==6 {p.bitrate}else{0},"screen_id":self.ui_state.screen,
            "width":self.ui_state.resolution[0],"height":self.ui_state.resolution[1]}),
        );
        self.ui_state.save();
        self.ui_state.last_notice = "正在等待远端确认参数".into();
    }
    pub(super) fn capture_frame_observed(&mut self) {
        if self.ui_state.capture.observe_frame(self.dimensions) {
            self.ui_state.last_notice = "分辨率已更新".into();
        }
    }
    pub(super) fn render(&mut self, ui: &mut egui::Ui) {
        self.display_metrics.root_paints += 1;
        let ctx = ui.ctx().clone();
        self.title_bar(ui);
        self.navigation(ui);
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(BG).inner_margin(egui::Margin {
                left: 39,
                right: 40,
                top: 24,
                bottom: 20,
            }))
            .show(ui, |ui| {
                if self.login_open || !self.logged_in {
                    self.login_view(ui);
                } else {
                    match self.ui_state.page {
                        Page::All | Page::Favorites => self.device_list(ui),
                        Page::Device => self.device_page(ui),
                        Page::Assistance => self.assistance_page(ui),
                        Page::Settings => self.settings_page(ui),
                    }
                }
            });
        if self.ui_state.add_help {
            let mut open = true;
            dialog("添加你的其他设备")
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .show(&ctx, |ui| {
                    ui.set_max_width(360.0);
                    ui.label(
                        "在其他设备安装并登录 UU 远程，使用同一个账号即可在设备列表中找到它。",
                    );
                    ui.add_space(8.0);
                    ui.label("本应用提供 Linux 主控功能，不能作为被控设备。");
                });
            self.ui_state.add_help = open;
        }
        if let Some(error) = self.error.clone() {
            dialog("操作提示")
                .collapsible(false)
                .resizable(false)
                .show(&ctx, |ui| {
                    ui.set_max_width(420.0);
                    ui.label(error);
                    ui.add_space(16.0);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add_sized([88.0, 30.0], egui::Button::new("关闭"))
                            .clicked()
                        {
                            self.error = None;
                        }
                    });
                });
        }
        self.assistance_dialog(&ctx);
        self.favorite_dialog(&ctx);
        self.device_tools_dialog(&ctx);
        self.session_windows(&ctx);
    }
    fn remote_dialogs(&mut self, ctx: &egui::Context) {
        if let Some(error) = self.error.clone() {
            dialog("操作提示")
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.set_max_width(420.0);
                    ui.label(error);
                    ui.add_space(16.0);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add_sized([88.0, 30.0], egui::Button::new("关闭"))
                            .clicked()
                        {
                            self.error = None;
                        }
                    });
                });
        }
        if let Some(enter) = self.ui_state.super_confirm {
            self.release();
            let mut open = true;
            dialog(if enter {"进入超级屏"} else {"退出超级屏"})
                .open(&mut open).collapsible(false).resizable(false)
                .show(ctx, |ui| {
                    ui.set_max_width(370.0);
                    ui.label(if enter {
                        "将切换到远端超级屏，当前画面会短暂中断。可在控制中心退出超级屏，返回远端显示器。"
                    } else {
                        "将退出超级屏并返回远端显示器，当前画面会短暂中断。"
                    });
                    ui.add_space(18.0);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if art::primary(ui, "确定", Vec2::new(88.0, 30.0)).clicked() {
                            self.ui_state.screen_choice_pending = Some(enter);
                            self.control(json!({"op":"super_screen","enabled":enter}));
                            self.ui_state.super_confirm = None;
                        }
                        if ui.add_sized([88.0,30.0],egui::Button::new("取消")).clicked() {
                            self.ui_state.super_confirm = None;
                        }
                    });
                });
            if !open {
                self.ui_state.super_confirm = None;
            }
        }
        if self.ui_state.text_window {
            let mut open = true;
            dialog("输入文字")
                .open(&mut open)
                .default_size([440.0, 170.0])
                .show(ctx, |ui| {
                    ui.label("在本机输入或粘贴文字，提交到远端当前输入位置");
                    ui.add(
                        egui::TextEdit::multiline(&mut self.ui_state.text)
                            .desired_width(f32::INFINITY)
                            .desired_rows(4),
                    );
                    ui.add_space(12.0);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add_enabled_ui(!self.ui_state.text.is_empty(), |ui| {
                                primary(ui, "提交文字")
                            })
                            .inner
                            .clicked()
                        {
                            let text = std::mem::take(&mut self.ui_state.text);
                            self.control(json!({"op":"text","text":text}));
                        }
                    });
                });
            self.ui_state.text_window = open;
        }
        if let Some((action, name)) = self.ui_state.remote_action.clone() {
            self.release();
            let mut open = true;
            dialog("确认远端操作")
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.label(format!("确定要{name}远端电脑吗？"));
                    ui.add_space(16.0);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if art::primary(ui, "确定", Vec2::new(88.0, 30.0)).clicked() {
                            self.control(json!({"op":"action","action":action}));
                            self.ui_state.remote_action = None;
                        }
                        if ui
                            .add_sized([88.0, 30.0], egui::Button::new("取消"))
                            .clicked()
                        {
                            self.ui_state.remote_action = None;
                        }
                    });
                });
            if !open {
                self.ui_state.remote_action = None;
            }
        }
    }
    fn title_bar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::top("title")
            .exact_size(48.0)
            .frame(
                egui::Frame::new()
                    .fill(SIDE)
                    .inner_margin(egui::Margin::symmetric(4, 8)),
            )
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                ui.horizontal(|ui| {
                    if art::button(ui, Icon::Back, "返回全部设备").clicked() {
                        self.ui_state.page = Page::All;
                    }
                    let (r, _) =
                        ui.allocate_exact_size(Vec2::new(28.0, 32.0), egui::Sense::hover());
                    art::logo(ui.painter(), r.center(), 22.0);
                    ui.label(RichText::new("UUX").size(12.0));
                    let drag_width = (ui.available_width() - 246.0).max(0.0);
                    let (_, drag) = ui.allocate_exact_size(
                        Vec2::new(drag_width, 32.0),
                        egui::Sense::click_and_drag(),
                    );
                    if drag.drag_started() {
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
                    }
                    if drag.double_clicked() {
                        let max = ui.input(|i| i.viewport().maximized.unwrap_or(false));
                        ui.ctx()
                            .send_viewport_cmd(egui::ViewportCommand::Maximized(!max));
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        art::window_buttons(ui);
                        ui.add_space(8.0);
                        ui.separator();
                        ui.add_space(8.0);
                        let menu = art::button(ui, Icon::Menu, "菜单");
                        egui::Popup::menu(&menu).show(|ui| {
                            ui.set_min_width(180.0);
                            if menu_item(ui, Icon::Settings, "设置").clicked() {
                                self.ui_state.page = Page::Settings;
                                ui.close();
                            }
                            if menu_item(ui, Icon::Info, "如何添加设备").clicked() {
                                self.ui_state.add_help = true;
                                ui.close();
                            }
                            ui.separator();
                            ui.label(concat!("UUX ", env!("CARGO_PKG_VERSION")));
                            ui.label(
                                RichText::new("社区 Linux 原生主控端")
                                    .size(12.0)
                                    .color(MUTED),
                            );
                        });
                        let account = art::button(ui, Icon::Account, "账号");
                        egui::Popup::menu(&account).show(|ui| {
                            ui.set_min_width(160.0);
                            ui.label(if self.logged_in {
                                &self.nickname
                            } else {
                                "未登录"
                            });
                            ui.separator();
                            if ui.button("扫码登录 / 切换账号").clicked() {
                                self.login();
                                ui.close();
                            }
                            if self.logged_in && ui.button("退出账号").clicked() {
                                self.disconnect();
                                self.send(json!({"op":"logout"}));
                                ui.close();
                            }
                        });
                        if ui
                            .add_enabled_ui(self.logged_in && !self.refreshing, |ui| {
                                art::button(ui, Icon::Refresh, "刷新设备")
                            })
                            .inner
                            .clicked()
                        {
                            self.refresh();
                        }
                    });
                });
            });
    }
    fn navigation(&mut self, ui: &mut egui::Ui) {
        egui::Panel::left("nav")
            .exact_size(238.0)
            .resizable(false)
            .show_separator_line(false)
            .frame(egui::Frame::new().fill(SIDE))
            .show(ui, |ui| {
                let bottom = ui.max_rect().bottom();
                ui.spacing_mut().item_spacing.y = 0.0;
                egui::ScrollArea::vertical()
                    .max_height((ui.available_height() - 57.0).max(100.0))
                    .show(ui, |ui| {
                        nav_group(
                            ui,
                            Icon::Devices,
                            "我的设备",
                            &mut self.ui_state.devices_collapsed,
                        );
                        if !self.ui_state.devices_collapsed {
                            for device in self.devices.clone() {
                                let selected = self.ui_state.page == Page::Device
                                    && self.selected.as_ref().is_some_and(|d| d.id == device.id);
                                let (r, response) = ui.allocate_exact_size(
                                    Vec2::new(ui.available_width(), 40.0),
                                    egui::Sense::click(),
                                );
                                nav_background(ui, r, &response, selected);
                                if device.online {
                                    ui.painter().circle_filled(
                                        egui::pos2(r.left() + 26.0, r.center().y),
                                        2.7,
                                        Color32::from_rgb(47, 212, 61),
                                    );
                                }
                                windows_icon(
                                    ui.painter(),
                                    egui::Rect::from_center_size(
                                        egui::pos2(r.left() + 49.0, r.center().y),
                                        Vec2::splat(20.0),
                                    ),
                                    device.online,
                                );
                                let mut text_job = egui::text::LayoutJob::simple_singleline(
                                    device.name.clone(),
                                    egui::FontId::proportional(13.0),
                                    if device.online { INK } else { MUTED },
                                );
                                text_job.wrap.max_width = (r.width() - 82.0).max(10.0);
                                text_job.wrap.max_rows = 1;
                                let text = ui.painter().layout_job(text_job);
                                let text_height = text.size().y;
                                ui.painter()
                                    .with_clip_rect(egui::Rect::from_min_max(
                                        egui::pos2(r.left() + 70.0, r.top()),
                                        egui::pos2(r.right() - 12.0, r.bottom()),
                                    ))
                                    .galley(
                                        egui::pos2(
                                            r.left() + 70.0,
                                            r.center().y - text_height / 2.0,
                                        ),
                                        text,
                                        INK,
                                    );
                                let response = response.on_hover_text(&device.name);
                                if response.clicked() {
                                    self.preview(&device);
                                    self.selected = Some(device.clone());
                                    self.ui_state.page = Page::Device;
                                }
                                if response.double_clicked()
                                    && device.online
                                    && !self.connected
                                    && !self.connecting
                                {
                                    self.connect(device);
                                }
                            }
                            if nav_row(
                                ui,
                                Icon::All,
                                "全部设备",
                                self.ui_state.page == Page::All,
                                true,
                                false,
                            )
                            .clicked()
                            {
                                self.ui_state.page = Page::All;
                            }
                        }
                        nav_group(
                            ui,
                            Icon::Assist,
                            "远程协助",
                            &mut self.ui_state.assistance_collapsed,
                        );
                        if !self.ui_state.assistance_collapsed {
                            if nav_row(
                                ui,
                                Icon::Assist,
                                "开始协助",
                                self.ui_state.page == Page::Assistance,
                                true,
                                false,
                            )
                            .clicked()
                            {
                                self.ui_state.page = Page::Assistance;
                            }
                            if nav_row(
                                ui,
                                Icon::Star,
                                "收藏设备",
                                self.ui_state.page == Page::Favorites,
                                true,
                                false,
                            )
                            .clicked()
                            {
                                self.ui_state.page = Page::Favorites;
                            }
                        }
                    });
                let space = (bottom - ui.cursor().top() - 57.0).max(0.0);
                ui.add_space(space);
                ui.separator();
                ui.add_space(8.0);
                if nav_row(
                    ui,
                    Icon::Settings,
                    "设置",
                    self.ui_state.page == Page::Settings,
                    false,
                    true,
                )
                .clicked()
                {
                    self.ui_state.page = Page::Settings;
                }
            });
    }
    fn device_list(&mut self, ui: &mut egui::Ui) {
        if self.ui_state.page == Page::Favorites {
            self.favorite_page(ui);
            return;
        }
        ui.label(RichText::new("全部设备").font(fonts::bold(24.0)));
        ui.add_space(18.0);
        let devices = self.devices.clone();
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(Vec2::splat(20.0), egui::Sense::hover());
            art::draw(ui.painter(), r.shrink(3.0), Icon::Down, INK);
            ui.label(format!("电脑 {}", devices.len()));
        });
        ui.add_space(4.0);
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 4.0;
            for device in devices {
                let (r, response) = ui.allocate_exact_size(
                    Vec2::new(ui.available_width(), 56.0),
                    egui::Sense::click(),
                );
                ui.painter().rect_filled(r, 3, Color32::WHITE);
                art::hover_background(ui, &response, r.shrink(1.0));
                ui.painter().rect_stroke(
                    r,
                    3,
                    egui::Stroke::new(
                        1.0,
                        if response.hovered() {
                            Color32::from_rgb(186, 206, 225)
                        } else {
                            BORDER
                        },
                    ),
                    egui::StrokeKind::Inside,
                );
                windows_icon(
                    ui.painter(),
                    egui::Rect::from_center_size(
                        egui::pos2(r.left() + 32.0, r.center().y),
                        Vec2::splat(32.0),
                    ),
                    true,
                );
                let mut job = egui::text::LayoutJob::simple_singleline(
                    device.name.clone(),
                    egui::FontId::proportional(14.0),
                    INK,
                );
                job.wrap.max_width = (r.width() - 165.0).max(40.0);
                job.wrap.max_rows = 1;
                let text = ui.painter().layout_job(job);
                ui.painter().galley(
                    egui::pos2(r.left() + 60.0, r.center().y - text.size().y / 2.0),
                    text,
                    INK,
                );
                art::draw(
                    ui.painter(),
                    egui::Rect::from_center_size(
                        egui::pos2(r.right() - 68.0, r.center().y),
                        Vec2::splat(17.0),
                    ),
                    Icon::Info,
                    INK,
                );
                art::draw(
                    ui.painter(),
                    egui::Rect::from_center_size(
                        egui::pos2(r.right() - 25.0, r.center().y),
                        Vec2::splat(14.0),
                    ),
                    Icon::Chevron,
                    INK,
                );
                let response = response.on_hover_text(format!(
                    "{} · {}",
                    if device.online { "在线" } else { "离线" },
                    device.version
                ));
                if response.clicked() {
                    self.preview(&device);
                    self.selected = Some(device.clone());
                    self.ui_state.page = Page::Device;
                }
                if response.double_clicked() && device.online && !self.connected && !self.connecting
                {
                    self.connect(device);
                }
            }
            if self.refreshing {
                ui.spinner();
            }
        });
    }
    fn assistance_page(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("远程协助").font(fonts::bold(24.0)));
        ui.add_space(18.0);
        if self.connected
            && let Some(id) = self.ui_state.active_assist.clone()
        {
            card().show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.colored_label(BLUE, format!("正在协助 {id}"));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("断开").clicked() {
                            self.disconnect();
                        }
                        if ui.button("返回桌面").clicked() {
                            self.ui_state.windows.remote = true;
                        }
                    });
                });
            });
            ui.add_space(12.0);
        }
        card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new("远控伙伴设备").font(fonts::bold(16.0)));
            ui.label(
                RichText::new("通过其他设备的「设备 ID」及「验证码」启动远程控制")
                    .size(12.0)
                    .color(MUTED),
            );
            ui.add_space(10.0);
            ui.separator();
            ui.add_space(18.0);
            ui.label(RichText::new("伙伴的设备 ID").size(12.0).color(MUTED));
            ui.horizontal(|ui| {
                let response = ui.add_sized(
                    [170.0, 32.0],
                    singleline(&mut self.ui_state.assist_id)
                        .hint_text("请输入设备 ID")
                        .char_limit(20),
                );
                ui.add_space(4.0);
                let allowed = self.logged_in
                    && !self.connected
                    && !self.connecting
                    && crate::core::cloud::assistance_id(&self.ui_state.assist_id).is_ok();
                if ui
                    .add_enabled_ui(allowed, |ui| primary(ui, "连接"))
                    .inner
                    .clicked()
                    || (allowed
                        && response.lost_focus()
                        && ui.input(|i| i.key_pressed(egui::Key::Enter)))
                {
                    self.ui_state.assist_code.clear();
                    self.ui_state.assist_prompt = true;
                }
            });
            ui.add_space(24.0);
            ui.separator();
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                ui.label("最近连接");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("清除记录").clicked() {
                        self.ui_state.prefs.assist_history.clear();
                        self.ui_state.save();
                    }
                });
            });
            ui.add_space(8.0);
            ui.horizontal_wrapped(|ui| {
                for id in self.ui_state.prefs.assist_history.clone() {
                    if ui.button(&id).clicked() {
                        self.ui_state.assist_id = id;
                        self.ui_state.assist_code.clear();
                    }
                }
                if self.ui_state.prefs.assist_history.is_empty() {
                    ui.label(RichText::new("暂无连接记录").size(12.0).color(MUTED));
                }
            });
            ui.add_space(10.0);
        });
        ui.add_space(14.0);
        ui.label(
            RichText::new("验证码由被控设备提供，仅用于本次连接。")
                .size(12.0)
                .color(MUTED),
        );
    }
    fn assistance_dialog(&mut self, ctx: &egui::Context) {
        if !self.ui_state.assist_prompt {
            return;
        }
        let mut open = true;
        dialog("连接伙伴设备").open(&mut open).show(ctx, |ui| {
            ui.label(format!("设备 ID：{}", self.ui_state.assist_id));
            ui.add_space(12.0);
            ui.label("设备验证码");
            let field = ui.add_sized(
                [320.0, 30.0],
                singleline(&mut self.ui_state.assist_code)
                    .password(true)
                    .char_limit(64)
                    .hint_text("请输入被控设备显示的验证码"),
            );
            if !ui.ctx().egui_wants_keyboard_input() {
                field.request_focus();
            }
            ui.add_space(18.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let allowed = !self.ui_state.assist_code.is_empty();
                if ui
                    .add_enabled_ui(allowed, |ui| primary(ui, "连接"))
                    .inner
                    .clicked()
                    || allowed && ui.input(|i| i.key_pressed(egui::Key::Enter))
                {
                    self.ui_state.assist_prompt = false;
                    self.connect_assistance();
                }
                if ui.button("取消").clicked() {
                    self.ui_state.assist_prompt = false;
                }
            });
        });
        if !open {
            self.ui_state.assist_prompt = false;
        }
        if !self.ui_state.assist_prompt {
            self.ui_state.assist_code.clear();
        }
    }
    fn connect_assistance(&mut self) {
        let id = match crate::core::cloud::assistance_id(&self.ui_state.assist_id) {
            Ok(id) => id,
            Err(e) => {
                self.error = Some(e.to_string());
                return;
            }
        };
        self.release();
        self.error = None;
        self.generation += 1;
        let code = std::mem::take(&mut self.ui_state.assist_code);
        if !self.send(json!({"op":"connect","assist_id":id,"assist_code":code,"generation":self.generation,"settings":self.ui_state.settings_for_device(&format!("assist:{id}"))})){return;}
        self.active_device = Some(Device {
            id: format!("assist:{id}"),
            name: format!("伙伴设备 {id}"),
            online: true,
            shared: true,
            version: String::new(),
            wallpaper: None,
        });
        self.ui_state.active_assist = Some(id);
        self.ui_state.windows.remote = true;
        self.video_wait_requested = false;
        self.texture = None;
        self.frames = 0;
        self.last_frame = None;
        self.connected = false;
        self.connecting = true;
        self.phase = "正在验证伙伴设备".into();
    }
    fn device_page(&mut self, ui: &mut egui::Ui) {
        let Some(device) = self.selected.clone() else {
            return;
        };
        let current = self
            .active_device
            .as_ref()
            .is_some_and(|d| d.id == device.id);
        let can_open =
            device.online && ((current && self.connected) || (!self.connected && !self.connecting));
        ui.horizontal(|ui| {
            egui::Frame::new()
                .fill(INK)
                .corner_radius(12)
                .inner_margin(egui::Margin::symmetric(8, 2))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        ui.colored_label(
                            if device.online {
                                Color32::from_rgb(0, 214, 151)
                            } else {
                                MUTED
                            },
                            "●",
                        );
                        ui.label(
                            RichText::new(if device.online { "在线" } else { "离线" })
                                .size(12.0)
                                .color(Color32::WHITE),
                        );
                    });
                });
            ui.label(RichText::new(&device.name).font(fonts::bold(24.0)));
        });
        ui.add_space(22.0);
        if self.connected && current {
            egui::Frame::new()
                .fill(Color32::from_rgb(222, 240, 255))
                .corner_radius(3)
                .inner_margin(14)
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.colored_label(BLUE, "远控中");
                        ui.label("当前正在控制此电脑");
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("断开远控").clicked() {
                                self.disconnect();
                            }
                        });
                    });
                });
            ui.add_space(12.0);
        }
        egui::Frame::new()
            .fill(Color32::WHITE)
            .corner_radius(4)
            .stroke(egui::Stroke::new(1.0, BORDER))
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                let (r, response) = ui.allocate_exact_size(
                    Vec2::new(ui.available_width(), 240.0),
                    if can_open {
                        egui::Sense::click()
                    } else {
                        egui::Sense::hover()
                    },
                );
                if can_open {
                    response
                        .clone()
                        .on_hover_cursor(egui::CursorIcon::PointingHand);
                }
                if let Some(texture) = self
                    .previews
                    .get(&device.id)
                    .or(self.texture.as_ref().filter(|_| current))
                {
                    let image_aspect = texture.size_vec2().x / texture.size_vec2().y;
                    let tile_aspect = r.width() / r.height();
                    let uv_size = if image_aspect > tile_aspect {
                        Vec2::new(tile_aspect / image_aspect, 1.0)
                    } else {
                        Vec2::new(1.0, image_aspect / tile_aspect)
                    };
                    ui.painter().image(
                        texture.id(),
                        r,
                        egui::Rect::from_center_size(egui::pos2(0.5, 0.5), uv_size),
                        Color32::WHITE,
                    );
                    ui.painter().rect_filled(
                        r,
                        0,
                        Color32::from_black_alpha(if can_open && response.hovered() {
                            22
                        } else {
                            55
                        }),
                    );
                } else {
                    let mut mesh = egui::Mesh::default();
                    for (pos, color) in [
                        (r.left_top(), Color32::from_rgb(18, 163, 186)),
                        (r.right_top(), Color32::from_rgb(18, 114, 146)),
                        (r.left_bottom(), Color32::from_rgb(0, 68, 89)),
                        (r.right_bottom(), Color32::from_rgb(1, 87, 110)),
                    ] {
                        mesh.colored_vertex(pos, color);
                    }
                    mesh.add_triangle(0, 1, 2);
                    mesh.add_triangle(1, 3, 2);
                    ui.painter().add(egui::Shape::mesh(mesh));
                    if can_open && response.hovered() {
                        ui.painter()
                            .rect_filled(r, 0, Color32::from_white_alpha(28));
                    }
                    art::draw(
                        ui.painter(),
                        egui::Rect::from_center_size(
                            r.center() - Vec2::new(0.0, 14.0),
                            Vec2::splat(54.0),
                        ),
                        Icon::Devices,
                        Color32::from_white_alpha(145),
                    );
                }
                ui.painter().text(
                    egui::pos2(r.center().x, r.bottom() - 48.0),
                    egui::Align2::CENTER_CENTER,
                    if current && self.connected {
                        "返回桌面  →"
                    } else {
                        "进入桌面  →"
                    },
                    egui::FontId::proportional(17.0),
                    Color32::WHITE,
                );
                if response.clicked() && device.online {
                    if current && self.connected {
                        self.ui_state.windows.remote = true;
                        ui.ctx().send_viewport_cmd_to(
                            session_windows::remote_id(),
                            egui::ViewportCommand::Focus,
                        );
                    } else if !self.connecting && !self.connected {
                        self.connect(device.clone());
                    }
                }
                let (footer, _) = ui.allocate_exact_size(
                    Vec2::new(ui.available_width(), 50.0),
                    egui::Sense::hover(),
                );
                let left = egui::Rect::from_min_max(
                    footer.min,
                    egui::pos2(footer.center().x, footer.bottom()),
                );
                let right = egui::Rect::from_min_max(
                    egui::pos2(footer.center().x, footer.top()),
                    footer.max,
                );
                let files = ui.interact(
                    left,
                    ui.scope_id().with("device-files"),
                    if can_open {
                        egui::Sense::click()
                    } else {
                        egui::Sense::hover()
                    },
                );
                let menu = ui.interact(
                    right,
                    ui.scope_id().with("device-more"),
                    egui::Sense::click(),
                );
                if can_open {
                    art::hover_background(ui, &files, left.shrink(2.0));
                }
                art::hover_background(ui, &menu, right.shrink(2.0));
                ui.painter().line_segment(
                    [
                        footer.center() - Vec2::new(0.0, 10.0),
                        footer.center() + Vec2::new(0.0, 10.0),
                    ],
                    egui::Stroke::new(1.0, BORDER),
                );
                ui.painter().text(
                    left.center(),
                    egui::Align2::CENTER_CENTER,
                    "文件传输",
                    egui::FontId::proportional(13.0),
                    if device.online { INK } else { MUTED },
                );
                ui.painter().text(
                    right.center(),
                    egui::Align2::CENTER_CENTER,
                    "···  更多",
                    egui::FontId::proportional(13.0),
                    INK,
                );
                if files.clicked() && can_open {
                    if current && self.connected {
                        self.open_files();
                        ui.ctx().send_viewport_cmd_to(
                            session_windows::files_id(),
                            egui::ViewportCommand::Focus,
                        );
                    } else {
                        self.connect_for_tools(device.clone());
                        self.ui_state.windows.files = true;
                    }
                }
                egui::Popup::menu(&menu).show(|ui| self.device_menu(ui, &device));
            });
        ui.add_space(18.0);
        ui.label(
            RichText::new(if device.online {
                "双击左侧设备可直接进入桌面"
            } else {
                "设备离线，开机并登录后可连接"
            })
            .size(12.0)
            .color(MUTED),
        );
    }
    fn remote_toolbar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::top("remote-bar")
            .exact_size(38.0)
            .frame(
                egui::Frame::new()
                    .fill(Color32::from_rgb(215, 221, 225))
                    .inner_margin(egui::Margin::symmetric(4, 3)),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    if art::button(ui, Icon::Back, "显示设备列表").clicked() {
                        self.release();
                        ui.ctx().send_viewport_cmd_to(
                            egui::ViewportId::ROOT,
                            egui::ViewportCommand::Focus,
                        );
                    }
                    let title_start = ui.next_widget_position();
                    // Preserve the right-side controls at the minimum window width.
                    let show_stats = ui.available_width() > 800.0;
                    let title_width =
                        (ui.available_width() - 410.0 - if show_stats { 120.0 } else { 0.0 })
                            .clamp(40.0, 160.0);
                    let name = self
                        .active_device
                        .as_ref()
                        .map_or("远程桌面", |d| d.name.as_str());
                    ui.add_sized([title_width, 26.0], egui::Label::new(name).truncate())
                        .on_hover_text(name);
                    ui.separator();
                    egui::Frame::new()
                        .fill(Color32::WHITE)
                        .corner_radius(6.0)
                        .inner_margin(egui::Margin::symmetric(10, 5))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                let (rect, _) =
                                    ui.allocate_exact_size(Vec2::splat(16.0), egui::Sense::hover());
                                art::draw(ui.painter(), rect, Icon::Fullscreen, INK);
                                ui.label(if self.ui_state.super_pending {
                                    "切换中…"
                                } else if self.ui_state.super_active {
                                    "超级屏"
                                } else {
                                    "远端显示器"
                                });
                            });
                        });
                    if show_stats {
                        ui.label(
                            RichText::new(format!(
                                "{} FPS  ·  {:.1} Mb/s",
                                self.ui_state.stats["fps"].as_u64().unwrap_or(0),
                                self.ui_state.stats["mbps"].as_f64().unwrap_or(0.0)
                            ))
                            .size(11.0)
                            .color(MUTED),
                        )
                        .on_hover_text(&self.ui_state.last_notice);
                    }
                    let (blank, _) = ui.allocate_exact_size(
                        Vec2::new((ui.available_width() - 268.0).max(0.0), 32.0),
                        egui::Sense::hover(),
                    );
                    let drag = ui.interact(
                        egui::Rect::from_min_max(title_start, blank.max),
                        ui.scope_id().with("remote-drag"),
                        egui::Sense::click_and_drag(),
                    );
                    if drag.drag_started() {
                        self.release();
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
                    }
                    if drag.double_clicked() {
                        let max = ui.input(|i| i.viewport().maximized.unwrap_or(false));
                        ui.ctx()
                            .send_viewport_cmd(egui::ViewportCommand::Maximized(!max));
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        art::window_buttons(ui);
                        ui.menu_button("控制中心", |ui| self.remote_menu(ui));
                        let keyboard_tip = if self.remote_input.armed {
                            format!("释放键鼠（{}）", self.ui_state.prefs.release_shortcut)
                        } else {
                            "接管键鼠".into()
                        };
                        if ui
                            .add_enabled_ui(!self.ui_state.prefs.read_only, |ui| {
                                art::button(ui, Icon::Keyboard, &keyboard_tip)
                            })
                            .inner
                            .clicked()
                        {
                            if self.remote_input.armed {
                                self.release();
                            } else {
                                self.remote_input.armed = true;
                            }
                        }
                    });
                });
            });
    }
    fn remote_menu(&mut self, ui: &mut egui::Ui) {
        ui.set_min_width(192.0);
        ui.spacing_mut().interact_size.y = 30.0;
        ui.spacing_mut().item_spacing.y = 2.0;
        self.release();
        submenu(ui, Icon::Fullscreen, "超级屏", |ui| {
            ui.set_min_width(176.0);
            ui.label(
                RichText::new(if self.ui_state.super_active {
                    "当前：超级屏"
                } else {
                    "当前：远端显示器"
                })
                .color(MUTED),
            );
            ui.separator();
            let enabled = !self.ui_state.super_pending && !self.ui_state.screens.is_empty();
            let title = if self.ui_state.super_active {
                "退出超级屏"
            } else {
                "进入超级屏"
            };
            if ui
                .add_enabled_ui(enabled, |ui| menu_item(ui, Icon::Fullscreen, title))
                .inner
                .clicked()
            {
                self.ui_state.super_confirm = Some(!self.ui_state.super_active);
                ui.close();
            }
            if self.ui_state.super_pending {
                ui.label("正在切换，请稍候…");
            }
        });
        submenu(ui, Icon::Devices, "画质", |ui| {
            ui.set_min_width(176.0);
            for (q, name) in [
                (5, "自动（清晰）"),
                (4, "原画 20M"),
                (3, "高清 8M"),
                (2, "清晰 2M"),
                (1, "流畅"),
                (6, "自定义"),
            ] {
                if ui
                    .selectable_value(&mut self.ui_state.prefs.quality, q, name)
                    .changed()
                {
                    self.apply_capture();
                }
            }
            if self.ui_state.prefs.quality == 6 {
                ui.add(
                    egui::Slider::new(&mut self.ui_state.prefs.bitrate, 500_000..=40_000_000)
                        .text("bit/s"),
                );
                if ui.button("应用码率").clicked() {
                    self.apply_capture();
                }
            }
            ui.separator();
            for fps in [144, 90, 60, 30] {
                if ui
                    .selectable_value(&mut self.ui_state.prefs.fps, fps, format!("{fps} 帧"))
                    .changed()
                {
                    self.apply_capture();
                }
            }
        });
        submenu(ui, Icon::Fullscreen, "窗口", |ui| {
            ui.menu_button("显示器", |ui| {
                for screen in self.ui_state.screens.clone() {
                    let id = screen["id"].as_i64().unwrap_or(0) as i32;
                    if ui
                        .selectable_value(
                            &mut self.ui_state.screen,
                            id,
                            screen["name"].as_str().unwrap_or("显示器"),
                        )
                        .changed()
                    {
                        self.apply_capture();
                    }
                }
            });
            ui.menu_button("分辨率", |ui| {
                if ui
                    .selectable_value(&mut self.ui_state.resolution, [0, 0], "跟随远端")
                    .changed()
                {
                    self.apply_capture();
                }
                let resolutions = self
                    .ui_state
                    .screens
                    .iter()
                    .find(|s| s["id"] == self.ui_state.screen)
                    .and_then(|s| s["resolutions"].as_array())
                    .cloned()
                    .unwrap_or_default();
                for r in resolutions {
                    let w = r["width"].as_i64().unwrap_or(0) as i32;
                    let h = r["height"].as_i64().unwrap_or(0) as i32;
                    if w > 0
                        && h > 0
                        && w <= 3840
                        && h <= 2160
                        && ui
                            .selectable_value(
                                &mut self.ui_state.resolution,
                                [w, h],
                                format!("{w} × {h}"),
                            )
                            .changed()
                    {
                        self.apply_capture();
                    }
                }
            });
            ui.separator();
            if ui
                .checkbox(&mut self.ui_state.prefs.scale_up, "允许画面放大")
                .changed()
            {
                self.ui_state.save();
            }
        });
        submenu(ui, Icon::Sound, "声音", |ui| {
            if ui
                .checkbox(&mut self.ui_state.prefs.audio, "播放远端声音")
                .changed()
            {
                self.control(json!({"op":"audio","enabled":self.ui_state.prefs.audio}));
                self.ui_state.save();
            }
        });
        submenu(ui, Icon::Settings, "安全", |ui| {
            if ui
                .checkbox(&mut self.ui_state.prefs.read_only, "仅观看，禁用键鼠")
                .changed()
            {
                self.release();
                self.apply_security();
            }
            if ui
                .checkbox(
                    &mut self.ui_state.prefs.lock_on_disconnect,
                    "断开后锁定远端",
                )
                .changed()
            {
                self.apply_security();
            }
            if ui.button("锁定远端电脑").clicked() {
                self.ui_state.remote_action = Some((10, "锁定".into()));
                ui.close();
            }
        });
        submenu(ui, Icon::Keyboard, "外设", |ui| {
            if ui
                .checkbox(&mut self.ui_state.prefs.ime, "使用本机输入法")
                .changed()
            {
                self.ui_state.save();
            }
            if ui
                .checkbox(&mut self.ui_state.prefs.clipboard, "同步文字剪贴板")
                .changed()
            {
                self.control(
                    json!({"op":"clipboard_enabled","enabled":self.ui_state.prefs.clipboard}),
                );
                self.ui_state.save();
            }
            ui.separator();
            ui.add_enabled_ui(!self.ui_state.prefs.read_only, |ui| {
                if menu_item(ui, Icon::Keyboard, "输入文字").clicked() {
                    self.ui_state.text_window = true;
                    ui.close();
                }
                if ui.button("粘贴本机剪贴板").clicked() {
                    self.control(json!({"op":"paste_local"}));
                    ui.close();
                }
            });
        });
        submenu(ui, Icon::All, "更多", |ui| {
            if menu_item(ui, Icon::Transfer, "文件传输").clicked() {
                self.open_files();
                ui.close();
            }
            ui.separator();
            for (action, name) in [(2, "显示桌面"), (13, "任务管理器")] {
                if ui.button(name).clicked() {
                    self.control(json!({"op":"action","action":action}));
                    ui.close();
                }
            }
            ui.separator();
            for (action, name) in [(9, "重启"), (12, "关闭")] {
                if ui.button(format!("{name}远端电脑")).clicked() {
                    self.ui_state.remote_action = Some((action, name.into()));
                    ui.close();
                }
            }
        });
        ui.separator();
        if menu_item(
            ui,
            Icon::Fullscreen,
            if self.fullscreen {
                "退出全屏"
            } else {
                "全屏"
            },
        )
        .on_hover_text(&self.ui_state.prefs.fullscreen_shortcut)
        .clicked()
        {
            self.fullscreen = !self.fullscreen;
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::Fullscreen(self.fullscreen));
            ui.close();
        }
        if menu_item(ui, Icon::Exit, "退出远控").clicked() {
            self.close_remote_window();
            ui.close();
        }
    }
    fn canvas(&mut self, ui: &mut egui::Ui) {
        self.display_metrics.remote_calls += 1;
        let (canvas, _) = ui.allocate_exact_size(ui.available_size(), egui::Sense::hover());
        if let Some(texture) = &self.texture {
            let scale = (canvas.width() / self.dimensions[0] as f32)
                .min(canvas.height() / self.dimensions[1] as f32);
            let scale = if self.ui_state.prefs.scale_up {
                scale
            } else {
                scale.min(1.0)
            };
            let rect = egui::Rect::from_center_size(
                canvas.center(),
                Vec2::new(
                    self.dimensions[0] as f32 * scale,
                    self.dimensions[1] as f32 * scale,
                ),
            );
            ui.painter().image(
                texture.id(),
                rect,
                egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
                Color32::WHITE,
            );
            self.display_metrics.draw(self.generation, self.frames);
            let response = ui.interact(
                rect,
                ui.scope_id().with("remote-canvas"),
                egui::Sense::click_and_drag(),
            );
            if response.clicked() {
                response.request_focus();
            }
            let pointer = ui.input(|i| i.pointer.hover_pos());
            let inside = pointer.is_some_and(|p| {
                !self.ui_state.resize_captured
                    && rect.contains(p)
                    && ui.ctx().layer_id_at(p) == Some(response.layer_id)
            });
            if inside
                && !self.ui_state.remote_hovered
                && !self.ui_state.prefs.read_only
                && !egui::Popup::is_any_open(ui.ctx())
            {
                response.request_focus();
                if !ui.input(|i| i.focused) {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Focus);
                }
            }
            self.ui_state.remote_hovered = inside;
            if !self.ui_state.prefs.read_only
                && !self.ui_state.resize_captured
                && !self.ui_state.text_window
                && self.ui_state.remote_action.is_none()
                && self.ui_state.super_confirm.is_none()
                && !self.ui_state.super_pending
                && self.error.is_none()
                && !egui::Popup::is_any_open(ui.ctx())
            {
                let events = std::mem::take(&mut self.pending_input)
                    .into_iter()
                    .filter(|event| match event {
                        egui::Event::PointerButton {
                            pos, pressed: true, ..
                        } => {
                            !rect.contains(*pos)
                                || ui.ctx().layer_id_at(*pos) == Some(response.layer_id)
                        }
                        _ => true,
                    })
                    .collect::<Vec<_>>();
                let (focused, pointer) = ui.input(|i| (i.focused, i.pointer.hover_pos()));
                self.remote_input.local_clipboard = self.ui_state.prefs.clipboard;
                let values =
                    self.remote_input
                        .events(&events, focused, rect, self.dimensions, pointer);
                self.inputs(values);
                if self.remote_input.armed && self.ui_state.prefs.ime {
                    ui.ctx().output_mut(|o| {
                        o.ime = Some(egui::output::IMEOutput {
                            rect,
                            purpose: egui::IMEPurpose::Normal,
                            should_interrupt_composition: false,
                            cursor_rect: egui::Rect::from_min_size(
                                pointer.unwrap_or(rect.center()),
                                Vec2::new(1.0, 20.0),
                            ),
                        })
                    });
                }
            } else {
                let values = self.remote_input.suspend();
                self.inputs(values);
            }
        } else {
            ui.painter().text(
                canvas.center(),
                egui::Align2::CENTER_CENTER,
                &self.phase,
                egui::FontId::proportional(18.0),
                Color32::WHITE,
            );
        }
    }
    fn remote_entry_path(&self, row: &Value) -> String {
        row["path"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| {
                format!(
                    "{}\\{}",
                    self.ui_state.remote_path.trim_end_matches('\\'),
                    row["name"].as_str().unwrap_or("")
                )
            })
    }
}

fn remote_parent(path: &str) -> String {
    let clean = path.trim_end_matches(['\\', '/']);
    clean
        .rsplit_once(['\\', '/'])
        .map(|(parent, _)| format!("{parent}\\"))
        .unwrap_or_default()
}

fn size_text(n: u64) -> String {
    if n >= 1024 * 1024 {
        format!("{:.2} MiB", n as f64 / 1048576.0)
    } else if n >= 1024 {
        format!("{:.1} KiB", n as f64 / 1024.0)
    } else {
        format!("{n} B")
    }
}
fn transfer_active(t: &Value) -> bool {
    matches!(
        t["status"].as_str(),
        Some(
            "等待中"
                | "准备中"
                | "传输中"
                | "文件夹传输中"
                | "已暂停"
                | "等待数据"
                | "等待远端确认"
        )
    )
}

#[cfg(test)]
mod screen_preference_tests {
    use super::*;

    #[test]
    fn remembers_confirmed_choices_per_device_across_restarts_and_not_cleanup() {
        let dir = tempfile::tempdir().unwrap();
        let mut ui = UiState::load(Some(dir.path().to_owned()));
        assert!(ui.settings_for_device("a").get("super_screen").is_none());
        ui.screen_choice_pending = Some(true);
        ui.observe_screen_choice(Some("a"), "waiting", false);
        assert!(ui.prefs.screen_preferences.is_empty());
        ui.observe_screen_choice(Some("a"), "failed", false);
        ui.observe_screen_choice(Some("a"), "ready", true);
        assert!(ui.prefs.screen_preferences.is_empty());

        ui.screen_choice_pending = Some(true);
        ui.observe_screen_choice(Some("a"), "ready", true);
        ui.screen_choice_pending = Some(false);
        ui.observe_screen_choice(Some("b"), "ready", false);
        ui.end_session();
        ui.observe_screen_choice(Some("a"), "ready", false);
        let restored = UiState::load(Some(dir.path().to_owned()));
        assert_eq!(restored.settings_for_device("a")["super_screen"], true);
        assert_eq!(restored.settings_for_device("b")["super_screen"], false);
        assert!(
            restored
                .settings_for_device("c")
                .get("super_screen")
                .is_none()
        );
    }
}

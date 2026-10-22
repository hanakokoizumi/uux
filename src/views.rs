#[path = "device_tools.rs"]
mod device_tools;
#[path = "favorites.rs"]
mod favorites;
#[path = "file_columns.rs"]
mod file_columns;
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
    output_device: String,
    microphone_device: String,
    codec: String,
    decoder_preference: String,
    true_color: bool,
    hdr: bool,
    dpi: i32,
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
            output_device: "default".into(),
            microphone_device: "default".into(),
            codec: "h264".into(),
            decoder_preference: "auto".into(),
            true_color: false,
            hdr: false,
            dpi: 0,
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
    capabilities: Value,
    microphone_status: Value,
    microphone_pending: bool,
    resource_count: usize,
    mcp_poll: Instant,
    mcp_notice: String,
    mcp_copied_at: Option<Instant>,
    path: PathBuf,
    page: Page,
    pub windows: SessionWindows,
    remote_resize: window_resize::ResizeGrip,
    root_resize: window_resize::ResizeGrip,
    file_resize: window_resize::ResizeGrip,
    resize_captured: bool,
    device_tools: device_tools::DeviceTools,
    terminal: terminal_view::TerminalView,
    ports: ports_view::PortsView,
    files_allowed: bool,
    text_window: bool,
    custom_bitrate_mbps: Option<f64>,
    text: String,
    stats: Value,
    screens: Vec<Value>,
    remote_streamer: Value,
    hidden_screens: BTreeSet<i32>,
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
    local_columns: file_columns::Columns<4>,
    remote_columns: file_columns::Columns<4>,
    task_columns: file_columns::Columns<6>,
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
    about_open: bool,
    assist_id: String,
    assist_code: String,
    active_assist: Option<String>,
    autostart: bool,
    shortcut_capture: Option<usize>,
    network_notice: String,
    download_draft: Option<String>,
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
            proxy_port: if prefs.proxy.port == 0 {
                String::new()
            } else {
                prefs.proxy.port.to_string()
            },
            proxy_draft: prefs.proxy.clone(),
            local_cache: Default::default(),
            local_path: prefs.download.clone(),
            local_address: prefs.download.clone(),
            saved_prefs: std::cell::RefCell::new(json!(prefs)),
            mcp_status: Value::Null,
            capabilities: Value::Null,
            microphone_status: Value::Null,
            microphone_pending: false,
            resource_count: 0,
            mcp_poll: Instant::now() - Duration::from_secs(10),
            mcp_notice: String::new(),
            mcp_copied_at: None,
            prefs,
            path,
            page: Page::All,
            windows: SessionWindows::default(),
            remote_resize: Default::default(),
            root_resize: Default::default(),
            file_resize: Default::default(),
            resize_captured: false,
            device_tools: Default::default(),
            terminal: Default::default(),
            ports: Default::default(),
            files_allowed: true,
            text_window: false,
            custom_bitrate_mbps: None,
            text: String::new(),
            stats: Value::Null,
            screens: vec![],
            remote_streamer: Value::Null,
            hidden_screens: Default::default(),
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
            local_columns: Default::default(),
            remote_columns: Default::default(),
            task_columns: Default::default(),
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
            about_open: false,
            assist_id: String::new(),
            assist_code: String::new(),
            active_assist: None,
            autostart: crate::platform::autostart_enabled(),
            shortcut_capture: None,
            network_notice: String::new(),
            download_draft: None,
        }
    }
    fn save(&self) {
        let current = json!(self.prefs);
        if crate::preferences_store::merge(&self.path, &self.saved_prefs.borrow(), &current).is_ok()
        {
            *self.saved_prefs.borrow_mut() = current;
        }
    }
    pub(super) fn can_retire(&self) -> bool {
        self.resource_count == 0
            && !self.windows.remote
            && !self.windows.files
            && !self.windows.terminal
            && !self.windows.ports
    }
    pub(super) fn select_device_page(&mut self) {
        self.page = Page::Device;
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
        self.custom_bitrate_mbps = None;
        self.file_dialog = None;
        self.remote_action = None;
        self.super_active = false;
        self.microphone_status = Value::Null;
        self.microphone_pending = false;
        self.super_pending = false;
        self.super_confirm = None;
        self.screen_choice_pending = None;
        self.screens.clear();
        self.remote_streamer = Value::Null;
        self.hidden_screens.clear();
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
            "codec":self.prefs.codec,"decoder_preference":self.prefs.decoder_preference,"true_color":self.prefs.true_color,"hdr":self.prefs.hdr,"dpi":self.prefs.dpi,"output_device":self.prefs.output_device,
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
fn singleline(text: &mut String) -> art::TextField<'_> {
    art::TextField::new(text)
}
fn combo(id: impl egui::AsIdSalt) -> egui::ComboBox<'static> {
    egui::ComboBox::from_id_salt(id).icon(|ui, rect, visuals, _| {
        art::draw(
            ui.painter(),
            egui::Rect::from_center_size(rect.center(), Vec2::splat(12.0)),
            Icon::Down,
            visuals.fg_stroke.color,
        );
    })
}
fn modal<R>(
    ctx: &egui::Context,
    id: &'static str,
    content: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::ModalResponse<R> {
    egui::Modal::new(egui::Id::unique((ctx.viewport_id(), id)))
        .backdrop_color(Color32::from_black_alpha(90))
        .frame(
            egui::Frame::new()
                .fill(Color32::WHITE)
                .corner_radius(8)
                .shadow(egui::epaint::Shadow {
                    offset: [0, 8],
                    blur: 24,
                    spread: 0,
                    color: Color32::from_black_alpha(42),
                }),
        )
        .show(ctx, |ui| {
            ui.set_width(446.0);
            ui.spacing_mut().item_spacing = Vec2::ZERO;
            content(ui)
        })
}
fn modal_body(ui: &mut egui::Ui, title: &str, content: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::new().inner_margin(24).show(ui, |ui| {
        ui.set_width(398.0);
        ui.spacing_mut().item_spacing = Vec2::new(8.0, 12.0);
        ui.label(RichText::new(title).size(16.0));
        content(ui);
    });
}
fn modal_actions(ui: &mut egui::Ui, accept: &str, enabled: bool, cancel: &str) -> (bool, bool) {
    egui::Frame::new()
        .fill(BG)
        .inner_margin(24)
        .corner_radius(egui::CornerRadius {
            nw: 0,
            ne: 0,
            sw: 8,
            se: 8,
        })
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            ui.horizontal(|ui| {
                let accept = ui
                    .add_enabled_ui(enabled, |ui| {
                        art::primary(ui, accept, Vec2::new(195.0, 32.0))
                    })
                    .inner
                    .clicked();
                let cancel = ui
                    .add_sized([195.0, 32.0], egui::Button::new(cancel))
                    .clicked();
                (accept, cancel)
            })
            .inner
        })
        .inner
}
fn modal_cancel(ui: &mut egui::Ui, label: &str) -> bool {
    egui::Frame::new()
        .fill(BG)
        .inner_margin(24)
        .corner_radius(egui::CornerRadius {
            nw: 0,
            ne: 0,
            sw: 8,
            se: 8,
        })
        .show(ui, |ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_sized([195.0, 32.0], egui::Button::new(label))
                    .clicked()
            })
            .inner
        })
        .inner
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
    menu_item_color(ui, icon, label, INK)
}
// Logical points: every action menu uses the same width at any desktop scale.
const MENU_WIDTH: f32 = 192.0;
fn menu_layout(ui: &mut egui::Ui) {
    ui.set_width(MENU_WIDTH);
    ui.spacing_mut().item_spacing.y = 2.0;
    ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
}
fn menu_item_color(ui: &mut egui::Ui, icon: Icon, label: &str, color: Color32) -> egui::Response {
    ui.spacing_mut().item_spacing.y = 2.0;
    let (r, response) = ui.allocate_exact_size(Vec2::new(MENU_WIDTH, 30.0), egui::Sense::click());
    art::hover_background(ui, &response, r);
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, ui.is_enabled(), label));
    art::draw(
        ui.painter(),
        egui::Rect::from_center_size(egui::pos2(r.left() + 15.0, r.center().y), Vec2::splat(17.0)),
        icon,
        color,
    );
    ui.painter().text(
        egui::pos2(r.left() + 36.0, r.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::proportional(13.0),
        color,
    );
    response
}
fn submenu(ui: &mut egui::Ui, icon: Icon, label: &str, content: impl FnOnce(&mut egui::Ui)) {
    let response = menu_item(ui, icon, label);
    show_submenu(ui, response, content);
}
fn text_submenu(
    ui: &mut egui::Ui,
    label: &str,
    selected: Option<bool>,
    content: impl FnOnce(&mut egui::Ui),
) -> bool {
    let response = menu_label(ui, label, selected);
    show_submenu(ui, response, content)
}
fn show_submenu(
    ui: &mut egui::Ui,
    response: egui::Response,
    content: impl FnOnce(&mut egui::Ui),
) -> bool {
    art::draw(
        ui.painter(),
        egui::Rect::from_center_size(
            response.rect.right_center() - Vec2::new(10.0, 0.0),
            Vec2::splat(10.0),
        ),
        Icon::Chevron,
        INK,
    );
    let bounds = ui.ctx().content_rect();
    // Keep the submenu small enough to fit on either side of its anchor row.
    // A full-height popup cannot satisfy egui's alignment candidates and may
    // otherwise be clamped over its parent, with its final items off-screen.
    let max_height =
        ((bounds.bottom() - response.rect.top()).max(response.rect.bottom() - bounds.top()) - 16.0)
            .max(80.0);
    egui::containers::menu::SubMenu::new()
        .show(ui, &response, |ui| {
            menu_layout(ui);
            // Popup areas start with a cached/default 400-point height. Override
            // that limit so a large window can display the complete menu again.
            ui.set_max_height(max_height);
            egui::ScrollArea::vertical()
                .max_height(max_height)
                .auto_shrink([false, true])
                .show(ui, content);
        })
        .is_some()
}
fn menu_label(ui: &mut egui::Ui, label: &str, selected: Option<bool>) -> egui::Response {
    ui.spacing_mut().item_spacing.y = 2.0;
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(MENU_WIDTH, 30.0), egui::Sense::click());
    art::hover_background(ui, &response, rect);
    if selected == Some(true) {
        ui.painter()
            .circle_filled(rect.left_center() + Vec2::new(15.0, 0.0), 2.0, INK);
    }
    let mut job = egui::text::LayoutJob::simple_singleline(
        label.into(),
        egui::FontId::proportional(13.0),
        INK,
    );
    job.wrap.max_width = rect.width() - 54.0;
    job.wrap.max_rows = 1;
    let text = ui.painter().layout_job(job);
    let elided = text.elided;
    ui.painter().galley(
        rect.left_center() + Vec2::new(36.0, -text.size().y / 2.0),
        text,
        INK,
    );
    response.widget_info(|| {
        if let Some(selected) = selected {
            egui::WidgetInfo::selected(egui::Role::RadioButton, ui.is_enabled(), selected, label)
        } else {
            egui::WidgetInfo::labeled(egui::Role::Button, ui.is_enabled(), label)
        }
    });
    if elided {
        response.on_hover_text(label)
    } else {
        response
    }
}
fn menu_choice<T: PartialEq>(
    ui: &mut egui::Ui,
    value: &mut T,
    choice: T,
    label: impl Into<String>,
) -> egui::Response {
    let selected = *value == choice;
    let mut response = menu_label(ui, &label.into(), Some(selected));
    if response.clicked() && !selected {
        *value = choice;
        response.mark_changed();
        ui.close();
    }
    response
}
fn menu_toggle(ui: &mut egui::Ui, value: &mut bool, label: &str) -> egui::Response {
    let mut response = menu_label(ui, label, Some(*value));
    if response.clicked() {
        *value = !*value;
        response.mark_changed();
        ui.close();
    }
    response
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
        let (row, response) =
            ui.allocate_exact_size(Vec2::new(ui.available_width(), 38.0), egui::Sense::hover());
        art::draw(
            ui.painter(),
            egui::Rect::from_center_size(
                row.left_center() + Vec2::new(12.0, 0.0),
                Vec2::splat(24.0),
            ),
            icon,
            INK,
        );
        let text_left = row.left() + 44.0;
        let text_width = (row.width() - 130.0).max(20.0);
        let text = |value: &str, size, color| {
            let mut job = egui::text::LayoutJob::simple_singleline(
                value.into(),
                egui::FontId::proportional(size),
                color,
            );
            job.wrap.max_width = text_width;
            job.wrap.max_rows = 1;
            ui.painter().layout_job(job)
        };
        let title = text(label, 13.0, INK);
        let subtitle = text(description, 12.0, MUTED);
        let height = title.size().y
            + if description.is_empty() {
                0.0
            } else {
                2.0 + subtitle.size().y
            };
        let y = row.center().y - height * 0.5;
        let title_height = title.size().y;
        ui.painter().galley(egui::pos2(text_left, y), title, INK);
        if !description.is_empty() {
            ui.painter().galley(
                egui::pos2(text_left, y + title_height + 2.0),
                subtitle,
                MUTED,
            );
            response.clone().on_hover_text(description);
        }
        response
            .widget_info(|| egui::WidgetInfo::labeled(egui::Role::Label, ui.is_enabled(), label));
        let control = egui::Rect::from_center_size(
            row.right_center() - Vec2::new(20.0, 0.0),
            Vec2::new(40.0, 20.0),
        );
        ui.scope_builder(egui::UiBuilder::new().max_rect(control), |ui| {
            changed = art::toggle(ui, value).changed();
        });
        ui.painter().text(
            egui::pos2(control.left() - 12.0, row.center().y),
            egui::Align2::RIGHT_CENTER,
            if *value { "开" } else { "关" },
            egui::FontId::proportional(13.0),
            INK,
        );
    });
    ui.add_space(2.0);
    changed
}

impl Desktop {
    /// Only invoked by the explicit local stdin diagnostic mode. Opening a
    /// dialog must never submit its action or change remote state.
    pub(in crate::app) fn diagnostic_dialog(&mut self, name: &str) -> Option<egui::ViewportId> {
        match name {
            "files" if !self.connected && !self.connecting => {
                self.ui_state.windows.files = true;
                Some(self.viewport("files"))
            }
            "remote" if !self.connected && !self.connecting => {
                self.show_remote_window();
                Some(self.viewport("remote"))
            }
            "terminal" if !self.connected && !self.connecting => {
                self.ui_state.windows.terminal = true;
                Some(self.viewport("terminal"))
            }
            "text" if self.ui_state.windows.remote => {
                self.ui_state.text_window = true;
                Some(self.viewport("remote"))
            }
            "super-screen" if self.ui_state.windows.remote => {
                self.ui_state.super_confirm = Some(!self.ui_state.super_active);
                Some(self.viewport("remote"))
            }
            "new-folder" if self.ui_state.windows.files => {
                self.ui_state.file_dialog =
                    Some(("local_mkdir".into(), self.ui_state.local_path.clone()));
                self.ui_state.file_name.clear();
                Some(self.viewport("files"))
            }
            _ => None,
        }
    }
    pub(super) fn native_event(&mut self, kind: &str, data: Value) {
        if kind == "remote_streamer" {
            let p = &mut self.ui_state.prefs;
            if crate::capabilities::capture_supported(
                &data,
                &json!({"codec":p.codec,"true_color":p.true_color,"hdr":p.hdr}),
            ) == Some(false)
                && let Some(row) = data["video_codecs"].as_array().and_then(|rows| {
                    rows.iter()
                        .find(|r| r["bit_depth"] == 8 && r["chroma_sampling"] == 1)
                })
            {
                p.codec = if row["video_codec"] == 2 {
                    "h265"
                } else {
                    "h264"
                }
                .into();
                p.true_color = false;
                p.hdr = false;
                self.ui_state.last_notice = "已按本次远端能力调整画质选项".into();
            }
            self.ui_state.remote_streamer = data;
            return;
        }
        match kind {
            "microphone_submitted" => self.ui_state.microphone_pending = true,
            "microphone_policy" => {
                self.ui_state.microphone_pending = false;
                if data["accepted"] != true {
                    self.ui_state.last_notice = format!(
                        "麦克风未开启：{}",
                        data["error"]
                            .as_str()
                            .map(str::to_owned)
                            .unwrap_or_else(|| format!("远端错误 {}", data["error_code"]))
                    );
                }
            }
            "microphone_status" => self.ui_state.microphone_status = data,
            "preferences_changed" => {
                if let Ok(prefs) = serde_json::from_value::<Preferences>(
                    crate::preferences_store::read(&self.ui_state.path),
                ) {
                    *self.ui_state.saved_prefs.borrow_mut() = json!(prefs);
                    self.ui_state.prefs = prefs;
                }
            }
            "virtual_display" => {
                self.ui_state.last_notice = match data["phase"].as_str() {
                    Some("ready") => "虚拟屏操作已生效".into(),
                    Some("failed" | "timeout") => format!(
                        "虚拟屏操作失败：{}",
                        data["message"].as_str().unwrap_or("请检查远端支持情况")
                    ),
                    _ => "正在等待远端确认虚拟屏操作".into(),
                };
            }
            "session_usage" => {
                self.ui_state.resource_count = data["resources"].as_u64().unwrap_or(0) as usize
            }
            "service_result" => {
                if let Some(error) = data["error"].as_str() {
                    self.ui_state.mcp_notice = error.into();
                } else if data["name"] == "uu_capabilities_get" {
                    self.ui_state.capabilities = data["result"].clone();
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
                if data["source"] == "clipboard" {
                    self.ui_state.last_notice = "正在准备文件剪贴板，可在文件窗口查看进度".into();
                } else {
                    self.ui_state.windows.files = true;
                    self.ui_state.last_notice = "正在接收被控端拖拽发送的文件".into();
                }
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
        if matches!(
            command["op"].as_str(),
            Some("move" | "relative" | "click" | "button_down" | "button_up" | "scroll" | "touch")
        ) && let Some(geometry) = self.frame_geometry
        {
            command["geometry"] = json!(geometry);
        }
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
            "codec":p.codec,"true_color":p.true_color,"hdr":p.hdr,"dpi":p.dpi,
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
        self.ui_state.root_resize.intercept(
            &ctx,
            self.error.is_none()
                && self.ui_state.favorite_draft.is_none()
                && self.ui_state.download_draft.is_none()
                && self.ui_state.device_tools.dialog.is_none()
                && !self.ui_state.assist_prompt
                && !self.ui_state.add_help
                && !self.ui_state.about_open
                && self.ui_state.shortcut_capture.is_none()
                && !egui::Popup::is_any_open(&ctx),
        );
        ui.painter().rect_filled(ui.max_rect(), 0, SIDE);
        self.title_bar(ui);
        self.navigation(ui);
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(BG)
                    .stroke(egui::Stroke::new(1.0, Color32::from_rgb(231, 235, 238)))
                    .corner_radius(egui::CornerRadius {
                        nw: 6,
                        ne: 0,
                        sw: 0,
                        se: 0,
                    })
                    .inner_margin(egui::Margin {
                        left: 38,
                        right: 40,
                        top: 23,
                        bottom: 20,
                    }),
            )
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
            let response = modal(&ctx, "add-device-help", |ui| {
                modal_body(ui, "添加你的其他设备", |ui| {
                    ui.label(
                        "在其他设备安装并登录 UU 远程，使用同一个账号即可在设备列表中找到它。",
                    );
                    ui.label(
                        RichText::new("本应用提供 Linux 主控功能，不能作为被控设备。")
                            .size(12.0)
                            .color(MUTED),
                    );
                });
                modal_cancel(ui, "关闭")
            });
            if response.inner || response.should_close() {
                self.ui_state.add_help = false;
            }
        }
        if self.ui_state.about_open {
            let response = modal(&ctx, "about-uux", |ui| {
                modal_body(ui, "关于 UUX", |ui| {
                    ui.horizontal(|ui| {
                        let (rect, _) =
                            ui.allocate_exact_size(Vec2::splat(40.0), egui::Sense::hover());
                        art::logo(ui.painter(), rect.center(), 32.0);
                        ui.vertical(|ui| {
                            ui.label(RichText::new("UUX").size(20.0).strong());
                            ui.label(
                                RichText::new(concat!("版本 ", env!("CARGO_PKG_VERSION")))
                                    .color(MUTED),
                            );
                        });
                    });
                });
                modal_cancel(ui, "关闭")
            });
            if response.inner || response.should_close() {
                self.ui_state.about_open = false;
            }
        }
        self.error_dialog(&ctx);
        self.assistance_dialog(&ctx);
        self.favorite_dialog(&ctx);
        self.device_tools_dialog(&ctx);
        self.ui_state.root_resize.paint_cursor(&ctx);
        self.session_windows(&ctx);
        for session in &mut self.other_sessions {
            session.session_windows(&ctx);
        }
    }
    fn error_dialog(&mut self, ctx: &egui::Context) {
        let Some(error) = self.error.clone() else {
            return;
        };
        let mut close = false;
        let response = modal(ctx, "operation-error", |ui| {
            modal_body(ui, "操作提示", |ui| {
                ui.label(error);
            });
            close = modal_cancel(ui, "关闭");
        });
        if close || response.should_close() {
            self.error = None;
        }
    }
    fn remote_dialogs(&mut self, ctx: &egui::Context) {
        self.error_dialog(ctx);
        if let Some(enter) = self.ui_state.super_confirm {
            self.release();
            let response = modal(ctx, "super-screen-confirm", |ui| {
                modal_body(
                    ui,
                    if enter {
                        "进入超级屏"
                    } else {
                        "退出超级屏"
                    },
                    |ui| {
                        ui.label(if enter {
                            "切换到远端超级屏，画面会短暂中断。"
                        } else {
                            "退出超级屏并返回远端显示器，画面会短暂中断。"
                        });
                        if enter {
                            ui.label(
                                RichText::new("之后可在控制中心退出超级屏，返回远端显示器。")
                                    .size(12.0)
                                    .color(MUTED),
                            );
                        }
                    },
                );
                modal_actions(
                    ui,
                    if enter {
                        "进入超级屏"
                    } else {
                        "退出超级屏"
                    },
                    !self.ui_state.super_pending,
                    "取消",
                )
            });
            if response.inner.0 {
                self.ui_state.screen_choice_pending = Some(enter);
                self.control(json!({"op":"super_screen","enabled":enter}));
                self.ui_state.super_confirm = None;
            } else if response.inner.1 || response.should_close() {
                self.ui_state.super_confirm = None;
            }
        }
        if self.ui_state.text_window {
            let response = modal(ctx, "remote-text", |ui| {
                modal_body(ui, "输入文字", |ui| {
                    ui.label("在本机输入或粘贴文字，提交到远端当前输入位置");
                    ui.add(
                        egui::TextEdit::multiline(&mut self.ui_state.text)
                            .desired_width(398.0)
                            .desired_rows(4),
                    );
                });
                modal_actions(
                    ui,
                    "提交文字",
                    !self.ui_state.text.is_empty() && !self.ui_state.prefs.read_only,
                    "关闭",
                )
            });
            if response.inner.0 {
                let text = std::mem::take(&mut self.ui_state.text);
                self.control(json!({"op":"text","text":text}));
            }
            if response.inner.1 || response.should_close() {
                self.ui_state.text_window = false;
            }
        }
        if let Some((action, name)) = self.ui_state.remote_action.clone() {
            self.release();
            let response = modal(ctx, "remote-action", |ui| {
                modal_body(ui, "确认远端操作", |ui| {
                    ui.label(format!("确定要{name}远端电脑吗？"));
                });
                modal_actions(ui, "确定", true, "取消")
            });
            if response.inner.0 {
                self.control(json!({"op":"action","action":action}));
                self.ui_state.remote_action = None;
            } else if response.inner.1 || response.should_close() {
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
                            menu_layout(ui);
                            if menu_item(ui, Icon::Settings, "设置").clicked() {
                                self.ui_state.page = Page::Settings;
                                ui.close();
                            }
                            if menu_item(ui, Icon::Info, "如何添加设备").clicked() {
                                self.ui_state.add_help = true;
                                ui.close();
                            }
                            ui.separator();
                            if menu_item(ui, Icon::Info, "关于").clicked() {
                                self.ui_state.about_open = true;
                                ui.close();
                            }
                        });
                        let account = art::button(ui, Icon::Account, "账号");
                        egui::Popup::menu(&account).show(|ui| {
                            menu_layout(ui);
                            ui.label(if self.logged_in {
                                &self.nickname
                            } else {
                                "未登录"
                            });
                            ui.separator();
                            let label = if self.logged_in {
                                "切换账号"
                            } else {
                                "扫码登录"
                            };
                            if menu_item(ui, Icon::Account, label).clicked() {
                                self.login();
                                ui.close();
                            }
                            if self.logged_in && menu_item(ui, Icon::Exit, "退出账号").clicked()
                            {
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
                ui.add_space(4.0);
                egui::ScrollArea::vertical()
                    .max_height((ui.available_height() - 61.0).max(100.0))
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
                                if response.double_clicked() && device.online && !self.disconnecting
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
                            Icon::AssistGroup,
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
                let space = (bottom - ui.cursor().top() - 61.0).max(0.0);
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
                if response.double_clicked() && device.online && !self.disconnecting {
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
        ui.add_space(14.0);
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
                            self.show_remote_window();
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
                    && !self.disconnecting
                    && crate::core::cloud::assistance_id(&self.ui_state.assist_id).is_ok();
                if ui
                    .add_enabled_ui(allowed, |ui| {
                        art::primary(ui, "连接", Vec2::new(124.0, 32.0))
                    })
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
                    if ui
                        .add_enabled_ui(!self.ui_state.prefs.assist_history.is_empty(), |ui| {
                            art::icon_label(ui, Icon::Trash, "清除记录", Vec2::new(80.0, 24.0))
                        })
                        .inner
                        .clicked()
                    {
                        self.ui_state.prefs.assist_history.clear();
                        self.ui_state.save();
                    }
                });
            });
            ui.add_space(8.0);
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = Vec2::new(12.0, 12.0);
                for id in self.ui_state.prefs.assist_history.clone() {
                    let (select, favorite, remove) =
                        art::history_chip(ui, &id, self.is_favorite_partner(&id));
                    if select.clicked() {
                        self.ui_state.assist_id = id.clone();
                        self.ui_state.assist_code.clear();
                    }
                    if favorite.clicked() {
                        self.toggle_favorite_partner(&id);
                    }
                    if remove.clicked() {
                        self.ui_state
                            .prefs
                            .assist_history
                            .retain(|item| item != &id);
                        self.ui_state.save();
                    }
                }
                if self.ui_state.prefs.assist_history.is_empty() {
                    ui.label(RichText::new("暂无连接记录").size(12.0).color(MUTED));
                }
            });
            ui.add_space(10.0);
        });
    }
    fn assistance_dialog(&mut self, ctx: &egui::Context) {
        if !self.ui_state.assist_prompt {
            return;
        }
        let mut submit = false;
        let mut cancel = false;
        let response = modal(ctx, "assistance-code", |ui| {
            modal_body(ui, "连接伙伴设备", |ui| {
                ui.label(format!("设备 ID：{}", self.ui_state.assist_id));
                ui.label("设备验证码");
                let field = ui.add_sized(
                    [398.0, 32.0],
                    singleline(&mut self.ui_state.assist_code)
                        .password(true)
                        .char_limit(64)
                        .hint_text("请输入设备验证码"),
                );
                if !ui.ctx().egui_wants_keyboard_input() {
                    field.request_focus();
                }
                submit = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            });
            let allowed = !self.ui_state.assist_code.is_empty();
            let buttons = modal_actions(ui, "连接", allowed, "取消");
            submit = allowed && (submit || buttons.0);
            cancel = buttons.1;
        });
        if submit {
            self.ui_state.assist_prompt = false;
            self.connect_assistance();
        } else if cancel || response.should_close() {
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
        let code = std::mem::take(&mut self.ui_state.assist_code);
        let device = Device {
            id: format!("assist:{id}"),
            name: format!("伙伴设备 {id}"),
            online: true,
            shared: true,
            version: String::new(),
            wallpaper: None,
        };
        if self.activate_device_context(&device) {
            self.ui_state.windows.remote = true;
            return;
        }
        self.release();
        self.error = None;
        self.generation += 1;
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
        let can_open = device.online && !self.disconnecting;
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
                        self.show_remote_window();
                    } else {
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
                let transfer_color = if device.online { INK } else { MUTED };
                art::draw(
                    ui.painter(),
                    egui::Rect::from_center_size(
                        left.center() - Vec2::new(36.0, 0.0),
                        Vec2::splat(18.0),
                    ),
                    Icon::Transfer,
                    transfer_color,
                );
                ui.painter().text(
                    left.center() + Vec2::new(10.0, 0.0),
                    egui::Align2::CENTER_CENTER,
                    "文件传输",
                    egui::FontId::proportional(13.0),
                    transfer_color,
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
                        self.raise_window(self.viewport("files"));
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
                    let (brand_rect, brand) =
                        ui.allocate_exact_size(Vec2::new(30.0, 32.0), egui::Sense::click());
                    art::hover_background(ui, &brand, brand_rect);
                    art::logo(ui.painter(), brand_rect.center(), 22.0);
                    if brand.on_hover_text("显示设备列表").clicked() {
                        self.release();
                        self.raise_window(egui::ViewportId::ROOT);
                    }
                    // Preserve the right-side controls at the minimum window width.
                    let show_stats = ui.available_width() > 800.0;
                    let tabs_width = if show_stats { 240.0 } else { 190.0 };
                    let title_width = (ui.available_width()
                        - 276.0
                        - tabs_width
                        - 48.0
                        - if show_stats { 120.0 } else { 0.0 })
                    .clamp(40.0, 160.0);
                    let name = self
                        .active_device
                        .as_ref()
                        .map_or("远程桌面", |d| d.name.as_str());
                    let title = ui
                        .allocate_ui_with_layout(
                            Vec2::new(title_width, 26.0),
                            egui::Layout::left_to_right(egui::Align::Center),
                            |ui| {
                                ui.add(
                                    egui::Label::new(name)
                                        .truncate()
                                        .sense(egui::Sense::click_and_drag()),
                                )
                            },
                        )
                        .inner
                        .on_hover_text(name);
                    if title.drag_started() {
                        self.release();
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
                    }
                    ui.separator();
                    egui::ScrollArea::horizontal()
                        .id_salt(("screen-tabs", self.window_namespace))
                        .max_width(tabs_width)
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                let screens = self.ui_state.screens.clone();
                                for screen in screens.iter().filter(|s| s["id"].as_i64().is_some())
                                {
                                    let id = screen["id"].as_i64().unwrap() as i32;
                                    if self.ui_state.hidden_screens.contains(&id) {
                                        continue;
                                    }
                                    let selected = id == self.ui_state.screen;
                                    let title = if screen["screen_type"] == 2 {
                                        "超级屏"
                                    } else {
                                        screen["name"].as_str().unwrap_or("显示器")
                                    };
                                    ui.push_id(id, |ui| {
                                        let (tab, close) = art::screen_tab(ui, title, selected);
                                        if tab.clicked() && !close.clicked() && !selected {
                                            self.release();
                                            self.ui_state.screen = id;
                                            self.ui_state.resolution = [0, 0];
                                            self.apply_capture();
                                        }
                                        if close.clicked() {
                                            self.ui_state.hidden_screens.insert(id);
                                            if selected {
                                                if let Some(next) = screens
                                                    .iter()
                                                    .filter_map(|s| s["id"].as_i64())
                                                    .map(|id| id as i32)
                                                    .find(|id| {
                                                        !self.ui_state.hidden_screens.contains(id)
                                                    })
                                                {
                                                    self.release();
                                                    self.ui_state.screen = next;
                                                    self.ui_state.resolution = [0, 0];
                                                    self.apply_capture();
                                                } else {
                                                    self.close_remote_window();
                                                }
                                            }
                                        }
                                    });
                                }
                                if screens.is_empty() {
                                    ui.label("正在获取屏幕…");
                                }
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
                        Vec2::new((ui.available_width() - 284.0).max(0.0), 32.0),
                        egui::Sense::hover(),
                    );
                    let drag = ui.interact(
                        blank,
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
                        let control =
                            art::icon_label(ui, Icon::Control, "控制中心", Vec2::new(92.0, 28.0));
                        if egui::Popup::menu(&control)
                            .show(|ui| self.remote_menu(ui))
                            .is_none()
                        {
                            self.ui_state.custom_bitrate_mbps = None;
                        }
                        let microphone = self.ui_state.microphone_status["enabled"] == true;
                        let microphone_tip = if self.ui_state.microphone_pending {
                            "正在等待远端确认麦克风"
                        } else if microphone {
                            "关闭麦克风"
                        } else {
                            "开启麦克风"
                        };
                        if ui
                            .add_enabled_ui(
                                self.connected && !self.ui_state.microphone_pending,
                                |ui| {
                                    art::button(
                                        ui,
                                        if microphone {
                                            Icon::Microphone
                                        } else {
                                            Icon::MicrophoneMuted
                                        },
                                        microphone_tip,
                                    )
                                },
                            )
                            .inner
                            .on_disabled_hover_text(microphone_tip)
                            .clicked()
                        {
                            self.configure_microphone(!microphone);
                        }
                    });
                });
            });
    }
    fn configure_microphone(&mut self, enabled: bool) {
        if self.connected && !self.ui_state.microphone_pending {
            self.ui_state.microphone_pending = true;
            self.control(json!({"op":"microphone","enabled":enabled,"device_id":self.ui_state.prefs.microphone_device}));
        }
    }
    fn remote_menu(&mut self, ui: &mut egui::Ui) {
        let mut custom_visible = false;
        menu_layout(ui);
        ui.spacing_mut().interact_size.y = 30.0;
        ui.spacing_mut().item_spacing.y = 2.0;
        self.release();
        submenu(ui, Icon::Fullscreen, "超级屏", |ui| {
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
        submenu(ui, Icon::Picture, "画质", |ui| {
            for (q, name) in [
                (5, "自动（清晰）"),
                (4, "原画 20M"),
                (3, "高清 8M"),
                (2, "清晰 2M"),
            ] {
                if menu_choice(ui, &mut self.ui_state.prefs.quality, q, name).changed() {
                    self.apply_capture();
                }
            }
            let custom_open = text_submenu(
                ui,
                "自定义",
                Some(matches!(self.ui_state.prefs.quality, 1 | 6)),
                |ui| {
                    if menu_choice(ui, &mut self.ui_state.prefs.quality, 1, "流畅").changed() {
                        self.apply_capture();
                    }
                    ui.separator();
                    let mut bitrate = self
                        .ui_state
                        .custom_bitrate_mbps
                        .unwrap_or(f64::from(self.ui_state.prefs.bitrate) / 1_000_000.0);
                    ui.horizontal(|ui| {
                        ui.label("码率");
                        ui.add(
                            egui::DragValue::new(&mut bitrate)
                                .range(0.5..=40.0)
                                .speed(0.5)
                                .fixed_decimals(1)
                                .suffix(" Mbps"),
                        );
                    });
                    ui.spacing_mut().slider_width = 160.0;
                    ui.add(egui::Slider::new(&mut bitrate, 0.5..=40.0).show_value(false));
                    self.ui_state.custom_bitrate_mbps = Some(bitrate);
                    if art::primary(ui, "应用", Vec2::new(MENU_WIDTH, 30.0)).clicked() {
                        self.ui_state.prefs.quality = 6;
                        self.ui_state.prefs.bitrate = (bitrate * 1_000_000.0).round() as i32;
                        self.apply_capture();
                        ui.close();
                    }
                },
            );
            custom_visible = custom_open;
            if !custom_open {
                self.ui_state.custom_bitrate_mbps = None;
            }
            ui.separator();
            for fps in [144, 90, 60, 30] {
                if menu_choice(ui, &mut self.ui_state.prefs.fps, fps, format!("{fps} 帧")).changed()
                {
                    self.apply_capture();
                }
            }
            ui.separator();
            for (enabled, label) in [(true, "真彩"), (false, "常规色彩")] {
                let available = crate::capabilities::capture_supported(
                    &self.ui_state.remote_streamer,
                    &json!({"true_color":enabled}),
                ) != Some(false);
                if ui
                    .add_enabled_ui(available, |ui| {
                        menu_choice(ui, &mut self.ui_state.prefs.true_color, enabled, label)
                    })
                    .inner
                    .on_disabled_hover_text("远端本次会话未提供此色彩格式")
                    .changed()
                {
                    self.apply_capture();
                }
            }
            let hdr_available = crate::capabilities::capture_supported(
                &self.ui_state.remote_streamer,
                &json!({"hdr":true}),
            ) != Some(false)
                && crate::core::pixels::RgbaConverter::hdr_supported();
            if ui
                .add_enabled_ui(hdr_available, |ui| {
                    menu_toggle(ui, &mut self.ui_state.prefs.hdr, "HDR")
                })
                .inner
                .on_disabled_hover_text(
                    "需要远端提供 10-bit 编码，以及本机 FFmpeg 的 zscale、tonemap 滤镜",
                )
                .on_hover_text(
                    "远端需支持 HDR；本地窗口进行 HDR 到 SDR 色调映射，MCP 编码流保留原始数据",
                )
                .changed()
            {
                self.apply_capture();
            }
            ui.separator();
            text_submenu(ui, "视频编码", None, |ui| {
                for (codec, label) in [("h264", "H.264"), ("h265", "H.265")] {
                    let available = crate::capabilities::capture_supported(
                        &self.ui_state.remote_streamer,
                        &json!({"codec":codec}),
                    ) != Some(false);
                    if ui
                        .add_enabled_ui(available, |ui| {
                            menu_choice(ui, &mut self.ui_state.prefs.codec, codec.to_owned(), label)
                        })
                        .inner
                        .on_disabled_hover_text("远端本次会话未提供此编码")
                        .changed()
                    {
                        self.apply_capture();
                    }
                }
            });
            text_submenu(ui, "本地解码", None, |ui| {
                for (preference, label) in [
                    ("auto", "自动（优先硬件）"),
                    ("software", "软件"),
                    ("vaapi", "VA-API"),
                    ("cuda", "NVIDIA CUDA"),
                ] {
                    if menu_choice(
                        ui,
                        &mut self.ui_state.prefs.decoder_preference,
                        preference.into(),
                        label,
                    )
                    .changed()
                    {
                        self.control(json!({"op":"decoder","preference":preference}));
                    }
                }
            });
        });
        submenu(ui, Icon::Fullscreen, "窗口", |ui| {
            text_submenu(ui, "显示器", None, |ui| {
                for screen in self.ui_state.screens.clone() {
                    let id = screen["id"].as_i64().unwrap_or(0) as i32;
                    if menu_choice(
                        ui,
                        &mut self.ui_state.screen,
                        id,
                        screen["name"].as_str().unwrap_or("显示器"),
                    )
                    .changed()
                    {
                        self.ui_state.hidden_screens.remove(&id);
                        self.ui_state.resolution = [0, 0];
                        self.apply_capture();
                    }
                }
            });
            text_submenu(ui, "分辨率", None, |ui| {
                if menu_choice(ui, &mut self.ui_state.resolution, [0, 0], "跟随远端").changed()
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
                        && menu_choice(
                            ui,
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
            text_submenu(ui, "虚拟屏", None, |ui| {
                if ui
                    .add_enabled_ui(
                        !self.ui_state.screens.iter().any(|s| s["screen_type"] == 2),
                        |ui| menu_label(ui, "添加虚拟屏", None),
                    )
                    .inner
                    .on_disabled_hover_text("请先退出超级屏，再添加普通虚拟屏")
                    .clicked()
                {
                    self.control(
                        json!({"op":"virtual_display_create","count":1,"width":1920,"height":1080}),
                    );
                    ui.close();
                }
                for screen in self
                    .ui_state
                    .screens
                    .clone()
                    .iter()
                    .filter(|s| s["screen_type"] == 1)
                {
                    text_submenu(
                        ui,
                        &format!("移除 {}", screen["name"].as_str().unwrap_or("虚拟屏")),
                        None,
                        |ui| {
                            ui.label("将移除远端虚拟显示器");
                            if menu_label(ui, "确认移除", None).clicked() {
                                self.control(json!({"op":"virtual_display_remove","screen_ids":[screen["id"]]}));
                                ui.close();
                            }
                        },
                    );
                }
            });
            text_submenu(ui, "DPI 缩放", None, |ui| {
                let dpis = self
                    .ui_state
                    .screens
                    .iter()
                    .find(|s| s["id"] == self.ui_state.screen)
                    .and_then(|s| s["dpis"].as_array())
                    .cloned()
                    .unwrap_or_default();
                for dpi in dpis.into_iter().filter_map(|v| v.as_i64()) {
                    if menu_choice(
                        ui,
                        &mut self.ui_state.prefs.dpi,
                        dpi as i32,
                        format!("{dpi}%"),
                    )
                    .changed()
                    {
                        self.apply_capture();
                    }
                }
            });
            ui.separator();
            if menu_toggle(ui, &mut self.ui_state.prefs.scale_up, "允许画面放大").changed() {
                self.ui_state.save();
            }
        });
        submenu(ui, Icon::Sound, "声音", |ui| {
            if menu_toggle(ui, &mut self.ui_state.prefs.audio, "播放远端声音").changed() {
                self.control(json!({"op":"audio","enabled":self.ui_state.prefs.audio}));
                self.ui_state.save();
            }
            text_submenu(ui, "声音输出设备", None, |ui| {
                let mut selected = self.ui_state.prefs.output_device.clone();
                menu_choice(ui, &mut selected, "default".into(), "跟随系统默认");
                for device in
                    self.ui_state.capabilities["local"]["audio_devices"]["output"]["devices"]
                        .as_array()
                        .into_iter()
                        .flatten()
                {
                    if let Some(id) = device["id"].as_str() {
                        menu_choice(
                            ui,
                            &mut selected,
                            id.to_owned(),
                            device["name"].as_str().unwrap_or(id),
                        );
                    }
                }
                if selected != self.ui_state.prefs.output_device {
                    self.ui_state.prefs.output_device = selected;
                    self.control(json!({"op":"audio","enabled":self.ui_state.prefs.audio,"device_id":self.ui_state.prefs.output_device}));
                    self.ui_state.save();
                }
                ui.separator();
                if menu_label(ui, "刷新设备", None).clicked() {
                    self.send(
                        json!({"op":"service_call","name":"uu_capabilities_get","arguments":{}}),
                    );
                }
            });
            ui.separator();
            let mut microphone = self.ui_state.microphone_status["enabled"] == true;
            if ui
                .add_enabled_ui(self.connected && !self.ui_state.microphone_pending, |ui| {
                    menu_toggle(ui, &mut microphone, "发送本地麦克风")
                })
                .inner
                .changed()
            {
                self.configure_microphone(microphone);
            }
            text_submenu(ui, "麦克风设备", None, |ui| {
                let mut selected = self.ui_state.prefs.microphone_device.clone();
                menu_choice(ui, &mut selected, "default".into(), "跟随系统默认");
                for device in
                    self.ui_state.capabilities["local"]["audio_devices"]["input"]["devices"]
                        .as_array()
                        .into_iter()
                        .flatten()
                {
                    if let Some(id) = device["id"].as_str() {
                        menu_choice(
                            ui,
                            &mut selected,
                            id.to_owned(),
                            device["name"].as_str().unwrap_or(id),
                        );
                    }
                }
                if selected != self.ui_state.prefs.microphone_device {
                    self.ui_state.prefs.microphone_device = selected;
                    if microphone {
                        self.configure_microphone(true);
                    }
                    self.ui_state.save();
                }
                if menu_label(ui, "刷新设备", None).clicked() {
                    self.send(
                        json!({"op":"service_call","name":"uu_capabilities_get","arguments":{}}),
                    );
                }
            });
            if self.ui_state.microphone_pending {
                ui.weak("等待远端确认麦克风");
            }
            if let Some(error) = self.ui_state.microphone_status["error"].as_str() {
                ui.label(RichText::new(error).color(MUTED));
            }
        });
        submenu(ui, Icon::Shield, "安全", |ui| {
            if menu_toggle(ui, &mut self.ui_state.prefs.read_only, "仅观看，禁用键鼠").changed()
            {
                self.release();
                self.apply_security();
            }
            if menu_toggle(
                ui,
                &mut self.ui_state.prefs.lock_on_disconnect,
                "断开后锁定远端",
            )
            .changed()
            {
                self.apply_security();
            }
            if menu_label(ui, "锁定远端电脑", None).clicked() {
                self.ui_state.remote_action = Some((10, "锁定".into()));
                ui.close();
            }
        });
        submenu(ui, Icon::Usb, "外设", |ui| {
            if menu_toggle(ui, &mut self.ui_state.prefs.ime, "使用本机输入法").changed() {
                self.ui_state.save();
            }
            if menu_toggle(ui, &mut self.ui_state.prefs.clipboard, "同步剪贴板").changed() {
                if !self.ui_state.prefs.clipboard {
                    self.control(json!({"op":"clipboard_focus","enabled":false}));
                }
                self.ui_state.save();
            }
            ui.separator();
            ui.add_enabled_ui(!self.ui_state.prefs.read_only, |ui| {
                if menu_item(ui, Icon::Keyboard, "输入文字").clicked() {
                    self.ui_state.text_window = true;
                    ui.close();
                }
                if menu_label(ui, "粘贴本机剪贴板", None).clicked() {
                    self.control(json!({"op":"paste_local"}));
                    ui.close();
                }
            });
        });
        submenu(ui, Icon::Grid, "更多", |ui| {
            if menu_item(ui, Icon::Transfer, "文件传输").clicked() {
                self.open_files();
                ui.close();
            }
            ui.separator();
            for (action, name) in [(2, "显示桌面"), (13, "任务管理器")] {
                if menu_label(ui, name, None).clicked() {
                    self.control(json!({"op":"action","action":action}));
                    ui.close();
                }
            }
            ui.separator();
            for (action, name) in [(9, "重启"), (12, "关闭")] {
                if menu_label(ui, &format!("{name}远端电脑"), None).clicked() {
                    self.ui_state.remote_action = Some((action, name.into()));
                    ui.close();
                }
            }
        });
        ui.separator();
        let fullscreen = menu_item(
            ui,
            Icon::Fullscreen,
            if self.fullscreen {
                "退出全屏"
            } else {
                "全屏"
            },
        );
        let mut hint = egui::text::LayoutJob::simple_singleline(
            self.ui_state.prefs.fullscreen_shortcut.clone(),
            egui::FontId::proportional(11.0),
            MUTED,
        );
        hint.wrap.max_width = 78.0;
        hint.wrap.max_rows = 1;
        let hint = ui.painter().layout_job(hint);
        ui.painter().galley(
            fullscreen.rect.right_center() - Vec2::new(hint.size().x + 8.0, hint.size().y / 2.0),
            hint,
            MUTED,
        );
        if fullscreen
            .on_hover_text(&self.ui_state.prefs.fullscreen_shortcut)
            .clicked()
        {
            self.fullscreen = !self.fullscreen;
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::Fullscreen(self.fullscreen));
            ui.close();
        }
        if menu_item_color(ui, Icon::Exit, "退出远控", Color32::from_rgb(255, 70, 50)).clicked()
        {
            self.close_remote_window();
            ui.close();
        }
        if !custom_visible {
            self.ui_state.custom_bitrate_mbps = None;
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
            if self.connected
                && self.ui_state.prefs.drag_send
                && !self.ui_state.prefs.read_only
                && !egui::Popup::is_any_open(ui.ctx())
                && let Some(point) = pointer.filter(|p| rect.contains(*p))
            {
                let paths = ui.input(|i| {
                    i.raw
                        .dropped_files
                        .iter()
                        .map(|f| f.path().to_owned())
                        .collect::<Vec<_>>()
                });
                if !paths.is_empty()
                    && let Some(geometry) = self.frame_geometry
                {
                    self.release();
                    self.control(json!({"op":"clipboard_files","paths":paths,"geometry":geometry,"drop":{"screen_id":geometry.screen_id,"x":(point.x-rect.left())/rect.width(),"y":(point.y-rect.top())/rect.height()}}));
                }
            }

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

#[cfg(test)]
mod menu_layout_tests {
    use super::*;

    #[test]
    fn action_menus_keep_one_width_after_wide_content_and_at_different_scales() {
        for scale in [1.0, 1.25, 1.5, 2.0] {
            let ctx = egui::Context::default();
            super::super::setup(&ctx);
            ctx.set_pixels_per_point(scale);
            let mut time = 0.0;
            let mut render = |legacy_wide: bool, pointer: egui::Pos2| {
                time += 0.05;
                let mut popup_rect = egui::Rect::NOTHING;
                let mut anchor = egui::Rect::NOTHING;
                let mut child_row = egui::Rect::NOTHING;
                ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            Vec2::new(1400.0, 900.0),
                        )),
                        time: Some(time),
                        events: vec![egui::Event::PointerMoved(pointer)],
                        ..Default::default()
                    },
                    |ui| {
                        let button = ui.add_sized([900.0, 40.0], egui::Button::new("更多"));
                        let popup = egui::Popup::menu(&button).open(true).show(|ui| {
                            if legacy_wide {
                                ui.set_width(600.0);
                            } else {
                                menu_layout(ui);
                            }
                            // Account names and file paths must not expand the menu.
                            ui.label("a-very-long-name/".repeat(30));
                            let row = menu_item(ui, Icon::Sound, "声音");
                            anchor = row.rect;
                            show_submenu(ui, row, |ui| {
                                child_row =
                                    menu_label(ui, "a-very-long-output-device-name", None).rect;
                            });
                        });
                        popup_rect = popup.unwrap().response.rect;
                    },
                )
                .drop_without_applying_deltas();
                (popup_rect, anchor, child_row)
            };
            for _ in 0..3 {
                render(true, egui::Pos2::ZERO);
            }
            let mut observed = render(false, egui::Pos2::ZERO);
            for _ in 0..8 {
                observed = render(false, observed.1.center());
            }
            assert!(
                (observed.1.width() - MENU_WIDTH).abs() < 0.5,
                "main row: {observed:?}"
            );
            assert!(
                (observed.2.width() - MENU_WIDTH).abs() < 0.5,
                "submenu row: {observed:?}"
            );
            assert!(
                observed.0.width() <= MENU_WIDTH + 20.0,
                "popup kept its old width: {observed:?}"
            );
        }
    }

    #[test]
    fn long_submenu_fits_small_windows_scrolls_and_expands_after_resize() {
        let ctx = egui::Context::default();
        super::super::setup(&ctx);
        let mut time = 0.0;
        let mut render = |size: Vec2, events: Vec<egui::Event>| {
            time += 0.05;
            let mut anchor = egui::Rect::NOTHING;
            let mut clip = egui::Rect::NOTHING;
            let mut first = egui::Rect::NOTHING;
            ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
                    time: Some(time),
                    events,
                    ..Default::default()
                },
                |ui| {
                    let button = ui.put(
                        egui::Rect::from_min_size(
                            egui::pos2(size.x - 200.0, 8.0),
                            Vec2::new(92.0, 28.0),
                        ),
                        egui::Button::new("控制中心"),
                    );
                    egui::Popup::menu(&button).open(true).show(|ui| {
                        ui.set_width(192.0);
                        anchor = egui::Rect::from_min_size(
                            ui.next_widget_position(),
                            Vec2::new(192.0, 30.0),
                        );
                        submenu(ui, Icon::Devices, "画质", |ui| {
                            clip = ui.clip_rect();
                            let mut selected = 0;
                            for item in 0..18 {
                                let response =
                                    menu_choice(ui, &mut selected, item, format!("选项 {item}"));
                                if item == 0 {
                                    first = response.rect;
                                }
                            }
                        });
                    });
                },
            )
            .drop_without_applying_deltas();
            (anchor, clip, first)
        };
        let small = Vec2::new(640.0, 400.0);
        render(small, vec![]);
        let (mut anchor, _, _) = render(small, vec![]);
        let mut observed = (anchor, egui::Rect::NOTHING, egui::Rect::NOTHING);
        for _ in 0..6 {
            observed = render(small, vec![egui::Event::PointerMoved(anchor.center())]);
            anchor = observed.0;
        }
        let (_, clip, first) = observed;
        assert!(
            clip.is_finite() && clip.height() > 100.0,
            "submenu did not open: {clip:?}"
        );
        assert!(
            clip.bottom() <= small.y && first.right() < anchor.left(),
            "submenu overlaps its parent or viewport: {clip:?}, row: {first:?}, anchor: {anchor:?}"
        );
        render(
            small,
            vec![egui::Event::PointerMoved(egui::pos2(
                first.center().x,
                clip.center().y,
            ))],
        );
        let wheel = egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: Vec2::new(0.0, -200.0),
            phase: egui::TouchPhase::Move,
            source: egui::MouseWheelSource::Unknown,
            modifiers: Default::default(),
        };
        let mut scrolled = render(small, vec![wheel]);
        for _ in 0..8 {
            scrolled = render(small, vec![]);
        }
        assert!(
            scrolled.2.top() < first.top() - 50.0,
            "mouse wheel did not scroll the submenu"
        );
        let large = Vec2::new(1280.0, 760.0);
        anchor = render(large, vec![]).0;
        for _ in 0..6 {
            observed = render(large, vec![egui::Event::PointerMoved(anchor.center())]);
            anchor = observed.0;
        }
        assert!(
            observed.1.height() > 550.0,
            "large window retained the small menu height: {:?}",
            observed.1
        );
    }
}

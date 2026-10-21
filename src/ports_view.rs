use super::*;
use crate::core::ports::RuleConfig;
use std::collections::HashMap;

#[derive(Default)]
pub(super) struct PortsView {
    status: HashMap<u64, Value>,
    draft: Option<Draft>,
    confirm_close: bool,
    resize: window_resize::ResizeGrip,
}
struct Draft {
    id: u64,
    device_id: String,
    name: String,
    local: String,
    host: String,
    port: String,
    error: Option<String>,
}
impl Draft {
    fn from_rule(rule: RuleConfig) -> Self {
        Self {
            id: rule.id,
            device_id: rule.device_id,
            name: rule.name,
            local: rule.local_port.to_string(),
            host: rule.target_host,
            port: rule.target_port.to_string(),
            error: None,
        }
    }
    fn rule(&self) -> Result<RuleConfig, String> {
        let rule = RuleConfig {
            id: self.id,
            device_id: self.device_id.clone(),
            name: self.name.trim().into(),
            local_port: self.local.parse().map_err(|_| "本地端口无效")?,
            target_host: self.host.trim().into(),
            target_port: self.port.parse().map_err(|_| "目标端口无效")?,
        };
        rule.validate().map_err(|e| e.to_string())?;
        Ok(rule)
    }
}
pub(super) fn viewport_id() -> egui::ViewportId {
    crate::backend::ports_viewport_id()
}
impl Desktop {
    pub(super) fn port_status(&mut self, data: Value) {
        if let Some(id) = data["id"].as_u64() {
            self.ui_state.ports.status.insert(id, data);
        }
    }
    pub(super) fn open_ports_for(&mut self, device: Device) {
        if !self.connected && !self.connecting {
            self.connect_for_tools(device);
        }
        self.ui_state.windows.ports = true;
    }
    fn change_port(&mut self, rule: &RuleConfig, enabled: bool) {
        self.ui_state.ports.status.insert(
            rule.id,
            json!({"enabled":enabled,"pending":true,"message":"处理中…"}),
        );
        self.control(json!({"op":"port_mapping","id":rule.id,"enabled":enabled,"rule":rule}));
    }
    fn close_ports(&mut self) {
        let ids = self
            .ui_state
            .ports
            .status
            .iter()
            .filter(|(_, s)| s["enabled"] == true)
            .map(|(&id, _)| id)
            .collect::<Vec<_>>();
        for id in ids {
            self.control(json!({"op":"port_mapping","id":id,"enabled":false}));
        }
        self.ui_state.windows.ports = false;
        self.ui_state.ports = Default::default();
    }
    pub(super) fn ports_window(&mut self, ctx: &egui::Context) {
        if !self.ui_state.windows.ports {
            return;
        }
        ctx.show_viewport_immediate(
            viewport_id(),
            egui::ViewportBuilder::default()
                .with_title("端口映射 — UUX")
                .with_inner_size([920.0, 680.0])
                .with_min_inner_size([800.0, 480.0])
                .with_decorations(false)
                .with_resizable(true),
            |ui, _| {
                let resizing = self.ui_state.ports.resize.intercept(
                    ui.ctx(),
                    self.ui_state.ports.draft.is_none() && !self.ui_state.ports.confirm_close,
                );
                let mut close = ui.input(|i| i.viewport().close_requested());
                egui::Panel::top("port-title")
                    .frame(egui::Frame::new().fill(SIDE).inner_margin(4))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            let (r, response) = ui.allocate_exact_size(
                                Vec2::new((ui.available_width() - 82.0).max(80.0), 24.0),
                                egui::Sense::click_and_drag(),
                            );
                            ui.painter().text(
                                r.left_center() + Vec2::new(8.0, 0.0),
                                egui::Align2::LEFT_CENTER,
                                "端口映射",
                                egui::FontId::proportional(12.0),
                                INK,
                            );
                            if !resizing && response.drag_started() {
                                ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
                            }
                            if art::flat_button(ui, "−", Vec2::new(32.0, 24.0)).clicked() {
                                ui.ctx()
                                    .send_viewport_cmd(egui::ViewportCommand::Minimized(true));
                            }
                            if art::flat_button(ui, "×", Vec2::new(32.0, 24.0)).clicked() {
                                close = true;
                            }
                        });
                    });
                if close {
                    ui.ctx()
                        .send_viewport_cmd(egui::ViewportCommand::CancelClose);
                    if self
                        .ui_state
                        .ports
                        .status
                        .values()
                        .any(|s| s["enabled"] == true)
                    {
                        self.ui_state.ports.confirm_close = true;
                    } else {
                        self.close_ports();
                        return;
                    }
                }
                egui::CentralPanel::default()
                    .frame(egui::Frame::new().fill(SIDE).inner_margin(egui::Margin {
                        left: 24,
                        right: 24,
                        top: 20,
                        bottom: 24,
                    }))
                    .show(ui, |ui| self.port_list(ui));
                self.port_dialog(ui.ctx());
                if self.ui_state.ports.confirm_close {
                    egui::Modal::new(egui::Id::unique("port-close")).show(ui.ctx(), |ui| {
                        ui.set_width(360.0);
                        ui.label("关闭端口映射窗口将停止所有已启用的映射。");
                        ui.add_space(16.0);
                        ui.horizontal(|ui| {
                            if primary(ui, "关闭并停止").clicked() {
                                self.close_ports();
                            }
                            if ui.button("取消").clicked() {
                                self.ui_state.ports.confirm_close = false;
                            }
                        });
                    });
                }
                self.ui_state.ports.resize.paint_cursor(ui.ctx());
                self.diagnostics(ui.ctx());
            },
        );
    }
    fn port_list(&mut self, ui: &mut egui::Ui) {
        let Some(device) = self.active_device.clone() else {
            return;
        };
        ui.horizontal(|ui| {
            ui.label(RichText::new(&device.name).font(fonts::bold(24.0)));
            ui.add_space(6.0);
            ui.colored_label(
                if self.connected {
                    Color32::from_rgb(0, 199, 145)
                } else {
                    MUTED
                },
                "●",
            );
            ui.label(if self.connected {
                "已连接"
            } else if self.connecting {
                "连接中…"
            } else {
                "未连接"
            });
        });
        ui.add_space(18.0);
        egui::Frame::new()
            .fill(BG)
            .stroke(egui::Stroke::new(1.0, BORDER))
            .corner_radius(3)
            .show(ui, |ui| {
                ui.set_min_size(ui.available_size());
                egui::Frame::new()
                    .inner_margin(egui::Margin {
                        left: 16,
                        right: 16,
                        top: 8,
                        bottom: 8,
                    })
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("映射列表").font(fonts::bold(16.0)));
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if art::flat_button(ui, "⊕ 添加", Vec2::new(60.0, 28.0))
                                        .clicked()
                                    {
                                        self.ui_state.ports.draft = Some(Draft {
                                            id: (uuid::Uuid::new_v4().as_u128() as u64).max(1),
                                            device_id: device.id.clone(),
                                            name: String::new(),
                                            local: String::new(),
                                            host: "127.0.0.1".into(),
                                            port: String::new(),
                                            error: None,
                                        });
                                    }
                                },
                            );
                        });
                    });
                ui.spacing_mut().item_spacing.y = 0.0;
                let fractions = [0.0, 0.21, 0.35, 0.49, 0.63, 0.81, 1.0];
                let (head, _) = ui.allocate_exact_size(
                    Vec2::new(ui.available_width(), 30.0),
                    egui::Sense::hover(),
                );
                ui.painter()
                    .hline(head.x_range(), head.top(), egui::Stroke::new(1.0, BORDER));
                for (index, label) in [
                    "名称",
                    "本地端口",
                    "目标地址",
                    "目标端口",
                    "映射状态",
                    "操作",
                ]
                .into_iter()
                .enumerate()
                {
                    let x = head.left() + head.width() * fractions[index];
                    ui.painter().text(
                        egui::pos2(x + 8.0, head.center().y),
                        egui::Align2::LEFT_CENTER,
                        label,
                        egui::FontId::proportional(13.0),
                        MUTED,
                    );
                    if index > 0 {
                        ui.painter()
                            .vline(x, head.y_range(), egui::Stroke::new(1.0, BORDER));
                    }
                }
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        for rule in self
                            .ui_state
                            .prefs
                            .port_rules
                            .clone()
                            .into_iter()
                            .filter(|r| r.device_id == device.id)
                        {
                            ui.push_id(rule.id, |ui| {
                                let (row, response) = ui.allocate_exact_size(
                                    Vec2::new(ui.available_width(), 48.0),
                                    egui::Sense::hover(),
                                );
                                if response.hovered() {
                                    ui.painter().rect_filled(row, 0, art::SELECTED);
                                }
                                let status = self
                                    .ui_state
                                    .ports
                                    .status
                                    .get(&rule.id)
                                    .cloned()
                                    .unwrap_or(json!({"message":"未启用"}));
                                let values = [
                                    rule.name.clone(),
                                    rule.local_port.to_string(),
                                    rule.target_host.clone(),
                                    rule.target_port.to_string(),
                                    status["message"].as_str().unwrap_or("").to_owned(),
                                ];
                                for (index, label) in values.into_iter().enumerate() {
                                    let cell = egui::Rect::from_min_max(
                                        egui::pos2(
                                            row.left() + row.width() * fractions[index] + 8.0,
                                            row.top(),
                                        ),
                                        egui::pos2(
                                            row.left() + row.width() * fractions[index + 1] - 8.0,
                                            row.bottom(),
                                        ),
                                    );
                                    let mut cell_ui =
                                        ui.new_child(egui::UiBuilder::new().max_rect(cell).layout(
                                            egui::Layout::left_to_right(egui::Align::Center),
                                        ));
                                    cell_ui
                                        .add(
                                            egui::Label::new(RichText::new(label).color(
                                                if index == 4 && status["status"] == "failed" {
                                                    Color32::from_rgb(200, 60, 60)
                                                } else {
                                                    INK
                                                },
                                            ))
                                            .truncate()
                                            .halign(egui::Align::Min),
                                        )
                                        .on_hover_text(if index == 4 {
                                            status["message"].as_str().unwrap_or("")
                                        } else {
                                            ""
                                        });
                                }
                                let rect = egui::Rect::from_min_size(
                                    egui::pos2(
                                        row.left() + row.width() * fractions[5] + 10.0,
                                        row.center().y - 11.0,
                                    ),
                                    Vec2::new(40.0, 22.0),
                                );
                                let mut enabled = status["enabled"] == true;
                                let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
                                if !self.connected || status["pending"] == true {
                                    child.disable();
                                }
                                if art::toggle(&mut child, &mut enabled).changed() {
                                    self.change_port(&rule, enabled);
                                }
                                let menu = ui.place(
                                    egui::Rect::from_center_size(
                                        egui::pos2(row.right() - 25.0, row.center().y),
                                        Vec2::new(32.0, 28.0),
                                    ),
                                    egui::Button::new("···").frame(false),
                                );
                                egui::Popup::menu(&menu).show(|ui| {
                                    if menu_item(ui, Icon::Edit, "编辑").clicked() {
                                        self.ui_state.ports.draft =
                                            Some(Draft::from_rule(rule.clone()));
                                        ui.close();
                                    }
                                    if menu_item(ui, Icon::Devices, "创建副本").clicked() {
                                        let mut copy = rule.clone();
                                        copy.id = (uuid::Uuid::new_v4().as_u128() as u64).max(1);
                                        copy.name = format!("{} 副本", copy.name);
                                        let mut draft = Draft::from_rule(copy);
                                        draft.local.clear();
                                        self.ui_state.ports.draft = Some(draft);
                                        ui.close();
                                    }
                                    if menu_item(ui, Icon::Trash, "删除").clicked() {
                                        if enabled {
                                            self.change_port(&rule, false);
                                        }
                                        self.ui_state.prefs.port_rules.retain(|r| r.id != rule.id);
                                        self.ui_state.save();
                                        ui.close();
                                    }
                                });
                            });
                        }
                    });
            });
    }
    fn port_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut draft) = self.ui_state.ports.draft.take() else {
            return;
        };
        let editing = self
            .ui_state
            .prefs
            .port_rules
            .iter()
            .any(|r| r.id == draft.id);
        let mut save = false;
        let mut cancel = false;
        let response = egui::Modal::new(egui::Id::unique("port-rule-editor")).show(ctx, |ui| {
            ui.set_width(400.0);
            ui.label(
                RichText::new(if editing {
                    "编辑端口映射"
                } else {
                    "添加端口映射"
                })
                .size(16.0),
            );
            ui.add_space(18.0);
            egui::Grid::new("port-rule-fields")
                .spacing([20.0, 16.0])
                .show(ui, |ui| {
                    for (label, text, hint) in [
                        ("名称", &mut draft.name, "映射名称"),
                        ("本地端口", &mut draft.local, "1–65535"),
                        ("目标地址", &mut draft.host, "127.0.0.1"),
                        ("目标端口", &mut draft.port, "1–65535"),
                    ] {
                        ui.label(label);
                        ui.add_sized([280.0, 32.0], singleline(text).hint_text(hint));
                        ui.end_row();
                    }
                });
            if let Some(error) = &draft.error {
                ui.colored_label(Color32::from_rgb(200, 60, 60), error);
            }
            ui.add_space(24.0);
            ui.horizontal(|ui| {
                save = primary(ui, if editing { "保存更改" } else { "添加" }).clicked();
                cancel = ui
                    .add_sized([112.0, 30.0], egui::Button::new("取消"))
                    .clicked();
            });
        });
        if save {
            let result = draft.rule().and_then(|rule| {
                if self.ui_state.prefs.port_rules.iter().any(|r| {
                    r.id != rule.id
                        && r.device_id == rule.device_id
                        && r.local_port == rule.local_port
                }) {
                    Err("该本地端口已存在映射，请使用其他端口".into())
                } else {
                    Ok(rule)
                }
            });
            match result {
                Ok(rule) => {
                    let active = self
                        .ui_state
                        .ports
                        .status
                        .get(&rule.id)
                        .is_some_and(|s| s["enabled"] == true);
                    if let Some(old) = self
                        .ui_state
                        .prefs
                        .port_rules
                        .iter_mut()
                        .find(|r| r.id == rule.id)
                    {
                        *old = rule.clone();
                    } else {
                        self.ui_state.prefs.port_rules.push(rule.clone());
                    }
                    self.ui_state.save();
                    if active {
                        self.change_port(&rule, true);
                    }
                    return;
                }
                Err(message) => draft.error = Some(message),
            }
        }
        if !cancel && !response.should_close() {
            self.ui_state.ports.draft = Some(draft);
        }
    }
}

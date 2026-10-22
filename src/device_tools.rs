use super::*;

pub(super) enum Dialog {
    Rename {
        device: Device,
        name: String,
        focus: bool,
    },
    Confirm {
        device: Device,
        action: &'static str,
        title: &'static str,
    },
}
pub(super) struct Pending {
    pub request: String,
    pub device: Device,
    pub action: String,
    pub name: String,
    pub temporary_session: bool,
    pub sent: bool,
}
#[derive(Default)]
pub(super) struct DeviceTools {
    pub dialog: Option<Dialog>,
    pub pending: Option<Pending>,
    pub error: Option<String>,
}

impl Desktop {
    pub(in crate::app) fn prepare_tool_preview(&mut self) {
        let Some(device) = self.selected.clone() else {
            return;
        };
        match self.options.view.as_deref() {
            Some("rename") => {
                self.ui_state.page = Page::Device;
                self.ui_state.device_tools.dialog = Some(Dialog::Rename {
                    name: device.name.clone(),
                    device,
                    focus: false,
                });
            }
            Some("ports") => {
                self.active_device = Some(device);
                self.ui_state.windows.ports = true;
            }
            _ => {}
        }
    }
    pub(super) fn device_menu(&mut self, ui: &mut egui::Ui, device: &Device) {
        menu_layout(ui);
        let available = self.ui_state.device_tools.pending.is_none();
        let idle = !self.connected && !self.connecting && !self.disconnecting;
        let same = self
            .active_device
            .as_ref()
            .is_some_and(|d| d.id == device.id);
        ui.add_enabled_ui(available, |ui| {
            if ui
                .add_enabled_ui(device.online, |ui| {
                    menu_item(ui, Icon::Transfer, "端口映射")
                })
                .inner
                .clicked()
            {
                self.open_ports_for(device.clone());
                ui.close();
            }
            if ui
                .add_enabled_ui(device.online, |ui| menu_item(ui, Icon::Terminal, "终端"))
                .inner
                .clicked()
            {
                self.open_terminal_for(device.clone());
                ui.close();
            }
            for (action, title, icon) in [
                ("wake", "远程开机", Icon::Power),
                ("shutdown", "关机", Icon::Power),
                ("reboot", "重启", Icon::Refresh),
            ] {
                if ui
                    .add_enabled_ui(
                        if action == "wake" {
                            !device.online
                        } else {
                            device.online && (idle || same)
                        },
                        |ui| menu_item(ui, icon, title),
                    )
                    .inner
                    .clicked()
                {
                    self.ui_state.device_tools.dialog = Some(Dialog::Confirm {
                        device: device.clone(),
                        action,
                        title,
                    });
                    self.ui_state.device_tools.error = None;
                    ui.close();
                }
            }
            if menu_item(ui, Icon::Edit, "重命名").clicked() {
                self.ui_state.device_tools.dialog = Some(Dialog::Rename {
                    device: device.clone(),
                    name: device.name.clone(),
                    focus: false,
                });
                self.ui_state.device_tools.error = None;
                ui.close();
            }
            if ui
                .add_enabled_ui(!device.online && !same, |ui| {
                    menu_item(ui, Icon::Trash, "删除设备")
                })
                .inner
                .clicked()
            {
                self.ui_state.device_tools.dialog = Some(Dialog::Confirm {
                    device: device.clone(),
                    action: "remove",
                    title: "删除设备",
                });
                self.ui_state.device_tools.error = None;
                ui.close();
            }
        });
    }

    pub(super) fn device_tools_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut dialog) = self.ui_state.device_tools.dialog.take() else {
            return;
        };
        let busy = self.ui_state.device_tools.pending.is_some();
        let mut submit = false;
        let mut cancel = false;
        let response = modal(ctx, "device-action-dialog", |ui| {
            ui.set_width(446.0);
            ui.spacing_mut().item_spacing = Vec2::ZERO;
            egui::Frame::new().inner_margin(24).show(ui, |ui| {
                ui.set_width(398.0);
                ui.spacing_mut().item_spacing.y = 12.0;
                match &mut dialog {
                    Dialog::Rename { name, focus, .. } => {
                        ui.label(RichText::new("修改设备名称").size(16.0));
                        let text = ui.add_sized(
                            [398.0, 32.0],
                            singleline(name).char_limit(40).margin(egui::Margin {
                                left: 8,
                                right: 32,
                                top: 5,
                                bottom: 5,
                            }),
                        );
                        if !*focus {
                            text.request_focus();
                            if let Some(mut state) = egui::TextEdit::load_state(ctx, text.id) {
                                state
                                    .cursor
                                    .set_char_range(Some(egui::text::CCursorRange::two(
                                        egui::text::CCursor::new(0),
                                        egui::text::CCursor::new(name.chars().count()),
                                    )));
                                state.store(ctx, text.id);
                            }
                            *focus = true;
                        }
                        let clear = egui::Rect::from_center_size(
                            text.rect.right_center() - Vec2::new(14.0, 0.0),
                            Vec2::splat(22.0),
                        );
                        // The field retains normal text selection; clearing is a separate control.
                        text.on_hover_text("最多可输入 40 个字符");
                        if ui
                            .place(clear, egui::Button::new("×").frame(false))
                            .clicked()
                        {
                            name.clear();
                        }
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("最多可输入40个字符").size(12.0).color(MUTED));
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    ui.label(
                                        RichText::new(format!("{}/40", name.chars().count()))
                                            .size(12.0)
                                            .color(MUTED),
                                    );
                                },
                            );
                        });
                    }
                    Dialog::Confirm {
                        device,
                        title,
                        action,
                        ..
                    } => {
                        ui.label(RichText::new(*title).size(16.0));
                        ui.label(if *action == "remove" {
                            format!("确定要从设备列表中删除 {} 吗？", device.name)
                        } else if *action == "wake" {
                            format!(
                                "通过官方远程开机配置唤醒 {}。请求提交后，请等待设备上线。",
                                device.name
                            )
                        } else {
                            format!(
                                "确定要{} {} 吗？当前远控连接将中断，请先保存远端工作。",
                                title, device.name
                            )
                        });
                    }
                }
                if let Some(message) = &self.ui_state.device_tools.error {
                    ui.colored_label(Color32::from_rgb(200, 60, 60), message);
                }
                if busy {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("正在处理…");
                    });
                }
            });
            egui::Frame::new()
                .fill(BG)
                .corner_radius(egui::CornerRadius {
                    nw: 0,
                    ne: 0,
                    sw: 8,
                    se: 8,
                })
                .inner_margin(24)
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    let valid = match &dialog {
                        Dialog::Rename { name, .. } => !name.trim().is_empty(),
                        _ => true,
                    };
                    ui.horizontal(|ui| {
                        submit = ui
                            .add_enabled_ui(!busy && valid, |ui| {
                                art::primary(
                                    ui,
                                    if matches!(dialog, Dialog::Rename { .. }) {
                                        "保存更改"
                                    } else {
                                        "确定"
                                    },
                                    Vec2::new(195.0, 32.0),
                                )
                            })
                            .inner
                            .clicked();
                        cancel = ui
                            .add_enabled(
                                !busy,
                                egui::Button::new("取消").min_size(Vec2::new(195.0, 32.0)),
                            )
                            .clicked();
                    });
                });
        });
        if submit {
            let (device, action, name) = match &dialog {
                Dialog::Rename { device, name, .. } => {
                    (device.clone(), "rename", name.trim().to_owned())
                }
                Dialog::Confirm { device, action, .. } => (device.clone(), *action, String::new()),
            };
            let request = uuid::Uuid::new_v4().to_string();
            let same = self.connected
                && self
                    .active_device
                    .as_ref()
                    .is_some_and(|d| d.id == device.id);
            let power = matches!(action, "shutdown" | "reboot");
            self.ui_state.device_tools.error = None;
            self.ui_state.device_tools.pending = Some(Pending {
                request: request.clone(),
                device: device.clone(),
                action: action.into(),
                name: name.clone(),
                temporary_session: power && !same,
                sent: false,
            });
            if power {
                if same {self.send_pending_power();} else {
                    self.connect_for_tools(device);
                }
            } else if !self.send(json!({"op":"device_action","device_id":device.id,"action":action,"name":name,"request_id":request})) {
                self.ui_state.device_tools.pending=None;
            }
        }
        if !cancel && (busy || !response.should_close()) {
            self.ui_state.device_tools.dialog = Some(dialog);
        }
    }

    pub(in crate::app) fn send_pending_power(&mut self) {
        if let Some(pending) = &mut self.ui_state.device_tools.pending
            && matches!(pending.action.as_str(), "shutdown" | "reboot")
            && !pending.sent
        {
            pending.sent = true;
            let command = json!({"op":"action","action":if pending.action=="shutdown"{12}else{9},"request_id":pending.request});
            self.control(command);
        }
    }
    pub(super) fn device_action_result(&mut self, data: &Value) {
        if !self
            .ui_state
            .device_tools
            .pending
            .as_ref()
            .is_some_and(|p| data["request_id"] == p.request)
        {
            return;
        }
        let pending = self.ui_state.device_tools.pending.take().unwrap();
        if data["ok"] == false {
            self.ui_state.device_tools.error = Some(
                data["message"]
                    .as_str()
                    .unwrap_or("操作失败，请重试")
                    .into(),
            );
            if pending.temporary_session {
                self.disconnect();
            }
            return;
        }
        if pending.action == "rename" {
            for device in self
                .devices
                .iter_mut()
                .chain(self.selected.iter_mut())
                .chain(self.active_device.iter_mut())
            {
                if device.id == pending.device.id {
                    device.name = pending.name.clone();
                }
            }
        }
        if pending.action == "remove" {
            self.devices.retain(|d| d.id != pending.device.id);
            if self
                .selected
                .as_ref()
                .is_some_and(|d| d.id == pending.device.id)
            {
                self.selected = None;
                self.ui_state.page = Page::All;
            }
        }
        self.ui_state.device_tools.dialog = None;
        self.status = "操作已提交".into();
        if pending.temporary_session {
            self.disconnect();
        } else {
            self.refresh();
        }
    }
}

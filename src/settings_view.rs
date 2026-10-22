use super::*;

impl Desktop {
    pub(super) fn settings_page(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("设置").font(fonts::bold(24.0)));
        ui.add_space(16.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 32.0;
            for (i, name) in ["常规", "安全", "键盘", "网络", "画面", "MCP"]
                .iter()
                .enumerate()
            {
                let (r, response) =
                    ui.allocate_exact_size(Vec2::new(28.0, 36.0), egui::Sense::click());
                art::hover_background(ui, &response, r.shrink2(Vec2::new(0.0, 4.0)));
                ui.painter().text(
                    r.center(),
                    egui::Align2::CENTER_CENTER,
                    *name,
                    egui::FontId::proportional(13.0),
                    if self.ui_state.setting_tab == i {
                        INK
                    } else {
                        MUTED
                    },
                );
                if self.ui_state.setting_tab == i {
                    ui.painter().rect_filled(
                        egui::Rect::from_center_size(
                            egui::pos2(r.center().x, r.bottom()),
                            Vec2::new(16.0, 3.0),
                        ),
                        2,
                        BLUE,
                    );
                }
                if response.clicked() {
                    self.ui_state.setting_tab = i;
                }
            }
        });
        ui.add_space(14.0);
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| match self.ui_state.setting_tab {
                0 => self.general_settings(ui),
                1 => self.security_settings(ui),
                2 => self.keyboard_settings(ui),
                3 => self.network_settings(ui),
                4 => self.picture_settings(ui),
                _ => self.mcp_settings(ui),
            });
        self.shortcut_dialog(ui.ctx());
        self.download_dialog(ui.ctx());
    }

    fn mcp_settings(&mut self, ui: &mut egui::Ui) {
        if self.ui_state.mcp_poll.elapsed() > Duration::from_secs(2) {
            self.ui_state.mcp_poll = Instant::now();
            self.send(json!({"op":"service_call","name":"uu_mcp_status","arguments":{}}));
        }
        let status = self.ui_state.mcp_status.clone();
        let mut enabled = status["enabled"].as_bool().unwrap_or(true);
        if setting_toggle(
            ui,
            Icon::Settings,
            "MCP 服务",
            "允许本机 MCP 客户端使用远控、终端、文件和端口映射",
            &mut enabled,
        ) {
            self.send(json!({"op":"service_call","name":"uu_mcp_configure","arguments":{"enabled":enabled}}));
        }
        card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label("接入配置");
            ui.label(
                RichText::new("与当前窗口共用账号和连接；MCP 可在主窗口关闭后继续运行。")
                    .color(MUTED),
            );
            let configuration = mcp_configuration_text(&status);
            ui.horizontal(|ui| {
                let copied = self
                    .ui_state
                    .mcp_copied_at
                    .is_some_and(|time| time.elapsed() < Duration::from_secs(3));
                if ui
                    .add_enabled(
                        configuration.is_some(),
                        egui::Button::new(if copied {
                            "已复制"
                        } else {
                            "复制 MCP 配置"
                        }),
                    )
                    .clicked()
                    && let Some(configuration) = configuration
                {
                    ui.ctx().copy_text(configuration);
                    self.ui_state.mcp_copied_at = Some(Instant::now());
                    ui.ctx().request_repaint_after(Duration::from_secs(3));
                }
                if status.is_null() {
                    ui.label(RichText::new("正在加载配置…").color(MUTED));
                }
            });
            let clients = status["clients"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|v| v["kind"] == "mcp")
                .count();
            ui.label(format!("已连接 MCP 客户端：{clients}"));
        });
        card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label("共享连接");
            for session in status["sessions"].as_array().into_iter().flatten() {
                let id = session["device_id"].as_str().unwrap_or("");
                let name = self
                    .devices
                    .iter()
                    .find(|d| d.id == id)
                    .map(|d| d.name.as_str())
                    .unwrap_or("远程协助设备");
                ui.horizontal(|ui| {
                    ui.label(name);
                    ui.label(
                        RichText::new(if session["tools_ready"] == true {
                            "已连接"
                        } else {
                            "连接中"
                        })
                        .color(MUTED),
                    );
                });
            }
            if status["sessions"].as_array().is_none_or(|v| v.is_empty()) {
                ui.label(RichText::new("暂无连接").color(MUTED));
            }
        });
        card().show(ui,|ui| {
            ui.set_width(ui.available_width());
            ui.label("任务");
            for task in status["tasks"].as_array().into_iter().flatten() {
                ui.horizontal(|ui| {
                    let label=match task["kind"].as_str().unwrap_or(""){"command"=>"命令执行","transfer"=>"文件传输","display"=>"屏幕设置","search"=>"文件搜索","read_text"=>"读取文本","write_text"=>"写入文本",_=>"任务"};
                    let state=match task["state"].as_str().unwrap_or(""){"running"=>"进行中","completed"=>"已完成","failed"=>"失败","cancelled"=>"已取消","cancelling"=>"正在取消",_=>"等待中"};
                    ui.label(label);ui.label(state);
                    if task["state"]=="running"&&ui.button("取消").clicked(){self.send(json!({"op":"service_call","name":"uu_task_control","arguments":{"task_id":task["task_id"],"action":"cancel"}}));}
                });
                if let Some(error)=task["error"].as_str(){ui.label(RichText::new(error).color(MUTED));}
            }
            if status["tasks"].as_array().is_none_or(|v|v.is_empty()){ui.label(RichText::new("暂无任务").color(MUTED));}
        });
        if !self.ui_state.mcp_notice.is_empty() {
            ui.label(&self.ui_state.mcp_notice);
        }
    }

    fn general_settings(&mut self, ui: &mut egui::Ui) {
        let old = self.ui_state.autostart;
        if setting_toggle(
            ui,
            Icon::Devices,
            "开机自动启动",
            "登录桌面后自动打开 UUX",
            &mut self.ui_state.autostart,
        ) && let Err(error) = crate::platform::set_autostart(self.ui_state.autostart)
        {
            self.ui_state.autostart = old;
            self.error = Some(error.to_string());
        }
        if setting_toggle(
            ui,
            Icon::Devices,
            "防止电脑休眠",
            "远控或传输期间保持本机唤醒，断开后恢复",
            &mut self.ui_state.prefs.keep_awake,
        ) {
            self.control(json!({"op":"keep_awake","enabled":self.ui_state.prefs.keep_awake}));
            self.ui_state.save();
        }
        if setting_toggle(
            ui,
            Icon::Transfer,
            "拖拽文件发送",
            "将文件拖入文件传输窗口时发送至当前远端目录",
            &mut self.ui_state.prefs.drag_send,
        ) {
            self.ui_state.save();
        }
        self.download_settings(ui);
        if setting_toggle(
            ui,
            Icon::Transfer,
            "接收被控端拖拽文件",
            "在被控电脑将文件拖入 UU 发送浮窗，保存到上方目录",
            &mut self.ui_state.prefs.drag_receive,
        ) {
            self.control(json!({"op":"drag_receive","enabled":self.ui_state.prefs.drag_receive,"directory":self.ui_state.prefs.download}));
            self.ui_state.save();
        }
        ui.add_space(2.0);
        card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                icon(ui, Icon::Devices);
                ui.label("关闭主窗口时");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    combo("close-mode")
                        .selected_text(if self.ui_state.prefs.close_minimizes {
                            "最小化到任务栏"
                        } else {
                            "退出应用"
                        })
                        .show_ui(ui, |ui| {
                            let changed = ui
                                .selectable_value(
                                    &mut self.ui_state.prefs.close_minimizes,
                                    true,
                                    "最小化到任务栏",
                                )
                                .changed()
                                | ui.selectable_value(
                                    &mut self.ui_state.prefs.close_minimizes,
                                    false,
                                    "退出应用",
                                )
                                .changed();
                            if changed {
                                self.ui_state.save();
                            }
                        });
                });
            });
        });
    }

    fn download_settings(&mut self, ui: &mut egui::Ui) {
        card().show(ui, |ui| {
            let (row, _) =
                ui.allocate_exact_size(Vec2::new(ui.available_width(), 38.0), egui::Sense::hover());
            art::draw(
                ui.painter(),
                egui::Rect::from_center_size(
                    row.left_center() + Vec2::new(12.0, 0.0),
                    Vec2::splat(24.0),
                ),
                Icon::Folder,
                INK,
            );
            ui.painter().text(
                row.left_top() + Vec2::new(44.0, 2.0),
                egui::Align2::LEFT_TOP,
                "接收文件的存储路径",
                egui::FontId::proportional(13.0),
                INK,
            );
            let mut job = egui::text::LayoutJob::simple_singleline(
                self.ui_state.prefs.download.clone(),
                egui::FontId::proportional(12.0),
                MUTED,
            );
            job.wrap.max_width = (row.width() - 128.0).max(40.0);
            job.wrap.max_rows = 1;
            let text = ui.painter().layout_job(job);
            ui.painter().galley(
                row.left_bottom() + Vec2::new(44.0, -text.size().y),
                text,
                MUTED,
            );
            let controls = egui::Rect::from_min_max(
                egui::pos2(row.right() - 76.0, row.center().y - 16.0),
                row.right_bottom(),
            );
            ui.scope_builder(egui::UiBuilder::new().max_rect(controls), |ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                ui.horizontal(|ui| {
                    if art::button(ui, Icon::Edit, "修改存储路径").clicked() {
                        self.ui_state.download_draft = Some(self.ui_state.prefs.download.clone());
                    }
                    if art::button(ui, Icon::Folder, "打开文件夹").clicked() {
                        let path = std::path::Path::new(&self.ui_state.prefs.download);
                        if !path.is_dir() {
                            self.error =
                                Some("存储目录尚不存在，请修改路径或先在文件管理器中创建。".into());
                        } else if let Ok(url) = url::Url::from_directory_path(path) {
                            ui.ctx().open_url(egui::OpenUrl::new_tab(url.to_string()));
                        }
                    }
                });
            });
        });
        ui.add_space(2.0);
    }

    fn download_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut draft) = self.ui_state.download_draft.take() else {
            return;
        };
        let mut save = false;
        let mut cancel = false;
        let response = modal(ctx, "download-directory", |ui| {
            modal_body(ui, "修改文件存储路径", |ui| {
                ui.add_sized(
                    [398.0, 32.0],
                    singleline(&mut draft).hint_text("请输入本机文件夹的完整路径"),
                );
                note(ui, "后续接收的文件将保存至此目录");
            });
            (save, cancel) = modal_actions(
                ui,
                "保存更改",
                std::path::Path::new(draft.trim()).is_absolute(),
                "取消",
            );
        });
        if save {
            self.ui_state.prefs.download = draft.trim().to_owned();
            self.ui_state.local_path = self.ui_state.prefs.download.clone();
            self.ui_state.local_address = self.ui_state.local_path.clone();
            self.ui_state.save();
            self.control(json!({"op":"drag_receive","enabled":self.ui_state.prefs.drag_receive,"directory":self.ui_state.prefs.download}));
        } else if !cancel && !response.should_close() {
            self.ui_state.download_draft = Some(draft);
        }
    }

    fn security_settings(&mut self, ui: &mut egui::Ui) {
        if setting_toggle(
            ui,
            Icon::Transfer,
            "允许文件传输",
            "关闭后停止本会话中的文件任务",
            &mut self.ui_state.prefs.allow_files,
        ) {
            if !self.ui_state.prefs.allow_files {
                self.ui_state.windows.files = false;
            }
            self.apply_security();
        }
        if setting_toggle(
            ui,
            Icon::Keyboard,
            "仅观看，禁用键鼠",
            "禁止本主控端向远端提交键鼠和文字",
            &mut self.ui_state.prefs.read_only,
        ) {
            self.release();
            self.apply_security();
        }
        if setting_toggle(
            ui,
            Icon::Transfer,
            "同步剪贴板",
            "同步文字和图片；文件粘贴共用文件传输任务",
            &mut self.ui_state.prefs.clipboard,
        ) {
            if !self.ui_state.prefs.clipboard {
                self.control(json!({"op":"clipboard_focus","enabled":false}));
            }
            self.ui_state.save();
        }
        if setting_toggle(
            ui,
            Icon::Devices,
            "断开后锁定远端电脑",
            "关闭最后一个会话窗口时请求锁定 Windows",
            &mut self.ui_state.prefs.lock_on_disconnect,
        ) {
            self.apply_security();
        }
        ui.add_space(2.0);
        card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label("当前账号");
                ui.label(RichText::new(&self.nickname).color(MUTED));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("退出账号").clicked() {
                        self.disconnect();
                        self.send(json!({"op":"logout"}));
                    }
                });
            });
        });
    }

    pub(super) fn apply_security(&mut self) {
        self.control(json!({"op":"local_security","allow_files":self.ui_state.prefs.allow_files,
            "read_only":self.ui_state.prefs.read_only,"lock_on_disconnect":self.ui_state.prefs.lock_on_disconnect}));
        self.ui_state.save();
    }

    fn keyboard_settings(&mut self, ui: &mut egui::Ui) {
        card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                icon(ui, Icon::Keyboard);
                ui.add_space(12.0);
                ui.vertical(|ui| {
                    ui.label("仅控制端响应的快捷键");
                    note(ui, "远控时以下按键保留在本机，不会传到被控端");
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add_sized([80.0, 24.0], egui::Button::new("添加"))
                        .clicked()
                    {
                        self.ui_state.shortcut_capture = Some(usize::MAX);
                    }
                });
            });
            ui.add_space(14.0);
            ui.separator();
            ui.add_space(6.0);
            if self.ui_state.prefs.local_shortcuts.is_empty() {
                ui.horizontal(|ui| {
                    ui.add_space(44.0);
                    note(ui, "当前暂无配置");
                });
            }
            let mut remove = None;
            for (i, shortcut) in self
                .ui_state
                .prefs
                .local_shortcuts
                .clone()
                .iter()
                .enumerate()
            {
                ui.horizontal(|ui| {
                    ui.add_space(44.0);
                    ui.label(shortcut);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if art::sized_icon_button(ui, Icon::Trash, "删除", Vec2::splat(24.0))
                            .clicked()
                        {
                            remove = Some(i);
                        }
                        if art::sized_icon_button(ui, Icon::Edit, "编辑", Vec2::splat(24.0))
                            .clicked()
                        {
                            self.ui_state.shortcut_capture = Some(i + 2);
                        }
                    });
                });
                ui.add_space(8.0);
            }
            if let Some(i) = remove {
                self.ui_state.prefs.local_shortcuts.remove(i);
                self.ui_state.save();
            }
        });
        ui.add_space(4.0);
        card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                icon(ui, Icon::Keyboard);
                ui.add_space(12.0);
                ui.label("功能快捷键");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add_sized([120.0, 24.0], egui::Button::new("还原默认按键"))
                        .clicked()
                    {
                        self.ui_state.prefs.release_shortcut = "Ctrl+Alt+G".into();
                        self.ui_state.prefs.fullscreen_shortcut = "Ctrl+Alt+F".into();
                        self.ui_state.save();
                    }
                });
            });
            for (index, name, shortcut) in [
                (
                    0,
                    "释放被控端键鼠",
                    self.ui_state.prefs.release_shortcut.clone(),
                ),
                (
                    1,
                    "全屏 / 退出全屏",
                    self.ui_state.prefs.fullscreen_shortcut.clone(),
                ),
            ] {
                ui.add_space(12.0);
                ui.separator();
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    ui.add_space(44.0);
                    ui.label(name);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if art::sized_icon_button(ui, Icon::Edit, "编辑", Vec2::splat(24.0))
                            .clicked()
                        {
                            self.ui_state.shortcut_capture = Some(index);
                        }
                        ui.spacing_mut().item_spacing.x = 4.0;
                        for key in shortcut.split('+').rev() {
                            egui::Frame::new()
                                .fill(Color32::from_rgb(233, 244, 255))
                                .corner_radius(3)
                                .inner_margin(egui::Margin::symmetric(6, 3))
                                .show(ui, |ui| {
                                    ui.label(RichText::new(key).size(12.0).color(BLUE));
                                });
                        }
                    });
                });
            }
        });
        ui.add_space(12.0);
        if setting_toggle(
            ui,
            Icon::Keyboard,
            "使用本机输入法",
            "将输入法确认后的文字提交到被控设备",
            &mut self.ui_state.prefs.ime,
        ) {
            self.ui_state.save();
        }
    }

    fn shortcut_dialog(&mut self, ctx: &egui::Context) {
        let Some(index) = self.ui_state.shortcut_capture else {
            return;
        };
        let mut candidate = None;
        let response = modal(ctx, "shortcut-capture", |ui| {
            modal_body(ui, "设置快捷键", |ui| {
                ui.label("请按下包含 Ctrl、Alt 或 Shift 的组合键");
                ui.label(RichText::new("按 Esc 取消").size(12.0).color(MUTED));
                for event in ui.input(|i| i.events.clone()) {
                    if let Some(shortcut) = crate::input::shortcut_from_event(&event) {
                        candidate = Some(shortcut);
                    }
                }
            });
            modal_cancel(ui, "取消")
        });
        if response.inner || response.should_close() {
            self.ui_state.shortcut_capture = None;
            return;
        }
        if let Some(shortcut) = candidate {
            let other = if index == 0 {
                &self.ui_state.prefs.fullscreen_shortcut
            } else {
                &self.ui_state.prefs.release_shortcut
            };
            if (index < 2 && &shortcut == other)
                || (index < 2 && self.ui_state.prefs.local_shortcuts.contains(&shortcut))
                || (index >= 2
                    && (shortcut == self.ui_state.prefs.fullscreen_shortcut
                        || shortcut == self.ui_state.prefs.release_shortcut))
            {
                self.error = Some("这个组合键已被其他快捷键设置使用".into());
            } else {
                match index {
                    0 => self.ui_state.prefs.release_shortcut = shortcut,
                    1 => self.ui_state.prefs.fullscreen_shortcut = shortcut,
                    usize::MAX => {
                        if !self.ui_state.prefs.local_shortcuts.contains(&shortcut) {
                            self.ui_state.prefs.local_shortcuts.push(shortcut);
                        }
                    }
                    i => {
                        if let Some(old) = self.ui_state.prefs.local_shortcuts.get_mut(i - 2) {
                            *old = shortcut;
                        }
                    }
                }
                self.ui_state.save();
            }
            self.ui_state.shortcut_capture = None;
        }
    }

    fn network_settings(&mut self, ui: &mut egui::Ui) {
        card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                icon(ui, Icon::Devices);
                ui.add_space(12.0);
                ui.label("代理设置");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let names = ["系统代理", "不使用代理", "手动设置"];
                    combo("proxy-mode")
                        .width(120.0)
                        .selected_text(
                            names
                                .get(self.ui_state.proxy_draft.mode as usize)
                                .copied()
                                .unwrap_or("系统代理"),
                        )
                        .show_ui(ui, |ui| {
                            for (i, name) in names.into_iter().enumerate() {
                                ui.selectable_value(
                                    &mut self.ui_state.proxy_draft.mode,
                                    i as u8,
                                    name,
                                );
                            }
                        });
                });
            });
            ui.add_space(14.0);
            ui.separator();
            ui.add_space(18.0);
            if self.ui_state.proxy_draft.mode == 2 {
                let proxy = &mut self.ui_state.proxy_draft;
                let port = &mut self.ui_state.proxy_port;
                ui.scope(|ui| {
                    ui.spacing_mut().interact_size.y = 32.0;
                    proxy_field(ui, "协议类型", |ui| {
                        combo("proxy-protocol")
                            .width(180.0)
                            .selected_text(if proxy.scheme == "http" {
                                "HTTP"
                            } else {
                                "SOCKS5"
                            })
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut proxy.scheme, "http".into(), "HTTP");
                                ui.selectable_value(&mut proxy.scheme, "socks5h".into(), "SOCKS5");
                            });
                    });
                    ui.add_space(16.0);
                    proxy_field(ui, "代理地址", |ui| {
                        ui.add_sized(
                            [180.0, 32.0],
                            singleline(&mut proxy.host).hint_text("请输入 IP 或域名"),
                        );
                    });
                    ui.add_space(16.0);
                    proxy_field(ui, "端口", |ui| {
                        if ui
                            .add_sized(
                                [180.0, 32.0],
                                singleline(port).hint_text("请输入端口").char_limit(5),
                            )
                            .changed()
                        {
                            port.retain(|c| c.is_ascii_digit());
                            proxy.port = port.parse().unwrap_or(0);
                        }
                    });
                    ui.add_space(16.0);
                    proxy_field(ui, "用户名", |ui| {
                        ui.add_sized(
                            [180.0, 32.0],
                            singleline(&mut proxy.username).hint_text("选填"),
                        );
                    });
                    ui.add_space(16.0);
                    proxy_field(ui, "密码", |ui| {
                        ui.add_sized(
                            [180.0, 32.0],
                            singleline(&mut proxy.password)
                                .hint_text("选填")
                                .password(true),
                        );
                    });
                });
            } else {
                note(
                    ui,
                    if self.ui_state.proxy_draft.mode == 0 {
                        "使用系统环境变量中的 HTTP / HTTPS 代理设置"
                    } else {
                        "直接连接账号及设备服务"
                    },
                );
            }
            ui.add_space(12.0);
            let valid = self.ui_state.proxy_draft.validate().is_ok();
            ui.horizontal(|ui| {
                ui.add_space(120.0);
                if ui
                    .add_enabled_ui(valid, |ui| {
                        ui.add_sized([88.0, 30.0], egui::Button::new("检测"))
                    })
                    .inner
                    .clicked()
                {
                    if let Err(e) = self.ui_state.proxy_draft.validate() {
                        self.ui_state.network_notice = e.to_string();
                    } else {
                        self.ui_state.network_notice = "正在检测连接…".into();
                        self.send(json!({"op":"proxy_test","settings":self.ui_state.proxy_draft}));
                    }
                }
                if ui
                    .add_enabled_ui(valid, |ui| art::primary(ui, "应用", Vec2::new(88.0, 30.0)))
                    .inner
                    .clicked()
                {
                    if let Err(e) = self.ui_state.proxy_draft.validate() {
                        self.ui_state.network_notice = e.to_string();
                    } else {
                        self.ui_state.prefs.proxy = self.ui_state.proxy_draft.clone();
                        self.ui_state.save();
                        self.send(json!({"op":"proxy_apply","settings":self.ui_state.prefs.proxy}));
                    }
                }
            });
            if !self.ui_state.network_notice.is_empty() {
                ui.add_space(12.0);
                note(ui, &self.ui_state.network_notice);
            }
        });
        ui.add_space(12.0);
        if setting_toggle(
            ui,
            Icon::Devices,
            "使用中继连接",
            "直连不稳定时可尝试，重新连接后生效",
            &mut self.ui_state.prefs.relay_only,
        ) {
            self.ui_state.save();
        }
    }

    fn picture_settings(&mut self, ui: &mut egui::Ui) {
        card().show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label("本机媒体能力");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("检测设备").clicked() {
                        self.send(json!({"op":"service_call","name":"uu_capabilities_get","arguments":{}}));
                    }
                });
            });
            for decoder in self.ui_state.capabilities["local"]["decoders"].as_array().into_iter().flatten() {
                ui.label(format!("{}  软件解码：{}",decoder["codec"].as_str().unwrap_or(""),if decoder["software_available"]==true {"可用"} else {"不可用"}));
            }
            for (kind,label) in [("output","声音输出"),("input","麦克风")] {
                let devices=&self.ui_state.capabilities["local"]["audio_devices"][kind];
                for device in devices["devices"].as_array().into_iter().flatten() {
                    ui.label(format!("{label}：{}{}",device["name"].as_str().unwrap_or("未命名设备"),if device["default"]==true {"（默认）"} else {""}));
                }
                if let Some(error)=devices["error"].as_str() {ui.label(error);}
            }
        });
        ui.add_space(12.0);

        let mut changed = false;
        card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            for (fps, row) in [(true, "默认帧率"), (false, "默认画质")] {
                ui.horizontal(|ui| {
                    ui.label(row);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if fps {
                            combo("default-fps")
                                .selected_text(format!("{} 帧", self.ui_state.prefs.fps))
                                .show_ui(ui, |ui| {
                                    for n in [30, 60, 90, 144] {
                                        changed |= ui
                                            .selectable_value(
                                                &mut self.ui_state.prefs.fps,
                                                n,
                                                format!("{n} 帧"),
                                            )
                                            .changed();
                                    }
                                });
                        } else {
                            let modes = [
                                (5, "自动"),
                                (4, "原画 20M"),
                                (3, "高清 8M"),
                                (2, "清晰 2M"),
                                (1, "流畅"),
                                (6, "自定义"),
                            ];
                            combo("default-quality")
                                .selected_text(
                                    modes
                                        .iter()
                                        .find(|m| m.0 == self.ui_state.prefs.quality)
                                        .map_or("自定义", |m| m.1),
                                )
                                .show_ui(ui, |ui| {
                                    for (q, name) in modes {
                                        changed |= ui
                                            .selectable_value(
                                                &mut self.ui_state.prefs.quality,
                                                q,
                                                name,
                                            )
                                            .changed();
                                    }
                                });
                        }
                    });
                });
                ui.add_space(12.0);
                ui.separator();
                ui.add_space(12.0);
            }
            if self.ui_state.prefs.quality == 6 {
                changed |= ui
                    .add(
                        egui::Slider::new(&mut self.ui_state.prefs.bitrate, 500_000..=40_000_000)
                            .text("bit/s"),
                    )
                    .changed();
            }
        });
        ui.add_space(4.0);
        changed |= setting_toggle(
            ui,
            Icon::Fullscreen,
            "允许画面放大",
            "按窗口比例缩放，关闭后不超过远端原始尺寸",
            &mut self.ui_state.prefs.scale_up,
        );
        if setting_toggle(
            ui,
            Icon::Sound,
            "播放远端声音",
            "使用本机系统默认音频输出",
            &mut self.ui_state.prefs.audio,
        ) {
            self.control(json!({"op":"audio","enabled":self.ui_state.prefs.audio}));
            changed = true;
        }
        if changed {
            self.ui_state.save();
            if self.connected {
                self.apply_capture();
            }
        }
    }
}
fn mcp_configuration_text(status: &Value) -> Option<String> {
    let configuration = &status["configuration"];
    let server = &configuration["mcpServers"]["uux"];
    if server["command"]
        .as_str()
        .is_none_or(|command| command.trim().is_empty())
        || server["args"]
            .as_array()
            .is_none_or(|args| args.iter().any(|arg| !arg.is_string()))
    {
        return None;
    }
    serde_json::to_string_pretty(configuration).ok()
}

#[cfg(test)]
mod mcp_configuration_tests {
    use super::*;

    #[test]
    fn only_complete_configuration_can_be_copied() {
        for status in [
            Value::Null,
            json!({}),
            json!({"configuration":null}),
            json!({"configuration":{"mcpServers":{"uux":{"command":null,"args":[]}}}}),
        ] {
            assert!(mcp_configuration_text(&status).is_none());
        }
        let configuration = json!({"mcpServers":{"uux":{
            "command":"/home/test/UUX app/uux","args":["--mcp","--state-dir","/home/test/配置"]
        }}});
        let copied = mcp_configuration_text(&json!({"configuration":configuration})).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&copied).unwrap(),
            configuration
        );
    }
}
fn icon(ui: &mut egui::Ui, icon: Icon) {
    let (r, _) = ui.allocate_exact_size(Vec2::splat(24.0), egui::Sense::hover());
    art::draw(ui.painter(), r, icon, INK);
}
fn note(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).size(12.0).color(MUTED));
}

// Keep every form label on the same baseline and every field at the same x,
// independently of the width of the text or a combo-box's current selection.
fn proxy_field(ui: &mut egui::Ui, label: &str, field: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        let (r, _) = ui.allocate_exact_size(Vec2::new(120.0, 32.0), egui::Sense::hover());
        ui.painter().text(
            egui::pos2(r.left() + 44.0, r.center().y),
            egui::Align2::LEFT_CENTER,
            label,
            egui::FontId::proportional(13.0),
            INK,
        );
        field(ui);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxy_fields_keep_equal_width_height_and_left_edge() {
        let ctx = egui::Context::default();
        super::super::super::setup(&ctx);
        for _ in 0..2 {
            let mut rects = vec![];
            let output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        Vec2::new(600.0, 400.0),
                    )),
                    ..Default::default()
                },
                |ui| {
                    ui.spacing_mut().interact_size.y = 32.0;
                    proxy_field(ui, "Protocol", |ui| {
                        rects.push(
                            combo("protocol")
                                .width(180.0)
                                .selected_text("HTTP")
                                .show_ui(ui, |_| {})
                                .response
                                .rect,
                        );
                    });
                    proxy_field(ui, "Address", |ui| {
                        rects.push(
                            ui.add_sized([180.0, 32.0], singleline(&mut String::new()))
                                .rect,
                        );
                    });
                    proxy_field(ui, "Port", |ui| {
                        rects.push(
                            ui.add_sized([180.0, 32.0], singleline(&mut "8080".into()))
                                .rect,
                        );
                    });
                },
            );
            output.drop_without_applying_deltas();
            for rect in rects {
                assert!((rect.left() - 120.0).abs() < 0.1, "{rect:?}");
                assert!((rect.width() - 180.0).abs() < 0.1, "{rect:?}");
                assert!((rect.height() - 32.0).abs() < 0.1, "{rect:?}");
            }
        }
    }
    #[test]
    fn single_line_text_is_vertically_centered_in_tall_fields() {
        let ctx = egui::Context::default();
        super::super::super::setup(&ctx);
        let output = ctx.run_ui(egui::RawInput::default(), |ui| {
            for height in [30.0, 32.0] {
                let field = singleline(&mut "8080".into())
                    .desired_width(180.0)
                    .min_size(Vec2::new(180.0, height))
                    .show(ui);
                let text_center = field.galley_pos.y + field.galley.size().y / 2.0;
                assert!((text_center - field.response.rect.center().y).abs() < 1.0);
            }
        });
        output.drop_without_applying_deltas();
    }
}

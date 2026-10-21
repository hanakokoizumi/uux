use super::*;

#[derive(Clone)]
struct LocalEntry {
    path: PathBuf,
    name: String,
    directory: bool,
    size: u64,
    modified: u64,
}
#[derive(Default)]
pub(super) struct LocalCache {
    path: String,
    entries: Vec<LocalEntry>,
    pending: Option<std::sync::mpsc::Receiver<Result<Vec<LocalEntry>, String>>>,
    scanned: Option<Instant>,
    error: Option<String>,
}
impl LocalCache {
    pub(super) fn invalidate(&mut self) {
        self.scanned = None;
        self.pending = None;
    }
    pub(super) fn refresh(&mut self, path: &str) {
        if let Some(pending) = &self.pending
            && let Ok(result) = pending.try_recv()
        {
            match result {
                Ok(entries) => {
                    self.entries = entries;
                    self.error = None;
                }
                Err(e) => {
                    self.entries.clear();
                    self.error = Some(e);
                }
            }
            self.pending = None;
            self.scanned = Some(Instant::now());
        }
        let changed = self.path != path;
        if changed
            || (self.pending.is_none()
                && self
                    .scanned
                    .is_none_or(|t| t.elapsed() > Duration::from_secs(3)))
        {
            self.path = path.into();
            if changed {
                self.entries.clear();
            }
            let (tx, rx) = std::sync::mpsc::channel();
            self.pending = Some(rx);
            let path = path.to_owned();
            std::thread::spawn(move || {
                let result = std::fs::read_dir(path)
                    .map(|dir| {
                        let mut entries = dir
                            .filter_map(Result::ok)
                            .map(|item| {
                                let meta = item.metadata().ok();
                                LocalEntry {
                                    path: item.path(),
                                    name: item.file_name().to_string_lossy().into(),
                                    directory: meta.as_ref().is_some_and(|m| m.is_dir()),
                                    size: meta.as_ref().map_or(0, |m| m.len()),
                                    modified: meta
                                        .and_then(|m| m.modified().ok())
                                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                                        .map_or(0, |d| d.as_secs()),
                                }
                            })
                            .collect::<Vec<_>>();
                        entries.sort_by_key(|e| (!e.directory, e.name.clone()));
                        entries
                    })
                    .map_err(|_| "目录不存在或无法读取".to_owned());
                let _ = tx.send(result);
            });
        }
    }
}

impl Desktop {
    fn local_keys(&self) -> Vec<String> {
        self.ui_state
            .local_cache
            .entries
            .iter()
            .map(|r| r.path.to_string_lossy().into_owned())
            .collect()
    }
    fn remote_keys(&self) -> Vec<String> {
        self.ui_state
            .entries
            .iter()
            .filter(|r| [0, 4].contains(&r["kind"].as_i64().unwrap_or(-1)))
            .map(|r| self.remote_entry_path(r))
            .collect()
    }
    fn navigate_local(&mut self, path: String, back: bool) {
        if path != self.ui_state.local_path {
            if back {
                self.ui_state.local_history.pop();
            } else {
                self.ui_state
                    .local_history
                    .push(self.ui_state.local_path.clone());
            }
            self.ui_state.local_selected.clear();
            self.ui_state.local_path = path;
        }
        self.ui_state.local_address = self.ui_state.local_path.clone();
    }
    fn navigate_remote(&mut self, path: String, back: bool) {
        self.ui_state.remote_back = back;
        self.control(json!({"op":"read_dir","path":path}));
    }
    fn send_selected(&mut self, remote: bool) {
        let items: Vec<Value> = if remote {
            self.ui_state.entries.iter().filter(|r| self.ui_state.remote_selected.chosen.contains(&self.remote_entry_path(r)))
                .map(|r|json!({"op":"download","remote":self.remote_entry_path(r),"local":self.ui_state.local_path,"directory":r["kind"]==0,"size":r["size"]})).collect()
        } else {
            self.ui_state.local_cache.entries.iter().filter(|r|self.ui_state.local_selected.chosen.contains(&r.path.to_string_lossy().into_owned()))
                .map(|r|json!({"op":"upload","local":r.path,"remote":self.ui_state.remote_path,"size":r.size})).collect()
        };
        if !items.is_empty() && !self.ui_state.remote_loading {
            self.control(json!({"op":"transfer_batch","items":items}));
        }
    }
    pub(super) fn files_view(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        egui::Panel::top("file-title")
            .exact_size(36.0)
            .frame(
                egui::Frame::new()
                    .fill(SIDE)
                    .inner_margin(egui::Margin::symmetric(12, 2)),
            )
            .show(ui, |ui| {
                ui.spacing_mut().interact_size.y = 32.0;
                ui.horizontal(|ui| {
                    let start = ui.next_widget_position();
                    ui.label(RichText::new("文件传输").size(12.0));
                    let (blank, _) = ui.allocate_exact_size(
                        Vec2::new((ui.available_width() - 120.0).max(0.0), 26.0),
                        egui::Sense::hover(),
                    );
                    let drag = ui.interact(
                        egui::Rect::from_min_max(start, blank.max),
                        ui.scope_id().with("file-drag"),
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
                    ui.with_layout(
                        egui::Layout::right_to_left(egui::Align::Center),
                        art::window_buttons,
                    );
                });
            });
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(SIDE).inner_margin(egui::Margin {
                left: 27,
                right: 27,
                top: 12,
                bottom: 16,
            }))
            .show(ui, |ui| {
                self.ui_state.local_cache.refresh(&self.ui_state.local_path);
                let local_keys = self.local_keys();
                let remote_keys = self.remote_keys();
                self.ui_state
                    .local_selected
                    .sync(&self.ui_state.local_path, &local_keys);
                self.ui_state
                    .remote_selected
                    .sync(&self.ui_state.remote_path, &remote_keys);
                ui.spacing_mut().item_spacing.x = 24.0;
                let table_height = (ui.available_height() * 0.55).clamp(230.0, 460.0);
                ui.columns(2, |cols| {
                    for col in cols.iter_mut() {
                        col.spacing_mut().item_spacing.x = 8.0;
                        col.spacing_mut().interact_size.y = 32.0;
                    }
                    cols[0].horizontal(|ui| {
                        let host = std::fs::read_to_string("/etc/hostname")
                            .unwrap_or_else(|_| "本机".into());
                        ui.add(
                            egui::Label::new(RichText::new(host.trim()).font(fonts::bold(24.0)))
                                .truncate(),
                        );
                        badge(ui, "本机", false);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if send_button(
                                ui,
                                "发送 →",
                                self.connected
                                    && !self.ui_state.remote_loading
                                    && !local_keys.is_empty()
                                    && !self.ui_state.local_selected.chosen.is_empty(),
                            )
                            .clicked()
                            {
                                self.send_selected(false);
                            }
                        });
                    });
                    cols[1].horizontal(|ui| {
                        if send_button(
                            ui,
                            "← 发送",
                            self.connected
                                && !self.ui_state.remote_loading
                                && !self.ui_state.remote_selected.chosen.is_empty(),
                        )
                        .clicked()
                        {
                            self.send_selected(true);
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.add(
                                egui::Label::new(
                                    RichText::new(
                                        self.active_device
                                            .as_ref()
                                            .map_or("远端", |d| d.name.as_str()),
                                    )
                                    .font(fonts::bold(24.0)),
                                )
                                .truncate(),
                            );
                            badge(ui, "远端", true);
                        });
                    });
                    cols[0].add_space(10.0);
                    cols[1].add_space(10.0);
                    cols[0].push_id("local-pane", |ui| {
                        self.local_file_pane(ui, table_height, &local_keys)
                    });
                    cols[1].push_id("remote-pane", |ui| {
                        self.remote_file_pane(ui, table_height, &remote_keys)
                    });
                });
                self.file_shortcuts(ui, &local_keys, &remote_keys);
                ui.add_space(22.0);
                self.transfer_list(ui);
                if let Some(error) = &self.error {
                    ui.colored_label(Color32::from_rgb(190, 45, 45), error);
                } else {
                    ui.label(
                        RichText::new(&self.ui_state.last_notice)
                            .size(11.0)
                            .color(MUTED),
                    );
                }
            });
        self.file_dialog(&ctx);
        if self.connected && self.ui_state.prefs.drag_send && !self.ui_state.remote_loading {
            let items: Vec<_> = ctx
                .input(|i| i.raw.dropped_files.clone())
                .iter()
                .filter(|f| f.path().exists())
                .map(|f| json!({"op":"upload","local":f.path(),"remote":self.ui_state.remote_path}))
                .collect();
            if !items.is_empty() {
                self.control(json!({"op":"transfer_batch","items":items}));
            }
        }
    }
    fn local_file_pane(&mut self, ui: &mut egui::Ui, height: f32, keys: &[String]) {
        let panel = file_panel().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 0.0;
            egui::Frame::new().inner_margin(7).show(ui, |ui| {
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled_ui(!self.ui_state.local_history.is_empty(), |ui| {
                            small_icon(ui, Icon::Back, "后退")
                        })
                        .inner
                        .clicked()
                        && let Some(path) = self.ui_state.local_history.last().cloned()
                    {
                        self.navigate_local(path, true);
                    }
                    if small_icon(ui, Icon::Up, "上级目录").clicked()
                        && let Some(path) = PathBuf::from(&self.ui_state.local_path).parent()
                    {
                        self.navigate_local(path.display().to_string(), false);
                    }
                    if small_icon(ui, Icon::Refresh, "刷新（F5）").clicked() {
                        self.ui_state.local_cache.scanned = None;
                    }
                    ui.separator();
                    if small_icon(ui, Icon::FolderAdd, "新建文件夹").clicked() {
                        self.ui_state.file_dialog =
                            Some(("local_mkdir".into(), self.ui_state.local_path.clone()));
                        self.ui_state.file_name.clear();
                    }
                    let r = ui.add_sized(
                        [ui.available_width(), 30.0],
                        singleline(&mut self.ui_state.local_address).font(egui::TextStyle::Body),
                    );
                    if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        self.navigate_local(self.ui_state.local_address.clone(), false);
                    }
                });
            });
            if file_header(ui, self.ui_state.local_selected.chosen.len(), keys.len()).clicked() {
                self.ui_state.local_selected.toggle_all(keys);
                self.ui_state.file_focus_remote = false;
            }
            egui::ScrollArea::vertical()
                .id_salt("local-files")
                .max_height(height - 94.0)
                .min_scrolled_height(height - 94.0)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    if let Some(error) = &self.ui_state.local_cache.error {
                        ui.colored_label(MUTED, error);
                    }
                    for row in self.ui_state.local_cache.entries.clone() {
                        let key = row.path.to_string_lossy().into_owned();
                        let r = file_row(
                            ui,
                            &row.name,
                            row.directory,
                            row.size,
                            row.modified,
                            self.ui_state.local_selected.chosen.contains(&key),
                        );
                        if r.clicked() {
                            let mods = ui.input(|i| i.modifiers);
                            let check = r
                                .interact_pointer_pos()
                                .is_some_and(|p| p.x < r.rect.left() + 31.0);
                            self.ui_state
                                .local_selected
                                .click(&key, keys, mods.ctrl, mods.shift, check);
                        }
                        if r.double_clicked() && row.directory {
                            self.navigate_local(key.clone(), false);
                        }
                        if r.secondary_clicked() {
                            self.ui_state.local_selected.context(&key);
                        }
                        r.context_menu(|ui| {
                            if ui.button("发送到远端").clicked() {
                                self.send_selected(false);
                                ui.close();
                            }
                            if ui.button("全选").clicked() {
                                self.ui_state.local_selected.all(keys);
                                ui.close();
                            }
                        });
                    }
                });
            pane_footer(
                ui,
                keys.len(),
                self.ui_state.local_selected.chosen.len(),
                self.ui_state.local_cache.pending.is_some(),
            );
        });
        if panel.response.contains_pointer() && ui.input(|i| i.pointer.any_pressed()) {
            self.ui_state.file_focus_remote = false;
        }
    }
    fn remote_file_pane(&mut self, ui: &mut egui::Ui, height: f32, keys: &[String]) {
        let panel = file_panel().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 0.0;
            egui::Frame::new().inner_margin(7).show(ui, |ui| {
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled_ui(
                            !self.ui_state.remote_history.is_empty()
                                && !self.ui_state.remote_loading,
                            |ui| small_icon(ui, Icon::Back, "后退"),
                        )
                        .inner
                        .clicked()
                        && let Some(path) = self.ui_state.remote_history.last().cloned()
                    {
                        self.navigate_remote(path, true);
                    }
                    if small_icon(ui, Icon::Up, "上级目录").clicked() {
                        self.navigate_remote(remote_parent(&self.ui_state.remote_path), false);
                    }
                    if small_icon(ui, Icon::Refresh, "刷新（F5）").clicked() {
                        self.navigate_remote(self.ui_state.remote_path.clone(), false);
                    }
                    ui.separator();
                    if small_icon(ui, Icon::FolderAdd, "新建文件夹").clicked() {
                        self.ui_state.file_dialog =
                            Some(("mkdir".into(), self.ui_state.remote_path.clone()));
                        self.ui_state.file_name.clear();
                    }
                    let r = ui.add_sized(
                        [ui.available_width(), 30.0],
                        singleline(&mut self.ui_state.remote_address).font(egui::TextStyle::Body),
                    );
                    if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        self.navigate_remote(self.ui_state.remote_address.clone(), false);
                    }
                });
            });
            if file_header(ui, self.ui_state.remote_selected.chosen.len(), keys.len()).clicked() {
                self.ui_state.remote_selected.toggle_all(keys);
                self.ui_state.file_focus_remote = true;
            }
            egui::ScrollArea::vertical()
                .id_salt("remote-files")
                .max_height(height - 94.0)
                .min_scrolled_height(height - 94.0)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    for row in self.ui_state.entries.clone() {
                        let dir = [0, 2, 3].contains(&row["kind"].as_i64().unwrap_or(4));
                        let key = self.remote_entry_path(&row);
                        let selectable = [0, 4].contains(&row["kind"].as_i64().unwrap_or(-1));
                        let r = file_row(
                            ui,
                            row["name"].as_str().unwrap_or(""),
                            dir,
                            row["size"].as_u64().unwrap_or(0),
                            row["modified"].as_u64().unwrap_or(0),
                            self.ui_state.remote_selected.chosen.contains(&key),
                        );
                        if r.clicked() && selectable {
                            let mods = ui.input(|i| i.modifiers);
                            let check = r
                                .interact_pointer_pos()
                                .is_some_and(|p| p.x < r.rect.left() + 31.0);
                            self.ui_state
                                .remote_selected
                                .click(&key, keys, mods.ctrl, mods.shift, check);
                        }
                        if r.double_clicked() && dir {
                            self.navigate_remote(key.clone(), false);
                        }
                        if r.secondary_clicked() && selectable {
                            self.ui_state.remote_selected.context(&key);
                        }
                        r.context_menu(|ui| {
                            if ui
                                .add_enabled(selectable, egui::Button::new("发送到本机"))
                                .clicked()
                            {
                                self.send_selected(true);
                                ui.close();
                            }
                            if ui.button("全选").clicked() {
                                self.ui_state.remote_selected.all(keys);
                                ui.close();
                            }
                            ui.separator();
                            let single =
                                selectable && self.ui_state.remote_selected.chosen.len() == 1;
                            if ui
                                .add_enabled(single, egui::Button::new("重命名"))
                                .clicked()
                            {
                                self.ui_state.file_dialog = Some(("rename".into(), key.clone()));
                                self.ui_state.file_name = row["name"].as_str().unwrap_or("").into();
                                ui.close();
                            }
                            if ui.add_enabled(single, egui::Button::new("删除")).clicked() {
                                self.ui_state.file_dialog = Some((
                                    if dir { "remove_dir" } else { "remove_file" }.into(),
                                    key.clone(),
                                ));
                                ui.close();
                            }
                        });
                    }
                });
            pane_footer(
                ui,
                self.ui_state.entries.len(),
                self.ui_state.remote_selected.chosen.len(),
                self.ui_state.remote_loading,
            );
        });
        if panel.response.contains_pointer() && ui.input(|i| i.pointer.any_pressed()) {
            self.ui_state.file_focus_remote = true;
        }
    }
    fn file_shortcuts(&mut self, ui: &mut egui::Ui, local: &[String], remote: &[String]) {
        if self.ui_state.file_dialog.is_some()
            || egui::Popup::is_any_open(ui.ctx())
            || ui.ctx().text_edit_focused()
        {
            return;
        }
        let right = self.ui_state.file_focus_remote;
        if ui.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, egui::Key::A)) {
            if right {
                self.ui_state.remote_selected.all(remote);
            } else {
                self.ui_state.local_selected.all(local);
            }
        }
        if ui.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, egui::Key::Enter)) {
            self.send_selected(right);
        }
        if ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            if right {
                self.ui_state.remote_selected.clear();
            } else {
                self.ui_state.local_selected.clear();
            }
        }
        if ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::F5)) {
            if right {
                self.navigate_remote(self.ui_state.remote_path.clone(), false);
            } else {
                self.ui_state.local_cache.scanned = None;
            }
        }
        if ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Backspace)) {
            if right {
                self.navigate_remote(remote_parent(&self.ui_state.remote_path), false);
            } else if let Some(p) = PathBuf::from(&self.ui_state.local_path).parent() {
                self.navigate_local(p.display().to_string(), false);
            }
        }
    }
    fn transfer_list(&mut self, ui: &mut egui::Ui) {
        file_panel().show(ui,|ui| {
            ui.set_width(ui.available_width());
            egui::Frame::new().inner_margin(10).show(ui,|ui| {
                ui.horizontal(|ui| {
                    ui.label("传输列表");
                    ui.label(RichText::new(format!("已传输 {} 项",self.ui_state.transfers.iter().filter(|t|t["status"]=="已完成").count())).size(11.0).color(MUTED));
                    let active=self.ui_state.transfers.iter().any(transfer_active);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center),|ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        if transfer_toolbar_button(ui,"清除完结任务",true).clicked(){self.ui_state.transfers.retain(transfer_active);self.ui_state.transfer_rates.retain(&self.ui_state.transfers);}
                        if transfer_toolbar_button(ui,"全部取消",active).clicked(){self.control(json!({"op":"transfers_cancel"}));}
                        for (label,paused) in [("全部开始",false),("全部暂停",true)] {
                            if transfer_toolbar_button(ui,label,active).clicked(){self.control(json!({"op":"transfers_pause","paused":paused}));}
                        }
                    });
                });
            });
            ui.separator();
            let columns=TRANSFER_COLUMNS;
            let (head,_)=ui.allocate_exact_size(Vec2::new(ui.available_width(),28.0),egui::Sense::hover());
            for (i,name) in ["名称","状态 / 进度 / 速度","大小","发送路径","接收路径","操作"].iter().enumerate(){cell(ui,head,columns[i],name,MUTED,12.0);}
            egui::ScrollArea::vertical().id_salt("transfers").max_height((ui.available_height()-25.0).max(70.0)).min_scrolled_height(65.0).auto_shrink([false,false]).show(ui,|ui| {
                for t in self.ui_state.transfers.clone(){
                    let (r,row)=ui.allocate_exact_size(Vec2::new(ui.available_width(),34.0),egui::Sense::hover());
                    let status=t["status"].as_str().unwrap_or("");
                    cell(ui,r,0.0,t["name"].as_str().unwrap_or(""),INK,13.0);
                    let speed=transfer_progress::speed_text(self.ui_state.transfer_rates.bytes_per_second(&t,Instant::now()));
                    transfer_status(ui,r,&t,&speed);
                    cell(ui,r,0.51,&if t["directory"]==true{"文件夹".into()}else{size_text(t["size"].as_u64().unwrap_or(0))},INK,12.0);
                    let upload=t["upload"].as_bool().unwrap_or(false);
                    cell(ui,r,0.63,t[if upload{"local"}else{"remote"}].as_str().unwrap_or(""),MUTED,11.0);
                    cell(ui,r,0.755,t[if upload{"remote"}else{"local"}].as_str().unwrap_or(""),MUTED,11.0);
                    row.on_hover_text(format!("{}\n{} → {}\n{} · {} · {}\n已传输 {} / {}{}",t["name"].as_str().unwrap_or(""),t[if upload{"local"}else{"remote"}].as_str().unwrap_or(""),t[if upload{"remote"}else{"local"}].as_str().unwrap_or(""),status,transfer_progress::percentage(&t),speed,size_text(t["done"].as_u64().unwrap_or(0)),if t["directory"]==true{"文件夹进度见子文件".into()}else{size_text(t["size"].as_u64().unwrap_or(0))},t["error"].as_str().map(|e|format!("\n{e}")).unwrap_or_default()));
                    let action=egui::Rect::from_min_max(egui::pos2(r.left()+r.width()*0.89,r.top()),r.max);
                    ui.scope_builder(egui::UiBuilder::new().max_rect(action),|ui| {
                        ui.spacing_mut().item_spacing.x=2.0;
                        ui.horizontal(|ui| {
                            if transfer_active(&t){
                                let paused=status=="已暂停";
                                if task_button(ui,if paused{Icon::Play}else{Icon::Pause},if paused{"继续任务"}else{"暂停任务"}).clicked(){self.control(json!({"op":"transfer_pause","id":t["id"],"paused":!paused}));}
                            }
                            if task_button(ui,if transfer_active(&t){Icon::Close}else{Icon::Trash},if transfer_active(&t){"取消任务"}else{"清除记录"}).clicked(){
                                if transfer_active(&t){self.control(json!({"op":"transfer_cancel","id":t["id"]}));}
                                else{self.ui_state.transfers.retain(|x|x["id"]!=t["id"]);self.ui_state.transfer_rates.retain(&self.ui_state.transfers);}
                            }
                        });
                    });
                }
            });
        });
    }
    fn file_dialog(&mut self, ctx: &egui::Context) {
        let Some((op, path)) = self.ui_state.file_dialog.clone() else {
            return;
        };
        let mut open = true;
        dialog(if op.starts_with("remove_"){"删除项目"}else if op.ends_with("mkdir"){"新建文件夹"}else{"重命名"})
            .open(&mut open).collapsible(false).resizable(false)
            .show(ctx,|ui| {
                ui.set_max_width(420.0);ui.label(&path);
                if op.starts_with("remove_"){ui.label("所选项目及其内容将被删除。");}
                else{ui.add_sized([ui.available_width(),30.0],singleline(&mut self.ui_state.file_name));}
                ui.add_space(16.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center),|ui| {
                    if ui.add_enabled_ui(op.starts_with("remove_") || !self.ui_state.file_name.is_empty(),|ui|art::primary(ui,"确定",Vec2::new(88.0,30.0))).inner.clicked(){
                        let name=&self.ui_state.file_name;
                        if op.starts_with("remove_"){self.control(json!({"op":op,"path":path,"recursive":op=="remove_dir"}));}
                        else if let Err(e)=crate::core::files::safe_name(name){self.error=Some(e.to_string());}
                        else if op=="local_mkdir"{
                            if let Err(e)=std::fs::create_dir(PathBuf::from(&path).join(name)){self.error=Some(format!("无法创建文件夹：{e}"));}
                            self.ui_state.local_cache.scanned=None;
                        }
                        else{self.control(json!({"op":op,"path":if op=="mkdir"{format!("{}\\{}",path.trim_end_matches('\\'),name)}else{path},"name":name}));}
                        self.ui_state.file_dialog=None;
                    }
                    if ui.add_sized([88.0,30.0],egui::Button::new("取消")).clicked(){self.ui_state.file_dialog=None;}
                });
            });
        if !open {
            self.ui_state.file_dialog = None;
        }
    }
}

fn transfer_toolbar_button(ui: &mut egui::Ui, label: &str, enabled: bool) -> egui::Response {
    ui.add_enabled_ui(enabled, |ui| {
        ui.add_sized(
            Vec2::new(116.0, 28.0),
            egui::Button::new(RichText::new(label).size(13.0)),
        )
    })
    .inner
}

fn file_panel() -> egui::Frame {
    egui::Frame::new()
        .fill(BG)
        .stroke(egui::Stroke::new(1.0, Color32::from_rgb(214, 216, 218)))
        .corner_radius(4)
}
fn badge(ui: &mut egui::Ui, text: &str, remote: bool) {
    let color = Color32::from_rgb(75, 91, 96);
    let galley =
        ui.painter()
            .layout_no_wrap(text.to_owned(), egui::FontId::proportional(11.0), color);
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(galley.size().x + 8.0, 20.0), egui::Sense::hover());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Label, true, text));
    ui.painter().rect_filled(
        rect,
        3,
        if remote {
            Color32::from_rgb(180, 237, 219)
        } else {
            Color32::from_rgb(221, 225, 230)
        },
    );
    ui.painter()
        .galley(rect.center() - galley.size() * 0.5, galley, color);
}
fn small_icon(ui: &mut egui::Ui, icon: Icon, title: &str) -> egui::Response {
    let (r, response) = ui.allocate_exact_size(Vec2::new(36.0, 30.0), egui::Sense::click());
    art::hover_background(ui, &response, r);
    art::draw(
        ui.painter(),
        egui::Rect::from_center_size(r.center(), Vec2::splat(18.0)),
        icon,
        if response.hovered() { INK } else { MUTED },
    );
    response.on_hover_text(title)
}
fn task_button(ui: &mut egui::Ui, icon: Icon, title: &str) -> egui::Response {
    let (r, response) = ui.allocate_exact_size(Vec2::splat(24.0), egui::Sense::click());
    art::hover_background(ui, &response, r);
    art::draw(ui.painter(), r.shrink(5.0), icon, INK);
    response.on_hover_text(title)
}
fn send_button(ui: &mut egui::Ui, label: &str, enabled: bool) -> egui::Response {
    ui.add_enabled_ui(enabled, |ui| art::primary(ui, label, Vec2::new(66.0, 28.0)))
        .inner
        .on_hover_text("发送选中的文件和文件夹（Ctrl+Enter）")
}
fn pane_footer(ui: &mut egui::Ui, total: usize, selected: usize, loading: bool) {
    let (r, _) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 22.0), egui::Sense::hover());
    ui.painter().text(
        r.left_center() + Vec2::new(10.0, 0.0),
        egui::Align2::LEFT_CENTER,
        if loading {
            "正在读取…".into()
        } else {
            format!("{total} 项  ·  已选择 {selected} 项")
        },
        egui::FontId::proportional(11.0),
        MUTED,
    );
}
fn checkbox(ui: &egui::Ui, r: egui::Rect, selected: bool, mixed: bool) {
    ui.painter().rect_filled(
        r,
        2,
        if selected || mixed {
            BLUE
        } else {
            Color32::TRANSPARENT
        },
    );
    ui.painter().rect_stroke(
        r,
        2,
        egui::Stroke::new(1.0, if selected || mixed { BLUE } else { MUTED }),
        egui::StrokeKind::Inside,
    );
    if mixed {
        ui.painter().line_segment(
            [
                r.left_center() + Vec2::new(3.0, 0.0),
                r.right_center() - Vec2::new(3.0, 0.0),
            ],
            egui::Stroke::new(1.5, Color32::WHITE),
        );
    } else if selected {
        ui.painter().add(egui::Shape::line(
            vec![
                r.min + Vec2::new(3.0, 7.0),
                r.min + Vec2::new(6.0, 10.0),
                r.min + Vec2::new(11.0, 4.0),
            ],
            egui::Stroke::new(1.5, Color32::WHITE),
        ));
    }
}
fn file_header(ui: &mut egui::Ui, selected: usize, total: usize) -> egui::Response {
    let (r, _) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 28.0), egui::Sense::hover());
    let box_rect =
        egui::Rect::from_center_size(egui::pos2(r.left() + 17.0, r.center().y), Vec2::splat(14.0));
    let response = ui.interact(
        box_rect.expand(7.0),
        ui.scope_id().with("select-all"),
        egui::Sense::click(),
    );
    art::hover_background(ui, &response, box_rect.expand(5.0));
    checkbox(
        ui,
        box_rect,
        total > 0 && selected == total,
        selected > 0 && selected < total,
    );
    for (x, name) in [
        (r.left() + 33.0, "名称"),
        (r.left() + r.width() * 0.29 + 10.0, "修改日期"),
        (r.left() + r.width() * 0.62 + 10.0, "类型"),
        (r.left() + r.width() * 0.77 + 10.0, "大小"),
    ] {
        ui.painter().text(
            egui::pos2(x, r.center().y),
            egui::Align2::LEFT_CENTER,
            name,
            egui::FontId::proportional(12.0),
            MUTED,
        );
    }
    ui.painter()
        .hline(r.x_range(), r.bottom(), egui::Stroke::new(1.0, BORDER));
    response.on_hover_text("全选 / 取消全选（Ctrl+A）")
}
const TRANSFER_COLUMNS: [f32; 6] = [0.0, 0.25, 0.51, 0.63, 0.755, 0.89];
fn transfer_status(ui: &egui::Ui, rect: egui::Rect, transfer: &Value, speed: &str) {
    let status = transfer["status"].as_str().unwrap_or("");
    let progress = transfer_progress::fraction(transfer);
    let label = if progress.is_some() {
        format!("{status} · {}", transfer_progress::percentage(transfer))
    } else {
        status.into()
    };
    let color = if status == "已完成" {
        Color32::from_rgb(0, 177, 130)
    } else if matches!(status, "失败" | "传输超时") {
        Color32::from_rgb(193, 65, 65)
    } else {
        INK
    };
    if transfer_active(transfer) {
        let mut first = rect;
        first.max.y = rect.top() + 17.0;
        let mut second = rect;
        second.min.y = rect.top() + 16.0;
        second.max.y = rect.bottom() - 3.0;
        cell(ui, first, 0.25, &label, color, 12.0);
        cell(
            ui,
            second,
            0.25,
            if transfer["directory"] == true {
                "进度见子文件"
            } else {
                speed
            },
            MUTED,
            11.0,
        );
        if let Some(progress) = progress {
            let start = rect.left() + rect.width() * 0.25 + 10.0;
            let end = rect.left() + rect.width() * 0.51 - 10.0;
            let y = rect.bottom() - 1.0;
            ui.painter()
                .hline(start..=end, y, egui::Stroke::new(2.0, BORDER));
            if progress > 0.0 {
                ui.painter().hline(
                    start..=start + (end - start) * progress,
                    y,
                    egui::Stroke::new(2.0, if status == "已暂停" { MUTED } else { BLUE }),
                );
            }
        }
    } else {
        cell(ui, rect, 0.25, &label, color, 12.0);
    }
}

fn cell(ui: &egui::Ui, r: egui::Rect, x: f32, text: &str, color: Color32, size: f32) {
    let remaining = TRANSFER_COLUMNS
        .iter()
        .find(|&&v| v > x + 0.001)
        .copied()
        .unwrap_or(1.0)
        - x;
    let mut job = egui::text::LayoutJob::simple_singleline(
        text.into(),
        egui::FontId::proportional(size),
        color,
    );
    job.wrap.max_width = (r.width() * remaining - 12.0).max(10.0);
    job.wrap.max_rows = 1;
    let galley = ui.painter().layout_job(job);
    ui.painter().galley(
        egui::pos2(
            r.left() + r.width() * x + 10.0,
            r.center().y - galley.size().y / 2.0,
        ),
        galley,
        color,
    );
}
fn file_row(
    ui: &mut egui::Ui,
    name: &str,
    dir: bool,
    size: u64,
    time: u64,
    selected: bool,
) -> egui::Response {
    let (r, response) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 34.0), egui::Sense::click());
    if selected || response.hovered() {
        ui.painter().rect_filled(
            r,
            0,
            if selected {
                Color32::from_rgb(220, 236, 251)
            } else {
                SIDE
            },
        );
    }
    let check_rect =
        egui::Rect::from_center_size(egui::pos2(r.left() + 17.0, r.center().y), Vec2::splat(14.0));
    checkbox(ui, check_rect, selected, false);
    let icon =
        egui::Rect::from_center_size(egui::pos2(r.left() + 46.0, r.center().y), Vec2::splat(18.0));
    if dir {
        ui.painter().rect_filled(
            icon.shrink2(Vec2::new(0.0, 3.0)),
            2,
            Color32::from_rgb(255, 199, 47),
        );
        ui.painter().rect_filled(
            egui::Rect::from_min_size(icon.min + Vec2::new(1.0, 1.0), Vec2::new(8.0, 5.0)),
            1,
            Color32::from_rgb(243, 178, 20),
        );
    } else {
        ui.painter().rect_stroke(
            icon.shrink2(Vec2::new(3.0, 1.0)),
            1,
            egui::Stroke::new(1.0, MUTED),
            egui::StrokeKind::Inside,
        );
    }
    let cells = [
        (
            r.left() + 62.0,
            r.left() + r.width() * 0.29,
            name.to_owned(),
            INK,
        ),
        (
            r.left() + r.width() * 0.29 + 10.0,
            r.left() + r.width() * 0.62,
            date(time),
            MUTED,
        ),
        (
            r.left() + r.width() * 0.62 + 10.0,
            r.left() + r.width() * 0.77,
            if dir {
                "文件夹".into()
            } else {
                name.rsplit_once('.').map_or("文件", |(_, e)| e).into()
            },
            MUTED,
        ),
        (
            r.left() + r.width() * 0.77 + 10.0,
            r.right() - 4.0,
            if dir { "--".into() } else { size_text(size) },
            MUTED,
        ),
    ];
    for (start, end, text, color) in cells {
        let mut job =
            egui::text::LayoutJob::simple_singleline(text, egui::FontId::proportional(12.0), color);
        job.wrap.max_width = (end - start - 5.0).max(10.0);
        job.wrap.max_rows = 1;
        let galley = ui.painter().layout_job(job);
        ui.painter().galley(
            egui::pos2(start, r.center().y - galley.size().y / 2.0),
            galley,
            color,
        );
    }
    response.on_hover_text(name)
}
fn date(time: u64) -> String {
    if time == 0 {
        return "--".into();
    }
    let seconds = if time > 100_000_000_000_000 {
        (time / 10_000_000).saturating_sub(11_644_473_600)
    } else if time > 100_000_000_000 {
        time / 1000
    } else {
        time
    };
    let time = seconds as libc::time_t;
    // localtime_r writes one tm into caller-owned storage and has no shared buffer.
    let mut tm = unsafe { std::mem::zeroed::<libc::tm>() };
    if unsafe { libc::localtime_r(&time, &mut tm) }.is_null() {
        return "--".into();
    }
    format!(
        "{:04}/{:02}/{:02} {:02}:{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min
    )
}

#[cfg(test)]
mod progress_layout_tests {
    use super::*;

    #[test]
    fn status_percentage_and_speed_fit_without_overlapping_at_minimum_width() {
        for width in [824.0, 924.0, 1384.0] {
            let ctx = egui::Context::default();
            fonts::setup(&ctx);
            let row = egui::Rect::from_min_size(egui::pos2(0.0, 20.0), Vec2::new(width, 34.0));
            let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
                transfer_status(
                    ui,
                    row,
                    &json!({"size":1000,"done":999,"status":"传输中"}),
                    "999.99 MiB/s",
                );
            });
            output.textures_delta.clear();
            let text = output
                .shapes
                .iter()
                .filter_map(|s| match &s.shape {
                    egui::Shape::Text(t) => {
                        Some((t.galley.job.text.as_str(), t.visual_bounding_rect()))
                    }
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert_eq!(text.len(), 2);
            assert_eq!(text[0].0, "传输中 · 99.9%");
            assert_eq!(text[1].0, "999.99 MiB/s");
            assert!(
                text[0].1.bottom() <= text[1].1.top(),
                "status and speed overlap: {text:?}"
            );
            for (_, rect) in text {
                assert!(rect.top() >= row.top() && rect.bottom() < row.bottom() - 1.0);
                assert!(rect.left() >= width * 0.25 && rect.right() < width * 0.51);
            }
        }
    }
}

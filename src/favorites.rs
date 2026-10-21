use super::*;

/// Stored with desktop preferences in the existing owner-only state file.
/// Never derive Debug: an optional saved connection code is private.
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct FavoriteDevice {
    id: String,
    #[serde(default)]
    code: String,
    #[serde(default)]
    name: String,
}
impl FavoriteDevice {
    fn label(&self) -> &str {
        if self.name.is_empty() {
            &self.id
        } else {
            &self.name
        }
    }
}

#[derive(Default)]
pub(super) struct FavoriteDraft {
    original_id: Option<String>,
    id: String,
    code: String,
    name: String,
    focused: bool,
}
impl FavoriteDraft {
    fn device(&self, existing: &[FavoriteDevice]) -> Result<FavoriteDevice, String> {
        let id = crate::core::cloud::assistance_id(&self.id).map_err(|e| e.to_string())?;
        if existing
            .iter()
            .any(|d| d.id == id && Some(&d.id) != self.original_id.as_ref())
        {
            return Err("此设备已在收藏列表中".into());
        }
        let code = self.code.trim();
        if code.len() > 64 || code.chars().any(char::is_control) {
            return Err("请输入有效的设备验证码".into());
        }
        let name = self.name.trim();
        if name.chars().count() > 40 || name.chars().any(char::is_control) {
            return Err("备注名称最多可输入 40 个字符".into());
        }
        Ok(FavoriteDevice {
            id,
            code: code.into(),
            name: name.into(),
        })
    }
}

impl Desktop {
    pub(super) fn favorite_page(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("收藏设备").font(fonts::bold(24.0)));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add_sized([120.0, 32.0], egui::Button::new("添加收藏设备"))
                    .clicked()
                {
                    self.ui_state.favorite_draft = Some(FavoriteDraft::default());
                }
            });
        });
        ui.add_space(22.0);
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 4.0;
            for device in self.ui_state.prefs.favorite_devices.clone() {
                self.favorite_row(ui, &device, None);
            }
            // Preserve older installations' account-device bookmarks. New entries
            // use public assistance IDs, not private account device identifiers.
            let legacy: Vec<_> = self
                .devices
                .iter()
                .filter(|d| self.ui_state.prefs.favorites.contains(&d.id))
                .cloned()
                .collect();
            for device in legacy {
                let row = FavoriteDevice {
                    id: device.id.clone(),
                    name: device.name.clone(),
                    code: String::new(),
                };
                self.favorite_row(ui, &row, Some(device));
            }
            if self.ui_state.prefs.favorite_devices.is_empty()
                && self.ui_state.prefs.favorites.is_empty()
            {
                ui.label(RichText::new("暂无收藏设备，点击右上角添加").color(MUTED));
            }
        });
    }

    fn favorite_row(
        &mut self,
        ui: &mut egui::Ui,
        device: &FavoriteDevice,
        account: Option<Device>,
    ) {
        ui.push_id((&device.id, account.is_some()), |ui| {
            let (r, response) =
                ui.allocate_exact_size(Vec2::new(ui.available_width(), 56.0), egui::Sense::click());
            ui.painter().rect_filled(r, 3, Color32::WHITE);
            art::hover_background(ui, &response, r.shrink(1.0));
            ui.painter().rect_stroke(
                r,
                3,
                egui::Stroke::new(1.0, BORDER),
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
                device.label().into(),
                egui::FontId::proportional(14.0),
                INK,
            );
            job.wrap.max_width = (r.width() - 205.0).max(40.0);
            job.wrap.max_rows = 1;
            let text = ui.painter().layout_job(job);
            ui.painter().galley(
                egui::pos2(r.left() + 60.0, r.center().y - text.size().y / 2.0),
                text,
                INK,
            );
            let allowed = self.logged_in
                && !self.connected
                && !self.connecting
                && !self.disconnecting
                && account.as_ref().is_none_or(|d| d.online);
            let rect = egui::Rect::from_center_size(
                egui::pos2(r.right() - 82.0, r.center().y),
                Vec2::new(68.0, 28.0),
            );
            let connect = ui
                .scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
                    ui.add_enabled(allowed, egui::Button::new("连接").min_size(rect.size()))
                })
                .inner;
            if connect.clicked() || allowed && response.double_clicked() {
                if let Some(account) = account.clone() {
                    self.connect(account);
                } else {
                    self.ui_state.assist_id = device.id.clone();
                    self.ui_state.assist_code = device.code.clone();
                    if device.code.is_empty() {
                        self.ui_state.assist_prompt = true;
                    } else {
                        self.connect_assistance();
                    }
                }
            }
            let rect = egui::Rect::from_center_size(
                egui::pos2(r.right() - 24.0, r.center().y),
                Vec2::new(32.0, 28.0),
            );
            let menu = ui
                .scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
                    art::flat_button(ui, "···", rect.size())
                })
                .inner;
            egui::Popup::menu(&menu).show(|ui| {
                if account.is_none() && menu_item(ui, Icon::Edit, "编辑").clicked() {
                    self.ui_state.favorite_draft = Some(FavoriteDraft {
                        original_id: Some(device.id.clone()),
                        id: device.id.clone(),
                        code: device.code.clone(),
                        name: device.name.clone(),
                        focused: false,
                    });
                    ui.close();
                }
                if menu_item(ui, Icon::Trash, "删除设备").clicked() {
                    self.ui_state
                        .prefs
                        .favorite_devices
                        .retain(|d| d.id != device.id);
                    self.ui_state.prefs.favorites.remove(&device.id);
                    self.ui_state.save();
                    ui.close();
                }
            });
        });
    }

    pub(super) fn favorite_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut draft) = self.ui_state.favorite_draft.take() else {
            return;
        };
        let editing = draft.original_id.is_some();
        let mut save = false;
        let mut cancel = false;
        let response = egui::Modal::new(egui::Id::unique("favorite-editor"))
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
                ui.set_width(494.0);
                ui.spacing_mut().item_spacing = Vec2::ZERO;
                let (r, _) = ui.allocate_exact_size(Vec2::new(494.0, 72.0), egui::Sense::hover());
                ui.painter().text(
                    r.left_center() + Vec2::new(24.0, 0.0),
                    egui::Align2::LEFT_CENTER,
                    if editing {
                        "编辑收藏设备"
                    } else {
                        "添加收藏设备"
                    },
                    egui::FontId::proportional(16.0),
                    INK,
                );
                ui.painter()
                    .hline(r.x_range(), r.bottom(), egui::Stroke::new(1.0, BORDER));
                egui::Frame::new()
                    .inner_margin(egui::Margin {
                        left: 24,
                        right: 24,
                        top: 16,
                        bottom: 24,
                    })
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 16.0;
                        let id = favorite_field(
                            ui,
                            "伙伴的设备 ID",
                            &mut draft.id,
                            "请输入设备 ID",
                            20,
                            false,
                        );
                        if !draft.focused {
                            id.request_focus();
                            draft.focused = true;
                        }
                        favorite_field(
                            ui,
                            "设备验证码（可选）",
                            &mut draft.code,
                            "请输入设备验证码",
                            64,
                            true,
                        );
                        favorite_field(
                            ui,
                            "备注名称（可选）",
                            &mut draft.name,
                            "请输入名称",
                            40,
                            false,
                        );
                        if !draft.id.is_empty()
                            && let Err(message) =
                                draft.device(&self.ui_state.prefs.favorite_devices)
                        {
                            ui.colored_label(Color32::from_rgb(201, 65, 65), message);
                        }
                    });
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
                            let valid = draft.device(&self.ui_state.prefs.favorite_devices).is_ok();
                            save = ui
                                .add_enabled_ui(valid, |ui| {
                                    art::primary(
                                        ui,
                                        if editing { "保存更改" } else { "添加" },
                                        Vec2::new(219.0, 32.0),
                                    )
                                })
                                .inner
                                .clicked()
                                || valid && ui.input(|i| i.key_pressed(egui::Key::Enter));
                            cancel = ui
                                .add_sized([219.0, 32.0], egui::Button::new("取消"))
                                .clicked();
                        });
                    });
            });
        if save {
            if let Ok(device) = draft.device(&self.ui_state.prefs.favorite_devices) {
                if let Some(old) = self
                    .ui_state
                    .prefs
                    .favorite_devices
                    .iter_mut()
                    .find(|d| Some(&d.id) == draft.original_id.as_ref())
                {
                    *old = device;
                } else {
                    self.ui_state.prefs.favorite_devices.push(device);
                }
                self.ui_state.save();
            }
        } else if !cancel && !response.should_close() {
            self.ui_state.favorite_draft = Some(draft);
        }
    }
}

fn favorite_field(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut String,
    hint: &str,
    limit: usize,
    password: bool,
) -> egui::Response {
    let (r, _) = ui.allocate_exact_size(Vec2::new(446.0, 72.0), egui::Sense::hover());
    ui.painter().rect_stroke(
        r,
        4,
        egui::Stroke::new(1.0, BORDER),
        egui::StrokeKind::Inside,
    );
    ui.painter().text(
        r.left_center() + Vec2::new(16.0, 0.0),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::proportional(13.0),
        INK,
    );
    let rect = egui::Rect::from_center_size(
        egui::pos2(r.right() - 100.0, r.center().y),
        Vec2::new(170.0, 32.0),
    );
    ui.place(
        rect,
        singleline(value)
            .id_salt(label)
            .hint_text(hint)
            .char_limit(limit)
            .password(password)
            .min_size(rect.size()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn favorites_validate_duplicates_and_optional_fields() {
        let mut draft = FavoriteDraft {
            id: "123 456 789".into(),
            ..Default::default()
        };
        let saved = draft.device(&[]).unwrap();
        assert_eq!(saved.id, "123456789");
        assert!(saved.code.is_empty());
        assert_eq!(saved.label(), "123456789");
        assert!(draft.device(std::slice::from_ref(&saved)).is_err());
        draft.original_id = Some(saved.id.clone());
        assert!(draft.device(&[saved]).is_ok());
        draft.name = "字".repeat(41);
        assert!(draft.device(&[]).is_err());
    }
}
